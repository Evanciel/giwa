//! GAP C13 위키 (ADR-0078). 실제 바이너리·실제 포트·실제 DB 파일로 위키 게시판의 `[[문서]]` 링크,
//! 아직 없는 문서의 만들기 링크(제목이 채워진 글쓰기 화면), 역링크, 게시판의 필요한 문서·외톨이 문서,
//! `giwa wiki`·`GET /api/v1/boards/{slug}/wiki` 를 본다.
#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::path::Path;
use std::process::{Child, Command, Stdio};

use serde_json::Value;

const BIN: &str = env!("CARGO_BIN_EXE_giwa");

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
        .expect("giwa 실행");
    let stdout = String::from_utf8(out.stdout).expect("UTF-8 출력");
    serde_json::from_str(stdout.trim()).unwrap_or_else(|e| panic!("JSON 아님: {e}\n{stdout}"))
}

fn giwa(data: &Path, args: &[&str]) -> Value {
    let v = run(data, args);
    assert_eq!(v["ok"], true, "{args:?} → {v}");
    v["data"].clone()
}

fn start_server(data: &Path) -> (Server, Value) {
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
    (server, v["data"].clone())
}

struct Res {
    status: u16,
    body: String,
}

fn http(bind: &str, path: &str, headers: &[(&str, &str)]) -> Res {
    let mut s = TcpStream::connect(bind).unwrap();
    let mut req = format!("GET {path} HTTP/1.1\r\nHost: {bind}\r\nConnection: close\r\n");
    for (k, v) in headers {
        req.push_str(&format!("{k}: {v}\r\n"));
    }
    req.push_str("\r\n");
    s.write_all(req.as_bytes()).unwrap();
    let mut raw = Vec::new();
    s.read_to_end(&mut raw).unwrap();
    let text = String::from_utf8(raw).unwrap();
    let (head, body) = text.split_once("\r\n\r\n").unwrap_or((&text, ""));
    Res {
        status: head[9..12].parse().unwrap(),
        body: body.to_owned(),
    }
}

#[test]
fn wiki_board_links_backlinks_and_index() {
    let tmp = tempfile::tempdir().unwrap();
    let data = tmp.path().join("사이트");
    giwa(
        &data,
        &["init", "--no-samples", "--preset", "blog", "--yes"],
    );
    let b = giwa(
        &data,
        &[
            "board",
            "create",
            "wiki",
            "--name",
            "설정집",
            "--kind",
            "wiki",
        ],
    );
    assert_eq!(b["kind"], "wiki");
    for (title, slug, body) in [
        ("대문", "front", "[[주인공]]과 [[마을|작은 마을]] 이야기"),
        ("주인공", "hero", "[[대문]]으로 돌아가기"),
        ("외딴 섬", "island", "아무도 모르는 곳"),
    ] {
        giwa(
            &data,
            &[
                "post",
                "new",
                "--board",
                "wiki",
                "--title",
                title,
                "--slug",
                slug,
                "--body",
                body,
                "--publish",
            ],
        );
    }
    let idx = giwa(&data, &["wiki", "wiki"]);
    assert_eq!(idx["pages"], 3);
    assert_eq!(idx["wanted"][0]["title"], "마을");
    assert_eq!(idx["orphans"][0]["title"], "외딴 섬");
    // 위키가 아닌 게시판
    let bad = run(&data, &["wiki", "blog"]);
    assert_eq!(bad["ok"], false);

    let (_server, started) = start_server(&data);
    let bind = started["bind"].as_str().unwrap().to_owned();

    let front = http(&bind, "/b/wiki/front", &[]);
    assert_eq!(front.status, 200, "{}", front.body);
    assert!(
        front
            .body
            .contains("<a class=\"wikilink\" href=\"/b/wiki/hero\""),
        "{}",
        front.body
    );
    assert!(front.body.contains("class=\"wikilink wikilink-new\""));
    assert!(front.body.contains(">작은 마을</a>"));
    assert!(front.body.contains("이 문서를 가리키는 문서"));
    assert!(
        front.body.contains("href=\"/b/wiki/hero\">주인공</a>"),
        "역링크"
    );

    let list = http(&bind, "/b/wiki", &[]);
    assert!(list.body.contains("필요한 문서") && list.body.contains("외톨이 문서"));
    assert!(list.body.contains(">외딴 섬</a>"));

    // 만들기 링크는 제목을 채운 글쓰기 화면 (손님은 로그인으로 보낼 수 있다 → 운영자 정책과 무관하게 주소만 본다)
    let href = front
        .body
        .split("class=\"wikilink wikilink-new\" href=\"")
        .nth(1)
        .unwrap()
        .split('"')
        .next()
        .unwrap()
        .replace("&amp;", "&");
    assert!(href.starts_with("/write/wiki?title="), "{href}");

    // REST
    let token = giwa(
        &data,
        &["token", "create", "--name", "읽기", "--scope", "read"],
    );
    let auth = format!("Bearer {}", token["secret"].as_str().unwrap());
    let api = http(
        &bind,
        "/api/v1/boards/wiki/wiki",
        &[("Authorization", &auth)],
    );
    assert_eq!(api.status, 200, "{}", api.body);
    let v: Value = serde_json::from_str(&api.body).unwrap();
    assert_eq!(v["data"]["wanted"][0]["count"], 1);
}
