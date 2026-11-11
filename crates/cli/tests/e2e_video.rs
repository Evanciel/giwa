//! 동영상 올리기(나눠 올리기)·범위 응답·동영상 섹션·본문 동영상 (ADR-0161).
//! 실제 바이너리·실제 포트·실제 DB 파일로 CLI, REST, 관리 화면 세 경로를 본다.
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

/// mp4 머리(4번째 바이트부터 `ftyp`)를 가진 가짜 동영상. 바이트마다 값이 달라 구간을 대조할 수 있다.
fn mp4(len: usize) -> Vec<u8> {
    let mut v: Vec<u8> = (0..len).map(|i| (i % 251) as u8).collect();
    v[..12].copy_from_slice(b"\0\0\0\x18ftypisom");
    v
}

#[test]
fn video_upload_ranges_sections_and_posts() {
    let tmp = tempfile::tempdir().unwrap();
    let data = tmp.path().join("사이트");
    let init = maru(
        &data,
        &["init", "--no-samples", "--preset", "company", "--yes"],
    );
    let token = init["data"]["token"]["secret"].as_str().unwrap().to_owned();
    let bearer = format!("Bearer {token}");

    // CLI: 한 번에 올리기. 머리 모양이 mp4 가 아니면 거절한다.
    let small = mp4(1000);
    let file = tmp.path().join("짧은 영상.mp4");
    std::fs::write(&file, &small).unwrap();
    let up = maru(&data, &["media", "upload", file.to_str().unwrap()]);
    assert_eq!(up["ok"], true, "{up}");
    assert_eq!(up["data"]["mime"], "video/mp4");
    assert_eq!(up["data"]["size"], 1000);
    let small_url = up["data"]["url"].as_str().unwrap().to_owned();
    assert!(small_url.ends_with(".mp4"), "{small_url}");
    let fake = tmp.path().join("가짜.mp4");
    std::fs::write(&fake, b"<html>not a video</html>").unwrap();
    let bad = maru(&data, &["media", "upload", fake.to_str().unwrap()]);
    assert_eq!(bad["ok"], false, "{bad}");

    // 동영상 섹션: 올린 동영상 주소를 받고, 보관함 밖·임시 경로는 거절한다.
    let place = |config: &str| {
        maru(
            &data,
            &[
                "widget", "place", "video", "--region", "main", "--scope", "home", "--config",
                config,
            ],
        )
    };
    for config in [
        r#"{"url":"/media/tmp/01abcdefghij.mp4"}"#,
        r#"{"url":"/media/../maru.db.mp4"}"#,
        r#"{"url":"/media/2026/11/a.mov"}"#,
    ] {
        assert_eq!(place(config)["ok"], false, "{config}");
    }
    let v = place(&format!(r#"{{"url":"{small_url}","loop_muted":true}}"#));
    assert_eq!(v["ok"], true, "{v}");

    // 본문 `::video[/media/…]{autoplay}`: 저절로 재생은 소리를 끈다.
    let body = format!("영상\n\n::video[{small_url}]{{autoplay}}\n");
    let post = maru(
        &data,
        &[
            "post",
            "new",
            "--board",
            "notice",
            "--title",
            "영상 글",
            "--slug",
            "clip",
            "--body",
            &body,
            "--publish",
            "--yes",
        ],
    );
    assert_eq!(post["ok"], true, "{post}");

    let list = maru(&data, &["media", "list", "--kind", "video"]);
    assert_eq!(list["data"]["total"], 1, "{list}");
    let list = maru(&data, &["media", "list", "--kind", "image"]);
    assert_eq!(list["data"]["total"], 0, "{list}");

    let (_server, bind) = start_server(&data);

    let home = http(&bind, "GET", "/", &[], b"").text();
    assert!(
        home.contains(&format!("<video src=\"{small_url}\"")),
        "{home}"
    );
    assert!(home.contains(" loop muted autoplay"), "{home}");
    let page = http(&bind, "GET", "/b/notice/clip", &[], b"");
    assert_eq!(page.status, 200, "{}", page.text());
    let html = page.text();
    assert!(
        html.contains("<div class=\"video video-file\"><video"),
        "{html}"
    );
    assert!(html.contains("muted"), "{html}");
    assert!(html.contains("autoplay"), "{html}");

    // 범위 없는 요청은 전체(200), 범위는 206, 범위 밖은 416.
    let res = http(&bind, "GET", &small_url, &[], b"");
    assert_eq!(res.status, 200);
    assert_eq!(res.header("content-type").as_deref(), Some("video/mp4"));
    assert_eq!(res.header("accept-ranges").as_deref(), Some("bytes"));
    assert_eq!(res.body, small);
    let res = http(&bind, "GET", &small_url, &[("Range", "bytes=10-19")], b"");
    assert_eq!(res.status, 206);
    assert_eq!(
        res.header("content-range").as_deref(),
        Some("bytes 10-19/1000")
    );
    assert_eq!(res.body, small[10..20]);
    let res = http(&bind, "GET", &small_url, &[("Range", "bytes=-100")], b"");
    assert_eq!(res.status, 206);
    assert_eq!(
        res.header("content-range").as_deref(),
        Some("bytes 900-999/1000")
    );
    let res = http(&bind, "GET", &small_url, &[("Range", "bytes=5000-")], b"");
    assert_eq!(res.status, 416);
    assert_eq!(res.header("content-range").as_deref(), Some("bytes */1000"));

    // REST 나눠 올리기: 같은 조각을 다시 보내도 안전하고, 빈틈이 생기면 409.
    let json_auth = [
        ("Authorization", bearer.as_str()),
        ("Content-Type", "application/json"),
    ];
    let res = http(
        &bind,
        "POST",
        "/api/v1/media/videos",
        &[("Content-Type", "application/json")],
        br#"{"filename":"a.mp4"}"#,
    );
    assert_eq!(res.status, 401, "{}", res.text());
    let res = http(
        &bind,
        "POST",
        "/api/v1/media/videos",
        &json_auth,
        br#"{"filename":"a.mov"}"#,
    );
    assert_eq!(res.status, 400, "{}", res.text());

    const MB: usize = 1024 * 1024;
    let big = mp4(5 * MB + 123);
    let res = http(
        &bind,
        "POST",
        "/api/v1/media/videos",
        &json_auth,
        r#"{"filename":"바다 영상.mp4"}"#.as_bytes(),
    );
    assert_eq!(res.status, 201, "{}", res.text());
    let id = res.json()["data"]["id"].as_str().unwrap().to_owned();
    let auth = [("Authorization", bearer.as_str())];
    let chunk = |offset: usize, bytes: &[u8]| {
        http(
            &bind,
            "PUT",
            &format!("/api/v1/media/videos/{id}?offset={offset}"),
            &auth,
            bytes,
        )
    };
    let res = chunk(0, &big[..3 * MB]);
    assert_eq!(res.status, 200, "{}", res.text());
    assert_eq!(res.json()["data"]["received"], 3 * MB);
    let res = chunk(0, &big[..3 * MB]);
    assert_eq!(res.json()["data"]["received"], 3 * MB, "다시 보내기");
    let res = chunk(4 * MB, &big[4 * MB..]);
    assert_eq!(res.status, 409, "{}", res.text());
    // 앞부분이 겹친 조각도 겹친 만큼 건너뛴다.
    let res = chunk(2 * MB, &big[2 * MB..]);
    assert_eq!(res.status, 200, "{}", res.text());
    assert_eq!(res.json()["data"]["received"], big.len());
    let res = http(
        &bind,
        "POST",
        &format!("/api/v1/media/videos/{id}/finish"),
        &json_auth,
        r#"{"filename":"바다 영상.mp4","alt":"파도"}"#.as_bytes(),
    );
    assert_eq!(res.status, 201, "{}", res.text());
    let m = res.json()["data"].clone();
    assert_eq!(m["size"], big.len());
    assert_eq!(m["alt_text"], "파도");
    let big_url = m["url"].as_str().unwrap().to_owned();
    assert!(!big_url.contains("바다"));
    // 임시 조각 파일은 공개되지 않는다.
    let tmp_path = format!("/media/tmp/{id}.part");
    assert_eq!(http(&bind, "GET", &tmp_path, &[], b"").status, 404);

    // 큰 파일: 범위 없으면 전체를 흘려보내고, 범위 하나는 4MB 까지만 준다.
    let res = http(&bind, "GET", &big_url, &[], b"");
    assert_eq!(res.status, 200);
    assert_eq!(res.body.len(), big.len());
    assert!(res.body == big, "전체 내용이 같다");
    let res = http(&bind, "GET", &big_url, &[("Range", "bytes=0-")], b"");
    assert_eq!(res.status, 206);
    assert_eq!(
        res.header("content-range"),
        Some(format!("bytes 0-{}/{}", 4 * MB - 1, big.len()))
    );
    assert!(res.body == big[..4 * MB]);

    // 머리가 동영상이 아니면 마무리에서 거절하고 조각을 지운다.
    let res = http(
        &bind,
        "POST",
        "/api/v1/media/videos",
        &json_auth,
        br#"{"filename":"b.webm"}"#,
    );
    let bad_id = res.json()["data"]["id"].as_str().unwrap().to_owned();
    let res = http(
        &bind,
        "PUT",
        &format!("/api/v1/media/videos/{bad_id}?offset=0"),
        &auth,
        b"not a webm at all",
    );
    assert_eq!(res.status, 200, "{}", res.text());
    let finish = |id: &str| {
        http(
            &bind,
            "POST",
            &format!("/api/v1/media/videos/{id}/finish"),
            &json_auth,
            br#"{"filename":"b.webm"}"#,
        )
    };
    assert_eq!(finish(&bad_id).status, 400);
    assert_eq!(finish(&bad_id).status, 404, "조각이 지워졌다");

    // 관리 화면(세션 + CSRF 머리): 같은 서비스.
    let (cookie, csrf) = admin_session(&data, &bind);
    let admin = [
        ("Cookie", cookie.as_str()),
        ("x-csrf-token", csrf.as_str()),
        ("Content-Type", "application/json"),
    ];
    let res = http(
        &bind,
        "POST",
        "/admin/media/videos",
        &admin,
        br#"{"filename":"c.mp4"}"#,
    );
    assert_eq!(res.status, 201, "{}", res.text());
    let aid = res.json()["data"]["id"].as_str().unwrap().to_owned();
    let res = http(
        &bind,
        "PUT",
        &format!("/admin/media/videos/{aid}?offset=0"),
        &admin[..2],
        &small,
    );
    assert_eq!(res.status, 200, "{}", res.text());
    let res = http(
        &bind,
        "POST",
        &format!("/admin/media/videos/{aid}/finish"),
        &admin,
        br#"{"filename":"c.mp4"}"#,
    );
    assert_eq!(res.status, 201, "{}", res.text());
    // CSRF 머리가 없으면 거절.
    let res = http(
        &bind,
        "POST",
        "/admin/media/videos",
        &[admin[0], admin[2]],
        br#"{"filename":"c.mp4"}"#,
    );
    assert_eq!(res.status, 403, "{}", res.text());

    // 목록: 종류로 거른다. 편집기 고르기 창도 같은 서비스.
    let list = http(&bind, "GET", "/api/v1/media?kind=video", &auth, b"").json();
    assert_eq!(list["data"]["total"], 3, "{list}");
    assert!(
        list["data"]["items"]
            .as_array()
            .unwrap()
            .iter()
            .all(|m| m["mime"].as_str().unwrap().starts_with("video/")),
        "{list}"
    );
    let list = http(&bind, "GET", "/api/v1/media?kind=image", &auth, b"").json();
    assert_eq!(list["data"]["total"], 0, "{list}");
    let picker = http(
        &bind,
        "GET",
        "/admin/editor/media?kind=video",
        &admin[..2],
        b"",
    );
    assert_eq!(picker.status, 200, "{}", picker.text());
    assert!(picker.text().contains(&small_url), "{}", picker.text());
}
