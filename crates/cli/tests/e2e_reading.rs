//! 읽기 경험: 공개 응답 ETag·304, 제목 앵커·자동 목차, 접기 블록.
//! 실제 바이너리·실제 포트·실제 DB 파일로, CLI 로 쓰고 공개 화면으로 읽는다.
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

fn maru(data: &Path, args: &[&str]) -> Value {
    let out = Command::new(BIN)
        .arg("--data")
        .arg(data)
        .args(args)
        .arg("--json")
        .output()
        .expect("maru 실행");
    let stdout = String::from_utf8(out.stdout).expect("UTF-8 출력");
    let v: Value = serde_json::from_str(stdout.trim()).unwrap_or_else(|e| {
        panic!(
            "JSON 아님: {e}\nstdout: {stdout}\nstderr: {}",
            String::from_utf8_lossy(&out.stderr)
        )
    });
    assert_eq!(v["ok"], true, "{v}");
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
    body: Vec<u8>,
}

impl Res {
    fn header(&self, name: &str) -> Option<String> {
        self.head.lines().find_map(|l| {
            let (k, v) = l.split_once(':')?;
            k.trim()
                .eq_ignore_ascii_case(name)
                .then(|| v.trim().to_owned())
        })
    }

    fn text(&self) -> String {
        String::from_utf8_lossy(&self.body).into_owned()
    }
}

fn http(bind: &str, method: &str, path: &str, headers: &[(&str, &str)]) -> Res {
    let mut s = TcpStream::connect(bind).unwrap();
    let mut req = format!("{method} {path} HTTP/1.1\r\nHost: {bind}\r\nConnection: close\r\n");
    for (k, v) in headers {
        req.push_str(&format!("{k}: {v}\r\n"));
    }
    req.push_str("\r\n");
    s.write_all(req.as_bytes()).unwrap();
    let mut raw = Vec::new();
    s.read_to_end(&mut raw).unwrap();
    let split = raw
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .expect("응답 머리");
    let head = String::from_utf8(raw[..split].to_vec()).unwrap();
    Res {
        status: head[9..12].parse().unwrap(),
        head,
        body: raw[split + 4..].to_vec(),
    }
}

fn path_of(url: &str) -> String {
    let rest = url.split_once("://").unwrap().1;
    rest[rest.find('/').unwrap()..].to_owned()
}

fn init(data: &Path) {
    maru(data, &["init", "--preset", "blog", "--yes"]);
}

#[test]
fn public_pages_answer_conditional_requests() {
    let tmp = tempfile::tempdir().unwrap();
    let data = tmp.path().join("site");
    init(&data);
    let post = maru(
        &data,
        &[
            "post",
            "new",
            "--title",
            "캐시 확인",
            "--slug",
            "cache-test",
            "--body",
            "첫 본문",
            "--publish",
        ],
    );
    let id = post["id"].as_str().unwrap().to_owned();
    let path = path_of(post["url"].as_str().unwrap());
    let (_server, bind) = start_server(&data);

    // 1. 첫 요청에 약한 ETag 와 재확인 지시
    let first = http(&bind, "GET", &path, &[]);
    assert_eq!(first.status, 200);
    let tag = first.header("etag").expect("ETag");
    assert!(tag.starts_with("W/\""), "{tag}");
    assert_eq!(first.header("cache-control").as_deref(), Some("no-cache"));

    // 2. 같은 값이면 본문 없는 304, 머리는 남는다
    let again = http(&bind, "GET", &path, &[("If-None-Match", &tag)]);
    assert_eq!(again.status, 304, "{}", again.text());
    assert!(again.body.is_empty());
    assert_eq!(again.header("etag").as_deref(), Some(tag.as_str()));
    // 강한 형태로 보내도 약한 비교로 맞는다
    let strong = tag.trim_start_matches("W/");
    assert_eq!(
        http(&bind, "GET", &path, &[("If-None-Match", strong)]).status,
        304
    );
    // 다른 값이면 200
    assert_eq!(
        http(&bind, "GET", &path, &[("If-None-Match", "W/\"nope\"")]).status,
        200
    );

    // 3. 글을 고치면 같은 ETag 로 물어도 새 본문
    maru(&data, &["post", "edit", &id, "--body", "고친 본문"]);
    let changed = http(&bind, "GET", &path, &[("If-None-Match", &tag)]);
    assert_eq!(changed.status, 200);
    assert!(changed.text().contains("고친 본문"));
    assert_ne!(changed.header("etag").unwrap(), tag);

    // 4. 홈·피드·테마 자산도, 미디어·관리 화면·API·404 는 건드리지 않는다
    for p in ["/", "/rss.xml", "/sitemap.xml", "/assets/tokens.css"] {
        let r = http(&bind, "GET", p, &[]);
        assert_eq!(r.status, 200, "{p}");
        assert!(r.header("etag").is_some(), "{p}");
    }
    let missing = http(&bind, "GET", "/b/blog/no-such-post", &[]);
    assert_eq!(missing.status, 404);
    assert!(missing.header("etag").is_none());
    let admin = http(&bind, "GET", "/admin/login", &[]);
    assert!(admin.header("etag").is_none());
    let api = http(&bind, "GET", "/api/v1/settings", &[]);
    assert!(api.header("etag").is_none());
}
