//! GAP M6: 공개 화면 글쓰기 (ADR-0053). 실제 바이너리·실제 포트·실제 DB 파일로
//! 손님 글 → 확인 질문 → 승인 대기 → 발행 → 목록·글 화면에 손님 이름, 비밀번호로 고치기·지우기,
//! 회원 글은 바로 공개, 다른 출처·함정 칸·CSRF 없는 요청 거부, 손님은 회원 게시판에서 로그인으로.
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
        // 드물게 500 이 난다(ADR-0120). 다음에 나면 서버 오류 줄이 게이트 기록에 남게 버리지 않는다.
        .stderr(Stdio::inherit())
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

fn db<T>(data: &Path, f: impl AsyncFnOnce(&maru_storage::Db) -> T) -> T {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(async {
        let db = maru_storage::Db::open(&data.join("maru.db"), false)
            .await
            .unwrap();
        let v = f(&db).await;
        db.close().await;
        v
    })
}

/// 지금 남아 있는 확인 질문의 답 (테스트는 한 번에 하나만 받는다).
fn latest_answer(data: &Path) -> String {
    db(data, async |db| {
        sqlx::query_scalar("SELECT answer FROM challenges ORDER BY issued_at DESC LIMIT 1")
            .fetch_one(db.read_pool())
            .await
            .unwrap()
    })
}

fn post_column(data: &Path, id: &str, column: &str) -> String {
    let sql = match column {
        "slug" => "SELECT slug FROM posts WHERE id = ?",
        "status" => "SELECT status FROM posts WHERE id = ?",
        "body_md" => "SELECT body_md FROM posts WHERE id = ?",
        "guest_pw_hash" => "SELECT guest_pw_hash FROM posts WHERE id = ?",
        other => panic!("열 {other}"),
    };
    db(data, async |db| {
        sqlx::query_scalar(sql)
            .bind(id)
            .fetch_one(db.read_pool())
            .await
            .unwrap()
    })
}

/// 숨은 칸 값 (`name="..." value="..."`).
fn hidden(body: &str, name: &str) -> String {
    let key = format!("name=\"{name}\" value=\"");
    let at = body.find(&key).unwrap_or_else(|| panic!("{name} 칸 없음"));
    let rest = &body[at + key.len()..];
    rest[..rest.find('"').unwrap()].to_owned()
}

/// 게시판 `free`(자유게시판).
fn site(data: &Path, write: &str) {
    maru(data, &["init", "--preset", "blog", "--yes"]);
    maru(
        data,
        &[
            "board", "create", "free", "--name", "자유", "--kind", "free",
        ],
    );
    let b = maru(data, &["board", "update", "free", "--write", write]);
    assert_eq!(b["policy"]["write"], write);
    assert_eq!(b["policy"]["write_approval"], "guest");
}

