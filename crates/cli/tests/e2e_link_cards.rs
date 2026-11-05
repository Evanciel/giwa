//! GAP E12·E22: 링크 카드 (ADR-0084). 실제 바이너리·실제 포트·실제 DB 파일과 이 테스트가 띄운 작은 웹 서버로
//! `::link[주소]` 가 처음엔 도메인만 보이다가, 가져온 뒤 제목·설명·사이트 이름·보관소 그림으로 채워지는지,
//! 이 컴퓨터 주소는 막히는지, SNS 게시물은 가져오지 않는지, 설정으로 끌 수 있는지 본다.
#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::thread;

use serde_json::Value;

const BIN: &str = env!("CARGO_BIN_EXE_maru");
const ALLOW: &str = "MARU_TEST_ALLOW_LOOPBACK_FETCH";

struct Server(Child);

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn run(data: &Path, args: &[&str], allow_loopback: bool) -> Value {
    let mut cmd = Command::new(BIN);
    cmd.arg("--data").arg(data).args(args).arg("--json");
    if allow_loopback {
        cmd.env(ALLOW, "1");
    } else {
        cmd.env_remove(ALLOW);
    }
    let out = cmd.output().expect("maru 실행");
    let stdout = String::from_utf8(out.stdout).expect("UTF-8 출력");
    serde_json::from_str(stdout.trim()).unwrap_or_else(|e| panic!("JSON 아님: {e}\n{stdout}"))
}

fn maru(data: &Path, args: &[&str]) -> Value {
    maru_env(data, args, false)
}

fn maru_env(data: &Path, args: &[&str], allow_loopback: bool) -> Value {
    let v = run(data, args, allow_loopback);
    assert_eq!(v["ok"], true, "{args:?} → {v}");
    v["data"].clone()
}

fn start_server(data: &Path) -> (Server, String) {
    let mut child = Command::new(BIN)
        .arg("--data")
        .arg(data)
        .args(["serve", "--bind", "127.0.0.1:0", "--json"])
        .env_remove(ALLOW)
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

fn png() -> Vec<u8> {
    let img = image::RgbImage::from_fn(64, 40, |x, y| {
        image::Rgb([(x * 4) as u8, (y * 6) as u8, 120])
    });
    let mut out = std::io::Cursor::new(Vec::new());
    img.write_to(&mut out, image::ImageFormat::Png).unwrap();
    out.into_inner()
}

/// 링크 카드가 읽을 작은 웹 서버. 받은 요청 수를 센다.
fn fake_site() -> (u16, Arc<AtomicUsize>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let hits = Arc::new(AtomicUsize::new(0));
    let seen = hits.clone();
    let image = png();
    thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { continue };
            seen.fetch_add(1, Ordering::SeqCst);
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut first = String::new();
            reader.read_line(&mut first).unwrap_or(0);
            loop {
                let mut l = String::new();
                if reader.read_line(&mut l).unwrap_or(0) == 0 || l == "\r\n" {
                    break;
                }
            }
            let path = first.split_whitespace().nth(1).unwrap_or("/").to_owned();
            let (status, ctype, body): (&str, &str, Vec<u8>) = match path.as_str() {
                "/page" => (
                    "200 OK",
                    "text/html; charset=utf-8",
                    "<!doctype html><html><head><title>예비 제목</title>\
                     <meta property=\"og:title\" content=\"링크 카드 &lt;시험&gt; 페이지\">\
                     <meta property=\"og:description\" content=\"설명 글입니다\">\
                     <meta property=\"og:site_name\" content=\"예시 사이트\">\
                     <meta property=\"og:image\" content=\"/img.png\"></head><body>본문</body></html>"
                        .as_bytes()
                        .to_vec(),
                ),
                "/img.png" => ("200 OK", "image/png", image.clone()),
                _ => ("404 Not Found", "text/plain", b"none".to_vec()),
            };
            let head = format!(
                "HTTP/1.1 {status}\r\nContent-Type: {ctype}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            );
            let _ = stream.write_all(head.as_bytes());
            let _ = stream.write_all(&body);
        }
    });
    (port, hits)
}

fn path_of(url: &str) -> String {
    let rest = url.split_once("://").unwrap().1;
    format!("/{}", rest.split_once('/').unwrap().1)
}

