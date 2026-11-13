//! Phase 3: changeset (ADR-0121). 실제 바이너리·실제 DB 파일로, 적용 전에는 사이트가 그대로이고
//! 적용하면 바뀌며 되돌리면 원래대로인지, 중간에 실패하면 아무것도 남지 않는지 본다.
#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};

use serde_json::{Value, json};

const BIN: &str = env!("CARGO_BIN_EXE_maru");

struct Server(Child);

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn run(data: &Path, args: &[&str]) -> Value {
    let out = Command::new(BIN)
        .arg("--data")
        .arg(data)
        .args(args)
        .arg("--json")
        .env_remove("MARU_TOKEN")
        .output()
        .expect("maru 실행");
    let stdout = String::from_utf8(out.stdout).expect("UTF-8 출력");
    serde_json::from_str(stdout.trim()).unwrap_or_else(|e| panic!("JSON 아님: {e}\n{stdout}"))
}

fn maru(data: &Path, args: &[&str]) -> Value {
    let v = run(data, args);
    assert_eq!(v["ok"], true, "{args:?} → {v}");
    v["data"].clone()
}

fn init(tmp: &Path) -> PathBuf {
    let data = tmp.join("데이터");
    maru(
        &data,
        &["init", "--no-samples", "--preset", "blog", "--yes"],
    );
    data
}

fn new_changeset(data: &Path, ops: &Value) -> String {
    let text = ops.to_string();
    let cs = maru(
        data,
        &["changeset", "new", "--title", "시험 변경", "--ops", &text],
    );
    assert_eq!(cs["status"], "preview");
    cs["id"].as_str().unwrap().to_owned()
}

fn board_slugs(data: &Path) -> Vec<String> {
    maru(data, &["board", "list"])
        .as_array()
        .unwrap()
        .iter()
        .map(|b| b["slug"].as_str().unwrap().to_owned())
        .collect()
}

fn start_server(data: &Path) -> (Server, String) {
    let mut child = Command::new(BIN)
        .arg("--data")
        .arg(data)
        .args(["serve", "--bind", "127.0.0.1:0", "--json"])
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .expect("serve 실행");
    let stdout = child.stdout.take().unwrap();
    let server = Server(child);
    let mut line = String::new();
    BufReader::new(stdout).read_line(&mut line).unwrap();
    let v: Value = serde_json::from_str(line.trim()).expect("serve 시작 JSON");
    (server, v["data"]["bind"].as_str().unwrap().to_owned())
}

fn get(bind: &str, path: &str) -> (u16, String) {
    let mut s = TcpStream::connect(bind).unwrap();
    let req = format!("GET {path} HTTP/1.1\r\nHost: {bind}\r\nConnection: close\r\n\r\n");
    s.write_all(req.as_bytes()).unwrap();
    let mut raw = Vec::new();
    s.read_to_end(&mut raw).unwrap();
    let text = String::from_utf8_lossy(&raw).into_owned();
    let (head, body) = text.split_once("\r\n\r\n").unwrap_or((&text, ""));
    (head[9..12].parse().unwrap(), body.to_owned())
}

