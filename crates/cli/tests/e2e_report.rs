//! GAP M7·M8: 반응·스크랩 모듈 (ADR-0068). 실제 바이너리·실제 포트·실제 DB 파일로
//! 켜기 → 공개 글의 단추 → 누르기 → DB·화면 → 끄기(단추 사라짐) → 탈퇴(기록 삭제)를 확인한다.
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

/// 가입하고 로그인해 쿠키 머리글 값을 돌려준다.
fn member(bind: &str, email: &str, name: &str) -> String {
    format!("maru_session={}", signup_and_login(bind, email, name))
}

/// 글 쪽의 댓글 폼으로 쓴다.
fn comment(bind: &str, cookie: &str, post: &str, body: &str) -> Res {
    let page = http(bind, "GET", post, &[("Cookie", cookie)], "");
    assert_eq!(page.status, 200, "{}", page.body);
    let tail = page.body.split("id=\"comment-form\"").nth(1).unwrap();
    let action = tail.split("action=\"").nth(1).unwrap();
    let action = action.split('"').next().unwrap().to_owned();
    http(
        bind,
        "POST",
        &action,
        &[FORM, ("Cookie", cookie)],
        &form(&[("body", body), ("reply_to", "")]),
    )
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

#[test]
fn banned_words_and_flood_limit_public_writes_only() {
    let tmp = tempfile::tempdir().unwrap();
    let data = tmp.path().join("사이트");
    setup(&data);
    maru(&data, &["site", "set", "--banned-words", "바보,Spam Word"]);
    let (server, bind) = start_server(&data);
    let cookie = member(&bind, "a@example.test", "회원가");
    let post = write_post(&bind, &cookie, "처음 글");

    // 띄어쓰기·기호·대소문자로 피할 수 없다. 쓴 글은 폼에 남고 어떤 말이 걸렸는지 알린다.
    let res = comment(&bind, &cookie, &post, "이 바 보.야");
    assert_eq!(res.status, 400, "{}", res.body);
    assert!(res.body.contains("금지어"), "{}", res.body);
    assert!(res.body.contains("바 보.야"), "{}", res.body);
    let res = comment(&bind, &cookie, &post, "spam-WORD 입니다");
    assert_eq!(res.status, 400, "{}", res.body);
    let res = comment(&bind, &cookie, &post, "괜찮은 말");
    assert_eq!(res.status, 303, "{}", res.body);

    // 글 제목도 걸러낸다.
    let page = http(&bind, "GET", "/write/free", &[("Cookie", &cookie)], "");
    let res = http(
        &bind,
        "POST",
        "/write/free",
        &[FORM, ("Cookie", &cookie)],
        &form(&[
            ("_csrf", &page.csrf()),
            ("title", "바보 글"),
            ("body", "본문입니다."),
        ]),
    );
    assert_eq!(res.status, 400, "{}", res.body);
    assert!(res.body.contains("금지어"), "{}", res.body);

    // 로컬 CLI 는 걸리지 않는다.
    let created = maru(
        &data,
        &[
            "post",
            "new",
            "--slug",
            "staff-post",
            "--board",
            "free",
            "--title",
            "바보 관리자",
            "--body",
            "본문",
        ],
    );
    assert!(created["id"].is_string(), "{created}");

    // 도배 방지를 켜면 바로 이어 쓸 수 없고, 쓴 글은 남는다.
    maru(&data, &["site", "set", "--flood-seconds", "600"]);
    let res = comment(&bind, &cookie, &post, "또 씁니다");
    assert_eq!(res.status, 429, "{}", res.body);
    assert!(res.body.contains("너무 빨리"), "{}", res.body);
    assert!(res.body.contains("또 씁니다"), "{}", res.body);

    // 범위를 벗어나면 받지 않는다. 비우면 모두 지운다.
    let v = run(&data, &["site", "set", "--flood-seconds", "3601"]);
    assert_eq!(v["ok"], false, "{v}");
    let s = maru(
        &data,
        &["site", "set", "--flood-seconds", "1", "--banned-words", ""],
    );
    assert_eq!(s["banned_words"].as_array().unwrap().len(), 0, "{s}");

    // 같은 말을 되풀이할 수 없다 (간격이 지나도).
    std::thread::sleep(std::time::Duration::from_millis(1100));
    let res = comment(&bind, &cookie, &post, "괜찮은 말");
    assert_eq!(res.status, 400, "{}", res.body);
    assert!(res.body.contains("같습니다"), "{}", res.body);
    let res = comment(&bind, &cookie, &post, "바보 다른 말");
    assert_eq!(res.status, 303, "{}", res.body);
    drop(server);
}

#[test]
fn reports_hide_content_and_a_moderator_decides() {
    let tmp = tempfile::tempdir().unwrap();
    let data = tmp.path().join("사이트");
    setup(&data);
    maru(&data, &["module", "enable", "report"]);
    maru(
        &data,
        &["module", "set", "report", "--field", "auto_hide=2"],
    );
    let (server, bind) = start_server(&data);
    let writer = member(&bind, "w@example.test", "글쓴이");
    let one = member(&bind, "b@example.test", "신고일");
    let two = member(&bind, "c@example.test", "신고이");
    drop(server);
    let (server, bind) = start_server(&data);
    let post = write_post(&bind, &writer, "신고될 글");
    let id = post_id(&data, "신고될 글");

    // 다른 사람 글에 신고 링크가 보인다. 자기 글, 손님에게는 없다.
    let link = format!("/m/report/new/post/{id}");
    let seen =
        |headers: &[(&str, &str)]| http(&bind, "GET", &post, headers, "").body.contains(&link);
    assert!(seen(&[("Cookie", &one)]));
    assert!(!seen(&[("Cookie", &writer)]));
    assert!(!seen(&[]));

    // 손님은 로그인으로 보낸다. 자기 글은 신고할 수 없다.
    let res = http(&bind, "GET", &link, &[], "");
    assert_eq!(res.status, 303);
    assert!(res.location().starts_with("/login"), "{}", res.head);
    let res = report(&bind, &writer, "post", &id, "spam");
    assert_eq!(res.status, 400, "{}", res.body);

    // 폼이 뜨고, 이유를 고르지 않으면 받지 않는다.
    let page = http(&bind, "GET", &link, &[("Cookie", &one)], "");
    assert_eq!(page.status, 200, "{}", page.body);
    assert!(page.body.contains("광고"), "{}", page.body);
    let res = report(&bind, &one, "post", &id, "");
    assert_eq!(res.status, 200, "{}", res.body);
    assert!(res.body.contains("이유를 고르세요"), "{}", res.body);

    // 같은 사람이 여러 번 내도 한 건이다: 가려지지 않는다.
    let res = report(&bind, &one, "post", &id, "spam");
    assert!(res.body.contains("신고를 받았습니다"), "{}", res.body);
    let res = report(&bind, &one, "post", &id, "abuse");
    assert!(res.body.contains("신고를 받았습니다"), "{}", res.body);
    assert_eq!(http(&bind, "GET", &post, &[], "").status, 200);
    let list = maru(&data, &["report", "list"]);
    assert_eq!(list["untrusted"], true, "{list}");
    assert_eq!(list["items"][0]["count"], 1, "{list}");

    // 다른 사람 신고가 모이면 자동으로 가린다.
    report(&bind, &two, "post", &id, "abuse");
    assert_eq!(http(&bind, "GET", &post, &[], "").status, 404);
    let list = maru(&data, &["report", "list"]);
    assert_eq!(list["items"][0]["auto_hidden"], true, "{list}");
    assert_eq!(list["items"][0]["count"], 2, "{list}");
    let audit = maru(&data, &["audit", "list", "--action", "report.auto_hide"]);
    assert_eq!(audit["items"].as_array().unwrap().len(), 1, "{audit}");

    // 가려진 글은 더 신고할 수 없다.
    let res = report(&bind, &one, "post", &id, "spam");
    assert_eq!(res.status, 404, "{}", res.body);

    // 일반 회원은 신고함을 볼 수 없다.
    let res = http(&bind, "GET", "/m/report", &[("Cookie", &one)], "");
    assert_eq!(res.status, 403, "{}", res.body);

    // 기각하면 자동으로 가린 것을 되살리고, 신고가 닫힌다.
    let done = maru(
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
    assert_eq!(done["resolved"], 2, "{done}");
    assert_eq!(http(&bind, "GET", &post, &[], "").status, 200);
    let list = maru(&data, &["report", "list"]);
    assert_eq!(list["total"], 0, "{list}");

    // 댓글: 신고가 모이면 가리고, 가림으로 처리하면 그대로다.
    let res = comment(&bind, &writer, &post, "신고될 댓글");
    assert_eq!(res.status, 303, "{}", res.body);
    let location = res.location();
    let cid = location.split("#comment-").nth(1).unwrap().to_owned();
    let page = http(&bind, "GET", &post, &[("Cookie", &one)], "");
    assert!(page.body.contains(&format!("/m/report/new/comment/{cid}")));
    report(&bind, &one, "comment", &cid, "abuse");
    report(&bind, &two, "comment", &cid, "abuse");
    let page = http(&bind, "GET", &post, &[], "");
    assert!(!page.body.contains("신고될 댓글"));
    let done = maru(
        &data,
        &[
            "report",
            "resolve",
            "--target-type",
            "comment",
            "--target-id",
            &cid,
            "--action",
            "uphold",
        ],
    );
    assert_eq!(done["resolved"], 2, "{done}");
    let page = http(&bind, "GET", &post, &[], "");
    assert!(!page.body.contains("신고될 댓글"));
    let list = maru(&data, &["report", "list", "--status", "upheld"]);
    assert_eq!(list["total"], 1, "{list}");
    drop(server);

    // 운영자 글은 몰려서 신고해도 가려지지 않는다.
    let (server, bind) = start_server(&data);
    let staff = maru(
        &data,
        &[
            "post",
            "new",
            "--slug",
            "staff-post",
            "--board",
            "free",
            "--title",
            "운영자 글",
            "--body",
            "본문",
        ],
    );
    let staff = staff["id"].as_str().unwrap().to_owned();
    maru(&data, &["post", "publish", &staff]);
    report(&bind, &one, "post", &staff, "spam");
    report(&bind, &two, "post", &staff, "spam");
    let list = maru(&data, &["report", "list"]);
    assert_eq!(list["items"][0]["auto_hidden"], false, "{list}");
    assert_eq!(list["items"][0]["public"], true, "{list}");
    drop(server);
}
