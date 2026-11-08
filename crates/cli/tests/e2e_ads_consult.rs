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
    maru(
        &data,
        &["init", "--no-samples", "--preset", "blog", "--yes"],
    );
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

/// ADR-0130: 키가 없으면 링크뿐, 키를 넣은 화면만 넓어지고 값은 data-* 로만 간다.
#[test]
fn 지도와_카카오는_키를_넣은_화면만_바깥을_부른다() {
    let tmp = tempfile::tempdir().unwrap();
    let data = tmp.path().join("사이트");
    maru(
        &data,
        &["init", "--no-samples", "--preset", "company", "--yes"],
    );
    maru(&data, &["module", "enable", "map"]);
    maru(&data, &["module", "enable", "kakao"]);
    maru(
        &data,
        &["page", "new", "visit", "--title", "오시는 길", "--publish"],
    );
    maru(
        &data,
        &["page", "new", "visit-map", "--title", "지도", "--publish"],
    );

    // 설정 검사는 모듈 몫이다.
    let bad = place(
        &data,
        "map",
        "main",
        "all",
        r#"{"address":"서울","provider":"kakao","key":"abc\"><script>"}"#,
    );
    assert_eq!(bad["ok"], false, "{bad}");
    let bad = place(
        &data,
        "kakao_channel",
        "main",
        "all",
        r#"{"channel":"abc"}"#,
    );
    assert_eq!(bad["ok"], false, "{bad}");

    // 키 없는 지도와 채널: 모든 화면이 기본 CSP.
    let ok = place(
        &data,
        "map",
        "footer",
        "all",
        r#"{"address":"서울 중구 세종대로 110 <b>","name":"본사"}"#,
    );
    assert_eq!(ok["ok"], true, "{ok}");
    let ok = place(
        &data,
        "kakao_channel",
        "footer",
        "all",
        r#"{"channel":"_abC12","name":"채널","chat":true}"#,
    );
    assert_eq!(ok["ok"], true, "{ok}");
    let (_server, bind) = start_server(&data);
    let home = get(&bind, "/");
    assert!(
        home.header("content-security-policy")
            .starts_with("default-src 'self'"),
        "{}",
        home.header("content-security-policy")
    );
    assert!(!home.body.contains("dapi.kakao.com"));
    assert!(!home.body.contains("kakaocdn.net"));
    assert!(home.body.contains("https://map.naver.com/p/search/"));
    assert!(home.body.contains("https://map.kakao.com/?q="));
    assert!(home.body.contains("&lt;b&gt;"), "주소는 글자로만 나간다");
    assert!(home.body.contains("https://pf.kakao.com/_abC12/chat"));

    // 키를 넣으면 그 위젯을 건 화면에서만 SDK 를 부른다.
    let widgets = maru(&data, &["widget", "list"]);
    assert!(widgets.to_string().contains("kakao_channel"), "{widgets}");
    let ok = place(
        &data,
        "map",
        "main",
        "page:visit",
        r#"{"address":"서울 중구 세종대로 110","provider":"kakao","key":"abcdef0123456789"}"#,
    );
    assert_eq!(ok["ok"], true, "{ok}");
    let ok = place(
        &data,
        "map",
        "main",
        "page:visit-map",
        r#"{"address":"서울 중구 세종대로 110","provider":"naver","key":"naverid123","lat":37.56,"lng":126.97}"#,
    );
    assert_eq!(ok["ok"], true, "{ok}");
    let ok = place(
        &data,
        "kakao_share",
        "main",
        "page:visit",
        r#"{"key":"abcdef0123456789abcdef0123456789","title":"제목 \"따옴표\""}"#,
    );
    assert_eq!(ok["ok"], true, "{ok}");
    let visit = get(&bind, "/p/visit");
    let csp = visit.header("content-security-policy");
    assert!(csp.contains("'strict-dynamic'"), "{csp}");
    assert!(
        visit
            .body
            .contains("sdk.js?appkey=abcdef0123456789&amp;libraries=services&amp;autoload=false"),
        "{}",
        visit.body
    );
    assert!(visit.body.contains("kakao_js_sdk/2.8.4/kakao.min.js"));
    assert!(visit.body.contains("integrity=\"sha384-"), "{}", visit.body);
    assert!(
        visit
            .body
            .contains("data-title=\"제목 &quot;따옴표&quot;\""),
        "{}",
        visit.body
    );
    let without_nonce = visit
        .body
        .split("<script")
        .skip(1)
        .filter(|tag| !tag.split('>').next().unwrap_or("").contains("nonce="))
        .count();
    assert_eq!(
        without_nonce, 0,
        "화면의 모든 script 에 nonce: {}",
        visit.body
    );
    let naver = get(&bind, "/p/visit-map");
    assert!(
        naver.body.contains("data-key=\"naverid123\""),
        "{}",
        naver.body
    );
    assert!(
        naver.body.contains("callback=maruNaverMap"),
        "{}",
        naver.body
    );
    assert!(naver.body.contains("data-lat=\"37.56\""), "{}", naver.body);
    // 다른 화면은 그대로다.
    assert!(
        get(&bind, "/")
            .header("content-security-policy")
            .starts_with("default-src 'self'")
    );
}
