//! GAP Q8: 댓글 그림 (ADR-0116). 실제 바이너리·실제 포트·실제 DB 파일로 게시판 역할 → 칸 → 올리기 →
//! 댓글 → 화면 → 고치기·빼기·지우기까지 본다.
#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::path::Path;
use std::process::{Child, Command, Stdio};

use serde_json::Value;

const BIN: &str = env!("CARGO_BIN_EXE_giwa");
const FORM: (&str, &str) = ("Content-Type", "application/x-www-form-urlencoded");
const SAME: (&str, &str) = ("Sec-Fetch-Site", "same-origin");
const PASSWORD: &str = "test-pass-1234";

/// 1×1 PNG
const PNG: &[u8] = &[
    137, 80, 78, 71, 13, 10, 26, 10, 0, 0, 0, 13, 73, 72, 68, 82, 0, 0, 0, 1, 0, 0, 0, 1, 8, 6, 0,
    0, 0, 31, 21, 196, 137, 0, 0, 0, 13, 73, 68, 65, 84, 120, 218, 99, 100, 96, 248, 95, 15, 0, 2,
    135, 1, 128, 235, 71, 186, 146, 0, 0, 0, 0, 73, 69, 78, 68, 174, 66, 96, 130,
];

struct Server(Child);

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn giwa(data: &Path, args: &[&str]) -> Value {
    let out = Command::new(BIN)
        .arg("--data")
        .arg(data)
        .args(args)
        .arg("--json")
        .output()
        .expect("giwa 실행");
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
    http_raw(bind, method, path, headers, body.as_bytes())
}

fn http_raw(bind: &str, method: &str, path: &str, headers: &[(&str, &str)], body: &[u8]) -> Res {
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
        return format!("giwa_session={s}");
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
        .cookie("giwa_session")
        .unwrap_or_else(|| panic!("로그인 실패 {}: {}", res.status, res.body));
    format!("giwa_session={session}")
}

fn upload(bind: &str, board: &str, name: &str, bytes: &[u8], cookie: Option<&str>) -> Res {
    let path = format!("/write/{board}/comment-image?filename={}", enc(name));
    let mut headers = vec![SAME];
    if let Some(c) = cookie {
        headers.push(("Cookie", c));
    }
    http_raw(bind, "POST", &path, &headers, bytes)
}

fn allowed(bind: &str, board: &str, cookie: Option<&str>) -> bool {
    let path = format!("/write/{board}/comment-image");
    let mut headers = vec![];
    if let Some(c) = cookie {
        headers.push(("Cookie", c));
    }
    let res = http(bind, "GET", &path, &headers, "");
    assert_eq!(res.status, 200, "{}", res.body);
    let v: Value = serde_json::from_str(&res.body).unwrap();
    v["data"]["allowed"].as_bool().unwrap()
}

fn say(bind: &str, url: &str, cookie: &str, body: &str, image: &str) -> Res {
    http(
        bind,
        "POST",
        &format!("{url}/comments"),
        &[FORM, SAME, ("Cookie", cookie)],
        &form(&[("body", body), ("image", image)]),
    )
}

/// 그 댓글 칸의 글자 (`<li id="comment-…">` 부터 다음 `</li>` 까지).
fn comment_html(page: &str, id: &str) -> String {
    let at = page.find(&format!("id=\"comment-{id}\"")).unwrap();
    page[at..at + page[at..].find("</li>").unwrap()].to_owned()
}

fn comment_id(location: &str) -> String {
    location.rsplit_once("#comment-").unwrap().1.to_owned()
}

