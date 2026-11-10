//! U14 운영 명령: doctor, token list/revoke, init 재실행, --quiet, 전 명령 --json --yes.
#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::Path;
use std::process::{Child, Command, Output, Stdio};

use serde_json::Value;

const BIN: &str = env!("CARGO_BIN_EXE_maru");

struct Server(Child);

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn run(data: &Path, args: &[&str]) -> Output {
    Command::new(BIN)
        .arg("--data")
        .arg(data)
        .args(args)
        .output()
        .expect("maru 실행")
}

/// `--json` 출력 한 줄을 읽는다. 성공 여부는 따지지 않는다.
fn json(out: &Output) -> Value {
    let stdout = String::from_utf8_lossy(&out.stdout);
    serde_json::from_str(stdout.trim()).unwrap_or_else(|e| {
        panic!(
            "JSON 아님: {e}\nstdout: {stdout}\nstderr: {}",
            String::from_utf8_lossy(&out.stderr)
        )
    })
}

fn ok(data: &Path, args: &[&str]) -> Value {
    let mut a = args.to_vec();
    a.push("--json");
    let out = run(data, &a);
    let v = json(&out);
    assert_eq!(v["ok"], true, "{args:?} → {v}");
    v["data"].clone()
}

fn checks(v: &Value) -> Vec<(String, String)> {
    v["checks"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| {
            (
                c["id"].as_str().unwrap().to_owned(),
                c["status"].as_str().unwrap().to_owned(),
            )
        })
        .collect()
}

fn status_of(v: &Value, id: &str) -> String {
    checks(v)
        .into_iter()
        .find(|(i, _)| i == id)
        .map(|(_, s)| s)
        .unwrap_or_else(|| panic!("{id} 항목 없음: {v}"))
}

