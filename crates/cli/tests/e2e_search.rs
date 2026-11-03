//! GAP C5: 사이트 검색 (ADR-0050). 실제 바이너리·실제 포트·실제 DB 파일로
//! 한글 2글자 검색·문법·게시판 안 검색·비밀글 차단·이상한 입력, 화면·REST·CLI 가 같은 답을 주는지 본다.
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
    status: u16,
    body: String,
}

fn get(bind: &str, path: &str, headers: &[(&str, &str)]) -> Res {
    send(bind, "GET", path, headers, "")
}

fn send(bind: &str, method: &str, path: &str, headers: &[(&str, &str)], body: &str) -> Res {
    let mut s = TcpStream::connect(bind).unwrap();
    let mut req = format!(
        "{method} {path} HTTP/1.1\r\nHost: {bind}\r\nConnection: close\r\nContent-Length: {}\r\n",
        body.len()
    );
    for (k, v) in headers {
        req.push_str(&format!("{k}: {v}\r\n"));
    }
    req.push_str("\r\n");
    req.push_str(body);
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

fn enc(s: &str) -> String {
    s.bytes()
        .map(|b| {
            if b.is_ascii_alphanumeric() || b"-_.~".contains(&b) {
                (b as char).to_string()
            } else {
                format!("%{b:02X}")
            }
        })
        .collect()
}

/// 검색 결과 화면에 나온 제목들 (결과 목록 `<h2><a ...>제목</a></h2>`).
fn hit_titles(html: &str) -> Vec<String> {
    let Some(list) = html.split("<ol class=\"hit-list\">").nth(1) else {
        return vec![];
    };
    let list = list.split("</ol>").next().unwrap();
    list.split("<h2><a href=\"")
        .skip(1)
        .map(|chunk| {
            let after = &chunk[chunk.find("\">").unwrap() + 2..];
            after[..after.find("</a>").unwrap()].to_owned()
        })
        .collect()
}

fn titles(v: &Value) -> Vec<String> {
    v["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|h| h["title"].as_str().unwrap().to_owned())
        .collect()
}

#[test]
fn search_finds_hangul_and_every_path_agrees() {
    let tmp = tempfile::tempdir().unwrap();
    let data = tmp.path().join("사이트");
    let init = maru(&data, &["init", "--preset", "blog", "--yes"]);
    let token = init["token"]["secret"].as_str().unwrap().to_owned();
    let bearer = format!("Bearer {token}");

    let new = |args: &[&str]| {
        let mut all = vec!["post", "new", "--publish"];
        all.extend_from_slice(args);
        maru(&data, &all)
    };
    new(&[
        "--title",
        "유니콘 여행기",
        "--slug",
        "trip",
        "--body",
        "**유니콘섬에서** 무지개길을 걸었다.",
    ]);
    new(&[
        "--title",
        "가을 메모",
        "--slug",
        "memo",
        "--body",
        "유니콘 인형을 샀다.",
        "--tag",
        "기념품",
    ]);
    new(&[
        "--title",
        "비밀 유니콘",
        "--slug",
        "hush",
        "--body",
        "비밀본문유니콘",
        "--secret",
    ]);
    maru(
        &data,
        &["post", "new", "--title", "초안 유니콘", "--slug", "draft"],
    );

    // CLI: 손님 기준 공개 검색
    let cli = maru(&data, &["search", "유니콘"]);
    let mut cli_titles = titles(&cli);
    cli_titles.sort();
    assert_eq!(cli_titles, ["가을 메모", "유니콘 여행기"]);
    assert_eq!(titles(&cli)[0], "유니콘 여행기", "제목 일치가 먼저");
    assert_eq!(
        titles(&maru(&data, &["search", "무지개"])),
        ["유니콘 여행기"]
    );
    assert_eq!(titles(&maru(&data, &["search", "길"])), ["유니콘 여행기"]);
    assert_eq!(
        titles(&maru(&data, &["search", "기념품", "--in", "tag"])),
        ["가을 메모"]
    );
    assert_eq!(
        titles(&maru(&data, &["search", "유니콘 -인형"])),
        ["유니콘 여행기"]
    );
    assert!(titles(&maru(&data, &["search", "비밀본문유니콘"])).is_empty());
    // 관리 목록 검색은 초안·비밀글까지
    let admin = maru(&data, &["post", "list", "--q", "유니콘"]);
    assert_eq!(admin["total"], 4);
    // 잘못된 옵션·빈 검색어는 다음 명령과 함께 실패
    let bad = run(&data, &["search", "유니콘", "--in", "everywhere"]);
    assert_eq!(bad["ok"], false);
    assert_eq!(bad["error"]["code"], "VALIDATION");
    let usage = run(&data, &["search"]);
    assert_eq!(usage["ok"], false);
    assert!(usage["error"]["next"].to_string().contains("maru search"));
    // 다시 만들기는 멱등
    let a = maru(&data, &["search", "reindex"]);
    let b = maru(&data, &["search", "reindex"]);
    assert_eq!(a, b);

    let (_server, bind) = start_server(&data);
    let q = enc("유니콘");

    // REST 는 CLI 와 같은 답
    let rest = get(&bind, &format!("/api/v1/search?q={q}"), &[]);
    assert_eq!(rest.status, 200, "{}", rest.body);
    let rest: Value = serde_json::from_str(&rest.body).unwrap();
    assert_eq!(rest["data"], cli);
    let bad = get(&bind, &format!("/api/v1/search?q={q}&sort=weird"), &[]);
    assert_eq!(bad.status, 400, "{}", bad.body);
    let empty = get(&bind, "/api/v1/search?q=", &[]);
    assert_eq!(empty.status, 400, "{}", empty.body);
    // REST 관리 목록 검색: 에이전트 토큰은 제가 쓴 글 안에서만 (초안 포함)
    let made = send(
        &bind,
        "POST",
        "/api/v1/posts",
        &[
            ("Authorization", &bearer),
            ("Content-Type", "application/json"),
        ],
        r#"{"board":"blog","title":"에이전트 초안","body_md":"유니콘 초안 메모"}"#,
    );
    assert_eq!(made.status, 201, "{}", made.body);
    let listed = get(
        &bind,
        &format!("/api/v1/posts?q={q}"),
        &[("Authorization", &bearer)],
    );
    let listed: Value = serde_json::from_str(&listed.body).unwrap();
    assert_eq!(titles(&listed["data"]), ["에이전트 초안"], "{listed}");
    // 다시 만들기는 사이트 관리 권한 (손님은 막힘)
    assert_eq!(
        send(&bind, "POST", "/api/v1/search/reindex", &[], "").status,
        401
    );

    // 화면도 같은 답, 찾은 말 강조, noindex
    let page = get(&bind, &format!("/search?q={q}"), &[]);
    assert_eq!(page.status, 200);
    assert_eq!(hit_titles(&page.body), titles(&cli));
    assert!(
        page.body
            .contains("<meta name=\"robots\" content=\"noindex\">")
    );
    assert!(page.body.contains("<mark>유니콘</mark>"));
    assert!(page.body.contains("검색 결과 2건"));
    assert!(
        !page.body.contains("비밀본문"),
        "비밀글 본문은 어디에도 없다"
    );
    assert!(
        !page.body.contains("비밀 유니콘"),
        "비밀글 제목도 공개 검색엔 없다"
    );
    // 머리글 검색 칸
    let home = get(&bind, "/", &[]);
    assert!(home.body.contains("class=\"site-search\""));
    // 게시판 안 검색
    let board = get(&bind, &format!("/b/blog?q={q}&in=title"), &[]);
    assert_eq!(board.status, 200);
    assert_eq!(hit_titles(&board.body), ["유니콘 여행기"]);
    assert!(
        board
            .body
            .contains("<meta name=\"robots\" content=\"noindex\">")
    );
    // 이상한 입력: 500 없이 안내
    for raw in [
        "\"",
        "*",
        "(유니콘",
        "NEAR(a b)",
        "{title} : x",
        "-유니콘",
        &"가".repeat(300),
    ] {
        let res = get(&bind, &format!("/search?q={}", enc(raw)), &[]);
        assert_eq!(res.status, 200, "{raw:?}: {}", res.body);
    }
    let long = get(&bind, &format!("/search?q={}", enc(&"가".repeat(300))), &[]);
    assert!(long.body.contains("class=\"search-error\""));
    // 머리글 검색 칸 숨김 설정 (`/search` 는 그대로 열린다)
    maru(&data, &["site", "set", "--show-search", "false"]);
    let home = get(&bind, "/", &[]);
    assert!(!home.body.contains("class=\"site-search\""));
    assert_eq!(get(&bind, &format!("/search?q={q}"), &[]).status, 200);
}
