//! GAP C8·C15: 게시판 형태·새 글·목록 번호·떠 있는 단추 (ADR-0054). 실제 바이너리·실제 포트·실제 DB 파일로
//! 종류 기본값(자유 → 표 목록, 블로그 → 웹진) → CLI `--layout gallery` 가 DB 에 남고 격자·대체 타일로 그려지는지,
//! 잘못된 형태는 거부, 24시간 지난 글엔 새 글 표시가 없는지, 글 화면에 맨 위·목록 단추가 있는지.
#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::path::Path;
use std::process::{Child, Command, Stdio};

use serde_json::Value;

const BIN: &str = env!("CARGO_BIN_EXE_giwa");

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
        .expect("giwa 실행");
    let stdout = String::from_utf8(out.stdout).expect("UTF-8 출력");
    serde_json::from_str(stdout.trim()).unwrap_or_else(|e| panic!("JSON 아님: {e}\n{stdout}"))
}

fn giwa(data: &Path, args: &[&str]) -> Value {
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

struct Res {
    status: u16,
    body: String,
}

fn http(bind: &str, method: &str, path: &str, headers: &[(&str, &str)], body: &str) -> Res {
    let mut s = TcpStream::connect(bind).unwrap();
    let mut req = format!("{method} {path} HTTP/1.1\r\nHost: {bind}\r\nConnection: close\r\n");
    for (k, v) in headers {
        req.push_str(&format!("{k}: {v}\r\n"));
    }
    req.push_str(&format!("Content-Length: {}\r\n\r\n{body}", body.len()));
    s.write_all(req.as_bytes()).unwrap();
    let mut raw = Vec::new();
    s.read_to_end(&mut raw).unwrap();
    let text = String::from_utf8(raw).unwrap();
    let (head, body) = text.split_once("\r\n\r\n").unwrap_or((&text, ""));
    Res {
        status: head[9..12].parse().unwrap(),
        body: body.to_owned(),
    }
}

fn db<T>(data: &Path, f: impl AsyncFnOnce(&giwa_storage::Db) -> T) -> T {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(async {
        let db = giwa_storage::Db::open(&data.join("giwa.db"), false)
            .await
            .unwrap();
        let v = f(&db).await;
        db.close().await;
        v
    })
}

fn board_layout(data: &Path, slug: &str) -> String {
    db(data, async |db| {
        sqlx::query_scalar("SELECT layout FROM boards WHERE slug = ?")
            .bind(slug)
            .fetch_one(db.read_pool())
            .await
            .unwrap()
    })
}

#[test]
fn board_layouts_follow_kind_then_cli_and_mark_new_posts() {
    let tmp = tempfile::tempdir().unwrap();
    let data = tmp.path().join("사이트");
    giwa(
        &data,
        &["init", "--no-samples", "--preset", "blog", "--yes"],
    );
    let free = giwa(
        &data,
        &[
            "board", "create", "free", "--name", "자유", "--kind", "free",
        ],
    );
    assert_eq!(free["layout"], "", "따로 정하지 않으면 빈 값(종류 기본값)");
    for (title, slug) in [("첫째 글", "one"), ("둘째 글", "two"), ("셋째 글", "three")] {
        giwa(
            &data,
            &[
                "post",
                "new",
                "--board",
                "free",
                "--title",
                title,
                "--slug",
                slug,
                "--publish",
            ],
        );
    }
    giwa(
        &data,
        &[
            "post",
            "new",
            "--title",
            "블로그 글",
            "--slug",
            "b1",
            "--publish",
        ],
    );
    // 첫째 글은 이틀 전에 올린 것으로: 새 글 표시가 없어야 한다.
    db(&data, async |db| {
        sqlx::query(
            "UPDATE posts SET published_at = '2020-01-01T00:00:00.000000Z' WHERE slug = 'one'",
        )
        .execute(db.write_pool())
        .await
        .unwrap();
    });

    let (_server, started) = start_server(&data);
    let bind = started["bind"].as_str().unwrap().to_owned();

    // 자유게시판은 표 목록: 번호가 전체에서 거꾸로, 넓은 화면.
    let list = http(&bind, "GET", "/b/free", &[], "");
    assert_eq!(list.status, 200, "{}", list.body);
    assert!(list.body.contains("class=\"post-table\""), "표 목록");
    assert!(list.body.contains("class=\"wrap wide\""));
    for n in ["3", "2", "1"] {
        assert!(
            list.body
                .contains(&format!("<span class=\"num\">{n}</span>")),
            "번호 {n}"
        );
    }
    assert_eq!(
        list.body.matches("class=\"new-mark\"").count(),
        2,
        "24시간 안 글 둘만"
    );
    // 블로그는 웹진(지금 모양).
    let blog = http(&bind, "GET", "/b/blog", &[], "");
    assert!(blog.body.contains("class=\"post-list\""));
    assert!(!blog.body.contains("post-table"));
    // 첫 화면은 게시판이 없어 웹진.
    let home = http(&bind, "GET", "/", &[], "");
    assert!(home.body.contains("class=\"post-list\""));

    // CLI 로 갤러리 → DB 열에 남고, 그림 없는 글은 대체 타일.
    let g = giwa(&data, &["board", "update", "free", "--layout", "gallery"]);
    assert_eq!(g["layout"], "gallery");
    assert_eq!(board_layout(&data, "free"), "gallery");
    let gallery = http(&bind, "GET", "/b/free", &[], "");
    assert!(gallery.body.contains("post-grid-gallery"), "갤러리 격자");
    assert_eq!(gallery.body.matches("class=\"tile-fallback\">").count(), 3);
    assert!(
        gallery.body.contains("class=\"tile-fallback\">첫</span>"),
        "제목 첫 글자"
    );
    let card = giwa(&data, &["board", "update", "free", "--layout", "card"]);
    assert_eq!(card["layout"], "card");
    let cards = http(&bind, "GET", "/b/free", &[], "");
    assert!(cards.body.contains("post-grid-card"));

    // 모르는 형태는 거부하고 DB 는 그대로.
    let bad = run(&data, &["board", "update", "free", "--layout", "masonry"]);
    assert_eq!(bad["ok"], false, "{bad}");
    assert_eq!(bad["error"]["code"], "VALIDATION");
    assert!(
        bad["error"]["message"]
            .as_str()
            .unwrap()
            .contains("gallery")
    );
    assert_eq!(board_layout(&data, "free"), "card");
    // 빈 값은 종류 기본값으로.
    giwa(&data, &["board", "update", "free", "--layout", ""]);
    let shown = run(&data, &["board", "show", "free"]);
    assert_eq!(shown["data"]["layout"], "");

    // 글 화면: 맨 위·목록 떠 있는 단추.
    let post = http(&bind, "GET", "/b/free/two", &[], "");
    assert_eq!(post.status, 200);
    assert!(post.body.contains("class=\"float-nav\""));
    assert!(post.body.contains("<a href=\"#main\">맨 위</a>"));
    assert!(
        post.body
            .contains("<a class=\"float-wide\" href=\"/b/free\">목록</a>")
    );
}
