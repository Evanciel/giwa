//! GAP C14: 포럼형 게시판 (ADR-0058). 실제 바이너리·실제 포트·실제 DB 파일로 활동순 목록,
//! 글쓴이의 답변 채택(확인 화면 → DB), CLI 글 잠금 → 화면 안내·댓글 거부를 확인한다.
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

/// 숨은 칸 값 (`name="..." value="..."`).
fn hidden(body: &str, name: &str) -> String {
    let key = format!("name=\"{name}\" value=\"");
    let at = body.find(&key).unwrap_or_else(|| panic!("{name} 칸 없음"));
    let rest = &body[at + key.len()..];
    rest[..rest.find('"').unwrap()].to_owned()
}

fn scalar(data: &Path, sql: &'static str, id: &str) -> Option<String> {
    let id = id.to_owned();
    db(data, async move |db| {
        sqlx::query_scalar(sql)
            .bind(id)
            .fetch_one(db.read_pool())
            .await
            .unwrap()
    })
}

fn comment_count(data: &Path, post: &str) -> i64 {
    let post = post.to_owned();
    db(data, async move |db| {
        sqlx::query_scalar("SELECT count(*) FROM comments WHERE post_id = ?")
            .bind(post)
            .fetch_one(db.read_pool())
            .await
            .unwrap()
    })
}

/// 회원으로 가입하고 로그인한 세션 쿠키.
fn sign_in(bind: &str, email: &str, name: &str) -> String {
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
            ("password", "test-pass-1234"),
            ("password2", "test-pass-1234"),
        ]),
    );
    assert_eq!(res.status, 200, "{}", res.body);
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
            ("password", "test-pass-1234"),
        ]),
    );
    assert_eq!(res.status, 303, "{}", res.body);
    format!("maru_session={}", res.cookie("maru_session").unwrap())
}

/// 공개 글쓰기 화면으로 글을 쓰고 그 주소를 돌려준다.
fn write(bind: &str, session: &str, title: &str) -> String {
    let page = http(bind, "GET", "/write/qa", &[("Cookie", session)], "");
    assert_eq!(page.status, 200, "{}", page.body);
    let csrf = hidden(&page.body, "_csrf");
    let res = http(
        bind,
        "POST",
        "/write/qa",
        &[FORM, SAME, ("Cookie", session)],
        &form(&[
            ("_csrf", &csrf),
            ("title", title),
            ("body", "어떻게 하나요?"),
        ]),
    );
    assert_eq!(res.status, 303, "{}", res.body);
    std::thread::sleep(std::time::Duration::from_millis(20));
    res.header("location").unwrap()
}

fn comment(bind: &str, session: &str, post_url: &str, body: &str) -> Res {
    http(
        bind,
        "POST",
        &format!("{post_url}/comments"),
        &[FORM, SAME, ("Cookie", session)],
        &form(&[("body", body), ("website", "")]),
    )
}

