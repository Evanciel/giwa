//! GAP Q9: 게시판 안내 문구와 베스트 카드 (ADR-0094). 실제 바이너리·실제 포트·실제 DB 파일로
//! 안내 문구를 넣고 고치고 지우는 것과, 인기 글 위젯의 카드 모양을 본다.
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

#[test]
fn board_description_and_best_cards() {
    let tmp = tempfile::tempdir().unwrap();
    let data = tmp.path().join("사이트");
    maru(&data, &["init", "--preset", "blog", "--yes"]);
    maru(
        &data,
        &[
            "board", "create", "free", "--name", "자유", "--kind", "free",
        ],
    );
    maru(
        &data,
        &[
            "post",
            "new",
            "--board",
            "free",
            "--title",
            "베스트 후보",
            "--slug",
            "best1",
            "--publish",
        ],
    );

    // 안내 문구: 평문, 줄바꿈 유지, 300자까지, 제어 문자 거부
    let b = maru(
        &data,
        &[
            "board",
            "update",
            "free",
            "--description",
            "  자유롭게 이야기해요.\r\n광고는 <b>지웁니다</b>.  ",
        ],
    );
    assert_eq!(
        b["description"], "자유롭게 이야기해요.\n광고는 <b>지웁니다</b>.",
        "{b}"
    );
    let long = "가".repeat(301);
    let bad = run(&data, &["board", "update", "free", "--description", &long]);
    assert_eq!(bad["error"]["code"], "VALIDATION", "{bad}");
    assert!(bad["error"]["message"].to_string().contains("300"), "{bad}");
    let bad = run(
        &data,
        &["board", "update", "free", "--description", "종\u{7}소리"],
    );
    assert_eq!(bad["error"]["code"], "VALIDATION", "{bad}");
    assert_eq!(
        maru(&data, &["board", "show", "free"])["description"],
        b["description"]
    );

    // 인기 글 위젯 카드 모양
    let bad = run(
        &data,
        &[
            "widget",
            "place",
            "popular_posts",
            "--region",
            "sidebar",
            "--config",
            r#"{"style":"slides"}"#,
        ],
    );
    assert_eq!(bad["ok"], false, "{bad}");
    assert!(
        bad["error"]["message"].to_string().contains("style"),
        "{bad}"
    );
    let w = maru(
        &data,
        &[
            "widget",
            "place",
            "popular_posts",
            "--region",
            "sidebar",
            "--title",
            "자유 베스트",
            "--config",
            r#"{"board":"free","style":"cards","days":0}"#,
        ],
    );
    assert_eq!(w["config"]["style"], "cards", "{w}");
    let plain = maru(
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
    assert_eq!(plain["config"]["style"], "lines", "기본은 제목 줄: {plain}");

    let (_server, bind) = start_server(&data);
    let (status, page) = get(&bind, "/b/free");
    assert_eq!(status, 200);
    assert!(
        page.contains(
            "<p class=\"board-desc\">자유롭게 이야기해요.\n광고는 &lt;b&gt;지웁니다&lt;/b&gt;.</p>"
        ),
        "줄바꿈은 그대로, HTML 은 글자로: {page}"
    );
    // 검색 결과 화면엔 안내 문구를 다시 보이지 않는다
    let (_, found) = get(&bind, "/b/free?q=%EB%B2%A0%EC%8A%A4%ED%8A%B8");
    assert!(!found.contains("board-desc"), "{found}");

    let (_, home) = get(&bind, "/");
    let at = home.find("widget-best").expect("베스트 카드");
    let cards = &home[at..at + home[at..].find("</ol>").unwrap()];
    assert!(
        cards.contains("<span class=\"rank\" aria-hidden=\"true\">1</span>"),
        "{cards}"
    );
    assert!(cards.contains("href=\"/b/free/best1\""), "{cards}");
    assert!(
        cards.contains("tile-fallback"),
        "그림 없는 글은 첫 글자: {cards}"
    );
    assert!(
        home.contains("class=\"widget-lines numbered\""),
        "제목 줄 위젯은 그대로: {home}"
    );

    // 빈 값이면 지운다
    let b = maru(&data, &["board", "update", "free", "--description", ""]);
    assert_eq!(b["description"], "");
    let (_, page) = get(&bind, "/b/free");
    assert!(!page.contains("board-desc"), "{page}");
}
