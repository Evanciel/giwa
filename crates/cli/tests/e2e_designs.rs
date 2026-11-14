//! W3 디자인 모음 (ADR-0158). 실제 바이너리·실제 포트·실제 DB 파일로
//! CLI `design gallery` 와 REST `/api/v1/designs` 가 같은 목록을 돌려주는지, 편집기 데이터가 디자인 견본을 싣는지 본다.
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
    let text = String::from_utf8(raw).unwrap();
    let (head, body) = text.split_once("\r\n\r\n").unwrap_or((&text, ""));
    (head[9..12].parse().unwrap(), body.to_owned())
}

#[test]
fn design_gallery_is_the_same_everywhere() {
    let tmp = tempfile::tempdir().unwrap();
    let data = tmp.path().join("사이트");
    giwa(
        &data,
        &["init", "--no-samples", "--preset", "blog", "--yes"],
    );

    let list = giwa(&data, &["design", "gallery"]);
    let items = list["items"].as_array().unwrap();
    assert!(items.len() >= 10, "{list}");
    let cafe = items
        .iter()
        .find(|d| d["id"] == "cafe")
        .expect("가게 디자인");
    assert!(cafe["blocks"].as_u64().unwrap() >= 2, "{cafe}");
    assert!(cafe["swatch"]["primary"].is_string(), "{cafe}");

    let one = giwa(&data, &["design", "gallery", "cafe"]);
    assert_eq!(one["id"], "cafe");
    assert_eq!(
        one["items"].as_array().unwrap().len() as u64,
        cafe["blocks"].as_u64().unwrap()
    );
    assert!(one["tokens"]["color_dark"]["bg"].is_string(), "{one}");
    let bad = run(&data, &["design", "gallery", "없는디자인"]);
    assert_eq!(bad["ok"], false, "{bad}");

    let (_server, bind) = start_server(&data);
    let (status, body) = get(&bind, "/api/v1/designs");
    assert_eq!(status, 200, "{body}");
    let rest: Value = serde_json::from_str(&body).unwrap();
    assert_eq!(rest["data"], list["items"], "REST 와 CLI 는 같은 목록");
    let (status, body) = get(&bind, "/api/v1/designs/cafe");
    assert_eq!(status, 200, "{body}");
    let rest: Value = serde_json::from_str(&body).unwrap();
    assert_eq!(rest["data"], one);
}

/// W4 내 블록 보관함 (ADR-0159). 저장은 멱등이고, 지우기도 여러 번 해도 같다. 손님은 보관함을 보지 못한다.
#[test]
fn saved_blocks_are_kept_for_reuse() {
    let tmp = tempfile::tempdir().unwrap();
    let data = tmp.path().join("사이트");
    giwa(
        &data,
        &["init", "--no-samples", "--preset", "blog", "--yes"],
    );
    let save = [
        "widget",
        "saved",
        "save",
        "quote",
        "--name",
        "대표 인사말",
        "--config",
        r#"{"text":"늘 처음처럼","name":"대표"}"#,
    ];
    let a = giwa(&data, &save);
    let b = giwa(&data, &save);
    assert_eq!(a["id"], b["id"], "같은 내용은 한 번만 저장");
    assert_eq!(a["region"], "main");
    assert_eq!(a["config"]["text"], "늘 처음처럼");

    let bad = run(
        &data,
        &[
            "widget",
            "saved",
            "save",
            "quote",
            "--name",
            "빈 인용",
            "--config",
            "{}",
        ],
    );
    assert_eq!(bad["ok"], false, "놓을 때와 같은 검사: {bad}");
    let bad = run(
        &data,
        &["widget", "saved", "save", "없는종류", "--name", "이름"],
    );
    assert_eq!(bad["ok"], false, "{bad}");

    let list = giwa(&data, &["widget", "saved", "list"]);
    assert_eq!(list.as_array().unwrap().len(), 1, "{list}");

    let (_server, bind) = start_server(&data);
    let (status, body) = get(&bind, "/api/v1/saved-blocks");
    assert_eq!(status, 401, "손님은 보관함을 볼 수 없다: {body}");
    drop(_server);

    let id = a["id"].as_str().unwrap();
    let gone = giwa(&data, &["widget", "saved", "delete", id]);
    assert_eq!(gone["deleted"], true);
    let again = giwa(&data, &["widget", "saved", "delete", id]);
    assert_eq!(again["deleted"], false, "두 번 지워도 오류가 아니다");
    let list = giwa(&data, &["widget", "saved", "list"]);
    assert!(list.as_array().unwrap().is_empty());
    let audit = giwa(&data, &["audit", "list"]).to_string();
    assert!(
        audit.contains("block.save") && audit.contains("block.delete"),
        "감사 기록"
    );
}
