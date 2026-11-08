//! GAP M19: 운영 신호 (ADR-0074). 실제 바이너리·실제 포트·실제 DB 파일로 신고 모듈의 신고 집중·묵은 신고가
//! CLI·REST·관리 화면 첫 쪽에 보이고, `maru signals alert` 가 소유자에게 메일을 한 번만 쌓는지 본다.
#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::path::Path;
use std::process::{Child, Command, Stdio};

use serde_json::Value;

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
        return s;
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
    res.cookie("maru_session")
        .unwrap_or_else(|| panic!("로그인 실패 {}: {}", res.status, res.body))
}

/// 회원이 공개 글쓰기로 글을 쓰고 글 주소를 돌려준다.
fn write_post(bind: &str, cookie: &str, title: &str) -> String {
    let page = http(bind, "GET", "/write/free", &[("Cookie", cookie)], "");
    assert_eq!(page.status, 200, "{}", page.body);
    let res = http(
        bind,
        "POST",
        "/write/free",
        &[FORM, ("Cookie", cookie)],
        &form(&[
            ("_csrf", &page.csrf()),
            ("title", title),
            ("body", "본문입니다."),
        ]),
    );
    assert_eq!(res.status, 303, "{}", res.body);
    res.location()
}

fn setup(data: &Path) {
    maru(data, &["init", "--no-samples", "--preset", "blog", "--yes"]);
    maru(
        data,
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
        data,
        &[
            "board", "create", "free", "--name", "자유", "--kind", "free",
        ],
    );
    maru(
        data,
        &["board", "update", "free", "--write-approval", "off"],
    );
}

/// 가입하고 로그인해 쿠키 머리글 값을 돌려준다.
fn member(bind: &str, email: &str, name: &str) -> String {
    format!("maru_session={}", signup_and_login(bind, email, name))
}

/// 신고 폼으로 낸다.
fn report(bind: &str, cookie: &str, kind: &str, id: &str, reason: &str) -> Res {
    http(
        bind,
        "POST",
        &format!("/m/report/new/{kind}/{id}"),
        &[FORM, ("Cookie", cookie)],
        &form(&[("reason", reason), ("note", "설명입니다")]),
    )
}

fn post_id(data: &Path, title: &str) -> String {
    let list = maru(data, &["post", "list", "--board", "free"]);
    let items = list["items"].as_array().unwrap();
    items
        .iter()
        .find(|p| p["title"] == title)
        .unwrap_or_else(|| panic!("글 없음: {list}"))["id"]
        .as_str()
        .unwrap()
        .to_owned()
}

fn db_exec(data: &Path, sql: &'static str) {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(async {
        let db = maru_storage::Db::open(&data.join("maru.db"), false)
            .await
            .unwrap();
        sqlx::query(sql).execute(db.write_pool()).await.unwrap();
        db.close().await;
    });
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

fn kinds(list: &Value) -> Vec<String> {
    list.as_array()
        .unwrap()
        .iter()
        .map(|s| s["kind"].as_str().unwrap().to_owned())
        .collect()
}

#[test]
fn report_signals_reach_cli_rest_admin_and_mail() {
    let tmp = tempfile::tempdir().unwrap();
    let data = tmp.path().join("사이트");
    setup(&data);
    maru(&data, &["module", "enable", "report"]);
    maru(
        &data,
        &[
            "module",
            "set",
            "report",
            "--field",
            "auto_hide=0",
            "--field",
            "crowd=2",
        ],
    );
    let admin = maru(
        &data,
        &["token", "create", "--name", "운영", "--scope", "publish"],
    );
    let admin = format!("Bearer {}", admin["secret"].as_str().unwrap());
    let reader = maru(
        &data,
        &["token", "create", "--name", "읽기", "--scope", "read"],
    );
    let reader = format!("Bearer {}", reader["secret"].as_str().unwrap());
    let link = maru(&data, &["login-link"]);
    let (server, bind) = start_server(&data);
    let writer = member(&bind, "w@example.test", "글쓴이");
    let one = member(&bind, "b@example.test", "신고일");
    let two = member(&bind, "c@example.test", "신고이");
    drop(server);
    let (_server, bind) = start_server(&data);
    write_post(&bind, &writer, "몰린 글");
    let id = post_id(&data, "몰린 글");

    // 한 건이면 아직 신호가 아니다. 두 사람이 신고하면 「신고 집중」.
    assert_eq!(report(&bind, &one, "post", &id, "spam").status, 200);
    assert!(maru(&data, &["signals"]).as_array().unwrap().is_empty());
    assert_eq!(report(&bind, &two, "post", &id, "abuse").status, 200);
    let list = maru(&data, &["signals"]);
    assert_eq!(kinds(&list), ["report_crowd"], "{list}");
    assert_eq!(list[0]["count"], 2);
    assert!(list[0]["message"].as_str().unwrap().contains("몰린 글"));
    assert_eq!(list[0]["url"], "/m/report");

    // 24시간 넘게 처리하지 않으면 「묵은 신고」도.
    db_exec(
        &data,
        "UPDATE mod_report_item SET at = '2000-01-01T00:00:00Z'",
    );
    let list = maru(&data, &["signals"]);
    assert_eq!(kinds(&list), ["report_crowd", "report_stale"], "{list}");
    assert_eq!(list[1]["count"], 1, "대상 하나에 신고 둘이면 한 건");

    // REST 는 같은 서비스. 읽기 토큰은 못 본다.
    let res = http(
        &bind,
        "GET",
        "/api/v1/signals",
        &[("Authorization", &admin)],
        "",
    );
    assert_eq!(res.status, 200, "{}", res.body);
    assert!(res.body.contains("report_stale"), "{}", res.body);
    let res = http(
        &bind,
        "GET",
        "/api/v1/signals",
        &[("Authorization", &reader)],
        "",
    );
    assert_eq!(res.status, 403, "{}", res.body);

    // 관리 화면 첫 쪽에 「살펴볼 일」. 회원은 신호를 보지 못한다.
    let session = sign_in(&bind, link["url"].as_str().unwrap());
    let home = http(&bind, "GET", "/admin", &[("Cookie", &session)], "");
    assert_eq!(home.status, 200);
    assert!(
        home.body.contains("살펴볼 일") && home.body.contains("신고가 몰렸습니다"),
        "{}",
        home.body
    );

    // 메일은 한 번만 쌓인다.
    let a = maru(&data, &["signals", "alert"]);
    assert_eq!(a["alerted"], 2, "{a}");
    assert_eq!(maru(&data, &["signals", "alert"])["alerted"], 0);
    let mails = maru(&data, &["mail", "list"]);
    let ours: Vec<&Value> = mails
        .as_array()
        .unwrap()
        .iter()
        .filter(|m| m["subject"].as_str().unwrap().contains("살펴볼 일"))
        .collect();
    assert_eq!(ours.len(), 1, "{mails}");

    // 처리하면 신호가 사라진다.
    maru(
        &data,
        &[
            "report",
            "resolve",
            "--target-type",
            "post",
            "--target-id",
            &id,
            "--action",
            "dismiss",
        ],
    );
    assert!(maru(&data, &["signals"]).as_array().unwrap().is_empty());
}
