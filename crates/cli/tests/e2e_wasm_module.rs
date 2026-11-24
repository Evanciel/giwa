//! 외부 모듈(WASM) 1단계 (Phase 5, ADR-0177). 실제 바이너리·실제 포트·실제 DB 파일로
//! 손으로 만든 시험 컴포넌트 「메아리」(crates/wasm/tests/guest)가 이벤트·툴·화면·자리·위젯을 받고,
//! 시간·메모리 한도와 사이트 밖 주소·남의 템플릿을 막고, 제거하면 자기 키-값 행만 지워지는지 본다.
#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::thread::sleep;
use std::time::{Duration, Instant};

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

/// (상태, 머리글, 본문)
fn http(
    bind: &str,
    method: &str,
    path: &str,
    headers: &[(&str, &str)],
    body: &str,
) -> (u16, String, String) {
    let mut s = TcpStream::connect(bind).unwrap();
    let mut req = format!("{method} {path} HTTP/1.1\r\nHost: {bind}\r\nConnection: close\r\n");
    for (k, v) in headers {
        req.push_str(&format!("{k}: {v}\r\n"));
    }
    req.push_str(&format!("Content-Length: {}\r\n\r\n{body}", body.len()));
    s.write_all(req.as_bytes()).unwrap();
    let mut raw = Vec::new();
    s.read_to_end(&mut raw).unwrap();
    let text = String::from_utf8_lossy(&raw).into_owned();
    let (head, body) = text.split_once("\r\n\r\n").unwrap_or((&text, ""));
    (
        head[9..12].parse().unwrap(),
        head.to_owned(),
        body.to_owned(),
    )
}

fn get(bind: &str, path: &str) -> (u16, String) {
    let (status, _, body) = http(bind, "GET", path, &[], "");
    (status, body)
}

