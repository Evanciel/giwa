//! ADR-0138: 화면 편집기. 실제 바이너리·실제 포트·실제 DB 파일로 편집기 화면·캔버스·발행을 거치고
//! CLI 로 DB 에 남은 위젯을 확인한다. 끌어다 놓기 같은 브라우저 동작은 브라우저에서 따로 본다.
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
    let text = String::from_utf8(raw).unwrap();
    let (head, body) = text.split_once("\r\n\r\n").unwrap_or((&text, ""));
    Res {
        status: head[9..12].parse().unwrap(),
        head: head.to_owned(),
        body: body.to_owned(),
    }
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

fn header<'a>(res: &'a Res, name: &str) -> &'a str {
    res.head
        .lines()
        .find_map(|l| {
            let (k, v) = l.split_once(':')?;
            k.trim().eq_ignore_ascii_case(name).then(|| v.trim())
        })
        .unwrap_or("")
}

fn draft_of(list: &Value) -> Vec<Value> {
    list.as_array()
        .unwrap()
        .iter()
        .map(|w| {
            serde_json::json!({
                "id": w["id"], "kind": w["kind"], "region": w["region"], "scope": w["scope"],
                "title": w["title"], "config": w["config"], "enabled": w["enabled"],
                "hide_on": w["hide_on"],
            })
        })
        .collect()
}