#[test]
fn guest_writes_with_captcha_waits_for_review_and_edits_by_password() {
    let tmp = tempfile::tempdir().unwrap();
    let data = tmp.path().join("사이트");
    site(&data, "guest");
    let (_server, started) = start_server(&data);
    let bind = started["bind"].as_str().unwrap().to_owned();

    let board = http(&bind, "GET", "/b/free", &[], "");
    assert!(board.body.contains("href=\"/write/free\""), "글쓰기 단추");
    let page = http(&bind, "GET", "/write/free", &[], "");
    assert_eq!(page.status, 200, "{}", page.body);
    assert_eq!(page.header("cache-control").unwrap(), "no-store");
    assert!(page.body.contains("확인 질문"));
    assert!(!page.body.contains("name=\"secret\""), "손님에게 비밀글 칸");
    let pre = page.cookie("maru_csrf").unwrap();
    let cookie = format!("maru_csrf={pre}");
    let csrf = hidden(&page.body, "_csrf");
    let token = hidden(&page.body, "captcha_token");
    let fields = [
        ("_csrf", csrf.as_str()),
        ("title", "손님이 쓴 글"),
        ("body", "**처음** 왔습니다.\n\n<script>alert(1)</script>"),
        ("tags", "인사, #첫글"),
        ("name", "지나가던손님"),
        ("password", "1234"),
        ("captcha_token", token.as_str()),
    ];

    // 다른 사이트에서 보낸 요청, 함정 칸, CSRF 없는 요청은 받지 않는다.
    let res = http(
        &bind,
        "POST",
        "/write/free",
        &[FORM, ("Cookie", &cookie), ("Sec-Fetch-Site", "cross-site")],
        &form(&fields),
    );
    assert_eq!(res.status, 403);
    let mut trap = fields.to_vec();
    trap.push(("website", "http://spam.example"));
    let res = http(
        &bind,
        "POST",
        "/write/free",
        &[FORM, SAME, ("Cookie", &cookie)],
        &form(&trap),
    );
    assert_eq!(res.status, 403);
    let res = http(&bind, "POST", "/write/free", &[FORM, SAME], &form(&fields));
    assert_eq!(res.status, 403, "CSRF 쿠키 없이");

    // 틀린 답: 적은 내용 그대로 새 질문과 함께.
    std::thread::sleep(std::time::Duration::from_millis(2100));
    let mut wrong = fields.to_vec();
    wrong.push(("captcha_answer", "999"));
    let again = http(
        &bind,
        "POST",
        "/write/free",
        &[FORM, SAME, ("Cookie", &cookie)],
        &form(&wrong),
    );
    assert_eq!(again.status, 400, "{}", again.body);
    assert!(again.body.contains("답이 맞지 않습니다"));
    assert!(again.body.contains("value=\"손님이 쓴 글\""));
    let token = hidden(&again.body, "captcha_token");
    let answer = latest_answer(&data);
    std::thread::sleep(std::time::Duration::from_millis(2100));
    let mut right: Vec<(&str, &str)> = fields
        .iter()
        .copied()
        .filter(|(k, _)| *k != "captcha_token")
        .collect();
    right.extend([
        ("captcha_token", token.as_str()),
        ("captcha_answer", answer.as_str()),
    ]);
    let res = http(
        &bind,
        "POST",
        "/write/free",
        &[FORM, SAME, ("Cookie", &cookie)],
        &form(&right),
    );
    assert_eq!(res.status, 200, "{}", res.body);
    assert!(res.body.contains("관리자가 확인한 뒤에 목록에 보입니다"));
    let board = http(&bind, "GET", "/b/free", &[], "");
    assert!(!board.body.contains("손님이 쓴 글"), "승인 전에 보임");

    // 승인 대기 → 운영자가 발행 → 목록·글 화면에 손님 이름.
    let queue = maru(&data, &["post", "list", "--status", "pending"]);
    assert_eq!(queue["total"], 1, "{queue}");
    let id = queue["items"][0]["id"].as_str().unwrap().to_owned();
    assert_eq!(queue["items"][0]["guest_name"], "지나가던손님");
    assert!(queue["items"][0].get("guest_pw_hash").is_none());
    assert!(post_column(&data, &id, "guest_pw_hash").starts_with("$argon2id$"));
    maru(&data, &["post", "publish", &id]);
    let board = http(&bind, "GET", "/b/free", &[], "");
    assert!(board.body.contains("손님이 쓴 글"));
    assert!(board.body.contains("지나가던손님"));
    let slug = post_column(&data, &id, "slug");
    let page = http(&bind, "GET", &format!("/b/free/{slug}"), &[], "");
    assert_eq!(page.status, 200);
    assert!(page.body.contains("<strong>처음</strong>"));
    assert!(!page.body.contains("<script>alert"), "정화되지 않은 HTML");
    // 화면의 고치기 링크(한글 슬러그는 퍼센트 인코딩)를 그대로 따라간다.
    let at = page
        .body
        .find("/edit\" rel=\"nofollow\"")
        .expect("고치기 링크");
    let start = page.body[..at].rfind("href=\"").unwrap() + 6;
    let url = page.body[start..at].to_owned();
    assert!(url.starts_with("/b/free/"), "{url}");

    // 비밀번호로 고치기.
    let edit = http(
        &bind,
        "GET",
        &format!("{url}/edit"),
        &[("Cookie", &cookie)],
        "",
    );
    assert_eq!(edit.status, 200, "{}", edit.body);
    assert!(edit.body.contains("쓸 때 정한 비밀번호"));
    assert!(edit.body.contains("value=\"인사, 첫글\""), "태그 채움");
    let csrf = hidden(&edit.body, "_csrf");
    let change = |pw: &'static str| {
        form(&[
            ("_csrf", csrf.as_str()),
            ("title", "손님이 쓴 글"),
            ("body", "고친 손님 본문"),
            ("tags", "인사"),
            ("password", pw),
        ])
    };
    let res = http(
        &bind,
        "POST",
        &format!("{url}/edit"),
        &[FORM, SAME, ("Cookie", &cookie)],
        &change("0000"),
    );
    assert_eq!(res.status, 403);
    let res = http(
        &bind,
        "POST",
        &format!("{url}/edit"),
        &[FORM, SAME, ("Cookie", &cookie)],
        &change("1234"),
    );
    assert_eq!(res.status, 303, "{}", res.body);
    assert_eq!(res.header("location").unwrap(), url);
    assert_eq!(post_column(&data, &id, "body_md"), "고친 손님 본문");

    // 비밀번호로 지우기 → 목록으로, DB 는 deleted.
    let del = http(
        &bind,
        "GET",
        &format!("{url}/delete"),
        &[("Cookie", &cookie)],
        "",
    );
    let csrf = hidden(&del.body, "_csrf");
    let res = http(
        &bind,
        "POST",
        &format!("{url}/delete"),
        &[FORM, SAME, ("Cookie", &cookie)],
        &form(&[("_csrf", &csrf), ("password", "1234")]),
    );
    assert_eq!(res.status, 303, "{}", res.body);
    assert_eq!(res.header("location").unwrap(), "/b/free");
    assert_eq!(post_column(&data, &id, "status"), "deleted");
}

