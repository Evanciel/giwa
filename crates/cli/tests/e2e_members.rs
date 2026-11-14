//! GAP M1·M2·M3: 회원 가입·메일 인증·내 정보 (ADR-0051). 실제 바이너리·실제 포트·실제 DB 파일로
//! 가입 폼 → 보낼 편지함의 링크 → 인증 → 로그인 → 닉네임 변경이 DB 에 남는지,
//! 가입 꺼짐·CSRF 없는 요청·재사용 링크를 막는지, 회원 관리가 REST·CLI 에서 같은 답을 주고
//! 비활성이 세션을 끊는지 본다.
#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use serde_json::Value;

const BIN: &str = env!("CARGO_BIN_EXE_giwa");
/// 테스트 전용 값. 실제 비밀번호가 아니다.
const PASSWORD: &str = "test-pass-1234";
const EMAIL: &str = "dal@example.test";

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

/// 보낼 편지함에서 `to` 에게 간 편지 중 `path` 링크가 든 것의 경로 부분 (`/verify/…`).
fn link_in_outbox(data: &Path, to: &str, path: &str) -> String {
    let list = giwa(data, &["mail", "list"]);
    for m in list.as_array().unwrap() {
        if m["to_addr"] != to {
            continue;
        }
        let body = m["body"].as_str().unwrap();
        if let Some(at) = body.find(path) {
            let rest = &body[at..];
            let end = rest.find(char::is_whitespace).unwrap_or(rest.len());
            return rest[..end].to_owned();
        }
    }
    panic!("{to} 에게 간 {path} 편지가 없음: {list}");
}