#[test]
fn edit_screen_draw_draft_publish_and_revert() {
    let tmp = tempfile::tempdir().unwrap();
    let data = tmp.path().join("사이트");
    maru(
        &data,
        &["init", "--no-samples", "--preset", "blog", "--yes"],
    );
    let link = maru(&data, &["login-link"]);
    let (_server, bind) = start_server(&data);

    // 로그인하지 않으면 로그인으로 보낸다.
    let r = http(&bind, "GET", "/admin/editor?screen=home", &[], "");
    assert_eq!(r.status, 303);
    assert!(r.location().starts_with("/admin/login"), "{}", r.location());

    let session = sign_in(&bind, link["url"].as_str().unwrap());
    let cookie = format!("maru_session={session}");
    let page = http(
        &bind,
        "GET",
        "/admin/editor?screen=home",
        &[("Cookie", &cookie)],
        "",
    );
    assert_eq!(page.status, 200, "{}", page.body);
    assert!(page.body.contains("id=\"editor-data\""));
    assert!(page.body.contains("/admin/assets/page-editor.js"));
    // 캔버스 iframe 을 담으려고 자기 CSP 를 쓴다.
    let csp = header(&page, "content-security-policy");
    assert!(csp.contains("frame-src 'self'"), "{csp}");
    assert!(csp.contains("frame-ancestors 'none'"), "{csp}");
    let csrf = {
        let at = page.body.find("data-csrf=\"").unwrap() + 11;
        page.body[at..at + page.body[at..].find('"').unwrap()].to_owned()
    };
    let bad = http(
        &bind,
        "GET",
        "/admin/editor?screen=post:x",
        &[("Cookie", &cookie)],
        "",
    );
    assert_ne!(bad.status, 200);

    // 캔버스: 실제 공개 화면에 편집 표시가 붙고, 같은 출처에서만 담을 수 있다.
    let frame = http(
        &bind,
        "GET",
        "/admin/editor/frame?screen=home",
        &[("Cookie", &cookie)],
        "",
    );
    assert_eq!(frame.status, 200, "{}", frame.body);
    assert!(frame.body.contains("data-maru-region=\"main\""));
    assert!(frame.body.contains("data-maru-region=\"hero\""));
    assert!(header(&frame, "content-security-policy").contains("frame-ancestors 'self'"));
    assert!(header(&frame, "cache-control").contains("no-store"));
    // 공개 화면에는 편집 표시가 없다.
    let public = http(&bind, "GET", "/", &[], "");
    assert!(!public.body.contains("data-maru"), "공개 화면에 편집 표시");

    // 초안 그리기: 새 블록이 캔버스에만 보이고 DB·공개 화면은 그대로다.
    let before = maru(&data, &["widget", "list"]);
    let mut draft = draft_of(&before);
    draft.push(serde_json::json!({
        "id": "new-1", "kind": "markdown_block", "region": "hero", "scope": "home",
        "title": "", "config": { "body": "편집기 새 블록" }, "enabled": true,
    }));
    draft.push(serde_json::json!({
        "id": "new-2", "kind": "faq", "region": "main", "scope": "home",
        "title": "", "config": { "items": "틀린 값" }, "enabled": true,
    }));
    let body = serde_json::json!({ "widgets": draft }).to_string();
    let json = ("Content-Type", "application/json");
    let no_csrf = http(
        &bind,
        "POST",
        "/admin/editor/frame?screen=home",
        &[json, ("Cookie", &cookie)],
        &body,
    );
    assert_eq!(no_csrf.status, 403);
    let drawn = http(
        &bind,
        "POST",
        "/admin/editor/frame?screen=home",
        &[json, ("Cookie", &cookie), ("x-csrf-token", &csrf)],
        &body,
    );
    assert_eq!(drawn.status, 200, "{}", drawn.body);
    assert!(drawn.body.contains("편집기 새 블록"));
    assert!(drawn.body.contains("data-maru-w=\"new-1\""));
    // 설정이 틀린 블록은 빼고 그리고 그 id 를 알린다.
    assert_eq!(header(&drawn, "x-maru-invalid"), "new-2");
    assert_eq!(maru(&data, &["widget", "list"]), before);
    assert!(
        !http(&bind, "GET", "/", &[], "")
            .body
            .contains("편집기 새 블록")
    );

    // 글자 바로 고치기 (ADR-0144): 캔버스의 글자 칸에만 설정 경로가 붙는다.
    let mut texts = draft_of(&before);
    texts.push(serde_json::json!({
        "id": "new-3", "kind": "hero", "region": "hero", "scope": "home", "title": "",
        "config": { "title": "바로 고칠 제목", "button": { "label": "보기", "link": "/" } }, "enabled": true,
    }));
    texts.push(serde_json::json!({
        "id": "new-4", "kind": "features", "region": "main", "scope": "home", "title": "묶음 제목",
        "config": { "items": [{ "title": "첫째", "text": "설명" }, { "title": "둘째", "link": "/" }] }, "enabled": true,
    }));
    let body = serde_json::json!({ "widgets": texts }).to_string();
    let drawn_texts = http(
        &bind,
        "POST",
        "/admin/editor/frame?screen=home",
        &[json, ("Cookie", &cookie), ("x-csrf-token", &csrf)],
        &body,
    );
    assert_eq!(drawn_texts.status, 200, "{}", drawn_texts.body);
    for field in [
        "data-maru-f=\"title\">바로 고칠 제목",
        "data-maru-f=\"button.label\">보기",
        "data-maru-f=\"@title\">묶음 제목",
        "data-maru-f=\"items.0.title\">첫째",
        "data-maru-f=\"items.0.text\">설명",
        "data-maru-f=\"items.1.title\">둘째",
    ] {
        assert!(drawn_texts.body.contains(field), "{field}");
    }

    // 발행: 변경 묶음 하나로 적용되어 공개 화면에 보인다.
    draft.pop();
    let publish = |widgets: &[Value]| {
        let body = serde_json::json!({ "screen": "home", "widgets": widgets }).to_string();
        let r = http(
            &bind,
            "POST",
            "/admin/editor/publish",
            &[json, ("Cookie", &cookie), ("x-csrf-token", &csrf)],
            &body,
        );
        assert_eq!(r.status, 200, "{}", r.body);
        serde_json::from_str::<Value>(&r.body).unwrap()["data"].clone()
    };
    let first = publish(&draft);
    let cs = first["changeset"].as_str().unwrap().to_owned();
    assert_eq!(first["ops"], 1);
    let shown = http(&bind, "GET", "/", &[], "");
    assert!(shown.body.contains("편집기 새 블록"), "발행 뒤 공개 화면");
    let after = maru(&data, &["widget", "list"]);
    let placed = after
        .as_array()
        .unwrap()
        .iter()
        .find(|w| w["kind"] == "markdown_block" && w["region"] == "hero")
        .expect("새 블록이 DB 에")
        .clone();
    assert_eq!(placed["scope"], "home");
    let cs_view = maru(&data, &["changeset", "show", &cs]);
    assert_eq!(cs_view["status"], "applied");
    assert_eq!(cs_view["title"], "화면 편집: 첫 화면");

    // 같은 초안을 다시 발행하면 할 일이 없다.
    let again = publish(&draft_of(&after));
    assert!(again["changeset"].is_null(), "{again}");

    // 숨기기·순서: 숨긴 블록은 DB 에 남고 공개 화면에서 빠진다.
    let mut hidden = draft_of(&after);
    for w in hidden.iter_mut() {
        if w["id"] == placed["id"] {
            w["enabled"] = Value::Bool(false);
        }
    }
    hidden.reverse();
    let second = publish(&hidden);
    let cs2 = second["changeset"].as_str().unwrap().to_owned();
    assert!(
        !http(&bind, "GET", "/", &[], "")
            .body
            .contains("편집기 새 블록")
    );
    let kept = maru(&data, &["widget", "list"]);
    assert!(
        kept.as_array()
            .unwrap()
            .iter()
            .any(|w| w["id"] == placed["id"] && w["enabled"] == false)
    );

    // 되돌리기: 그 묶음 하나만 되돌리면 다시 보인다.
    maru(&data, &["changeset", "revert", &cs2]);
    assert!(
        http(&bind, "GET", "/", &[], "")
            .body
            .contains("편집기 새 블록")
    );
    // 고친 시각만 다르고 나머지는 숨기기 전과 같다.
    let strip = |v: Value| {
        let mut v = v;
        for w in v.as_array_mut().unwrap() {
            w.as_object_mut().unwrap().remove("updated_at");
        }
        v
    };
    assert_eq!(strip(maru(&data, &["widget", "list"])), strip(after));

    // 사이트 공통 부분 (ADR-0145): 이름·메뉴 초안이 캔버스에만 보이고, 발행하면 같은 묶음에 들어간다.
    let canvas = http(
        &bind,
        "GET",
        "/admin/editor/frame?screen=home",
        &[("Cookie", &cookie)],
        "",
    );
    assert!(
        canvas.body.contains("data-maru-s=\"name\""),
        "사이트 이름 표시"
    );
    let widgets_now = draft_of(&maru(&data, &["widget", "list"]));
    let site_body = |name: &str, label: &str| {
        serde_json::json!({
            "screen": "home",
            "widgets": widgets_now,
            "site": {
                "name": name,
                "description": "",
                "menus": { "header": [{ "label": label, "link": { "kind": "url", "href": "/recent" }, "children": [] }] },
            },
        })
        .to_string()
    };
    let drawn_site = http(
        &bind,
        "POST",
        "/admin/editor/frame?screen=home",
        &[json, ("Cookie", &cookie), ("x-csrf-token", &csrf)],
        &site_body("편집기 이름", "새 메뉴 이름"),
    );
    assert_eq!(drawn_site.status, 200, "{}", drawn_site.body);
    assert!(drawn_site.body.contains("편집기 이름"));
    assert!(
        drawn_site
            .body
            .contains("data-maru-f=\"menus.header.0.label\">새 메뉴 이름")
    );
    assert!(
        !http(&bind, "GET", "/", &[], "")
            .body
            .contains("편집기 이름")
    );
    // 틀린 값은 저장된 값으로 그리고 알린다.
    let bad_site = http(
        &bind,
        "POST",
        "/admin/editor/frame?screen=home",
        &[json, ("Cookie", &cookie), ("x-csrf-token", &csrf)],
        &site_body(" ", ""),
    );
    assert_eq!(
        header(&bad_site, "x-maru-invalid"),
        "site:name,site:menu.header"
    );
    let published = http(
        &bind,
        "POST",
        "/admin/editor/publish",
        &[json, ("Cookie", &cookie), ("x-csrf-token", &csrf)],
        &site_body("편집기 이름", "새 메뉴 이름"),
    );
    assert_eq!(published.status, 200, "{}", published.body);
    let out: Value = serde_json::from_str(&published.body).unwrap();
    assert_eq!(out["data"]["ops"], 2, "{out}");
    let home = http(&bind, "GET", "/", &[], "").body;
    assert!(home.contains("편집기 이름") && home.contains("새 메뉴 이름"));
    assert!(!home.contains("data-maru"), "공개 화면에 편집 표시");
    assert_eq!(maru(&data, &["site", "show"])["name"], "편집기 이름");
    let cs3 = out["data"]["changeset"].as_str().unwrap().to_owned();
    maru(&data, &["changeset", "revert", &cs3]);
    assert_ne!(maru(&data, &["site", "show"])["name"], "편집기 이름");

    // 기기별 숨김 (ADR-0146): 지우지 않고 그 기기 폭에서만 감추는 클래스가 붙는다.
    let mut devices = draft_of(&maru(&data, &["widget", "list"]));
    let set_hide = |list: &mut Vec<Value>, hide: Value| {
        for w in list.iter_mut() {
            if w["id"] == placed["id"] {
                w["hide_on"] = hide.clone();
            }
        }
    };
    set_hide(&mut devices, serde_json::json!(["mobile"]));
    let frame_body = |list: &[Value]| serde_json::json!({ "widgets": list }).to_string();
    let drawn_dev = http(
        &bind,
        "POST",
        "/admin/editor/frame?screen=home",
        &[json, ("Cookie", &cookie), ("x-csrf-token", &csrf)],
        &frame_body(&devices),
    );
    assert_eq!(drawn_dev.status, 200, "{}", drawn_dev.body);
    assert!(
        drawn_dev.body.contains(" hide-mobile\""),
        "캔버스에 숨김 클래스"
    );
    assert!(
        !http(&bind, "GET", "/", &[], "")
            .body
            .contains("hide-mobile")
    );
    // 세 기기 모두는 받지 않는다(그건 숨기기다).
    let mut everywhere = devices.clone();
    set_hide(
        &mut everywhere,
        serde_json::json!(["desktop", "tablet", "mobile"]),
    );
    let bad_dev = http(
        &bind,
        "POST",
        "/admin/editor/frame?screen=home",
        &[json, ("Cookie", &cookie), ("x-csrf-token", &csrf)],
        &frame_body(&everywhere),
    );
    assert_eq!(
        header(&bad_dev, "x-maru-invalid"),
        placed["id"].as_str().unwrap()
    );
    let dev_pub = publish(&devices);
    assert_eq!(dev_pub["ops"], 1, "{dev_pub}");
    let home = http(&bind, "GET", "/", &[], "").body;
    assert!(home.contains("편집기 새 블록") && home.contains(" hide-mobile\""));
    let stored = maru(&data, &["widget", "list"]);
    let stored = stored
        .as_array()
        .unwrap()
        .iter()
        .find(|w| w["id"] == placed["id"])
        .unwrap();
    assert_eq!(stored["hide_on"], serde_json::json!(["mobile"]));
    // CLI 도 같은 서비스: 빈 값이면 모든 기기에 다시 보인다.
    let id = placed["id"].as_str().unwrap();
    let cleared = maru(&data, &["widget", "configure", id, "--hide-on", ""]);
    assert_eq!(cleared["hide_on"], serde_json::json!([]));
    let tablet = maru(
        &data,
        &["widget", "configure", id, "--hide-on", "tablet,desktop"],
    );
    assert_eq!(tablet["hide_on"], serde_json::json!(["desktop", "tablet"]));
}

