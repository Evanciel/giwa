//! 커뮤니티 스킨·프리셋 (ADR-0081). 실제 바이너리·실제 포트·실제 DB 파일로
//! `init --preset community` 가 스킨·게시판·메뉴·첫 화면 위젯을 한 번만 놓는지, 공개 화면이 스킨 틀로
//! 그려지는지, 회원 막대 `/me/bar` 가 손님·회원·모듈 항목을 캐시 없이 주는지, 스킨을 되돌릴 수 있는지 본다.
#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::path::Path;
use std::process::{Child, Command, Stdio};

use serde_json::Value;

const BIN: &str = env!("CARGO_BIN_EXE_maru");
/// 테스트 전용 값. 실제 비밀번호가 아니다.
const PASSWORD: &str = "test-pass-1234";

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
            (n == name && !val.is_empty()).then(|| val.to_owned())
        })
    }

    fn header(&self, name: &str) -> String {
        self.head
            .lines()
            .find_map(|l| {
                let (k, v) = l.split_once(':')?;
                k.eq_ignore_ascii_case(name).then(|| v.trim().to_owned())
            })
            .unwrap_or_default()
    }

    fn json(&self) -> Value {
        serde_json::from_str(&self.body).unwrap_or_else(|e| panic!("JSON 아님: {e}\n{}", self.body))
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
    let text = String::from_utf8_lossy(&raw).into_owned();
    let (head, body) = text.split_once("\r\n\r\n").unwrap_or((&text, ""));
    Res {
        status: head[9..12].parse().unwrap(),
        head: head.to_owned(),
        body: body.to_owned(),
    }
}

const FORM: (&str, &str) = ("Content-Type", "application/x-www-form-urlencoded");

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

fn signup_and_login(bind: &str, email: &str, name: &str) -> String {
    let page = http(bind, "GET", "/signup", &[], "");
    let pre = page.cookie("maru_csrf").unwrap();
    let cookie = format!("maru_csrf={pre}");
    let res = http(
        bind,
        "POST",
        "/signup",
        &[FORM, ("Cookie", &cookie)],
        &form(&[
            ("_csrf", &pre),
            ("email", email),
            ("display_name", name),
            ("password", PASSWORD),
            ("password2", PASSWORD),
        ]),
    );
    assert_eq!(res.status, 200, "{}", res.body);
    let res = http(
        bind,
        "POST",
        "/login",
        &[FORM, ("Cookie", &cookie)],
        &form(&[
            ("_csrf", &pre),
            ("next", "/"),
            ("email", email),
            ("password", PASSWORD),
        ]),
    );
    let s = res
        .cookie("maru_session")
        .unwrap_or_else(|| panic!("로그인 실패 {}: {}", res.status, res.body));
    format!("maru_session={s}")
}

