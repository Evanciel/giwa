//! GAP S13: 나가는 웹훅 (ADR-0085). 실제 바이너리·실제 DB 파일과 이 테스트가 띄운 작은 받는 서버로
//! 서명, 이벤트 고르기, 실패한 곳만 다시 보내기, 사설 주소 거부, 키가 출력·감사에 새지 않는지를 본다.
#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::path::Path;
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;

use hmac::{Hmac, KeyInit, Mac};
use serde_json::Value;
use sha2::Sha256;

const BIN: &str = env!("CARGO_BIN_EXE_giwa");
const ALLOW: &str = "GIWA_TEST_ALLOW_LOOPBACK_FETCH";

fn run(data: &Path, args: &[&str], allow_loopback: bool) -> Value {
    let mut cmd = Command::new(BIN);
    cmd.arg("--data").arg(data).args(args).arg("--json");
    if allow_loopback {
        cmd.env(ALLOW, "1");
    } else {
        cmd.env_remove(ALLOW);
    }
    let out = cmd.output().expect("giwa 실행");
    let stdout = String::from_utf8(out.stdout).expect("UTF-8 출력");
    serde_json::from_str(stdout.trim()).unwrap_or_else(|e| panic!("JSON 아님: {e}\n{stdout}"))
}

fn giwa(data: &Path, args: &[&str]) -> Value {
    let v = run(data, args, true);
    assert_eq!(v["ok"], true, "{args:?} → {v}");
    v["data"].clone()
}

#[derive(Debug, Clone)]
struct Got {
    path: String,
    headers: Vec<(String, String)>,
    body: String,
}

impl Got {
    fn header(&self, name: &str) -> &str {
        self.headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
            .unwrap_or("")
    }
}

/// 받은 요청을 모으는 서버. `/b` 는 `fail_b` 가 켜져 있으면 500 을 돌려준다.
fn receiver() -> (u16, Arc<Mutex<Vec<Got>>>, Arc<AtomicBool>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let got = Arc::new(Mutex::new(Vec::new()));
    let fail_b = Arc::new(AtomicBool::new(false));
    let (seen, fail) = (got.clone(), fail_b.clone());
    thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { continue };
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut first = String::new();
            reader.read_line(&mut first).unwrap_or(0);
            let mut headers = Vec::new();
            loop {
                let mut l = String::new();
                if reader.read_line(&mut l).unwrap_or(0) == 0 || l == "\r\n" {
                    break;
                }
                if let Some((k, v)) = l.trim_end().split_once(':') {
                    headers.push((k.trim().to_owned(), v.trim().to_owned()));
                }
            }
            let len: usize = headers
                .iter()
                .find(|(k, _)| k.eq_ignore_ascii_case("content-length"))
                .and_then(|(_, v)| v.parse().ok())
                .unwrap_or(0);
            let mut body = vec![0u8; len];
            reader.read_exact(&mut body).unwrap_or(());
            let path = first.split_whitespace().nth(1).unwrap_or("/").to_owned();
            let status = if path == "/b" && fail.load(Ordering::SeqCst) {
                "500 Internal Server Error"
            } else {
                "204 No Content"
            };
            seen.lock().unwrap().push(Got {
                path,
                headers,
                body: String::from_utf8_lossy(&body).into_owned(),
            });
            let _ = stream.write_all(
                format!("HTTP/1.1 {status}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
                    .as_bytes(),
            );
        }
    });
    (port, got, fail_b)
}

fn hits(got: &Arc<Mutex<Vec<Got>>>, path: &str, event: &str) -> Vec<Got> {
    got.lock()
        .unwrap()
        .iter()
        .filter(|g| g.path == path && g.header("Giwa-Webhook-Event") == event)
        .cloned()
        .collect()
}

