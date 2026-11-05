//! GAP Q11: 상대 시간·수 줄여 쓰기와 최근 본 게시판 자리 (ADR-0096). 실제 바이너리·실제 포트·
//! 실제 DB 파일로 목록·글·댓글·모아보기의 표시를 확인한다(최근 본 게시판 목록은 브라우저 저장이라 자리만 본다).
#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::path::Path;
use std::process::{Child, Command, Stdio};

use serde_json::Value;

const BIN: &str = env!("CARGO_BIN_EXE_maru");
const FORM: (&str, &str) = ("Content-Type", "application/x-www-form-urlencoded");
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
            (n == name).then(|| val.to_owned())
        })
    }

    fn location(&self) -> String {
        self.head
            .lines()
            .find_map(|l| {
                let (k, v) = l.split_once(':')?;
                k.eq_ignore_ascii_case("location")
                    .then(|| v.trim().to_owned())
            })
            .unwrap_or_default()
    }

    fn csrf(&self) -> String {
        let at = self.body.find("name=\"_csrf\" value=\"").unwrap() + 20;
        self.body[at..at + self.body[at..].find('"').unwrap()].to_owned()
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
    if let Some(s) = res.cookie("maru_session") {
        return s;
    }
    let login = http(bind, "GET", "/login", &[("Cookie", &cookie)], "");
    let res = http(
        bind,
        "POST",
        "/login",
        &[FORM, ("Cookie", &cookie)],
        &form(&[
            ("_csrf", &login.csrf()),
            ("next", "/"),
            ("email", email),
            ("password", PASSWORD),
        ]),
    );
    res.cookie("maru_session")
        .unwrap_or_else(|| panic!("로그인 실패 {}: {}", res.status, res.body))
}

/// 회원이 공개 글쓰기로 글을 쓰고 글 주소를 돌려준다.
fn write_post(bind: &str, cookie: &str, title: &str) -> String {
    let page = http(bind, "GET", "/write/free", &[("Cookie", cookie)], "");
    assert_eq!(page.status, 200, "{}", page.body);
    let res = http(
        bind,
        "POST",
        "/write/free",
        &[FORM, ("Cookie", cookie)],
        &form(&[
            ("_csrf", &page.csrf()),
            ("title", title),
            ("body", "본문입니다."),
        ]),
    );
    assert_eq!(res.status, 303, "{}", res.body);
    res.location()
}

#[test]
fn relative_time_short_counts_and_recent_boards_slot() {
    let tmp = tempfile::tempdir().unwrap();
    let data = tmp.path().join("사이트");
    maru(&data, &["init", "--preset", "blog", "--yes"]);
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
    maru(
        &data,
        &[
            "board", "create", "free", "--name", "자유", "--kind", "free",
        ],
    );
    maru(
        &data,
        &["board", "update", "free", "--write-approval", "off"],
    );
    let (_server, bind) = start_server(&data);

    let author = format!(
        "maru_session={}",
        signup_and_login(&bind, "moon@example.test", "달빛이")
    );
    let url = write_post(&bind, &author, "많이 본 글");
    write_post(&bind, &author, "조금 본 글");
    let page = http(&bind, "GET", &url, &[], "");
    let res = http(
        &bind,
        "POST",
        &format!("{url}/comments"),
        &[FORM, ("Cookie", &author), ("Sec-Fetch-Site", "same-origin")],
        &form(&[("body", "댓글 시각 확인")]),
    );
    assert_eq!(res.status, 303, "{} {}", res.body, page.status);
    let rest = http(&bind, "GET", "/api/v1/boards/free/posts", &[], "");
    let v: Value = serde_json::from_str(&rest.body).unwrap();
    let id_of = |t: &str| {
        v["data"]["posts"]["items"]
            .as_array()
            .unwrap()
            .iter()
            .find(|p| p["title"] == t)
            .unwrap()["id"]
            .as_str()
            .unwrap()
            .to_owned()
    };
    maru(
        &data,
        &["post", "views", &id_of("많이 본 글"), "--set", "12345"],
    );
    maru(
        &data,
        &["post", "views", &id_of("조금 본 글"), "--set", "1234"],
    );

    // 게시판 목록: 방금 쓴 글은 「방금 전」(전체 날짜는 title), 조회는 줄여서
    let board = http(&bind, "GET", "/b/free", &[], "").body;
    assert!(board.contains("\">방금 전</time>"), "{board}");
    assert!(
        board.contains(" title=\"20"),
        "전체 날짜는 title 에: {board}"
    );
    assert!(
        board.contains("<span class=\"hits-label\">조회 </span>1.2만</span>"),
        "{board}"
    );
    assert!(
        board.contains("<span class=\"hits-label\">조회 </span>1,234</span>"),
        "{board}"
    );
    // 최근 본 게시판: 게시판 화면이 자신을 알리고, 빈 자리는 숨겨 둔다(모두에게 같은 HTML)
    assert!(
        board.contains("data-visit=\"/b/free\" data-visit-name=\"자유\""),
        "{board}"
    );
    assert!(board.contains("data-recent-boards hidden"), "{board}");
    let home = http(&bind, "GET", "/", &[], "").body;
    assert!(home.contains("data-recent-boards hidden"), "{home}");
    assert!(
        !home.contains("data-visit="),
        "게시판이 아닌 화면은 알리지 않는다"
    );

    // 글 화면: 조회 줄여 쓰기, 댓글 시각은 상대 시간
    let post = http(&bind, "GET", &url, &[], "").body;
    assert!(
        post.contains("<span class=\"views\">조회 1.2만</span>"),
        "{post}"
    );
    assert!(post.contains("\">방금 전</time>"), "{post}");

    // 모아보기도 같은 표시
    let recent = http(&bind, "GET", "/recent", &[], "").body;
    assert!(recent.contains("\">방금 전</time>"), "{recent}");
    assert!(recent.contains("1.2만"), "{recent}");

    // 시간대를 바꿔도 상대 시간은 그대로(하루가 넘은 날짜에만 쓴다)
    maru(&data, &["site", "set", "--tz-offset", "0"]);
    let board = http(&bind, "GET", "/b/free", &[], "").body;
    assert!(board.contains("\">방금 전</time>"), "{board}");
}