#[test]
fn community_preset_skin_and_account_bar() {
    let tmp = tempfile::tempdir().unwrap();
    let data = tmp.path().join("커뮤니티");
    maru(&data, &["init", "--preset", "community", "--yes"]);

    // 프리셋: 스킨·도배 방지·게시판·메뉴·위젯
    let site = maru(&data, &["site", "show"]);
    assert_eq!(site["skin"], "community", "{site}");
    assert_eq!(site["flood_seconds"], 15);
    let boards = maru(&data, &["board", "list"]);
    let slugs: Vec<&str> = boards
        .as_array()
        .unwrap()
        .iter()
        .map(|b| b["slug"].as_str().unwrap())
        .collect();
    for s in ["notice", "free", "qna", "photo", "forum"] {
        assert!(slugs.contains(&s), "{s} 없음: {slugs:?}");
    }
    let widgets = maru(&data, &["widget", "list"]);
    let count = widgets.as_array().unwrap().len();
    assert_eq!(count, 6, "{widgets}");

    // 다시 적용해도 늘지 않는다. 사용자가 바꾼 설정은 덮어쓰지 않는다.
    maru(&data, &["site", "set", "--flood-seconds", "30"]);
    maru(&data, &["init", "--preset", "community", "--yes"]);
    assert_eq!(
        maru(&data, &["widget", "list"]).as_array().unwrap().len(),
        count
    );
    assert_eq!(maru(&data, &["site", "show"])["flood_seconds"], 30);

    // 모르는 스킨은 가능한 값을 알려 준다.
    let bad = run(&data, &["site", "set", "--skin", "nope"]);
    assert_eq!(bad["ok"], false);
    assert!(
        bad["error"]["message"]
            .as_str()
            .unwrap()
            .contains("community"),
        "{bad}"
    );

    maru(&data, &["module", "enable", "notify"]);
    maru(&data, &["module", "enable", "message"]);
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
    let (_server, bind) = start_server(&data);

    // 공개 화면은 스킨 틀. 첫 화면에 게시판별 최신 글 판, 머리글에 게시판 메뉴.
    let home = http(&bind, "GET", "/", &[], "");
    assert_eq!(home.status, 200);
    assert!(home.body.contains("/assets/community.css"), "{}", home.body);
    assert!(home.body.contains("data-bar=\"/me/bar\""));
    assert!(home.body.contains("class=\"cm-nav\""));
    assert!(home.body.contains("widget-latest_posts"));
    assert!(home.body.contains("자유게시판"));
    let board = http(&bind, "GET", "/b/free", &[], "");
    assert_eq!(board.status, 200);
    assert!(board.body.contains("community.css"));
    assert!(board.body.contains("post-table") || board.body.contains("아직 글이 없습니다"));
    let css = http(&bind, "GET", "/assets/community.css", &[], "");
    assert_eq!(css.status, 200);
    let js = http(&bind, "GET", "/assets/community.js", &[], "");
    assert_eq!(js.status, 200);
    assert!(js.header("content-type").contains("javascript"));
    let tokens = http(&bind, "GET", "/assets/tokens.css", &[], "");
    assert!(
        tokens.body.contains(":root[data-mode=\"dark\"]"),
        "{}",
        tokens.body
    );

    // 손님 막대
    let bar = http(&bind, "GET", "/me/bar", &[], "");
    assert_eq!(bar.status, 200);
    assert_eq!(bar.header("cache-control"), "no-store");
    let v = bar.json();
    assert_eq!(v["signed_in"], false);
    assert_eq!(v["login_url"], "/login");
    assert_eq!(v["signup_url"], "/signup");

    // 회원 막대: 이름과 켜진 모듈의 알림·쪽지
    let moon = signup_and_login(&bind, "moon@example.test", "달빛");
    let v = http(&bind, "GET", "/me/bar", &[("Cookie", &moon)], "").json();
    assert_eq!(v["signed_in"], true, "{v}");
    assert_eq!(v["name"], "달빛");
    let labels: Vec<&str> = v["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["label"].as_str().unwrap())
        .collect();
    assert!(labels.contains(&"알림") && labels.contains(&"쪽지"), "{v}");
    assert!(
        v["items"]
            .as_array()
            .unwrap()
            .iter()
            .all(|i| i["count"] == 0)
    );

    // 쪽지를 받으면 수가 오른다.
    let sun = signup_and_login(&bind, "sun@example.test", "햇살");
    let res = http(
        &bind,
        "POST",
        "/m/message/send",
        &[FORM, ("Cookie", &sun), ("Sec-Fetch-Site", "same-origin")],
        &form(&[("to", "달빛"), ("body", "안녕하세요")]),
    );
    assert_eq!(res.status, 303, "{}", res.body);
    let v = http(&bind, "GET", "/me/bar", &[("Cookie", &moon)], "").json();
    let msg = v["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|i| i["label"] == "쪽지")
        .unwrap();
    assert_eq!(msg["count"], 1, "{v}");

    // 스킨을 되돌리면 기본 틀
    maru(&data, &["site", "set", "--skin", "default"]);
    let home = http(&bind, "GET", "/", &[], "");
    assert!(!home.body.contains("community.css"));
}
