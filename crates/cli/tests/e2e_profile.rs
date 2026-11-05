//! GAP Q2: 회원 공개 프로필과 글쓴이 카드 (ADR-0087). 실제 바이너리·실제 포트·실제 DB 파일로
//! 자기소개 저장 → `/u/닉네임`(손님 기준 공개 글만) → 글쓴이 카드 → REST·CLI → 비활성·탈퇴를 확인한다.
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
fn write_post(bind: &str, cookie: &str, board: &str, title: &str) -> String {
    let page = http(
        bind,
        "GET",
        &format!("/write/{board}"),
        &[("Cookie", cookie)],
        "",
    );
    assert_eq!(page.status, 200, "{}", page.body);
    let res = http(
        bind,
        "POST",
        &format!("/write/{board}"),
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

fn comment(bind: &str, cookie: &str, post_url: &str, body: &str) {
    let res = http(
        bind,
        "POST",
        &format!("{post_url}/comments"),
        &[FORM, ("Cookie", cookie), ("Sec-Fetch-Site", "same-origin")],
        &form(&[("body", body)]),
    );
    assert_eq!(res.status, 303, "{}", res.body);
}

/// `/me` 의 닉네임·자기소개 폼을 보낸다.
fn save_profile(bind: &str, cookie: &str, name: &str, bio: &str) -> Res {
    let me = http(bind, "GET", "/me", &[("Cookie", cookie)], "");
    assert_eq!(me.status, 200, "{}", me.body);
    http(
        bind,
        "POST",
        "/me/profile",
        &[FORM, ("Cookie", cookie)],
        &form(&[("_csrf", &me.csrf()), ("display_name", name), ("bio", bio)]),
    )
}

#[test]
fn public_profile_and_author_card() {
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
    for (slug, name) in [("free", "자유"), ("vip", "회원방")] {
        maru(
            &data,
            &["board", "create", slug, "--name", name, "--kind", "free"],
        );
        maru(&data, &["board", "update", slug, "--write-approval", "off"]);
    }
    maru(&data, &["board", "update", "vip", "--read", "member"]);
    let (_server, bind) = start_server(&data);

    let moon = format!(
        "maru_session={}",
        signup_and_login(&bind, "moon@example.test", "달빛이")
    );
    let sun = format!(
        "maru_session={}",
        signup_and_login(&bind, "sun@example.test", "햇살이")
    );
    let open_url = write_post(&bind, &moon, "free", "모두가 보는 글");
    let vip_url = write_post(&bind, &moon, "vip", "회원만 보는 글");
    comment(&bind, &sun, &open_url, "공개 글에 단 댓글");
    comment(&bind, &sun, &vip_url, "회원방에 단 댓글");

    // 자기소개: 줄바꿈은 살리고 HTML 은 글자로.
    let saved = save_profile(&bind, &moon, "달빛이", "  안녕하세요\r\n<b>굵게</b>  ");
    assert_eq!(saved.status, 303, "{}", saved.body);
    let too_long = save_profile(&bind, &moon, "달빛이", &"가".repeat(301));
    assert_eq!(too_long.status, 400);
    assert!(too_long.body.contains("300자 이하"), "{}", too_long.body);

    let moon_path = format!("/u/{}", enc("달빛이"));
    let page = http(&bind, "GET", &moon_path, &[], "");
    assert_eq!(page.status, 200, "{}", page.body);
    let body = &page.body;
    assert!(body.contains("<h1 id=\"profile-title\">달빛이"), "{body}");
    assert!(
        body.contains("안녕하세요\n&lt;b&gt;굵게&lt;/b&gt;"),
        "{body}"
    );
    assert!(body.contains("모두가 보는 글"), "{body}");
    assert!(
        !body.contains("회원만 보는 글"),
        "손님 기준: 회원방 글은 안 보인다"
    );
    assert!(body.contains("글 1 · 댓글 0"), "{body}");
    assert!(!body.contains("moon@example.test"), "이메일은 없다");
    assert!(body.contains("noindex"), "검색 엔진에 내보내지 않는다");

    let page = http(&bind, "GET", &format!("/u/{}", enc("햇살이")), &[], "").body;
    assert!(page.contains("공개 글에 단 댓글"), "{page}");
    assert!(!page.contains("회원방에 단 댓글"), "{page}");
    assert!(page.contains(&format!("{open_url}#comment-")), "{page}");

    // 글 아래 글쓴이 카드.
    let post = http(&bind, "GET", &open_url, &[], "").body;
    let card = &post[post.find("class=\"author-card\"").expect("글쓴이 카드")..];
    assert!(card.contains(&format!("href=\"{moon_path}\"")), "{card}");
    assert!(
        card.contains("&lt;b&gt;굵게") && card.contains("프로필 보기"),
        "{card}"
    );

    // REST(토큰 없으면 손님 기준)와 CLI(운영자 기준)는 같은 서비스를 쓴다.
    let api = http(
        &bind,
        "GET",
        &format!("/api/v1{}", moon_path.replace("/u/", "/profiles/")),
        &[],
        "",
    );
    assert_eq!(api.status, 200, "{}", api.body);
    let v: Value = serde_json::from_str(&api.body).unwrap();
    assert_eq!(v["data"]["post_count"], 1);
    assert_eq!(v["data"]["untrusted"], true);
    assert!(v["data"].get("email").is_none());
    let cli = maru(&data, &["member", "profile", "달빛이"]);
    assert_eq!(cli["post_count"], 2, "운영자는 회원방 글도 센다");
    assert_eq!(cli["bio"], "안녕하세요\n<b>굵게</b>");
    let missing = run(&data, &["member", "profile", "없는사람"]);
    assert_eq!(missing["error"]["code"], "NOT_FOUND");
    assert_eq!(http(&bind, "GET", "/u/없는사람", &[], "").status, 404);

    // 닉네임을 바꾸면 새 주소로.
    assert_eq!(
        save_profile(&bind, &moon, "달그림자", "안녕하세요").status,
        303
    );
    assert_eq!(http(&bind, "GET", &moon_path, &[], "").status, 404);
    assert_eq!(
        http(&bind, "GET", &format!("/u/{}", enc("달그림자")), &[], "").status,
        200
    );

    // 비활성 회원은 프로필도 카드도 없다.
    maru(&data, &["member", "disable", "moon@example.test"]);
    assert_eq!(
        http(&bind, "GET", &format!("/u/{}", enc("달그림자")), &[], "").status,
        404
    );
    let post = http(&bind, "GET", &open_url, &[], "").body;
    assert!(!post.contains("class=\"author-card\""), "{post}");
    maru(&data, &["member", "enable", "moon@example.test"]);

    // 탈퇴하면 자기소개가 지워진다.
    save_profile(&bind, &sun, "햇살이", "햇살 소개");
    assert_eq!(
        maru(&data, &["member", "profile", "햇살이"])["bio"],
        "햇살 소개"
    );
    let me = http(&bind, "GET", "/me", &[("Cookie", &sun)], "").csrf();
    let out = http(
        &bind,
        "POST",
        "/me/withdraw",
        &[FORM, ("Cookie", &sun)],
        &form(&[("_csrf", &me), ("password", PASSWORD), ("agree", "yes")]),
    );
    assert!(
        out.status == 200 && out.body.contains("탈퇴했습니다"),
        "{}",
        out.body
    );
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let left: i64 = rt.block_on(async {
        let db = maru_storage::Db::open(&data.join("maru.db"), false)
            .await
            .unwrap();
        let n = sqlx::query_scalar("SELECT count(*) FROM member_profiles")
            .fetch_one(db.read_pool())
            .await
            .unwrap();
        db.close().await;
        n
    });
    assert_eq!(
        left, 1,
        "탈퇴한 회원의 자기소개 줄은 지워지고 달그림자 것만 남는다"
    );
    assert_eq!(
        http(&bind, "GET", &format!("/u/{}", enc("햇살이")), &[], "").status,
        404
    );
}