/// 1×1 PNG
const PNG: &[u8] = &[
    137, 80, 78, 71, 13, 10, 26, 10, 0, 0, 0, 13, 73, 72, 68, 82, 0, 0, 0, 1, 0, 0, 0, 1, 8, 6, 0,
    0, 0, 31, 21, 196, 137, 0, 0, 0, 13, 73, 68, 65, 84, 120, 218, 99, 100, 96, 248, 95, 15, 0, 2,
    135, 1, 128, 235, 71, 186, 146, 0, 0, 0, 0, 73, 69, 78, 68, 174, 66, 96, 130,
];

/// ADR-0147: 그림·링크 고르기. 그림 판 목록, 캔버스의 그림 칸 표시, 고른 그림·링크의 발행.
#[test]
fn editor_picks_images_and_links() {
    let tmp = tempfile::tempdir().unwrap();
    let data = tmp.path().join("사이트");
    maru(
        &data,
        &["init", "--no-samples", "--preset", "blog", "--yes"],
    );
    let file = tmp.path().join("가게.png");
    std::fs::write(&file, PNG).unwrap();
    let up = maru(
        &data,
        &[
            "media",
            "upload",
            file.to_str().unwrap(),
            "--alt",
            "가게 앞",
        ],
    );
    let url = up["url"].as_str().unwrap().to_owned();
    let link = maru(&data, &["login-link"]);
    let (_server, bind) = start_server(&data);
    let session = sign_in(&bind, link["url"].as_str().unwrap());
    let cookie = format!("maru_session={session}");
    let page = http(
        &bind,
        "GET",
        "/admin/editor?screen=home",
        &[("Cookie", &cookie)],
        "",
    );
    assert!(page.body.contains("/admin/assets/media-picker.js"));
    let csrf = {
        let at = page.body.find("data-csrf=\"").unwrap() + 11;
        page.body[at..at + page.body[at..].find('"').unwrap()].to_owned()
    };

    // 그림 판 목록: 로그인과 CSRF 머리글이 있어야 하고, 최근 것부터 보관함 설명까지 준다.
    assert_eq!(
        http(&bind, "GET", "/admin/editor/media", &[], "").status,
        401
    );
    assert_eq!(
        http(
            &bind,
            "GET",
            "/admin/editor/media",
            &[("Cookie", &cookie)],
            ""
        )
        .status,
        403
    );
    let list = http(
        &bind,
        "GET",
        "/admin/editor/media?page=1",
        &[("Cookie", &cookie), ("x-csrf-token", &csrf)],
        "",
    );
    assert_eq!(list.status, 200, "{}", list.body);
    let v: Value = serde_json::from_str(&list.body).unwrap();
    assert_eq!(v["data"]["items"][0]["url"], url.as_str(), "{v}");
    assert_eq!(v["data"]["items"][0]["alt_text"], "가게 앞");
    assert_eq!(v["data"]["more"], false);

    // 캔버스: 그림 칸에 경로 표시가 붙고, 그림이 없으면 「그림 넣기」 자리가 생긴다.
    let mut draft = draft_of(&maru(&data, &["widget", "list"]));
    draft.push(serde_json::json!({
        "id": "new-img", "kind": "image_text", "region": "main", "scope": "home", "title": "",
        "config": { "body": "그림 옆 글", "image": url, "image_alt": "가게 앞" },
        "enabled": true, "hide_on": [],
    }));
    draft.push(serde_json::json!({
        "id": "new-empty", "kind": "image_text", "region": "main", "scope": "home", "title": "",
        "config": { "body": "그림 없는 글" }, "enabled": true, "hide_on": [],
    }));
    draft.push(serde_json::json!({
        "id": "new-btn", "kind": "cta", "region": "main", "scope": "home", "title": "문의",
        "config": { "text": "연락 주세요", "button": { "label": "문의하기", "link": "/b/notice" } },
        "enabled": true, "hide_on": [],
    }));
    let json = ("Content-Type", "application/json");
    let body = serde_json::json!({ "widgets": draft }).to_string();
    let drawn = http(
        &bind,
        "POST",
        "/admin/editor/frame?screen=home",
        &[json, ("Cookie", &cookie), ("x-csrf-token", &csrf)],
        &body,
    );
    assert_eq!(drawn.status, 200, "{}", drawn.body);
    assert_eq!(header(&drawn, "x-maru-invalid"), "", "{}", drawn.body);
    assert!(
        drawn
            .body
            .contains(" data-maru-img=\"image\" data-maru-alt=\"image_alt\">"),
        "그림 칸 표시"
    );
    assert!(
        drawn.body.contains(
            "<div class=\"maru-img-empty\" data-maru-img=\"image\" data-maru-alt=\"image_alt\">"
        ),
        "빈 그림 자리"
    );
    assert!(drawn.body.contains("data-maru-f=\"button.label\""));

    // 발행: 고른 그림·링크가 DB 에 남고, 공개 화면에는 편집 표시가 없다.
    let published = http(
        &bind,
        "POST",
        "/admin/editor/publish",
        &[json, ("Cookie", &cookie), ("x-csrf-token", &csrf)],
        &serde_json::json!({ "screen": "home", "widgets": draft }).to_string(),
    );
    assert_eq!(published.status, 200, "{}", published.body);
    let home = http(&bind, "GET", "/", &[], "").body;
    assert!(home.contains("그림 옆 글") && home.contains(&url));
    assert!(home.contains("href=\"/b/notice\""));
    assert!(!home.contains("data-maru"), "공개 화면에 편집 표시");
    assert!(!home.contains("maru-img-empty"), "공개 화면에 빈 그림 자리");
    let stored = maru(&data, &["widget", "list"]);
    let it = stored
        .as_array()
        .unwrap()
        .iter()
        .find(|w| w["config"]["body"] == "그림 옆 글")
        .unwrap();
    assert_eq!(it["config"]["image"], url.as_str());
    assert_eq!(it["config"]["image_alt"], "가게 앞");
}

