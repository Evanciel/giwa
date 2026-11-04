//! GAP S14: 감사 로그 보기 (ADR-0062). 실제 바이너리·실제 포트·실제 DB 파일로 CLI 조건 조회,
//! REST 권한(기본 토큰 403), 관리 화면(이름·조건·쪽)을 확인한다.
#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::path::Path;
use std::process::{Child, Command, Stdio};

use serde_json::Value;

const BIN: &str = env!("CARGO_BIN_EXE_maru");

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

/// 로그인 링크를 열고 확인 버튼까지 눌러 세션 쿠키를 받는다.
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
        &[
            ("Content-Type", "application/x-www-form-urlencoded"),
            ("Cookie", &cookie),
        ],
        &format!("_csrf={csrf}"),
    );
    assert_eq!(res.status, 303, "{}", res.body);
    res.cookie("maru_session").unwrap()
}

#[test]
fn audit_log_is_listed_by_cli_rest_and_admin_screen() {
    let tmp = tempfile::tempdir().unwrap();
    let data = tmp.path().join("사이트");
    let init = maru(&data, &["init", "--preset", "blog", "--yes"]);
    let reader = format!("Bearer {}", init["token"]["secret"].as_str().unwrap());
    let link = maru(&data, &["login-link"]);
    let post = maru(
        &data,
        &[
            "post",
            "new",
            "--title",
            "감사 글",
            "--slug",
            "audit",
            "--publish",
        ],
    );
    let id = post["id"].as_str().unwrap().to_owned();

    let list = maru(&data, &["audit", "list", "--action", "post"]);
    let items = list["items"].as_array().unwrap();
    assert!(!items.is_empty(), "{list}");
    assert!(
        items
            .iter()
            .all(|e| e["action"].as_str().unwrap().starts_with("post."))
    );
    assert_eq!(items[0]["action"], "post.publish", "최신 순");
    assert_eq!(items[0]["actor_type"], "local");
    let target = maru(
        &data,
        &[
            "audit",
            "list",
            "--target-type",
            "post",
            "--target-id",
            &id,
            "--per-page",
            "1",
        ],
    );
    assert_eq!(target["items"].as_array().unwrap().len(), 1);
    assert!(target["total"].as_i64().unwrap() >= 2);
    let out = Command::new(BIN)
        .arg("--data")
        .arg(&data)
        .args(["audit", "list", "--actor-type", "robot", "--json"])
        .output()
        .unwrap();
    let bad: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(bad["ok"], false);
    assert_eq!(bad["error"]["code"], "VALIDATION", "{bad}");

    let (_server, bind) = start_server(&data);
    let get = |path: &str, headers: &[(&str, &str)]| http(&bind, "GET", path, headers, "");
    assert_eq!(get("/api/v1/audit", &[]).status, 401);
    assert_eq!(
        get("/api/v1/audit", &[("Authorization", &reader)]).status,
        403,
        "에이전트 토큰은 감사 로그를 못 본다"
    );

    // 로그인도 기록된다. 사람의 기록에는 회원 이름이 붙는다.
    let session = sign_in(&bind, link["url"].as_str().unwrap());
    let cookie = format!("maru_session={session}");
    let page = get(
        "/admin/audit?action=post&target_id=",
        &[("Cookie", &cookie)],
    );
    assert_eq!(page.status, 200, "{}", page.body);
    assert!(page.body.contains("<code>post.publish</code>"));
    assert!(!page.body.contains("<code>board.create</code>"), "조건 밖");
    assert!(page.body.contains("로컬 CLI"));
    assert!(page.body.contains("조건 지우기"));
    let all = get("/admin/audit", &[("Cookie", &cookie)]);
    assert!(
        all.body.contains("<code>board.create</code>"),
        "{}",
        all.body
    );
    assert!(all.body.contains("<details>"), "바뀐 내용 펼치기");
    let humans = maru(&data, &["audit", "list", "--actor-type", "human"]);
    let login = &humans["items"][0];
    assert_eq!(
        login["action"].as_str().unwrap().split('.').next(),
        Some("auth"),
        "{humans}"
    );
    assert!(
        !login["actor_name"].as_str().unwrap().is_empty(),
        "{humans}"
    );
    assert!(all.body.contains("사람 "), "행위자 표시");
    let bad = get("/admin/audit?actor_type=robot", &[("Cookie", &cookie)]);
    assert_eq!(bad.status, 400, "{}", bad.body);
}
