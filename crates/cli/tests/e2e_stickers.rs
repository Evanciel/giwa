//! GAP E15: 스티커 (ADR-0115). 실제 바이너리·실제 포트·실제 DB 파일로 설정을 켜고 끄며 댓글·글의 그림과
//! 고르기 창 표시, 내장 파일, CLI 목록을 본다.
#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::path::Path;
use std::process::{Child, Command, Stdio};

use serde_json::Value;

const BIN: &str = env!("CARGO_BIN_EXE_maru");
const FORM: (&str, &str) = ("Content-Type", "application/x-www-form-urlencoded");
const SAME: (&str, &str) = ("Sec-Fetch-Site", "same-origin");
const PASSWORD: &str = "test-pass-1234";

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
        return format!("maru_session={s}");
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
    let session = res
        .cookie("maru_session")
        .unwrap_or_else(|| panic!("로그인 실패 {}: {}", res.status, res.body));
    format!("maru_session={session}")
}

#[test]
fn stickers_in_comments_and_posts() {
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

    // CLI 목록과 내장 파일이 맞다.
    let list = maru(&data, &["stickers"]);
    let items = list["items"].as_array().unwrap();
    assert_eq!(items.len(), 63);
    assert_eq!(items[0]["id"], "cat-1");
    let src = items[0]["src"].as_str().unwrap().to_owned();
    assert_eq!(src, "/assets/stickers/cat/1.png");
    let label = items[0]["label"].as_str().unwrap().to_owned();

    let (server, bind) = start_server(&data);
    let png = http(&bind, "GET", &src, &[], "");
    assert_eq!(png.status, 200);
    assert!(
        png.head
            .to_ascii_lowercase()
            .contains("content-type: image/png"),
        "{}",
        png.head
    );
    let index = http(&bind, "GET", "/assets/stickers/index.json", &[], "");
    assert_eq!(index.status, 200);
    let index: Value = serde_json::from_str(&index.body).unwrap();
    assert_eq!(index["sets"][0]["items"][0]["src"], src.as_str());
    assert_eq!(
        http(&bind, "GET", "/assets/stickers.js", &[], "").status,
        200
    );

    // 글과 댓글을 쓴다. 설정이 꺼져 있으면(기본값) 고르기 창도 그림도 없다.
    let member = signup_and_login(&bind, "moon@example.test", "달님");
    let page = http(&bind, "GET", "/write/free", &[("Cookie", &member)], "");
    assert_eq!(page.status, 200, "{}", page.body);
    assert!(!page.body.contains("data-stickers"));
    let body = format!("좋아요 ![{label}]({src}) 정말로\n\n![바깥](https://example.test/a.png)");
    let res = http(
        &bind,
        "POST",
        "/write/free",
        &[FORM, ("Cookie", &member)],
        &form(&[
            ("_csrf", &page.csrf()),
            ("title", "스티커 글"),
            ("body", &body),
        ]),
    );
    assert_eq!(res.status, 303, "{}", res.body);
    let url = res.location();
    let comment = ":cat-1: 안녕 <b>굵게</b> 12:30 :cat-999:";
    let res = http(
        &bind,
        "POST",
        &format!("{url}/comments"),
        &[FORM, SAME, ("Cookie", &member)],
        &form(&[("body", comment)]),
    );
    assert_eq!(res.status, 303, "{}", res.body);

    let post = http(&bind, "GET", &url, &[], "");
    assert_eq!(post.status, 200, "{}", post.body);
    assert!(!post.body.contains("data-stickers"));
    assert!(!post.body.contains("/assets/stickers.js"));
    assert!(post.body.contains(":cat-1: 안녕"), "꺼져 있으면 글자");
    // 글의 그림 문법은 Markdown 이라 설정과 상관없이 그려지고, 스티커 주소만 스티커 모양이다.
    assert!(
        post.body.contains(&format!(
            "<img src=\"{src}\" alt=\"{label}\" class=\"sticker\""
        )),
        "{}",
        post.body
    );
    assert!(
        !post.body.contains("a.png\" alt=\"바깥\" class=\"sticker\""),
        "바깥 그림은 스티커가 아니다"
    );
    drop(server);

    // 켜면 댓글의 이름이 그림이 되고, 나머지 글자는 이스케이프되고, 모르는 이름은 글자 그대로다.
    let set = maru(&data, &["site", "set", "--stickers", "true"]);
    assert_eq!(set["stickers"], true);
    let (_server, bind) = start_server(&data);
    let post = http(&bind, "GET", &url, &[], "");
    assert_eq!(post.status, 200, "{}", post.body);
    assert!(post.body.contains("data-stickers=\"code\""));
    assert!(post.body.contains("/assets/stickers.js"));
    let at = post.body.find("class=\"comment-body\"").unwrap();
    let shown = &post.body[at..at + post.body[at..].find("</p>").unwrap()];
    assert!(
        shown.contains(&format!(
            "<img class=\"sticker\" src=\"{src}\" alt=\"{label}\""
        )),
        "{shown}"
    );
    assert!(shown.contains("&lt;b&gt;굵게&lt;/b&gt;"), "{shown}");
    assert!(shown.contains("12:30 :cat-999:"), "{shown}");
    assert!(!shown.contains("<b>"), "{shown}");

    // 저장한 값은 글자 그대로다: 고치기 화면에 이름이 돌아온다.
    let edit_at = at + post.body[at..].find("rel=\"nofollow\">고치기").unwrap();
    let href_at = post.body[..edit_at].rfind("href=\"").unwrap() + 6;
    let edit_url = &post.body[href_at..href_at + post.body[href_at..].find('"').unwrap()];
    let edit = http(&bind, "GET", edit_url, &[("Cookie", &member)], "");
    assert_eq!(edit.status, 200, "{}", edit.body);
    assert!(
        edit.body.contains("data-stickers=\"code\""),
        "{}",
        edit.body
    );
    assert!(
        edit.body.contains(":cat-1: 안녕 &lt;b&gt;"),
        "{}",
        edit.body
    );

    // 공개 글쓰기는 Markdown 형식으로 고른다.
    let page = http(&bind, "GET", "/write/free", &[("Cookie", &member)], "");
    assert!(
        page.body.contains("data-stickers=\"markdown\""),
        "{}",
        page.body
    );

    // 관리 화면 설정·프리셋 값이 같은 칸을 쓴다.
    assert_eq!(maru(&data, &["site", "show"])["stickers"], true);
}
