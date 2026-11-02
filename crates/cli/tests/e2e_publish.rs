//! E2E-1 (PHASE1_PLAN §8): init → REST 로 한글 초안 → 공개 URL 404 → CLI 발행 → 실행 중인 서버에 노출
//! → DB 를 직접 읽어 상태·리비전·감사 로그의 Actor 를 대조한다.
#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::path::Path;
use std::process::{Child, Command, Stdio};

use serde_json::Value;

const BIN: &str = env!("CARGO_BIN_EXE_maru");

/// 테스트가 실패해도 서버 프로세스를 남기지 않는다.
struct Server(Child);

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn maru(data: &Path, args: &[&str]) -> Value {
    let out = Command::new(BIN)
        .arg("--data")
        .arg(data)
        .args(args)
        .arg("--json")
        .output()
        .expect("maru 실행");
    let stdout = String::from_utf8(out.stdout).expect("UTF-8 출력");
    let v: Value = serde_json::from_str(stdout.trim()).unwrap_or_else(|e| {
        panic!(
            "JSON 아님: {e}\nstdout: {stdout}\nstderr: {}",
            String::from_utf8_lossy(&out.stderr)
        )
    });
    assert_eq!(v["ok"], true, "{v}");
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
    let bind = v["data"]["bind"].as_str().unwrap().to_owned();
    (server, bind)
}

/// 최소 HTTP/1.1 클라이언트. 응답 상태와 본문을 돌려준다.
fn http(
    bind: &str,
    method: &str,
    path: &str,
    headers: &[(&str, &str)],
    body: &str,
) -> (u16, String) {
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
    let status: u16 = text[9..12].parse().unwrap();
    let body = text
        .split_once("\r\n\r\n")
        .map(|(_, b)| b.to_owned())
        .unwrap_or_default();
    (status, body)
}

fn encode(segment: &str) -> String {
    segment
        .bytes()
        .map(|b| {
            if b.is_ascii_alphanumeric() || b == b'-' {
                (b as char).to_string()
            } else {
                format!("%{b:02X}")
            }
        })
        .collect()
}

#[test]
fn e2e1_init_rest_draft_cli_publish_public_page() {
    let tmp = tempfile::tempdir().unwrap();
    let data = tmp.path().join("내 블로그");

    // 1. init
    let init = maru(&data, &["init", "--preset", "blog", "--yes"]);
    assert_eq!(init["created"], true);
    let token = init["token"]["secret"].as_str().unwrap().to_owned();
    assert!(token.starts_with("maru_"));
    // 다시 실행해도 안전하고 토큰이 늘지 않는다.
    let again = maru(&data, &["init", "--preset", "blog", "--yes"]);
    assert_eq!(again["created"], false);
    assert!(again["token"].is_null());

    let (_server, bind) = start_server(&data);

    // 2. 발급된 토큰으로 REST 초안
    let auth = format!("Bearer {token}");
    let body = r#"{"board":"blog","title":"러스트로 만든 블로그","body_md":"안녕하세요, **마루**입니다."}"#;
    let (status, res) = http(
        &bind,
        "POST",
        "/api/v1/posts",
        &[
            ("Authorization", &auth),
            ("Content-Type", "application/json"),
        ],
        body,
    );
    assert_eq!(status, 201, "{res}");
    let created: Value = serde_json::from_str(&res).unwrap();
    let id = created["data"]["id"].as_str().unwrap().to_owned();
    let slug = created["data"]["slug"].as_str().unwrap().to_owned();
    assert_eq!(created["data"]["status"], "draft");
    let public_path = format!("/b/blog/{}", encode(&slug));

    // 잘못된 토큰은 401, 기본 토큰은 발행 403
    let (status, _) = http(
        &bind,
        "POST",
        "/api/v1/posts",
        &[("Authorization", "Bearer maru_nope")],
        body,
    );
    assert_eq!(status, 401);
    let (status, res) = http(
        &bind,
        "POST",
        &format!("/api/v1/posts/{id}/publish"),
        &[("Authorization", &auth)],
        "",
    );
    assert_eq!(status, 403, "{res}");
    assert!(res.contains("\"FORBIDDEN\""));

    // 3. 초안의 공개 URL 은 404
    let (status, _) = http(&bind, "GET", &public_path, &[], "");
    assert_eq!(status, 404);

    // 4. CLI 로 발행 (서버 실행 중, 다른 프로세스)
    let published = maru(&data, &["post", "publish", &id]);
    assert_eq!(published["status"], "published");
    assert!(published["url"].as_str().unwrap().ends_with(&public_path));

    // 5. 실행 중인 서버에 바로 보인다 (프로세스 캐시 없음)
    let (status, html) = http(&bind, "GET", &public_path, &[], "");
    assert_eq!(status, 200);
    assert!(html.contains("러스트로 만든 블로그"));
    assert!(html.contains("<strong>마루</strong>"));
    let (status, home) = http(&bind, "GET", "/", &[], "");
    assert_eq!(status, 200);
    assert!(home.contains("러스트로 만든 블로그"));

    // 6. DB 직접 대조
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(async {
        let db = maru_storage::Db::open(&data.join("maru.db"), false).await.unwrap();
        let status: String = sqlx::query_scalar("SELECT status FROM posts WHERE id = ?")
            .bind(&id)
            .fetch_one(db.read_pool())
            .await
            .unwrap();
        assert_eq!(status, "published");
        let revisions: i64 = sqlx::query_scalar("SELECT count(*) FROM revisions WHERE post_id = ?")
            .bind(&id)
            .fetch_one(db.read_pool())
            .await
            .unwrap();
        assert_eq!(revisions, 1);
        let token_id = init["token"]["id"].as_str().unwrap().to_owned();
        let audits: Vec<(String, Option<String>, String)> = sqlx::query_as(
            "SELECT actor_type, actor_id, action FROM audit_log WHERE target_id = ? ORDER BY at, id",
        )
        .bind(&id)
        .fetch_all(db.read_pool())
        .await
        .unwrap();
        assert_eq!(
            audits,
            vec![
                ("agent".into(), Some(token_id), "post.create".into()),
                ("local".into(), None, "post.publish".into()),
            ]
        );
        db.close().await;
    });
}

#[test]
fn serve_reports_port_in_use() {
    let tmp = tempfile::tempdir().unwrap();
    let data = tmp.path().join("site");
    maru(&data, &["init", "--yes"]);
    let busy = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = busy.local_addr().unwrap().port();
    let out = Command::new(BIN)
        .arg("--data")
        .arg(&data)
        .args(["serve", "--bind", &format!("127.0.0.1:{port}"), "--json"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(4));
    let v: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v["error"]["code"], "PORT_IN_USE");
    assert!(v["error"]["next"][0].as_str().unwrap().contains("--bind"));
}

#[test]
fn uninitialized_dir_points_to_init() {
    let tmp = tempfile::tempdir().unwrap();
    let out = Command::new(BIN)
        .arg("--data")
        .arg(tmp.path().join("없음"))
        .args(["post", "list", "--json"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(1));
    let v: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v["error"]["code"], "NOT_FOUND");
    assert!(
        v["error"]["next"][0]
            .as_str()
            .unwrap()
            .starts_with("maru init")
    );
}
