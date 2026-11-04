//! GAP M5·M6·M10: 댓글·비회원 댓글·자체 확인 질문 (ADR-0052). 실제 바이너리·실제 포트·실제 DB 파일로
//! 손님 댓글 → 확인 질문 → 승인 대기 → 승인 → 글 화면·목록·REST 에 보이는지, 다른 출처·함정 칸을 막는지,
//! 비밀번호로 고치는지, 회원 댓글은 바로 보이는지 본다.
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

fn start_server(data: &Path) -> (Server, Value) {
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
    (server, v["data"].clone())
}

struct Res {
    status: u16,
    head: String,
    body: String,
}

impl Res {
    fn header(&self, name: &str) -> Option<String> {
        let name = name.to_ascii_lowercase();
        self.head.lines().find_map(|l| {
            let (k, v) = l.split_once(':')?;
            (k.trim().to_ascii_lowercase() == name).then(|| v.trim().to_owned())
        })
    }

    /// `Set-Cookie` 에서 이름에 맞는 값.
    fn cookie(&self, name: &str) -> Option<String> {
        self.head.lines().find_map(|l| {
            let (k, v) = l.split_once(':')?;
            if !k.trim().eq_ignore_ascii_case("set-cookie") {
                return None;
            }
            let first = v.trim().split(';').next()?;
            let (k, v) = first.split_once('=')?;
            (k == name && !v.is_empty()).then(|| v.to_owned())
        })
    }

