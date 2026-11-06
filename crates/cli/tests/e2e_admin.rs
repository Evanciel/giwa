//! U12·U13: 실제 바이너리·실제 포트로 로그인 링크, 세션, CSRF, 관리 화면 글 발행, 요청 제한을 확인한다.
//! E2E-2 (PHASE1_PLAN §8) 의 자동화 가능한 부분. 실제 브라우저 확인은 따로 한다.
#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::fs::File;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::path::Path;
use std::process::{Child, Command, Stdio};

use serde_json::Value;

const BIN: &str = env!("CARGO_BIN_EXE_maru");
const PASSWORD: &str = "시험용-비밀번호-1234";

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
    assert_eq!(v["ok"], true, "{v}");
    v["data"].clone()
}

/// 서버 로그는 파일로 받아 비밀 값이 새지 않는지 나중에 본다.
fn start_server(data: &Path, log: &Path) -> (Server, String) {
    let mut child = Command::new(BIN)
        .arg("--data")
        .arg(data)
        .args(["serve", "--bind", "127.0.0.1:0", "--json"])
        .env("RUST_LOG", "debug")
        .stdout(Stdio::piped())
        .stderr(File::create(log).unwrap())
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
    fn header(&self, name: &str) -> Option<String> {
        let name = name.to_ascii_lowercase();
        self.head.lines().find_map(|l| {
            let (k, v) = l.split_once(':')?;
            (k.trim().to_ascii_lowercase() == name).then(|| v.trim().to_owned())
        })
    }

    fn cookies(&self) -> Vec<String> {
        self.head
            .lines()
            .filter_map(|l| {
                let (k, v) = l.split_once(':')?;
                k.trim()
                    .eq_ignore_ascii_case("set-cookie")
                    .then(|| v.trim().to_owned())
            })
            .collect()
    }

    /// `Set-Cookie` 에서 이름에 맞는 값.
    fn cookie(&self, name: &str) -> Option<String> {
        self.cookies().into_iter().find_map(|c| {
            let first = c.split(';').next()?.to_owned();
            let (k, v) = first.split_once('=')?;
            (k == name).then(|| v.to_owned())
        })
    }

    fn location(&self) -> String {
        self.header("location").unwrap_or_default()
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

const FORM: (&str, &str) = ("Content-Type", "application/x-www-form-urlencoded");

fn path_of(url: &str) -> String {
    let rest = url.split_once("://").unwrap().1;
    rest[rest.find('/').unwrap()..].to_owned()
}

/// 로그인 링크를 열고 확인 버튼까지 눌러 세션 쿠키를 받는다.
fn sign_in_with_link(bind: &str, link_path: &str) -> String {
    let page = http(bind, "GET", link_path, &[], "");
    assert_eq!(page.status, 200, "{}", page.body);
    let pre = page.cookie("maru_csrf").unwrap();
    let cookie = format!("maru_csrf={pre}");
    let res = http(
        bind,
        "POST",
        link_path,
        &[FORM, ("Cookie", &cookie)],
        &form(&[("_csrf", &pre)]),
    );
    assert_eq!(res.status, 303, "{}", res.body);
    res.cookie("maru_session").unwrap()
}

fn db_scalar(data: &Path, sql: &'static str) -> i64 {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(async {
        let db = maru_storage::Db::open(&data.join("maru.db"), false)
            .await
            .unwrap();
        let v: i64 = sqlx::query_scalar(sql)
            .fetch_one(db.read_pool())
            .await
            .unwrap();
        db.close().await;
        v
    })
}