/// ADR-0148: 블록 꾸미기(여백·정렬)와 사이트 디자인 판. 토큰 초안은 캔버스용 CSS 로만 돌아오고,
/// 발행하면 블록과 같은 변경 묶음의 `tokens.set` 이 되며 되돌리면 원래 색이다.
#[test]
fn editor_styles_blocks_and_site_design() {
    let tmp = tempfile::tempdir().unwrap();
    let data = tmp.path().join("사이트");
    maru(
        &data,
        &["init", "--no-samples", "--preset", "blog", "--yes"],
    );
    let link = maru(&data, &["login-link"]);
    let (_server, bind) = start_server(&data);
    let session = sign_in(&bind, link["url"].as_str().unwrap());
    let cookie = format!("maru_session={session}");
    let page = http(
        &bind,
        "GET",
        "/admin/editor?screen=home",
        &[("Cookie", &cookie)],
        "",
    );
    assert!(page.body.contains("\"fonts\":"), "편집기 재료에 서체 목록");
    assert!(page.body.contains("data-tab=\"design\""));
    let csrf = {
        let at = page.body.find("data-csrf=\"").unwrap() + 11;
        page.body[at..at + page.body[at..].find('"').unwrap()].to_owned()
    };
    let json = ("Content-Type", "application/json");
    let auth = [
        json,
        ("Cookie", cookie.as_str()),
        ("x-csrf-token", csrf.as_str()),
    ];
    let before_primary = maru(&data, &["design", "show"])["color"]["primary"].clone();

    // 꾸미기: 여백·정렬이 띠 클래스가 되고, 모르는 값은 알린다.
    let mut draft = draft_of(&maru(&data, &["widget", "list"]));
    draft.push(serde_json::json!({
        "id": "new-cta", "kind": "cta", "region": "main", "scope": "home", "title": "문의",
        "config": { "text": "연락 주세요", "space": "roomy", "align": "center" },
        "enabled": true, "hide_on": [],
    }));
    let drawn = http(
        &bind,
        "POST",
        "/admin/editor/frame?screen=home",
        &auth,
        &serde_json::json!({ "widgets": draft }).to_string(),
    );
    assert_eq!(drawn.status, 200, "{}", drawn.body);
    assert!(
        drawn.body.contains("tone-strong space-roomy align-center"),
        "띠 클래스"
    );
    let mut wrong = draft.clone();
    wrong.last_mut().unwrap()["config"]["space"] = serde_json::json!("huge");
    let bad = http(
        &bind,
        "POST",
        "/admin/editor/frame?screen=home",
        &auth,
        &serde_json::json!({ "widgets": wrong }).to_string(),
    );
    assert_eq!(header(&bad, "x-maru-invalid"), "new-cta");

    // 디자인 판: 저장하지 않고 검사한 CSS 를 돌려준다. 대비가 모자라면 거절한다.
    let css = http(
        &bind,
        "POST",
        "/admin/editor/tokens",
        &auth,
        r##"{"tokens":{"color":{"primary":"#1d4ed8"},"radius":{"sm":2,"md":4,"lg":6}}}"##,
    );
    assert_eq!(css.status, 200, "{}", css.body);
    assert!(header(&css, "content-type").starts_with("text/css"));
    assert!(css.body.contains("#1d4ed8") && css.body.contains("--radius-md: 4px;"));
    let weak = http(
        &bind,
        "POST",
        "/admin/editor/tokens",
        &auth,
        r##"{"tokens":{"color":{"primary":"#f4f4f4"}}}"##,
    );
    assert_eq!(weak.status, 400, "{}", weak.body);
    assert!(weak.body.contains("\"ok\":false"));
    assert_eq!(
        http(
            &bind,
            "POST",
            "/admin/editor/tokens",
            &[json, ("Cookie", &cookie)],
            r#"{"tokens":{}}"#
        )
        .status,
        403
    );
    assert_eq!(
        maru(&data, &["design", "show"])["color"]["primary"],
        before_primary,
        "미리보기는 저장하지 않는다"
    );

    // 발행: 블록과 디자인이 한 묶음. 되돌리면 원래 색.
    let published = http(
        &bind,
        "POST",
        "/admin/editor/publish",
        &auth,
        &serde_json::json!({
            "screen": "home", "widgets": draft,
            "tokens": { "color": { "primary": "#1d4ed8" } },
        })
        .to_string(),
    );
    assert_eq!(published.status, 200, "{}", published.body);
    let out: Value = serde_json::from_str(&published.body).unwrap();
    assert_eq!(out["data"]["ops"], 2, "{out}");
    assert_eq!(
        maru(&data, &["design", "show"])["color"]["primary"],
        "#1d4ed8"
    );
    let home = http(&bind, "GET", "/", &[], "").body;
    assert!(home.contains("space-roomy align-center"));
    let stored = maru(&data, &["widget", "list"]);
    let cta = stored
        .as_array()
        .unwrap()
        .iter()
        .find(|w| w["kind"] == "cta")
        .unwrap();
    assert_eq!(cta["config"]["space"], "roomy");
    assert_eq!(cta["config"]["align"], "center");
    // 같은 디자인을 다시 보내면 연산이 없다.
    let again = http(
        &bind,
        "POST",
        "/admin/editor/publish",
        &auth,
        &serde_json::json!({
            "screen": "home",
            "widgets": draft_of(&stored),
            "tokens": { "color": { "primary": "#1d4ed8" } },
        })
        .to_string(),
    );
    let again: Value = serde_json::from_str(&again.body).unwrap();
    assert_eq!(again["data"]["ops"], 0, "{again}");
    let cs = out["data"]["changeset"].as_str().unwrap().to_owned();
    maru(&data, &["changeset", "revert", &cs]);
    assert_eq!(
        maru(&data, &["design", "show"])["color"]["primary"],
        before_primary
    );
}

