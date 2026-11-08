//! GAP M11: 알림 모듈 (ADR-0069). 실제 바이너리·실제 포트·실제 DB 파일.
//! 댓글·답글 → 받는 사람 → 편지 → 읽음 → 가리기 → 메일 끄기 → 모듈 끄기.
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

/// 글 화면의 폼 주소로 댓글을 쓰고 댓글 ID 를 돌려준다.
fn comment(bind: &str, cookie: &str, url: &str, body: &str, reply_to: Option<&str>) -> String {
    let page = http(bind, "GET", url, &[("Cookie", cookie)], "").body;
    let at = page.find("id=\"comment-form\"").expect("댓글 폼");
    let at = at + page[at..].find("action=\"").unwrap() + 8;
    let action = &page[at..at + page[at..].find('"').unwrap()];
    let res = http(
        bind,
        "POST",
        action,
        &[FORM, ("Cookie", cookie), ("Sec-Fetch-Site", "same-origin")],
        &form(&[("body", body), ("reply_to", reply_to.unwrap_or(""))]),
    );
    assert_eq!(res.status, 303, "{}", res.body);
    let location = res.location();
    location
        .split_once("#comment-")
        .unwrap_or_else(|| panic!("공개된 댓글이 아니다: {location}"))
        .1
        .to_owned()
}

fn window() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs()
        / 600
}

