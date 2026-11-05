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

#[test]
fn reactions_and_scraps_from_the_public_post_page() {
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
    let (server, bind) = start_server(&data);

    let author = format!(
        "maru_session={}",
        signup_and_login(&bind, "moon@example.test", "달빛이")
    );
    let reader_session = signup_and_login(&bind, "sun@example.test", "햇살이");
    let reader = format!("maru_session={reader_session}");
    let url = write_post(&bind, &author, "반응 받을 글");
    let as_reader = [("Cookie", reader.as_str())];

    // 끈 동안에는 단추가 없다.
    let page = http(&bind, "GET", &url, &as_reader, "").body;
    assert!(!page.contains("react-bar"), "{page}");

    maru(&data, &["module", "enable", "reaction"]);
    maru(&data, &["module", "enable", "scrap"]);

    // 켜면 글 화면에 단추가 생긴다. 재시작하지 않았다.
    let page = http(&bind, "GET", &url, &as_reader, "").body;
    let at = page.find("action=\"/m/reaction/post/").unwrap() + 8;
    let action = page[at..at + page[at..].find('"').unwrap()].to_owned();
    let post_id = action.rsplit('/').next().unwrap().to_owned();
    assert!(
        page.contains("추천") && page.contains("비추천") && page.contains("스크랩"),
        "{page}"
    );
    assert!(page.contains("aria-pressed=\"false\""));

    let press = |cookie: &str, kind: &str| {
        http(
            &bind,
            "POST",
            &action,
            &[FORM, ("Cookie", cookie), ("Sec-Fetch-Site", "same-origin")],
            &form(&[("kind", kind)]),
        )
    };
    let counts = |post: &str| {
        maru(
            &data,
            &[
                "reaction",
                "counts",
                "--target-type",
                "post",
                "--target-id",
                post,
            ],
        )["kinds"]
            .clone()
    };

    // 다른 사이트에서 온 폼은 거절.
    let res = http(
        &bind,
        "POST",
        &action,
        &[FORM, ("Cookie", &reader), ("Sec-Fetch-Site", "cross-site")],
        &form(&[("kind", "up")]),
    );
    assert_eq!(res.status, 403);
    assert_eq!(counts(&post_id)[0]["count"], 0);

    // 누르면 DB 에 남고 글 화면으로 돌아와 눌린 모양과 수가 보인다.
    let res = press(&reader, "up");
    assert_eq!(res.status, 303, "{}", res.body);
    assert!(res.location().ends_with("#reactions"), "{}", res.location());
    assert_eq!(counts(&post_id)[0]["count"], 1);
    let page = http(&bind, "GET", &url, &as_reader, "").body;
    assert!(page.contains("class=\"pill on\""), "{page}");
    assert!(page.contains("aria-pressed=\"true\""));

    // 다른 것을 누르면 바뀌고, 같은 것을 또 누르면 취소된다. 한 사람 하나.
    press(&reader, "down");
    let now = counts(&post_id);
    assert_eq!(
        (now[0]["count"].clone(), now[1]["count"].clone()),
        (0.into(), 1.into())
    );
    press(&reader, "down");
    let now = counts(&post_id);
    assert_eq!(
        (now[0]["count"].clone(), now[1]["count"].clone()),
        (0.into(), 0.into())
    );
    press(&reader, "up");

    // 자기 글에는 못 누른다, 끄지 않은 종류만, 손님은 로그인으로.
    let res = press(&author, "up");
    assert_eq!(res.status, 403, "{}", res.body);
    assert_eq!(press(&reader, "heart").status, 400);
    let res = http(
        &bind,
        "POST",
        &action,
        &[FORM, ("Sec-Fetch-Site", "same-origin")],
        &form(&[("kind", "up")]),
    );
    assert_eq!(res.status, 303);
    assert!(
        res.location().starts_with("/login?next="),
        "{}",
        res.location()
    );
    assert_eq!(counts(&post_id)[0]["count"], 1);

    // 설정: 종류를 늘리고 손님을 받으면 손님 단추가 동작한다. 같은 브라우저는 한 번만.
    maru(
        &data,
        &[
            "module",
            "set",
            "reaction",
            "--field",
            "kinds=up,down,heart",
            "--field",
            "guest=true",
        ],
    );
    let bad = run(
        &data,
        &["module", "set", "reaction", "--field", "kinds=up,nope"],
    );
    assert_eq!(bad["error"]["code"], "VALIDATION", "{bad}");
    assert!(http(&bind, "GET", &url, &[], "").body.contains("좋아요"));
    let guest = |kind: &str| {
        http(
            &bind,
            "POST",
            &action,
            &[
                FORM,
                ("Sec-Fetch-Site", "same-origin"),
                ("User-Agent", "시험 브라우저"),
            ],
            &form(&[("kind", kind)]),
        )
    };
    assert_eq!(guest("heart").status, 303);
    assert_eq!(counts(&post_id)[2]["count"], 1);
    guest("heart");
    assert_eq!(counts(&post_id)[2]["count"], 0, "손님도 다시 누르면 취소");
    guest("heart");

    // 댓글 반응.
    let comment_page = http(&bind, "GET", &url, &as_reader, "");
    let comment_action = {
        let b = &comment_page.body;
        let at = b
            .find("method=\"post\" action=\"")
            .map(|_| b.find("/comments\"").unwrap())
            .unwrap();
        let start = b[..at].rfind('"').unwrap() + 1;
        format!("{}/comments", &b[start..at])
    };
    let res = http(
        &bind,
        "POST",
        &comment_action,
        &[FORM, ("Cookie", &author), ("Sec-Fetch-Site", "same-origin")],
        &form(&[("body", "첫 댓글입니다")]),
    );
    assert_eq!(res.status, 303, "{}", res.body);
    let page = http(&bind, "GET", &url, &as_reader, "").body;
    let at = page.find("action=\"/m/reaction/comment/").unwrap() + 8;
    let c_action = page[at..at + page[at..].find('"').unwrap()].to_owned();
    let comment_id = c_action.rsplit('/').next().unwrap().to_owned();
    let res = http(
        &bind,
        "POST",
        &c_action,
        &[FORM, ("Cookie", &reader), ("Sec-Fetch-Site", "same-origin")],
        &form(&[("kind", "up")]),
    );
    assert_eq!(res.status, 303, "{}", res.body);
    assert_eq!(res.location(), format!("{url}#comment-{comment_id}"));
    let c = maru(
        &data,
        &[
            "reaction",
            "counts",
            "--target-type",
            "comment",
            "--target-id",
            &comment_id,
        ],
    );
    assert_eq!(c["kinds"][0]["count"], 1);
    // 댓글 반응을 끄면 단추가 사라진다.
    maru(
        &data,
        &["module", "set", "reaction", "--field", "comments=false"],
    );
    assert!(
        !http(&bind, "GET", &url, &[], "")
            .body
            .contains("/m/reaction/comment/")
    );

    // 스크랩: 손님은 로그인으로, 회원은 넣고 빼기, 내 정보·모음 화면.
    let scrap = format!("/m/scrap/post/{post_id}");
    let res = http(
        &bind,
        "POST",
        &scrap,
        &[FORM, ("Sec-Fetch-Site", "same-origin")],
        "",
    );
    assert_eq!(res.status, 303);
    assert!(res.location().starts_with("/login?next="));
    assert_eq!(http(&bind, "GET", "/m/scrap", &[], "").status, 303);
    let hit = |extra: &str| {
        http(
            &bind,
            "POST",
            &scrap,
            &[FORM, ("Cookie", &reader), ("Sec-Fetch-Site", "same-origin")],
            extra,
        )
    };
    let res = hit("");
    assert_eq!(res.status, 303, "{}", res.body);
    assert!(
        http(&bind, "GET", &url, &as_reader, "")
            .body
            .contains("스크랩함")
    );
    assert!(
        !http(&bind, "GET", &url, &[], "").body.contains("스크랩함"),
        "손님에게는 안 눌린 모양"
    );
    let me = http(&bind, "GET", "/me", &as_reader, "").body;
    assert!(
        me.contains("반응 받을 글") && me.contains("id=\"me-scrap\""),
        "{me}"
    );
    let list = http(&bind, "GET", "/m/scrap", &as_reader, "");
    assert_eq!(list.status, 200);
    assert!(
        list.body.contains("반응 받을 글") && list.body.contains("내 스크랩"),
        "{}",
        list.body
    );
    assert!(list.body.contains("noindex"), "개인 화면은 색인하지 않는다");
    let res = hit("from=list&page=1");
    assert_eq!(res.location(), "/m/scrap?page=1");
    assert!(
        http(&bind, "GET", "/m/scrap", &as_reader, "")
            .body
            .contains("아직 스크랩한")
    );
    hit("");

    // 게시판 즐겨찾기 (GAP Q26, ADR-0108): 손님은 로그인으로, 회원은 JSON 으로 넣고 빼기, /me/bar 에 보인다.
    let star = |cookie: Option<&str>, accept: &str, site: &str, path: &str| {
        let mut h = vec![("Accept", accept), ("Sec-Fetch-Site", site)];
        if let Some(c) = cookie {
            h.push(("Cookie", c));
        }
        http(&bind, "POST", path, &h, "")
    };
    let res = star(None, "text/html", "same-origin", "/m/scrap/board/free");
    assert_eq!(res.status, 303);
    assert!(res.location().starts_with("/login?next=") && res.location().ends_with("free"));
    assert_eq!(
        star(
            Some(&reader),
            "application/json",
            "cross-site",
            "/m/scrap/board/free"
        )
        .status,
        403
    );
    assert_eq!(
        star(
            Some(&reader),
            "application/json",
            "same-origin",
            "/m/scrap/board/nope"
        )
        .status,
        404
    );
    let res = star(
        Some(&reader),
        "application/json",
        "same-origin",
        "/m/scrap/board/free",
    );
    assert_eq!(res.status, 200, "{}", res.body);
    assert!(res.body.contains("\"favorite\":true"), "{}", res.body);
    let bar = http(&bind, "GET", "/me/bar", &as_reader, "").body;
    let bar: Value = serde_json::from_str(&bar).unwrap();
    assert_eq!(bar["board_star"], "/m/scrap/board/");
    assert_eq!(
        bar["favorite_boards"],
        serde_json::json!([{ "name": "자유", "url": "/b/free" }])
    );
    assert!(
        http(&bind, "GET", "/b/free", &[], "")
            .body
            .contains("data-board=\"free\"")
    );
    let res = star(
        Some(&reader),
        "text/html",
        "same-origin",
        "/m/scrap/board/free",
    );
    assert_eq!(res.status, 303);
    assert_eq!(res.location(), "/b/free");
    let bar = http(&bind, "GET", "/me/bar", &as_reader, "").body;
    assert!(bar.contains("\"favorite_boards\":[]"), "{bar}");
    star(
        Some(&reader),
        "application/json",
        "same-origin",
        "/m/scrap/board/free",
    );

    // 감사 기록.
    let audit = maru(&data, &["audit", "list", "--action", "reaction"]);
    assert!(audit["total"].as_i64().unwrap() >= 5, "{audit}");
    assert!(
        maru(&data, &["audit", "list", "--action", "scrap"])["total"]
            .as_i64()
            .unwrap()
            >= 3
    );

    // 끄면 단추가 사라지고 주소가 없어지며 글·내 정보는 그대로다. 다시 켜면 그대로 있다.
    maru(&data, &["module", "disable", "reaction"]);
    maru(&data, &["module", "disable", "scrap"]);
    let page = http(&bind, "GET", &url, &as_reader, "");
    assert_eq!(page.status, 200);
    assert!(
        !page.body.contains("react-bar") && !page.body.contains("slot-row"),
        "{}",
        page.body
    );
    assert_eq!(http(&bind, "GET", "/me", &as_reader, "").status, 200);
    assert!(
        !http(&bind, "GET", "/me", &as_reader, "")
            .body
            .contains("me-scrap")
    );
    assert_eq!(hit("").status, 404);
    maru(&data, &["module", "enable", "reaction"]);
    maru(&data, &["module", "enable", "scrap"]);
    assert_eq!(counts(&post_id)[0]["count"], 1);
    assert!(
        http(&bind, "GET", &url, &as_reader, "")
            .body
            .contains("스크랩함")
    );

    // 탈퇴하면 반응과 모음이 함께 사라진다. 다음 배달에서 처리한다.
    let me = http(&bind, "GET", "/me", &as_reader, "");
    let at = me.body.find("action=\"/me/withdraw\"").unwrap();
    let csrf_at = me.body[at..].find("name=\"_csrf\" value=\"").unwrap() + at + 20;
    let csrf = me.body[csrf_at..csrf_at + me.body[csrf_at..].find('"').unwrap()].to_owned();
    let res = http(
        &bind,
        "POST",
        "/me/withdraw",
        &[FORM, ("Cookie", &reader)],
        &form(&[("_csrf", &csrf), ("password", PASSWORD), ("agree", "yes")]),
    );
    assert_eq!(res.status, 200, "{}", res.body);
    assert!(res.body.contains("탈퇴했습니다"), "{}", res.body);
    let start = std::time::Instant::now();
    while counts(&post_id)[0]["count"] != 0 {
        assert!(start.elapsed().as_secs() < 15, "탈퇴 뒤에도 반응이 남았다");
        std::thread::sleep(std::time::Duration::from_millis(200));
    }

    // 제거해도 코어 글은 그대로다.
    let refused = run(&data, &["module", "remove", "reaction"]);
    assert_eq!(refused["error"]["code"], "VALIDATION");
    maru(&data, &["module", "remove", "reaction", "--purge", "--yes"]);
    maru(&data, &["module", "remove", "scrap", "--purge", "--yes"]);
    let page = http(&bind, "GET", &url, &[], "");
    assert_eq!(page.status, 200);
    assert!(page.body.contains("반응 받을 글"));
    drop(server);
}