/// ADR-0149: 편집기에서 틀을 골라 공개 전 페이지를 만들고, 발행하면 공개·메뉴, 되돌리면 다시 숨는다.
/// CLI `page new --starter` 는 새로 만들 때만 틀의 블록을 놓는다(멱등).
#[test]
fn editor_creates_page_from_starter() {
    let tmp = tempfile::tempdir().unwrap();
    let data = tmp.path().join("사이트");
    maru(
        &data,
        &["init", "--no-samples", "--preset", "blog", "--yes"],
    );
    let link = maru(&data, &["login-link"]);
    let (_server, bind) = start_server(&data);
    let session = sign_in(&bind, link["url"].as_str().unwrap());
    let cookie = format!("maru_session={session}");
    let page = http(
        &bind,
        "GET",
        "/admin/editor?screen=home",
        &[("Cookie", &cookie)],
        "",
    );
    assert!(page.body.contains("data-new-page"), "새 페이지 단추");
    assert!(page.body.contains("value=\"about\""), "틀 목록");
    let csrf = {
        let at = page.body.find("data-csrf=\"").unwrap() + 11;
        page.body[at..at + page.body[at..].find('"').unwrap()].to_owned()
    };
    let json = ("Content-Type", "application/json");
    let auth = [
        json,
        ("Cookie", cookie.as_str()),
        ("x-csrf-token", csrf.as_str()),
    ];
    let body = r#"{"title":"회사 소개","slug":"about-us","starter":"about"}"#;

    // CSRF 없으면 403, 모르는 틀은 400.
    let no_csrf = http(
        &bind,
        "POST",
        "/admin/editor/pages",
        &[json, ("Cookie", &cookie)],
        body,
    );
    assert_eq!(no_csrf.status, 403);
    let unknown = http(
        &bind,
        "POST",
        "/admin/editor/pages",
        &auth,
        r#"{"title":"x","slug":"x","starter":"nope"}"#,
    );
    assert_eq!(unknown.status, 400, "{}", unknown.body);
    assert!(!maru(&data, &["page", "list"]).to_string().contains("\"x\""));

    // 만들기: 공개 전 페이지. 공개 주소는 404, 캔버스는 그린다.
    let made = http(&bind, "POST", "/admin/editor/pages", &auth, body);
    assert_eq!(made.status, 200, "{}", made.body);
    let made: Value = serde_json::from_str(&made.body).unwrap();
    assert_eq!(made["data"]["screen"], "page:about-us");
    let url = made["data"]["url"].as_str().unwrap().to_owned();
    assert!(url.contains("starter=about"), "{url}");
    assert_eq!(http(&bind, "GET", "/p/about-us", &[], "").status, 404);
    let canvas = http(
        &bind,
        "GET",
        "/admin/editor/frame?screen=page:about-us",
        &[("Cookie", &cookie)],
        "",
    );
    assert_eq!(canvas.status, 200, "공개 전 페이지도 캔버스에 그린다");
    assert_eq!(
        http(&bind, "POST", "/admin/editor/pages", &auth, body).status,
        409,
        "같은 주소는 다시 만들지 않는다"
    );

    // 틀의 블록과 메뉴는 편집기 초안으로 온다.
    let editor = http(
        &bind,
        "GET",
        &format!("{url}&menu=1"),
        &[("Cookie", &cookie)],
        "",
    );
    assert_eq!(editor.status, 200);
    assert!(editor.body.contains("\"page_status\":\"draft\""));
    assert!(editor.body.contains("\"add_menu\":true"));
    assert!(editor.body.contains("\"starter\":[{"));
    assert!(
        editor.body.contains("(공개 전)"),
        "화면 고르기에 공개 전 페이지"
    );

    // 발행: page.publish + 블록 + 메뉴가 한 묶음. 공개되고 메뉴에 보인다.
    let mut draft = draft_of(&maru(&data, &["widget", "list"]));
    draft.push(serde_json::json!({
        "id": "new-hero", "kind": "hero", "region": "main", "scope": "page:about-us", "title": "",
        "config": { "title": "회사 소개" }, "enabled": true, "hide_on": [],
    }));
    let site = maru(&data, &["site", "show"]);
    let published = http(
        &bind,
        "POST",
        "/admin/editor/publish",
        &auth,
        &serde_json::json!({
            "screen": "page:about-us", "widgets": draft,
            "site": {
                "name": site["name"], "description": site["description"],
                "menus": { "header": [{ "label": "회사 소개", "link": { "kind": "page", "slug": "about-us" }, "children": [] }] },
            },
        })
        .to_string(),
    );
    assert_eq!(published.status, 200, "{}", published.body);
    let out: Value = serde_json::from_str(&published.body).unwrap();
    assert_eq!(out["data"]["ops"], 3, "{out}");
    let public = http(&bind, "GET", "/p/about-us", &[], "");
    assert_eq!(public.status, 200);
    assert!(public.body.contains("회사 소개"));
    assert!(
        http(&bind, "GET", "/", &[], "")
            .body
            .contains("href=\"/p/about-us\"")
    );

    // 되돌리면 다시 404.
    let cs = out["data"]["changeset"].as_str().unwrap().to_owned();
    maru(&data, &["changeset", "revert", &cs]);
    assert_eq!(http(&bind, "GET", "/p/about-us", &[], "").status, 404);

    // CLI: 틀의 블록은 새로 만들 때만 놓는다.
    let first = maru(
        &data,
        &[
            "page",
            "new",
            "contact",
            "--title",
            "연락처",
            "--starter",
            "contact",
            "--if-not-exists",
        ],
    );
    assert_eq!(first["created"], true);
    assert_eq!(first["blocks"], 3);
    assert_eq!(first["layout"], "blocks");
    let again = maru(
        &data,
        &[
            "page",
            "new",
            "contact",
            "--title",
            "연락처",
            "--starter",
            "contact",
            "--if-not-exists",
        ],
    );
    assert_eq!(again["created"], false);
    assert_eq!(again["blocks"], 0);
    let on_contact = maru(&data, &["widget", "list"])
        .as_array()
        .unwrap()
        .iter()
        .filter(|w| w["scope"] == "page:contact")
        .count();
    assert_eq!(on_contact, 3);
}