#[test]
fn member_attaches_an_image_to_a_comment() {
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
            "--flood-seconds",
            "0",
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
    let (server, bind) = start_server(&data);
    let member = signup_and_login(&bind, "moon@example.test", "달님");

    let page = http(&bind, "GET", "/write/free", &[("Cookie", &member)], "");
    let res = http(
        &bind,
        "POST",
        "/write/free",
        &[FORM, ("Cookie", &member)],
        &form(&[
            ("_csrf", &page.csrf()),
            ("title", "그림 글"),
            ("body", "본문"),
        ]),
    );
    assert_eq!(res.status, 303, "{}", res.body);
    let url = res.location();

    // 기본값: 게시판이 올리기를 받지 않으면 칸도 없고 올릴 수도, 붙일 수도 없다.
    let post = http(&bind, "GET", &url, &[], "");
    assert!(!post.body.contains("data-comment-image"));
    assert!(!post.body.contains("/assets/comment-image.js"));
    assert!(!allowed(&bind, "free", Some(&member)));
    assert_eq!(
        upload(&bind, "free", "a.png", PNG, Some(&member)).status,
        403
    );
    let res = say(&bind, &url, &member, "몰래", "/media/2026/11/none.png");
    assert_eq!(res.status, 403, "{}", res.body);
    drop(server);

    // 게시판을 열면 칸이 나오고, 회원만 된다.
    giwa(&data, &["board", "update", "free", "--upload", "member"]);
    let (server, bind) = start_server(&data);
    let post = http(&bind, "GET", &url, &[], "");
    assert!(post.body.contains("data-comment-image"), "{}", post.body);
    assert!(post.body.contains("/assets/comment-image.js"));
    assert_eq!(
        http(&bind, "GET", "/assets/comment-image.js", &[], "").status,
        200
    );
    assert!(allowed(&bind, "free", Some(&member)));
    assert!(!allowed(&bind, "free", None));
    assert_eq!(upload(&bind, "free", "a.png", PNG, None).status, 401);

    // 다른 사이트에서 보낸 요청, 문서 파일은 받지 않는다.
    let cross = http_raw(
        &bind,
        "POST",
        "/write/free/comment-image?filename=a.png",
        &[("Sec-Fetch-Site", "cross-site"), ("Cookie", &member)],
        PNG,
    );
    assert_eq!(cross.status, 403);
    assert_eq!(
        upload(&bind, "free", "약관.pdf", b"%PDF-1.4", Some(&member)).status,
        400
    );

    let res = upload(&bind, "free", "달.png", PNG, Some(&member));
    assert_eq!(res.status, 201, "{}", res.body);
    let v: Value = serde_json::from_str(&res.body).unwrap();
    let image = v["data"]["url"].as_str().unwrap().to_owned();
    assert!(image.starts_with("/media/"), "{image}");
    assert_eq!(http(&bind, "GET", &image, &[], "").status, 200);

    // 바깥 주소, 없는 그림은 댓글에 붙지 않고 쓴 글은 다시 쓰기 화면에 돌아온다.
    for bad in [
        "https://example.test/a.png",
        "/media/2026/11/01none.png",
        "javascript:alert(1)",
    ] {
        let res = say(&bind, &url, &member, "바깥 그림", bad);
        assert_eq!(res.status, 400, "{bad}: {}", res.body);
        assert!(res.body.contains("바깥 그림"), "{}", res.body);
    }

    // 붙인 댓글: 화면에 그림이 있고, 본문은 그대로다. 그림만은 받지 않는다.
    assert_eq!(say(&bind, &url, &member, "  ", &image).status, 400);
    let res = say(&bind, &url, &member, "달 사진입니다", &image);
    assert_eq!(res.status, 303, "{}", res.body);
    let id = comment_id(&res.location());
    let post = http(&bind, "GET", &url, &[], "");
    let shown = comment_html(&post.body, &id);
    assert!(shown.contains(&format!("<img src=\"{image}\"")), "{shown}");
    assert!(shown.contains("달님 님이 붙인 그림"), "{shown}");
    assert!(shown.contains("달 사진입니다"), "{shown}");

    // 관리 경로(CLI)의 JSON 에도 주소가 나온다.
    let listed = giwa(&data, &["comment", "list", "--status", "published"]);
    assert_eq!(listed["items"][0]["image"], image.as_str());

    // 쓰기 요청 제한(분당 10회)이 메모리에 있어 서버를 다시 띄운다. 세션은 DB 에 남는다.
    drop(server);
    let (_server, bind) = start_server(&data);

    // 고치기 화면: 칸과 지금 그림이 있다. 본문만 고치면 그림은 남고, 빈 값을 보내면 빠진다.
    let edit = http(
        &bind,
        "GET",
        &format!("/c/{id}/edit"),
        &[("Cookie", &member)],
        "",
    );
    assert_eq!(edit.status, 200, "{}", edit.body);
    assert!(edit.body.contains("data-comment-image"), "{}", edit.body);
    assert!(
        edit.body
            .contains(&format!("name=\"image\" value=\"{image}\"")),
        "{}",
        edit.body
    );
    let csrf = edit.csrf();
    let send_edit = |pairs: &[(&str, &str)]| {
        http(
            &bind,
            "POST",
            &format!("/c/{id}/edit"),
            &[FORM, SAME, ("Cookie", &member)],
            &form(pairs),
        )
    };
    let res = send_edit(&[("_csrf", &csrf), ("body", "고친 글")]);
    assert_eq!(res.status, 303, "{}", res.body);
    let post = http(&bind, "GET", &url, &[], "");
    let shown = comment_html(&post.body, &id);
    assert!(
        shown.contains("고친 글") && shown.contains(&image),
        "{shown}"
    );

    let res = send_edit(&[("_csrf", &csrf), ("body", "고친 글"), ("image", "")]);
    assert_eq!(res.status, 303, "{}", res.body);
    let post = http(&bind, "GET", &url, &[], "");
    assert!(!comment_html(&post.body, &id).contains("<img"));

    let res = send_edit(&[
        ("_csrf", &csrf),
        ("body", "다시"),
        ("image", "https://a.test/b.png"),
    ]);
    assert_eq!(res.status, 400, "{}", res.body);
    let res = send_edit(&[("_csrf", &csrf), ("body", "다시"), ("image", &image)]);
    assert_eq!(res.status, 303, "{}", res.body);

    // 역할을 닫아도 있던 그림은 그대로 두거나 뺄 수 있다. 새로 붙이지는 못한다.
    giwa(&data, &["board", "update", "free", "--upload", "none"]);
    let second = say(&bind, &url, &member, "새 그림", &image);
    assert_eq!(second.status, 403, "{}", second.body);
    let res = send_edit(&[("_csrf", &csrf), ("body", "또"), ("image", &image)]);
    assert_eq!(res.status, 303, "{}", res.body);
    giwa(&data, &["board", "update", "free", "--upload", "member"]);

    // 지우면 그림도 사라진다.
    let res = http(
        &bind,
        "POST",
        &format!("/c/{id}/delete"),
        &[FORM, SAME, ("Cookie", &member)],
        &form(&[("_csrf", &csrf)]),
    );
    assert!(res.status == 303 || res.status == 200, "{}", res.body);
    let post = http(&bind, "GET", &url, &[], "");
    assert!(!post.body.contains(&format!("<img src=\"{image}\"")));
    let listed = giwa(&data, &["comment", "list", "--status", "deleted"]);
    assert!(listed["items"][0]["image"].is_null(), "{listed}");

    // 감사 기록에 올리기·붙이기가 남는다.
    let audit = giwa(&data, &["audit", "list", "--action", "media.upload"]);
    assert_eq!(audit["total"], 1, "{audit}");
}