#[test]
fn 적용하면_사이트가_바뀌고_되돌리면_원래대로다() {
    let tmp = tempfile::tempdir().unwrap();
    let data = init(tmp.path());
    let site_before = maru(&data, &["site", "show"]);
    let design_before = maru(&data, &["design", "show"]);
    let menu_before = maru(&data, &["menu", "show", "header"]);
    let boards_before = board_slugs(&data);

    let id = new_changeset(
        &data,
        &json!([
            { "op": "board.create", "slug": "cs-board", "name": "묶음 게시판", "kind": "forum" },
            { "op": "page.create", "slug": "cs-page", "title": "묶음 쪽" },
            { "op": "page.publish", "slug": "cs-page" },
            { "op": "menu.set", "location": "header", "items": [
                { "label": "묶음", "link": { "kind": "page", "slug": "cs-page" } }
            ] },
            { "op": "tokens.set", "tokens": { "color": { "primary": "#1f4f8f" } } },
            { "op": "settings.set", "values": { "name": "묶음 사이트" } },
            { "op": "widget.place", "kind": "latest_posts", "region": "sidebar", "title": "묶음 위젯",
              "config": {} }
        ]),
    );

    // 적용 전: 사이트는 그대로다.
    assert!(!board_slugs(&data).contains(&"cs-board".to_owned()));
    assert_eq!(maru(&data, &["site", "show"]), site_before);
    assert_eq!(
        maru(&data, &["changeset", "list", "--status", "preview"])["items"]
            .as_array()
            .unwrap()
            .len(),
        1
    );

    let applied = maru(&data, &["changeset", "apply", &id]);
    assert_eq!(applied["status"], "applied", "{applied}");
    assert!(!applied["inverse"].as_array().unwrap().is_empty());

    // 사본이 남지 않는다.
    let left: Vec<_> = std::fs::read_dir(data.join("previews")).unwrap().collect();
    assert!(left.is_empty(), "{left:?}");

    // 적용 후: 실제 화면과 DB 에 남았다.
    assert!(board_slugs(&data).contains(&"cs-board".to_owned()));
    assert_eq!(maru(&data, &["site", "show"])["name"], "묶음 사이트");
    assert_eq!(
        maru(&data, &["design", "show"])["color"]["primary"],
        "#1f4f8f"
    );
    assert_eq!(
        maru(&data, &["page", "show", "cs-page"])["status"],
        "published"
    );
    assert_eq!(
        maru(&data, &["menu", "show", "header"]),
        json!([{ "label": "묶음", "link": { "kind": "page", "slug": "cs-page" }, "children": [] }])
    );
    {
        let (_server, bind) = start_server(&data);
        let (status, home) = get(&bind, "/");
        assert_eq!(status, 200);
        assert!(home.contains("묶음 사이트"));
        let css = get(&bind, "/assets/tokens.css").1;
        assert!(
            css.contains("#1f4f8f"),
            "토큰이 화면 CSS 에 들어간다: {css}"
        );
        assert_eq!(get(&bind, "/p/cs-page").0, 200, "발행한 쪽이 열린다");
    }

    // 감사 기록에 어느 묶음인지 남는다.
    let audit = maru(&data, &["audit", "list", "--action", "board.create"]);
    let entry = audit["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["diff"].to_string().contains("cs-board"))
        .expect("게시판 기록");
    assert_eq!(entry["changeset_id"], id.as_str(), "{entry}");

    // 다시 적용해도 그대로다(멱등).
    assert_eq!(
        maru(&data, &["changeset", "apply", &id])["status"],
        "applied"
    );

    let reverted = maru(&data, &["changeset", "revert", &id]);
    assert_eq!(reverted["status"], "reverted", "{reverted}");
    assert_eq!(reverted["inverse"], json!([]));
    assert_eq!(board_slugs(&data), boards_before);
    assert_eq!(maru(&data, &["site", "show"]), site_before);
    assert_eq!(maru(&data, &["design", "show"]), design_before);
    assert_eq!(maru(&data, &["menu", "show", "header"]), menu_before);
    let page = run(&data, &["page", "show", "cs-page"]);
    assert!(
        page["ok"] == false || page["data"]["status"] == "deleted",
        "{page}"
    );
    let widgets = maru(&data, &["widget", "list"]);
    assert!(!widgets.to_string().contains("묶음 위젯"), "{widgets}");

    // 다시 되돌려도 그대로다.
    assert_eq!(
        maru(&data, &["changeset", "revert", &id])["status"],
        "reverted"
    );
}

#[test]
fn 고치기_지우기를_되돌리면_처음_값이다() {
    let tmp = tempfile::tempdir().unwrap();
    let data = init(tmp.path());
    maru(
        &data,
        &[
            "board",
            "create",
            "old",
            "--name",
            "옛 게시판",
            "--kind",
            "forum",
        ],
    );
    let board_before = maru(&data, &["board", "show", "old"]);
    let site_before = maru(&data, &["site", "show"]);

    let id = new_changeset(
        &data,
        &json!([
            { "op": "board.update", "slug": "old", "patch": { "name": "새 이름", "sort_order": 9 } },
            { "op": "board.delete", "slug": "old" },
            { "op": "tokens.reset" },
            { "op": "settings.set", "values": { "banned_words": "새말" } }
        ]),
    );
    assert_eq!(
        maru(&data, &["changeset", "apply", &id])["status"],
        "applied"
    );
    assert!(!board_slugs(&data).contains(&"old".to_owned()));

    assert_eq!(
        maru(&data, &["changeset", "revert", &id])["status"],
        "reverted"
    );
    let after = maru(&data, &["board", "show", "old"]);
    for key in [
        "slug",
        "name",
        "kind",
        "layout",
        "sort_order",
        "policy",
        "description",
    ] {
        assert_eq!(after[key], board_before[key], "{key}");
    }
    assert_eq!(maru(&data, &["site", "show"]), site_before);
}

#[test]
fn 중간_연산이_틀리면_사이트는_그대로이고_고쳐서_다시_적용한다() {
    let tmp = tempfile::tempdir().unwrap();
    let data = init(tmp.path());
    // 담을 때 사본에서 돌려 보아, 틀린 연산은 저장하지 않고 몇 번째인지 바로 알려 준다.
    let listed = |data: &Path| {
        maru(data, &["changeset", "list"])["items"]
            .as_array()
            .unwrap()
            .len()
    };
    let count_before = listed(&data);
    let text = json!([
        { "op": "board.create", "slug": "first", "name": "첫째" },
        { "op": "board.delete", "slug": "no-such-board" }
    ])
    .to_string();
    let rejected = run(
        &data,
        &["changeset", "new", "--title", "시험 변경", "--ops", &text],
    );
    assert_eq!(rejected["ok"], false, "{rejected}");
    assert!(
        rejected["error"]["message"]
            .as_str()
            .unwrap()
            .contains("2번째"),
        "{rejected}"
    );
    assert_eq!(listed(&data), count_before, "틀린 묶음은 남지 않는다");

    // 담은 뒤에 사이트가 바뀌어 적용할 때 틀리게 된 경우.
    maru(
        &data,
        &[
            "board",
            "create",
            "gone",
            "--name",
            "곧 지울 판",
            "--kind",
            "forum",
        ],
    );
    let id = new_changeset(
        &data,
        &json!([
            { "op": "board.create", "slug": "first", "name": "첫째" },
            { "op": "board.delete", "slug": "gone" }
        ]),
    );
    maru(&data, &["board", "delete", "gone"]);
    let boards_before = board_slugs(&data);

    let v = run(&data, &["changeset", "apply", &id]);
    assert_eq!(v["ok"], false, "{v}");
    assert!(
        v["error"]["message"].as_str().unwrap().contains("2번째"),
        "{v}"
    );
    assert_eq!(board_slugs(&data), boards_before, "앞 연산도 남지 않는다");

    let cs = maru(&data, &["changeset", "show", &id]);
    assert_eq!(cs["status"], "preview");
    assert!(cs["error"].as_str().unwrap().contains("2번째"), "{cs}");
    assert_eq!(cs["inverse"], json!([]));

    // 틀린 연산은 담을 때 거절한다.
    let bad = run(
        &data,
        &["changeset", "add", &id, "--op", r#"{"op":"site.burn"}"#],
    );
    assert_eq!(bad["ok"], false, "{bad}");
    assert_eq!(
        maru(&data, &["changeset", "show", &id])["ops"]
            .as_array()
            .unwrap()
            .len(),
        2
    );

    // 없던 게시판을 만드는 연산을 앞에 더한 새 묶음은 통과한다.
    let fixed = new_changeset(
        &data,
        &json!([{ "op": "board.create", "slug": "first", "name": "첫째" }]),
    );
    maru(
        &data,
        &[
            "changeset",
            "add",
            &fixed,
            "--op",
            r#"{"op":"board.update","slug":"first","patch":{"name":"첫째 고침"}}"#,
        ],
    );
    assert_eq!(
        maru(&data, &["changeset", "apply", &fixed])["status"],
        "applied"
    );
    assert_eq!(
        maru(&data, &["board", "show", "first"])["name"],
        "첫째 고침"
    );

    // 버린 것은 적용할 수 없고, 다시 버려도 그대로다.
    assert_eq!(
        maru(&data, &["changeset", "discard", &id])["status"],
        "discarded"
    );
    assert_eq!(
        maru(&data, &["changeset", "discard", &id])["status"],
        "discarded"
    );
    let v = run(&data, &["changeset", "apply", &id]);
    assert_eq!(v["ok"], false, "{v}");
    assert!(!v["error"]["next"].as_array().unwrap().is_empty(), "{v}");
}

#[test]
fn 되돌리면_데이터를_잃는_경우는_멈추고_치운_뒤_이어서_되돌린다() {
    let tmp = tempfile::tempdir().unwrap();
    let data = init(tmp.path());
    maru(
        &data,
        &[
            "board",
            "create",
            "elsewhere",
            "--name",
            "다른 판",
            "--kind",
            "forum",
        ],
    );
    let boards_before = board_slugs(&data);
    let site_before = maru(&data, &["site", "show"]);
    let id = new_changeset(
        &data,
        &json!([
            { "op": "settings.set", "values": { "name": "바뀐 이름" } },
            { "op": "board.create", "slug": "kept", "name": "남길 판", "kind": "forum" }
        ]),
    );
    maru(&data, &["changeset", "apply", &id]);

    // 적용 뒤에 그 게시판에 글이 생겼다.
    let post = maru(
        &data,
        &[
            "post",
            "new",
            "--board",
            "kept",
            "--title",
            "남길 글",
            "--body",
            "본문",
        ],
    );
    let post_id = post["id"].as_str().unwrap().to_owned();

    let v = run(&data, &["changeset", "revert", &id]);
    assert_eq!(v["ok"], false, "{v}");
    let cs = maru(&data, &["changeset", "show", &id]);
    assert_eq!(cs["status"], "failed", "{cs}");
    assert!(cs["error"].as_str().is_some(), "{cs}");
    assert!(
        board_slugs(&data).contains(&"kept".to_owned()),
        "글이 있는 게시판을 지우지 않는다"
    );
    assert_eq!(maru(&data, &["post", "show", &post_id])["title"], "남길 글");

    // 글을 치우면 남은 것부터 이어서 되돌린다.
    maru(&data, &["post", "move", &post_id, "--board", "elsewhere"]);
    assert_eq!(
        maru(&data, &["changeset", "revert", &id])["status"],
        "reverted"
    );
    assert_eq!(board_slugs(&data), boards_before);
    assert_eq!(maru(&data, &["site", "show"]), site_before);
}

/// 쿠키와 함께 읽는다. (상태, 머리글, 본문)
fn request(bind: &str, method: &str, path: &str, cookie: &str) -> (u16, String, String) {
    let mut s = TcpStream::connect(bind).unwrap();
    let mut req = format!("{method} {path} HTTP/1.1\r\nHost: {bind}\r\nConnection: close\r\n");
    if !cookie.is_empty() {
        req.push_str(&format!("Cookie: {cookie}\r\n"));
    }
    req.push_str("Content-Length: 0\r\nSec-Fetch-Site: same-origin\r\n\r\n");
    s.write_all(req.as_bytes()).unwrap();
    let mut raw = Vec::new();
    s.read_to_end(&mut raw).unwrap();
    let text = String::from_utf8_lossy(&raw).into_owned();
    let (head, body) = text.split_once("\r\n\r\n").unwrap_or((&text, ""));
    (
        head[9..12].parse().unwrap(),
        head.to_owned(),
        body.to_owned(),
    )
}

fn set_cookie_value(head: &str, name: &str) -> String {
    head.lines()
        .find_map(|l| {
            let (k, v) = l.split_once(':')?;
            (k.eq_ignore_ascii_case("set-cookie") && v.trim().starts_with(&format!("{name}=")))
                .then(|| v.trim().split(';').next().unwrap().to_owned())
        })
        .unwrap_or_else(|| panic!("{name} 쿠키가 없다: {head}"))
}

fn path_of(url: &str) -> String {
    let rest = url.split_once("://").unwrap().1;
    format!("/{}", rest.split_once('/').unwrap().1)
}

#[test]
fn 미리보기는_사본만_보여_주고_적용하면_끝난다() {
    let tmp = tempfile::tempdir().unwrap();
    let data = init(tmp.path());
    let name_before = maru(&data, &["site", "show"])["name"].clone();
    let id = new_changeset(
        &data,
        &json!([
            { "op": "settings.set", "values": { "name": "미리 본 이름" } },
            { "op": "tokens.set", "tokens": { "color": { "primary": "#1f4f8f" } } },
            { "op": "board.create", "slug": "previewed", "name": "미리 본 판", "kind": "forum" }
        ]),
    );
    let (_server, bind) = start_server(&data);

    // 표 없이 들어오면 로그인으로 보낸다.
    let (status, head, _) = request(&bind, "GET", &format!("/__preview/{id}"), "");
    assert_eq!(status, 303, "{head}");
    assert!(head.contains("/admin/login"), "{head}");
    let (status, _, _) = request(&bind, "GET", &format!("/__preview/{id}?token=틀린값"), "");
    assert_eq!(status, 403);

    let link = maru(&data, &["changeset", "preview-url", &id]);
    let url = link["url"].as_str().unwrap();
    assert!(url.contains(&bind), "지금 도는 서버 주소: {url}");
    let (status, head, _) = request(&bind, "GET", &path_of(url), "");
    assert_eq!(status, 303, "{head}");
    let preview = set_cookie_value(&head, "maru_preview");
    // 토큰은 한 번만 쓴다.
    assert_eq!(request(&bind, "GET", &path_of(url), "").0, 403);

    // 쿠키가 있으면 변경이 보이고 띠가 붙는다.
    let (status, head, home) = request(&bind, "GET", "/", &preview);
    assert_eq!(status, 200);
    assert!(home.contains("미리 본 이름"));
    assert!(home.contains("maru-preview-band"), "띠");
    assert!(head.to_ascii_lowercase().contains("no-store"), "{head}");
    assert_eq!(request(&bind, "GET", "/b/previewed", &preview).0, 200);
    assert!(
        request(&bind, "GET", "/assets/tokens.css", &preview)
            .2
            .contains("#1f4f8f"),
        "토큰도 사본에서 온다"
    );
    assert_eq!(request(&bind, "GET", "/__preview/band.css", "").0, 200);

    // 쓰기는 받지 않는다.
    assert_eq!(request(&bind, "POST", "/login", &preview).0, 403);

    // 쿠키가 없으면 실제 사이트다. 파일에도 남지 않았다.
    let (_, _, real) = request(&bind, "GET", "/", "");
    assert!(!real.contains("미리 본 이름"));
    assert!(!real.contains("maru-preview-band"));
    assert_eq!(request(&bind, "GET", "/b/previewed", "").0, 404);
    assert_eq!(maru(&data, &["site", "show"])["name"], name_before);
    assert!(!board_slugs(&data).contains(&"previewed".to_owned()));

    // 끝내기는 쿠키를 지운다.
    let (status, head, _) = request(&bind, "GET", "/__preview/exit", &preview);
    assert_eq!(status, 303);
    assert!(head.contains("maru_preview=;"), "{head}");

    // 연산을 더하면 사본도 따라온다.
    maru(
        &data,
        &[
            "changeset",
            "add",
            &id,
            "--op",
            r#"{"op":"settings.set","values":{"name":"더 고친 이름"}}"#,
        ],
    );
    assert!(
        request(&bind, "GET", "/", &preview)
            .2
            .contains("더 고친 이름")
    );

    // 적용하면 미리보기는 끝나고 실제 사이트에 보인다.
    assert_eq!(
        maru(&data, &["changeset", "apply", &id])["status"],
        "applied"
    );
    let (status, head, home) = request(&bind, "GET", "/", &preview);
    assert_eq!(status, 200);
    assert!(head.contains("maru_preview=;"), "{head}");
    assert!(!home.contains("maru-preview-band"));
    assert!(home.contains("더 고친 이름"));
}

#[test]
fn 틀린_묶음은_미리보기_때_어디가_틀렸는지_알려_준다() {
    let tmp = tempfile::tempdir().unwrap();
    let data = init(tmp.path());
    // 담을 때는 맞았는데 그 뒤에 사이트가 바뀌어 틀리게 된 묶음.
    maru(
        &data,
        &[
            "board",
            "create",
            "gone",
            "--name",
            "곧 지울 판",
            "--kind",
            "forum",
        ],
    );
    let id = new_changeset(&data, &json!([{ "op": "board.delete", "slug": "gone" }]));
    maru(&data, &["board", "delete", "gone"]);
    let (_server, bind) = start_server(&data);
    let url = maru(&data, &["changeset", "preview-url", &id])["url"]
        .as_str()
        .unwrap()
        .to_owned();
    let (status, head, body) = request(&bind, "GET", &path_of(&url), "");
    assert_eq!(status, 404, "{body}");
    assert!(body.contains("1번째"), "{body}");
    // 맨 글자가 아니라 까닭과 돌아갈 길이 있는 화면. 꾸밈은 CSP 에 맞게 바깥 파일.
    assert!(
        head.to_lowercase().contains("content-type: text/html"),
        "{head}"
    );
    assert!(body.contains("미리보기를 열 수 없습니다"), "{body}");
    assert!(
        body.contains("/admin/changesets") && body.contains("/__preview/band.css"),
        "{body}"
    );
    assert!(!body.contains("<style"), "{body}");
}

#[test]
fn 소유자_세션으로_미리보기를_연다() {
    let tmp = tempfile::tempdir().unwrap();
    let data = init(tmp.path());
    let id = new_changeset(
        &data,
        &json!([{ "op": "settings.set", "values": { "name": "소유자가 본 이름" } }]),
    );
    let (_server, bind) = start_server(&data);
    let link = maru(&data, &["login-link"]);
    let path = path_of(link["url"].as_str().unwrap());
    let (_, head, _) = request(&bind, "GET", &path, "");
    let csrf = set_cookie_value(&head, "maru_csrf");
    let body = format!("_csrf={}", csrf.split_once('=').unwrap().1);
    let mut s = TcpStream::connect(&bind).unwrap();
    let req = format!(
        "POST {path} HTTP/1.1\r\nHost: {bind}\r\nConnection: close\r\nCookie: {csrf}\r\n\
         Sec-Fetch-Site: same-origin\r\nContent-Type: application/x-www-form-urlencoded\r\n\
         Content-Length: {}\r\n\r\n{body}",
        body.len()
    );
    s.write_all(req.as_bytes()).unwrap();
    let mut raw = Vec::new();
    s.read_to_end(&mut raw).unwrap();
    let session = set_cookie_value(&String::from_utf8_lossy(&raw), "maru_session");
    let (status, head, _) = request(&bind, "GET", &format!("/__preview/{id}"), &session);
    assert_eq!(status, 303, "{head}");
    let preview = set_cookie_value(&head, "maru_preview");
    let both = format!("{session}; {preview}");
    assert!(
        request(&bind, "GET", "/", &both)
            .2
            .contains("소유자가 본 이름")
    );
    // 관리 화면은 미리보기 중에도 실제 사이트다.
    let admin = request(&bind, "GET", "/admin", &both).2;
    assert!(!admin.contains("maru-preview-band"), "{admin}");
}