fn free_port() -> u16 {
    TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

#[test]
fn doctor_reports_unknown_when_not_initialized() {
    let tmp = tempfile::tempdir().unwrap();
    let data = tmp.path().join("없는 사이트");
    let out = run(&data, &["doctor", "--json"]);
    assert_eq!(out.status.code(), Some(1));
    let v = json(&out);
    assert_eq!(v["ok"], true);
    let d = &v["data"];
    assert_eq!(d["healthy"], false);
    assert_eq!(status_of(d, "data_dir"), "fail");
    assert_eq!(status_of(d, "database"), "unknown");
    assert!(d["summary"]["unknown"].as_u64().unwrap() >= 4);
    let next = d["checks"][1]["next"].as_str().unwrap();
    assert!(next.starts_with("maru init"), "{next}");

    // 사람용 출력도 「못 쟀다」를 드러낸다.
    let human = run(&data, &["doctor"]);
    let text = String::from_utf8_lossy(&human.stdout);
    assert!(text.contains("[못 쟀다]"), "{text}");
    assert!(text.contains("[실패]"), "{text}");
}

#[test]
fn doctor_on_fresh_site_and_port_states() {
    let tmp = tempfile::tempdir().unwrap();
    let data = tmp.path().join("site");
    let port = free_port();
    let bind = format!("127.0.0.1:{port}");
    ok(&data, &["init", "--no-samples", "--bind", &bind, "--yes"]);

    let out = run(&data, &["doctor", "--json"]);
    assert_eq!(out.status.code(), Some(0));
    let d = json(&out)["data"].clone();
    assert_eq!(d["healthy"], true, "{d}");
    for (id, want) in [
        ("data_dir", "pass"),
        ("config", "pass"),
        ("database", "pass"),
        ("owner", "warn"),
        ("tokens", "pass"),
        ("port", "pass"),
        ("site_url", "pass"),
        ("theme", "pass"),
        ("font", "warn"),
        ("disk_free", "unknown"),
    ] {
        assert_eq!(status_of(&d, id), want, "{id}: {d}");
    }

    // 다른 프로그램이 포트를 쥐고 있으면 주의 + 다른 포트 안내.
    {
        let _hold = TcpListener::bind(&bind).unwrap();
        let d = ok(&data, &["doctor"]);
        assert_eq!(status_of(&d, "port"), "warn", "{d}");
        let c = d["checks"]
            .as_array()
            .unwrap()
            .iter()
            .find(|c| c["id"] == "port")
            .unwrap()
            .clone();
        assert!(c["next"].as_str().unwrap().contains("--bind"), "{c}");
    }

    // maru 서버가 떠 있으면 통과.
    let mut child = Command::new(BIN)
        .arg("--data")
        .arg(&data)
        .args(["serve", "--json"])
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let stdout = child.stdout.take().unwrap();
    let _server = Server(child);
    let mut line = String::new();
    BufReader::new(stdout).read_line(&mut line).unwrap();
    let d = ok(&data, &["doctor"]);
    assert_eq!(status_of(&d, "port"), "pass", "{d}");
    let msg = d["checks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["id"] == "port")
        .unwrap()["message"]
        .as_str()
        .unwrap()
        .to_owned();
    assert!(msg.contains("응답"), "{msg}");
}

fn http_status(bind: &str, path: &str, token: &str) -> u16 {
    let mut s = TcpStream::connect(bind).unwrap();
    let req = format!(
        "GET {path} HTTP/1.1\r\nHost: {bind}\r\nAuthorization: Bearer {token}\r\nConnection: close\r\n\r\n"
    );
    s.write_all(req.as_bytes()).unwrap();
    let mut raw = String::new();
    s.read_to_string(&mut raw).unwrap();
    raw[9..12].parse().unwrap()
}

#[test]
fn token_list_and_revoke() {
    let tmp = tempfile::tempdir().unwrap();
    let data = tmp.path().join("site");
    let init = ok(&data, &["init", "--no-samples", "--yes"]);
    let init_secret = init["token"]["secret"].as_str().unwrap().to_owned();
    let made = ok(
        &data,
        &[
            "token",
            "create",
            "--name",
            "배포봇",
            "--scope",
            "publish",
            "--yes",
        ],
    );
    let id = made["id"].as_str().unwrap().to_owned();
    let secret = made["secret"].as_str().unwrap().to_owned();

    let list = ok(&data, &["token", "list"]);
    let tokens = list["tokens"].as_array().unwrap();
    assert_eq!(tokens.len(), 2);
    let text = list.to_string();
    assert!(!text.contains(&secret), "목록에 원문이 없어야 한다");
    assert!(!text.contains("token_hash"));

    // 에이전트 토큰은 목록·폐기 불가.
    let out = Command::new(BIN)
        .arg("--data")
        .arg(&data)
        .args(["token", "list", "--json"])
        .env("MARU_TOKEN", &init_secret)
        .output()
        .unwrap();
    assert_eq!(json(&out)["error"]["code"], "FORBIDDEN");

    let (_server, bind) = {
        let mut child = Command::new(BIN)
            .arg("--data")
            .arg(&data)
            .args(["serve", "--bind", "127.0.0.1:0", "--json"])
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let stdout = child.stdout.take().unwrap();
        let server = Server(child);
        let mut line = String::new();
        BufReader::new(stdout).read_line(&mut line).unwrap();
        let v: Value = serde_json::from_str(line.trim()).unwrap();
        (server, v["data"]["bind"].as_str().unwrap().to_owned())
    };
    assert_eq!(http_status(&bind, "/api/v1/boards", &secret), 200);

    let revoked = ok(&data, &["token", "revoke", &id, "--yes"]);
    assert!(revoked["revoked_at"].is_string());
    // 폐기 직후 같은 토큰은 거부.
    assert_eq!(http_status(&bind, "/api/v1/boards", &secret), 401);
    // 다시 폐기해도 성공, 시각은 그대로.
    let again = ok(&data, &["token", "revoke", &id, "--yes"]);
    assert_eq!(again["revoked_at"], revoked["revoked_at"]);
    // 없는 id 는 NOT_FOUND + 다음 명령.
    let out = run(&data, &["token", "revoke", "nope", "--json"]);
    let v = json(&out);
    assert_eq!(v["error"]["code"], "NOT_FOUND");
    assert_eq!(v["error"]["next"][0], "maru token list");
}

#[test]
fn init_rerun_after_midway_failure_still_issues_token_and_owner() {
    let tmp = tempfile::tempdir().unwrap();
    let data = tmp.path().join("site");
    let long = "가".repeat(200);
    let out = run(
        &data,
        &["init", "--no-samples", "--name", &long, "--yes", "--json"],
    );
    assert_ne!(out.status.code(), Some(0));
    assert_eq!(json(&out)["error"]["code"], "VALIDATION");

    let v = ok(
        &data,
        &[
            "init",
            "--no-samples",
            "--name",
            "다시 만든 블로그",
            "--yes",
        ],
    );
    assert!(v["token"]["secret"].is_string(), "{v}");
    assert!(v["login_link"]["url"].is_string(), "{v}");
    // 세 번째 실행은 아무것도 새로 내지 않는다.
    let v = ok(&data, &["init", "--no-samples", "--yes"]);
    assert!(v["token"].is_null(), "{v}");
    assert!(v["login_link"].is_null(), "{v}");
    assert_eq!(
        ok(&data, &["token", "list"])["tokens"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
}

#[test]
fn quiet_prints_only_one_time_secrets() {
    let tmp = tempfile::tempdir().unwrap();
    let data = tmp.path().join("site");
    let out = run(&data, &["init", "--no-samples", "--yes", "--quiet"]);
    assert!(out.status.success());
    let lines: Vec<String> = String::from_utf8_lossy(&out.stdout)
        .lines()
        .map(str::to_owned)
        .collect();
    assert_eq!(lines.len(), 2, "{lines:?}");
    assert!(lines[0].starts_with("maru_"));
    assert!(lines[1].contains("/admin/link/"));

    let out = run(
        &data,
        &["token", "create", "--name", "스크립트", "--yes", "--quiet"],
    );
    let text = String::from_utf8_lossy(&out.stdout);
    assert_eq!(text.lines().count(), 1);
    assert!(text.starts_with("maru_"));

    let out = run(&data, &["post", "list", "--quiet"]);
    assert!(out.status.success());
    assert!(
        out.stdout.is_empty(),
        "{}",
        String::from_utf8_lossy(&out.stdout)
    );
}

#[test]
fn every_command_speaks_json_with_yes() {
    let tmp = tempfile::tempdir().unwrap();
    let data = tmp.path().join("site");
    let commands: &[&[&str]] = &[
        &["init"],
        &["doctor"],
        &["board", "list"],
        &["board", "show", "blog"],
        &["post", "list"],
        &["page", "list"],
        &["page", "show", "about"],
        &["menu", "show"],
        &["site", "show"],
        &["design", "show"],
        &["token", "list"],
        &["login-link"],
        &["post", "show", "없는-글"],
        &["media", "list"],
        &["media", "show", "없는-미디어"],
    ];
    for args in commands {
        let mut a = args.to_vec();
        a.extend(["--json", "--yes"]);
        let out = run(&data, &a);
        let v = json(&out);
        assert!(v["ok"].is_boolean(), "{args:?} → {v}");
        if v["ok"] == false {
            assert!(v["error"]["code"].is_string(), "{args:?} → {v}");
            assert!(v["error"]["message"].is_string(), "{args:?} → {v}");
        }
        assert!(
            out.stderr.is_empty() || v["ok"] == false,
            "{args:?}: stderr 오염: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
}

/// Unix 에서는 SIGTERM 으로 정상 종료하는지 본다. Windows 는 콘솔 신호를 자식에게 보낼 수 없어
/// 수동 확인 대상이다.
#[cfg(unix)]
#[test]
fn serve_stops_on_sigterm() {
    let tmp = tempfile::tempdir().unwrap();
    let data = tmp.path().join("site");
    ok(&data, &["init", "--no-samples", "--yes"]);
    let mut child = Command::new(BIN)
        .arg("--data")
        .arg(&data)
        .args(["serve", "--bind", "127.0.0.1:0"])
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let mut out = BufReader::new(child.stdout.take().unwrap());
    let mut line = String::new();
    out.read_line(&mut line).unwrap();
    let pid = child.id().to_string();
    assert!(
        Command::new("kill")
            .args(["-TERM", &pid])
            .status()
            .unwrap()
            .success()
    );
    let status = child.wait().unwrap();
    assert!(status.success(), "{status:?}");
    let mut rest = String::new();
    out.read_to_string(&mut rest).unwrap();
    assert!(rest.contains("서버를 멈췄습니다"), "{rest}");
}