#[test]
fn member_post_is_public_at_once_and_guests_are_sent_to_login() {
    let tmp = tempfile::tempdir().unwrap();
    let data = tmp.path().join("사이트");
    site(&data, "member");
    maru(
        &data,
        &[
            "site",
            "set",
            "--signup",
            "open",
            "--signup-verify",
            "false",
            "--captcha",
            "off",
        ],
    );
    let (_server, started) = start_server(&data);
    let bind = started["bind"].as_str().unwrap().to_owned();

    let res = http(&bind, "GET", "/write/free", &[], "");
    assert_eq!(res.status, 303);
    assert_eq!(
        res.header("location").unwrap(),
        "/login?next=%2Fwrite%2Ffree"
    );

    let page = http(&bind, "GET", "/signup", &[], "");
    let pre = page.cookie("maru_csrf").unwrap();
    let cookie = format!("maru_csrf={pre}");
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
            ("next", "/write/free"),
            ("email", "dal@example.test"),
            ("password", "test-pass-1234"),
        ]),
    );
    assert_eq!(res.status, 303, "{}", res.body);
    let session = format!("maru_session={}", res.cookie("maru_session").unwrap());

    let page = http(&bind, "GET", "/write/free", &[("Cookie", &session)], "");
    assert_eq!(page.status, 200, "{}", page.body);
    assert!(!page.body.contains("확인 질문"));
    assert!(page.body.contains("name=\"secret\""));
    let csrf = hidden(&page.body, "_csrf");
    let fields = [
        ("_csrf", csrf.as_str()),
        ("title", "회원 글"),
        ("body", "바로 보입니다"),
    ];
    let res = http(
        &bind,
        "POST",
        "/write/free",
        &[FORM, ("Cookie", &session), ("Sec-Fetch-Site", "cross-site")],
        &form(&fields),
    );
    assert_eq!(res.status, 403, "다른 사이트 POST");
    let res = http(
        &bind,
        "POST",
        "/write/free",
        &[FORM, SAME, ("Cookie", &session)],
        &form(&fields),
    );
    assert_eq!(res.status, 303, "{}", res.body);
    let url = res.header("location").unwrap().to_owned();
    assert!(url.starts_with("/b/free/"), "{url}");
    let page = http(&bind, "GET", &url, &[], "");
    assert!(page.body.contains("바로 보입니다"));
    assert!(page.body.contains("달빛"));
    let mine = maru(
        &data,
        &["post", "list", "--board", "free", "--status", "published"],
    );
    assert_eq!(mine["items"][0]["title"], "회원 글");
    assert!(mine["items"][0]["author_id"].is_string());

    // 손님은 회원 글 고치기 화면을 열지 못하고 로그인으로.
    let res = http(&bind, "GET", &format!("{url}/edit"), &[], "");
    assert_eq!(res.status, 303);
    assert!(res.header("location").unwrap().starts_with("/login"));
    // 글쓴이는 고친다.
    let edit = http(
        &bind,
        "GET",
        &format!("{url}/edit"),
        &[("Cookie", &session)],
        "",
    );
    assert!(!edit.body.contains("쓸 때 정한 비밀번호"));
    let csrf = hidden(&edit.body, "_csrf");
    let res = http(
        &bind,
        "POST",
        &format!("{url}/edit"),
        &[FORM, SAME, ("Cookie", &session)],
        &form(&[
            ("_csrf", &csrf),
            ("title", "회원 글"),
            ("body", "고친 회원 본문"),
        ]),
    );
    assert_eq!(res.status, 303, "{}", res.body);
    let page = http(&bind, "GET", &url, &[], "");
    assert!(page.body.contains("고친 회원 본문"));
}
