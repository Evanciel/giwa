//! GAP M17: 개인정보 보유기간·휴면 회원 (ADR-0073). 실제 바이너리·실제 포트·실제 DB 파일로
//! CLI 설정 → `maru privacy run` 이 미리 알림을 쌓고 → REST 가 휴면으로 바꾸면 세션이 끊기고 주소가
//! 분리 표로 가는지, 관리 화면이 주소 대신 「휴면」을 보이는지, 비밀번호 로그인으로 돌아오는지 본다.
#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::path::Path;
use std::process::{Child, Command, Stdio};

use serde_json::Value;

const BIN: &str = env!("CARGO_BIN_EXE_maru");
/// 테스트 전용 값. 실제 비밀번호가 아니다.
const PASSWORD: &str = "test-pass-1234";
const EMAIL: &str = "moon@example.test";

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
            let first = v.trim().split(';').next()?;
            let (k, v) = first.split_once('=')?;
            (k == name && !v.is_empty()).then(|| v.to_owned())
        })
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

fn db_exec(data: &Path, sql: &'static str) {
    db(data, async |db| {
        sqlx::query(sql).execute(db.write_pool()).await.unwrap();
    });
}

fn db_text(data: &Path, sql: &'static str) -> String {
    db(data, async |db| {
        sqlx::query_scalar(sql)
            .fetch_one(db.read_pool())
            .await
            .unwrap()
    })
}

fn db_count(data: &Path, sql: &'static str) -> i64 {
    db(data, async |db| {
        sqlx::query_scalar(sql)
            .fetch_one(db.read_pool())
            .await
            .unwrap()
    })
}

fn login_password(bind: &str) -> Res {
    let page = http(bind, "GET", "/login", &[], "");
    let pre = page.cookie("maru_csrf").expect("로그인 폼 CSRF 쿠키");
    http(
        bind,
        "POST",
        "/login",
        &[FORM, ("Cookie", &format!("maru_csrf={pre}"))],
        &form(&[
            ("_csrf", &pre),
            ("next", "/me"),
            ("email", EMAIL),
            ("password", PASSWORD),
        ]),
    )
}

/// 운영자 로그인 링크로 관리 화면 세션을 얻는다.
fn sign_in(bind: &str, link_url: &str) -> String {
    let rest = link_url.split_once("://").unwrap().1;
    let path = &rest[rest.find('/').unwrap()..];
    let page = http(bind, "GET", path, &[], "");
    let csrf = page.cookie("maru_csrf").unwrap();
    let res = http(
        bind,
        "POST",
        path,
        &[FORM, ("Cookie", &format!("maru_csrf={csrf}"))],
        &format!("_csrf={csrf}"),
    );
    assert_eq!(res.status, 303, "{}", res.body);
    format!("maru_session={}", res.cookie("maru_session").unwrap())
}