#[test]
fn comments_replies_and_answers_notify_the_right_members() {
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
    maru(&data, &["module", "enable", "notify"]);
    let (server, bind) = start_server(&data);

    let session =
        |email: &str, name: &str| format!("maru_session={}", signup_and_login(&bind, email, name));
    let moon = session("moon@example.test", "달빛이");
    let sun = session("sun@example.test", "햇살이");
    let star = session("star@example.test", "별빛이");
    // 가입·로그인도 같은 요청 제한(분당 10회)을 쓴다. 서버를 새로 띄워 칸을 비운다.
    drop(server);
    let (server, bind) = start_server(&data);
    let id_of = |email: &str| {
        maru(&data, &["member", "show", email])["id"]
            .as_str()
            .unwrap()
            .to_owned()
    };
    let (moon_id, sun_id, star_id) = (
        id_of("moon@example.test"),
        id_of("sun@example.test"),
        id_of("star@example.test"),
    );
    let deliver = || maru(&data, &["module", "deliver"]);
    let list = |member: &str| maru(&data, &["notify", "list", "--member", member]);
    let mails = |to: &str| {
        let all = maru(&data, &["mail", "list", "--limit", "100"]);
        let all = all
            .as_array()
            .cloned()
            .unwrap_or_else(|| all["items"].as_array().cloned().expect("편지 목록"));
        all.into_iter()
            .filter(|m| m["to_addr"] == to)
            .collect::<Vec<_>>()
    };

    let url = write_post(&bind, &moon, "알림 받을 글");
    let start = window();

    // 내 글에 남이 쓴 댓글 → 글 작성자에게 한 건과 편지 한 통. 쓴 사람은 받지 않는다.
    let first = comment(&bind, &sun, &url, "잘 읽었습니다", None);
    deliver();
    let got = list(&moon_id);
    assert_eq!(got["total"], 1, "{got}");
    assert_eq!(got["unread"], 1);
    let item = got["items"][0].clone();
    assert_eq!(item["kind"], "comment");
    assert_eq!(item["unread"], true);
    assert_eq!(item["title"], "알림 받을 글");
    assert!(
        item["text"]
            .as_str()
            .unwrap()
            .contains("햇살이님이 내 글에 댓글"),
        "{item}"
    );
    assert!(
        item["url"]
            .as_str()
            .unwrap()
            .ends_with(&format!("#comment-{first}")),
        "{item}"
    );
    assert_eq!(list(&sun_id)["total"], 0);
    let to_moon = mails("moon@example.test");
    assert_eq!(to_moon.len(), 1, "{to_moon:?}");
    assert!(
        to_moon[0]["subject"].as_str().unwrap().contains("새 댓글"),
        "{:?}",
        to_moon[0]
    );
    assert!(mails("sun@example.test").is_empty());

    // 내 글에 내가 쓴 댓글은 알리지 않는다.
    comment(&bind, &moon, &url, "고맙습니다", None);
    deliver();
    assert_eq!(list(&moon_id)["total"], 1);

    // 다른 사람이 햇살이의 댓글에 답글 → 햇살이는 답글, 달빛이는 댓글 알림.
    let second = comment(&bind, &star, &url, "저도요", Some(&first));
    deliver();
    let got = list(&sun_id);
    assert_eq!(got["total"], 1, "{got}");
    assert_eq!(got["items"][0]["kind"], "reply");
    assert!(
        got["items"][0]["text"]
            .as_str()
            .unwrap()
            .contains("별빛이님이 내 댓글에 답글"),
        "{got}"
    );
    assert_eq!(list(&moon_id)["total"], 2);
    assert_eq!(list(&star_id)["total"], 0);
    assert_eq!(mails("sun@example.test").len(), 1);
    // 같은 글·같은 종류의 편지는 10분 칸에 한 통이다.
    if window() == start {
        assert_eq!(mails("moon@example.test").len(), 1);
    }

    // 글 작성자가 자기 댓글의 답글을 받을 때는 답글 한 건만 받는다(같은 사건에 두 건이 아니다).
    let mine = comment(&bind, &moon, &url, "한 가지 더요", None);
    let third = comment(&bind, &sun, &url, "네", Some(&mine));
    deliver();
    let got = list(&moon_id);
    assert_eq!(got["total"], 3, "{got}");
    assert_eq!(got["items"][0]["kind"], "reply", "{got}");

    // 같은 사건을 다시 받아도 한 건이다: 이미 공개된 댓글을 다시 승인한다.
    maru(&data, &["comment", "approve", &second]);
    deliver();
    assert_eq!(list(&moon_id)["total"], 3);

    // 화면: 내 알림, 내 정보 칸.
    let as_moon = [("Cookie", moon.as_str())];
    let page = http(&bind, "GET", "/m/notify", &as_moon, "");
    assert_eq!(page.status, 200, "{}", page.body);
    assert!(
        page.body.contains("햇살이님이 내 글에 댓글"),
        "{}",
        page.body
    );
    assert!(page.body.contains("새 알림 3"), "{}", page.body);
    assert!(page.body.contains("noindex"), "{}", page.body);
    let me = http(&bind, "GET", "/me", &as_moon, "");
    assert!(me.body.contains("me-notify"), "{}", me.body);
    // 손님은 로그인으로, 알림이 새지 않는다.
    let guest = http(&bind, "GET", "/m/notify", &[], "");
    assert_eq!(guest.status, 303);
    assert!(
        guest.location().starts_with("/login"),
        "{}",
        guest.location()
    );

    // 열기: 다른 사이트에서 온 요청, 남의 알림은 막고, 내 알림은 읽음 뒤 원본으로 간다.
    let item_id = list(&moon_id)["items"][1]["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let open = format!("/m/notify/open/{item_id}");
    let post = |path: &str, cookie: &str, site: &str| {
        http(
            &bind,
            "POST",
            path,
            &[FORM, ("Cookie", cookie), ("Sec-Fetch-Site", site)],
            "",
        )
    };
    assert_eq!(post(&open, &moon, "cross-site").status, 403);
    assert_eq!(post(&open, &sun, "same-origin").status, 404);
    assert_eq!(list(&moon_id)["unread"], 3);
    let res = post(&open, &moon, "same-origin");
    assert_eq!(res.status, 303, "{}", res.body);
    assert!(res.location().contains("#comment-"), "{}", res.location());
    assert_eq!(list(&moon_id)["unread"], 2);
    // 다시 열어도 그대로다.
    assert_eq!(post(&open, &moon, "same-origin").status, 303);
    assert_eq!(list(&moon_id)["unread"], 2);
    // 모두 읽음.
    assert_eq!(post("/m/notify/read-all", &moon, "same-origin").status, 303);
    assert_eq!(list(&moon_id)["unread"], 0);

    // 댓글을 가리면 알림을 거둔다.
    maru(&data, &["comment", "hide", &third]);
    deliver();
    assert_eq!(list(&moon_id)["total"], 2);

    // 메일 받기를 끄면 사이트 안에만 쌓인다.
    let before = mails("sun@example.test").len();
    assert_eq!(post("/m/notify/prefs", &sun, "same-origin").status, 303);
    comment(&bind, &star, &url, "한 번 더요", Some(&first));
    deliver();
    assert_eq!(list(&sun_id)["total"], 2);
    assert_eq!(mails("sun@example.test").len(), before);

    // 답변글 → 위 글의 작성자. 이름은 쓴 사람(CLI 는 운영자)이다.
    let post_id = list(&moon_id)["items"][0]["post_id"]
        .as_str()
        .unwrap()
        .to_owned();
    maru(&data, &["board", "update", "free", "--reply", "editor"]);
    let answer = maru(
        &data,
        &[
            "post",
            "reply",
            &post_id,
            "--title",
            "답변입니다",
            "--body",
            "답변 본문",
        ],
    );
    // 초안일 때는 알리지 않는다.
    deliver();
    assert_ne!(list(&moon_id)["items"][0]["kind"], "answer");
    maru(&data, &["post", "publish", answer["id"].as_str().unwrap()]);
    deliver();
    let got = list(&moon_id);
    assert_eq!(got["items"][0]["kind"], "answer", "{got}");
    assert_eq!(got["items"][0]["title"], "답변입니다", "{got}");

    // 끄면 화면이 없다. 켜면 다시 있다. 재시작하지 않았다.
    maru(&data, &["module", "disable", "notify"]);
    assert_eq!(http(&bind, "GET", "/m/notify", &as_moon, "").status, 404);
    maru(&data, &["module", "enable", "notify"]);
    assert_eq!(http(&bind, "GET", "/m/notify", &as_moon, "").status, 200);

    drop(server);
}
