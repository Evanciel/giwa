//! `maru update`, `maru export static`, `maru theme` (ADR-0131·0132·0133).
#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::net::TcpListener;
use std::path::Path;
use std::process::Command;
use std::sync::{Arc, Mutex};

use serde_json::Value;
use sha2::{Digest, Sha256};

const BIN: &str = env!("CARGO_BIN_EXE_maru");

fn run_with(bin: &Path, data: &Path, args: &[&str]) -> Value {
    let out = Command::new(bin)
        .arg("--data")
        .arg(data)
        .args(args)
        .arg("--json")
        .env_remove("MARU_BASE_URL")
        .output()
        .expect("maru 실행");
    let stdout = String::from_utf8(out.stdout).expect("UTF-8 출력");
    serde_json::from_str(stdout.trim()).unwrap_or_else(|e| panic!("JSON 아님: {e}{stdout}"))
}

fn run(data: &Path, args: &[&str]) -> Value {
    run_with(Path::new(BIN), data, args)
}

fn maru(data: &Path, args: &[&str]) -> Value {
    let v = run(data, args);
    assert_eq!(v["ok"], true, "{args:?} → {v}");
    v["data"].clone()
}

type Files = Arc<Mutex<HashMap<String, Vec<u8>>>>;

/// 릴리스 자리: 경로마다 정해 둔 바이트를 돌려준다.
fn release_server() -> (String, Files) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let files: Files = Arc::default();
    let shared = files.clone();
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let mut stream = stream.unwrap();
            let mut line = String::new();
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            reader.read_line(&mut line).unwrap();
            loop {
                let mut h = String::new();
                reader.read_line(&mut h).unwrap();
                if h.trim().is_empty() {
                    break;
                }
            }
            let path = line.split_whitespace().nth(1).unwrap_or("/").to_owned();
            let body = shared.lock().unwrap().get(&path).cloned();
            let (status, body) = match body {
                Some(b) => ("200 OK", b),
                None => ("404 Not Found", Vec::new()),
            };
            let head = format!(
                "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            );
            let _ = stream.write_all(head.as_bytes());
            let _ = stream.write_all(&body);
        }
    });
    (base, files)
}

fn asset_name() -> String {
    let target = match (std::env::consts::OS, std::env::consts::ARCH) {
        ("linux", "x86_64") => "x86_64-unknown-linux-musl",
        ("linux", "aarch64") => "aarch64-unknown-linux-musl",
        ("macos", "x86_64") => "x86_64-apple-darwin",
        ("macos", "aarch64") => "aarch64-apple-darwin",
        ("windows", "x86_64") => "x86_64-pc-windows-msvc",
        ("windows", "aarch64") => "aarch64-pc-windows-msvc",
        other => panic!("{other:?}"),
    };
    let ext = if cfg!(windows) { ".exe" } else { "" };
    format!("maru-{target}{ext}")
}

fn sums(name: &str, bytes: &[u8]) -> Vec<u8> {
    format!("{}  {name}\n", hex::encode(Sha256::digest(bytes))).into_bytes()
}

