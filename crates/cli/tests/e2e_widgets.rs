//! GAP S1: 위젯 (ADR-0059). 실제 바이너리·실제 포트·실제 DB 파일로 CLI 위젯 놓기 → 공개 화면,
//! 범위(첫 화면·게시판·페이지), 끄기·빼기 → 화면과 DB 를 확인한다.
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

fn widget_rows(data: &Path) -> i64 {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(async {
        let db = maru_storage::Db::open(&data.join("maru.db"), false)
            .await
            .unwrap();
        let n: i64 = sqlx::query_scalar("SELECT count(*) FROM widgets")
            .fetch_one(db.read_pool())
            .await
            .unwrap();
        db.close().await;
        n
    })
}

#[test]
fn widgets_render_by_scope_and_disappear_when_turned_off() {
    let tmp = tempfile::tempdir().unwrap();
    let data = tmp.path().join("사이트");
    maru(&data, &["init", "--preset", "blog", "--yes"]);
    // 프리셋의 소개 페이지는 초안으로 만들어진다.
    maru(&data, &["page", "publish", "about"]);
    for (title, body) in [("첫 소식", "하나"), ("둘째 소식", "둘")] {
        maru(
            &data,
            &["post", "new", "--publish", "--title", title, "--body", body],
        );
    }

    let types = maru(&data, &["widget", "types"]);
    assert_eq!(types.as_array().unwrap().len(), 10);
    assert!(types[0]["config_schema"].is_object(), "{types}");

    let bad = run(
        &data,
        &[
            "widget",
            "place",
            "latest_posts",
            "--region",
            "sidebar",
            "--config",
            r#"{"limit":99}"#,
        ],
    );
    assert_eq!(bad["ok"], false);
    assert!(
        bad["error"]["message"].as_str().unwrap().contains("limit"),
        "{bad}"
    );
    assert!(
        bad["error"]["next"].to_string().contains("widget types"),
        "{bad}"
    );

    let latest = maru(
        &data,
        &[
            "widget",
            "place",
            "latest_posts",
            "--region",
            "sidebar",
            "--title",
            "새 글",
            "--config",
            r#"{"board":"blog","limit":5}"#,
        ],
    );
    let again = maru(
        &data,
        &[
            "widget",
            "place",
            "latest_posts",
            "--region",
            "sidebar",
            "--title",
            "새 글",
            "--config",
            r#"{"board":"blog","limit":5}"#,
        ],
    );
    assert_eq!(latest["id"], again["id"], "같은 설정은 한 번만 놓인다");
    maru(
        &data,
        &[
            "widget",
            "place",
            "markdown_block",
            "--region",
            "main",
            "--scope",
            "home",
            "--config",
            r#"{"body":"**첫 화면** 안내 <script>x()</script>"}"#,
        ],
    );
    let page_only = maru(
        &data,
        &[
            "widget",
            "place",
            "notice_bar",
            "--region",
            "header",
            "--scope",
            "page:about",
            "--config",
            r#"{"text":"소개 페이지 알림","link":"/"}"#,
        ],
    );
    assert_eq!(widget_rows(&data), 3);

    let (_server, info) = start_server(&data);
    let bind = info["bind"].as_str().unwrap().to_owned();

    let (status, home) = get(&bind, "/");
    assert_eq!(status, 200);
    assert!(
        home.contains("class=\"with-sidebar"),
        "곁가지가 있으면 두 단"
    );
    assert!(home.contains("aria-labelledby=\"w-"), "{home}");
    assert!(home.contains(">새 글</h2>"));
    assert!(home.contains("둘째 소식") && home.contains("첫 소식"));
    assert!(home.contains("<strong>첫 화면</strong> 안내"));
    assert!(!home.contains("<script>x()"), "글 상자도 정화한다");
    assert!(!home.contains("소개 페이지 알림"), "범위 밖");

    let (_, board) = get(&bind, "/b/blog");
    assert!(board.contains(">새 글</h2>"), "all 범위는 게시판에도");
    assert!(
        !board.contains("<strong>첫 화면</strong>"),
        "home 범위는 첫 화면만"
    );

    let (_, about) = get(&bind, "/p/about");
    assert!(about.contains("소개 페이지 알림"));
    assert!(about.contains("role=\"note\""));

    let id = page_only["id"].as_str().unwrap();
    maru(&data, &["widget", "configure", id, "--disable"]);
    let (_, about) = get(&bind, "/p/about");
    assert!(!about.contains("소개 페이지 알림"), "끈 위젯은 빠진다");

    let list = maru(&data, &["widget", "list"]);
    assert_eq!(list.as_array().unwrap().len(), 3);
    for w in list.as_array().unwrap() {
        maru(&data, &["widget", "remove", w["id"].as_str().unwrap()]);
    }
    assert_eq!(widget_rows(&data), 0);
    let (_, home) = get(&bind, "/");
    assert!(!home.contains("with-sidebar"), "곁가지가 없으면 한 단");
    assert!(!home.contains(">새 글</h2>"));

    let gone = run(&data, &["widget", "remove", id]);
    assert_eq!(gone["ok"], false);
    assert_eq!(gone["error"]["code"], "NOT_FOUND", "{gone}");
}

