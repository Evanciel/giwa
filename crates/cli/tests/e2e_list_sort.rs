//! GAP Q7: 목록 순서(최신·조회·댓글)와 검색 범위(댓글·글쓴이) (ADR-0092). 실제 바이너리·실제 포트·실제 DB 파일로
//! 회원이 쓴 글·댓글을 순서대로 늘어놓고, 댓글 내용과 글쓴이 이름으로 찾는다.
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

/// 목록 HTML 의 글 주소 조각(순서대로). 곁 위젯이 없는 블로그 기본 화면이다.
fn listed(html: &str) -> Vec<String> {
    let at = html.find("class=\"post-table\"").expect("목록 표");
    let table = &html[at..at + html[at..].find("</ul>").unwrap()];
    let mut out: Vec<String> = Vec::new();
    for (i, m) in table.match_indices("href=\"/b/free/") {
        let rest = &table[i + m.len()..];
        let slug = rest[..rest.find('"').unwrap()].to_owned();
        if !slug.contains('#') && !out.contains(&slug) {
            out.push(slug);
        }
    }
    out
}

#[test]
fn list_sort_and_comment_author_search() {
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
            "--posts-per-page",
            "2",
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
        &[
            "board",
            "update",
            "free",
            "--write-approval",
            "off",
            "--layout",
            "list",
        ],
    );
    let (_server, bind) = start_server(&data);

    let author = format!(
        "maru_session={}",
        signup_and_login(&bind, "moon@example.test", "달빛이")
    );
    let reader = format!(
        "maru_session={}",
        signup_and_login(&bind, "sun@example.test", "햇살이")
    );
    let a = write_post(&bind, &author, "첫째 글");
    let b = write_post(&bind, &author, "둘째 글");
    let c = write_post(&bind, &author, "셋째 글");
    let slug = |url: &str| url.rsplit('/').next().unwrap().to_owned();
    let (sa, sb, sc) = (slug(&a), slug(&b), slug(&c));
    comment(&bind, &reader, &a, "고양이 사진 좋네요");
    comment(&bind, &reader, &a, "두 번째 댓글");
    comment(&bind, &reader, &b, "강아지도 있어요 100%");

    // 조회수는 운영자가 정한 값으로 (B 가 가장 많이)
    let rest = http(
        &bind,
        "GET",
        "/api/v1/boards/free/posts?per_page=10",
        &[],
        "",
    );
    let v: Value = serde_json::from_str(&rest.body).unwrap();
    let id_of = |s: &str| {
        v["data"]["posts"]["items"]
            .as_array()
            .unwrap()
            .iter()
            .find(|p| p["title"] == s)
            .unwrap()["id"]
            .as_str()
            .unwrap()
            .to_owned()
    };
    maru(&data, &["post", "views", &id_of("둘째 글"), "--set", "50"]);
    maru(&data, &["post", "views", &id_of("셋째 글"), "--set", "7"]);

    // 기본은 최신부터, 순서 단추 셋
    let page = http(&bind, "GET", "/b/free", &[], "").body;
    assert_eq!(listed(&page), [sc.clone(), sb.clone()], "{page}");
    assert!(page.contains("aria-label=\"목록 순서\""), "{page}");
    assert!(page.contains("aria-current=\"page\">최신</a>"), "{page}");
    // 댓글 많은 순: A(2) B(1), 쪽 넘김은 순서를 유지
    let page = http(&bind, "GET", "/b/free?order=comments", &[], "").body;
    assert_eq!(listed(&page), [sa.clone(), sb.clone()], "{page}");
    assert!(page.contains("aria-current=\"page\">댓글</a>"), "{page}");
    assert!(page.contains("order=comments&amp;page=2"), "{page}");
    // 조회 많은 순: B(50) C(7) A(0)
    let page = http(&bind, "GET", "/b/free?order=views", &[], "").body;
    assert_eq!(listed(&page), [sb.clone(), sc.clone()], "{page}");
    let page = http(&bind, "GET", "/b/free?order=views&page=2", &[], "").body;
    assert_eq!(listed(&page), [sa.as_str()], "{page}");
    // 순위 번호: 조회 순 2쪽 첫 줄은 3위, 처음·이전 단추 (GAP Q16)
    assert!(
        page.contains("class=\"rank rank-top\" title=\"3위\">3</span>"),
        "{page}"
    );
    assert!(page.contains(">처음</a>"), "{page}");
    assert!(!page.contains(">마지막</a>"), "{page}");
    let page = http(&bind, "GET", "/b/free?order=views", &[], "").body;
    assert!(
        page.contains("class=\"rank rank-top\" title=\"1위\">1</span>"),
        "{page}"
    );
    // 쪽 번호 묶음: 지금 쪽은 링크 아님, 2쪽 링크와 마지막
    assert!(
        page.contains("<span class=\"visually-hidden\">현재 </span>1</span>"),
        "{page}"
    );
    assert!(
        page.contains("href=\"/b/free?order=views&amp;page=2\" aria-label=\"2쪽\""),
        "{page}"
    );
    assert!(page.contains(">마지막</a>"), "{page}");
    assert!(!page.contains(">처음</a>"), "{page}");
    // 최신 순은 번호, 순서 줄은 목록 아래, 쪽 번호와 검색은 한 묶음
    let page = http(&bind, "GET", "/b/free", &[], "").body;
    assert!(!page.contains("class=\"rank"), "{page}");
    let table = page.find("class=\"post-table\"").unwrap();
    let foot = page.find("class=\"list-foot\"").expect("목록 아래 줄");
    let pager = page.find("class=\"list-pager\"").expect("쪽 번호 묶음");
    assert!(table < foot && foot < pager, "{page}");
    assert!(
        page[foot..pager].contains("aria-label=\"목록 순서\""),
        "{page}"
    );
    assert!(page[pager..].contains("aria-label=\"페이지\""), "{page}");
    assert!(page[pager..].contains("class=\"board-search\""), "{page}");
    // 휴대폰 머리 정렬 선택 (GAP Q28, ADR-0111): 지금 순서가 접힌 제목, 펼치면 세 순서
    let head = &page[page.find("class=\"board-head\"").unwrap()..table];
    assert!(head.contains("<details class=\"head-sort\">"), "{head}");
    assert!(
        head.contains("<span class=\"visually-hidden\">목록 순서: </span>최신 순</summary>"),
        "{head}"
    );
    assert!(head.contains("aria-current=\"page\">최신 순</a>"), "{head}");
    // 모르는 순서는 최신으로
    let page = http(&bind, "GET", "/b/free?order=random", &[], "").body;
    assert_eq!(listed(&page), [sc.clone(), sb.clone()], "{page}");
    // REST 도 같은 서비스
    let rest = http(
        &bind,
        "GET",
        "/api/v1/boards/free/posts?order=comments",
        &[],
        "",
    );
    let v: Value = serde_json::from_str(&rest.body).unwrap();
    assert_eq!(v["data"]["sort"], "comments", "{v}");
    assert_eq!(v["data"]["posts"]["items"][0]["title"], "첫째 글", "{v}");

    // 댓글 검색: 맞은 댓글 토막을 보인다
    let page = http(
        &bind,
        "GET",
        &format!("/search?q={}&in=comment", enc("고양이")),
        &[],
        "",
    )
    .body;
    assert!(page.contains("검색 결과 1건"), "{page}");
    assert!(page.contains(&format!("href=\"/b/free/{sa}\"")), "{page}");
    assert!(page.contains("<mark>고양이</mark>"), "{page}");
    assert!(
        page.contains("<option value=\"comment\" selected>댓글</option>"),
        "{page}"
    );
    // %·_ 는 글자 그대로
    let page = http(
        &bind,
        "GET",
        &format!("/search?q={}&in=comment", enc("100%")),
        &[],
        "",
    )
    .body;
    assert!(page.contains("검색 결과 1건"), "{page}");
    let page = http(
        &bind,
        "GET",
        &format!("/search?q={}&in=comment", enc("0%")),
        &[],
        "",
    )
    .body;
    assert!(page.contains("검색 결과 1건"), "{page}");
    let page = http(
        &bind,
        "GET",
        &format!("/search?q={}&in=comment", enc("_")),
        &[],
        "",
    )
    .body;
    assert!(
        !page.contains("검색 결과 1건") && !page.contains("검색 결과 3건"),
        "{page}"
    );
    // 게시판 안 댓글 검색
    let page = http(
        &bind,
        "GET",
        &format!("/b/free?q={}&in=comment", enc("강아지")),
        &[],
        "",
    )
    .body;
    assert!(page.contains(&format!("href=\"/b/free/{sb}\"")), "{page}");
    assert!(!page.contains(&format!("href=\"/b/free/{sa}\"")), "{page}");

    // 글쓴이 검색: 닉네임 일부로, 댓글 쓴 사람은 글쓴이가 아니다
    let page = http(
        &bind,
        "GET",
        &format!("/search?q={}&in=author", enc("달빛")),
        &[],
        "",
    )
    .body;
    assert!(page.contains("검색 결과 3건"), "{page}");
    let page = http(
        &bind,
        "GET",
        &format!("/search?q={}&in=author", enc("햇살이")),
        &[],
        "",
    )
    .body;
    assert!(page.contains("검색 결과 0건"), "{page}");
    assert!(
        page.contains("글자 그대로"),
        "글자 그대로 찾는다는 안내: {page}"
    );
    // REST 검색도 같은 서비스, 모르는 곳은 이유와 함께 거부
    let rest = http(
        &bind,
        "GET",
        &format!("/api/v1/search?q={}&in=author", enc("달빛이")),
        &[],
        "",
    );
    let v: Value = serde_json::from_str(&rest.body).unwrap();
    assert_eq!(v["data"]["total"], 3, "{v}");
    let rest = http(&bind, "GET", "/api/v1/search?q=x&in=everywhere", &[], "");
    assert_eq!(rest.status, 400, "{}", rest.body);
    assert!(rest.body.contains("comment, author"), "{}", rest.body);
}