#[test]
fn signup_verify_login_and_profile_reach_the_db() {
    let tmp = tempfile::tempdir().unwrap();
    let data = tmp.path().join("사이트");
    let init = giwa(
        &data,
        &["init", "--no-samples", "--preset", "blog", "--yes"],
    );
    let writer = format!("Bearer {}", init["token"]["secret"].as_str().unwrap());
    let admin = giwa(
        &data,
        &[
            "token",
            "create",
            "--name",
            "회원 관리",
            "--scope",
            "read",
            "--scope",
            "admin",
        ],
    );
    let admin = format!("Bearer {}", admin["secret"].as_str().unwrap());

    // 기본값은 가입을 받지 않는다.
    let (server, started) = start_server(&data);
    let bind = started["bind"].as_str().unwrap().to_owned();
    assert_eq!(started["mail"]["transport"], "file", "{started}");
    assert_eq!(http(&bind, "GET", "/signup", &[], "").status, 404);
    assert!(
        !http(&bind, "GET", "/", &[], "")
            .body
            .contains("site-account")
    );
    drop(server);

    let set = giwa(
        &data,
        &["site", "set", "--signup", "open", "--captcha", "off"],
    );
    assert_eq!(set["signup"], "open");
    assert_eq!(set["signup_verify"], true);
    let (_server, started) = start_server(&data);
    let bind = started["bind"].as_str().unwrap().to_owned();
    let mail_dir = std::path::PathBuf::from(started["mail"]["dir"].as_str().unwrap());
    assert!(
        http(&bind, "GET", "/", &[], "")
            .body
            .contains("href=\"/me\"")
    );

    // 폼의 CSRF 쿠키가 없으면 받지 않는다.
    let page = http(&bind, "GET", "/signup", &[], "");
    assert_eq!(page.status, 200);
    let pre = page.cookie("giwa_csrf").expect("가입 폼 CSRF 쿠키");
    let pre_cookie = format!("giwa_csrf={pre}");
    let signup = form(&[
        ("_csrf", &pre),
        ("email", EMAIL),
        ("display_name", "달빛"),
        ("password", PASSWORD),
        ("password2", PASSWORD),
    ]);
    let res = http(&bind, "POST", "/signup", &[FORM], &signup);
    assert_eq!(res.status, 403);
    assert_eq!(giwa(&data, &["member", "list"])["total"], 1, "운영자만");

    let res = http(
        &bind,
        "POST",
        "/signup",
        &[FORM, ("Cookie", &pre_cookie)],
        &signup,
    );
    assert_eq!(res.status, 200, "{}", res.body);
    assert!(res.body.contains("메일을 확인하세요"));

    // 인증 전에는 로그인하지 못한다.
    let login = http(&bind, "GET", "/login", &[("Cookie", &pre_cookie)], "");
    let res = http(
        &bind,
        "POST",
        "/login",
        &[FORM, ("Cookie", &pre_cookie)],
        &form(&[
            ("_csrf", &login.csrf()),
            ("next", "/me"),
            ("email", EMAIL),
            ("password", PASSWORD),
        ]),
    );
    assert_ne!(res.status, 303, "인증 전 로그인이 됨");
    assert!(res.cookie("giwa_session").is_none());

    // 서버가 편지를 파일로 남긴다 (5초마다).
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        let n = std::fs::read_dir(&mail_dir)
            .map(|d| {
                d.filter(|e| {
                    e.as_ref()
                        .is_ok_and(|e| e.path().extension().is_some_and(|x| x == "eml"))
                })
                .count()
            })
            .unwrap_or(0);
        if n > 0 {
            break;
        }
        assert!(Instant::now() < deadline, "편지 파일이 생기지 않음");
        std::thread::sleep(Duration::from_millis(250));
    }

    // 링크: GET 은 확인 화면만 (메일 미리보기가 소모하지 않게), POST 가 인증한다.
    let verify = link_in_outbox(&data, EMAIL, "/verify/");
    let page = http(&bind, "GET", &verify, &[("Cookie", &pre_cookie)], "");
    assert_eq!(page.status, 200);
    assert!(page.body.contains("달빛"));
    let res = http(
        &bind,
        "POST",
        &verify,
        &[FORM, ("Cookie", &pre_cookie)],
        &form(&[("_csrf", &pre)]),
    );
    assert_eq!(res.status, 303, "{}", res.body);
    assert_eq!(res.header("location").as_deref(), Some("/me?done=verified"));
    let session = format!("giwa_session={}", res.cookie("giwa_session").unwrap());
    let again = http(
        &bind,
        "POST",
        &verify,
        &[FORM, ("Cookie", &pre_cookie)],
        &form(&[("_csrf", &pre)]),
    );
    assert!(again.cookie("giwa_session").is_none(), "링크를 두 번 씀");

    // 내 정보에서 닉네임을 바꾸면 DB 에 남는다.
    let me = http(&bind, "GET", "/me", &[("Cookie", &session)], "");
    assert_eq!(me.status, 200);
    assert!(me.body.contains("달빛"));
    let res = http(
        &bind,
        "POST",
        "/me/profile",
        &[FORM, ("Cookie", &session)],
        &form(&[("_csrf", &pre), ("display_name", "새벽달")]),
    );
    assert_eq!(res.status, 403, "폼 CSRF 값으로 세션 폼을 통과함");
    let res = http(
        &bind,
        "POST",
        "/me/profile",
        &[FORM, ("Cookie", &session)],
        &form(&[("_csrf", &me.csrf()), ("display_name", "새벽달")]),
    );
    assert_eq!(res.status, 303, "{}", res.body);
    let shown = giwa(&data, &["member", "show", EMAIL]);
    assert_eq!(shown["display_name"], "새벽달");
    assert!(shown["email_verified_at"].is_string());
    assert_eq!(shown["roles"], serde_json::json!(["member"]));
    let id = shown["id"].as_str().unwrap().to_owned();

    // 회원 관리: 읽기·쓰기 토큰은 못 보고, 관리 토큰은 CLI 와 같은 답.
    assert_eq!(http(&bind, "GET", "/api/v1/members", &[], "").status, 401);
    assert_eq!(
        http(
            &bind,
            "GET",
            "/api/v1/members",
            &[("Authorization", &writer)],
            ""
        )
        .status,
        403
    );
    let rest = http(
        &bind,
        "GET",
        "/api/v1/members",
        &[("Authorization", &admin)],
        "",
    );
    assert_eq!(rest.status, 200);
    let rest: Value = serde_json::from_str(&rest.body).unwrap();
    let cli = giwa(&data, &["member", "list"]);
    assert_eq!(rest["data"]["total"], cli["total"]);
    assert!(
        rest["data"]["items"]
            .as_array()
            .unwrap()
            .iter()
            .any(|m| m["id"] == id.as_str() && m["display_name"] == "새벽달")
    );

    // 비활성은 세션을 끊는다.
    let res = http(
        &bind,
        "PATCH",
        &format!("/api/v1/members/{id}"),
        &[
            ("Authorization", &admin),
            ("Content-Type", "application/json"),
        ],
        r#"{"status":"disabled"}"#,
    );
    assert_eq!(res.status, 200, "{}", res.body);
    assert_ne!(
        http(&bind, "GET", "/me", &[("Cookie", &session)], "").status,
        200
    );
    let enabled = giwa(&data, &["member", "enable", EMAIL]);
    assert_eq!(enabled["status"], "active");
    let role = giwa(&data, &["member", "role", EMAIL, "editor"]);
    assert_eq!(role["roles"], serde_json::json!(["editor"]));
    let bad = run(&data, &["member", "role", EMAIL, "owner"]);
    assert_eq!(bad["ok"], false);

    // 편지 본문의 링크가 감사 기록에 새지 않는다.
    let token = verify.trim_start_matches("/verify/");
    assert_eq!(
        db_count(
            &data,
            "SELECT count(*) FROM audit_log WHERE instr(diff, ?) > 0",
            token
        ),
        0,
        "감사 기록에 링크 값"
    );
    assert!(
        db_count(
            &data,
            "SELECT count(*) FROM audit_log WHERE action = ?",
            "member.status"
        ) >= 2
    );
}

fn db_count(data: &Path, sql: &'static str, arg: &str) -> i64 {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(async {
        let db = giwa_storage::Db::open(&data.join("giwa.db"), false)
            .await
            .unwrap();
        let v: i64 = sqlx::query_scalar(sql)
            .bind(arg)
            .fetch_one(db.read_pool())
            .await
            .unwrap();
        db.close().await;
        v
    })
}
