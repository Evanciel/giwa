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
}