fn verify(g: &Got, secret: &str) -> bool {
    let ts = g.header("Giwa-Webhook-Timestamp");
    let mut mac = Hmac::<Sha256>::new_from_slice(secret.as_bytes()).unwrap();
    mac.update(format!("{ts}.{}", g.body).as_bytes());
    g.header("Giwa-Webhook-Signature") == format!("v1={}", hex::encode(mac.finalize().into_bytes()))
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

#[test]
fn webhooks_are_signed_filtered_and_retried_per_endpoint() {
    let tmp = tempfile::tempdir().unwrap();
    let data = tmp.path();
    giwa(data, &["init", "--no-samples", "--preset", "blog", "--yes"]);
    giwa(data, &["module", "enable", "webhook"]);
    let (port, got, fail_b) = receiver();
    let a = format!("http://127.0.0.1:{port}/a");
    let b = format!("http://127.0.0.1:{port}/b");

    // 이 컴퓨터 주소·모르는 이벤트는 받지 않는다
    let v = run(data, &["webhook", "add", "--url", &a], false);
    assert_eq!(v["error"]["code"], "VALIDATION", "{v}");
    assert!(v.to_string().contains("공개 인터넷 주소가 아닙니다"), "{v}");
    let v = run(
        data,
        &["webhook", "add", "--url", "ftp://example.test/x"],
        true,
    );
    assert_eq!(v["error"]["code"], "VALIDATION", "{v}");
    let v = run(
        data,
        &["webhook", "add", "--url", &a, "--event", "member.update"],
        true,
    );
    assert_eq!(v["error"]["code"], "VALIDATION", "{v}");

    // 더하기: 키는 처음 한 번만, 같은 주소를 다시 더하면 이벤트만 바뀐다
    let added = giwa(
        data,
        &["webhook", "add", "--url", &a, "--event", "comment.create"],
    );
    assert_eq!(added["created"], true);
    let secret_a = added["secret"].as_str().unwrap().to_owned();
    assert!(
        secret_a.starts_with("mwhs_") && secret_a.len() > 60,
        "{added}"
    );
    let again = giwa(
        data,
        &[
            "webhook",
            "add",
            "--url",
            &a,
            "--event",
            "post.publish",
            "--event",
            "comment.create",
        ],
    );
    assert_eq!(again["created"], false);
    assert_eq!(again["id"], added["id"]);
    assert!(again.get("secret").is_none(), "{again}");
    let added_b = giwa(data, &["webhook", "add", "--url", &b]);
    assert_eq!(added_b["events"][0], "*");
    let id_a = added["id"].as_str().unwrap().to_owned();
    let id_b = added_b["id"].as_str().unwrap().to_owned();

    let list = giwa(data, &["webhook", "list"]);
    assert_eq!(list["items"].as_array().unwrap().len(), 2, "{list}");
    assert!(
        !list.to_string().contains(&secret_a),
        "목록에 키 전체가 보이면 안 된다"
    );
    assert_eq!(
        list["items"][0]["secret_hint"],
        format!("…{}", &secret_a[secret_a.len() - 4..])
    );

    // 시험 알림
    let ping = giwa(data, &["webhook", "ping", "--id", &id_a]);
    assert_eq!(ping["http_status"], 204);
    let pings = hits(&got, "/a", "webhook.ping");
    assert_eq!(pings.len(), 1);
    assert!(verify(&pings[0], &secret_a), "{:?}", pings[0]);

    // 글을 발행하면 두 곳에 간다. b 가 실패하면 실패함에 남고 a 는 한 번만 받는다
    fail_b.store(true, Ordering::SeqCst);
    let post = giwa(
        data,
        &[
            "post",
            "new",
            "--title",
            "웹훅 시험",
            "--body",
            "본문",
            "--publish",
        ],
    );
    let post_id = post["id"].as_str().unwrap().to_owned();
    giwa(data, &["module", "deliver"]);
    let to_a = hits(&got, "/a", "post.publish");
    assert_eq!(to_a.len(), 1, "{to_a:?}");
    let g = &to_a[0];
    assert!(verify(g, &secret_a), "서명이 맞아야 한다: {g:?}");
    let body: Value = serde_json::from_str(&g.body).unwrap();
    assert_eq!(body["event"], "post.publish");
    assert_eq!(body["target"]["type"], "post");
    assert_eq!(body["target"]["id"], post_id.as_str());
    assert_eq!(body["id"], g.header("Giwa-Webhook-Id"));
    assert!(!g.body.contains("본문"), "글 내용은 싣지 않는다");
    assert_eq!(g.header("Content-Type"), "application/json");
    assert_eq!(hits(&got, "/b", "post.publish").len(), 1);
    let failures = giwa(data, &["module", "failures", "webhook"]);
    let f = failures.as_array().unwrap();
    assert_eq!(f.len(), 1, "{failures}");
    assert_eq!(f[0]["action"], "post.publish");
    assert!(
        f[0]["last_error"].as_str().unwrap().contains("응답 500"),
        "{failures}"
    );
    let failed = giwa(data, &["webhook", "deliveries", "--failed", "true"]);
    assert_eq!(failed["items"].as_array().unwrap().len(), 1, "{failed}");
    assert_eq!(failed["items"][0]["endpoint_id"], id_b.as_str());

    // 다시 시도할 때가 되면 b 에만 다시 보낸다
    fail_b.store(false, Ordering::SeqCst);
    db(data, async |db| {
        sqlx::query("UPDATE module_event_failures SET next_at = '2000-01-01T00:00:00Z'")
            .execute(db.write_pool())
            .await
            .unwrap();
    });
    giwa(data, &["module", "deliver"]);
    assert_eq!(
        hits(&got, "/a", "post.publish").len(),
        1,
        "a 는 다시 받지 않는다"
    );
    let to_b = hits(&got, "/b", "post.publish");
    assert_eq!(to_b.len(), 2);
    assert_eq!(
        to_b[0].header("Giwa-Webhook-Id"),
        to_b[1].header("Giwa-Webhook-Id"),
        "같은 이벤트 ID 로 다시 온다(받는 쪽이 겹침을 거른다)"
    );
    assert!(
        giwa(data, &["module", "failures", "webhook"])
            .as_array()
            .unwrap()
            .is_empty()
    );
    let log = giwa(data, &["webhook", "deliveries", "--endpoint", &id_b]);
    assert_eq!(log["items"][0]["ok"], true, "{log}");
    assert_eq!(log["items"][0]["attempts"], 2, "{log}");

    // 고른 이벤트만: 글 고침은 a 에 가지 않고 전부 받는 b 에만 간다
    giwa(data, &["post", "edit", &post_id, "--title", "고친 제목"]);
    giwa(data, &["module", "deliver"]);
    assert!(hits(&got, "/a", "post.update").is_empty());
    assert_eq!(hits(&got, "/b", "post.update").len(), 1);

    // 키를 바꾸면 옛 키 서명은 맞지 않는다
    let rotated = giwa(data, &["webhook", "rotate", "--id", &id_a]);
    let secret_new = rotated["secret"].as_str().unwrap().to_owned();
    assert_ne!(secret_new, secret_a);
    giwa(data, &["webhook", "ping", "--id", &id_a]);
    let last = hits(&got, "/a", "webhook.ping").pop().unwrap();
    assert!(verify(&last, &secret_new) && !verify(&last, &secret_a));

    // 감사에는 호스트만, 키는 어디에도 없다
    let audit = giwa(data, &["audit", "list", "--action", "webhook.add"]);
    let text = audit.to_string();
    assert!(text.contains(&format!("127.0.0.1:{port}")), "{audit}");
    assert!(!text.contains("/a") && !text.contains("mwhs_"), "{audit}");

    // 지우기
    let removed = giwa(data, &["webhook", "remove", "--id", &id_b]);
    assert_eq!(removed["removed"], id_b.as_str());
    assert_eq!(
        giwa(data, &["webhook", "list"])["items"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    let v = run(data, &["webhook", "remove", "--id", &id_b], true);
    assert_eq!(v["error"]["code"], "NOT_FOUND", "{v}");
}
