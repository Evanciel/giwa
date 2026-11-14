//! GAP C9: 답변글 (ADR-0056). 실제 바이너리·실제 포트·실제 DB 파일로 CLI 답변 → 순서 경로가 DB 에 남고,
//! 받지 않는 게시판은 원인과 다음 명령으로 거부, 정책을 열면 공개 글 화면에 답변 단추, 목록은 스레드대로 들여 쓴다.
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

fn thread_path(data: &Path, id: &str) -> String {
    let id = id.to_owned();
    db(data, async move |db| {
        sqlx::query_scalar("SELECT thread_path FROM posts WHERE id = ?")
            .bind(id)
            .fetch_one(db.read_pool())
            .await
            .unwrap()
    })
}

#[test]
fn replies_thread_in_cli_and_public_screens() {
    let tmp = tempfile::tempdir().unwrap();
    let data = tmp.path().join("사이트");
    giwa(
        &data,
        &["init", "--no-samples", "--preset", "blog", "--yes"],
    );
    giwa(
        &data,
        &["board", "create", "qna", "--name", "문의", "--kind", "qna"],
    );
    giwa(
        &data,
        &[
            "board", "create", "free", "--name", "자유", "--kind", "free",
        ],
    );
    let q = giwa(
        &data,
        &[
            "post",
            "new",
            "--board",
            "qna",
            "--title",
            "배송 문의",
            "--slug",
            "ship",
            "--publish",
        ],
    );
    let q_id = q["id"].as_str().unwrap();
    let a = giwa(
        &data,
        &[
            "post",
            "reply",
            q_id,
            "--title",
            "RE: 배송 문의",
            "--body",
            "내일 보냅니다.",
            "--publish",
        ],
    );
    assert_eq!(a["parent_id"], q_id);
    assert_eq!(a["status"], "published");
    assert_eq!(thread_path(&data, a["id"].as_str().unwrap()), "0001.");

    // 답변을 받지 않는 게시판: 원인과 다음 명령
    let f = giwa(
        &data,
        &[
            "post",
            "new",
            "--board",
            "free",
            "--title",
            "자유 글",
            "--slug",
            "talk",
            "--publish",
        ],
    );
    let f_id = f["id"].as_str().unwrap();
    let v = run(&data, &["post", "reply", f_id, "--title", "RE"]);
    assert_eq!(v["ok"], false);
    assert_eq!(v["error"]["code"], "FORBIDDEN");
    assert_eq!(
        v["error"]["next"][0],
        "giwa board update free --reply editor"
    );
    // 정책을 열면 상태에 보인다. 알 수 없는 역할은 거부.
    let b = giwa(&data, &["board", "update", "free", "--reply", "member"]);
    assert_eq!(b["policy"]["reply"], "member");
    let v = run(&data, &["board", "update", "free", "--reply", "anyone"]);
    assert_eq!(v["error"]["code"], "VALIDATION");

    let (_server, info) = start_server(&data);
    let bind = info["bind"].as_str().unwrap();
    // 목록: 질문 바로 아래 들여 쓴 답변
    let list = http(bind, "GET", "/b/qna", &[], "");
    assert_eq!(list.status, 200);
    let qi = list.body.find("배송 문의").unwrap();
    let mark = list.body.find("reply-mark depth-1").expect("답변 표시");
    assert!(qi < mark, "답변이 질문 뒤에");
    // 답변 화면: 위 글 링크. 문의 게시판은 운영자만 답하므로 공개 답변 단추가 없다.
    let page = http(
        bind,
        "GET",
        &format!("/b/qna/{}", a["slug"].as_str().unwrap()),
        &[],
        "",
    );
    assert_eq!(page.status, 200);
    assert!(page.body.contains("답변한 글: <a href=\"/b/qna/ship\">"));
    assert!(!page.body.contains("/write/qna?reply="));
    // 회원 답변을 연 게시판은 글 화면에 답변 단추. 손님이 누르면 로그인으로.
    let talk = http(bind, "GET", "/b/free/talk", &[], "");
    let link = format!("/write/free?reply={f_id}");
    assert!(talk.body.contains(&link), "답변 단추");
    let form = http(bind, "GET", &link, &[], "");
    assert_eq!(form.status, 303);
}
