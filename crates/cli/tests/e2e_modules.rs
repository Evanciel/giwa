//! 모듈 시스템 (SPEC Phase 2 완료 기준, ADR-0066). 실제 바이너리·실제 포트·실제 DB 파일로
//! 켜기 → 서버가 저절로 배달 → 위젯·화면 → 끄기(404, 위젯 빠짐) → 제거(코어 그대로)를 확인한다.
#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::thread::sleep;
use std::time::{Duration, Instant};

use serde_json::{Value, json};

const BIN: &str = env!("CARGO_BIN_EXE_maru");
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
        .expect("maru 실행");
    let stdout = String::from_utf8(out.stdout).expect("UTF-8 출력");
    serde_json::from_str(stdout.trim()).unwrap_or_else(|e| panic!("JSON 아님: {e}\n{stdout}"))
}

fn maru(data: &Path, args: &[&str]) -> Value {
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

fn http(
    bind: &str,
    method: &str,
    path: &str,
    headers: &[(&str, &str)],
    body: &str,
) -> (u16, String) {
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
    let status = text[9..12].parse().unwrap();
    let body = text
        .split_once("\r\n\r\n")
        .map_or("", |(_, b)| b)
        .to_owned();
    (status, body)
}

fn get(bind: &str, path: &str) -> (u16, String) {
    http(bind, "GET", path, &[], "")
}

/// 머리글까지 함께 받는다 (쿠키를 읽으려고).
fn http_full(
    bind: &str,
    method: &str,
    path: &str,
    headers: &[(&str, &str)],
    body: &str,
) -> (u16, String, String) {
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
    (
        head[9..12].parse().unwrap(),
        head.to_owned(),
        body.to_owned(),
    )
}

fn cookie_of(head: &str, name: &str) -> Option<String> {
    head.lines().find_map(|l| {
        let (k, v) = l.split_once(':')?;
        if !k.trim().eq_ignore_ascii_case("set-cookie") {
            return None;
        }
        let (n, val) = v.trim().split(';').next()?.split_once('=')?;
        (n == name).then(|| val.to_owned())
    })
}

fn csrf_of(body: &str) -> String {
    let at = body.find("name=\"_csrf\" value=\"").unwrap() + 20;
    body[at..at + body[at..].find('"').unwrap()].to_owned()
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
fn signup_and_login(bind: &str, email: &str, name: &str) -> String {
    let (_, head, _) = http_full(bind, "GET", "/signup", &[], "");
    let pre = cookie_of(&head, "maru_csrf").unwrap();
    let cookie = format!("maru_csrf={pre}");
    let (_, head, _) = http_full(
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
    if let Some(s) = cookie_of(&head, "maru_session") {
        return s;
    }
    let (_, _, page) = http_full(bind, "GET", "/login", &[("Cookie", &cookie)], "");
    let (status, head, body) = http_full(
        bind,
        "POST",
        "/login",
        &[FORM, ("Cookie", &cookie)],
        &form(&[
            ("_csrf", &csrf_of(&page)),
            ("next", "/"),
            ("email", email),
            ("password", PASSWORD),
        ]),
    );
    cookie_of(&head, "maru_session").unwrap_or_else(|| panic!("로그인 실패 {status}: {body}"))
}

/// 공개 글쓰기 화면으로 글을 쓴다.
fn write_post(bind: &str, session: &str, title: &str) {
    let cookie = format!("maru_session={session}");
    let (status, _, page) = http_full(bind, "GET", "/write/free", &[("Cookie", &cookie)], "");
    assert_eq!(status, 200, "{page}");
    let (status, _, body) = http_full(
        bind,
        "POST",
        "/write/free",
        &[FORM, ("Cookie", &cookie)],
        &form(&[
            ("_csrf", &csrf_of(&page)),
            ("title", title),
            ("body", "본문입니다."),
        ]),
    );
    assert_eq!(status, 303, "{body}");
}

/// 조건이 맞을 때까지 잠깐 기다린다 (서버의 배달 주기는 2초).
fn wait_until(what: &str, mut check: impl FnMut() -> bool) {
    let start = Instant::now();
    while start.elapsed() < Duration::from_secs(15) {
        if check() {
            return;
        }
        sleep(Duration::from_millis(200));
    }
    panic!("기다렸지만 안 됐다: {what}");
}

#[test]
fn point_module_lifecycle_through_cli_rest_and_screens() {
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
    let email = "moon@example.test";

    // 기본은 꺼져 있고 설치도 안 됐다. 모듈 툴은 안내와 함께 거절.
    let list = maru(&data, &["module", "list"]);
    assert_eq!(list[0]["id"], "point");
    assert_eq!(list[0]["installed"], false);
    let refused = run(&data, &["point", "ranking"]);
    assert_eq!(refused["ok"], false);
    assert_eq!(refused["error"]["code"], "CONFLICT", "{refused}");
    assert!(
        refused["error"]["next"][0]
            .as_str()
            .unwrap()
            .contains("module enable point")
    );
    // 모르는 명령은 사용법 오류, 모르는 툴·칸도 마찬가지.
    let unknown = run(&data, &["nope", "x"]);
    assert_eq!(unknown["error"]["code"], "USAGE", "{unknown}");
    assert_eq!(run(&data, &["point", "nope"])["error"]["code"], "USAGE");
    assert_eq!(
        run(&data, &["point", "ranking", "--nope", "1"])["error"]["code"],
        "USAGE"
    );
    assert_eq!(
        run(&data, &["point", "ranking", "--limit", "많이"])["error"]["code"],
        "USAGE"
    );

    // 위젯 종류는 켜야 생긴다.
    let types = maru(&data, &["widget", "types"]);
    assert!(
        !types
            .as_array()
            .unwrap()
            .iter()
            .any(|t| t["id"] == "point_ranking")
    );
    let bad = run(
        &data,
        &["widget", "place", "point_ranking", "--region", "sidebar"],
    );
    assert_eq!(bad["error"]["code"], "VALIDATION", "{bad}");

    let (server, bind) = start_server(&data);
    assert_eq!(
        get(&bind, "/m/point/ranking").0,
        404,
        "꺼진 모듈의 주소는 없다"
    );
    // 켜기 전의 로그인은 소급하지 않는다.
    let session = signup_and_login(&bind, email, "달빛이");

    // 서버가 켜진 채 CLI 로 켜면 재시작 없이 붙는다.
    let on = maru(&data, &["module", "enable", "point"]);
    assert_eq!(on["enabled"], true);
    assert_eq!(
        maru(&data, &["module", "enable", "point"])["enabled"],
        true,
        "멱등"
    );
    let (status, page) = get(&bind, "/m/point/ranking");
    assert_eq!(status, 200, "{page}");
    assert!(
        page.contains("포인트 순위") && page.contains("아직 포인트를"),
        "{page}"
    );
    assert!(
        page.contains("<title>포인트 순위 · "),
        "제목에 사이트 이름: {page}"
    );

    // 위젯을 놓는다: 설정 검사도 모듈이 한다.
    let bad = run(
        &data,
        &[
            "widget",
            "place",
            "point_ranking",
            "--region",
            "sidebar",
            "--config",
            r#"{"limit":99}"#,
        ],
    );
    assert_eq!(bad["error"]["code"], "VALIDATION", "{bad}");
    let widget = maru(
        &data,
        &[
            "widget",
            "place",
            "point_ranking",
            "--region",
            "sidebar",
            "--title",
            "활동왕",
        ],
    );
    assert_eq!(widget["config"]["limit"], 5, "기본값을 채운다");
    let (_, home) = get(&bind, "/");
    assert!(
        home.contains(">활동왕</h2>") && home.contains("아직 포인트를"),
        "{home}"
    );

    // 웹에서 쓴 글을 서버가 저절로 넘겨받아 글쓴이에게 적립한다.
    write_post(&bind, &session, "첫 글");
    wait_until("글 적립", || {
        maru(&data, &["point", "balance", "--member", email])["balance"] == 10
    });
    wait_until("위젯에 회원이 보임", || {
        get(&bind, "/").1.contains("달빛이")
    });
    let (_, ranking) = get(&bind, "/m/point/ranking");
    assert!(
        ranking.contains("달빛이") && ranking.contains("10"),
        "{ranking}"
    );

    // 다른 프로세스(CLI)가 쓴 글도 감사 기록으로 넘어간다. 글쓴이가 없으면 적립하지 않을 뿐이다.
    let cli_post = maru(
        &data,
        &[
            "post",
            "new",
            "--board",
            "blog",
            "--title",
            "운영자 글",
            "--body",
            "본문",
            "--publish",
        ],
    );
    assert_eq!(cli_post["status"], "published");
    assert!(
        maru(&data, &["module", "failures"])
            .as_array()
            .unwrap()
            .is_empty()
    );

    // 설정: CLI 와 REST 가 같은 검증을 쓴다.
    let saved = maru(&data, &["module", "set", "point", "--field", "post=20"]);
    assert_eq!(saved["settings"]["post"], 20);
    let bad = run(&data, &["module", "set", "point", "--field", "post=많이"]);
    assert_eq!(bad["error"]["code"], "VALIDATION", "{bad}");

    // REST: 목록·툴·권한.
    let token = maru(
        &data,
        &[
            "token", "create", "--name", "도구", "--scope", "read", "--scope", "admin",
        ],
    );
    let token = token["secret"].as_str().unwrap().to_owned();
    let auth = format!("Bearer {token}");
    let headers = [
        ("Authorization", auth.as_str()),
        ("Content-Type", "application/json"),
    ];
    let (status, body) = http(&bind, "GET", "/api/v1/modules", &headers, "");
    assert_eq!(status, 200, "{body}");
    let v: Value = serde_json::from_str(&body).unwrap();
    assert_eq!(v["data"][0]["enabled"], true);
    assert_eq!(v["data"][0]["settings"]["post"], 20);
    let (status, body) = http(
        &bind,
        "POST",
        "/api/v1/modules/point/tools/ranking",
        &headers,
        &json!({ "limit": 3 }).to_string(),
    );
    assert_eq!(status, 200, "{body}");
    let v: Value = serde_json::from_str(&body).unwrap();
    assert_eq!(v["data"]["rows"][0]["balance"], 10);
    let (status, _) = http(&bind, "GET", "/api/v1/modules", &[], "");
    assert_eq!(status, 401);
    let (status, body) = http(&bind, "DELETE", "/api/v1/modules/point", &headers, "");
    assert_eq!(status, 400, "purge 없이는 지우지 않는다: {body}");

    // 끄기: 주소 404, 위젯 조용히 빠짐, 위젯 설정·데이터 유지.
    let (status, body) = http(&bind, "POST", "/api/v1/modules/point/disable", &headers, "");
    assert_eq!(status, 200, "{body}");
    assert_eq!(get(&bind, "/m/point/ranking").0, 404);
    let (status, home) = get(&bind, "/");
    assert_eq!(status, 200);
    assert!(!home.contains("활동왕"), "{home}");
    assert_eq!(
        maru(&data, &["widget", "list"]).as_array().unwrap().len(),
        1
    );
    maru(&data, &["module", "enable", "point"]);
    assert_eq!(
        maru(&data, &["point", "balance", "--member", email])["balance"],
        10
    );
    assert!(get(&bind, "/").1.contains("활동왕"));

    // 제거: 확인 없이는 거절, 있으면 표가 사라지고 코어 글은 그대로.
    let refused = run(&data, &["module", "remove", "point"]);
    assert_eq!(refused["error"]["code"], "VALIDATION", "{refused}");
    let removed = maru(&data, &["module", "remove", "point", "--purge", "--yes"]);
    assert_eq!(removed["reverted"].as_array().unwrap().len(), 1);
    assert_eq!(maru(&data, &["module", "list"])[0]["installed"], false);
    assert_eq!(get(&bind, "/m/point/ranking").0, 404);
    let (status, home) = get(&bind, "/");
    assert_eq!(status, 200);
    assert!(home.contains("첫 글"), "코어 글은 그대로: {home}");
    assert!(!home.contains("활동왕"));

    // 감사 기록
    let audit = maru(&data, &["audit", "list", "--action", "module"]);
    assert!(audit["total"].as_i64().unwrap() >= 5, "{audit}");
    drop(server);
}

/// 관리 화면의 폼으로 켜고, 설정을 고치고, 끄고, 지운다.
#[test]
fn admin_screen_drives_the_module_lifecycle() {
    let tmp = tempfile::tempdir().unwrap();
    let data = tmp.path().join("사이트");
    maru(&data, &["init", "--preset", "blog", "--yes"]);
    let link = maru(&data, &["login-link"]);
    let (_server, bind) = start_server(&data);

    // 로그인 링크의 확인 화면을 거쳐 세션을 받는다.
    let rest = link["url"].as_str().unwrap().split_once("://").unwrap().1;
    let path = &rest[rest.find('/').unwrap()..];
    let (_, head, _) = http_full(&bind, "GET", path, &[], "");
    let pre = cookie_of(&head, "maru_csrf").unwrap();
    let cookie = format!("maru_csrf={pre}");
    let headers = [FORM, ("Cookie", cookie.as_str())];
    let (_, head, body) = http_full(&bind, "POST", path, &headers, &form(&[("_csrf", &pre)]));
    let session = cookie_of(&head, "maru_session").unwrap_or_else(|| panic!("로그인 실패: {body}"));
    let cookie = format!("maru_session={session}");
    let get = |p: &str| http_full(&bind, "GET", p, &[("Cookie", &cookie)], "");
    let post = |p: &str, csrf: &str, pairs: &[(&str, &str)]| {
        let mut all = vec![("_csrf", csrf)];
        all.extend_from_slice(pairs);
        http_full(&bind, "POST", p, &[FORM, ("Cookie", &cookie)], &form(&all))
    };

    let (status, _, page) = get("/admin/modules");
    assert_eq!(status, 200, "{page}");
    assert!(
        page.contains("설치 안 됨") && page.contains("/admin/modules/point/enable"),
        "{page}"
    );
    assert!(
        !page.contains("name=\"post\""),
        "꺼진 모듈은 설정 칸이 없다"
    );
    let csrf = csrf_of(&page);

    // CSRF 가 틀리면 켜지지 않는다.
    assert_eq!(post("/admin/modules/point/enable", "틀림", &[]).0, 403);
    assert_eq!(maru(&data, &["module", "list"])[0]["enabled"], false);

    // 켜기: 알림은 한 번만 보이고 설정 칸이 생긴다.
    let (status, head, body) = post("/admin/modules/point/enable", &csrf, &[]);
    assert_eq!(status, 303, "{body}");
    let at = head
        .lines()
        .find_map(|l| {
            l.strip_prefix("location: ")
                .or(l.strip_prefix("Location: "))
        })
        .unwrap();
    let (_, _, page) = get(at);
    assert_eq!(page.matches("class=\"notice\"").count(), 1, "{page}");
    assert!(
        page.contains("name=\"post\" ") || page.contains("name=\"post\">"),
        "{page}"
    );
    assert_eq!(maru(&data, &["module", "list"])[0]["enabled"], true);

    // 설정: 잘못되면 422 + 값 유지 + 오류는 한 번만, 맞으면 DB 에 남는다.
    let fields = [
        ("post", "많이"),
        ("comment", "2"),
        ("login", "1"),
        ("levels", "100,500"),
    ];
    let (status, _, page) = post("/admin/modules/point/settings", &csrf, &fields);
    assert_eq!(status, 422, "{page}");
    assert!(page.contains("value=\"많이\""), "{page}");
    assert_eq!(page.matches("class=\"error\"").count(), 1, "{page}");
    assert_eq!(
        maru(&data, &["module", "show", "point"])["settings"]["post"],
        10
    );
    let fields = [
        ("post", "30"),
        ("comment", "2"),
        ("login", "1"),
        ("levels", "100,500"),
    ];
    assert_eq!(post("/admin/modules/point/settings", &csrf, &fields).0, 303);
    let shown = maru(&data, &["module", "show", "point"]);
    assert_eq!(shown["settings"]["post"], 30);
    assert_eq!(shown["settings"]["levels"], "100,500");

    // 제거: 확인 칸이 맞아야 한다. 먼저 끄기.
    assert_eq!(post("/admin/modules/point/disable", &csrf, &[]).0, 303);
    assert_eq!(get("/m/point/ranking").0, 404);
    let (status, _, page) = post("/admin/modules/point/remove", &csrf, &[("confirm", "아님")]);
    assert_eq!(status, 422, "{page}");
    assert_eq!(maru(&data, &["module", "list"])[0]["installed"], true);
    assert_eq!(
        post(
            "/admin/modules/point/remove",
            &csrf,
            &[("confirm", "point")]
        )
        .0,
        303
    );
    assert_eq!(maru(&data, &["module", "list"])[0]["installed"], false);

    // 없는 모듈·동작, 로그인 안 한 사람.
    assert_eq!(post("/admin/modules/nope/enable", &csrf, &[]).0, 404);
    assert_eq!(post("/admin/modules/point/nope", &csrf, &[]).0, 404);
    assert_eq!(http_full(&bind, "GET", "/admin/modules", &[], "").0, 303);
}

/// 복원하면 커서가 감사 기록 끝으로 옮겨져 옛 사건을 다시 넘기지 않는다.
#[test]
fn restore_moves_the_cursor_to_the_end() {
    let tmp = tempfile::tempdir().unwrap();
    let data = tmp.path().join("사이트");
    maru(&data, &["init", "--preset", "blog", "--yes"]);
    maru(&data, &["login-link"]);
    maru(&data, &["module", "enable", "point"]);
    maru(
        &data,
        &[
            "point",
            "adjust",
            "--member",
            "owner@localhost",
            "--amount",
            "7",
            "--reason",
            "복원 시험",
        ],
    );
    let made = maru(&data, &["backup"]);
    let file = made["path"].as_str().unwrap().to_owned();
    maru(
        &data,
        &[
            "post",
            "new",
            "--board",
            "blog",
            "--title",
            "백업 뒤 글",
            "--body",
            "본문",
            "--publish",
        ],
    );
    maru(&data, &["restore", &file, "--yes"]);

    let list = maru(&data, &["module", "list"]);
    assert_eq!(list[0]["enabled"], true, "모듈 상태도 함께 돌아온다");
    assert_eq!(maru(&data, &["module", "deliver"])["delivered"], 0);
    assert_eq!(
        maru(&data, &["point", "balance", "--member", "owner@localhost"])["balance"],
        7
    );
    // 복원 뒤의 사건은 다시 넘어간다.
    maru(
        &data,
        &[
            "post",
            "new",
            "--board",
            "blog",
            "--title",
            "복원 뒤 글",
            "--body",
            "본문",
            "--publish",
        ],
    );
    assert!(
        maru(&data, &["module", "deliver"])["delivered"]
            .as_i64()
            .unwrap()
            >= 1
    );
}
