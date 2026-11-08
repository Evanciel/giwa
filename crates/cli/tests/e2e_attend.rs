//! GAP Q10: 출석체크 (ADR-0095). 실제 바이너리·실제 포트·실제 DB 파일로
//! 손님 화면 → 출처 검사 → 회원 출석(하루 한 번) → 달력·오늘 출석 목록 → CLI 툴을 확인한다.
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

fn header(res: &Res, name: &str) -> String {
    res.head
        .lines()
        .find_map(|l| {
            let (k, v) = l.split_once(':')?;
            k.trim()
                .eq_ignore_ascii_case(name)
                .then(|| v.trim().to_owned())
        })
        .unwrap_or_default()
}

#[test]
fn attendance_once_a_day_with_calendar() {
    let tmp = tempfile::tempdir().unwrap();
    let data = tmp.path().join("사이트");
    maru(
        &data,
        &["init", "--no-samples", "--preset", "blog", "--yes"],
    );
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
    maru(&data, &["module", "enable", "point"]);
    let (_server, bind) = start_server(&data);

    // 손님: 목록은 보이고 출석 단추 대신 로그인 링크. 사람마다 다른 화면이라 공유 캐시 금지.
    let page = http(&bind, "GET", "/m/point/attend", &[], "");
    assert_eq!(page.status, 200, "{}", page.body);
    assert!(page.body.contains("로그인하고 출석하기"), "{}", page.body);
    assert!(page.body.contains("href=\"/login?next="), "{}", page.body);
    assert!(
        page.body.contains("아직 오늘 출석한 회원이 없습니다"),
        "{}",
        page.body
    );
    assert!(
        page.body
            .contains("출석하면 5점을 드립니다. 7일 연달아 출석할 때마다 20점을 더 드립니다.")
    );
    assert!(!page.body.contains("attend-cal\""), "손님은 달력 없음");
    assert!(
        header(&page, "cache-control").starts_with("private"),
        "{}",
        page.head
    );
    // 손님이 누르면 로그인으로, 다른 사이트에서 온 요청은 거부
    let res = http(
        &bind,
        "POST",
        "/m/point/attend",
        &[("Sec-Fetch-Site", "same-origin")],
        "",
    );
    assert_eq!(res.status, 303);
    assert!(res.location().starts_with("/login?next="), "{}", res.head);
    let cookie = format!(
        "maru_session={}",
        signup_and_login(&bind, "moon@example.test", "달빛이")
    );
    let res = http(
        &bind,
        "POST",
        "/m/point/attend",
        &[("Cookie", &cookie), ("Sec-Fetch-Site", "cross-site")],
        "",
    );
    assert_eq!(res.status, 403);

    // 회원: 출석 단추와 이번 달 달력(오늘 표시)
    let page = http(&bind, "GET", "/m/point/attend", &[("Cookie", &cookie)], "");
    assert!(page.body.contains(">출석하기</button>"), "{}", page.body);
    assert!(page.body.contains("class=\"attend-cal\""), "{}", page.body);
    assert!(
        page.body.contains(" today\" aria-current=\"date\""),
        "{}",
        page.body
    );

    // 출석 → 화면에 이름·연속 1일·1명. 두 번 눌러도 한 번
    for _ in 0..2 {
        let res = http(
            &bind,
            "POST",
            "/m/point/attend",
            &[("Cookie", &cookie), ("Sec-Fetch-Site", "same-origin")],
            "",
        );
        assert_eq!(res.status, 303, "{}", res.body);
        assert_eq!(res.location(), "/m/point/attend");
    }
    let page = http(&bind, "GET", "/m/point/attend", &[("Cookie", &cookie)], "");
    assert!(
        page.body
            .contains("오늘 출석했습니다. 연속 <strong>1일</strong>째예요."),
        "{}",
        page.body
    );
    assert!(!page.body.contains(">출석하기</button>"));
    assert!(
        page.body
            .contains("<span class=\"attend-count\">1명</span>"),
        "{}",
        page.body
    );
    assert!(
        page.body
            .contains("<span class=\"name\">달빛이</span><span class=\"streak\">연속 1일</span>")
    );
    assert!(
        page.body
            .contains("class=\"on today\" aria-current=\"date\""),
        "{}",
        page.body
    );
    assert!(
        page.body
            .contains("<span class=\"attend-count\">1일</span>"),
        "이번 달 1일"
    );
    // 손님에게도 오늘 출석 목록은 같다
    let guest = http(&bind, "GET", "/m/point/attend", &[], "").body;
    assert!(
        guest.contains("<span class=\"name\">달빛이</span>"),
        "{guest}"
    );

    // 지난달·모르는 달·앞으로 올 달
    let page = http(
        &bind,
        "GET",
        "/m/point/attend?month=2020-01",
        &[("Cookie", &cookie)],
        "",
    )
    .body;
    assert!(page.contains("내 출석 · 2020년 1월"), "{page}");
    assert!(
        page.contains("?month=2020-02\" rel=\"nofollow\">다음 달</a>"),
        "{page}"
    );
    assert!(!page.contains("class=\"on"), "그달엔 출석 없음");
    let now = http(&bind, "GET", "/m/point/attend", &[("Cookie", &cookie)], "").body;
    for odd in ["2999-01", "nope", "2026-13"] {
        let page = http(
            &bind,
            "GET",
            &format!("/m/point/attend?month={odd}"),
            &[("Cookie", &cookie)],
            "",
        )
        .body;
        assert!(!page.contains("다음 달</a>"), "{odd}: 이번 달");
        assert_eq!(
            page.contains("class=\"on today\""),
            now.contains("class=\"on today\"")
        );
    }

    // CLI 툴: 같은 서비스
    let today = maru(&data, &["point", "attendance"]);
    assert_eq!(today["count"], 1, "{today}");
    assert_eq!(today["rows"][0]["name"], "달빛이");
    assert_eq!(today["rows"][0]["streak"], 1);
    let mine = maru(
        &data,
        &["point", "attendance", "--member", "moon@example.test"],
    );
    assert_eq!(mine["attended_today"], true, "{mine}");
    assert_eq!(mine["streak"], 1);
    assert_eq!(mine["days"].as_array().unwrap().len(), 1);
    let bad = run(&data, &["point", "attendance", "--day", "2026-02-30"]);
    assert_eq!(bad["error"]["code"], "VALIDATION", "{bad}");
    let bad = run(
        &data,
        &[
            "point",
            "attendance",
            "--member",
            "moon@example.test",
            "--month",
            "11월",
        ],
    );
    assert_eq!(bad["error"]["code"], "VALIDATION", "{bad}");
    // 회원이 아닌 CLI 는 출석할 수 없다
    let refused = run(&data, &["point", "attend"]);
    assert_eq!(refused["error"]["code"], "UNAUTHORIZED", "{refused}");
    // 출석 점수는 한 번만
    let history = maru(
        &data,
        &["point", "history", "--member", "moon@example.test"],
    );
    let attends = history["items"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|i| i["reason"] == "출석")
        .count();
    assert_eq!(attends, 1, "{history}");
    assert!(
        history["items"]
            .as_array()
            .unwrap()
            .iter()
            .any(|i| i["reason"] == "출석" && i["amount"] == 5),
        "{history}"
    );

    // 순위 화면에서 출석체크로 가는 링크
    let ranking = http(&bind, "GET", "/m/point/ranking", &[], "").body;
    assert!(ranking.contains("href=\"/m/point/attend\""), "{ranking}");
}