#[test]
fn e2e2_link_password_post_logout_login() {
    let tmp = tempfile::tempdir().unwrap();
    let data = tmp.path().join("사이트");
    let log = tmp.path().join("server.log");

    // init 이 운영자와 로그인 링크를 낸다. 다시 실행하면 새로 내지 않는다.
    let init = maru(&data, &["init", "--preset", "blog", "--yes"]);
    assert_eq!(init["owner"]["email"], "owner@localhost");
    assert_eq!(init["owner"]["created"], true);
    let first_link = init["login_link"]["url"].as_str().unwrap().to_owned();
    let again = maru(&data, &["init", "--preset", "blog", "--yes"]);
    assert!(again["login_link"].is_null(), "{again}");
    assert!(again["owner"].is_null(), "{again}");

    // 새 링크를 받으면 이전 링크는 못 쓴다.
    let link = maru(&data, &["login-link"]);
    assert_eq!(link["email"], "owner@localhost");
    let link_path = path_of(link["url"].as_str().unwrap());

    let (_server, bind) = start_server(&data, &log);

    // 비로그인은 로그인 화면으로.
    let res = http(&bind, "GET", "/admin", &[], "");
    assert_eq!(res.status, 303);
    assert_eq!(res.location(), "/admin/login?next=/admin");
    let res = http(&bind, "GET", "/admin/posts/new", &[], "");
    assert_eq!(res.status, 303);

    let old = http(&bind, "GET", &path_of(&first_link), &[], "");
    assert_eq!(old.status, 401);
    assert!(old.body.contains("maru login-link"));

    // 링크 확인 화면: 헤더와 CSRF 쿠키.
    let page = http(&bind, "GET", &link_path, &[], "");
    assert_eq!(page.status, 200);
    assert!(page.body.contains("owner@localhost"));
    assert_eq!(
        page.header("referrer-policy").as_deref(),
        Some("no-referrer")
    );
    assert_eq!(page.header("cache-control").as_deref(), Some("no-store"));
    assert!(
        page.header("content-security-policy")
            .unwrap()
            .contains("frame-ancestors 'none'")
    );
    let pre = page.cookie("maru_csrf").unwrap();
    let pre_cookie = format!("maru_csrf={pre}");

    // CSRF 없이, 엉뚱한 값으로는 거부. 링크는 그대로 살아 있다.
    let res = http(
        &bind,
        "POST",
        &link_path,
        &[FORM],
        &form(&[("_csrf", &pre)]),
    );
    assert_eq!(res.status, 403);
    let other = "0".repeat(64);
    let res = http(
        &bind,
        "POST",
        &link_path,
        &[FORM, ("Cookie", &pre_cookie)],
        &form(&[("_csrf", &other)]),
    );
    assert_eq!(res.status, 403);

    // 사용 → 비밀번호 화면으로, 세션 쿠키 속성 확인.
    let res = http(
        &bind,
        "POST",
        &link_path,
        &[FORM, ("Cookie", &pre_cookie)],
        &form(&[("_csrf", &pre)]),
    );
    assert_eq!(res.status, 303, "{}", res.body);
    assert_eq!(res.location(), "/admin/password");
    let set = res
        .cookies()
        .into_iter()
        .find(|c| c.starts_with("maru_session="))
        .unwrap();
    for attr in ["HttpOnly", "SameSite=Lax", "Path=/", "Max-Age=1209600"] {
        assert!(set.contains(attr), "{set}");
    }
    assert!(
        !set.contains("Secure"),
        "http 에서는 Secure 를 붙이지 않는다: {set}"
    );
    let link_session = res.cookie("maru_session").unwrap();

    // 재사용 거부.
    let res = http(
        &bind,
        "POST",
        &link_path,
        &[FORM, ("Cookie", &pre_cookie)],
        &form(&[("_csrf", &pre)]),
    );
    assert_eq!(res.status, 401);

    // 비밀번호 정하기.
    let s1 = format!("maru_session={link_session}");
    let page = http(&bind, "GET", "/admin/password", &[("Cookie", &s1)], "");
    assert_eq!(page.status, 200);
    let csrf1 = page.csrf();
    let res = http(
        &bind,
        "POST",
        "/admin/password",
        &[FORM, ("Cookie", &s1)],
        &form(&[
            ("_csrf", &csrf1),
            ("password", PASSWORD),
            ("confirm", "다른 값-1234567"),
        ]),
    );
    assert_eq!(res.status, 400);
    let res = http(
        &bind,
        "POST",
        "/admin/password",
        &[FORM, ("Cookie", &s1)],
        &form(&[
            ("_csrf", &csrf1),
            ("password", PASSWORD),
            ("confirm", PASSWORD),
        ]),
    );
    assert_eq!(res.status, 303, "{}", res.body);
    let session = res.cookie("maru_session").unwrap();
    // 비밀번호를 바꾸면 이전 세션은 끊긴다.
    assert_eq!(
        http(&bind, "GET", "/admin", &[("Cookie", &s1)], "").status,
        303
    );

    let s2 = format!("maru_session={session}");
    let home = http(&bind, "GET", "/admin?done=password", &[("Cookie", &s2)], "");
    assert_eq!(home.status, 200);
    assert!(home.body.contains("비밀번호를 저장했습니다"));
    let csrf2 = home.csrf();

    // 두 번째 세션 (다른 기기). 그 CSRF 값은 첫 세션에서 통하지 않는다.
    let l2 = maru(&data, &["login-link"]);
    let other_session = sign_in_with_link(&bind, &path_of(l2["url"].as_str().unwrap()));
    let s3 = format!("maru_session={other_session}");
    let csrf3 = http(&bind, "GET", "/admin", &[("Cookie", &s3)], "").csrf();
    assert_ne!(csrf2, csrf3);

    let post_form = |csrf: &str| {
        form(&[
            ("_csrf", csrf),
            ("board", "blog"),
            ("title", "관리 화면에서 쓴 글"),
            // 브라우저는 textarea 줄바꿈을 CRLF 로 보낸다.
            ("body_md", "## 소제목\r\n\r\n브라우저에서 발행합니다."),
            ("intent", "publish"),
        ])
    };
    let res = http(
        &bind,
        "POST",
        "/admin/posts/new",
        &[FORM, ("Cookie", &s2)],
        &post_form(""),
    );
    assert_eq!(res.status, 403);
    let res = http(
        &bind,
        "POST",
        "/admin/posts/new",
        &[FORM, ("Cookie", &s2)],
        &post_form(&csrf3),
    );
    assert_eq!(res.status, 403);
    assert_eq!(
        db_scalar(
            &data,
            "SELECT count(*) FROM posts WHERE title = '관리 화면에서 쓴 글'"
        ),
        0
    );

    // 작성 + 발행.
    let res = http(
        &bind,
        "POST",
        "/admin/posts/new",
        &[FORM, ("Cookie", &s2)],
        &post_form(&csrf2),
    );
    assert_eq!(res.status, 303, "{}", res.body);
    let loc = res.location();
    assert!(loc.ends_with("?done=published"), "{loc}");
    let edit = http(&bind, "GET", &loc, &[("Cookie", &s2)], "");
    assert_eq!(edit.status, 200);
    assert!(edit.body.contains("발행했습니다"));
    // 저장할 때 줄바꿈을 LF 하나로 맞춘다 (CLI·REST 로 쓴 글과 같은 모양).
    assert_eq!(
        db_scalar(
            &data,
            "SELECT count(*) FROM posts WHERE title = '관리 화면에서 쓴 글' \
             AND body_md = '## 소제목' || char(10) || char(10) || '브라우저에서 발행합니다.'"
        ),
        1
    );

    // 글 에디터: textarea 는 그대로 두고(스크립트 없이도 저장된다) 번들을 붙인다.
    assert!(edit.body.contains("data-editor>"), "{}", edit.body);
    assert!(
        edit.body
            .contains("<script src=\"/admin/assets/editor.js\" defer></script>")
    );
    let csp = edit.header("content-security-policy").unwrap_or_default();
    assert!(csp.contains("script-src 'self'"), "{csp}");
    assert!(csp.contains("connect-src 'self'"), "{csp}");
    assert!(!csp.contains("unsafe-inline"), "{csp}");
    for (asset, mime) in [
        ("editor.js", "javascript"),
        ("editor.css", "text/css"),
        ("editor.licenses.txt", "text/plain"),
    ] {
        let res = http(&bind, "GET", &format!("/admin/assets/{asset}"), &[], "");
        assert_eq!(res.status, 200, "{asset}");
        let ct = res.header("content-type").unwrap_or_default();
        assert!(ct.contains(mime), "{asset}: {ct}");
    }
    let public = edit
        .body
        .split("href=\"")
        .find(|s| s.starts_with("/b/blog/"))
        .map(|s| s[..s.find('"').unwrap()].to_owned())
        .expect("공개 페이지 링크");
    let page = http(&bind, "GET", &public, &[], "");
    assert_eq!(page.status, 200);
    assert!(page.body.contains("브라우저에서 발행합니다."));
    assert_eq!(
        db_scalar(
            &data,
            "SELECT count(*) FROM audit_log a JOIN posts p ON p.id = a.target_id \
             WHERE p.title = '관리 화면에서 쓴 글' AND a.actor_type = 'human' AND a.action = 'post.publish'"
        ),
        1
    );

    // 로그아웃 → 쿠키 삭제, 세션 행 삭제, 같은 쿠키 거부.
    let sessions = db_scalar(&data, "SELECT count(*) FROM sessions");
    let res = http(
        &bind,
        "POST",
        "/admin/logout",
        &[FORM, ("Cookie", &s2)],
        &form(&[("_csrf", &csrf2)]),
    );
    assert_eq!(res.status, 303);
    assert!(
        res.cookies()
            .iter()
            .any(|c| c.starts_with("maru_session=;") && c.contains("Max-Age=0"))
    );
    assert_eq!(
        db_scalar(&data, "SELECT count(*) FROM sessions"),
        sessions - 1
    );
    assert_eq!(
        http(&bind, "GET", "/admin", &[("Cookie", &s2)], "").status,
        303
    );
    // 다른 기기는 영향 없음.
    assert_eq!(
        http(&bind, "GET", "/admin", &[("Cookie", &s3)], "").status,
        200
    );

    // 비밀번호로 다시 로그인. 틀린 비밀번호와 없는 계정은 같은 응답.
    let page = http(&bind, "GET", "/admin/login?next=/admin/posts/new", &[], "");
    let pre = page.cookie("maru_csrf").unwrap();
    let pc = format!("maru_csrf={pre}");
    let attempt = |email: &str, pw: &str, next: &str| {
        http(
            &bind,
            "POST",
            "/admin/login",
            &[FORM, ("Cookie", &pc)],
            &form(&[
                ("_csrf", &pre),
                ("email", email),
                ("password", pw),
                ("next", next),
            ]),
        )
    };
    let wrong = attempt("owner@localhost", "틀린-비밀번호-0000", "/admin");
    let ghost = attempt("ghost@localhost", PASSWORD, "/admin");
    assert_eq!(wrong.status, 401);
    assert_eq!(ghost.status, 401);
    let msg = |b: &str| {
        let at = b.find("role=\"alert\">").unwrap();
        b[at..at + b[at..].find("</p>").unwrap()].to_owned()
    };
    assert_eq!(msg(&wrong.body), msg(&ghost.body));
    let ok = attempt("owner@localhost", PASSWORD, "/admin/posts/new");
    assert_eq!(ok.status, 303);
    assert_eq!(ok.location(), "/admin/posts/new");
    // 외부 next 는 /admin 으로.
    let ok = attempt("owner@localhost", PASSWORD, "//evil.test/x");
    assert_eq!(ok.location(), "/admin");

    // 서버 로그에 비밀 값이 없다.
    let logs = std::fs::read_to_string(&log).unwrap();
    for secret in [
        PASSWORD,
        link_session.as_str(),
        session.as_str(),
        link_path.trim_start_matches("/admin/link/"),
        csrf2.as_str(),
    ] {
        assert!(!logs.contains(secret), "로그에 비밀 값: {secret}");
    }
}

