//! GAP M12·M14: 쪽지·투표 모듈 (ADR-0070). 실제 바이너리·실제 포트·실제 DB 파일.
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

fn prepare(data: &Path) {
    maru(data, &["init", "--preset", "blog", "--yes"]);
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

fn submit(bind: &str, cookie: &str, path: &str, body: &str) -> Res {
    http(
        bind,
        "POST",
        path,
        &[FORM, ("Cookie", cookie), ("Sec-Fetch-Site", "same-origin")],
        body,
    )
}

#[test]
fn members_exchange_private_messages_and_can_block() {
    let tmp = tempfile::tempdir().unwrap();
    let data = tmp.path().join("사이트");
    prepare(&data);
    maru(&data, &["module", "enable", "message"]);
    let (server, bind) = start_server(&data);
    let moon = format!(
        "maru_session={}",
        signup_and_login(&bind, "moon@example.test", "달빛이")
    );
    let sun = format!(
        "maru_session={}",
        signup_and_login(&bind, "sun@example.test", "햇살이")
    );
    let star = format!(
        "maru_session={}",
        signup_and_login(&bind, "star@example.test", "별빛이")
    );
    let get = |path: &str, cookie: &str| http(&bind, "GET", path, &[("Cookie", cookie)], "");

    // 손님은 로그인으로.
    let guest = http(&bind, "GET", "/m/message", &[], "");
    assert_eq!(guest.status, 303);
    assert!(
        guest.location().starts_with("/login"),
        "{}",
        guest.location()
    );

    // 잘못된 받는 사람·빈 내용·나 자신은 폼으로 돌아오고 쓴 글은 남는다.
    let res = submit(
        &bind,
        &moon,
        "/m/message/send",
        &form(&[("to", "없는이름"), ("body", "<b>지키는 글</b>")]),
    );
    assert_eq!(res.status, 200, "{}", res.body);
    assert!(res.body.contains("받을 수 없는 회원"), "{}", res.body);
    assert!(res.body.contains("&lt;b&gt;지키는 글"), "{}", res.body);
    let res = submit(
        &bind,
        &moon,
        "/m/message/send",
        &form(&[("to", "달빛이"), ("body", "나에게")]),
    );
    assert!(res.body.contains("나에게는"), "{}", res.body);
    // 다른 사이트에서 온 요청은 막는다.
    let res = http(
        &bind,
        "POST",
        "/m/message/send",
        &[FORM, ("Cookie", &moon), ("Sec-Fetch-Site", "cross-site")],
        &form(&[("to", "햇살이"), ("body", "가짜")]),
    );
    assert_eq!(res.status, 403);

    // 보내기 → 받은 쪽지·안 읽음 → 읽기 → 읽음. HTML 은 이스케이프한다.
    let res = submit(
        &bind,
        &moon,
        "/m/message/send",
        &form(&[("to", "햇살이"), ("body", "안녕하세요 <script>1</script>")]),
    );
    assert_eq!(res.status, 303, "{}", res.body);
    assert_eq!(res.location(), "/m/message/sent");
    let inbox = get("/m/message", &sun).body;
    assert!(inbox.contains("달빛이"), "{inbox}");
    assert!(inbox.contains("안 읽음"), "{inbox}");
    assert!(!inbox.contains("<script>1"), "{inbox}");
    let at = inbox.find("/m/message/view/").unwrap();
    let view = inbox[at..at + inbox[at..].find('"').unwrap()].to_owned();
    let id = view.rsplit('/').next().unwrap().to_owned();

    // 제3자·손님은 읽지 못한다. 없는 것과 같다.
    assert_eq!(get(&view, &star).status, 404);
    assert_eq!(http(&bind, "GET", &view, &[], "").status, 303);
    let page = get(&view, &sun);
    assert_eq!(page.status, 200);
    assert!(page.body.contains("&lt;script&gt;1"), "{}", page.body);
    assert!(!get("/m/message", &sun).body.contains("안 읽음"));
    // 보낸 쪽에도 읽음으로 보인다.
    assert!(get(&view, &moon).body.contains("읽음"));

    // 운영자 CLI·토큰은 쪽지를 읽지 못한다.
    let denied = run(&data, &["message", "list"]);
    assert_eq!(denied["ok"], false, "{denied}");
    assert_eq!(denied["error"]["code"], "FORBIDDEN", "{denied}");

    // 감사 기록에 본문이 없다.
    let audit = maru(&data, &["audit", "list"]).to_string();
    assert!(audit.contains("message.send"), "{audit}");
    assert!(!audit.contains("안녕하세요"), "{audit}");

    // 받은 쪽이 지워도 보낸 쪽에는 남고, 둘 다 지우면 없어진다.
    let del = format!("/m/message/delete/{id}");
    assert_eq!(submit(&bind, &star, &del, "").status, 404);
    assert_eq!(submit(&bind, &sun, &del, "").status, 303);
    assert_eq!(get(&view, &sun).status, 404);
    assert_eq!(get(&view, &moon).status, 200);
    assert_eq!(submit(&bind, &moon, &del, "").status, 303);
    assert_eq!(get(&view, &moon).status, 404);

    // 차단하면 같은 말로 거절한다. 풀면 다시 된다.
    assert_eq!(
        submit(
            &bind,
            &sun,
            "/m/message/block",
            &form(&[("name", "별빛이")])
        )
        .status,
        303
    );
    assert!(get("/m/message/blocks", &sun).body.contains("별빛이"));
    let res = submit(
        &bind,
        &star,
        "/m/message/send",
        &form(&[("to", "햇살이"), ("body", "차단 뒤")]),
    );
    assert!(res.body.contains("받을 수 없는 회원"), "{}", res.body);
    assert_eq!(
        submit(
            &bind,
            &sun,
            "/m/message/block",
            &form(&[("name", "별빛이"), ("action", "off")])
        )
        .status,
        303
    );
    let res = submit(
        &bind,
        &star,
        "/m/message/send",
        &form(&[("to", "햇살이"), ("body", "풀린 뒤")]),
    );
    assert_eq!(res.status, 303, "{}", res.body);

    // 내 정보 칸.
    assert!(get("/me", &sun).body.contains("me-message"));

    // 하루 한도.
    maru(
        &data,
        &["module", "set", "message", "--field", "daily_limit=1"],
    );
    let res = submit(
        &bind,
        &star,
        "/m/message/send",
        &form(&[("to", "햇살이"), ("body", "한도 넘김")]),
    );
    assert_eq!(res.status, 429, "{}", res.body);

    // 끄면 화면이 없다.
    maru(&data, &["module", "disable", "message"]);
    assert_eq!(get("/m/message", &sun).status, 404);
    drop(server);
}

#[test]
fn polls_attach_to_posts_and_count_member_votes() {
    let tmp = tempfile::tempdir().unwrap();
    let data = tmp.path().join("사이트");
    prepare(&data);
    maru(&data, &["module", "enable", "poll"]);
    let (server, bind) = start_server(&data);
    let moon = format!(
        "maru_session={}",
        signup_and_login(&bind, "moon@example.test", "달빛이")
    );
    let sun = format!(
        "maru_session={}",
        signup_and_login(&bind, "sun@example.test", "햇살이")
    );
    let url = write_post(&bind, &moon, "투표 받을 글");
    let get = |path: &str, cookie: &str| http(&bind, "GET", path, &[("Cookie", cookie)], "");

    // 작성자에게만 만들기 단추가 보인다.
    let page = get(&url, &moon).body;
    let at = page.find("/m/poll/new/").expect("만들기 단추");
    let new_url = page[at..at + page[at..].find('"').unwrap()].to_owned();
    let post_id = new_url.rsplit('/').next().unwrap().to_owned();
    assert!(!get(&url, &sun).body.contains("/m/poll/new/"));
    assert_eq!(get(&new_url, &sun).status, 403);

    // 잘못 쓰면 폼으로 돌아온다.
    let res = submit(
        &bind,
        &moon,
        &new_url,
        &form(&[("question", "무엇?"), ("options", "하나뿐")]),
    );
    assert_eq!(res.status, 200, "{}", res.body);
    assert!(res.body.contains("서로 다른 보기"), "{}", res.body);
    assert_eq!(
        submit(
            &bind,
            &sun,
            &new_url,
            &form(&[("question", "남이"), ("options", "가\n나")])
        )
        .status,
        403
    );

    // 만들기: 줄마다 보기, 겹친 줄은 하나로.
    let res = submit(
        &bind,
        &moon,
        &new_url,
        &form(&[
            ("question", "어느 쪽이 좋나요?"),
            ("options", "산\n바다\n바다\n"),
        ]),
    );
    assert_eq!(res.status, 303, "{}", res.body);
    let polled = maru(&data, &["poll", "get", "--post", &post_id]);
    assert_eq!(polled["options"].as_array().unwrap().len(), 2, "{polled}");
    let sea = polled["options"][1]["id"].as_str().unwrap().to_owned();
    let hill = polled["options"][0]["id"].as_str().unwrap().to_owned();
    // 두 번째 투표는 만들 수 없다.
    let again = run(
        &data,
        &[
            "poll",
            "create",
            "--post",
            &post_id,
            "--question",
            "또",
            "--options",
            "가",
            "--options",
            "나",
        ],
    );
    assert_eq!(again["error"]["code"], "CONFLICT", "{again}");

    // 투표 전에는 결과가 안 보인다. 손님은 투표 단추가 없다.
    let page = get(&url, &sun).body;
    assert!(page.contains("어느 쪽이 좋나요?"), "{page}");
    assert!(page.contains("결과는 투표한 뒤에"), "{page}");
    assert!(!page.contains("<progress"), "{page}");
    let guest = http(&bind, "GET", &url, &[], "").body;
    assert!(!guest.contains("/m/poll/vote/"), "{guest}");
    assert!(guest.contains("어느 쪽이 좋나요?"), "{guest}");

    let vote = format!("/m/poll/vote/{post_id}");
    // 손님은 로그인으로. 다른 사이트의 요청은 막는다. 다른 값은 안 된다.
    let res = http(
        &bind,
        "POST",
        &vote,
        &[FORM, ("Sec-Fetch-Site", "same-origin")],
        &form(&[("option", &sea)]),
    );
    assert_eq!(res.status, 303);
    assert!(res.location().starts_with("/login"), "{}", res.location());
    let res = http(
        &bind,
        "POST",
        &vote,
        &[FORM, ("Cookie", &sun), ("Sec-Fetch-Site", "cross-site")],
        &form(&[("option", &sea)]),
    );
    assert_eq!(res.status, 403);
    assert_eq!(
        submit(&bind, &sun, &vote, &form(&[("option", "없는보기")])).status,
        400
    );
    // 하나만 고르는 투표에 둘을 낼 수 없다.
    let two = format!("option={}&option={}", enc(&sea), enc(&hill));
    assert_eq!(submit(&bind, &sun, &vote, &two).status, 400);

    // 투표 → 결과가 보인다. 다시 하면 대신한다.
    assert_eq!(
        submit(&bind, &sun, &vote, &form(&[("option", &sea)])).status,
        303
    );
    let page = get(&url, &sun).body;
    assert!(page.contains("<progress"), "{page}");
    assert!(page.contains("참여 1명"), "{page}");
    assert_eq!(
        submit(&bind, &sun, &vote, &form(&[("option", &hill)])).status,
        303
    );
    let polled = maru(&data, &["poll", "get", "--post", &post_id]);
    assert_eq!(polled["voters"], 1, "{polled}");
    assert_eq!(polled["options"][0]["votes"], 1, "{polled}");
    assert_eq!(polled["options"][1]["votes"], 0, "{polled}");

    // 감사 기록에 무엇을 골랐는지 남지 않는다.
    let audit = maru(&data, &["audit", "list"]).to_string();
    assert!(audit.contains("poll.vote"), "{audit}");
    assert!(!audit.contains(&format!("\"{hill}\"")), "{audit}");

    // 마감: 남은 못 하고, 뒤로는 표를 받지 않는다.
    let close = format!("/m/poll/close/{post_id}");
    assert_eq!(submit(&bind, &sun, &close, "").status, 403);
    assert_eq!(submit(&bind, &moon, &close, "").status, 303);
    assert_eq!(
        submit(&bind, &sun, &vote, &form(&[("option", &sea)])).status,
        400
    );
    let page = get(&url, &sun).body;
    assert!(page.contains("마감됨"), "{page}");
    assert!(!page.contains("/m/poll/vote/"), "{page}");

    // 지우면 사라지고 다시 만들 수 있다. CLI 는 배열을 --options 를 되풀이해 받는다.
    assert_eq!(
        submit(&bind, &moon, &format!("/m/poll/delete/{post_id}"), "").status,
        303
    );
    assert!(!get(&url, &sun).body.contains("어느 쪽이 좋나요?"));
    let made = maru(
        &data,
        &[
            "poll",
            "create",
            "--post",
            &post_id,
            "--question",
            "여럿 고르기",
            "--options",
            "가, 그리고",
            "--options",
            "나",
            "--multiple",
            "true",
        ],
    );
    assert_eq!(made["multiple"], true, "{made}");
    assert_eq!(made["options"][0]["label"], "가, 그리고", "{made}");

    // 끄면 글 화면에서 사라진다.
    maru(&data, &["module", "disable", "poll"]);
    assert!(!get(&url, &sun).body.contains("여럿 고르기"));
    drop(server);
}
