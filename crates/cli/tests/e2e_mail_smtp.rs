//! GAP S5: 편지를 SMTP 로 보내기 (ADR-0083). 실제 바이너리·실제 DB 파일과 이 테스트가 띄운 작은 SMTP 서버로
//! `giwa mail setup`·`giwa mail test`·`serve` 시작 정보가 맞는지, 비밀번호가 설정 파일에 들어가지 않는지 본다.
#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::io::{BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};
use std::thread;

use serde_json::Value;

const BIN: &str = env!("CARGO_BIN_EXE_giwa");

fn run(data: &Path, args: &[&str]) -> Value {
    run_with_stdin(data, args, "")
}

fn run_with_stdin(data: &Path, args: &[&str], input: &str) -> Value {
    let mut child = Command::new(BIN)
        .arg("--data")
        .arg(data)
        .args(args)
        .arg("--json")
        .env_remove("GIWA_SMTP_PASSWORD")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("giwa 실행");
    child
        .stdin
        .take()
        .unwrap()
        .write_all(input.as_bytes())
        .unwrap();
    let out = child.wait_with_output().unwrap();
    let stdout = String::from_utf8(out.stdout).expect("UTF-8 출력");
    serde_json::from_str(stdout.trim()).unwrap_or_else(|e| panic!("JSON 아님: {e}\n{stdout}"))
}

fn giwa(data: &Path, args: &[&str]) -> Value {
    let v = run(data, args);
    assert_eq!(v["ok"], true, "{args:?} → {v}");
    v["data"].clone()
}

/// 한 연결만 받는 아주 작은 SMTP 서버. 주고받은 줄을 모두 모은다.
fn fake_smtp() -> (u16, Arc<Mutex<String>>, thread::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let log = Arc::new(Mutex::new(String::new()));
    let seen = log.clone();
    let handle = thread::spawn(move || {
        let (stream, _) = listener.accept().unwrap();
        serve_smtp(stream, &seen);
    });
    (port, log, handle)
}

fn serve_smtp(stream: TcpStream, log: &Mutex<String>) {
    let mut out = stream.try_clone().unwrap();
    let mut reader = BufReader::new(stream);
    out.write_all(b"220 fake.example.test ESMTP\r\n").unwrap();
    let mut in_data = false;
    let mut line = String::new();
    loop {
        line.clear();
        if reader.read_line(&mut line).unwrap_or(0) == 0 {
            return;
        }
        log.lock().unwrap().push_str(&line);
        if in_data {
            if line == ".\r\n" {
                in_data = false;
                out.write_all(b"250 queued\r\n").unwrap();
            }
            continue;
        }
        let cmd = line.to_ascii_uppercase();
        let reply: &[u8] = if cmd.starts_with("EHLO") || cmd.starts_with("HELO") {
            b"250-fake.example.test\r\n250-8BITMIME\r\n250 AUTH PLAIN LOGIN\r\n"
        } else if cmd.starts_with("AUTH") {
            b"235 accepted\r\n"
        } else if cmd.starts_with("DATA") {
            in_data = true;
            b"354 go ahead\r\n"
        } else if cmd.starts_with("QUIT") {
            out.write_all(b"221 bye\r\n").unwrap();
            return;
        } else {
            b"250 ok\r\n"
        };
        out.write_all(reply).unwrap();
    }
}

