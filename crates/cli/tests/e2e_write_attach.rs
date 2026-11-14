//! GAP M4: 공개 글쓰기 파일 붙이기 (ADR-0065). 실제 바이너리·실제 포트·실제 DB 파일로
//! 게시판 역할 → 글쓰기 화면의 칸 → 올리기 → 글에 링크 → 내려받기까지 확인한다.
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

    fn csrf(&self) -> String {
        let at = self.body.find("name=\"_csrf\" value=\"").unwrap() + 20;
        self.body[at..at + self.body[at..].find('"').unwrap()].to_owned()
    }

    fn json(&self) -> Value {
        serde_json::from_str(&self.body).unwrap_or_else(|e| panic!("{e}: {}", self.body))
    }
}

fn http(bind: &str, method: &str, path: &str, headers: &[(&str, &str)], body: &[u8]) -> Res {
    let mut s = TcpStream::connect(bind).unwrap();
    let mut req = format!("{method} {path} HTTP/1.1\r\nHost: {bind}\r\nConnection: close\r\n");
    for (k, v) in headers {
        req.push_str(&format!("{k}: {v}\r\n"));
    }
    req.push_str(&format!("Content-Length: {}\r\n\r\n", body.len()));
    s.write_all(req.as_bytes()).unwrap();
    s.write_all(body).unwrap();
    let mut raw = Vec::new();
    s.read_to_end(&mut raw).unwrap();
    let split = raw
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .unwrap_or(raw.len());
    let head = String::from_utf8_lossy(&raw[..split]).into_owned();
    Res {
        status: head[9..12].parse().unwrap(),
        head,
        body: String::from_utf8_lossy(raw.get(split + 4..).unwrap_or_default()).into_owned(),
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

/// 가입하고 로그인해 세션 쿠키를 받는다 (메일 확인·확인 질문을 끈 사이트).
fn signup_and_login(bind: &str, email: &str) -> String {
    let page = http(bind, "GET", "/signup", &[], b"");
    let pre = page.cookie("giwa_csrf").unwrap();
    let cookie = format!("giwa_csrf={pre}");
    let res = http(
        bind,
        "POST",
        "/signup",
        &[FORM, ("Cookie", &cookie)],
        form(&[
            ("_csrf", &pre),
            ("email", email),
            ("display_name", "붙임이"),
            ("password", PASSWORD),
            ("password2", PASSWORD),
        ])
        .as_bytes(),
    );
    if let Some(s) = res.cookie("giwa_session") {
        return s;
    }
    let login = http(bind, "GET", "/login", &[("Cookie", &cookie)], b"");
    let res = http(
        bind,
        "POST",
        "/login",
        &[FORM, ("Cookie", &cookie)],
        form(&[
            ("_csrf", &login.csrf()),
            ("next", "/"),
            ("email", email),
            ("password", PASSWORD),
        ])
        .as_bytes(),
    );
    res.cookie("giwa_session")
        .unwrap_or_else(|| panic!("로그인 실패 {}: {}", res.status, res.body))
}

#[test]
fn member_attaches_a_file_to_a_post_from_the_public_write_form() {
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
    let session = signup_and_login(&bind, "attach@example.test");
    let cookie = format!("giwa_session={session}");
    let as_member = [("Cookie", cookie.as_str())];

    // 기본은 받지 않는다: 칸이 없고 올려도 거절.
    let page = http(&bind, "GET", "/write/free", &as_member, b"");
    assert_eq!(page.status, 200, "{}", page.body);
    assert!(!page.body.contains("data-attach-url"));
    let csrf = page.csrf();
    let upload = |name: &str, bytes: &[u8], token: &str, cookie: Option<&str>| {
        let mut h = vec![("X-CSRF-Token", token)];
        if let Some(c) = cookie {
            h.push(("Cookie", c));
        }
        http(
            &bind,
            "POST",
            &format!("/write/free/files?filename={}", enc(name)),
            &h,
            bytes,
        )
    };
    let pdf = b"%PDF-1.4 \xec\x9d\xb4\xec\x9a\xa9 \xec\x95\xbd\xea\xb4\x80";
    let res = upload("약관.pdf", pdf, &csrf, Some(&cookie));
    assert_eq!(res.status, 403, "{}", res.body);

    // 게시판 역할을 회원으로 열면 칸이 보이고, 손님 역할은 고를 수 없다.
    giwa(&data, &["board", "update", "free", "--upload", "member"]);
    let bad = run(&data, &["board", "update", "free", "--upload", "guest"]);
    assert_eq!(bad["ok"], false);
    assert_eq!(bad["error"]["code"], "VALIDATION", "{bad}");
    let page = http(&bind, "GET", "/write/free", &as_member, b"");
    assert!(page.body.contains("data-attach-url=\"/write/free/files\""));
    assert!(page.body.contains("/assets/write.js"));
    let js = http(&bind, "GET", "/assets/write.js", &[], b"");
    assert_eq!(js.status, 200);
    assert!(js.body.contains("X-CSRF-Token"));
    let guest_page = http(&bind, "GET", "/write/free", &[], b"");
    assert!(
        !guest_page.body.contains("data-attach-url"),
        "손님에게는 칸이 없다"
    );

    // CSRF 가 틀리면, 로그인하지 않았으면 거절. 위험한 종류도 거절.
    assert_eq!(upload("약관.pdf", pdf, "틀림", Some(&cookie)).status, 403);
    assert_eq!(upload("약관.pdf", pdf, "x", None).status, 403);
    let res = upload(
        "evil.html",
        b"<script>alert(1)</script>",
        &csrf,
        Some(&cookie),
    );
    assert_eq!(res.status, 400, "{}", res.body);
    assert_eq!(res.json()["ok"], false);

    // 올리면 본문에 넣을 마크다운을 준다.
    let res = upload("약관.pdf", pdf, &csrf, Some(&cookie));
    assert_eq!(res.status, 201, "{}", res.body);
    let att = res.json()["data"].clone();
    let url = att["url"].as_str().unwrap().to_owned();
    let md = att["markdown"].as_str().unwrap().to_owned();
    assert!(md.starts_with("[약관.pdf (1KB)](/files/"), "{md}");

    // 그 마크다운으로 글을 쓰면 링크가 보이고, 파일은 내려받기로만 열린다.
    let res = http(
        &bind,
        "POST",
        "/write/free",
        &[FORM, ("Cookie", &cookie)],
        form(&[
            ("_csrf", &csrf),
            ("title", "첨부 있는 글"),
            ("body", &format!("읽어 보세요.\n\n{md}")),
        ])
        .as_bytes(),
    );
    assert_eq!(res.status, 303, "{}", res.body);
    let location = res
        .head
        .lines()
        .find_map(|l| {
            let (k, v) = l.split_once(':')?;
            k.eq_ignore_ascii_case("location")
                .then(|| v.trim().to_owned())
        })
        .unwrap();
    let post = http(&bind, "GET", &location, &[], b"");
    assert_eq!(post.status, 200);
    assert!(
        post.body.contains(&format!("href=\"{url}\"")),
        "{}",
        post.body
    );
    let file = http(&bind, "GET", &url, &[], b"");
    assert_eq!(file.status, 200);
    assert!(
        file.head.to_lowercase().contains("attachment"),
        "{}",
        file.head
    );
    let audit = giwa(
        &data,
        &[
            "audit",
            "list",
            "--action",
            "file.upload",
            "--actor-type",
            "human",
        ],
    );
    assert_eq!(audit["total"], 1, "{audit}");
}
