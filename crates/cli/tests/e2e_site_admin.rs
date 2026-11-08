//! GAP S4·M4: 관리 화면에서 사이트 구조·회원·설정·디자인 고치기 (ADR-0064).
//! 실제 바이너리·실제 포트·실제 DB 파일로 각 폼을 보내고 CLI 로 DB 에 남은 값을 확인한다.
#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::path::Path;
use std::process::{Child, Command, Stdio};

use serde_json::Value;

const BIN: &str = env!("CARGO_BIN_EXE_maru");
const FORM: (&str, &str) = ("Content-Type", "application/x-www-form-urlencoded");

struct Server(Child);

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn maru(data: &Path, args: &[&str]) -> Value {
    let out = Command::new(BIN)
        .arg("--data")
        .arg(data)
        .args(args)
        .arg("--json")
        .output()
        .expect("maru 실행");
    let stdout = String::from_utf8(out.stdout).expect("UTF-8 출력");
    let v: Value =
        serde_json::from_str(stdout.trim()).unwrap_or_else(|e| panic!("JSON 아님: {e}\n{stdout}"));
    assert_eq!(v["ok"], true, "{args:?} → {v}");
    v["data"].clone()
}

fn start_server(data: &Path) -> (Server, String) {
    let mut child = Command::new(BIN)
        .arg("--data")
        .arg(data)
        .args(["serve", "--bind", "127.0.0.1:0", "--json"])
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("serve 실행");
    let stdout = child.stdout.take().unwrap();
    let server = Server(child);
    let mut line = String::new();
    BufReader::new(stdout).read_line(&mut line).unwrap();
    let v: Value = serde_json::from_str(line.trim()).expect("serve 시작 JSON");
    (server, v["data"]["bind"].as_str().unwrap().to_owned())
}

struct Res {
    status: u16,
    head: String,
    body: String,
}

impl Res {
    fn cookie(&self, name: &str) -> Option<String> {
        self.head.lines().find_map(|l| {
            let (k, v) = l.split_once(':')?;
            if !k.trim().eq_ignore_ascii_case("set-cookie") {
                return None;
            }
            let (n, val) = v.trim().split(';').next()?.split_once('=')?;
            (n == name).then(|| val.to_owned())
        })
    }

    fn location(&self) -> String {
        self.head
            .lines()
            .find_map(|l| {
                let (k, v) = l.split_once(':')?;
                k.trim()
                    .eq_ignore_ascii_case("location")
                    .then(|| v.trim().to_owned())
            })
            .unwrap_or_default()
    }
}

fn http(bind: &str, method: &str, path: &str, headers: &[(&str, &str)], body: &str) -> Res {
    let mut s = TcpStream::connect(bind).unwrap();
    let mut req = format!("{method} {path} HTTP/1.1\r\nHost: {bind}\r\nConnection: close\r\n");
    for (k, v) in headers {
        req.push_str(&format!("{k}: {v}\r\n"));
    }
    req.push_str(&format!("Content-Length: {}\r\n\r\n{body}", body.len()));
    s.write_all(req.as_bytes()).unwrap();
    let mut raw = Vec::new();
    s.read_to_end(&mut raw).unwrap();
    // 그림 응답(썸네일)도 받으므로 글자가 아닌 바이트는 바꿔 읽는다. 머리글만 보면 된다.
    let text = String::from_utf8_lossy(&raw).into_owned();
    let (head, body) = text.split_once("\r\n\r\n").unwrap_or((&text, ""));
    Res {
        status: head[9..12].parse().unwrap(),
        head: head.to_owned(),
        body: body.to_owned(),
    }
}

