//! GAP Q6: 모든 게시판 모아보기 `/recent` (ADR-0091). 실제 바이너리·실제 포트·실제 DB 파일로
//! 손님이 읽을 수 있는 게시판의 새 글·인기 글이 게시판 이름과 함께 모이는지 본다.
#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::path::Path;
use std::process::{Child, Command, Stdio};

use serde_json::Value;

const BIN: &str = env!("CARGO_BIN_EXE_maru");

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

fn get(bind: &str, path: &str) -> (u16, String) {
    let mut s = TcpStream::connect(bind).unwrap();
    let req = format!("GET {path} HTTP/1.1\r\nHost: {bind}\r\nConnection: close\r\n\r\n");
    s.write_all(req.as_bytes()).unwrap();
    let mut raw = Vec::new();
    s.read_to_end(&mut raw).unwrap();
    let text = String::from_utf8_lossy(&raw).into_owned();
    let (head, body) = text.split_once("\r\n\r\n").unwrap_or((&text, ""));
    (head[9..12].parse().unwrap(), body.to_owned())
}

/// 본문 목록 표만 (곁 위젯 목록은 빼고)
fn table(page: &str) -> &str {
    let at = page.find("recent-table").expect("모아보기 표");
    &page[at..at + page[at..].find("</ul>").unwrap()]
}

fn post(data: &Path, board: &str, slug: &str, title: &str) {
    maru(
        data,
        &[
            "post",
            "new",
            "--board",
            board,
            "--title",
            title,
            "--slug",
            slug,
            "--publish",
        ],
    );
}

#[test]
fn recent_collects_every_readable_board() {
    let tmp = tempfile::tempdir().unwrap();
    let data = tmp.path().join("사이트");
    maru(
        &data,
        &["init", "--no-samples", "--preset", "blog", "--yes"],
    );
    maru(&data, &["site", "set", "--posts-per-page", "2"]);
    maru(
        &data,
        &[
            "board", "create", "free", "--name", "자유", "--kind", "free",
        ],
    );
    maru(
        &data,
        &["board", "create", "qna", "--name", "질문", "--kind", "free"],
    );
    maru(
        &data,
        &[
            "board",
            "create",
            "vip",
            "--name",
            "회원방",
            "--kind",
            "free",
        ],
    );
    maru(&data, &["board", "update", "vip", "--read", "member"]);
    post(&data, "free", "f1", "자유 첫 글");
    post(&data, "qna", "q1", "질문 하나");
    post(&data, "vip", "v1", "회원만 읽는 글");
    post(&data, "free", "f2", "자유 둘째 글");
    // 게시판을 고르지 않은 새 글·인기 위젯
    maru(
        &data,
        &[
            "widget",
            "place",
            "latest_posts",
            "--region",
            "sidebar",
            "--title",
            "새 글",
            "--config",
            "{}",
        ],
    );
    maru(
        &data,
        &[
            "widget",
            "place",
            "popular_posts",
            "--region",
            "sidebar",
            "--title",
            "인기",
            "--config",
            "{}",
        ],
    );

    let (_server, bind) = start_server(&data);

    // 새 글: 최신부터 한 쪽에 2개, 게시판 이름 링크, 회원방 글은 없음
    let (status, full) = get(&bind, "/recent");
    assert_eq!(status, 200, "{full}");
    assert!(full.contains("<h1 class=\"page-title\">새 글"), "{full}");
    assert!(full.contains("href=\"/recent?page=2\""), "{full}");
    assert!(full.contains("aria-current=\"page\">새 글</a>"), "{full}");
    assert!(!full.contains("noindex"), "{full}");
    let page = table(&full);
    let f2 = page.find("자유 둘째 글").expect("최신 글");
    let q1 = page.find("질문 하나").expect("다른 게시판 글");
    assert!(f2 < q1, "최신부터");
    assert!(!page.contains("자유 첫 글"), "둘째 쪽으로: {page}");
    assert!(page.contains("<a href=\"/b/qna\">질문</a>"), "{page}");
    assert!(page.contains("<a href=\"/b/free\">자유</a>"), "{page}");
    assert!(!page.contains("회원만 읽는 글"), "{page}");

    let (_, full) = get(&bind, "/recent?page=2");
    let page = table(&full);
    assert!(page.contains("자유 첫 글"), "{page}");
    assert!(!page.contains("회원만 읽는 글"), "{page}");

    // 인기: 최근 7일, 회원방 없음, 검색 엔진엔 내보내지 않는다
    let (status, full) = get(&bind, "/recent?sort=popular");
    assert_eq!(status, 200, "{full}");
    assert!(full.contains("aria-current=\"page\">인기 글</a>"), "{full}");
    assert!(full.contains("최근 7일"), "{full}");
    assert!(full.contains("noindex"), "{full}");
    let page = table(&full);
    assert!(
        page.contains("자유 첫 글") && page.contains("질문 하나"),
        "{page}"
    );
    assert!(!full.contains("회원만 읽는 글"), "{full}");

    // 게시판 없는 위젯의 「더 보기」가 여기로 온다
    let (_, home) = get(&bind, "/");
    assert!(home.contains("href=\"/recent\""), "{home}");
    assert!(home.contains("href=\"/recent?sort=popular\""), "{home}");
}