#[test]
fn login_attempts_are_rate_limited_per_connection_address() {
    let tmp = tempfile::tempdir().unwrap();
    let data = tmp.path().join("site");
    maru(&data, &["init", "--yes"]);
    let (_server, bind) = start_server(&data, &tmp.path().join("server.log"));
    let page = http(&bind, "GET", "/admin/login", &[], "");
    let pre = page.cookie("maru_csrf").unwrap();
    let pc = format!("maru_csrf={pre}");
    let body = form(&[
        ("_csrf", &pre),
        ("email", "owner@localhost"),
        ("password", "틀린-비밀번호-0000"),
    ]);
    // 분 단위 고정 창이라 도중에 분이 바뀌면 한 번 초기화된다. 그래도 21번 안에는 반드시 걸린다.
    let mut statuses = Vec::new();
    for i in 0..21 {
        // 전달 헤더를 바꿔도 연결 주소로 센다.
        let fwd = format!("203.0.113.{i}");
        let res = http(
            &bind,
            "POST",
            "/admin/login",
            &[FORM, ("Cookie", &pc), ("X-Forwarded-For", &fwd)],
            &body,
        );
        if res.status == 429 {
            assert_eq!(res.header("retry-after").as_deref(), Some("60"));
        }
        statuses.push(res.status);
        if res.status == 429 {
            break;
        }
    }
    let first_limited = statuses
        .iter()
        .position(|s| *s == 429)
        .unwrap_or_else(|| panic!("제한에 걸리지 않음: {statuses:?}"));
    assert!((10..=20).contains(&first_limited), "{statuses:?}");
    assert!(
        statuses[..first_limited].iter().all(|s| *s == 401),
        "{statuses:?}"
    );
    // 공개 화면은 별도 한도라 계속 열린다.
    assert_eq!(http(&bind, "GET", "/", &[], "").status, 200);
}