fn enc(s: &str) -> String {
    let mut out = String::new();
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || b"-_.~".contains(&b) {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

fn wait_until(what: &str, mut check: impl FnMut() -> bool) {
    let start = Instant::now();
    while start.elapsed() < Duration::from_secs(20) {
        if check() {
            return;
        }
        sleep(Duration::from_millis(200));
    }
    panic!("기다렸지만 안 됐다: {what}");
}

fn copy_dir(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    for entry in std::fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        let target = to.join(entry.file_name());
        if entry.path().is_dir() {
            copy_dir(&entry.path(), &target);
        } else {
            std::fs::copy(entry.path(), target).unwrap();
        }
    }
}

fn kv_rows(data: &Path, module: &str) -> i64 {
    let db = data.join("giwa.db");
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(async {
        let pool = sqlx::SqlitePool::connect(&format!("sqlite:{}", db.display()))
            .await
            .unwrap();
        let n: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM ext_module_kv WHERE module = ?")
            .bind(module)
            .fetch_one(&pool)
            .await
            .unwrap();
        pool.close().await;
        n
    })
}

#[test]
fn external_wasm_module_gets_events_tools_and_screens() {
    let tmp = tempfile::tempdir().unwrap();
    let data = tmp.path().join("사이트");
    giwa(
        &data,
        &["init", "--no-samples", "--preset", "blog", "--yes"],
    );
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join("../wasm/tests/fixtures/echo");
    copy_dir(&fixture, &data.join("modules").join("echo"));
    // 잘못된 모듈 폴더는 건너뛰고 사이트는 그대로 뜬다.
    let broken = data.join("modules").join("broken");
    std::fs::create_dir_all(&broken).unwrap();
    std::fs::write(broken.join("module.toml"), "[module]\nid = \"broken\"\n").unwrap();

    // 내장 모듈과 같은 목록에 보이고, 켜기 전에는 툴이 안내와 함께 거절된다.
    let list = giwa(&data, &["module", "list"]);
    let echo = list
        .as_array()
        .unwrap()
        .iter()
        .find(|m| m["id"] == "echo")
        .expect("외부 모듈이 목록에 있다");
    assert_eq!(echo["installed"], false);
    assert_eq!(echo["name"], "메아리");
    assert_eq!(echo["widgets"][0], "echo_box");
    assert!(
        !list.as_array().unwrap().iter().any(|m| m["id"] == "broken"),
        "{list}"
    );
    let refused = run(&data, &["echo", "count"]);
    assert_eq!(refused["error"]["code"], "CONFLICT", "{refused}");

    giwa(&data, &["module", "enable", "echo"]);
    let (_server, bind) = start_server(&data);

    // 화면: 테마로 그린 페이지. 손님이 손님인 줄 안다.
    let (status, page) = get(&bind, "/m/echo/hello?q=1");
    assert_eq!(status, 200, "{page}");
    assert!(page.contains("받은 이벤트 0개"), "{page}");
    assert!(page.contains("보는 사람: 손님"), "{page}");
    assert!(
        page.contains("<title>메아리 · "),
        "제목에 사이트 이름: {page}"
    );
    assert_eq!(get(&bind, "/m/echo").0, 200, "모듈 첫 주소");
    assert_eq!(get(&bind, "/m/echo/none").0, 404);

    // 이벤트: 글을 쓰면 서버가 배달하고, 모듈은 자기 키-값에 센다.
    let post = giwa(
        &data,
        &[
            "post",
            "new",
            "--board",
            "blog",
            "--title",
            "메아리 글",
            "--body",
            "본문",
            "--publish",
        ],
    );
    let mut counted = Value::Null;
    wait_until("메아리가 이벤트를 받는다", || {
        counted = giwa(&data, &["echo", "count"]);
        counted["events"].as_u64().unwrap_or(0) >= 1
    });
    assert!(
        counted["last"]
            .as_str()
            .is_some_and(|a| a.starts_with("post.")),
        "{counted}"
    );
    assert_eq!(counted["settings"]["greeting"], "안녕하세요", "설정 기본값");
    assert_eq!(counted["actor"]["type"], "local", "{counted}");
    let events = counted["events"].as_u64().unwrap();

    // JSON·글자 응답, 로그인 머리글은 모듈에 넘기지 않는다.
    let (status, json) = get(&bind, "/m/echo/json");
    assert_eq!(status, 200);
    assert_eq!(
        serde_json::from_str::<Value>(&json).unwrap()["events"],
        events
    );
    let (_, text) = get(&bind, "/m/echo/text?안녕");
    assert!(text.starts_with("메아리 "), "{text}");
    let (_, _, seen) = http(
        &bind,
        "GET",
        "/m/echo/headers",
        &[("Cookie", "giwa_session=x"), ("Authorization", "Bearer x")],
        "",
    );
    assert!(seen.contains("host"), "{seen}");
    assert!(
        !seen.contains("cookie") && !seen.contains("authorization"),
        "{seen}"
    );

    // 쓰기 폼: 같은 사이트에서 온 것만 모듈이 받는다.
    let (status, head, _) = http(
        &bind,
        "POST",
        "/m/echo/note",
        &[("Sec-Fetch-Site", "same-origin")],
        "메모=하나",
    );
    assert_eq!(status, 303, "{head}");
    assert!(head.contains("location: /m/echo/hello"), "{head}");
    let (_, _, body) = http(
        &bind,
        "POST",
        "/m/echo/note",
        &[("Origin", "http://evil.example")],
        "메모=둘",
    );
    assert_eq!(body, "다른 사이트에서 온 요청");

    // 사이트 밖 주소·남의 템플릿은 막는다.
    let (status, head, _) = http(&bind, "GET", "/m/echo/away", &[], "");
    assert!(status >= 400 && !head.contains("example.com"), "{head}");
    assert!(get(&bind, "/m/echo/raw").0 >= 400);

    // 끝나지 않는 화면·메모리 폭주는 끊기고, 사이트와 모듈은 계속 일한다.
    let started = Instant::now();
    assert!(get(&bind, "/m/echo/spin").0 >= 500);
    assert!(
        started.elapsed() < Duration::from_secs(4),
        "화면 한도 1초: {:?}",
        started.elapsed()
    );
    assert!(get(&bind, "/m/echo/eat").0 >= 500);
    assert_eq!(get(&bind, "/m/echo/json").0, 200, "다음 호출은 정상");
    assert_eq!(get(&bind, "/").0, 200);

    // 글 아래 자리.
    let slug = post["slug"].as_str().unwrap();
    let (status, page) = get(&bind, &format!("/b/blog/{}", enc(slug)));
    assert_eq!(status, 200, "{page}");
    assert!(page.contains("메아리가 받은 이벤트"), "{page}");

    // 위젯.
    giwa(
        &data,
        &["widget", "place", "echo_box", "--region", "sidebar"],
    );
    let (_, home) = get(&bind, "/");
    assert!(home.contains("class=\"echo-widget\">메아리 "), "{home}");

    // 툴: 키-값, 모듈이 낸 실패, 시간 한도.
    let saved = giwa(
        &data,
        &["echo", "remember", "--key", "a", "--value", "하나"],
    );
    assert_eq!(saved["saved"], "a");
    let notes = giwa(&data, &["echo", "notes"]);
    assert!(
        notes["notes"]
            .as_array()
            .unwrap()
            .iter()
            .any(|n| n["key"] == "a" && n["value"] == "하나"),
        "{notes}"
    );
    let failed = run(&data, &["echo", "fail"]);
    assert_eq!(failed["error"]["code"], "CONFLICT", "{failed}");
    assert!(
        failed["error"]["message"]
            .as_str()
            .unwrap()
            .contains("일부러 낸 실패"),
        "{failed}"
    );
    let started = Instant::now();
    let spun = run(&data, &["echo", "spin"]);
    assert_eq!(spun["ok"], false, "{spun}");
    assert!(
        started.elapsed() < Duration::from_secs(9),
        "툴 한도 5초: {:?}",
        started.elapsed()
    );

    // 끄면 주소가 없어지고, 제거하면 이 모듈의 키-값 행만 지워진다.
    assert!(kv_rows(&data, "echo") > 0);
    giwa(&data, &["module", "disable", "echo"]);
    assert_eq!(get(&bind, "/m/echo/hello").0, 404);
    giwa(&data, &["module", "remove", "echo", "--purge", "--yes"]);
    assert_eq!(kv_rows(&data, "echo"), 0);
    // 컴파일 결과는 모듈 폴더에 남아 다음 시작에 쓴다.
    let cache = data.join("modules").join("echo").join(".cache");
    assert!(
        std::fs::read_dir(&cache)
            .unwrap()
            .flatten()
            .any(|e| e.path().extension().is_some_and(|x| x == "cwasm")),
        "캐시가 있다"
    );
}
