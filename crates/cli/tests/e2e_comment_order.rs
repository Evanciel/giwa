//! GAP Q8: 댓글 순서(등록순·최신순)와 「글쓴이」 표시 (ADR-0093). 실제 바이너리·실제 포트·실제 DB 파일로
//! 회원 둘이 댓글·대댓글을 달고 순서와 표시가 화면·REST 에 같게 나오는지 본다.
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

/// 댓글을 달고 그 댓글 ID 를 돌려준다. `reply_to` 가 있으면 대댓글.
fn comment(bind: &str, cookie: &str, post_url: &str, body: &str, reply_to: &str) -> String {
    let res = http(
        bind,
        "POST",
        &format!("{post_url}/comments"),
        &[FORM, ("Cookie", cookie), ("Sec-Fetch-Site", "same-origin")],
        &if reply_to.is_empty() {
            form(&[("body", body)])
        } else {
            form(&[("body", body), ("reply_to", reply_to)])
        },
    );
    assert_eq!(res.status, 303, "{}", res.body);
    res.location()
        .rsplit("#comment-")
        .next()
        .unwrap()
        .to_owned()
}

fn at(page: &str, needle: &str) -> usize {
    page.find(needle)
        .unwrap_or_else(|| panic!("{needle} 없음: {page}"))
}

#[test]
fn comment_order_and_post_author_mark() {
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
    let reader = format!(
        "maru_session={}",
        signup_and_login(&bind, "sun@example.test", "햇살이")
    );
    let url = write_post(&bind, &author, "댓글 순서 시험");

    // 댓글이 하나면 순서 단추가 없다.
    let first = comment(&bind, &reader, &url, "가장 먼저 단 댓글", "");
    let page = http(&bind, "GET", &url, &[], "").body;
    assert!(!page.contains("aria-label=\"댓글 순서\""), "{page}");

    comment(&bind, &author, &url, "글쓴이가 단 댓글", "");
    comment(&bind, &reader, &url, "마지막에 단 댓글", "");
    comment(&bind, &author, &url, "첫 댓글에 단 대댓글", &first);

    // 기본: 단 순서, 대댓글은 뿌리 아래
    let page = http(&bind, "GET", &url, &[], "").body;
    let (a, r, b, c) = (
        at(&page, "가장 먼저 단 댓글"),
        at(&page, "첫 댓글에 단 대댓글"),
        at(&page, "글쓴이가 단 댓글"),
        at(&page, "마지막에 단 댓글"),
    );
    assert!(a < r && r < b && b < c, "단 순서: {page}");
    assert!(page.contains("aria-label=\"댓글 순서\""), "{page}");
    assert!(page.contains("aria-current=\"true\">등록순</a>"), "{page}");
    assert!(page.contains("?corder=newest#comments\""), "{page}");
    // 「글쓴이」 표시: 글쓴이 댓글 둘에만
    assert_eq!(
        page.matches("<span class=\"op-badge\">글쓴이</span>")
            .count(),
        2,
        "{page}"
    );
    let op = at(&page, "op-badge");
    assert!(op > a && op < b, "첫 「글쓴이」 표시는 대댓글 머리: {page}");

    // 최신순: 뿌리만 뒤집고 대댓글은 그 뿌리 아래
    let page = http(&bind, "GET", &format!("{url}?corder=newest"), &[], "").body;
    let (a, r, b, c) = (
        at(&page, "가장 먼저 단 댓글"),
        at(&page, "첫 댓글에 단 대댓글"),
        at(&page, "글쓴이가 단 댓글"),
        at(&page, "마지막에 단 댓글"),
    );
    assert!(c < b && b < a && a < r, "최신순: {page}");
    assert!(page.contains("aria-current=\"true\">최신순</a>"), "{page}");
    // 모르는 순서는 단 순서
    let page = http(&bind, "GET", &format!("{url}?corder=random"), &[], "").body;
    assert!(at(&page, "가장 먼저 단 댓글") < at(&page, "마지막에 단 댓글"));

    // REST 도 같은 서비스
    let rest = http(&bind, "GET", "/api/v1/boards/free/posts", &[], "");
    let v: Value = serde_json::from_str(&rest.body).unwrap();
    let post_id = v["data"]["posts"]["items"][0]["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let rest = http(
        &bind,
        "GET",
        &format!("/api/v1/posts/{post_id}/comments?order=newest"),
        &[],
        "",
    );
    let v: Value = serde_json::from_str(&rest.body).unwrap();
    let list = v["data"].as_array().unwrap();
    assert_eq!(list[0]["body"], "마지막에 단 댓글", "{v}");
    assert_eq!(list[0]["by_post_author"], false);
    assert_eq!(list[1]["body"], "글쓴이가 단 댓글");
    assert_eq!(list[1]["by_post_author"], true);
    assert_eq!(list[3]["body"], "첫 댓글에 단 대댓글");
}