/// ADR-0151: 꺼진 앱의 블록은 「앱 더하기」 재료로 오고, 편집기에서 켜면 그 자리에서 블록 종류가 된다.
#[test]
fn editor_enables_app_blocks() {
    let tmp = tempfile::tempdir().unwrap();
    let data = tmp.path().join("사이트");
    maru(
        &data,
        &["init", "--no-samples", "--preset", "blog", "--yes"],
    );
    assert_eq!(maru(&data, &["module", "show", "map"])["enabled"], false);
    let link = maru(&data, &["login-link"]);
    let (_server, bind) = start_server(&data);
    let session = sign_in(&bind, link["url"].as_str().unwrap());
    let cookie = format!("maru_session={session}");
    let page = http(
        &bind,
        "GET",
        "/admin/editor?screen=home",
        &[("Cookie", &cookie)],
        "",
    );
    assert!(page.body.contains("\"app\":\"map\""), "꺼진 지도 앱의 블록");
    assert!(page.body.contains("\"can_enable\":true"));
    let csrf = {
        let at = page.body.find("data-csrf=\"").unwrap() + 11;
        page.body[at..at + page.body[at..].find('"').unwrap()].to_owned()
    };

    assert_eq!(
        http(
            &bind,
            "POST",
            "/admin/editor/apps/map",
            &[("Cookie", &cookie)],
            ""
        )
        .status,
        403,
        "CSRF 없이는 켜지 않는다"
    );
    let auth = [("Cookie", cookie.as_str()), ("x-csrf-token", csrf.as_str())];
    assert_eq!(
        http(&bind, "POST", "/admin/editor/apps/nope", &auth, "").status,
        404
    );
    assert_eq!(maru(&data, &["module", "show", "map"])["enabled"], false);

    let on = http(&bind, "POST", "/admin/editor/apps/map", &auth, "");
    assert_eq!(on.status, 200, "{}", on.body);
    let on: Value = serde_json::from_str(&on.body).unwrap();
    let kinds = on["data"]["kinds"].as_array().unwrap();
    assert!(kinds.iter().any(|k| k["id"] == "map"), "켠 뒤 지도 블록");
    assert!(on["data"]["types"]["map"].is_object(), "설정 형식");
    assert!(
        !on["data"]["apps"]
            .as_array()
            .unwrap()
            .iter()
            .any(|a| a["app"] == "map")
    );
    assert_eq!(maru(&data, &["module", "show", "map"])["enabled"], true);
    // 다시 눌러도 같다(멱등).
    assert_eq!(
        http(&bind, "POST", "/admin/editor/apps/map", &auth, "").status,
        200
    );
}