#[test]
fn smtp_setup_and_test_mail() {
    let tmp = tempfile::tempdir().unwrap();
    let data = tmp.path();
    giwa(data, &["init", "--no-samples", "--preset", "blog", "--yes"]);

    // 암호화 없는 SMTP 는 이 컴퓨터 밖으로 못 보낸다
    let v = run(
        data,
        &[
            "mail",
            "setup",
            "--transport",
            "smtp",
            "--host",
            "smtp.example.test",
            "--security",
            "none",
        ],
    );
    assert_eq!(v["ok"], false, "{v}");
    assert_eq!(v["error"]["code"], "VALIDATION", "{v}");
    let v = run(data, &["mail", "setup", "--transport", "pigeon"]);
    assert_eq!(v["error"]["code"], "USAGE", "{v}");

    // 비밀번호는 표준 입력으로만, 설정 파일엔 들어가지 않는다
    let (port, log, server) = fake_smtp();
    let port_s = port.to_string();
    let setup = [
        "mail",
        "setup",
        "--transport",
        "smtp",
        "--host",
        "127.0.0.1",
        "--port",
        &port_s,
        "--security",
        "none",
        "--user",
        "mailer",
        "--password-stdin",
        "--from",
        "noreply@example.test",
    ];
    let v = run_with_stdin(data, &setup, "test-smtp-pass-2026\n");
    assert_eq!(v["ok"], true, "{v}");
    let d = &v["data"];
    assert_eq!(d["transport"], "smtp");
    assert_eq!(d["smtp"]["port"], port);
    assert_eq!(d["smtp"]["security"], "none");
    assert_eq!(d["password"], "file");
    assert_eq!(d["restart"], true);
    let config = std::fs::read_to_string(data.join("config.toml")).unwrap();
    assert!(
        config.contains("[mail.smtp]") && config.contains("username = \"mailer\""),
        "{config}"
    );
    assert!(!config.contains("test-smtp-pass-2026"), "{config}");
    assert_eq!(
        std::fs::read_to_string(data.join("secrets").join("smtp-password")).unwrap(),
        "test-smtp-pass-2026"
    );

    // 비밀번호 없이 다시 정해도 이미 둔 비밀번호는 그대로다(여러 번 해도 같은 결과)
    let again: Vec<&str> = setup
        .iter()
        .copied()
        .filter(|a| *a != "--password-stdin")
        .collect();
    let d = giwa(data, &again);
    assert_eq!(d["password"], "file");
    assert_eq!(
        std::fs::read_to_string(data.join("config.toml")).unwrap(),
        config
    );

    // 로그인 이름·비밀번호(secrets 에 둔 값)로 로그인하고 보낸다
    let d = giwa(data, &["mail", "test", "--to", "sun@example.test"]);
    assert_eq!(d["status"], "sent", "{d}");
    assert_eq!(d["transport"], "smtp");
    server.join().unwrap();
    let talk = log.lock().unwrap().clone();
    assert!(talk.contains("EHLO example.test"), "{talk}");
    // AUTH PLAIN = base64("\0mailer\0test-smtp-pass-2026")
    assert!(
        talk.contains("AUTH PLAIN AG1haWxlcgB0ZXN0LXNtdHAtcGFzcy0yMDI2"),
        "{talk}"
    );
    assert!(talk.contains("MAIL FROM:<noreply@example.test>"), "{talk}");
    assert!(talk.contains("RCPT TO:<sun@example.test>"), "{talk}");
    assert!(talk.contains("Message-ID: <"), "{talk}");
    assert!(talk.contains("@example.test>"), "{talk}");
    assert!(
        talk.contains("Content-Type: text/plain; charset=utf-8"),
        "{talk}"
    );
    assert!(!talk.contains("test-smtp-pass-2026"), "{talk}");

    let list = giwa(data, &["mail", "list"]);
    assert_eq!(list[0]["status"], "sent", "{list}");
    assert_eq!(list[0]["to_addr"], "sun@example.test");
    let audit = giwa(data, &["audit", "list", "--action", "mail.setup"]);
    assert!(
        !audit.to_string().contains("test-smtp-pass-2026"),
        "{audit}"
    );

    // 서버가 없으면 실패로 끝나고, 편지는 다시 시도할 상태로 남는다
    let closed = TcpListener::bind("127.0.0.1:0").unwrap();
    let dead = closed.local_addr().unwrap().port().to_string();
    drop(closed);
    giwa(
        data,
        &[
            "mail",
            "setup",
            "--transport",
            "smtp",
            "--host",
            "127.0.0.1",
            "--port",
            &dead,
            "--security",
            "none",
        ],
    );
    let v = run(data, &["mail", "test", "--to", "moon@example.test"]);
    assert_eq!(v["ok"], false, "{v}");
    assert_eq!(v["error"]["code"], "EXTERNAL", "{v}");
    assert!(
        v["error"]["message"].as_str().unwrap().contains("SMTP"),
        "{v}"
    );
    let queued = giwa(data, &["mail", "list", "--status", "queued"]);
    assert_eq!(queued[0]["to_addr"], "moon@example.test", "{queued}");

    // serve 시작 정보에 지금 방식이 보인다(비밀번호 값은 없다)
    let mut child = Command::new(BIN)
        .arg("--data")
        .arg(data)
        .args(["serve", "--bind", "127.0.0.1:0", "--json"])
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let mut line = String::new();
    BufReader::new(child.stdout.take().unwrap())
        .read_line(&mut line)
        .unwrap();
    let _ = child.kill();
    let _ = child.wait();
    let started: Value = serde_json::from_str(line.trim()).unwrap();
    let mail = &started["data"]["mail"];
    assert_eq!(mail["transport"], "smtp", "{started}");
    assert_eq!(mail["password"], "not_needed", "{started}");

    // 파일 방식으로 되돌리면 [mail] 이 빠진다
    let d = giwa(data, &["mail", "setup", "--transport", "file"]);
    assert_eq!(d["transport"], "file");
    let config = std::fs::read_to_string(data.join("config.toml")).unwrap();
    assert!(
        !config.contains("\n[mail") && !config.contains("transport"),
        "{config}"
    );
}