    /// 화면의 첫 `_csrf` 숨은 필드 값.
    fn csrf(&self) -> String {
        let at = self
            .body
            .find("name=\"_csrf\" value=\"")
            .expect("_csrf 필드");
        let rest = &self.body[at + 20..];
        rest[..rest.find('"').unwrap()].to_owned()
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

const FORM: (&str, &str) = ("Content-Type", "application/x-www-form-urlencoded");

fn form(pairs: &[(&str, &str)]) -> String {
    pairs
        .iter()
        .map(|(k, v)| format!("{}={}", enc(k), enc(v)))
        .collect::<Vec<_>>()
        .join("&")
}

fn enc(s: &str) -> String {
    s.bytes()
        .map(|b| {
            if b.is_ascii_alphanumeric() || b"-_.~".contains(&b) {
                (b as char).to_string()
            } else {
                format!("%{b:02X}")
            }
        })
        .collect()
}

const SAME: (&str, &str) = ("Sec-Fetch-Site", "same-origin");

/// 지금 남아 있는 확인 질문의 답 (테스트는 한 번에 하나만 받는다).
fn latest_answer(data: &Path) -> String {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(async {
        let db = maru_storage::Db::open(&data.join("maru.db"), false)
            .await
            .unwrap();
        let v: String =
            sqlx::query_scalar("SELECT answer FROM challenges ORDER BY issued_at DESC LIMIT 1")
                .fetch_one(db.read_pool())
                .await
                .unwrap();
        db.close().await;
        v
    })
}

/// 숨은 칸 값 (`name="..." value="..."`).
fn hidden(body: &str, name: &str) -> String {
    let key = format!("name=\"{name}\" value=\"");
    let at = body.find(&key).unwrap_or_else(|| panic!("{name} 칸 없음"));
    let rest = &body[at + key.len()..];
    rest[..rest.find('"').unwrap()].to_owned()
}

/// 게시판 `free`(비회원 댓글, 비회원 것은 승인 뒤)와 공개 글 하나.
fn site(data: &Path) -> (String, String) {
    maru(data, &["init", "--preset", "blog", "--yes"]);
    maru(
        data,
        &[
            "board", "create", "free", "--name", "자유", "--kind", "free",
        ],
    );
    let b = maru(data, &["board", "update", "free", "--comment", "guest"]);
    assert_eq!(b["policy"]["comment"], "guest");
    assert_eq!(b["policy"]["comment_approval"], "guest");
    let p = maru(
        data,
        &[
            "post",
            "new",
            "--board",
            "free",
            "--title",
            "댓글 받는 글",
            "--slug",
            "talk",
            "--body",
            "본문",
            "--publish",
        ],
    );
    (p["id"].as_str().unwrap().to_owned(), "/b/free/talk".into())
}

#[test]
fn guest_comment_captcha_review_approve_and_edit() {
    let tmp = tempfile::tempdir().unwrap();
    let data = tmp.path().join("사이트");
    let (post_id, url) = site(&data);
    let moderator = maru(
        &data,
        &[
            "token", "create", "--name", "관리", "--scope", "read", "--scope", "publish",
        ],
    );
    let moderator = format!("Bearer {}", moderator["secret"].as_str().unwrap());
    let (_server, started) = start_server(&data);
    let bind = started["bind"].as_str().unwrap().to_owned();
    let action = format!("{url}/comments");

    let page = http(&bind, "GET", &url, &[], "");
    assert_eq!(page.status, 200);
    assert!(page.body.contains("id=\"comment-form\""));
    assert!(page.body.contains("비회원으로 쓸 때만"));

    let fields = [
        ("body", "<b>손님</b> 댓글입니다"),
        ("name", "지나가던손님"),
        ("password", "1234"),
    ];
    // 다른 사이트에서 보낸 요청, 함정 칸을 채운 요청은 받지 않는다.
    let res = http(
        &bind,
        "POST",
        &action,
        &[FORM, ("Sec-Fetch-Site", "cross-site")],
        &form(&fields),
    );
    assert_eq!(res.status, 403);
    let mut trap = fields.to_vec();
    trap.push(("website", "http://spam.example"));
    assert_eq!(
        http(&bind, "POST", &action, &[FORM, SAME], &form(&trap)).status,
        403
    );

    // 손님은 확인 질문을 한 번 거친다. 적은 내용은 그대로 남아 있다.
    let check = http(&bind, "POST", &action, &[FORM, SAME], &form(&fields));
    assert_eq!(check.status, 200, "{}", check.body);
    assert!(check.body.contains("확인 질문"));
    assert!(
        check.body.contains("&lt;b&gt;손님&lt;/b&gt;"),
        "본문 이스케이프"
    );
    let token = hidden(&check.body, "captcha_token");
    // 틀린 답은 새 질문과 함께 다시.
    std::thread::sleep(std::time::Duration::from_millis(2100));
    let mut wrong = fields.to_vec();
    wrong.extend([("captcha_token", token.as_str()), ("captcha_answer", "999")]);
    let again = http(&bind, "POST", &action, &[FORM, SAME], &form(&wrong));
    assert_eq!(again.status, 400);
    assert!(again.body.contains("답이 맞지 않습니다"));
    let token = hidden(&again.body, "captcha_token");
    let answer = latest_answer(&data);
    std::thread::sleep(std::time::Duration::from_millis(2100));
    let mut right = fields.to_vec();
    right.extend([
        ("captcha_token", token.as_str()),
        ("captcha_answer", answer.as_str()),
    ]);
    let res = http(&bind, "POST", &action, &[FORM, SAME], &form(&right));
    assert_eq!(res.status, 303, "{}", res.body);
    assert_eq!(
        res.header("location").unwrap(),
        format!("{url}?comment=pending#comments")
    );
    let pending = http(&bind, "GET", &format!("{url}?comment=pending"), &[], "");
    assert!(pending.body.contains("관리자가 확인한 뒤에 보입니다"));
    assert!(!pending.body.contains("댓글입니다"), "승인 전에 보임");

    // 승인 큐: CLI 와 REST 가 같은 답.
    let queue = maru(&data, &["comment", "list"]);
    assert_eq!(queue["total"], 1);
    let id = queue["items"][0]["id"].as_str().unwrap().to_owned();
    assert_eq!(queue["items"][0]["untrusted"], true);
    let rest = http(
        &bind,
        "GET",
        "/api/v1/comments",
        &[("Authorization", &moderator)],
        "",
    );
    assert_eq!(rest.status, 200, "{}", rest.body);
    let rest: Value = serde_json::from_str(&rest.body).unwrap();
    assert_eq!(rest["data"]["items"][0]["id"], id.as_str());
    assert_eq!(http(&bind, "GET", "/api/v1/comments", &[], "").status, 401);

    let ok = http(
        &bind,
        "POST",
        &format!("/api/v1/comments/{id}/approve"),
        &[("Authorization", &moderator)],
        "",
    );
    assert_eq!(ok.status, 200, "{}", ok.body);
    let page = http(&bind, "GET", &url, &[], "");
    assert!(page.body.contains("&lt;b&gt;손님&lt;/b&gt; 댓글입니다"));
    assert!(page.body.contains("지나가던손님"));
    assert!(!page.body.contains("<b>손님</b>"), "HTML 이 그대로 나감");
    assert!(!page.body.contains("· 고침"), "승인만 했는데 고침 표시");
    let board = http(&bind, "GET", "/b/free", &[], "");
    assert!(board.body.contains("[1]"), "목록 댓글 수");
    let public: Value = serde_json::from_str(
        &http(
            &bind,
            "GET",
            &format!("/api/v1/posts/{post_id}/comments"),
            &[],
            "",
        )
        .body,
    )
    .unwrap();
    assert_eq!(public["data"].as_array().unwrap().len(), 1);
    assert!(public["data"][0].get("guest_pw_hash").is_none());

    // 비회원 댓글 고치기: 비밀번호가 맞아야.
    let edit_url = format!("/c/{id}/edit");
    let form_page = http(&bind, "GET", &edit_url, &[], "");
    assert_eq!(form_page.status, 200);
    let pre = form_page.cookie("maru_csrf").unwrap();
    let cookie = format!("maru_csrf={pre}");
    let res = http(
        &bind,
        "POST",
        &edit_url,
        &[FORM, ("Cookie", &cookie)],
        &form(&[("_csrf", &pre), ("body", "고친 댓글"), ("password", "0000")]),
    );
    assert_eq!(res.status, 403);
    let res = http(
        &bind,
        "POST",
        &edit_url,
        &[FORM, ("Cookie", &cookie)],
        &form(&[("_csrf", &pre), ("body", "고친 댓글"), ("password", "1234")]),
    );
    assert_eq!(res.status, 303, "{}", res.body);
    let published = maru(&data, &["comment", "list", "--status", "published"]);
    assert_eq!(published["items"][0]["body"], "고친 댓글");
    let page = http(&bind, "GET", &url, &[], "");
    assert!(page.body.contains("고친 댓글"));
    assert!(page.body.contains("· 고침"), "고친 댓글 표시");
}

#[test]
fn member_comment_is_public_at_once() {
    let tmp = tempfile::tempdir().unwrap();
    let data = tmp.path().join("사이트");
    let (_, url) = site(&data);
    maru(
        &data,
        &[
            "site",
            "set",
            "--signup",
            "open",
            "--signup-verify",
            "false",
        ],
    );
    let (_server, started) = start_server(&data);
    let bind = started["bind"].as_str().unwrap().to_owned();

    // 가입 폼에도 확인 질문이 있다.
    let page = http(&bind, "GET", "/signup", &[], "");
    assert!(page.body.contains("확인 질문"));
    let pre = page.cookie("maru_csrf").unwrap();
    let cookie = format!("maru_csrf={pre}");
    let token = hidden(&page.body, "captcha_token");
    let answer = latest_answer(&data);
    std::thread::sleep(std::time::Duration::from_millis(2100));
    let res = http(
        &bind,
        "POST",
        "/signup",
        &[FORM, ("Cookie", &cookie)],
        &form(&[
            ("_csrf", &pre),
            ("email", "dal@example.test"),
            ("display_name", "달빛"),
            ("password", "test-pass-1234"),
            ("password2", "test-pass-1234"),
            ("captcha_token", &token),
            ("captcha_answer", &answer),
        ]),
    );
    assert_eq!(res.status, 200, "{}", res.body);
    let login = http(&bind, "GET", "/login", &[("Cookie", &cookie)], "");
    let res = http(
        &bind,
        "POST",
        "/login",
        &[FORM, ("Cookie", &cookie)],
        &form(&[
            ("_csrf", &login.csrf()),
            ("next", &url),
            ("email", "dal@example.test"),
            ("password", "test-pass-1234"),
        ]),
    );
    assert_eq!(res.status, 303, "{}", res.body);
    let session = format!("maru_session={}", res.cookie("maru_session").unwrap());

    // 회원은 질문 없이 바로 공개.
    let res = http(
        &bind,
        "POST",
        &format!("{url}/comments"),
        &[FORM, SAME, ("Cookie", &session)],
        &form(&[("body", "회원 댓글")]),
    );
    assert_eq!(res.status, 303, "{}", res.body);
    let to = res.header("location").unwrap();
    assert!(to.starts_with(&format!("{url}#comment-")), "{to}");
    let page = http(&bind, "GET", &url, &[], "");
    assert!(page.body.contains("회원 댓글"));
    assert!(page.body.contains("달빛"));
    let id = to.rsplit("#comment-").next().unwrap();
    // 내 정보에 「내가 쓴 댓글」, 그 댓글 자리로 가는 링크 (GAP M3).
    let me = http(&bind, "GET", "/me", &[("Cookie", &session)], "");
    assert!(me.body.contains("내가 쓴 댓글"), "{}", me.body);
    assert!(
        me.body.contains(&format!("href=\"{url}#comment-{id}\"")),
        "{}",
        me.body
    );
    let all = http(&bind, "GET", "/me/comments", &[("Cookie", &session)], "");
    assert_eq!(all.status, 200);
    assert!(all.body.contains("회원 댓글"));
    let anon = http(&bind, "GET", "/me/comments", &[], "");
    assert_eq!(anon.status, 303, "로그인하지 않으면 로그인으로");
    // 남이 연 지우기 화면: 회원 댓글은 비밀번호로 못 지운다 → 로그인으로.
    let other = http(&bind, "GET", &format!("/c/{id}/delete"), &[], "");
    assert_eq!(other.status, 303);
    assert!(other.header("location").unwrap().starts_with("/login"));
}
