//! 광고 자리(ADR-0079)·상담 버튼(ADR-0080). 실제 바이너리·실제 포트·실제 DB 파일로
//! 자체 배너는 기본 CSP 를 그대로 두고, 애드센스 위젯이 있는 화면에만 nonce 정책이 걸리며 화면의
//! 모든 스크립트가 그 nonce 를 다는지, 상담 버튼이 주소와 외부 이동 안내를 같이 그리는지 본다.
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

struct Res {
    head: String,
    body: String,
}

impl Res {
    fn header(&self, name: &str) -> String {
        self.head
            .lines()
            .find_map(|l| {
                let (k, v) = l.split_once(':')?;
                k.eq_ignore_ascii_case(name).then(|| v.trim().to_owned())
            })
            .unwrap_or_default()
    }
}

fn get(bind: &str, path: &str) -> Res {
    let mut s = TcpStream::connect(bind).unwrap();
    let req = format!("GET {path} HTTP/1.1\r\nHost: {bind}\r\nConnection: close\r\n\r\n");
    s.write_all(req.as_bytes()).unwrap();
    let mut raw = Vec::new();
    s.read_to_end(&mut raw).unwrap();
    let text = String::from_utf8_lossy(&raw).into_owned();
    let (head, body) = text.split_once("\r\n\r\n").unwrap_or((&text, ""));
    assert!(head.starts_with("HTTP/1.1 200"), "{path}: {head}");
    Res {
        head: head.to_owned(),
        body: body.to_owned(),
    }
}

fn place(data: &Path, kind: &str, region: &str, scope: &str, config: &str) -> Value {
    run(
        data,
        &[
            "widget", "place", kind, "--region", region, "--scope", scope, "--config", config,
        ],
    )
}

#[test]
fn ad_slots_scope_the_strict_csp_and_consult_button_tells_where_it_goes() {
    let tmp = tempfile::tempdir().unwrap();
    let data = tmp.path().join("사이트");
    maru(&data, &["init", "--preset", "blog", "--yes"]);
    maru(
        &data,
        &[
            "post",
            "new",
            "--title",
            "첫 글",
            "--body",
            "본문",
            "--publish",
        ],
    );

    // 꺼진 모듈의 위젯 종류는 모른다.
    let off = place(
        &data,
        "consult_button",
        "footer",
        "all",
        r#"{"channel":"kakao","value":"_abc"}"#,
    );
    assert_eq!(off["ok"], false, "{off}");
    maru(&data, &["module", "enable", "consult"]);
    maru(&data, &["module", "enable", "ads"]);

    // 설정 검사는 모듈 몫이다.
    let bad = place(
        &data,
        "consult_button",
        "footer",
        "all",
        r#"{"channel":"kakao","value":"abc"}"#,
    );
    assert_eq!(bad["ok"], false);
    assert!(
        bad["error"]["message"]
            .as_str()
            .unwrap()
            .contains("채널 ID"),
        "{bad}"
    );
    let bad = place(
        &data,
        "ad_slot",
        "sidebar",
        "all",
        r#"{"provider":"adsense","client":"pub-1","slot":"1"}"#,
    );
    assert_eq!(bad["ok"], false);
    let ok = place(
        &data,
        "consult_button",
        "footer",
        "all",
        r#"{"channel":"kakao","value":"_abC12","floating":true,"note":"평일 10~18시"}"#,
    );
    assert_eq!(ok["ok"], true, "{ok}");
    let ok = place(
        &data,
        "ad_slot",
        "sidebar",
        "home",
        r#"{"provider":"house","image":"/media/2026/11/a.jpg","link":"https://shop.example.test/","alt":"봄 행사"}"#,
    );
    assert_eq!(ok["ok"], true, "{ok}");
    let ok = place(
        &data,
        "ad_slot",
        "sidebar",
        "boards",
        r#"{"provider":"adsense","client":"ca-pub-1234567890123456","slot":"1234567890"}"#,
    );
    assert_eq!(ok["ok"], true, "{ok}");

    let (_server, bind) = start_server(&data);

    // 첫 화면: 자체 배너뿐이라 기본 CSP. 상담 버튼과 안내가 보인다.
    let home = get(&bind, "/");
    let csp = home.header("content-security-policy");
    assert!(csp.starts_with("default-src 'self'"), "{csp}");
    assert!(
        home.body.contains("rel=\"noopener noreferrer sponsored\""),
        "{}",
        home.body
    );
    assert!(home.body.contains(">광고<"));
    assert!(home.body.contains("https://pf.kakao.com/_abC12/chat"));
    assert!(home.body.contains("consult-floating"));
    assert!(home.body.contains("카카오톡 채널 1:1 채팅으로 이동합니다"));
    assert!(home.body.contains("평일 10~18시"));
    assert!(!home.body.contains("adsbygoogle"));
    assert!(!home.body.contains("nonce="));

    // 게시판 화면: 애드센스가 있어 이 응답만 nonce 정책. 화면의 모든 스크립트가 같은 nonce 를 단다.
    let board = get(&bind, "/b/blog");
    let csp = board.header("content-security-policy");
    assert!(csp.contains("'strict-dynamic'"), "{csp}");
    assert!(csp.contains("object-src 'none'") && csp.contains("frame-ancestors 'self'"));
    let at = csp.find("'nonce-").unwrap() + 7;
    let nonce = &csp[at..at + csp[at..].find('\'').unwrap()];
    assert_eq!(nonce.len(), 32);
    let scripts = board.body.matches("<script").count();
    assert!(scripts >= 2, "{}", board.body);
    assert_eq!(
        board.body.matches(&format!("nonce=\"{nonce}\"")).count(),
        scripts,
        "모든 스크립트에 nonce"
    );
    assert!(
        board
            .body
            .contains("data-ad-client=\"ca-pub-1234567890123456\"")
    );
    // 응답마다 nonce 가 다르다.
    let again = get(&bind, "/b/blog").header("content-security-policy");
    assert_ne!(again, csp);

    // 모듈을 끄면 위젯과 정책이 함께 빠진다.
    maru(&data, &["module", "disable", "ads"]);
    let board = get(&bind, "/b/blog");
    assert!(
        board
            .header("content-security-policy")
            .starts_with("default-src 'self'")
    );
    assert!(!board.body.contains("adsbygoogle"));
}