#[test]
fn login_link_points_at_the_running_server() {
    let tmp = tempfile::tempdir().unwrap();
    let data = tmp.path().join("site");
    let log = tmp.path().join("server.log");
    maru(&data, &["init", "--yes"]);

    let off = maru(&data, &["login-link"]);
    assert!(
        off["url"]
            .as_str()
            .unwrap()
            .starts_with("http://127.0.0.1:8080/"),
        "{off}"
    );

    {
        // 설정(8080)과 다른 포트에서 돌면 그 주소를 찍고, 그 링크로 실제로 들어간다.
        let (_server, bind) = start_server(&data, &log);
        let on = maru(&data, &["login-link"]);
        let url = on["url"].as_str().unwrap();
        assert!(url.starts_with(&format!("http://{bind}/")), "{on} {bind}");
        let cookie = format!("maru_session={}", sign_in_with_link(&bind, &path_of(url)));
        assert_eq!(
            http(&bind, "GET", "/admin", &[("Cookie", &cookie)], "").status,
            200
        );
    }

    // 강제 종료로 기록만 남아도 옛 주소를 믿지 않는다.
    let stale = data.join("server.json");
    assert!(stale.exists());
    let after = maru(&data, &["login-link"]);
    assert!(
        after["url"]
            .as_str()
            .unwrap()
            .starts_with("http://127.0.0.1:8080/"),
        "{after}"
    );
}