fn enc(s: &str) -> String {
    let mut out = String::new();
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || b"-_.~".contains(&b) {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

fn form(pairs: &[(&str, &str)]) -> String {
    pairs
        .iter()
        .map(|(k, v)| format!("{}={}", enc(k), enc(v)))
        .collect::<Vec<_>>()
        .join("&")
}

/// 화면의 폼 값(input 의 value, select 의 고른 option)을 모은다. 따옴표 안에 `"` 는 없다고 본다.
fn form_values(html: &str) -> Vec<(String, String)> {
    let attr = |tag: &str, name: &str| -> Option<String> {
        let key = format!(" {name}=\"");
        let at = tag.find(&key)? + key.len();
        Some(tag[at..at + tag[at..].find('"')?].to_owned())
    };
    let mut out = Vec::new();
    let mut rest = html;
    while let Some(i) = rest.find('<') {
        rest = &rest[i..];
        let end = rest.find('>').unwrap_or(rest.len());
        let tag = &rest[..end];
        // 라디오는 고른 것만 보낸다 (브라우저와 같게).
        let unchecked_radio = tag.contains("type=\"radio\"") && !tag.contains(" checked");
        if tag.starts_with("<input") && !tag.contains("type=\"checkbox\"") && !unchecked_radio {
            if let (Some(n), Some(v)) = (attr(tag, "name"), attr(tag, "value")) {
                out.push((n, v));
            }
        } else if tag.starts_with("<select")
            && let Some(n) = attr(tag, "name")
        {
            let close = rest.find("</select>").unwrap_or(rest.len());
            let body = &rest[..close];
            if let Some(sel) = body.find(" selected") {
                let open = body[..sel].rfind("<option").unwrap();
                let v = attr(&body[open..sel + 9], "value").unwrap_or_default();
                out.push((n, v));
            }
        }
        rest = &rest[1..];
    }
    out
}

fn sign_in(bind: &str, link_url: &str) -> String {
    let rest = link_url.split_once("://").unwrap().1;
    let path = &rest[rest.find('/').unwrap()..];
    let page = http(bind, "GET", path, &[], "");
    assert_eq!(page.status, 200, "{}", page.body);
    let csrf = page.cookie("maru_csrf").unwrap();
    let cookie = format!("maru_csrf={csrf}");
    let res = http(
        bind,
        "POST",
        path,
        &[FORM, ("Cookie", &cookie)],
        &format!("_csrf={csrf}"),
    );
    assert_eq!(res.status, 303, "{}", res.body);
    res.cookie("maru_session").unwrap()
}

#[test]
fn site_structure_members_settings_and_design_from_admin_screens() {
    let tmp = tempfile::tempdir().unwrap();
    let data = tmp.path().join("사이트");
    maru(
        &data,
        &["init", "--no-samples", "--preset", "blog", "--yes"],
    );
    maru(
        &data,
        &[
            "site",
            "set",
            "--signup",
            "open",
            "--captcha",
            "off",
            "--signup-verify",
            "false",
        ],
    );
    let link = maru(&data, &["login-link"]);
    let (_server, bind) = start_server(&data);
    let session = sign_in(&bind, link["url"].as_str().unwrap());
    let cookie = format!("maru_session={session}");
    let get = |path: &str| {
        let r = http(&bind, "GET", path, &[("Cookie", &cookie)], "");
        assert_eq!(r.status, 200, "{path}: {}", r.body);
        r
    };
    let csrf = {
        let page = get("/admin/boards");
        let at = page.body.find("name=\"_csrf\" value=\"").unwrap() + 20;
        page.body[at..at + page.body[at..].find('"').unwrap()].to_owned()
    };
    let post = |path: &str, pairs: &[(&str, &str)]| {
        let mut all = vec![("_csrf", csrf.as_str())];
        all.extend_from_slice(pairs);
        http(
            &bind,
            "POST",
            path,
            &[FORM, ("Cookie", &cookie)],
            &form(&all),
        )
    };

    // ---- 게시판: 만들기 → 정책(M4)·여분 칸 → 지우기
    let res = post(
        "/admin/boards",
        &[("name", "장터"), ("slug", "market"), ("kind", "free")],
    );
    assert_eq!(res.status, 303, "{}", res.body);
    assert_eq!(res.location(), "/admin/boards/market?done=created");
    let dup = post(
        "/admin/boards",
        &[("name", "또 장터"), ("slug", "market"), ("kind", "free")],
    );
    assert_eq!(dup.status, 422, "같은 주소");
    assert!(dup.body.contains("또 장터"), "쓴 값을 다시 채운다");
    let edit = get("/admin/boards/market");
    assert!(edit.body.contains("역할별 권한"));
    let mut values = form_values(&edit.body);
    let set = |values: &mut Vec<(String, String)>, k: &str, v: &str| {
        values.retain(|(n, _)| n != k);
        values.push((k.into(), v.into()));
    };
    set(&mut values, "p_write", "editor");
    set(&mut values, "p_comment", "none");
    set(&mut values, "layout", "card");
    values.push((
        "field_schema".into(),
        r#"[{"key":"price","label":"가격","type":"number"}]"#.into(),
    ));
    let body: Vec<(&str, &str)> = values
        .iter()
        .filter(|(n, _)| n != "_csrf")
        .map(|(n, v)| (n.as_str(), v.as_str()))
        .collect();
    let res = post("/admin/boards/market", &body);
    assert_eq!(res.status, 303, "{}", res.body);
    let board = maru(&data, &["board", "show", "market"]);
    assert_eq!(board["policy"]["write"], "editor", "{board}");
    assert_eq!(board["policy"]["comment"], "none");
    assert_eq!(board["layout"], "card");
    assert_eq!(board["field_schema"][0]["key"], "price");
    let mut bad = body.clone();
    bad.retain(|(n, _)| *n != "p_list");
    bad.push(("p_list", "robot"));
    let res = post("/admin/boards/market", &bad);
    assert_eq!(res.status, 422, "{}", res.body);
    assert!(res.body.contains("robot"));
    let res = post("/admin/boards/market/delete", &[]);
    assert_eq!(res.status, 303);
    assert!(
        !maru(&data, &["board", "list"])
            .as_array()
            .unwrap()
            .iter()
            .any(|b| b["slug"] == "market")
    );

    // ---- 페이지: 만들기 → 본문 저장(주소 바꾸기) → 발행
    let res = post(
        "/admin/pages",
        &[("title", "이용 약관"), ("slug", "terms-draft")],
    );
    assert_eq!(res.status, 303, "{}", res.body);
    let res = post(
        "/admin/pages/terms-draft",
        &[
            ("title", "이용 약관"),
            ("slug", "terms"),
            ("layout", "default"),
            ("sort_order", "5"),
            ("body_md", "## 제1조\n\n함께 씁니다."),
        ],
    );
    assert_eq!(res.status, 303, "{}", res.body);
    assert_eq!(res.location(), "/admin/pages/terms?done=saved");
    let res = post("/admin/pages/terms/publish", &[]);
    assert_eq!(res.status, 303);
    let page = maru(&data, &["page", "show", "terms"]);
    assert_eq!(page["status"], "published");
    assert_eq!(page["sort_order"], 5);
    let public = http(&bind, "GET", "/p/terms", &[], "");
    assert_eq!(public.status, 200);
    assert!(public.body.contains("제1조"));
    let res = post(
        "/admin/pages/terms",
        &[
            ("title", "이용 약관"),
            ("slug", "terms"),
            ("sort_order", "다섯"),
        ],
    );
    assert_eq!(res.status, 422);

    // ---- 메뉴: 첫 화면 / 블로그 / 그 아래 약관
    let blog = maru(&data, &["board", "list"])[0]["slug"]
        .as_str()
        .unwrap()
        .to_owned();
    let res = post(
        "/admin/menus/header",
        &[
            ("r0_order", "10"),
            ("r0_label", "처음"),
            ("r0_kind", "home"),
            ("r0_target", ""),
            ("r1_order", "20"),
            ("r1_label", "글"),
            ("r1_kind", "board"),
            ("r1_target", &blog),
            ("r2_order", "30"),
            ("r2_label", "약관"),
            ("r2_kind", "page"),
            ("r2_target", "terms"),
            ("r2_child", "1"),
            ("r3_order", "40"),
            ("r3_label", ""),
            ("r3_kind", "url"),
            ("r3_target", ""),
        ],
    );
    assert_eq!(res.status, 303, "{}", res.body);
    let menu = maru(&data, &["menu", "show", "header"]);
    assert_eq!(menu.as_array().unwrap().len(), 2, "{menu}");
    assert_eq!(menu[1]["children"][0]["link"]["slug"], "terms");
    let res = post(
        "/admin/menus/header",
        &[
            ("r0_label", "고아"),
            ("r0_kind", "url"),
            ("r0_target", "/x"),
            ("r0_child", "1"),
        ],
    );
    assert_eq!(res.status, 422);
    assert!(res.body.contains("고아"), "보낸 줄을 다시 보인다");
    assert_eq!(
        maru(&data, &["menu", "show", "header"])
            .as_array()
            .unwrap()
            .len(),
        2,
        "실패하면 그대로"
    );
    assert!(get("/admin/menus").body.contains("value=\"약관\""));

    // ---- 회원: 가입한 사람을 편집자로 → 사용 멈춤 → 다시
    let page = http(&bind, "GET", "/signup", &[], "");
    let pre = page.cookie("maru_csrf").unwrap();
    let res = http(
        &bind,
        "POST",
        "/signup",
        &[FORM, ("Cookie", &format!("maru_csrf={pre}"))],
        &form(&[
            ("_csrf", &pre),
            ("email", "seed@example.test"),
            ("display_name", "씨앗"),
            ("password", "test-pass-1234"),
            ("password2", "test-pass-1234"),
        ]),
    );
    assert!(res.status == 303 || res.status == 200, "{}", res.body);
    let member = maru(&data, &["member", "show", "seed@example.test"]);
    let id = member["id"].as_str().unwrap().to_owned();
    let list = get("/admin/members");
    assert!(list.body.contains("seed@example.test"));
    let res = post(
        &format!("/admin/members/{id}/role"),
        &[("role", "editor"), ("page", "1")],
    );
    assert_eq!(res.status, 303, "{}", res.body);
    assert_eq!(
        maru(&data, &["member", "show", &id])["roles"],
        serde_json::json!(["editor"])
    );
    let res = post(&format!("/admin/members/{id}/disable"), &[("page", "1")]);
    assert_eq!(res.status, 303);
    assert_eq!(maru(&data, &["member", "show", &id])["status"], "disabled");
    let res = post(&format!("/admin/members/{id}/enable"), &[]);
    assert_eq!(res.status, 303);
    assert_eq!(maru(&data, &["member", "show", &id])["status"], "active");

    // ---- 설정: 언어·시간대는 고르기 목록이다 (ADR-0137)
    let page = get("/admin/settings").body;
    assert!(
        page.contains("한국·일본 (UTC+9)") && page.contains(">한국어</option>"),
        "{page}"
    );
    let mut values = form_values(&page);
    let set_v = |values: &mut Vec<(String, String)>, k: &str, v: &str| {
        values.retain(|(n, _)| n != k);
        values.push((k.into(), v.into()));
    };
    set_v(&mut values, "name", "새 이름 사이트");
    set_v(&mut values, "posts_per_page", "7");
    values.push(("description".into(), "관리 화면에서 고친 소개".into()));
    values.push(("show_views".into(), "1".into()));
    let body: Vec<(&str, &str)> = values
        .iter()
        .filter(|(n, _)| n != "_csrf")
        .map(|(n, v)| (n.as_str(), v.as_str()))
        .collect();
    let res = post("/admin/settings", &body);
    assert_eq!(res.status, 303, "{}", res.body);
    let site = maru(&data, &["site", "show"]);
    assert_eq!(site["name"], "새 이름 사이트");
    assert_eq!(site["posts_per_page"], 7);
    assert_eq!(site["show_views"], true);
    assert_eq!(site["show_search"], false, "꺼진 체크 칸은 끈다");
    let mut bad = body.clone();
    bad.retain(|(n, _)| *n != "tz_offset_min");
    bad.push(("tz_offset_min", "서울"));
    assert_eq!(post("/admin/settings", &bad).status, 422);

    // ---- 디자인: 강조색 바꾸기, 대비가 낮으면 거부, 기본값으로
    let values = form_values(&get("/admin/design").body);
    let mut body: Vec<(&str, &str)> = values
        .iter()
        .filter(|(n, _)| n != "_csrf" && n != "color__primary")
        .map(|(n, v)| (n.as_str(), v.as_str()))
        .collect();
    body.push(("color__primary", "#123456"));
    let res = post("/admin/design", &body);
    assert_eq!(res.status, 303, "{}", res.body);
    assert_eq!(
        maru(&data, &["design", "show"])["color"]["primary"],
        "#123456"
    );
    let bg = values
        .iter()
        .find(|(n, _)| n == "color__bg")
        .unwrap()
        .1
        .clone();
    let mut low: Vec<(&str, &str)> = body
        .iter()
        .copied()
        .filter(|(n, _)| *n != "color__fg")
        .collect();
    low.push(("color__fg", &bg));
    let res = post("/admin/design", &low);
    assert_eq!(res.status, 422, "{}", res.body);
    let res = post("/admin/design/reset", &[]);
    assert_eq!(res.status, 303);
    assert_ne!(
        maru(&data, &["design", "show"])["color"]["primary"],
        "#123456"
    );
    let audit = maru(&data, &["audit", "list", "--action", "design."]);
    assert!(audit["total"].as_i64().unwrap() >= 2, "{audit}");

    // ---- 테마 카드: 입히면 바로 바뀌고, 다른 테마로 바꾸면 앞 테마의 바탕색이 남지 않는다
    let res = post("/admin/design/theme", &[("theme", "cafe")]);
    assert_eq!(res.status, 303, "{}", res.body);
    assert_eq!(maru(&data, &["design", "show"])["color"]["bg"], "#fbf8f2");
    let page = get("/admin/design").body;
    assert!(page.contains("지금 스킨") && page.contains("이 스킨 입히기"));
    let res = post("/admin/design/theme", &[("theme", "community")]);
    assert_eq!(res.status, 303, "{}", res.body);
    let tokens = maru(&data, &["design", "show"]);
    assert_eq!(tokens["color"]["primary"], "#2754c5");
    assert_eq!(tokens["color"]["bg"], "#fbfbfa", "{tokens}");
    assert_eq!(
        post("/admin/design/theme", &[("theme", "없는테마")]).status,
        404
    );

    // ---- 테마 = 사이트 종류. 고르면 바로 바꾸지 않고 변경 묶음을 만든다 (ADR-0137)
    let page = get("/admin/themes").body;
    assert!(
        page.contains("회사 소개") && page.contains("미리 보고 고르기"),
        "{page}"
    );
    let res = post("/admin/themes", &[("preset", "company")]);
    assert_eq!(res.status, 303, "{}", res.body);
    let at = res.location();
    assert!(at.starts_with("/admin/changesets/"), "{at}");
    let cs = get(&at).body;
    assert!(
        cs.contains("아직 사이트는 그대로입니다") && cs.contains("게시판 갖추기"),
        "{cs}"
    );
    assert_eq!(post("/admin/themes", &[("preset", "없는종류")]).status, 404);

    // ---- 카드 그림은 실제 화면 썸네일이다 (ADR-0141). 종류·스킨마다 하나씩 있고 그림으로 내려온다
    let types = get("/admin/themes").body;
    let skins = get("/admin/design").body;
    for id in ["community", "shop", "clinic", "realty", "linkbio"] {
        let src = format!("/admin/assets/shots/site-{id}.webp");
        assert!(types.contains(&src), "{src}");
        assert!(
            get(&src).head.to_ascii_lowercase().contains("image/webp"),
            "{src}"
        );
    }
    for id in ["default", "cafe", "community"] {
        let src = format!("/admin/assets/shots/skin-{id}.webp");
        assert!(skins.contains(&src), "{src}");
        assert!(
            get(&src).head.to_ascii_lowercase().contains("image/webp"),
            "{src}"
        );
    }
    assert!(!types.contains("class=\"site-art\""), "견본 그림이 남았다");

    // ---- 위젯: 놓고, 사람 말로 보이고, 제목·보일 곳·설정을 고친다
    let res = post(
        "/admin/widgets",
        &[
            ("kind", "latest_posts"),
            ("region", "sidebar"),
            ("scope", "all"),
            ("title", "새 글"),
            ("config", "{\"limit\": 3}"),
        ],
    );
    assert_eq!(res.status, 303, "{}", res.body);
    let list = maru(&data, &["widget", "list"]);
    let items = list
        .as_array()
        .or(list["items"].as_array())
        .unwrap()
        .clone();
    let w = items
        .iter()
        .find(|w| w["title"] == "새 글")
        .expect("놓았다");
    let id = w["id"].as_str().unwrap().to_owned();
    let res = post(
        &format!("/admin/widgets/{id}/edit"),
        &[
            ("title", "고친 새 글"),
            ("scope", "home"),
            ("config", "{\"limit\": 9, \"view\": \"card\"}"),
        ],
    );
    assert_eq!(res.status, 303, "{}", res.body);
    let list = maru(&data, &["widget", "list"]);
    let items = list
        .as_array()
        .or(list["items"].as_array())
        .unwrap()
        .clone();
    let w = items.iter().find(|w| w["id"] == id.as_str()).unwrap();
    assert_eq!(w["title"], "고친 새 글");
    assert_eq!(w["scope"], "home");
    assert_eq!(w["config"]["limit"], 9, "{w}");
    let page = get("/admin/widgets").body;
    assert!(
        page.contains("첫 화면에 보임") && page.contains(">9개<") && page.contains(">사진 카드<"),
        "{page}"
    );
    let res = post(
        &format!("/admin/widgets/{id}/edit"),
        &[("title", "x"), ("scope", "home"), ("config", "{깨진")],
    );
    assert_eq!(res.status, 422, "깨진 설정은 거절하고 칸을 다시 보인다");
    assert!(res.body.contains("{깨진"));

    // ---- 활동 기록: 사람 말로
    let page = get("/admin/audit").body;
    assert!(page.contains("위젯을 고쳤습니다"), "{page}");

    // ---- 대시보드: 처음 할 일과 현황, 글 목록은 따로
    let home = get("/admin").body;
    assert!(
        home.contains("시작하기") && home.contains("최근 14일 방문자"),
        "{home}"
    );
    assert!(
        home.contains("지금 스킨") && home.contains("커뮤니티"),
        "{home}"
    );
    assert!(get("/admin/posts").body.contains("글 검색"));

    // 로그인 안 했으면 로그인 화면으로, CSRF 가 틀리면 거절
    let anon = http(&bind, "GET", "/admin/settings", &[], "");
    assert_eq!(anon.status, 303);
    let forged = http(
        &bind,
        "POST",
        "/admin/settings",
        &[FORM, ("Cookie", &cookie)],
        "_csrf=wrong&name=x",
    );
    assert_eq!(forged.status, 403);
}
