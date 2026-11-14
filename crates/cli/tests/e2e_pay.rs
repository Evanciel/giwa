//! 결제 모듈 (ADR-0166). 실제 바이너리·실제 포트·실제 DB 파일과 이 테스트가 띄운 가짜 토스페이먼츠 서버로
//! 결제 요청 링크 → 결제 창 → 승인, 금액 위조·지난 창 거절, 웹훅 본문 위조 무시, 부분·전액 환불과 멱등 키,
//! 무통장 입금, 가상계좌 입금 알림, 기간 만료, 5년 지난 구매자 정보 지우기, 키가 출력·감사에 새지 않는지를 본다.
#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::thread;

use serde_json::{Value, json};

const BIN: &str = env!("CARGO_BIN_EXE_giwa");
const FORM: (&str, &str) = ("Content-Type", "application/x-www-form-urlencoded");
const SAME: (&str, &str) = ("Sec-Fetch-Site", "same-origin");
const API_ENV: &str = "GIWA_PAY_TOSS_API";
const CLIENT_KEY: &str = "test_gck_e2eclient";
const SECRET_KEY: &str = "test_gsk_e2esecret9876";

struct Server(Child);

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn run(data: &Path, api: &str, args: &[&str]) -> Value {
    let out = Command::new(BIN)
        .arg("--data")
        .arg(data)
        .args(args)
        .arg("--json")
        .env(API_ENV, api)
        .output()
        .expect("giwa 실행");
    let stdout = String::from_utf8(out.stdout).expect("UTF-8 출력");
    serde_json::from_str(stdout.trim()).unwrap_or_else(|e| panic!("JSON 아님: {e}\n{stdout}"))
}

fn giwa(data: &Path, api: &str, args: &[&str]) -> Value {
    let v = run(data, api, args);
    assert_eq!(v["ok"], true, "{args:?} → {v}");
    v["data"].clone()
}

fn start_server(data: &Path, api: &str) -> (Server, String) {
    let mut child = Command::new(BIN)
        .arg("--data")
        .arg(data)
        .args(["serve", "--bind", "127.0.0.1:0", "--json"])
        .env(API_ENV, api)
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
}