#[test]
fn agent_token_cannot_issue_login_link() {
    let tmp = tempfile::tempdir().unwrap();
    let data = tmp.path().join("site");
    let init = maru(&data, &["init", "--yes"]);
    let secret = init["token"]["secret"].as_str().unwrap();
    let out = Command::new(BIN)
        .arg("--data")
        .arg(&data)
        .args(["login-link", "--json"])
        .env("MARU_TOKEN", secret)
        .output()
        .unwrap();
    let v: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v["ok"], false);
    assert_eq!(v["error"]["code"], "FORBIDDEN");
}

/// 발행 서랍의 분류·태그·공개 범위·공지가 서비스를 거쳐 DB 에 남고, 편집 화면이 그 값을 다시 보여 준다.
/// 칸이 없는 옛 폼으로 저장해도 값이 지워지지 않는다.
#[test]
fn publish_drawer_fields_reach_the_db() {
    let tmp = tempfile::tempdir().unwrap();
    let data = tmp.path().join("site");
    maru(&data, &["init", "--preset", "blog", "--yes"]);
    maru(&data, &["category", "add", "--board", "blog", "여행"]);
    let (_server, bind) = start_server(&data, &tmp.path().join("serve.log"));
    let link = maru(&data, &["login-link"]);
    let session = sign_in_with_link(&bind, &path_of(link["url"].as_str().unwrap()));
    let cookie = format!("maru_session={session}");

    let page = http(&bind, "GET", "/admin/posts/new", &[("Cookie", &cookie)], "");
    assert_eq!(page.status, 200);
    assert!(page.body.contains("name=\"visibility\""));
    assert!(
        page.body.contains("name=\"notice_shown\""),
        "운영자에게는 공지 칸"
    );
    assert!(page.body.contains("data-board=\"blog\""), "분류 선택지");
    let csrf = page.csrf();

    let res = http(
        &bind,
        "POST",
        "/admin/posts/new",
        &[FORM, ("Cookie", &cookie)],
        &form(&[
            ("_csrf", &csrf),
            ("board", "blog"),
            ("title", "서랍에서 쓴 글"),
            ("body_md", "본문입니다."),
            ("category", "여행"),
            ("tags", "#제주, 바다, 제주"),
            ("notice_shown", "1"),
            ("notice", "1"),
            ("visibility", "secret"),
            ("intent", "publish"),
        ]),
    );
    assert_eq!(res.status, 303, "{}", res.body);
    assert!(
        res.location().ends_with("?done=published"),
        "{}",
        res.location()
    );
    let edit_path = res.location().split('?').next().unwrap().to_owned();
    let id = edit_path.rsplit('/').next().unwrap().to_owned();
    let p = maru(&data, &["post", "show", &id]);
    assert_eq!(p["status"], "published");
    assert!(p["category_id"].is_string(), "{p}");
    assert_eq!(p["tags"], serde_json::json!(["제주", "바다"]));
    assert_eq!(
        (p["is_notice"].clone(), p["is_secret"].clone()),
        (true.into(), true.into())
    );

    // 편집 화면이 값을 다시 보여 준다
    let edit = http(&bind, "GET", &edit_path, &[("Cookie", &cookie)], "");
    assert!(edit.body.contains("value=\"제주, 바다\""), "태그 값");
    assert!(edit.body.contains("value=\"secret\" checked"), "공개 범위");
    assert!(
        edit.body.contains("name=\"notice\" value=\"1\" checked"),
        "공지"
    );
    assert!(
        edit.body.contains("data-board=\"blog\" selected>여행"),
        "분류"
    );

    // 칸이 없는 폼(옛 화면)으로 저장해도 지워지지 않는다
    let res = http(
        &bind,
        "POST",
        &edit_path,
        &[FORM, ("Cookie", &cookie)],
        &form(&[
            ("_csrf", &csrf),
            ("title", "서랍에서 쓴 글 (고침)"),
            ("body_md", "본문입니다."),
            ("intent", "draft"),
        ]),
    );
    assert_eq!(res.status, 303, "{}", res.body);
    let kept = maru(&data, &["post", "show", &id]);
    assert_eq!(kept["tags"], p["tags"]);
    assert_eq!(kept["category_id"], p["category_id"]);
    assert_eq!(
        (kept["is_notice"].clone(), kept["is_secret"].clone()),
        (true.into(), true.into())
    );

    // 비공개 = 숨김
    let res = http(
        &bind,
        "POST",
        &edit_path,
        &[FORM, ("Cookie", &cookie)],
        &form(&[
            ("_csrf", &csrf),
            ("title", "서랍에서 쓴 글 (고침)"),
            ("body_md", "본문입니다."),
            ("visibility", "private"),
            ("intent", "draft"),
        ]),
    );
    assert!(
        res.location().ends_with("?done=hidden"),
        "{}",
        res.location()
    );
    assert_eq!(maru(&data, &["post", "show", &id])["status"], "hidden");

    // 서랍의 「새 분류」: CSRF 머리가 있어야 만든다
    let body = r#"{"board":"blog","name":"맛집"}"#;
    let json = ("Content-Type", "application/json");
    let res = http(
        &bind,
        "POST",
        "/admin/categories",
        &[json, ("Cookie", &cookie)],
        body,
    );
    assert_eq!(res.status, 403, "{}", res.body);
    let res = http(
        &bind,
        "POST",
        "/admin/categories",
        &[json, ("Cookie", &cookie), ("X-CSRF-Token", &csrf)],
        body,
    );
    assert_eq!(res.status, 201, "{}", res.body);
    let made: Value = serde_json::from_str(&res.body).unwrap();
    assert_eq!(made["data"]["slug"], "맛집");
}
