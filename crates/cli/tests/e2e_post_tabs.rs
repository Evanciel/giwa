//! GAP Q3: 글 탭 위젯 (ADR-0088). 실제 바이너리·실제 포트·실제 DB 파일로 설정 검사 → 첫 화면의
//! 탭 마크업(JS 없이도 읽힘)·손님이 못 읽는 게시판 탭 빼기·스크립트 → 빼기를 확인한다.
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

fn post(data: &Path, board: &str, title: &str) {
    maru(
        data,
        &[
            "post",
            "new",
            "--board",
            board,
            "--title",
            title,
            "--body",
            "본문",
            "--publish",
        ],
    );
}

#[test]
fn post_tabs_group_lists_and_skip_unreadable_boards() {
    let tmp = tempfile::tempdir().unwrap();
    let data = tmp.path().join("사이트");
    maru(
        &data,
        &["init", "--no-samples", "--preset", "blog", "--yes"],
    );
    maru(
        &data,
        &[
            "board",
            "create",
            "vip",
            "--name",
            "회원방",
            "--kind",
            "free",
        ],
    );
    maru(&data, &["board", "update", "vip", "--read", "member"]);
    post(&data, "blog", "블로그 새 소식");
    post(&data, "vip", "회원만 읽는 소식");

    let types = maru(&data, &["widget", "types"]);
    assert!(
        types
            .as_array()
            .unwrap()
            .iter()
            .any(|t| t["id"] == "post_tabs" && t["config_schema"].is_object()),
        "{types}"
    );

    let place = |config: &str| {
        run(
            &data,
            &[
                "widget",
                "place",
                "post_tabs",
                "--region",
                "sidebar",
                "--title",
                "글 모음",
                "--config",
                config,
            ],
        )
    };
    // 잘못된 설정은 이유와 함께 거부한다.
    for (config, why) in [
        (r#"{"tabs":[{"label":"하나"}]}"#, "tabs"),
        (
            r#"{"tabs":[{"label":"같음"},{"label":"같음"}]}"#,
            "겹칩니다",
        ),
        (
            r#"{"tabs":[{"label":"가"},{"label":"나","source":"random"}]}"#,
            "source",
        ),
        (
            r#"{"tabs":[{"label":"가"},{"label":"나","board":"없는판"}]}"#,
            "없는판",
        ),
        (
            r#"{"tabs":[{"label":"가"},{"label":"나"}],"pages":9}"#,
            "pages",
        ),
    ] {
        let bad = place(config);
        assert_eq!(bad["ok"], false, "{config}");
        assert!(bad["error"]["message"].to_string().contains(why), "{bad}");
    }

    let config = r#"{"tabs":[
        {"label":"블로그","board":"blog"},
        {"label":"회원방","board":"vip"},
        {"label":"인기","source":"popular","days":0}
    ],"limit":5,"pages":2}"#;
    let w = place(config);
    assert_eq!(w["ok"], true, "{w}");
    assert_eq!(
        w["data"]["config"]["tabs"][0]["source"], "latest",
        "기본값을 채운다"
    );
    let again = place(config);
    assert_eq!(
        again["data"]["id"], w["data"]["id"],
        "같은 설정이면 새로 놓지 않는다"
    );

    let (_server, info) = start_server(&data);
    let bind = info["bind"].as_str().unwrap();
    let (status, home) = get(bind, "/");
    assert_eq!(status, 200);
    let at = home.find("data-tabs").expect("글 탭 상자");
    let tabs = &home[at..at + home[at..].find("tabs.js").expect("탭 스크립트")];
    // JS 가 없을 때를 위해 탭 목록은 숨기고 목록마다 제목을 단다.
    assert!(
        tabs.contains("role=\"tablist\" aria-label=\"글 모음\" hidden"),
        "{tabs}"
    );
    assert!(
        tabs.contains(">블로그</button>") && tabs.contains(">인기</button>"),
        "{tabs}"
    );
    assert!(tabs.contains("aria-selected=\"true\""), "{tabs}");
    assert!(
        tabs.contains("<h3 class=\"tab-panel-title\">블로그</h3>"),
        "{tabs}"
    );
    assert_eq!(
        tabs.matches("role=\"tabpanel\"").count(),
        2,
        "손님이 못 읽는 회원방 탭은 뺀다"
    );
    assert!(
        !tabs.contains("회원방") && !tabs.contains("회원만 읽는 소식"),
        "{tabs}"
    );
    assert!(tabs.contains("블로그 새 소식"), "{tabs}");
    // 쪽 넘김 (GAP Q29): 탭 칸마다 limit 개씩 나눌 수 있게 표시한다.
    assert_eq!(tabs.matches("data-page-size=\"5\"").count(), 2, "{tabs}");
    assert!(
        tabs.contains("<time class=\"date\""),
        "날짜를 보인다: {tabs}"
    );
    assert!(tabs.contains("href=\"/b/blog\""), "게시판 탭엔 더 보기");

    let (status, js) = get(bind, "/assets/tabs.js");
    assert_eq!(status, 200);
    assert!(
        js.contains("aria-selected") && js.contains("ArrowRight"),
        "{js}"
    );
    assert!(!js.contains("setInterval"), "저절로 넘어가지 않는다");

    // 끄면 사라진다.
    let id = w["data"]["id"].as_str().unwrap();
    maru(&data, &["widget", "remove", id]);
    assert!(!get(bind, "/").1.contains("data-tabs"));
}
