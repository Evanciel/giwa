//! 미디어 보관함 (G7 #14, ADR-0172): 쓰는 곳 찾기, 안 쓰는 것만 보기, 쓰는 곳이 있으면 지우기 거절.
//! CLI·REST·관리 화면 세 경로를 실제 바이너리·실제 포트·실제 DB 파일로 본다.
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

fn upload(data: &Path, dir: &Path, name: &str, alt: &str) -> Value {
    let file = dir.join(name);
    std::fs::write(&file, jpeg(900, 500)).unwrap();
    let up = maru(
        data,
        &["media", "upload", file.to_str().unwrap(), "--alt", alt],
    );
    assert_eq!(up["ok"], true, "{up}");
    up["data"].clone()
}

fn form(bind: &str, path: &str, cookie: &str, body: &str) -> Res {
    http(
        bind,
        "POST",
        path,
        &[
            ("Content-Type", "application/x-www-form-urlencoded"),
            ("Cookie", cookie),
            ("Origin", &format!("http://{bind}")),
        ],
        body.as_bytes(),
    )
}

#[test]
fn library_finds_uses_and_guards_delete() {
    let tmp = tempfile::tempdir().unwrap();
    let data = tmp.path().join("사이트");
    let init = maru(&data, &["init", "--no-samples", "--yes"]);
    let token = init["data"]["token"]["secret"].as_str().unwrap().to_owned();
    let bearer = format!("Bearer {token}");

    let a = upload(&data, tmp.path(), "a.jpg", "노란 벽");
    let b = upload(&data, tmp.path(), "b.jpg", "골목");
    let c = upload(&data, tmp.path(), "c.jpg", "남는 사진");
    let (a_id, b_id, c_id) = (
        a["id"].as_str().unwrap().to_owned(),
        b["id"].as_str().unwrap().to_owned(),
        c["id"].as_str().unwrap().to_owned(),
    );
    // 글은 원본 주소, 페이지는 축소본 주소로 쓴다. 축소본도 같은 미디어를 쓰는 곳이다.
    let a_url = a["url"].as_str().unwrap();
    let b_small = b["variants"][0]["url"].as_str().unwrap();
    let post = maru(
        &data,
        &[
            "post",
            "new",
            "--title",
            "가을",
            "--slug",
            "autumn",
            "--body",
            &format!("![노란 벽]({a_url})"),
            "--publish",
            "--yes",
        ],
    );
    assert_eq!(post["ok"], true, "{post}");
    let page = maru(
        &data,
        &[
            "page",
            "new",
            "street",
            "--title",
            "소개",
            "--body",
            &format!("![골목]({b_small})"),
        ],
    );
    assert_eq!(page["ok"], true, "{page}");

    // CLI: 쓰는 곳, 안 쓰는 것만, 찾기
    let uses = maru(&data, &["media", "usage", &a_id]);
    assert_eq!(uses["ok"], true, "{uses}");
    assert_eq!(uses["data"].as_array().unwrap().len(), 1, "{uses}");
    assert_eq!(uses["data"][0]["place"], "post");
    assert_eq!(uses["data"][0]["label"], "가을");
    let uses = maru(&data, &["media", "usage", &b_id]);
    assert_eq!(uses["data"][0]["place"], "page", "{uses}");
    let unused = maru(&data, &["media", "list", "--unused"]);
    assert_eq!(unused["data"]["total"], 1, "{unused}");
    assert_eq!(unused["data"]["items"][0]["id"], c_id.as_str());
    let found = maru(&data, &["media", "list", "--q", "노란"]);
    assert_eq!(found["data"]["total"], 1, "{found}");
    assert_eq!(found["data"]["items"][0]["uses"], 1);
    assert_eq!(found["data"]["items"][0]["used_in"][0]["place"], "post");

    // CLI: 쓰는 곳이 있으면 거절하고 다음 명령을 알려 준다. --force 면 지운다.
    let refused = maru(&data, &["media", "delete", &a_id]);
    assert_eq!(refused["ok"], false, "{refused}");
    assert_eq!(refused["error"]["code"], "CONFLICT", "{refused}");
    assert!(
        refused
            .to_string()
            .contains(&format!("maru media delete {a_id} --force")),
        "{refused}"
    );
    assert_eq!(maru(&data, &["media", "show", &a_id])["ok"], true);
    let forced = maru(&data, &["media", "delete", &a_id, "--force"]);
    assert_eq!(forced["ok"], true, "{forced}");

    let (_server, bind) = start_server(&data);
    let auth = [("Authorization", bearer.as_str())];

    // REST: 같은 서비스
    let res = http(&bind, "GET", "/api/v1/media?unused=true", &auth, b"");
    assert_eq!(res.status, 200, "{}", res.text());
    assert_eq!(res.json()["data"]["total"], 1);
    let res = http(
        &bind,
        "GET",
        &format!("/api/v1/media/{b_id}/usage"),
        &auth,
        b"",
    );
    assert_eq!(res.json()["data"][0]["place"], "page", "{}", res.text());
    let res = http(
        &bind,
        "DELETE",
        &format!("/api/v1/media/{b_id}"),
        &auth,
        b"",
    );
    // 에이전트 토큰은 원래 미디어를 지울 수 없다(post.moderate). 쓰는 곳 검사보다 권한이 먼저다.
    assert_eq!(res.status, 403, "{}", res.text());
    // 원래 목록(조건 없음)은 쓰는 곳을 세지 않는다 (그림 고르기 창이 자주 부른다).
    let res = http(&bind, "GET", "/api/v1/media", &auth, b"");
    assert!(
        res.json()["data"]["items"][0].get("uses").is_none(),
        "{}",
        res.text()
    );

    // 관리 화면
    let (cookie, csrf) = admin_session(&data, &bind);
    let res = http(&bind, "GET", "/admin/media", &[("Cookie", &cookie)], b"");
    assert_eq!(res.status, 200, "{}", res.text());
    let html = res.text();
    assert!(html.contains("href=\"/admin/media\""), "메뉴에 미디어");
    assert!(html.contains(&format!("id=\"m-{c_id}\"")), "{html}");
    assert!(html.contains("쓰는 곳 없음"), "{html}");
    assert!(html.contains("쓰는 곳 1곳"), "{html}");
    assert!(html.contains("그래도 지우기"), "{html}");
    let res = http(
        &bind,
        "GET",
        "/admin/media?unused=1",
        &[("Cookie", &cookie)],
        b"",
    );
    let html = res.text();
    assert!(html.contains(&format!("id=\"m-{c_id}\"")), "{html}");
    assert!(!html.contains(&format!("id=\"m-{b_id}\"")), "{html}");

    // 대체 텍스트 고치기: 돌아가는 곳은 보관함 안만
    let res = form(
        &bind,
        &format!("/admin/media/{c_id}/alt"),
        &cookie,
        &format!("_csrf={csrf}&alt=%EC%83%88+%EC%84%A4%EB%AA%85&back=https%3A%2F%2Fevil.test%2F"),
    );
    assert_eq!(res.status, 303, "{}", res.text());
    assert_eq!(
        res.header("location").as_deref(),
        Some("/admin/media?done=alt")
    );
    assert_eq!(
        maru(&data, &["media", "show", &c_id])["data"]["alt_text"],
        "새 설명"
    );

    // CSRF 가 틀리면 지우지 않는다.
    let res = form(
        &bind,
        &format!("/admin/media/{c_id}/delete"),
        &cookie,
        "_csrf=wrong",
    );
    assert_ne!(res.status, 303);
    assert_eq!(maru(&data, &["media", "show", &c_id])["ok"], true);

    // 쓰는 곳이 있으면 force 없이는 409 와 이유
    let res = form(
        &bind,
        &format!("/admin/media/{b_id}/delete"),
        &cookie,
        &format!("_csrf={csrf}"),
    );
    assert_eq!(res.status, 409, "{}", res.text());
    assert!(res.text().contains("1곳에서 씁니다"), "{}", res.text());
    let res = form(
        &bind,
        &format!("/admin/media/{b_id}/delete"),
        &cookie,
        &format!("_csrf={csrf}&force=1&back=%2Fadmin%2Fmedia%3Fkind%3Dimage%26"),
    );
    assert_eq!(res.status, 303, "{}", res.text());
    assert_eq!(
        res.header("location").as_deref(),
        Some("/admin/media?kind=image&done=deleted")
    );
    assert_eq!(maru(&data, &["media", "show", &b_id])["ok"], false);
    let res = form(
        &bind,
        &format!("/admin/media/{c_id}/delete"),
        &cookie,
        &format!("_csrf={csrf}"),
    );
    assert_eq!(res.status, 303, "{}", res.text());
    assert_eq!(maru(&data, &["media", "list"])["data"]["total"], 0);
}