#[test]
fn forum_orders_by_activity_accepts_answers_and_locks() {
    let tmp = tempfile::tempdir().unwrap();
    let data = tmp.path().join("사이트");
    maru(&data, &["init", "--preset", "blog", "--yes"]);
    let b = maru(
        &data,
        &[
            "board",
            "create",
            "qa",
            "--name",
            "질문답변",
            "--kind",
            "forum",
        ],
    );
    assert_eq!(b["kind"], "forum");
    let bad = run(&data, &["board", "update", "qa", "--order", "hot"]);
    assert_eq!(bad["ok"], false);
    assert!(
        bad["error"]["message"]
            .as_str()
            .unwrap()
            .contains("activity"),
        "{bad}"
    );
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
    let (server, started) = start_server(&data);
    let bind = started["bind"].as_str().unwrap().to_owned();
    let asker = sign_in(&bind, "asker@example.test", "질문러");
    let helper = sign_in(&bind, "helper@example.test", "도우미");

    // 활동순: 오래된 글에 댓글이 달리면 목록 맨 위로.
    let old = write(&bind, &asker, "오래된 질문");
    write(&bind, &asker, "새 질문");
    let list = http(&bind, "GET", "/b/qa", &[], "");
    assert!(list.body.contains("최근 활동"));
    assert!(list.body.find("새 질문").unwrap() < list.body.find("오래된 질문").unwrap());
    let res = comment(&bind, &helper, &old, "설정에서 바꾸면 됩니다");
    assert_eq!(res.status, 303, "{}", res.body);
    let list = http(&bind, "GET", "/b/qa", &[], "");
    assert!(
        list.body.find("오래된 질문").unwrap() < list.body.find("새 질문").unwrap(),
        "댓글 달린 글이 위로"
    );
    let post_id = scalar(&data, "SELECT id FROM posts WHERE title = ?", "오래된 질문").unwrap();
    assert!(
        scalar(
            &data,
            "SELECT last_activity_at FROM posts WHERE id = ?",
            &post_id
        )
        .is_some(),
        "DB 에 활동 시각"
    );
    let cid = scalar(
        &data,
        "SELECT id FROM comments WHERE post_id = ? ORDER BY created_at DESC LIMIT 1",
        &post_id,
    )
    .unwrap();

    // 채택: 글 화면의 단추 → 확인 화면(로그인 필요) → DB.
    let page = http(&bind, "GET", &old, &[], "");
    let accept = format!("/c/{cid}/accept");
    assert!(page.body.contains(&accept), "채택 단추");
    let res = http(&bind, "GET", &accept, &[], "");
    assert_eq!(res.status, 303);
    assert!(res.header("location").unwrap().starts_with("/login"));
    let res = http(&bind, "GET", &accept, &[("Cookie", &helper)], "");
    assert_eq!(res.status, 403, "남의 글은 채택 못 한다");
    let confirm = http(&bind, "GET", &accept, &[("Cookie", &asker)], "");
    assert_eq!(confirm.status, 200, "{}", confirm.body);
    assert!(confirm.body.contains("답변 채택"));
    let csrf = confirm.csrf();
    let res = http(
        &bind,
        "POST",
        &accept,
        &[FORM, ("Cookie", &asker), ("Sec-Fetch-Site", "cross-site")],
        &form(&[("_csrf", &csrf)]),
    );
    assert_eq!(res.status, 403, "다른 사이트 POST");
    let res = http(
        &bind,
        "POST",
        &accept,
        &[FORM, SAME, ("Cookie", &asker)],
        &form(&[("_csrf", &csrf)]),
    );
    assert_eq!(res.status, 303, "{}", res.body);
    assert_eq!(
        res.header("location").unwrap(),
        format!("{old}#comment-{cid}")
    );
    assert_eq!(
        scalar(
            &data,
            "SELECT accepted_comment_id FROM posts WHERE id = ?",
            &post_id
        ),
        Some(cid.clone())
    );
    let page = http(&bind, "GET", &old, &[], "");
    assert!(page.body.contains("채택된 답변"));
    let list = http(&bind, "GET", "/b/qa", &[], "");
    assert!(list.body.contains("solved-mark"), "목록에 해결 표시");
    // 첫 화면 위젯 줄에도 해결 표시 (GAP Q23, ADR-0110). 위젯은 놓자마자 보인다.
    maru(
        &data,
        &[
            "widget",
            "place",
            "latest_posts",
            "--region",
            "sidebar",
            "--config",
            r#"{"board":"qa"}"#,
        ],
    );
    let home = http(&bind, "GET", "/", &[], "");
    let side = &home.body[home.body.find("widget-latest_posts").expect("위젯")..];
    assert!(
        side.contains("<span class=\"solved-mark\">해결</span>"),
        "{side}"
    );

    // 비밀번호·폼 POST 는 분당 10번으로 센다. 서버를 다시 띄워 칸을 비운다 (세션은 DB 에 있다).
    drop(server);
    let (_server, started) = start_server(&data);
    let bind = started["bind"].as_str().unwrap().to_owned();

    // 잠금: CLI → 화면 안내, 회원 댓글 거부, 운영자 풀기.
    let p = maru(&data, &["post", "lock", &post_id]);
    assert_eq!(p["is_locked"], true);
    let page = http(&bind, "GET", &old, &[], "");
    assert!(page.body.contains("잠긴 글이라"));
    assert!(!page.body.contains("id=\"comment-form\""));
    let res = comment(&bind, &helper, &old, "한마디 더");
    assert_eq!(res.status, 403, "{}", res.body);
    assert_eq!(comment_count(&data, &post_id), 1);
    let err = run(
        &data,
        &["post", "accept", &post_id, "--comment", "없는-댓글"],
    );
    assert_eq!(err["ok"], false);
    maru(&data, &["post", "lock", &post_id, "--off"]);
    let res = comment(&bind, &helper, &old, "한마디 더");
    assert_eq!(res.status, 303, "{}", res.body);
    assert_eq!(comment_count(&data, &post_id), 2);

    // CLI 로 채택을 뺀다.
    let p = maru(&data, &["post", "accept", &post_id, "--clear"]);
    assert!(p["accepted_comment_id"].is_null());
    assert_eq!(
        scalar(
            &data,
            "SELECT accepted_comment_id FROM posts WHERE id = ?",
            &post_id
        ),
        None
    );
}
