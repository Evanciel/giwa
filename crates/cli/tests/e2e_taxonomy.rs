//! GAP C1~C4: 분류·태그·공지·비밀글. CLI 와 REST 가 같은 서비스를 거쳐 같은 답을 주는지,
//! 비밀글 본문이 손님에게 새지 않는지, 권한이 없을 때 원인과 다음 명령을 주는지 실제 바이너리로 본다.
#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::path::Path;
use std::process::{Child, Command, Output, Stdio};

use serde_json::Value;

const BIN: &str = env!("CARGO_BIN_EXE_giwa");

/// 테스트가 실패해도 서버 프로세스를 남기지 않는다.
struct Server(Child);

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn run(data: &Path, args: &[&str], token: Option<&str>) -> Value {
    let mut cmd = Command::new(BIN);
    cmd.arg("--data").arg(data).args(args).arg("--json");
    if let Some(t) = token {
        cmd.env("GIWA_TOKEN", t);
    }
    let out: Output = cmd.output().expect("giwa 실행");
    let stdout = String::from_utf8_lossy(&out.stdout);
    serde_json::from_str(stdout.trim()).unwrap_or_else(|e| {
        panic!(
            "JSON 아님: {e}\nstdout: {stdout}\nstderr: {}",
            String::from_utf8_lossy(&out.stderr)
        )
    })
}

fn giwa(data: &Path, args: &[&str]) -> Value {
    let v = run(data, args, None);
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
    let bind = v["data"]["bind"].as_str().unwrap().to_owned();
    (server, bind)
}

/// 최소 HTTP/1.1 클라이언트. 응답 상태와 본문(JSON)을 돌려준다.
fn http(bind: &str, method: &str, path: &str, token: Option<&str>, body: &str) -> (u16, Value) {
    let (status, body) = raw(bind, method, path, token, body);
    let v = serde_json::from_str(&body).unwrap_or_else(|e| panic!("JSON 아님: {e}\n{body}"));
    (status, v)
}

/// 공개 화면(HTML·XML)을 손님으로 읽는다.
fn html(bind: &str, path: &str) -> (u16, String) {
    raw(bind, "GET", path, None, "")
}

fn raw(bind: &str, method: &str, path: &str, token: Option<&str>, body: &str) -> (u16, String) {
    let mut s = TcpStream::connect(bind).unwrap();
    let mut req = format!("{method} {path} HTTP/1.1\r\nHost: {bind}\r\nConnection: close\r\n");
    if let Some(t) = token {
        req.push_str(&format!("Authorization: Bearer {t}\r\n"));
    }
    if !body.is_empty() {
        req.push_str("Content-Type: application/json\r\n");
    }
    req.push_str(&format!("Content-Length: {}\r\n\r\n{body}", body.len()));
    s.write_all(req.as_bytes()).unwrap();
    let mut raw = Vec::new();
    s.read_to_end(&mut raw).unwrap();
    let text = String::from_utf8(raw).unwrap();
    let status: u16 = text[9..12].parse().unwrap();
    let body = text
        .split_once("\r\n\r\n")
        .map(|(_, b)| b.to_owned())
        .unwrap_or_default();
    (status, body)
}

fn encode(segment: &str) -> String {
    segment
        .bytes()
        .map(|b| {
            if b.is_ascii_alphanumeric() || b == b'-' {
                (b as char).to_string()
            } else {
                format!("%{b:02X}")
            }
        })
        .collect()
}