/// ADR-0152: 고치던 초안은 서버에 남아 같은 사람의 다른 세션(다른 기기·브라우저)에서 이어 열린다.
/// 에이전트는 REST 로 읽기만 하고, 발행하면 사라진다.
#[test]
fn editor_drafts_follow_the_person() {
    let tmp = tempfile::tempdir().unwrap();
    let data = tmp.path().join("사이트");
    maru(
        &data,
        &["init", "--no-samples", "--preset", "blog", "--yes"],
    );
    let secret = maru(
        &data,
        &[
            "token",
            "create",
            "--name",
            "거들기",
            "--scope",
            "read",
            "--scope",
            "design",
        ],
    )["secret"]
        .as_str()
        .unwrap()
        .to_owned();
    let (_server, bind) = start_server(&data);
    let first = format!(
        "maru_session={}",
        sign_in(&bind, maru(&data, &["login-link"])["url"].as_str().unwrap())
    );
    let second = format!(
        "maru_session={}",
        sign_in(&bind, maru(&data, &["login-link"])["url"].as_str().unwrap())
    );
    assert_ne!(first, second, "다른 세션");
    let page = http(
        &bind,
        "GET",
        "/admin/editor?screen=home",
        &[("Cookie", &first)],
        "",
    );
    assert!(page.body.contains("\"draft\":null"), "처음엔 초안이 없다");
    let csrf = {
        let at = page.body.find("data-csrf=\"").unwrap() + 11;
        page.body[at..at + page.body[at..].find('"').unwrap()].to_owned()
    };
    let json = ("Content-Type", "application/json");
    let body = serde_json::json!({
        "body": { "widgets": [{ "id": "new-1", "kind": "html", "region": "home_main",
            "scope": "home", "title": "다른 기기에서 이어 쓸 블록", "config": {}, "enabled": true }] },
        "base": "12345",
    })
    .to_string();
    let path = "/admin/editor/draft?screen=home";
    assert_eq!(
        http(&bind, "PUT", path, &[json, ("Cookie", &first)], &body).status,
        403,
        "CSRF 없이는 두지 않는다"
    );
    let auth = [
        json,
        ("Cookie", first.as_str()),
        ("x-csrf-token", csrf.as_str()),
    ];
    let put = http(&bind, "PUT", path, &auth, &body);
    assert_eq!(put.status, 200, "{}", put.body);
    let big = serde_json::json!({ "body": { "x": "가".repeat(200_000) }, "base": "1" }).to_string();
    assert_eq!(
        http(&bind, "PUT", path, &auth, &big).status,
        400,
        "크기 한도"
    );
    assert_eq!(
        http(
            &bind,
            "PUT",
            "/admin/editor/draft?screen=post:x",
            &auth,
            &body
        )
        .status,
        400
    );

    // 같은 사람의 다른 세션에서 이어 열린다.
    let other = http(
        &bind,
        "GET",
        "/admin/editor?screen=home",
        &[("Cookie", &second)],
        "",
    );
    assert!(
        other.body.contains("다른 기기에서 이어 쓸 블록"),
        "다른 세션의 초안"
    );
    assert!(other.body.contains("\"base\":\"12345\""));

    // 에이전트는 REST 로 읽는다.
    let bearer = format!("Bearer {secret}");
    let rest = http(
        &bind,
        "GET",
        "/api/v1/editor/drafts/home",
        &[("Authorization", &bearer)],
        "",
    );
    assert_eq!(rest.status, 200, "{}", rest.body);
    let rest: Value = serde_json::from_str(&rest.body).unwrap();
    assert_eq!(rest["data"].as_array().unwrap().len(), 1);
    assert_eq!(rest["data"][0]["screen"], "home");

    // 발행하면(고칠 것이 없어도) 그 화면의 초안을 지운다.
    let now = draft_of(&maru(&data, &["widget", "list"]));
    let body = serde_json::json!({ "screen": "home", "widgets": now }).to_string();
    let r = http(&bind, "POST", "/admin/editor/publish", &auth, &body);
    assert_eq!(r.status, 200, "{}", r.body);
    let after = http(
        &bind,
        "GET",
        "/admin/editor?screen=home",
        &[("Cookie", &second)],
        "",
    );
    assert!(after.body.contains("\"draft\":null"), "발행 뒤 초안이 없다");
    let del = http(&bind, "DELETE", path, &auth, "");
    assert_eq!(del.status, 200, "버리기는 여러 번 해도 같다");
}
