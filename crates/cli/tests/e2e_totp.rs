//! GAP M16: 2단계 인증 (ADR-0072). 실제 바이너리·실제 포트·실제 DB 파일로
//! 내 정보에서 켜기 → 비밀번호 로그인이 코드 확인 화면으로 가는지, 틀린 코드·CSRF 없는 요청을 막는지,
//! 맞는 코드·복구 코드로 들어가고 그 결과가 DB 에 남는지, 운영자가 관리 화면·CLI 로 끌 수 있는지 본다.
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

fn now() -> i64 {
    time::OffsetDateTime::now_utc().unix_timestamp()
}

/// `<p class="totp-secret"><code>ABCD EFGH …</code>` 의 비밀 (공백 뺌).
fn secret_on(page: &str) -> String {
    let at = page
        .find("class=\"totp-secret\"><code>")
        .expect("설정 화면의 비밀")
        + "class=\"totp-secret\"><code>".len();
    let rest = &page[at..];
    rest[..rest.find('<').unwrap()].replace(' ', "")
}

/// `<ol class="code-list">` 의 복구 코드들.
fn codes_on(page: &str) -> Vec<String> {
    let at = page.find("class=\"code-list\"").expect("복구 코드 목록");
    let list = &page[at..page[at..].find("</ol>").unwrap() + at];
    list.split("<code>")
        .skip(1)
        .map(|c| c[..c.find('<').unwrap()].to_owned())
        .collect()
}

fn login_password(bind: &str, path: &str, next: &str) -> Res {
    let page = http(
        bind,
        "GET",
        if path == "/admin/login" {
            "/admin/login"
        } else {
            "/login"
        },
        &[],
        "",
    );
    let pre = page.cookie("maru_csrf").expect("로그인 폼 CSRF 쿠키");
    http(
        bind,
        "POST",
        path,
        &[FORM, ("Cookie", &format!("maru_csrf={pre}"))],
        &form(&[
            ("_csrf", &pre),
            ("next", next),
            ("email", EMAIL),
            ("password", PASSWORD),
        ]),
    )
}

