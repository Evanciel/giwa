//! GAP S6: 백업·복원 (ADR-0063). 실제 바이너리·실제 포트·실제 DB 파일로
//! 켜진 서버 옆 CLI 백업, 켜진 서버면 복원 거부, 관리 화면 만들기·내려받기·지우기,
//! 서버를 끈 뒤 복원하면 지운 글이 돌아오는지 확인한다.
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

fn maru_err(data: &Path, args: &[&str]) -> Value {
    let v = run(data, args);
    assert_eq!(v["ok"], false, "{args:?} → {v}");
    v["error"].clone()
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
    head: String,
    body: String,
    body_len: usize,
}

impl Res {
    fn cookie(&self, name: &str) -> Option<String> {
        self.head.lines().find_map(|l| {
            let (k, v) = l.split_once(':')?;
            if !k.trim().eq_ignore_ascii_case("set-cookie") {
                return None;
            }
            let (n, val) = v.trim().split(';').next()?.split_once('=')?;
            (n == name).then(|| val.to_owned())
        })
    }
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
    let split = raw
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .unwrap_or(raw.len());
    let head = String::from_utf8_lossy(&raw[..split]).into_owned();
    let rest = raw.get(split + 4..).unwrap_or_default();
    Res {
        status: head[9..12].parse().unwrap(),
        head,
        body: String::from_utf8_lossy(rest).into_owned(),
        body_len: rest.len(),
    }
}

/// 로그인 링크를 열고 확인 버튼까지 눌러 세션 쿠키를 받는다.
fn sign_in(bind: &str, link_url: &str) -> String {
    let rest = link_url.split_once("://").unwrap().1;
    let path = &rest[rest.find('/').unwrap()..];
    let page = http(bind, "GET", path, &[], "");
    assert_eq!(page.status, 200, "{}", page.body);
    let csrf = page.cookie("maru_csrf").unwrap();
    let cookie = format!("maru_csrf={csrf}");
    let res = http(
        bind,
        "POST",
        path,
        &[
            ("Content-Type", "application/x-www-form-urlencoded"),
            ("Cookie", &cookie),
        ],
        &format!("_csrf={csrf}"),
    );
    assert_eq!(res.status, 303, "{}", res.body);
    res.cookie("maru_session").unwrap()
}

#[test]
fn backup_while_running_then_restore_after_stop() {
    let tmp = tempfile::tempdir().unwrap();
    let data = tmp.path().join("사이트");
    let init = maru(
        &data,
        &["init", "--no-samples", "--preset", "blog", "--yes"],
    );
    let agent = format!("Bearer {}", init["token"]["secret"].as_str().unwrap());
    let link = maru(&data, &["login-link"]);
    let post = maru(
        &data,
        &[
            "post",
            "new",
            "--title",
            "되살릴 글",
            "--slug",
            "keep",
            "--publish",
        ],
    );
    let id = post["id"].as_str().unwrap().to_owned();

    let (server, bind) = start_server(&data);
    // 서버가 켜진 채로 CLI 백업
    let made = maru(&data, &["backup"]);
    let name = made["name"].as_str().unwrap().to_owned();
    let file = made["path"].as_str().unwrap().to_owned();
    assert_eq!(made["secrets"], true);
    assert!(Path::new(&file).exists());
    assert_eq!(
        maru(&data, &["backup", "list"]).as_array().unwrap().len(),
        1
    );

    // 켜진 서버면 복원 거부. --yes 없으면 사용법 오류.
    let err = maru_err(&data, &["restore", &file, "--yes"]);
    assert_eq!(err["code"], "CONFLICT", "{err}");
    let err = maru_err(&data, &["restore", &file]);
    assert_eq!(err["code"], "USAGE", "{err}");

    // REST: 에이전트 토큰은 못 다룬다.
    let get = |path: &str, headers: &[(&str, &str)]| http(&bind, "GET", path, headers, "");
    assert_eq!(get("/api/v1/backups", &[]).status, 401);
    assert_eq!(
        get("/api/v1/backups", &[("Authorization", &agent)]).status,
        403
    );

    // 관리 화면: 목록 → 만들기(비밀 빼기) → 내려받기 → 지우기
    let session = sign_in(&bind, link["url"].as_str().unwrap());
    let page = get(
        "/admin/backups",
        &[("Cookie", &format!("maru_session={session}"))],
    );
    assert_eq!(page.status, 200, "{}", page.body);
    assert!(page.body.contains(&name), "{}", page.body);
    assert!(page.body.contains("maru restore"));
    let at = page.body.find("name=\"_csrf\" value=\"").unwrap() + 20;
    let csrf = page.body[at..at + page.body[at..].find('"').unwrap()].to_owned();
    let cookie = format!("maru_session={session}");
    let form = [
        ("Content-Type", "application/x-www-form-urlencoded"),
        ("Cookie", cookie.as_str()),
    ];
    let res = http(
        &bind,
        "POST",
        "/admin/backups",
        &form,
        &format!("_csrf={csrf}&no_secrets=1"),
    );
    assert_eq!(res.status, 303, "{}", res.body);
    let list = maru(&data, &["backup", "list"]);
    let bare = list
        .as_array()
        .unwrap()
        .iter()
        .find(|b| b["secrets"] == false)
        .expect("비밀 뺀 백업")["name"]
        .as_str()
        .unwrap()
        .to_owned();
    let down = get(&format!("/admin/backups/{bare}"), &[("Cookie", &cookie)]);
    assert_eq!(down.status, 200, "{}", down.head);
    assert!(down.head.contains("application/gzip"), "{}", down.head);
    assert!(down.head.contains("attachment"), "{}", down.head);
    assert!(down.body_len > 100);
    let bad = get("/admin/backups/..%2Fmaru.db", &[("Cookie", &cookie)]);
    assert_eq!(bad.status, 404);
    let res = http(
        &bind,
        "POST",
        &format!("/admin/backups/{bare}/delete"),
        &form,
        &format!("_csrf={csrf}"),
    );
    assert_eq!(res.status, 303, "{}", res.body);
    assert_eq!(
        maru(&data, &["backup", "list"]).as_array().unwrap().len(),
        1
    );
    let forged = http(
        &bind,
        "POST",
        &format!("/admin/backups/{name}/delete"),
        &form,
        "_csrf=wrong",
    );
    assert_eq!(forged.status, 403, "CSRF 값이 틀리면 거절");

    let audit = maru(&data, &["audit", "list", "--action", "backup."]);
    assert_eq!(audit["total"], 3, "만들기 둘·지우기 하나: {audit}");

    // 글을 지우고 서버를 끈 뒤 복원하면 돌아온다. DB 는 첫 백업 시점 사본이다.
    assert_eq!(maru(&data, &["post", "delete", &id])["status"], "deleted");
    drop(server);
    let done = maru(&data, &["restore", &file, "--yes"]);
    assert!(Path::new(done["previous"].as_str().unwrap()).exists());
    assert_eq!(maru(&data, &["post", "show", &id])["status"], "published");
    // 되살린 데이터로 서버가 다시 뜬다.
    let (_server, bind) = start_server(&data);
    assert_eq!(http(&bind, "GET", "/", &[], "").status, 200);
}
