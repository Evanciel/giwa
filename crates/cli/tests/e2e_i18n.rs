//! 영어 사이트 (S7, ADR-0134). 실제 바이너리·실제 포트·실제 DB 파일로, `site.lang = en` 이면
//! 손님 화면과 오류가 영어로 나오는지 본다. 사이트 내용(게시판 이름, 글)은 사용자가 쓴 그대로다.
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
        .env_remove("GIWA_TOKEN")
        .env_remove("GIWA_LANG")
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
        .env_remove("GIWA_LANG")
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

fn get(bind: &str, path: &str) -> (u16, String) {
    let mut s = TcpStream::connect(bind).unwrap();
    s.write_all(
        format!("GET {path} HTTP/1.1\r\nHost: {bind}\r\nConnection: close\r\n\r\n").as_bytes(),
    )
    .unwrap();
    let mut out = Vec::new();
    s.read_to_end(&mut out).unwrap();
    let text = String::from_utf8(out).unwrap();
    let (head, body) = text.split_once("\r\n\r\n").unwrap_or((&text, ""));
    (head[9..12].parse().unwrap(), body.to_owned())
}

/// 한글이 든 줄. 주석(`<!-- -->`)은 손님에게 보이지 않으므로 뺀다.
fn hangul_lines(html: &str) -> Vec<String> {
    let mut text = String::new();
    let mut rest = html;
    while let Some(at) = rest.find("<!--") {
        text.push_str(&rest[..at]);
        rest = rest[at..].split_once("-->").map_or("", |(_, r)| r);
    }
    text.push_str(rest);
    text.lines()
        .filter(|l| l.chars().any(|c| ('가'..='힣').contains(&c)))
        .map(|l| l.trim().chars().take(160).collect())
        .collect()
}

#[test]
fn 영어_사이트는_손님_화면과_오류가_영어다() {
    let tmp = tempfile::tempdir().unwrap();
    let data = tmp.path().join("site");
    giwa(
        &data,
        &[
            "init",
            "--no-samples",
            "--preset",
            "blog",
            "--name",
            "Demo",
            "--yes",
        ],
    );
    giwa(
        &data,
        &[
            "site",
            "set",
            "--lang",
            "en",
            "--description",
            "A demo site",
            "--signup",
            "open",
            "--captcha",
            "off",
        ],
    );
    // 프리셋이 만든 한국어 내용을 영어로 바꾼다. 남은 한글은 화면 틀에서 온 것이다.
    giwa(&data, &["board", "update", "blog", "--name", "Blog"]);
    giwa(
        &data,
        &[
            "page",
            "edit",
            "about",
            "--title",
            "About",
            "--body",
            "About us.",
        ],
    );
    giwa(
        &data,
        &["menu", "set", "header", "--item", "Posts=board:blog"],
    );
    let post = giwa(
        &data,
        &[
            "post",
            "new",
            "--board",
            "blog",
            "--title",
            "Hello",
            "--body",
            "First post.",
            "--publish",
        ],
    );
    let slug = post["slug"].as_str().unwrap().to_owned();

    let (_server, bind) = start_server(&data);
    for path in [
        "/".to_owned(),
        "/b/blog".to_owned(),
        format!("/b/blog/{slug}"),
        "/login".to_owned(),
        "/signup".to_owned(),
        "/search?q=hello".to_owned(),
        "/search?q=nothing-matches".to_owned(),
    ] {
        let (status, html) = get(&bind, &path);
        assert_eq!(status, 200, "{path}");
        assert!(html.contains("<html lang=\"en\""), "{path}");
        let left = hangul_lines(&html);
        assert!(left.is_empty(), "{path}:\n{}", left.join("\n"));
    }
    let (status, html) = get(&bind, "/no-such-page");
    assert_eq!(status, 404);
    assert!(
        hangul_lines(&html).is_empty(),
        "{}",
        hangul_lines(&html).join("\n")
    );

    // 오류 문장도 영어다. 키와 자리는 한국어판과 같다.
    let v = run(&data, &["post", "show", "no-such-post"]);
    assert_eq!(v["ok"], false, "{v}");
    let message = v["error"]["message"].as_str().unwrap();
    assert!(!message.chars().any(|c| ('가'..='힣').contains(&c)), "{v}");

    // 한국어로 돌리면 다시 한국어다.
    giwa(&data, &["site", "set", "--lang", "ko"]);
    let (_, html) = get(&bind, "/login");
    assert!(html.contains("<html lang=\"ko\""));
    assert!(!hangul_lines(&html).is_empty());
}