#[test]
fn taxonomy_cli_and_rest_agree_and_secret_body_stays_private() {
    let tmp = tempfile::tempdir().unwrap();
    let data = tmp.path().join("site");
    let init = giwa(
        &data,
        &["init", "--no-samples", "--preset", "blog", "--yes"],
    );
    let token = init["token"]["secret"].as_str().unwrap().to_owned();

    // 분류: 두 번 만들어도 하나 (멱등)
    let travel = giwa(&data, &["category", "add", "--board", "blog", "제주 여행"]);
    assert_eq!(travel["slug"], "제주-여행");
    let again = giwa(&data, &["category", "add", "--board", "blog", "제주 여행"]);
    assert_eq!(again["id"], travel["id"]);
    giwa(&data, &["category", "add", "--board", "blog", "맛집"]);
    let moved = giwa(
        &data,
        &[
            "category", "move", "--board", "blog", "맛집", "--order", "-1",
        ],
    );
    assert_eq!(moved["sort_order"], -1);

    // 기본(글쓰기) 토큰은 분류를 만들 수 없다: 원인 코드가 있다.
    let denied = run(
        &data,
        &["category", "add", "--board", "blog", "몰래"],
        Some(&token),
    );
    assert_eq!(denied["ok"], false);
    assert_eq!(denied["error"]["code"], "FORBIDDEN", "{denied}");
    // 없는 분류로 글을 쓰면 NOT_FOUND 와 목록 명령
    let missing = run(
        &data,
        &["post", "new", "--title", "글", "--category", "없음"],
        None,
    );
    assert_eq!(missing["error"]["code"], "NOT_FOUND", "{missing}");
    let next = missing["error"]["next"].as_array().unwrap();
    assert!(
        next.iter()
            .any(|c| c.as_str().unwrap().contains("giwa category list")),
        "{missing}"
    );

    // 글: 분류·태그 / 공지 / 비밀
    let trip = giwa(
        &data,
        &[
            "post",
            "new",
            "--title",
            "한라산 등반",
            "--body",
            "정상까지 다녀왔습니다.",
            "--category",
            "제주-여행",
            "--tag",
            "#등산",
            "--tag",
            "제주",
            "--publish",
        ],
    );
    assert_eq!(trip["category_id"], travel["id"]);
    assert_eq!(trip["tags"], serde_json::json!(["등산", "제주"]));
    let notice = giwa(
        &data,
        &[
            "post",
            "new",
            "--title",
            "공지입니다",
            "--body",
            "읽어 주세요.",
            "--notice",
            "--publish",
        ],
    );
    assert_eq!(notice["is_notice"], true);
    let secret = giwa(
        &data,
        &[
            "post",
            "new",
            "--title",
            "비밀 상담",
            "--body",
            "아무도 몰라야 하는 본문",
            "--tag",
            "제주",
            "--secret",
            "--publish",
        ],
    );
    assert_eq!(secret["is_secret"], true);
    let secret_id = secret["id"].as_str().unwrap().to_owned();

    let (_server, bind) = start_server(&data);

    // 게시판 목록 (손님): 공지는 위에 따로, 비밀글은 제목만
    let (status, res) = http(&bind, "GET", "/api/v1/boards/blog/posts", None, "");
    assert_eq!(status, 200, "{res}");
    let page = &res["data"];
    assert_eq!(page["notices"].as_array().unwrap().len(), 1);
    assert_eq!(page["notices"][0]["title"], "공지입니다");
    assert_eq!(page["posts"]["total"], 2, "{page}");
    assert_eq!(page["categories"].as_array().unwrap().len(), 2);
    let items = page["posts"]["items"].as_array().unwrap();
    let hidden = items
        .iter()
        .find(|p| p["id"] == secret_id.as_str())
        .unwrap();
    assert_eq!(hidden["title"], "비밀 상담");
    assert_eq!(hidden["body_md"], "");
    assert!(
        !res.to_string().contains("아무도 몰라야"),
        "목록에 비밀 본문이 샌다"
    );

    // 분류로 거르기
    let path = format!("/api/v1/boards/blog/posts?category={}", encode("제주-여행"));
    let (status, res) = http(&bind, "GET", &path, None, "");
    assert_eq!(status, 200, "{res}");
    assert_eq!(res["data"]["posts"]["total"], 1);
    assert_eq!(res["data"]["category"]["name"], "제주 여행");

    // 분류 목록: REST 와 CLI 가 같은 답
    let (status, res) = http(&bind, "GET", "/api/v1/boards/blog/categories", None, "");
    assert_eq!(status, 200, "{res}");
    let cli_list = giwa(&data, &["category", "list", "--board", "blog"]);
    assert_eq!(res["data"], cli_list);
    assert_eq!(cli_list[0]["slug"], "맛집", "순서가 작은 것이 앞");

    // 태그 목록: 비밀글은 세지 않는다. REST 와 CLI 가 같은 답
    let (status, res) = http(&bind, "GET", "/api/v1/tags", None, "");
    assert_eq!(status, 200, "{res}");
    let cli_tags = giwa(&data, &["tag", "list"]);
    assert_eq!(res["data"], cli_tags);
    let jeju = cli_tags
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["name"] == "제주")
        .unwrap();
    assert_eq!(jeju["posts"], 1);
    let (status, res) = http(
        &bind,
        "GET",
        &format!("/api/v1/tags/{}", encode("제주")),
        None,
        "",
    );
    assert_eq!(status, 200, "{res}");
    assert_eq!(res["data"]["posts"]["total"], 1);
    assert!(!res.to_string().contains("비밀 상담"));

    // 비밀글 본문: 손님·읽기 토큰은 403 과 안내, 운영자(CLI)는 읽는다
    let (status, res) = http(
        &bind,
        "GET",
        &format!("/api/v1/posts/{secret_id}"),
        None,
        "",
    );
    assert_eq!(status, 403, "{res}");
    assert_eq!(res["error"]["code"], "FORBIDDEN");
    assert!(res["error"]["hint"].is_string(), "{res}");
    assert!(!res.to_string().contains("아무도 몰라야"));
    let shown = giwa(&data, &["post", "show", &secret_id]);
    assert_eq!(shown["body_md"], "아무도 몰라야 하는 본문");

    // 공개 화면: 어느 경로로도 비밀 본문이 새지 않는다
    let secret_path = format!("/b/blog/{}", encode(secret["slug"].as_str().unwrap()));
    let (status, page) = html(&bind, &secret_path);
    assert_eq!(status, 403, "{page}");
    assert!(page.contains("비밀글입니다"), "{page}");
    assert!(page.contains("noindex"));
    let (status, board) = html(&bind, "/b/blog");
    assert_eq!(status, 200);
    assert!(board.contains("class=\"notice-list\"") && board.contains("공지입니다"));
    assert!(board.contains("class=\"lock\"") && board.contains("비밀 상담"));
    assert!(board.contains("?category="), "분류 탭");
    let cat_path = format!("/b/blog?category={}", encode("제주-여행"));
    let (status, filtered) = html(&bind, &cat_path);
    assert_eq!(status, 200);
    assert!(filtered.contains("한라산 등반") && !filtered.contains("비밀 상담"));
    assert!(filtered.contains("aria-current=\"page\">제주 여행"));
    let (status, _) = html(&bind, "/b/blog?category=없음");
    assert_eq!(status, 404);
    let trip_path = format!("/b/blog/{}", encode(trip["slug"].as_str().unwrap()));
    let (_, trip_page) = html(&bind, &trip_path);
    assert!(trip_page.contains("rel=\"tag\">#등산"), "글 아래 태그");
    assert!(trip_page.contains(&format!("/tags/{}", encode("등산"))));
    assert!(trip_page.contains(&cat_path), "분류 경로");
    let (status, tags_page) = html(&bind, "/tags");
    assert_eq!(status, 200);
    assert!(tags_page.contains("#제주"));
    let (status, tag_page) = html(&bind, &format!("/tags/{}", encode("제주")));
    assert_eq!(status, 200);
    assert!(tag_page.contains("한라산 등반") && !tag_page.contains("비밀 상담"));
    let (status, _) = html(&bind, "/tags/없는-태그");
    assert_eq!(status, 404);
    for feed in ["/rss.xml", "/sitemap.xml"] {
        let (status, xml) = html(&bind, feed);
        assert_eq!(status, 200);
        assert!(xml.contains(&trip_path), "{feed}");
        assert!(!xml.contains(&secret_path), "{feed} 에 비밀글");
        assert!(!xml.contains("비밀"), "{feed} 에 비밀글");
    }
    for (path, text) in [("/", "아무도 몰라야"), ("/b/blog", "아무도 몰라야")] {
        assert!(!html(&bind, path).1.contains(text), "{path}");
    }

    // REST 쓰기: 기본 토큰은 태그·분류가 붙은 초안은 쓰지만 공지는 못 한다
    let (status, res) = http(
        &bind,
        "POST",
        "/api/v1/posts",
        Some(&token),
        r##"{"board":"blog","title":"에이전트 초안","body_md":"본문","category":"맛집","tags":["#국밥","국밥","부산"]}"##,
    );
    assert_eq!(status, 201, "{res}");
    assert_eq!(res["data"]["tags"], serde_json::json!(["국밥", "부산"]));
    let draft_id = res["data"]["id"].as_str().unwrap().to_owned();
    let (status, res) = http(
        &bind,
        "POST",
        "/api/v1/posts",
        Some(&token),
        r#"{"board":"blog","title":"몰래 공지","body_md":"본문","notice":true}"#,
    );
    assert_eq!(status, 403, "{res}");
    let (status, res) = http(
        &bind,
        "POST",
        "/api/v1/boards/blog/categories",
        Some(&token),
        r#"{"name":"몰래"}"#,
    );
    assert_eq!(status, 403, "{res}");

    // CLI 로 고치기: 분류 비우기·태그 모두 빼기·공지 해제
    let edited = giwa(
        &data,
        &["post", "edit", &draft_id, "--category=", "--clear-tags"],
    );
    assert!(edited["category_id"].is_null(), "{edited}");
    assert_eq!(edited["tags"], serde_json::json!([]));
    let id = notice["id"].as_str().unwrap();
    let edited = giwa(&data, &["post", "edit", id, "--notice", "false"]);
    assert_eq!(edited["is_notice"], false);

    // 분류를 지우면 글은 남고 분류만 빈다
    giwa(
        &data,
        &["category", "remove", "--board", "blog", "제주-여행"],
    );
    let trip_id = trip["id"].as_str().unwrap();
    let shown = giwa(&data, &["post", "show", trip_id]);
    assert!(shown["category_id"].is_null());
}
