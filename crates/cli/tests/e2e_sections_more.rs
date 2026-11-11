//! W1 새 섹션 6종과 구역 이름 (ADR-0156). 실제 바이너리·실제 포트·실제 DB 파일로
//! CLI 설정 검사 → 첫 화면 HTML(동영상 재생기 주소·구역 id·SNS 주소·남은 날·빈 사진 전시 빼기)을 확인한다.
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

fn place(data: &Path, kind: &str, config: &str) -> Value {
    run(
        data,
        &[
            "widget", "place", kind, "--region", "main", "--scope", "home", "--config", config,
        ],
    )
}

#[test]
fn new_sections_render_and_reject_bad_values() {
    let tmp = tempfile::tempdir().unwrap();
    let data = tmp.path().join("사이트");
    maru(
        &data,
        &["init", "--no-samples", "--preset", "company", "--yes"],
    );

    let types = maru(&data, &["widget", "types"]);
    for kind in ["video", "gallery", "logos", "countdown", "social", "quote"] {
        let t = types
            .as_array()
            .unwrap()
            .iter()
            .find(|t| t["id"] == kind)
            .unwrap_or_else(|| panic!("{kind} 종류가 없습니다"));
        assert!(
            t["config_schema"]["properties"]["anchor"].is_object(),
            "{kind}: {t}"
        );
    }

    // 잘못된 값은 이유와 함께 거절한다.
    for (kind, config) in [
        ("video", r#"{"url":"https://example.com/a.mp4"}"#),
        (
            "social",
            r#"{"items":[{"service":"instagram","url":"javascript:alert(1)"}]}"#,
        ),
        (
            "social",
            r#"{"items":[{"service":"myspace","url":"https://example.com"}]}"#,
        ),
        ("countdown", r#"{"target":"다음 주"}"#),
        ("quote", r#"{"text":"한 문장","anchor":"오시는길"}"#),
    ] {
        let v = place(&data, kind, config);
        assert_eq!(v["ok"], false, "{kind} {config} → {v}");
    }

    for (kind, config) in [
        (
            "video",
            r#"{"url":"https://www.youtube.com/watch?v=dQw4w9WgXcQ","ratio":"4x3"}"#,
        ),
        (
            "social",
            r#"{"anchor":"contact","items":[{"service":"email","url":"hello@example.com"},{"service":"instagram","url":"https://www.instagram.com/example"}]}"#,
        ),
        // 같은 구역 이름은 뒤의 블록에 id 를 달지 않는다.
        (
            "quote",
            r#"{"text":"늘 처음처럼","name":"대표","anchor":"contact"}"#,
        ),
        // 사진을 하나도 고르지 않은 사진 전시는 공개 화면에 보이지 않는다.
        ("gallery", r#"{"items":[{"caption":"사진은 나중에"}]}"#),
        ("logos", r#"{"items":[{"name":"함께하는 회사"}]}"#),
        (
            "countdown",
            r#"{"target":"2099-01-01T00:00:00+09:00","text":"개업까지"}"#,
        ),
        (
            "countdown",
            r#"{"target":"2001-01-01T00:00:00+09:00","done":"행사가 끝났습니다"}"#,
        ),
    ] {
        let v = place(&data, kind, config);
        assert_eq!(v["ok"], true, "{kind} {config} → {v}");
    }

    let (_server, info) = start_server(&data);
    let bind = info["bind"].as_str().unwrap();
    let (status, html) = get(bind, "/");
    assert_eq!(status, 200);
    assert!(
        html.contains("https://www.youtube-nocookie.com/embed/dQw4w9WgXcQ"),
        "동영상 재생기"
    );
    assert!(html.contains("vid-frame r-4x3"), "비율");
    assert_eq!(
        html.matches(r#"id="s-contact""#).count(),
        1,
        "구역 id 는 하나"
    );
    assert!(
        html.contains(r#"href="mailto:hello@example.com""#),
        "메일 주소"
    );
    assert!(html.contains("인스타그램"), "SNS 단추 글자");
    assert!(html.contains("늘 처음처럼"), "큰 인용");
    assert!(!html.contains("사진은 나중에"), "빈 사진 전시는 빠진다");
    assert!(html.contains("함께하는 회사"), "로고 이름 글자");
    assert!(html.contains("data-countdown="), "남은 날 스크립트 표시");
    assert!(html.contains("개업까지"));
    assert!(html.contains("행사가 끝났습니다"), "지난 남은 날");
}

const PNG: &[u8] = &[
    137, 80, 78, 71, 13, 10, 26, 10, 0, 0, 0, 13, 73, 72, 68, 82, 0, 0, 0, 1, 0, 0, 0, 1, 8, 6, 0,
    0, 0, 31, 21, 196, 137, 0, 0, 0, 13, 73, 68, 65, 84, 120, 218, 99, 100, 96, 248, 95, 15, 0, 2,
    135, 1, 128, 235, 71, 186, 146, 0, 0, 0, 0, 73, 69, 78, 68, 174, 66, 96, 130,
];

/// W2 섹션 배경 그림·등장 효과 (ADR-0157).
#[test]
fn section_backgrounds_and_motion() {
    let tmp = tempfile::tempdir().unwrap();
    let data = tmp.path().join("사이트");
    maru(
        &data,
        &["init", "--no-samples", "--preset", "company", "--yes"],
    );
    let file = tmp.path().join("배경.png");
    std::fs::write(&file, PNG).unwrap();
    let up = maru(&data, &["media", "upload", file.to_str().unwrap()]);
    let url = up["url"].as_str().unwrap().to_owned();

    for (kind, config) in [
        (
            "quote",
            r#"{"text":"한 문장","bg_image":"https://example.com/a.jpg"}"#.to_owned(),
        ),
        ("quote", r#"{"text":"한 문장","motion":"spin"}"#.to_owned()),
        (
            "quote",
            format!(r#"{{"text":"한 문장","bg_image":"{url}","bg_dim":"black"}}"#),
        ),
    ] {
        let v = place(&data, kind, &config);
        assert_eq!(v["ok"], false, "{config} → {v}");
    }
    for config in [
        format!(
            r#"{{"text":"배경 있는 인용","bg_image":"{url}","bg_dim":"strong","motion":"rise"}}"#
        ),
        // 보관소에 없는 그림은 배경을 그리지 않는다.
        r#"{"text":"없는 그림","bg_image":"/media/2000/01/none.png"}"#.to_owned(),
    ] {
        let v = place(&data, "quote", &config);
        assert_eq!(v["ok"], true, "{config} → {v}");
    }

    let (_server, info) = start_server(&data);
    let (status, html) = get(info["bind"].as_str().unwrap(), "/");
    assert_eq!(status, 200);
    assert_eq!(
        html.matches("has-bg dim-strong").count(),
        1,
        "배경 띠는 하나"
    );
    assert!(html.contains("motion-rise"), "등장 효과");
    assert!(
        html.contains(&format!(r#"<img class="sec-bg" src="{url}""#)),
        "배경 그림"
    );
    assert!(!html.contains("/media/2000/01/none.png"), "없는 그림");
}