fn status_of(list: &Value, url: &str) -> Value {
    list["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|p| p["url"] == url)
        .cloned()
        .unwrap_or_else(|| panic!("{url} 없음: {list}"))
}

#[test]
fn link_cards_fill_after_fetch() {
    let tmp = tempfile::tempdir().unwrap();
    let data = tmp.path();
    maru(data, &["init", "--preset", "blog", "--yes"]);
    let (port, hits) = fake_site();
    let page = format!("http://127.0.0.1:{port}/page");
    let missing = format!("http://127.0.0.1:{port}/missing");
    let sns = "https://x.com/someone/status/1";
    let body = format!("앞 글\n\n::link[{page}]\n\n::link[{missing}]\n\n::link[{sns}]\n\n뒤 글\n");
    let post = maru(
        data,
        &[
            "post",
            "new",
            "--title",
            "링크 모음",
            "--body",
            &body,
            "--publish",
        ],
    );
    let path = path_of(post["url"].as_str().unwrap());

    // 처음 열면 도메인만 보이고, 주소는 가져올 차례로 적힌다(SNS 는 적지 않는다)
    {
        let (_srv, bind) = start_server(data);
        let (status, html) = get(&bind, &path);
        assert_eq!(status, 200, "{html}");
        assert_eq!(
            html.matches("<div class=\"link-card\">").count(),
            3,
            "{html}"
        );
        assert!(
            html.contains("<strong class=\"link-card-title\">127.0.0.1</strong>"),
            "{html}"
        );
        assert!(
            html.contains(&format!(
                "<span class=\"link-card-site\">127.0.0.1:{port}/page</span>"
            )),
            "{html}"
        );
        assert!(
            html.contains("<strong class=\"link-card-title\">X 게시물</strong>"),
            "{html}"
        );
        assert!(html.contains("rel=\"nofollow noopener\""), "{html}");
    }
    let list = maru(data, &["link", "list"]);
    assert_eq!(list["untrusted"], true);
    assert_eq!(list["items"].as_array().unwrap().len(), 2, "{list}");
    assert_eq!(status_of(&list, &page)["status"], "pending");
    assert_eq!(
        hits.load(Ordering::SeqCst),
        0,
        "화면을 그릴 때는 남의 주소에 접속하지 않는다"
    );

    // 이 컴퓨터 주소는 막는다(사설 주소 차단)
    let r = maru(data, &["link", "fetch"]);
    assert_eq!(r["retry"], 2, "{r}");
    let list = maru(data, &["link", "list"]);
    let p = status_of(&list, &page);
    assert_eq!(p["status"], "pending");
    assert!(
        p["last_error"]
            .as_str()
            .unwrap()
            .contains("공개 인터넷 주소가 아닙니다"),
        "{p}"
    );
    assert_eq!(hits.load(Ordering::SeqCst), 0);

    // 시험에서만 이 컴퓨터 주소를 열고 가져온다
    let r = maru_env(data, &["link", "fetch"], true);
    assert_eq!(r["ok"], 1, "{r}");
    assert_eq!(r["retry"], 1, "{r}");
    let list = maru(data, &["link", "list"]);
    let p = status_of(&list, &page);
    assert_eq!(p["status"], "ok", "{p}");
    assert_eq!(p["title"], "링크 카드 <시험> 페이지");
    assert_eq!(p["description"], "설명 글입니다");
    assert_eq!(p["site_name"], "예시 사이트");
    let image = p["image_url"].as_str().unwrap().to_owned();
    assert!(image.starts_with("/media/"), "{p}");
    let m = status_of(&list, &missing);
    assert!(m["last_error"].as_str().unwrap().contains("404"), "{m}");
    // 세 번 실패하면 그만둔다
    maru_env(data, &["link", "fetch"], true);
    maru_env(data, &["link", "fetch"], true);
    let list = maru(data, &["link", "list", "--status", "failed"]);
    assert_eq!(list["items"][0]["url"], missing.as_str(), "{list}");
    let audit = maru(data, &["audit", "list", "--action", "link.thumbnail"]);
    assert!(audit.to_string().contains(&page), "{audit}");

    // 다시 열면 가져온 정보로 채워진다. 남의 페이지 글자는 이스케이프된다
    {
        let (_srv, bind) = start_server(data);
        let (_, html) = get(&bind, &path);
        assert!(
            html.contains(
                "<strong class=\"link-card-title\">링크 카드 &lt;시험&gt; 페이지</strong>"
            ),
            "{html}"
        );
        assert!(
            html.contains("<span class=\"link-card-desc\">설명 글입니다</span>"),
            "{html}"
        );
        assert!(
            html.contains("<span class=\"link-card-site\">예시 사이트 · 127.0.0.1</span>"),
            "{html}"
        );
        assert!(
            html.contains(&format!("<img class=\"link-card-img\" src=\"{image}\"")),
            "{html}"
        );
        assert!(
            !html.contains(&format!("127.0.0.1:{port}/img.png")),
            "그림은 보관소 주소로만"
        );
        let (status, _) = get(&bind, &image);
        assert_eq!(status, 200, "보관소 그림");
        // REST 도 같은 정보를 준다
        let token = maru(
            data,
            &["token", "create", "--name", "읽기", "--scope", "draft"],
        );
        let secret = token["secret"].as_str().unwrap();
        let mut s = TcpStream::connect(&bind).unwrap();
        s.write_all(
            format!(
                "GET /api/v1/links?status=ok HTTP/1.1\r\nHost: {bind}\r\nAuthorization: Bearer {secret}\r\nConnection: close\r\n\r\n"
            )
            .as_bytes(),
        )
        .unwrap();
        let mut raw = String::new();
        s.read_to_string(&mut raw).unwrap();
        assert!(
            raw.contains("\"untrusted\":true") && raw.contains("예시 사이트"),
            "{raw}"
        );
    }

    // 다시 가져오기·끄기
    let p = maru(data, &["link", "refresh", &page]);
    assert_eq!(p["status"], "pending");
    let v = run(
        data,
        &["link", "refresh", "https://nowhere.example.test/"],
        false,
    );
    assert_eq!(v["error"]["code"], "NOT_FOUND", "{v}");
    maru(data, &["site", "set", "--link-cards", "false"]);
    let r = maru_env(data, &["link", "fetch"], true);
    assert_eq!(r["ok"], 0, "{r}");
    let (_srv, bind) = start_server(data);
    let (_, html) = get(&bind, &path);
    assert!(
        html.contains("<strong class=\"link-card-title\">127.0.0.1</strong>"),
        "{html}"
    );
    assert!(!html.contains("예시 사이트"), "{html}");
}
