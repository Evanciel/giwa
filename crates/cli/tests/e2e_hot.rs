//! GAP Q5: 인기 표시와 인기글만 보기 (ADR-0090). 실제 바이너리·실제 포트·실제 DB 파일로
//! 손님 추천이 기준에 닿으면 목록에 「인기」가 붙고, 인기글 화면에 그 글만 모이는지 본다.
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

fn http(
    bind: &str,
    method: &str,
    path: &str,
    headers: &[(&str, &str)],
    body: &str,
) -> (u16, String) {
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
    (head[9..12].parse().unwrap(), body.to_owned())
}

fn get(bind: &str, path: &str) -> (u16, String) {
    http(bind, "GET", path, &[], "")
}

/// 서로 다른 손님(브라우저) `who` 가 글에 추천을 누른다.
fn up(bind: &str, post_id: &str, who: &str) {
    let (status, body) = http(
        bind,
        "POST",
        &format!("/m/reaction/post/{post_id}"),
        &[
            ("Content-Type", "application/x-www-form-urlencoded"),
            ("Sec-Fetch-Site", "same-origin"),
            ("User-Agent", who),
        ],
        "kind=up",
    );
    assert_eq!(status, 303, "{body}");
}

fn new_post(data: &Path, board: &str, slug: &str, title: &str) -> String {
    let v = maru(
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
    v["id"]
        .as_str()
        .unwrap_or_else(|| panic!("글 id: {v}"))
        .to_owned()
}

#[test]
fn hot_mark_and_hot_only_view() {
    let tmp = tempfile::tempdir().unwrap();
    let data = tmp.path().join("사이트");
    maru(
        &data,
        &["init", "--no-samples", "--preset", "blog", "--yes"],
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
            "create",
            "staff",
            "--name",
            "운영진",
            "--kind",
            "free",
        ],
    );
    let hot = new_post(&data, "free", "hot1", "모두가 추천한 글");
    let cold = new_post(&data, "free", "cold1", "조용한 글");
    let hidden = new_post(&data, "staff", "staff1", "운영진만 보는 글");

    maru(&data, &["module", "enable", "reaction"]);
    // 기준은 0~100000
    let bad = run(&data, &["module", "set", "reaction", "--field", "hot=-1"]);
    assert_eq!(bad["error"]["code"], "VALIDATION", "{bad}");
    maru(
        &data,
        &[
            "module",
            "set",
            "reaction",
            "--field",
            "guest=true",
            "--field",
            "hot=2",
        ],
    );

    let (_server, bind) = start_server(&data);
    up(&bind, &hot, "guest-a");
    up(&bind, &hot, "guest-b");
    up(&bind, &cold, "guest-a");
    // 추천을 받은 뒤 운영진만 읽게 바꾼 게시판의 글은 기준을 넘어도 손님 인기글 화면에 없다.
    up(&bind, &hidden, "guest-a");
    up(&bind, &hidden, "guest-b");
    maru(&data, &["board", "update", "staff", "--read", "admin"]);
    let ok = maru(
        &data,
        &[
            "reaction",
            "counts",
            "--target-type",
            "post",
            "--target-id",
            &hidden,
        ],
    );
    assert_eq!(ok["kinds"][0]["count"], 2);

    // 목록: 기준(2) 이상인 글에만 「인기」, 인기글만 보기 링크
    let (status, page) = get(&bind, "/b/free");
    assert_eq!(status, 200);
    assert_eq!(
        page.matches("<span class=\"hot-mark\">인기</span>").count(),
        1,
        "{page}"
    );
    let mark = page.find("hot-mark").unwrap();
    let title = page.find("모두가 추천한 글").unwrap();
    assert!(
        title < mark && mark - title < 400,
        "인기 표시는 그 글 제목 옆: {page}"
    );
    assert!(
        page.contains("href=\"/m/reaction/hot?board=free\""),
        "{page}"
    );

    // 인기글 화면: 그 게시판의 기준 이상 글만
    let (status, page) = get(&bind, "/m/reaction/hot?board=free");
    assert_eq!(status, 200, "{page}");
    assert!(page.contains("자유 인기글"), "{page}");
    assert!(page.contains("href=\"/b/free/hot1\""), "{page}");
    assert!(!page.contains("조용한 글"), "{page}");
    assert!(page.contains("추천 2"), "{page}");
    assert!(page.contains("noindex"), "{page}");

    // 전체 인기글: 손님이 못 읽는 게시판은 빠진다
    let (status, page) = get(&bind, "/m/reaction/hot");
    assert_eq!(status, 200, "{page}");
    assert!(page.contains("href=\"/b/free/hot1\""), "{page}");
    assert!(!page.contains("운영진만 보는 글"), "{page}");
    // 손님이 못 읽는 게시판을 주소로 골라도 열리지 않는다
    let (status, _) = get(&bind, "/m/reaction/hot?board=staff");
    assert_ne!(status, 200);

    // 기준 0 이면 표시도 링크도 없고 화면도 닫힌다
    maru(&data, &["module", "set", "reaction", "--field", "hot=0"]);
    let (_, page) = get(&bind, "/b/free");
    assert!(!page.contains("hot-mark"), "{page}");
    assert!(!page.contains("/m/reaction/hot"), "{page}");
    let (status, _) = get(&bind, "/m/reaction/hot");
    assert_eq!(status, 404);
}
