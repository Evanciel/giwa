//! Phase 3: 토큰은 changeset 으로만 (SPEC §5.3, ADR-0123). REST·CLI·관리 화면이 같은 규칙을 따르는지 본다.
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

fn run_as(data: &Path, token: Option<&str>, args: &[&str]) -> Value {
    let mut cmd = Command::new(BIN);
    cmd.arg("--data").arg(data).args(args).arg("--json");
    match token {
        Some(t) => cmd.env("MARU_TOKEN", t),
        None => cmd.env_remove("MARU_TOKEN"),
    };
    let out = cmd.output().expect("maru 실행");
    let stdout = String::from_utf8(out.stdout).expect("UTF-8 출력");
    serde_json::from_str(stdout.trim()).unwrap_or_else(|e| panic!("JSON 아님: {e}\n{stdout}"))
}

fn maru(data: &Path, args: &[&str]) -> Value {
    let v = run_as(data, None, args);
    assert_eq!(v["ok"], true, "{args:?} → {v}");
    v["data"].clone()
}

fn init(tmp: &Path) -> PathBuf {
    let data = tmp.join("데이터");
    maru(&data, &["init", "--preset", "blog", "--yes"]);
    data
}

fn token(data: &Path, extra: &[&str]) -> String {
    let mut args = vec!["token", "create", "--name", "시험"];
    args.extend(extra);
    maru(data, &args)["secret"].as_str().unwrap().to_owned()
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

/// (상태, 머리글, 본문)
fn http(
    bind: &str,
    method: &str,
    path: &str,
    headers: &[(&str, &str)],
    body: &str,
) -> (u16, String, String) {
    let mut s = TcpStream::connect(bind).unwrap();
    let mut req = format!("{method} {path} HTTP/1.1\r\nHost: {bind}\r\nConnection: close\r\n");
    for (k, v) in headers {
        req.push_str(&format!("{k}: {v}\r\n"));
    }
    req.push_str(&format!("Content-Length: {}\r\n\r\n{body}", body.len()));
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

fn api(bind: &str, token: &str, method: &str, path: &str, body: &Value) -> (u16, Value) {
    let auth = format!("Bearer {token}");
    let (status, _, text) = http(
        bind,
        method,
        &format!("/api/v1{path}"),
        &[
            ("Authorization", &auth),
            ("Content-Type", "application/json"),
        ],
        &body.to_string(),
    );
    (status, serde_json::from_str(&text).unwrap_or(Value::Null))
}

fn set_cookie(head: &str, name: &str) -> String {
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

fn sign_in(data: &Path, bind: &str) -> String {
    let link = maru(data, &["login-link"]);
    let path = path_of(link["url"].as_str().unwrap());
    let (_, head, _) = http(bind, "GET", &path, &[], "");
    let csrf = set_cookie(&head, "maru_csrf");
    let body = format!("_csrf={}", csrf.split_once('=').unwrap().1);
    let (status, head, text) = http(bind, "POST", &path, &[FORM, ("Cookie", &csrf)], &body);
    assert_eq!(status, 303, "{text}");
    set_cookie(&head, "maru_session")
}

const FORM: (&str, &str) = ("Content-Type", "application/x-www-form-urlencoded");

fn csrf_of(html: &str) -> String {
    let at = html.find("name=\"_csrf\" value=\"").unwrap() + 20;
    html[at..at + html[at..].find('"').unwrap()].to_owned()
}

fn has_board(data: &Path, slug: &str) -> bool {
    maru(data, &["board", "list"])
        .as_array()
        .unwrap()
        .iter()
        .any(|b| b["slug"] == slug)
}

#[test]
fn 토큰은_바로_쓰지_못하고_묶음에_담아_승인받는다() {
    let tmp = tempfile::tempdir().unwrap();
    let data = init(tmp.path());
    let t = token(
        &data,
        &[
            "--scope",
            "read",
            "--scope",
            "structure",
            "--scope",
            "design",
            "--scope",
            "admin",
        ],
    );
    let (_server, bind) = start_server(&data);

    // REST 로 바로 쓰면 거절되고 다음 할 일을 알려 준다.
    let (status, res) = api(
        &bind,
        &t,
        "POST",
        "/boards",
        &json!({ "slug": "direct", "name": "바로" }),
    );
    assert_eq!(status, 403, "{res}");
    assert!(
        res["error"]["message"]
            .as_str()
            .unwrap()
            .contains("변경 묶음"),
        "{res}"
    );
    assert!(
        !res["error"]["next"].as_array().unwrap().is_empty(),
        "{res}"
    );
    assert!(!has_board(&data, "direct"), "트랜잭션이 되돌려졌다");
    for (method, path, body) in [
        ("PATCH", "/settings", json!({ "name": "바로 바꿈" })),
        ("PUT", "/menus/header", json!([])),
        (
            "PATCH",
            "/design/tokens",
            json!({ "color": { "primary": "#1f4f8f" } }),
        ),
    ] {
        let (status, res) = api(&bind, &t, method, path, &body);
        assert_eq!(status, 403, "{path}: {res}");
    }
    let (status, res) = api(&bind, &t, "POST", "/modules/point/enable", &json!({}));
    assert_eq!(status, 403, "{res}");

    // CLI 도 토큰이면 같다.
    let v = run_as(
        &data,
        Some(&t),
        &["board", "create", "direct", "--name", "바로"],
    );
    assert_eq!(v["ok"], false, "{v}");

    // 읽기는 그대로 된다.
    assert_eq!(api(&bind, &t, "GET", "/widgets", &json!({})).0, 200);
    assert_eq!(api(&bind, &t, "GET", "/pages", &json!({})).0, 200);

    // 묶음은 만들 수 있지만 적용은 못 한다.
    let (status, cs) = api(
        &bind,
        &t,
        "POST",
        "/changesets",
        &json!({
            "title": "게시판 하나",
            "ops": [{ "op": "board.create", "slug": "staged", "name": "담은 판", "kind": "forum" }]
        }),
    );
    assert_eq!(status, 201, "{cs}");
    let id = cs["data"]["id"].as_str().unwrap().to_owned();
    assert_eq!(cs["data"]["actor_type"], "agent");
    let (status, res) = api(
        &bind,
        &t,
        "POST",
        &format!("/changesets/{id}/apply"),
        &json!({}),
    );
    assert_eq!(status, 403, "{res}");
    assert!(!has_board(&data, "staged"));

    let (status, res) = api(
        &bind,
        &t,
        "POST",
        &format!("/changesets/{id}/ops"),
        &json!({ "ops": [{ "op": "board.update", "slug": "staged", "patch": { "name": "고친 판" } }] }),
    );
    assert_eq!(status, 200, "{res}");
    let (status, link) = api(
        &bind,
        &t,
        "POST",
        &format!("/changesets/{id}/preview-link"),
        &json!({}),
    );
    assert_eq!(status, 200, "{link}");
    assert!(link["data"]["url"].as_str().unwrap().contains("token="));

    // 가진 스코프 밖의 연산은 담지 못한다.
    let reader = token(&data, &["--scope", "read", "--scope", "structure"]);
    let (status, res) = api(
        &bind,
        &reader,
        "POST",
        "/changesets",
        &json!({ "title": "넘침", "ops": [{ "op": "tokens.reset" }] }),
    );
    assert_eq!(status, 403, "{res}");

    // 사람이 승인하면 들어가고, 되돌리면 빠진다.
    assert_eq!(
        maru(&data, &["changeset", "apply", &id])["status"],
        "applied"
    );
    assert_eq!(maru(&data, &["board", "show", "staged"])["name"], "고친 판");
    let (status, list) = api(&bind, &t, "GET", "/changesets?status=applied", &json!({}));
    assert_eq!(status, 200);
    assert_eq!(list["data"]["items"].as_array().unwrap().len(), 1, "{list}");
    assert_eq!(
        maru(&data, &["changeset", "revert", &id])["status"],
        "reverted"
    );
    assert!(!has_board(&data, "staged"));
}

#[test]
fn 자동_적용_토큰은_승인_없이_적용한다() {
    let tmp = tempfile::tempdir().unwrap();
    let data = init(tmp.path());
    let t = token(
        &data,
        &["--scope", "read", "--scope", "structure", "--auto-apply"],
    );
    assert!(
        maru(&data, &["token", "list"])
            .to_string()
            .contains("\"auto_apply\":true"),
        "목록에 보인다"
    );
    let (_server, bind) = start_server(&data);

    // 자동 적용이어도 바로 쓰지는 못한다.
    let (status, _) = api(
        &bind,
        &t,
        "POST",
        "/boards",
        &json!({ "slug": "direct", "name": "바로" }),
    );
    assert_eq!(status, 403);

    let (_, cs) = api(
        &bind,
        &t,
        "POST",
        "/changesets",
        &json!({ "title": "자동", "ops": [{ "op": "board.create", "slug": "auto", "name": "자동 판", "kind": "forum" }] }),
    );
    let id = cs["data"]["id"].as_str().unwrap().to_owned();
    let (status, res) = api(
        &bind,
        &t,
        "POST",
        &format!("/changesets/{id}/apply"),
        &json!({}),
    );
    assert_eq!(status, 200, "{res}");
    assert_eq!(res["data"]["status"], "applied");
    assert!(has_board(&data, "auto"));
    let (status, res) = api(
        &bind,
        &t,
        "POST",
        &format!("/changesets/{id}/revert"),
        &json!({}),
    );
    assert_eq!(status, 200, "{res}");
    assert!(!has_board(&data, "auto"));
}

#[test]
fn 관리_화면에서_보고_적용하고_되돌린다() {
    let tmp = tempfile::tempdir().unwrap();
    let data = init(tmp.path());
    let t = token(&data, &["--scope", "read", "--scope", "structure"]);
    let (_server, bind) = start_server(&data);
    let (_, cs) = api(
        &bind,
        &t,
        "POST",
        "/changesets",
        &json!({
            "title": "<script>제목</script> 게시판",
            "ops": [{ "op": "board.create", "slug": "from-admin", "name": "화면 판", "kind": "forum" }]
        }),
    );
    let id = cs["data"]["id"].as_str().unwrap().to_owned();
    let session = sign_in(&data, &bind);
    let cookie = [("Cookie", session.as_str())];

    // 로그인하지 않으면 볼 수 없다.
    let (status, head, _) = http(&bind, "GET", "/admin/changesets", &[], "");
    assert_eq!(status, 303, "{head}");

    let (status, _, list) = http(&bind, "GET", "/admin/changesets", &cookie, "");
    assert_eq!(status, 200);
    assert!(list.contains("승인 대기"), "{list}");
    assert!(!list.contains("<script>제목"), "제목은 글자로 나온다");
    assert!(list.contains("&lt;script&gt;제목"));

    let page = format!("/admin/changesets/{id}");
    let (status, _, html) = http(&bind, "GET", &page, &cookie, "");
    assert_eq!(status, 200);
    assert!(html.contains("게시판 만들기"), "{html}");
    assert!(html.contains("from-admin"));
    assert!(html.contains(&format!("/__preview/{id}")), "미리보기 링크");
    let csrf = format!("_csrf={}", csrf_of(&html));

    // 관리자 세션으로 미리보기에 들어가면 띠에서 이 화면으로 돌아온다.
    let (status, head, _) = http(&bind, "GET", &format!("/__preview/{id}"), &cookie, "");
    assert_eq!(status, 303, "{head}");
    let preview = set_cookie(&head, "maru_preview");
    let (_, _, home) = http(&bind, "GET", "/", &[("Cookie", &preview)], "");
    assert!(home.contains(&page), "{home}");

    let bad = http(
        &bind,
        "POST",
        &format!("{page}/apply"),
        &[FORM, ("Cookie", &session)],
        "_csrf=틀림",
    );
    assert_ne!(bad.0, 303, "CSRF 값이 틀리면 거절");
    assert!(!has_board(&data, "from-admin"));

    let post = |action: &str| {
        http(
            &bind,
            "POST",
            &format!("{page}/{action}"),
            &[FORM, ("Cookie", &session)],
            &csrf,
        )
    };
    let (status, head, body) = post("apply");
    assert_eq!(status, 303, "{body}");
    assert!(head.contains("done=apply"), "{head}");
    assert!(has_board(&data, "from-admin"));
    assert_eq!(
        maru(&data, &["changeset", "show", &id])["status"],
        "applied"
    );
    // 적용한 뒤에는 미리보기가 끝났다.
    let (_, _, home) = http(&bind, "GET", "/", &[("Cookie", &preview)], "");
    assert!(!home.contains("maru-preview-band"));
    // 감사 기록에는 사람으로 남고 묶음 번호가 붙는다.
    let audit = maru(&data, &["audit", "list", "--action", "board.create"]);
    let entry = audit["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["diff"].to_string().contains("from-admin"))
        .expect("기록");
    assert_eq!(entry["actor_type"], "human", "{entry}");
    assert_eq!(entry["changeset_id"], id.as_str(), "{entry}");

    let (status, _, html) = http(&bind, "GET", &page, &cookie, "");
    assert_eq!(status, 200);
    assert!(html.contains("되돌리기"));
    assert!(!html.contains("사이트에 적용"));

    let (status, _, body) = post("apply");
    assert_eq!(status, 303, "다시 적용해도 그대로: {body}");
    let (status, _, body) = post("revert");
    assert_eq!(status, 303, "{body}");
    assert!(!has_board(&data, "from-admin"));

    // 지금 상태로 할 수 없는 일은 까닭을 화면에 보인다.
    let (status, _, body) = post("discard");
    assert_eq!(status, 409, "{body}");
    assert!(body.contains("상태라 이 일을 할 수 없습니다"), "{body}");
}