#[test]
fn 업데이트는_체크섬과_실행을_확인한_뒤에만_바꾼다() {
    let tmp = tempfile::tempdir().unwrap();
    let data = tmp.path().join("data");
    let exe = tmp
        .path()
        .join(if cfg!(windows) { "maru.exe" } else { "maru" });
    std::fs::copy(BIN, &exe).unwrap();
    let original = std::fs::read(&exe).unwrap();
    let old = exe.with_file_name(format!(
        "{}.old",
        exe.file_name().unwrap().to_str().unwrap()
    ));

    // 공개 주소가 없으면 다음 명령을 말한다.
    let v = run_with(&exe, &data, &["update"]);
    assert_eq!(v["ok"], false, "{v}");
    assert!(v.to_string().contains("--base-url"), "{v}");

    let (base, files) = release_server();
    let name = asset_name();
    let path = format!("/{name}");
    let args = ["update", "--base-url", base.as_str(), "--yes"];

    // 같거나 낮은 판: 받지 않는다.
    files
        .lock()
        .unwrap()
        .insert("/VERSION".into(), b"0.0.1\n".to_vec());
    let v = run_with(&exe, &data, &args);
    assert_eq!(v["data"]["update_available"], false, "{v}");
    assert_eq!(std::fs::read(&exe).unwrap(), original);

    // 높은 판이어도 --yes 없이는 바꾸지 않는다.
    files
        .lock()
        .unwrap()
        .insert("/VERSION".into(), b"99.0.0\n".to_vec());
    let v = run_with(&exe, &data, &["update", "--base-url", base.as_str()]);
    assert_eq!(v["data"]["update_available"], true, "{v}");
    assert_eq!(v["data"]["updated"], false, "{v}");
    assert_eq!(std::fs::read(&exe).unwrap(), original);

    // 체크섬이 틀리면 바꾸지 않는다.
    let mut newer = original.clone();
    newer.extend_from_slice(&[0u8; 16]);
    files.lock().unwrap().insert(path.clone(), newer.clone());
    files
        .lock()
        .unwrap()
        .insert("/SHA256SUMS".into(), sums(&name, b"other"));
    let v = run_with(&exe, &data, &args);
    assert_eq!(v["ok"], false, "{v}");
    assert_eq!(std::fs::read(&exe).unwrap(), original);
    assert!(!old.exists());

    // 체크섬이 맞아도 실행되지 않으면 바꾸지 않는다.
    files
        .lock()
        .unwrap()
        .insert(path.clone(), b"not a program".to_vec());
    files
        .lock()
        .unwrap()
        .insert("/SHA256SUMS".into(), sums(&name, b"not a program"));
    let v = run_with(&exe, &data, &args);
    assert_eq!(v["ok"], false, "{v}");
    assert_eq!(std::fs::read(&exe).unwrap(), original);
    assert!(!old.exists());
    let left: Vec<_> = std::fs::read_dir(tmp.path())
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .filter(|n| n.contains(".new-"))
        .collect();
    assert!(left.is_empty(), "{left:?}");

    // 맞으면 바꾸고 옛 파일을 남긴다. 그 사이에 서명을 확인하지 않았다고 말한다.
    files.lock().unwrap().insert(path.clone(), newer.clone());
    files
        .lock()
        .unwrap()
        .insert("/SHA256SUMS".into(), sums(&name, &newer));
    let v = run_with(&exe, &data, &args);
    assert_eq!(v["data"]["updated"], true, "{v}");
    assert_eq!(v["data"]["signature_checked"], false, "{v}");
    assert_eq!(std::fs::read(&exe).unwrap(), newer);
    assert_eq!(std::fs::read(&old).unwrap(), original);
    let out = Command::new(&exe).arg("--version").output().unwrap();
    assert!(out.status.success());
}

fn exported(out: &Path) -> Vec<String> {
    fn walk(dir: &Path, base: &Path, into: &mut Vec<String>) {
        for e in std::fs::read_dir(dir).unwrap() {
            let p = e.unwrap().path();
            if p.is_dir() {
                walk(&p, base, into);
            } else {
                into.push(
                    p.strip_prefix(base)
                        .unwrap()
                        .to_string_lossy()
                        .replace('\\', "/"),
                );
            }
        }
    }
    let mut all = Vec::new();
    walk(out, out, &mut all);
    all
}