#[test]
fn popups_render_hidden_with_script_only_in_period_and_scope() {
    let tmp = tempfile::tempdir().unwrap();
    let data = tmp.path().join("사이트");
    maru(&data, &["init", "--preset", "blog", "--yes"]);
    let bad = run(
        &data,
        &[
            "widget",
            "place",
            "popup",
            "--region",
            "main",
            "--config",
            r#"{"body":"x"}"#,
        ],
    );
    maru(
        &data,
        &[
            "post",
            "new",
            "--publish",
            "--title",
            "사진 글",
            "--body",
            "본문",
        ],
    );
    assert_eq!(bad["ok"], false, "팝업은 popup 영역에만");
    maru(
        &data,
        &[
            "widget",
            "place",
            "popup",
            "--region",
            "popup",
            "--scope",
            "home",
            "--title",
            "점검 안내",
            "--config",
            r#"{"body":"**11일** 새벽 점검","dismiss_days":3,"position":"bottom-right","ends_at":"2099-01-01T00:00:00Z"}"#,
        ],
    );
    maru(
        &data,
        &[
            "widget",
            "place",
            "popup",
            "--region",
            "popup",
            "--config",
            r#"{"body":"지난 행사","ends_at":"2020-01-01T00:00:00Z"}"#,
        ],
    );
    // 사진 모음 열 수는 style 속성이 아니라 클래스로 (CSP style-src 'self')
    maru(
        &data,
        &[
            "widget",
            "place",
            "gallery_grid",
            "--region",
            "main",
            "--scope",
            "home",
            "--config",
            r#"{"board":"blog","columns":3}"#,
        ],
    );

    let (_server, info) = start_server(&data);
    let bind = info["bind"].as_str().unwrap().to_owned();
    let (_, home) = get(&bind, "/");
    assert!(home.contains("role=\"dialog\""), "{home}");
    assert!(home.contains("data-dismiss-days=\"3\" hidden>"));
    assert!(home.contains("3일 동안 보지 않기"));
    assert!(home.contains("popup-bottom-right"));
    assert!(home.contains("<strong>11일</strong> 새벽 점검"));
    assert!(home.contains("src=\"/assets/site.js\""));
    assert!(!home.contains("지난 행사"), "기간이 끝난 팝업은 빠진다");
    assert!(home.contains("widget-gallery cols-3"));
    assert!(
        !home.contains("style=\""),
        "CSP 가 막는 style 속성을 쓰지 않는다"
    );

    let (_, board) = get(&bind, "/b/blog");
    assert!(!board.contains("role=\"dialog\""), "home 범위");
    assert!(!board.contains("site.js"), "팝업이 없으면 스크립트도 없다");

    let (status, js) = get(&bind, "/assets/site.js");
    assert_eq!(status, 200);
    assert!(js.contains("maru-popup:"));
}