#[test]
fn totp_setup_login_recovery_and_reset() {
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
    let (server, started) = start_server(&data);
    let bind = started["bind"].as_str().unwrap().to_owned();

    // 가입하고 로그인한다 (2단계 인증 전에는 바로 세션).
    let page = http(&bind, "GET", "/signup", &[], "");
    let pre = page.cookie("maru_csrf").unwrap();
    let pre_cookie = format!("maru_csrf={pre}");
    let res = http(
        &bind,
        "POST",
        "/signup",
        &[FORM, ("Cookie", &pre_cookie)],
        &form(&[
            ("_csrf", &pre),
            ("email", EMAIL),
            ("display_name", "달빛"),
            ("password", PASSWORD),
            ("password2", PASSWORD),
        ]),
    );
    assert_eq!(res.status, 200, "{}", res.body);
    let res = login_password(&bind, "/login", "/me");
    assert_eq!(res.status, 303, "{}", res.body);
    let old = format!("maru_session={}", res.cookie("maru_session").unwrap());

    // 내 정보 → 켜기 시작 → 비밀이 보인다.
    let me = http(&bind, "GET", "/me", &[("Cookie", &old)], "");
    assert!(me.body.contains("2단계 인증 켜기"), "{}", me.body);
    let csrf = me.csrf();
    let res = http(
        &bind,
        "POST",
        "/me/totp/start",
        &[FORM, ("Cookie", &old)],
        &form(&[("_csrf", &csrf)]),
    );
    assert_eq!(res.status, 303, "{}", res.body);
    assert_eq!(
        res.header("location").as_deref(),
        Some("/me?done=totp_start#me-totp")
    );
    let me = http(&bind, "GET", "/me?done=totp_start", &[("Cookie", &old)], "");
    let secret = secret_on(&me.body);
    assert_eq!(secret.len(), 32);
    assert!(me.body.contains("otpauth://totp/"));
    // QR 그림은 서버가 SVG 로 그린다(외부 QR 서비스 없음). 비밀 키는 그림 글자로 새지 않는다.
    let qr_at = me.body.find("class=\"totp-qr\"").expect("QR 그림");
    let qr = &me.body[qr_at
        ..me.body[qr_at..]
            .find("</div>")
            .map_or(me.body.len(), |e| qr_at + e)];
    assert!(qr.contains("<svg aria-hidden=\"true\""), "{qr}");
    assert!(!qr.contains("<?xml") && !qr.contains(&secret), "{qr}");

    // 틀린 코드는 켜지지 않는다. 맞는 코드면 복구 코드가 한 번 보이고 세션이 바뀐다.
    let res = http(
        &bind,
        "POST",
        "/me/totp/enable",
        &[FORM, ("Cookie", &old)],
        &form(&[("_csrf", &csrf), ("code", "000000")]),
    );
    assert_eq!(res.status, 400, "{}", res.body);
    assert!(res.body.contains("form-error"));
    assert_eq!(maru(&data, &["member", "show", EMAIL])["two_factor"], false);
    let at = now();
    let code = maru_core::services::totp::code_at(&secret, at).unwrap();
    let res = http(
        &bind,
        "POST",
        "/me/totp/enable",
        &[FORM, ("Cookie", &old)],
        &form(&[("_csrf", &csrf), ("code", &code)]),
    );
    assert_eq!(res.status, 200, "{}", res.body);
    let codes = codes_on(&res.body);
    assert_eq!(codes.len(), 10, "{codes:?}");
    let fresh = format!("maru_session={}", res.cookie("maru_session").unwrap());
    assert_eq!(maru(&data, &["member", "show", EMAIL])["two_factor"], true);
    // 예전 세션은 끊겼다.
    assert_eq!(
        http(&bind, "GET", "/me", &[("Cookie", &old)], "").status,
        303
    );
    let me = http(&bind, "GET", "/me", &[("Cookie", &fresh)], "");
    assert!(me.body.contains("남은 복구 코드 10개"), "{}", me.body);
    assert!(!me.body.contains("code-list"), "복구 코드가 다시 보임");

    // 비밀번호만으로는 세션이 생기지 않고 코드 확인 화면으로 간다.
    let res = login_password(&bind, "/login", "/me");
    assert_eq!(res.status, 303, "{}", res.body);
    assert_eq!(
        res.header("location").as_deref(),
        Some("/login/totp?next=/me")
    );
    assert!(res.cookie("maru_session").is_none());
    let challenge = format!("maru_totp={}", res.cookie("maru_totp").unwrap());
    // 확인 단계 값으로는 내 정보를 볼 수 없다.
    assert_eq!(
        http(&bind, "GET", "/me", &[("Cookie", &challenge)], "").status,
        303
    );
    assert_eq!(http(&bind, "GET", "/login/totp", &[], "").status, 401);
    let page = http(
        &bind,
        "GET",
        "/login/totp?next=/me",
        &[("Cookie", &challenge)],
        "",
    );
    assert_eq!(page.status, 200);
    assert!(page.body.contains("one-time-code"));
    let pre = page.cookie("maru_csrf").unwrap();
    let both = format!("{challenge}; maru_csrf={pre}");
    let res = http(
        &bind,
        "POST",
        "/login/totp",
        &[FORM, ("Cookie", &challenge)],
        &form(&[("_csrf", &pre), ("code", "000000"), ("next", "/me")]),
    );
    assert_eq!(res.status, 403, "CSRF 쿠키 없이 통과함");
    let res = http(
        &bind,
        "POST",
        "/login/totp",
        &[FORM, ("Cookie", &both)],
        &form(&[("_csrf", &pre), ("code", "000000"), ("next", "/me")]),
    );
    assert_eq!(res.status, 401, "{}", res.body);
    assert!(res.body.contains("4번 더"), "{}", res.body);
    let next = maru_core::services::totp::code_at(&secret, at + 30).unwrap();
    let res = http(
        &bind,
        "POST",
        "/login/totp",
        &[FORM, ("Cookie", &both)],
        &form(&[("_csrf", &pre), ("code", &next), ("next", "/me")]),
    );
    assert_eq!(res.status, 303, "{}", res.body);
    assert_eq!(res.header("location").as_deref(), Some("/me"));
    let session = format!("maru_session={}", res.cookie("maru_session").unwrap());
    assert!(
        res.head.contains("maru_totp=;"),
        "확인 단계 쿠키를 지우지 않음"
    );
    assert_eq!(
        http(&bind, "GET", "/me", &[("Cookie", &session)], "").status,
        200
    );
    drop(server); // 로그인 시도 제한(분당 10번)을 비운다

    // 관리 화면 로그인도 같은 확인을 거치고, 복구 코드로 들어갈 수 있다.
    let (_server, started) = start_server(&data);
    let bind = started["bind"].as_str().unwrap().to_owned();
    let res = login_password(&bind, "/admin/login", "/admin");
    assert_eq!(
        res.header("location").as_deref(),
        Some("/login/totp?next=/admin"),
        "{}",
        res.body
    );
    let challenge = format!("maru_totp={}", res.cookie("maru_totp").unwrap());
    let page = http(
        &bind,
        "GET",
        "/login/totp?next=/admin",
        &[("Cookie", &challenge)],
        "",
    );
    let pre = page.cookie("maru_csrf").unwrap();
    let res = http(
        &bind,
        "POST",
        "/login/totp",
        &[FORM, ("Cookie", &format!("{challenge}; maru_csrf={pre}"))],
        &form(&[
            ("_csrf", &pre),
            ("code", &codes[0].to_uppercase()),
            ("next", "/admin"),
        ]),
    );
    assert_eq!(res.status, 303, "{}", res.body);
    assert_eq!(res.header("location").as_deref(), Some("/admin"));
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
    assert!(me.body.contains("남은 복구 코드 9개"), "{}", me.body);

    // 운영자는 관리 화면에서 회원의 2단계 인증을 끌 수 있다.
    let link = maru(&data, &["login-link", "--email", "owner@localhost"]);
    let url = link["url"].as_str().unwrap();
    let path = &url[url.find("/admin/link/").unwrap()..];
    let page = http(&bind, "GET", path, &[], "");
    let pre = page.cookie("maru_csrf").unwrap();
    let res = http(
        &bind,
        "POST",
        path,
        &[FORM, ("Cookie", &format!("maru_csrf={pre}"))],
        &form(&[("_csrf", &page.csrf())]),
    );
    let owner = format!(
        "maru_session={}",
        res.cookie("maru_session").expect("운영자 세션")
    );
    let list = http(&bind, "GET", "/admin/members", &[("Cookie", &owner)], "");
    assert!(list.body.contains("2단계 끄기"), "{}", list.body);
    let id = maru(&data, &["member", "show", EMAIL])["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let res = http(
        &bind,
        "POST",
        &format!("/admin/members/{id}/totp_reset"),
        &[FORM, ("Cookie", &owner)],
        &form(&[("_csrf", &list.csrf())]),
    );
    assert_eq!(res.status, 303, "{}", res.body);
    assert!(
        res.header("location")
            .is_some_and(|l| l.contains("done=totp_reset"))
    );
    assert_eq!(maru(&data, &["member", "show", EMAIL])["two_factor"], false);
    let res = login_password(&bind, "/login", "/me");
    assert!(
        res.cookie("maru_session").is_some(),
        "끈 뒤에도 코드를 물음"
    );

    // CLI 로도 끈다. 꺼져 있으면 바꾼 것이 없다고 답한다.
    let out = maru(&data, &["member", "totp-reset", EMAIL]);
    assert_eq!(out["reset"], false, "{out}");
    let bad = run(&data, &["member", "totp-reset", "ghost@example.test"]);
    assert_eq!(bad["ok"], false);
}
