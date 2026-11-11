//! 홈 화면에 추가: 앱 정보·아이콘·서비스 워커·오프라인 안내 (ADR-0162).
//! 실제 바이너리·실제 포트·실제 DB 파일로 본다.
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
    serde_json::from_str(stdout.trim()).unwrap_or_else(|e| panic!("JSON 아님: {e}\n{stdout}"))
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
        let name = name.to_ascii_lowercase();
        self.head.lines().find_map(|l| {
            let (k, v) = l.split_once(':')?;
            (k.trim().to_ascii_lowercase() == name).then(|| v.trim().to_owned())
        })
    }

    fn text(&self) -> String {
        String::from_utf8_lossy(&self.body).into_owned()
    }

    fn json(&self) -> Value {
        serde_json::from_slice(&self.body)
            .unwrap_or_else(|e| panic!("JSON 아님 ({e}): {}", self.text()))
    }
}

fn http(bind: &str, method: &str, path: &str, headers: &[(&str, &str)], body: &[u8]) -> Res {
    let mut s = TcpStream::connect(bind).unwrap();
    let mut req = format!("{method} {path} HTTP/1.1\r\nHost: {bind}\r\nConnection: close\r\n");
    for (k, v) in headers {
        req.push_str(&format!("{k}: {v}\r\n"));
    }
    req.push_str(&format!("Content-Length: {}\r\n\r\n", body.len()));
    s.write_all(req.as_bytes()).unwrap();
    // 서버가 상한 초과로 본문을 다 받기 전에 응답하고 끊을 수 있다. 그때도 응답은 읽는다.
    let _ = s.write_all(body);
    let mut raw = Vec::new();
    let _ = s.read_to_end(&mut raw);
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

fn png_size(bytes: &[u8]) -> (u32, u32) {
    assert!(bytes.starts_with(b"\x89PNG\r\n\x1a\n"), "PNG 아님");
    let n = |i: usize| u32::from_be_bytes([bytes[i], bytes[i + 1], bytes[i + 2], bytes[i + 3]]);
    (n(16), n(20))
}

/// W7 홈 화면에 추가 (ADR-0162).
#[test]
fn home_screen_install_manifest_icons_and_worker() {
    let tmp = tempfile::tempdir().unwrap();
    let data = tmp.path().join("사이트");
    maru(
        &data,
        &["init", "--no-samples", "--preset", "company", "--yes"],
    );
    let (_server, bind) = start_server(&data);
    let primary = maru(&data, &["design", "show"])["data"]["color"]["primary"]
        .as_str()
        .unwrap()
        .to_owned();

    // 공개 화면 머리: 앱 정보·아이콘·주소창 색, 서비스 워커 켜짐 표시.
    let home = http(&bind, "GET", "/", &[], b"").text();
    assert!(
        home.contains(r#"<link rel="manifest" href="/manifest.webmanifest">"#),
        "{home}"
    );
    assert!(home.contains(r#"<link rel="apple-touch-icon" href="/apple-touch-icon.png">"#));
    assert!(home.contains(r#"<link rel="icon" href="/icon.svg" type="image/svg+xml">"#));
    assert!(
        home.contains(&format!(r#"<meta name="theme-color" content="{primary}">"#)),
        "{home}"
    );
    assert!(home.contains(r#"data-pwa="1""#));

    let res = http(&bind, "GET", "/manifest.webmanifest", &[], b"");
    assert_eq!(res.status, 200);
    assert_eq!(
        res.header("content-type").as_deref(),
        Some("application/manifest+json")
    );
    let m = res.json();
    assert_eq!(m["display"], "standalone");
    assert_eq!(m["start_url"], "/");
    assert_eq!(m["theme_color"], primary.as_str());
    assert!(!m["name"].as_str().unwrap().is_empty());
    let icons = m["icons"].as_array().unwrap();
    assert!(icons.iter().any(|i| i["purpose"] == "maskable"));
    for i in icons {
        let res = http(&bind, "GET", i["src"].as_str().unwrap(), &[], b"");
        assert_eq!(res.status, 200, "{i}");
    }

    for (path, size) in [
        ("/icon-192.png", 192),
        ("/icon-512.png", 512),
        ("/apple-touch-icon.png", 180),
        ("/favicon.ico", 64),
    ] {
        let res = http(&bind, "GET", path, &[], b"");
        assert_eq!(res.status, 200, "{path}");
        assert_eq!(res.header("content-type").as_deref(), Some("image/png"));
        assert_eq!(png_size(&res.body), (size, size), "{path}");
    }
    let svg = http(&bind, "GET", "/icon.svg", &[], b"");
    assert_eq!(svg.header("content-type").as_deref(), Some("image/svg+xml"));
    assert!(svg.text().starts_with("<svg"));
    assert!(svg.text().contains(&primary), "{}", svg.text());

    // 서비스 워커: 오프라인 안내와 그 스타일만 담는다.
    let sw = http(&bind, "GET", "/sw.js", &[], b"");
    assert_eq!(sw.status, 200);
    assert!(sw.header("content-type").unwrap().contains("javascript"));
    assert_eq!(sw.header("cache-control").as_deref(), Some("no-cache"));
    let js = sw.text();
    assert!(
        js.contains(r#"["/offline", "/assets/tokens.css", "/assets/site.css"]"#),
        "{js}"
    );
    assert!(!js.contains("/admin") && !js.contains("/me"), "{js}");
    let offline = http(&bind, "GET", "/offline", &[], b"");
    assert_eq!(offline.status, 200);
    let html = offline.text();
    assert!(html.contains("인터넷에 연결되어 있지 않습니다"), "{html}");
    assert!(html.contains(r#"<meta name="robots" content="noindex">"#));

    // 색을 바꾸면 아이콘·워커 판도 바뀐다.
    let icon_before = http(&bind, "GET", "/icon-192.png", &[], b"").body;
    let set = maru(&data, &["design", "set", "--set", "color.primary=#8a2b4f"]);
    assert_eq!(set["ok"], true, "{set}");
    let icon_after = http(&bind, "GET", "/icon-192.png", &[], b"").body;
    assert_ne!(icon_before, icon_after);
    assert!(
        http(&bind, "GET", "/icon.svg", &[], b"")
            .text()
            .contains("#8a2b4f")
    );
    assert_ne!(http(&bind, "GET", "/sw.js", &[], b"").text(), js);

    // 끄면 앱 정보·워커는 없는 주소, 머리에서도 빠진다. 아이콘은 남는다.
    let off = maru(&data, &["site", "set", "--pwa", "false"]);
    assert_eq!(off["ok"], true, "{off}");
    let home = http(&bind, "GET", "/", &[], b"").text();
    assert!(!home.contains("manifest.webmanifest"), "{home}");
    assert!(!home.contains("data-pwa"), "{home}");
    assert_eq!(
        http(&bind, "GET", "/manifest.webmanifest", &[], b"").status,
        404
    );
    assert_eq!(http(&bind, "GET", "/sw.js", &[], b"").status, 404);
    assert_eq!(http(&bind, "GET", "/icon-192.png", &[], b"").status, 200);
}