#[test]
fn 정적_내보내기는_손님이_보는_것만_내린다() {
    let tmp = tempfile::tempdir().unwrap();
    let data = tmp.path().join("사이트");
    let out = tmp.path().join("밖");
    maru(&data, &["init", "--preset", "blog", "--yes"]);
    let board = maru(&data, &["board", "list"])[0]["slug"]
        .as_str()
        .unwrap()
        .to_owned();
    let published = maru(
        &data,
        &[
            "post",
            "new",
            "--board",
            &board,
            "--title",
            "공개 글",
            "--body",
            "본문",
            "--publish",
        ],
    );
    let slug = published["slug"].as_str().unwrap().to_owned();
    let id = published["id"].as_str().unwrap().to_owned();
    maru(
        &data,
        &[
            "post",
            "new",
            "--board",
            &board,
            "--title",
            "초안 글",
            "--body",
            "숨은 본문",
        ],
    );
    for n in 0..25 {
        maru(
            &data,
            &[
                "post",
                "new",
                "--board",
                &board,
                "--title",
                &format!("쪽 나누기 {n}"),
                "--body",
                "본문",
                "--publish",
            ],
        );
    }
    maru(&data, &["board", "create", "notes", "--name", "회원 노트"]);
    maru(&data, &["board", "update", "notes", "--read", "member"]);
    maru(
        &data,
        &[
            "post",
            "new",
            "--board",
            "notes",
            "--title",
            "회원 글",
            "--body",
            "회원 본문",
            "--publish",
        ],
    );
    maru(
        &data,
        &["redirect", "add", "/old/x", &format!("/b/{board}")],
    );
    let before = maru(&data, &["post", "show", &id]).to_string();

    let report = maru(&data, &["export", "static", "--out", out.to_str().unwrap()]);
    assert!(report["pages"].as_u64().unwrap() > 3, "{report}");
    let all = exported(&out);
    for need in [
        "index.html".to_owned(),
        "404.html".to_owned(),
        "_redirects".to_owned(),
        "sitemap.xml".to_owned(),
        "robots.txt".to_owned(),
        "assets/site.css".to_owned(),
        "assets/tokens.css".to_owned(),
        format!("b/{board}/index.html"),
        format!("b/{board}/page-2/index.html"),
        format!("b/{board}/{slug}/index.html"),
    ] {
        assert!(all.contains(&need), "{need} 없음: {all:?}");
    }
    assert!(
        !all.iter()
            .any(|f| f.starts_with("admin") || f.starts_with("api/")),
        "{all:?}"
    );
    assert!(
        !all.iter().any(|f| f.starts_with("b/notes/")),
        "회원 게시판: {all:?}"
    );
    let text: String = all
        .iter()
        .filter(|f| f.ends_with(".html"))
        .map(|f| std::fs::read_to_string(out.join(f)).unwrap())
        .collect();
    assert!(!text.contains("숨은 본문"));
    assert!(!text.contains("회원 본문"));
    assert!(!text.contains("?page=2"), "쪽 링크가 파일 경로로 바뀐다");
    let redirects = std::fs::read_to_string(out.join("_redirects")).unwrap();
    assert!(
        redirects.contains(&format!("/old/x /b/{board} 301")),
        "{redirects}"
    );

    // 조회수에 들어가지 않는다.
    assert_eq!(maru(&data, &["post", "show", &id]).to_string(), before);

    // 비어 있지 않은 폴더는 묻고 덮어쓴다.
    let v = run(&data, &["export", "static", "--out", out.to_str().unwrap()]);
    assert_eq!(v["ok"], false, "{v}");
    assert!(v.to_string().contains("--yes"), "{v}");
    maru(
        &data,
        &["export", "static", "--out", out.to_str().unwrap(), "--yes"],
    );
}

#[test]
fn 테마는_색과_모양만_입힌다() {
    let tmp = tempfile::tempdir().unwrap();
    let data = tmp.path().join("사이트");
    maru(&data, &["init", "--preset", "blog", "--yes"]);
    let boards = maru(&data, &["board", "list"]).to_string();

    let list = maru(&data, &["theme", "list"]);
    let ids: Vec<&str> = list["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["id"].as_str().unwrap())
        .collect();
    assert!(ids.contains(&"cafe"), "{ids:?}");

    let before = maru(&data, &["theme", "tokens", "get"]);
    assert_eq!(before, maru(&data, &["design", "show"]));
    maru(&data, &["theme", "apply", "cafe"]);
    let after = maru(&data, &["theme", "tokens", "get"]);
    assert_ne!(before, after);
    assert_eq!(
        maru(&data, &["board", "list"]).to_string(),
        boards,
        "구조는 그대로"
    );

    let v = run(&data, &["theme", "apply", "없는테마"]);
    assert_eq!(v["ok"], false, "{v}");
}