#[test]
fn dormant_notice_sleep_and_wake() {
    let tmp = tempfile::tempdir().unwrap();
    let data = tmp.path().join("사이트");
    maru(&data, &["init", "--preset", "blog", "--yes"]);
    let s = maru(
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
            "--dormant-days",
            "365",
            "--mail-retention-days",
            "30",
        ],
    );
    assert_eq!(
        (
            s["dormant_days"].as_u64(),
            s["mail_retention_days"].as_u64()
        ),
        (Some(365), Some(30))
    );
    let admin = maru(
        &data,
        &["token", "create", "--name", "운영", "--scope", "admin"],
    );
    let admin = format!("Bearer {}", admin["secret"].as_str().unwrap());
    let reader = maru(
        &data,
        &["token", "create", "--name", "읽기", "--scope", "read"],
    );
    let reader = format!("Bearer {}", reader["secret"].as_str().unwrap());
    let link = maru(&data, &["login-link"]);
    let (_server, bind) = start_server(&data);

    // 가입하고 로그인한다.
    let page = http(&bind, "GET", "/signup", &[], "");
    let pre = page.cookie("maru_csrf").unwrap();
    let res = http(
        &bind,
        "POST",
        "/signup",
        &[FORM, ("Cookie", &format!("maru_csrf={pre}"))],
        &form(&[
            ("_csrf", &pre),
            ("email", EMAIL),
            ("display_name", "달빛"),
            ("password", PASSWORD),
            ("password2", PASSWORD),
        ]),
    );
    assert_eq!(res.status, 200, "{}", res.body);
    let res = login_password(&bind);
    assert_eq!(res.status, 303, "{}", res.body);
    let moon = format!("maru_session={}", res.cookie("maru_session").unwrap());
    assert_eq!(
        db_count(
            &data,
            "SELECT count(*) FROM members WHERE email = 'moon@example.test' AND last_login_at IS NOT NULL"
        ),
        1,
        "로그인하면 마지막 로그인이 남는다"
    );

    // 1년 넘게 안 들어온 것으로 돌리면 CLI 가 미리 알림을 쌓는다 (운영자는 빠진다).
    db_exec(
        &data,
        "UPDATE members SET last_login_at = '2000-01-01T00:00:00Z', created_at = '2000-01-01T00:00:00Z'",
    );
    let r = maru(&data, &["privacy", "run"]);
    assert_eq!(
        (r["notified"].as_u64(), r["dormant"].as_u64()),
        (Some(1), Some(0)),
        "{r}"
    );
    assert_eq!(
        db_count(
            &data,
            "SELECT count(*) FROM mail_outbox WHERE to_addr = 'moon@example.test' AND subject LIKE '%휴면%'"
        ),
        1
    );

    // 알린 지 30일이 지나면 REST 로 돌린 정리가 휴면으로 바꾼다. 읽기 토큰은 못 돌린다.
    db_exec(
        &data,
        "UPDATE members SET dormant_notice_at = '2000-02-01T00:00:00Z' WHERE dormant_notice_at IS NOT NULL",
    );
    let res = http(
        &bind,
        "POST",
        "/api/v1/privacy/run",
        &[("Authorization", &reader)],
        "",
    );
    assert_eq!(res.status, 403, "{}", res.body);
    let res = http(
        &bind,
        "POST",
        "/api/v1/privacy/run",
        &[("Authorization", &admin)],
        "",
    );
    assert_eq!(res.status, 200, "{}", res.body);
    let v: Value = serde_json::from_str(&res.body).unwrap();
    assert_eq!(v["data"]["dormant"], 1, "{v}");
    assert_eq!(
        db_text(&data, "SELECT email FROM member_dormant"),
        EMAIL,
        "주소는 분리 표로"
    );
    assert!(
        db_text(
            &data,
            "SELECT email FROM members WHERE display_name = '달빛'"
        )
        .ends_with("@invalid")
    );

    // 세션이 끊겼다.
    let me = http(&bind, "GET", "/me", &[("Cookie", &moon)], "");
    assert_ne!(me.status, 200, "휴면이면 로그인이 풀린다");

    // 관리 화면은 주소 대신 「휴면」을 보인다. REST 회원 보기에도 dormant 가 있다.
    let session = sign_in(&bind, link["url"].as_str().unwrap());
    let list = http(&bind, "GET", "/admin/members", &[("Cookie", &session)], "");
    assert_eq!(list.status, 200);
    assert!(
        list.body.contains("휴면") && list.body.contains("메일 따로 보관"),
        "{}",
        list.body
    );
    assert!(!list.body.contains(EMAIL));
    let res = http(
        &bind,
        "GET",
        "/api/v1/members",
        &[("Authorization", &admin)],
        "",
    );
    assert!(res.body.contains("\"dormant\":true"), "{}", res.body);

    // 비밀번호로 로그인하면 바로 돌아온다.
    let res = login_password(&bind);
    assert_eq!(res.status, 303, "{}", res.body);
    assert_eq!(db_count(&data, "SELECT count(*) FROM member_dormant"), 0);
    assert_eq!(
        db_text(
            &data,
            "SELECT email FROM members WHERE display_name = '달빛'"
        ),
        EMAIL
    );
    assert_eq!(
        db_count(
            &data,
            "SELECT count(*) FROM audit_log WHERE action IN ('member.dormant_notice', 'member.dormant', 'member.wake')"
        ),
        3
    );
    let me = http(
        &bind,
        "GET",
        "/me",
        &[(
            "Cookie",
            &format!("maru_session={}", res.cookie("maru_session").unwrap()),
        )],
        "",
    );
    assert_eq!(me.status, 200);
}

/// 켜진 모듈의 보존 정리: 알림 모듈은 `keep_days` 가 지난 알림을 지운다. 꺼진 모듈은 부르지 않는다.
#[test]
fn module_purge_removes_old_notifications() {
    let tmp = tempfile::tempdir().unwrap();
    let data = tmp.path().join("사이트");
    maru(&data, &["init", "--preset", "blog", "--yes"]);
    maru(&data, &["module", "enable", "notify"]);
    db_exec(
        &data,
        "INSERT INTO mod_notify_item (id, member_id, kind, source, post_id, comment_id, at, read_at)
         SELECT 'old', id, 'comment', 'comment:x', 'p1', 'x', '2000-01-01T00:00:00Z', NULL FROM members LIMIT 1",
    );
    db_exec(
        &data,
        "INSERT INTO mod_notify_item (id, member_id, kind, source, post_id, comment_id, at, read_at)
         SELECT 'new', id, 'comment', 'comment:y', 'p1', 'y', strftime('%Y-%m-%dT%H:%M:%SZ', 'now'), NULL FROM members LIMIT 1",
    );
    maru(&data, &["module", "disable", "notify"]);
    let r = maru(&data, &["privacy", "run"]);
    assert_eq!(r["module_removed"], 0, "꺼진 모듈은 정리하지 않는다");
    maru(&data, &["module", "enable", "notify"]);
    let r = maru(&data, &["privacy", "run"]);
    assert_eq!(r["module_removed"], 1, "{r}");
    assert_eq!(
        db_text(&data, "SELECT group_concat(id) FROM mod_notify_item"),
        "new"
    );
    maru(
        &data,
        &["module", "set", "notify", "--field", "keep_days=7"],
    );
    let err = Command::new(BIN)
        .arg("--data")
        .arg(&data)
        .args([
            "module",
            "set",
            "notify",
            "--field",
            "keep_days=3",
            "--json",
        ])
        .output()
        .unwrap();
    assert!(!err.status.success(), "keep_days 는 7 이상");
}
