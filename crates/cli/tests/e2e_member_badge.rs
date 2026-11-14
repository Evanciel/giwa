//! GAP Q1: 회원 이름 옆 레벨 표시 (ADR-0086). 실제 바이너리·실제 포트·실제 DB 파일로
//! 포인트 모듈 켜기 → 목록·글·댓글의 이름 옆 표시 → 레벨 이름 설정 → 끄기(사라짐)를 확인한다.
#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::path::Path;
use std::process::{Child, Command, Stdio};

use serde_json::Value;

const BIN: &str = env!("CARGO_BIN_EXE_giwa");
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
        .expect("giwa 실행");
    let stdout = String::from_utf8(out.stdout).expect("UTF-8 출력");
    serde_json::from_str(stdout.trim()).unwrap_or_else(|e| panic!("JSON 아님: {e}\n{stdout}"))
}

fn giwa(data: &Path, args: &[&str]) -> Value {
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
    let pre = page.cookie("giwa_csrf").unwrap();
    let cookie = format!("giwa_csrf={pre}");
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
    if let Some(s) = res.cookie("giwa_session") {
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
    res.cookie("giwa_session")
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

fn badge(level: u32, label: &str) -> String {
    format!("<span class=\"level-badge\" title=\"레벨 {level}\">{label}</span>")
}

#[test]
fn level_badges_next_to_member_names() {
    let tmp = tempfile::tempdir().unwrap();
    let data = tmp.path().join("사이트");
    giwa(
        &data,
        &["init", "--no-samples", "--preset", "blog", "--yes"],
    );
    giwa(
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
    giwa(
        &data,
        &[
            "board", "create", "free", "--name", "자유", "--kind", "free",
        ],
    );
    giwa(
        &data,
        &["board", "update", "free", "--write-approval", "off"],
    );
    let (_server, bind) = start_server(&data);

    let author = format!(
        "giwa_session={}",
        signup_and_login(&bind, "moon@example.test", "달빛이")
    );
    let reader = format!(
        "giwa_session={}",
        signup_and_login(&bind, "sun@example.test", "햇살이")
    );
    let url = write_post(&bind, &author, "레벨 보일 글");
    let res = http(
        &bind,
        "POST",
        &format!("{url}/comments"),
        &[FORM, ("Cookie", &reader), ("Sec-Fetch-Site", "same-origin")],
        &form(&[("body", "반갑습니다")]),
    );
    assert_eq!(res.status, 303, "{}", res.body);

    // 끈 동안에는 표시가 없다.
    let list = http(&bind, "GET", "/b/free", &[], "").body;
    assert!(list.contains("달빛이"), "{list}");
    assert!(!list.contains("level-badge"), "{list}");
    assert!(
        !http(&bind, "GET", &url, &[], "")
            .body
            .contains("level-badge")
    );

    giwa(&data, &["module", "enable", "point"]);
    giwa(
        &data,
        &[
            "point",
            "adjust",
            "--member",
            "moon@example.test",
            "--amount",
            "600",
            "--reason",
            "시험",
        ],
    );

    // 켜면 재시작 없이 목록·글·댓글에 붙는다. 기준 100,500 을 넘었으니 3, 0 점이면 1.
    let list = http(&bind, "GET", "/b/free", &[], "").body;
    assert!(
        list.contains(&format!("달빛이</span> {}", badge(3, "Lv.3"))),
        "{list}"
    );
    let page = http(&bind, "GET", &url, &[], "").body;
    assert!(page.contains(&badge(3, "Lv.3")), "{page}");
    assert!(
        page.contains(&format!("햇살이</strong> {}", badge(1, "Lv.1"))),
        "{page}"
    );
    // 보는 사람과 무관하다: 로그인한 회원에게도 같다.
    let as_reader = http(&bind, "GET", &url, &[("Cookie", &reader)], "").body;
    assert!(as_reader.contains(&badge(3, "Lv.3")));

    // 레벨 이름은 기준보다 하나 많아야 한다.
    let bad = run(
        &data,
        &["module", "set", "point", "--field", "level_names=새싹,잎새"],
    );
    assert_eq!(bad["error"]["code"], "VALIDATION", "{bad}");
    let long = run(
        &data,
        &[
            "module",
            "set",
            "point",
            "--field",
            "level_names=새싹,잎새,아주아주긴나무이름,숲,산",
        ],
    );
    assert_eq!(long["error"]["code"], "VALIDATION", "{long}");
    giwa(
        &data,
        &[
            "module",
            "set",
            "point",
            "--field",
            "level_names=새싹,잎새,나무,숲,산",
        ],
    );
    let page = http(&bind, "GET", &url, &[], "").body;
    assert!(page.contains(&badge(3, "나무")), "{page}");
    assert!(page.contains(&badge(1, "새싹")), "{page}");

    // 끄면 사라진다.
    giwa(&data, &["module", "disable", "point"]);
    let list = http(&bind, "GET", "/b/free", &[], "").body;
    assert!(!list.contains("level-badge"), "{list}");
    assert!(
        !http(&bind, "GET", &url, &[], "")
            .body
            .contains("level-badge")
    );

    // 운영진 표시 (GAP Q25, ADR-0107): 모듈 없이도 붙고, 등급 표시보다 앞선다.
    let staff = "<span class=\"level-badge staff\" title=\"운영진\">운영진</span>";
    giwa(&data, &["member", "role", "sun@example.test", "editor"]);
    let page = http(&bind, "GET", &url, &[], "").body;
    assert!(page.contains(&format!("햇살이</strong> {staff}")), "{page}");
    giwa(&data, &["module", "enable", "point"]);
    let page = http(&bind, "GET", &url, &[], "").body;
    assert!(page.contains(&format!("햇살이</strong> {staff}")), "{page}");
    assert!(page.contains(&badge(3, "나무")), "{page}");
    giwa(&data, &["member", "role", "sun@example.test", "member"]);
    let page = http(&bind, "GET", &url, &[], "").body;
    assert!(!page.contains("level-badge staff"), "{page}");
}
