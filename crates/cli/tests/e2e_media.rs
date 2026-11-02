//! 미디어 업로드·제공과 미리보기를 실제 바이너리·실제 포트·실제 DB 파일로 확인한다.
//! CLI, REST(토큰), 관리 화면(세션 + CSRF 헤더) 세 경로가 같은 서비스를 쓰는지 본다.
#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::io::{BufRead, BufReader, Cursor, Read, Write};
use std::net::TcpStream;
use std::path::Path;
use std::process::{Child, Command, Stdio};

use image::{DynamicImage, ImageFormat, Rgb, RgbImage};
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

    fn cookie(&self, name: &str) -> Option<String> {
        self.head.lines().find_map(|l| {
            let (k, v) = l.split_once(':')?;
            if !k.trim().eq_ignore_ascii_case("set-cookie") {
                return None;
            }
            let (ck, cv) = v.trim().split(';').next()?.split_once('=')?;
            (ck == name).then(|| cv.to_owned())
        })
    }

    fn csrf_field(&self) -> String {
        let body = self.text();
        let at = body.find("name=\"_csrf\" value=\"").expect("_csrf 필드");
        let rest = &body[at + 20..];
        rest[..rest.find('"').unwrap()].to_owned()
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

fn jpeg(w: u32, h: u32) -> Vec<u8> {
    let img = DynamicImage::ImageRgb8(RgbImage::from_pixel(w, h, Rgb([240, 180, 20])));
    let mut buf = Vec::new();
    img.write_to(&mut Cursor::new(&mut buf), ImageFormat::Jpeg)
        .unwrap();
    buf
}

fn png(w: u32, h: u32) -> Vec<u8> {
    let img = DynamicImage::ImageRgb8(RgbImage::from_pixel(w, h, Rgb([10, 20, 30])));
    let mut buf = Vec::new();
    img.write_to(&mut Cursor::new(&mut buf), ImageFormat::Png)
        .unwrap();
    buf
}

fn path_of(url: &str) -> String {
    let rest = url.split_once("://").unwrap().1;
    rest[rest.find('/').unwrap()..].to_owned()
}

/// 로그인 링크로 세션을 받고, 글쓰기 화면에서 세션 CSRF 값을 읽는다.
fn admin_session(data: &Path, bind: &str) -> (String, String) {
    let link = maru(data, &["login-link"]);
    let link_path = path_of(link["data"]["url"].as_str().unwrap());
    let page = http(bind, "GET", &link_path, &[], b"");
    let pre = page.cookie("maru_csrf").unwrap();
    let res = http(
        bind,
        "POST",
        &link_path,
        &[
            ("Content-Type", "application/x-www-form-urlencoded"),
            ("Cookie", &format!("maru_csrf={pre}")),
        ],
        format!("_csrf={pre}").as_bytes(),
    );
    assert_eq!(res.status, 303, "{}", res.text());
    let cookie = format!("maru_session={}", res.cookie("maru_session").unwrap());
    let edit = http(bind, "GET", "/admin/posts/new", &[("Cookie", &cookie)], b"");
    assert_eq!(edit.status, 200);
    (cookie, edit.csrf_field())
}

#[test]
fn media_upload_serve_and_preview_through_all_paths() {
    let tmp = tempfile::tempdir().unwrap();
    let data = tmp.path().join("사이트");
    let init = maru(&data, &["init", "--yes"]);
    let token = init["data"]["token"]["secret"].as_str().unwrap().to_owned();
    let bearer = format!("Bearer {token}");

    // CLI: 한글 파일 이름도 받고, 경로에는 ID 만 쓴다.
    let file = tmp.path().join("가을 사진.jpg");
    std::fs::write(&file, jpeg(1400, 700)).unwrap();
    let up = maru(
        &data,
        &[
            "media",
            "upload",
            file.to_str().unwrap(),
            "--alt",
            "노란 벽",
        ],
    );
    assert_eq!(up["ok"], true, "{up}");
    let m = &up["data"];
    assert_eq!(m["width"], 1400);
    assert_eq!(m["variants"].as_array().unwrap().len(), 2);
    let url = m["url"].as_str().unwrap().to_owned();
    assert!(!url.contains("가을"));

    let (_server, bind) = start_server(&data);

    // 공개 제공: 형식, 오래 캐시, 스니핑 금지.
    let res = http(&bind, "GET", &url, &[], b"");
    assert_eq!(res.status, 200);
    assert_eq!(res.header("content-type").as_deref(), Some("image/jpeg"));
    assert!(res.header("cache-control").unwrap().contains("immutable"));
    assert_eq!(
        res.header("x-content-type-options").as_deref(),
        Some("nosniff")
    );
    assert_eq!(image::guess_format(&res.body).unwrap(), ImageFormat::Jpeg);
    let small = m["variants"][0]["url"].as_str().unwrap();
    assert_eq!(http(&bind, "GET", small, &[], b"").status, 200);

    // 보관소 밖·규칙 밖 경로는 파일 시스템을 보지 않고 404.
    for bad in [
        "/media/../config.toml",
        "/media/%2e%2e/maru.db",
        "/media/2026/x.svg",
        "/media/2026/X.JPG",
        "/media/2026/11/없는.jpg",
    ] {
        assert_eq!(http(&bind, "GET", bad, &[], b"").status, 404, "{bad}");
    }

    // REST: 토큰 없으면 401, 그림이 아니면 400, 정상은 201.
    let svg = b"<svg xmlns='http://www.w3.org/2000/svg' onload='alert(1)'/>";
    let res = http(
        &bind,
        "POST",
        "/api/v1/media?filename=a.png",
        &[],
        &png(20, 20),
    );
    assert_eq!(res.status, 401, "{}", res.text());
    let res = http(
        &bind,
        "POST",
        "/api/v1/media?filename=a.png",
        &[("Authorization", &bearer)],
        svg,
    );
    assert_eq!(res.status, 400, "{}", res.text());
    assert_eq!(res.json()["error"]["code"], "VALIDATION");
    let res = http(
        &bind,
        "POST",
        "/api/v1/media?filename=%EB%8F%84%ED%91%9C.png&alt=%EB%8F%84%ED%91%9C",
        &[("Authorization", &bearer)],
        &png(30, 20),
    );
    assert_eq!(res.status, 201, "{}", res.text());
    let rest_media = res.json()["data"].clone();
    assert_eq!(rest_media["mime"], "image/png");
    assert_eq!(rest_media["alt_text"], "도표");

    let list = http(
        &bind,
        "GET",
        "/api/v1/media",
        &[("Authorization", &bearer)],
        b"",
    );
    assert_eq!(list.json()["data"]["total"], 2);

    // 10MB 를 넘는다고 선언한 본문은 받기 전에 머리만 보고 막는다.
    // (본문까지 보내면 Windows 는 서버가 끊을 때 응답을 버리므로 머리만 보낸다.)
    let mut s = TcpStream::connect(&bind).unwrap();
    let req = format!(
        "POST /api/v1/media?filename=a.jpg HTTP/1.1\r\nHost: {bind}\r\nConnection: close\r\n\
         Authorization: {bearer}\r\nContent-Length: {}\r\n\r\n",
        11 * 1024 * 1024
    );
    s.write_all(req.as_bytes()).unwrap();
    let mut head = [0u8; 12];
    s.read_exact(&mut head).unwrap();
    assert_eq!(&head[9..12], b"413", "{}", String::from_utf8_lossy(&head));

    // 미리보기: 공개 화면과 같은 정화.
    let body = serde_json::json!({ "body_md": "# 제목\n\n<script>alert(1)</script>\n\n![고양이](/media/x.jpg)" });
    let res = http(
        &bind,
        "POST",
        "/api/v1/preview",
        &[
            ("Authorization", &bearer),
            ("Content-Type", "application/json"),
        ],
        body.to_string().as_bytes(),
    );
    assert_eq!(res.status, 200, "{}", res.text());
    let html = res.json()["data"]["html"].as_str().unwrap().to_owned();
    assert!(html.contains("<h1>제목</h1>"), "{html}");
    assert!(
        html.contains("<img src=\"/media/x.jpg\" alt=\"고양이\""),
        "{html}"
    );
    assert!(!html.contains("<script"), "{html}");

    // 관리 화면: 세션만으로는 안 되고 CSRF 헤더가 맞아야 한다.
    let (cookie, csrf) = admin_session(&data, &bind);
    let res = http(
        &bind,
        "POST",
        "/admin/media?filename=b.jpg",
        &[("Cookie", &cookie)],
        &jpeg(100, 100),
    );
    assert_eq!(res.status, 403, "{}", res.text());
    let res = http(
        &bind,
        "POST",
        "/admin/media?filename=b.jpg",
        &[("X-CSRF-Token", &csrf)],
        &jpeg(100, 100),
    );
    assert_eq!(res.status, 401, "{}", res.text());
    let res = http(
        &bind,
        "POST",
        "/admin/media?filename=b.jpg&alt=%EC%A0%95%EC%82%AC%EA%B0%81%ED%98%95",
        &[("Cookie", &cookie), ("X-CSRF-Token", &csrf)],
        &jpeg(100, 100),
    );
    assert_eq!(res.status, 201, "{}", res.text());
    assert_eq!(res.json()["data"]["alt_text"], "정사각형");
    assert_eq!(res.header("cache-control").as_deref(), Some("no-store"));

    let res = http(
        &bind,
        "POST",
        "/admin/preview",
        &[
            ("Cookie", &cookie),
            ("X-CSRF-Token", &csrf),
            ("Content-Type", "application/json"),
        ],
        r#"{"body_md":"**굵게**"}"#.as_bytes(),
    );
    assert_eq!(res.status, 200, "{}", res.text());
    assert_eq!(
        res.json()["data"]["html"].as_str().unwrap().trim(),
        "<p><strong>굵게</strong></p>"
    );

    // 세 경로가 모두 같은 표에 남는다.
    let all = maru(&data, &["media", "list"]);
    assert_eq!(all["data"]["total"], 3);

    // 지우면 파일도 사라진다.
    let id = rest_media["id"].as_str().unwrap();
    let del = maru(&data, &["media", "delete", id]);
    assert_eq!(del["ok"], true, "{del}");
    let gone = rest_media["url"].as_str().unwrap();
    assert_eq!(http(&bind, "GET", gone, &[], b"").status, 404);
}