impl Res {
    fn header(&self, name: &str) -> String {
        self.head
            .lines()
            .find_map(|l| {
                let (k, v) = l.split_once(':')?;
                k.trim()
                    .eq_ignore_ascii_case(name)
                    .then(|| v.trim().to_owned())
            })
            .unwrap_or_default()
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
    let text = String::from_utf8_lossy(&raw).into_owned();
    let (head, body) = text.split_once("\r\n\r\n").unwrap_or((&text, ""));
    Res {
        status: head[9..12].parse().unwrap(),
        head: head.to_owned(),
        body: body.to_owned(),
    }
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

fn form(pairs: &[(&str, &str)]) -> String {
    pairs
        .iter()
        .map(|(k, v)| format!("{}={}", enc(k), enc(v)))
        .collect::<Vec<_>>()
        .join("&")
}

fn base64(input: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::new();
    for c in input.chunks(3) {
        let b = [c[0], *c.get(1).unwrap_or(&0), *c.get(2).unwrap_or(&0)];
        let n = (u32::from(b[0]) << 16) | (u32::from(b[1]) << 8) | u32::from(b[2]);
        for i in 0..4 {
            if i <= c.len() {
                out.push(T[(n >> (18 - 6 * i) & 63) as usize] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

/// 가짜 토스페이먼츠가 받은 요청.
#[derive(Debug, Clone)]
struct Got {
    method: String,
    path: String,
    auth: String,
    idem: String,
    body: Value,
}

/// 결제 상태를 들고 공식 문서의 응답 모양(Payment 객체)으로 답하는 가짜 서버.
/// paymentKey `pk-vbank` 은 가상계좌(입금 대기), `pk-refuse` 는 승인 거절이다.
struct Toss {
    api: String,
    got: Arc<Mutex<Vec<Got>>>,
    payments: Arc<Mutex<HashMap<String, Value>>>,
}

impl Toss {
    fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let api = format!("http://127.0.0.1:{}", listener.local_addr().unwrap().port());
        let got = Arc::new(Mutex::new(Vec::new()));
        let payments = Arc::new(Mutex::new(HashMap::new()));
        let (seen, pays) = (got.clone(), payments.clone());
        thread::spawn(move || {
            let mut answered: HashMap<String, (u16, Value)> = HashMap::new();
            for stream in listener.incoming() {
                let Ok(stream) = stream else { continue };
                answer(stream, &seen, &pays, &mut answered);
            }
        });
        Self { api, got, payments }
    }

    fn count(&self, method: &str, prefix: &str) -> usize {
        self.got
            .lock()
            .unwrap()
            .iter()
            .filter(|g| g.method == method && g.path.starts_with(prefix))
            .count()
    }

    fn last(&self, method: &str, prefix: &str) -> Got {
        self.got
            .lock()
            .unwrap()
            .iter()
            .rev()
            .find(|g| g.method == method && g.path.starts_with(prefix))
            .cloned()
            .unwrap()
    }

    fn set_status(&self, key: &str, status: &str) {
        self.payments.lock().unwrap().get_mut(key).unwrap()["status"] = json!(status);
    }
}

fn answer(
    stream: TcpStream,
    seen: &Mutex<Vec<Got>>,
    pays: &Mutex<HashMap<String, Value>>,
    answered: &mut HashMap<String, (u16, Value)>,
) {
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
            headers.push((k.trim().to_lowercase(), v.trim().to_owned()));
        }
    }
    let h = |n: &str| {
        headers
            .iter()
            .find(|(k, _)| k == n)
            .map(|(_, v)| v.clone())
            .unwrap_or_default()
    };
    let len: usize = h("content-length").parse().unwrap_or(0);
    let mut raw = vec![0u8; len];
    reader.read_exact(&mut raw).unwrap_or(());
    let mut parts = first.split_whitespace();
    let method = parts.next().unwrap_or("").to_owned();
    let path = parts.next().unwrap_or("/").to_owned();
    let body: Value = serde_json::from_slice(&raw).unwrap_or(Value::Null);
    let idem = h("idempotency-key");
    seen.lock().unwrap().push(Got {
        method: method.clone(),
        path: path.clone(),
        auth: h("authorization"),
        idem: idem.clone(),
        body: body.clone(),
    });
    // 같은 멱등 키는 처음 응답을 그대로 (공식 문서).
    let (status, out) = if let Some(prev) = answered.get(&idem).filter(|_| !idem.is_empty()) {
        prev.clone()
    } else {
        let r = respond(&method, &path, &body, pays);
        if !idem.is_empty() {
            answered.insert(idem, r.clone());
        }
        r
    };
    let text = out.to_string();
    let mut stream = stream;
    let _ = stream.write_all(
        format!(
            "HTTP/1.1 {status} X\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{text}",
            text.len()
        )
        .as_bytes(),
    );
}

fn respond(
    method: &str,
    path: &str,
    body: &Value,
    pays: &Mutex<HashMap<String, Value>>,
) -> (u16, Value) {
    let mut pays = pays.lock().unwrap();
    if method == "POST" && path == "/v1/payments/confirm" {
        let key = body["paymentKey"].as_str().unwrap_or("").to_owned();
        if key == "pk-refuse" {
            return (
                400,
                json!({ "code": "REJECT_CARD_PAYMENT", "message": "한도초과 혹은 잔액부족으로 결제에 실패했습니다." }),
            );
        }
        let amount = body["amount"].as_i64().unwrap_or(0);
        let mut p = json!({
            "paymentKey": key, "orderId": body["orderId"], "status": "DONE",
            "totalAmount": amount, "balanceAmount": amount, "method": "카드",
        });
        if key == "pk-vbank" {
            p["status"] = json!("WAITING_FOR_DEPOSIT");
            p["method"] = json!("가상계좌");
            p["secret"] = json!("dep-secret-1");
        }
        pays.insert(key, p.clone());
        return (200, p);
    }
    let rest = path.strip_prefix("/v1/payments/").unwrap_or("");
    if method == "GET" {
        return match pays.get(rest) {
            Some(p) => (200, p.clone()),
            None => (
                404,
                json!({ "code": "NOT_FOUND_PAYMENT", "message": "없음" }),
            ),
        };
    }
    if method == "POST"
        && let Some(key) = rest.strip_suffix("/cancel")
        && let Some(p) = pays.get_mut(key)
    {
        let balance = p["balanceAmount"].as_i64().unwrap();
        let cancel = body["cancelAmount"].as_i64().unwrap_or(balance);
        let left = balance - cancel;
        p["balanceAmount"] = json!(left);
        p["status"] = json!(if left == 0 {
            "CANCELED"
        } else {
            "PARTIAL_CANCELED"
        });
        return (200, p.clone());
    }
    (404, json!({ "code": "NOT_FOUND", "message": "없음" }))
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

/// 주문의 만든 때(`created`)나 기한을 2020년으로 돌린다.
fn backdate(data: &Path, id: &str, created: bool) {
    let id = id.to_owned();
    let q = if created {
        "UPDATE mod_pay_order SET created_at = '2020-01-01T00:00:00.000000Z' WHERE id = ?"
    } else {
        "UPDATE mod_pay_order SET expires_at = '2020-01-01T00:00:00.000000Z' WHERE id = ?"
    };
    db(data, async move |db| {
        sqlx::query(q)
            .bind(id)
            .execute(db.write_pool())
            .await
            .unwrap();
    });
}

/// 결제 요청 링크 `http://…/m/pay/o/<id>?k=<값>` 에서 (주문, 1회용 값).
fn link(v: &Value) -> (String, String) {
    let url = v["pay_url"].as_str().unwrap();
    let path = &url[url.find("/m/pay/o/").unwrap() + 9..];
    let (id, q) = path.split_once("?k=").unwrap();
    (id.to_owned(), q.to_owned())
}

fn order(data: &Path, api: &str, id: &str) -> Value {
    giwa(data, api, &["pay", "order", "--id", id])
}

fn open_toss(bind: &str, id: &str, k: &str) -> Res {
    http(
        bind,
        "POST",
        &format!("/m/pay/o/{id}/toss"),
        &[FORM, SAME],
        &form(&[("k", k)]),
    )
}

fn back_from_toss(bind: &str, id: &str, k: &str, key: &str, pg: &str, amount: &str) -> Res {
    http(
        bind,
        "GET",
        &format!(
            "/m/pay/return/toss?o={id}&k={k}&paymentKey={key}&orderId={pg}&amount={amount}&paymentType=NORMAL"
        ),
        &[],
        "",
    )
}

fn webhook(bind: &str, body: &Value) -> Res {
    http(
        bind,
        "POST",
        "/m/pay/webhook/toss",
        &[("Content-Type", "application/json")],
        &body.to_string(),
    )
}

#[test]
fn pay_link_toss_bank_refund_webhook_expiry() {
    let tmp = tempfile::tempdir().unwrap();
    let data = tmp.path().join("사이트");
    let toss = Toss::start();
    let api = toss.api.clone();
    let data = data.as_path();
    giwa(
        data,
        &api,
        &["init", "--no-samples", "--preset", "blog", "--yes"],
    );
    giwa(data, &api, &["module", "enable", "pay"]);

    // 결제 방법이 없으면 결제 요청을 만들지 않고 다음 명령을 알려 준다.
    let none = run(
        data,
        &api,
        &[
            "pay",
            "order_create",
            "--name",
            "상담비",
            "--amount",
            "30000",
        ],
    );
    assert_eq!(none["ok"], false, "{none}");
    assert!(none.to_string().contains("provider_set"), "{none}");

    // 키 검사: 운영 키는 https 사이트에서만, API 개별 연동 키는 아직, test/live 섞기 거절.
    for (c, s) in [
        ("live_gck_x1", "live_gsk_x2"),
        ("test_ck_x1", "test_sk_x2"),
        ("test_gck_x1", "live_gsk_x2"),
    ] {
        let v = run(
            data,
            &api,
            &[
                "pay",
                "provider_set",
                "--provider",
                "toss",
                "--client-key",
                c,
                "--secret-key",
                s,
            ],
        );
        assert_eq!(v["ok"], false, "{c}/{s} → {v}");
    }
    let set = giwa(
        data,
        &api,
        &[
            "pay",
            "provider_set",
            "--provider",
            "toss",
            "--enabled",
            "true",
            "--client-key",
            CLIENT_KEY,
            "--secret-key",
            SECRET_KEY,
        ],
    );
    assert_eq!(set["secret_hint"], "…9876");
    assert!(
        set["webhook_url"]
            .as_str()
            .unwrap()
            .ends_with("/m/pay/webhook/toss")
    );
    let listed = giwa(data, &api, &["pay", "provider_list"]);
    assert!(!listed.to_string().contains(SECRET_KEY), "{listed}");
    let leaked = db(data, async |db| {
        sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM audit_log WHERE diff LIKE '%e2esecret%'")
            .fetch_one(db.read_pool())
            .await
            .unwrap()
    });
    assert_eq!(leaked, 0, "시크릿 키가 감사 기록에 남으면 안 된다");

    // 무통장 입금은 계좌가 있어야 켜진다.
    let v = run(
        data,
        &api,
        &[
            "pay",
            "provider_set",
            "--provider",
            "bank",
            "--enabled",
            "true",
        ],
    );
    assert_eq!(v["ok"], false, "{v}");
    giwa(
        data,
        &api,
        &[
            "pay",
            "provider_set",
            "--provider",
            "bank",
            "--enabled",
            "true",
            "--bank",
            "시험은행",
            "--account",
            "000-0000-0000",
            "--holder",
            "마루상점",
        ],
    );

    let (_server, bind) = start_server(data, &api);

    // 1. 결제 요청 링크 → 주문 화면. 1회용 값이 틀리면 없는 주문처럼 보인다.
    let created = giwa(
        data,
        &api,
        &[
            "pay",
            "order_create",
            "--name",
            "상담비",
            "--amount",
            "30000",
            "--buyer-name",
            "손님",
            "--buyer-email",
            "buyer@example.test",
        ],
    );
    assert_eq!(created["order"]["status"], "pending");
    let (id, k) = link(&created);
    assert_eq!(
        http(&bind, "GET", &format!("/m/pay/o/{id}?k=wrong"), &[], "").status,
        404
    );
    let page = http(&bind, "GET", &format!("/m/pay/o/{id}?k={k}"), &[], "");
    assert_eq!(page.status, 200, "{}", page.body);
    assert!(page.body.contains("30,000원"), "{}", page.body);
    assert!(page.body.contains("카드·간편결제로 결제"));
    assert!(page.body.contains("무통장 입금으로 주문"));
    assert_eq!(page.header("cache-control"), "no-store");

    // 2. 결제 창: 다른 출처는 거절, 이 화면에만 결제사 스크립트 허용, 시크릿 키는 안 나간다.
    let cross = http(
        &bind,
        "POST",
        &format!("/m/pay/o/{id}/toss"),
        &[FORM, ("Sec-Fetch-Site", "cross-site")],
        &form(&[("k", &k)]),
    );
    assert_eq!(cross.status, 403);
    let first = open_toss(&bind, &id, &k);
    assert_eq!(first.status, 200, "{}", first.body);
    assert!(
        first.body.contains(&format!("data-order-id=\"{id}-1\"")),
        "{}",
        first.body
    );
    assert!(
        first
            .body
            .contains(&format!("data-client-key=\"{CLIENT_KEY}\""))
    );
    assert!(first.body.contains("data-amount=\"30000\""));
    assert!(!first.body.contains(SECRET_KEY));
    assert!(
        first
            .header("content-security-policy")
            .contains("https://js.tosspayments.com")
    );
    assert!(
        http(&bind, "GET", "/m/pay/checkout.js", &[], "")
            .body
            .contains("renderPaymentWindow")
    );

    // 3. 창을 다시 열면 지난 창으로는 승인하지 않는다. 금액을 바꿔 돌아와도 승인하지 않는다.
    let second = open_toss(&bind, &id, &k);
    assert!(second.body.contains(&format!("data-order-id=\"{id}-2\"")));
    let stale = back_from_toss(&bind, &id, &k, "pk-1", &format!("{id}-1"), "30000");
    assert_eq!(stale.status, 409, "{}", stale.body);
    let forged = back_from_toss(&bind, &id, &k, "pk-1", &format!("{id}-2"), "100");
    assert_eq!(forged.status, 409);
    assert!(
        forged.body.contains("주문 금액과 다릅니다"),
        "{}",
        forged.body
    );
    assert_eq!(
        toss.count("POST", "/v1/payments/confirm"),
        0,
        "결제사에 묻지도 않는다"
    );

    // 4. 제대로 돌아오면 서버가 승인한다: Basic 인증, 멱등 키 = 결제사 주문 번호.
    let ok = back_from_toss(&bind, &id, &k, "pk-1", &format!("{id}-2"), "30000");
    assert_eq!(ok.status, 303, "{}", ok.body);
    assert_eq!(ok.header("location"), format!("/m/pay/o/{id}?k={k}"));
    let c = toss.last("POST", "/v1/payments/confirm");
    assert_eq!(
        c.auth,
        format!("Basic {}", base64(format!("{SECRET_KEY}:").as_bytes()))
    );
    assert_eq!(c.idem, format!("{id}-2"));
    assert_eq!(c.body["amount"], 30000);
    let o = order(data, &api, &id);
    assert_eq!(o["order"]["status"], "paid");
    assert_eq!(o["method"], "카드");
    assert_eq!(o["untrusted"], json!(["buyer"]));
    // 새로 고쳐도 두 번 승인하지 않는다.
    assert_eq!(
        back_from_toss(&bind, &id, &k, "pk-1", &format!("{id}-2"), "30000").status,
        303
    );
    assert_eq!(toss.count("POST", "/v1/payments/confirm"), 1);
    let paid_page = http(&bind, "GET", &format!("/m/pay/o/{id}?k={k}"), &[], "");
    assert!(paid_page.body.contains("결제 완료"));
    assert!(!paid_page.body.contains("카드·간편결제로 결제"));

    // 5. 웹훅 본문은 믿지 않는다: 「취소됐다」고 위조해도 조회 결과(DONE)대로 둔다.
    let lies = webhook(
        &bind,
        &json!({ "eventType": "PAYMENT_STATUS_CHANGED", "createdAt": "2026-11-13T00:00:00.000000",
                 "data": { "paymentKey": "pk-1", "orderId": format!("{id}-2"), "status": "CANCELED",
                           "totalAmount": 30000 } }),
    );
    assert_eq!(lies.status, 200);
    assert_eq!(
        toss.count("GET", "/v1/payments/pk-1"),
        1,
        "조회 API 로 다시 물었다"
    );
    assert_eq!(order(data, &api, &id)["order"]["status"], "paid");
    assert_eq!(webhook(&bind, &json!("이상한 본문")).status, 200);

    // 6. 에이전트는 환불하지 못한다(사람만).
    let token = giwa(
        data,
        &api,
        &[
            "token", "create", "--name", "도구", "--scope", "read", "--scope", "admin",
        ],
    );
    let auth = format!("Bearer {}", token["secret"].as_str().unwrap());
    let agent = http(
        &bind,
        "POST",
        "/api/v1/modules/pay/tools/refund",
        &[
            ("Authorization", &auth),
            ("Content-Type", "application/json"),
        ],
        &json!({ "order": id, "reason": "에이전트 시도" }).to_string(),
    );
    assert_eq!(agent.status, 403, "{}", agent.body);
    assert!(agent.body.contains("사람이 합니다"), "{}", agent.body);
    let agent_orders = http(
        &bind,
        "POST",
        "/api/v1/modules/pay/tools/orders",
        &[
            ("Authorization", &auth),
            ("Content-Type", "application/json"),
        ],
        "{}",
    );
    assert_eq!(
        agent_orders.status, 200,
        "에이전트는 볼 수는 있다: {}",
        agent_orders.body
    );

    // 7. 부분 환불 → 나머지 전액 환불. 멱등 키는 환불마다 다르고, 전액이면 cancelAmount 를 싣지 않는다.
    let part = giwa(
        data,
        &api,
        &[
            "pay",
            "refund",
            "--order",
            &id,
            "--amount",
            "10000",
            "--reason",
            "일부 취소",
        ],
    );
    assert_eq!(part["status"], "partial_refunded");
    let c1 = toss.last("POST", "/v1/payments/pk-1/cancel");
    assert_eq!(c1.body["cancelAmount"], 10000);
    assert_eq!(c1.body["cancelReason"], "일부 취소");
    assert_eq!(c1.idem, part["refund_id"].as_str().unwrap());
    let rest = giwa(
        data,
        &api,
        &["pay", "refund", "--order", &id, "--reason", "나머지 취소"],
    );
    assert_eq!(rest["status"], "refunded");
    assert_eq!(rest["amount"], 20000);
    let c2 = toss.last("POST", "/v1/payments/pk-1/cancel");
    assert!(c2.body.get("cancelAmount").is_none(), "{:?}", c2.body);
    assert_ne!(c1.idem, c2.idem);
    let again = run(
        data,
        &api,
        &["pay", "refund", "--order", &id, "--reason", "또"],
    );
    assert_eq!(again["ok"], false);
    let o = order(data, &api, &id);
    assert_eq!(o["order"]["refunded"], 30000);
    assert_eq!(o["refunds"].as_array().unwrap().len(), 2);
    // 결제사 알림이 와도 이미 환불한 주문은 그대로.
    webhook(
        &bind,
        &json!({ "eventType": "PAYMENT_STATUS_CHANGED", "data": { "paymentKey": "pk-1", "orderId": format!("{id}-2") } }),
    );
    assert_eq!(order(data, &api, &id)["order"]["status"], "refunded");

    // 8. 무통장 입금: 입금자 이름 → 입금 대기·계좌 안내 → 운영자가 입금 확인.
    let b = giwa(
        data,
        &api,
        &[
            "pay",
            "order_create",
            "--name",
            "수강료",
            "--amount",
            "50000",
        ],
    );
    let (bid, bk) = link(&b);
    let empty = http(
        &bind,
        "POST",
        &format!("/m/pay/o/{bid}/bank"),
        &[FORM, SAME],
        &form(&[("k", &bk), ("depositor", "")]),
    );
    assert_eq!(empty.status, 422, "{}", empty.body);
    let chose = http(
        &bind,
        "POST",
        &format!("/m/pay/o/{bid}/bank"),
        &[FORM, SAME],
        &form(&[("k", &bk), ("depositor", "김입금")]),
    );
    assert_eq!(chose.status, 303, "{}", chose.body);
    let waiting = http(&bind, "GET", &format!("/m/pay/o/{bid}?k={bk}"), &[], "");
    assert!(waiting.body.contains("000-0000-0000"), "{}", waiting.body);
    assert!(waiting.body.contains("김입금"));
    assert_eq!(
        order(data, &api, &bid)["order"]["status"],
        "awaiting_deposit"
    );
    let signals = giwa(data, &api, &["signals"]);
    assert!(signals.to_string().contains("pay_deposit"), "{signals}");
    let marked = giwa(data, &api, &["pay", "mark_paid", "--id", &bid]);
    assert_eq!(marked["changed"], true);
    assert_eq!(
        giwa(data, &api, &["pay", "mark_paid", "--id", &bid])["changed"],
        false
    );
    let bank_refund = giwa(
        data,
        &api,
        &["pay", "refund", "--order", &bid, "--reason", "환불"],
    );
    assert_eq!(bank_refund["manual"], true);
    assert_eq!(
        toss.count("POST", "/v1/payments/pk-1/cancel"),
        2,
        "무통장 환불은 결제사에 보내지 않는다"
    );

    // 9. 가상계좌: 승인하면 입금 대기, secret 이 틀린 입금 알림은 무시, 맞으면 조회로 확인해 결제 완료.
    let v = giwa(
        data,
        &api,
        &[
            "pay",
            "order_create",
            "--name",
            "가상계좌",
            "--amount",
            "7000",
        ],
    );
    let (vid, vk) = link(&v);
    open_toss(&bind, &vid, &vk);
    assert_eq!(
        back_from_toss(&bind, &vid, &vk, "pk-vbank", &format!("{vid}-1"), "7000").status,
        303
    );
    assert_eq!(
        order(data, &api, &vid)["order"]["status"],
        "awaiting_deposit"
    );
    toss.set_status("pk-vbank", "DONE");
    webhook(
        &bind,
        &json!({ "createdAt": "x", "secret": "wrong", "status": "DONE", "transactionKey": "t", "orderId": format!("{vid}-1") }),
    );
    assert_eq!(
        order(data, &api, &vid)["order"]["status"],
        "awaiting_deposit"
    );
    webhook(
        &bind,
        &json!({ "createdAt": "x", "secret": "dep-secret-1", "status": "DONE", "transactionKey": "t", "orderId": format!("{vid}-1") }),
    );
    assert_eq!(order(data, &api, &vid)["order"]["status"], "paid");

    // 10. 결제사가 승인을 거절하면 주문은 결제 대기 그대로, 까닭을 보여 주고 다시 결제할 수 있다.
    let r = giwa(
        data,
        &api,
        &["pay", "order_create", "--name", "거절", "--amount", "9000"],
    );
    let (rid, rk) = link(&r);
    open_toss(&bind, &rid, &rk);
    let refused = back_from_toss(&bind, &rid, &rk, "pk-refuse", &format!("{rid}-1"), "9000");
    assert_eq!(refused.status, 409);
    assert!(refused.body.contains("한도초과"), "{}", refused.body);
    let o = order(data, &api, &rid);
    assert_eq!(o["order"]["status"], "pending");
    assert!(
        o["last_error"]
            .as_str()
            .unwrap()
            .contains("REJECT_CARD_PAYMENT")
    );
    // 결제 창이 실패로 돌려보낸 문장은 이스케이프해 보여 준다.
    let failed = http(
        &bind,
        "GET",
        &format!("/m/pay/o/{rid}?k={rk}&code=X&message=%3Cb%3E%EC%8B%A4%ED%8C%A8%3C%2Fb%3E"),
        &[],
        "",
    );
    assert!(failed.body.contains("&lt;b&gt;실패"), "{}", failed.body);

    // 11. 기간이 지나면 끝나고 결제할 수 없다.
    let e = giwa(
        data,
        &api,
        &[
            "pay",
            "order_create",
            "--name",
            "기한",
            "--amount",
            "1000",
            "--expires-minutes",
            "5",
        ],
    );
    let (eid, ek) = link(&e);
    backdate(data, &eid, false);
    let ended = http(&bind, "GET", &format!("/m/pay/o/{eid}?k={ek}"), &[], "");
    assert!(ended.body.contains("기간 끝남"), "{}", ended.body);
    assert_eq!(open_toss(&bind, &eid, &ek).status, 409);

    // 12. 5년이 지난 주문의 구매자 정보는 지운다(결제 기록은 남는다).
    backdate(data, &id, true);
    giwa(data, &api, &["privacy", "run"]);
    let o = order(data, &api, &id);
    assert_eq!(o["buyer"]["email"], "");
    assert_eq!(o["order"]["amount"], 30000);

    // 감사 기록: 결제·환불 이벤트가 맡긴 모듈 정보와 함께 남고, 구매자 메일은 남지 않는다.
    let (paid, mails) = db(data, async |db| {
        let paid = sqlx::query_scalar::<_, i64>(
            "SELECT COUNT(*) FROM audit_log WHERE action = 'pay.paid' AND diff LIKE '%\"owner\":\"pay\"%'",
        )
        .fetch_one(db.read_pool())
        .await
        .unwrap();
        let mails = sqlx::query_scalar::<_, i64>(
            "SELECT COUNT(*) FROM audit_log WHERE diff LIKE '%buyer@example.test%'",
        )
        .fetch_one(db.read_pool())
        .await
        .unwrap();
        (paid, mails)
    });
    assert_eq!(paid, 3, "토스·무통장·가상계좌");
    assert_eq!(mails, 0);
}
