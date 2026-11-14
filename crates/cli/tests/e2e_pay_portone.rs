//! PortOne V2 어댑터 (ADR-0171). 실제 바이너리·실제 포트·실제 DB 파일과 이 테스트가 띄운 가짜 PortOne 서버로
//! 키 모양·운영 채널 잠금, 결제 창(CSP·값), 실패로 돌아오기, 돌아오면 조회로 결제 완료, 금액·상점이 다른 결제 거절,
//! 웹훅 두 번(멱등)·본문 위조 무시, 부분 환불(currentCancellableAmount), 접수만 된 취소(REQUESTED)와 그 뒤 웹훅,
//! 키가 출력·감사에 새지 않는지를 본다.
#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;

use serde_json::{Value, json};

const BIN: &str = env!("CARGO_BIN_EXE_giwa");
const FORM: (&str, &str) = ("Content-Type", "application/x-www-form-urlencoded");
const SAME: (&str, &str) = ("Sec-Fetch-Site", "same-origin");
const API_ENV: &str = "GIWA_PAY_PORTONE_API";
const STORE: &str = "store-e2e-0001";
const CHANNEL: &str = "channel-key-e2e-0002";
const SECRET: &str = "e2eportonesecret5678";

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

/// 가짜 PortOne 이 받은 요청.
#[derive(Debug, Clone)]
struct Got {
    method: String,
    path: String,
    auth: String,
    body: Value,
}

/// 결제 건을 들고 공식 문서의 응답 모양으로 답하는 가짜 서버. 브라우저 SDK 가 한 결제는 `pay` 로 넣는다.
/// `requested` 가 켜져 있으면 취소를 접수만 한다(REQUESTED).
struct PortOne {
    api: String,
    got: Arc<Mutex<Vec<Got>>>,
    payments: Arc<Mutex<HashMap<String, Value>>>,
    requested: Arc<AtomicBool>,
}

impl PortOne {
    fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let api = format!("http://127.0.0.1:{}", listener.local_addr().unwrap().port());
        let got = Arc::new(Mutex::new(Vec::new()));
        let payments = Arc::new(Mutex::new(HashMap::new()));
        let requested = Arc::new(AtomicBool::new(false));
        let (seen, pays, req) = (got.clone(), payments.clone(), requested.clone());
        thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(stream) = stream else { continue };
                answer(stream, &seen, &pays, &req);
            }
        });
        Self {
            api,
            got,
            payments,
            requested,
        }
    }

    fn pay(&self, id: &str, store: &str, total: i64, status: &str) {
        self.payments.lock().unwrap().insert(
            id.to_owned(),
            json!({
                "status": status, "id": id, "storeId": store, "currency": "KRW",
                "amount": { "total": total, "paid": total, "cancelled": 0 },
                "method": { "type": "PaymentMethodCard" },
            }),
        );
    }

    fn set(&self, id: &str, status: &str, cancelled: i64) {
        let mut pays = self.payments.lock().unwrap();
        let p = pays.get_mut(id).unwrap();
        p["status"] = json!(status);
        p["amount"]["cancelled"] = json!(cancelled);
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
}

fn answer(
    stream: TcpStream,
    seen: &Mutex<Vec<Got>>,
    pays: &Mutex<HashMap<String, Value>>,
    requested: &AtomicBool,
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
    seen.lock().unwrap().push(Got {
        method: method.clone(),
        path: path.clone(),
        auth: h("authorization"),
        body: body.clone(),
    });
    let (status, out) = if h("authorization") != format!("PortOne {SECRET}") {
        (
            401,
            json!({ "type": "UNAUTHORIZED", "message": "인증 정보가 올바르지 않습니다." }),
        )
    } else {
        respond(&method, &path, &body, pays, requested)
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
    requested: &AtomicBool,
) -> (u16, Value) {
    let mut pays = pays.lock().unwrap();
    let rest = path.strip_prefix("/payments/").unwrap_or("");
    let missing = || {
        (
            404,
            json!({ "type": "PAYMENT_NOT_FOUND", "message": "결제 건이 존재하지 않습니다." }),
        )
    };
    if method == "GET" {
        return pays.get(rest).map_or_else(missing, |p| (200, p.clone()));
    }
    if method == "POST"
        && let Some(id) = rest.strip_suffix("/cancel")
    {
        let Some(p) = pays.get_mut(id) else {
            return missing();
        };
        let total = p["amount"]["total"].as_i64().unwrap();
        let cancelled = p["amount"]["cancelled"].as_i64().unwrap();
        let left = total - cancelled;
        // 공식 문서: 취소 가능 잔액 검증에 실패하면 거절한다.
        if body["currentCancellableAmount"].as_i64() != Some(left) {
            return (
                409,
                json!({ "type": "CANCEL_AMOUNT_EXCEEDS_CANCELLABLE_AMOUNT", "message": "취소 가능 잔액 검증에 실패했습니다." }),
            );
        }
        let amount = body["amount"].as_i64().unwrap_or(left);
        let cancel_id = format!("cancel-{}", cancelled + amount);
        if requested.load(Ordering::SeqCst) {
            return (
                200,
                json!({ "cancellation": { "status": "REQUESTED", "id": cancel_id, "totalAmount": amount } }),
            );
        }
        let now = cancelled + amount;
        p["amount"]["cancelled"] = json!(now);
        p["status"] = json!(if now == total {
            "CANCELLED"
        } else {
            "PARTIAL_CANCELLED"
        });
        return (
            200,
            json!({ "cancellation": { "status": "SUCCEEDED", "id": cancel_id, "totalAmount": amount } }),
        );
    }
    missing()
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

fn count(data: &Path, q: &'static str) -> i64 {
    db(data, async move |db| {
        sqlx::query_scalar::<_, i64>(q)
            .fetch_one(db.read_pool())
            .await
            .unwrap()
    })
}

/// 결제 요청 링크 `http://…/m/pay/o/<id>?k=<값>` 에서 (주문, 1회용 값).
fn link(v: &Value) -> (String, String) {
    let url = v["pay_url"].as_str().unwrap();
    let path = &url[url.find("/m/pay/o/").unwrap() + 9..];
    let (id, q) = path.split_once("?k=").unwrap();
    (id.to_owned(), q.to_owned())
}

fn new_order(data: &Path, api: &str, name: &str, amount: &str) -> (String, String) {
    link(&giwa(
        data,
        api,
        &[
            "pay",
            "order_create",
            "--name",
            name,
            "--amount",
            amount,
            "--buyer-name",
            "구매자시험",
            "--buyer-email",
            "buyer@example.test",
        ],
    ))
}

fn order(data: &Path, api: &str, id: &str) -> Value {
    giwa(data, api, &["pay", "order", "--id", id])
}

fn open(bind: &str, id: &str, k: &str) -> Res {
    http(
        bind,
        "POST",
        &format!("/m/pay/o/{id}/portone"),
        &[FORM, SAME],
        &form(&[("k", k)]),
    )
}

fn back(bind: &str, id: &str, k: &str, query: &str) -> Res {
    http(
        bind,
        "GET",
        &format!("/m/pay/return/portone?o={id}&k={k}&{query}"),
        &[],
        "",
    )
}

fn webhook(bind: &str, kind: &str, payment_id: &str) -> Res {
    let body = json!({
        "type": kind, "timestamp": "2026-11-14T00:00:00Z",
        "data": { "paymentId": payment_id, "storeId": STORE, "transactionId": "tx-1" },
    });
    http(
        bind,
        "POST",
        "/m/pay/webhook/portone",
        &[("Content-Type", "application/json")],
        &body.to_string(),
    )
}

#[test]
fn portone_return_webhook_refund() {
    let tmp = tempfile::tempdir().unwrap();
    let data = tmp.path().join("사이트");
    let portone = PortOne::start();
    let api = portone.api.clone();
    let data = data.as_path();
    giwa(
        data,
        &api,
        &["init", "--no-samples", "--preset", "blog", "--yes"],
    );
    giwa(data, &api, &["module", "enable", "pay"]);

    // 1. 키 모양 검사. PortOne 키로는 시험·운영을 알 수 없어 mode 를 적지 않으면 운영으로 보고 잠근다.
    let set = |extra: &[&str]| {
        let mut args = vec![
            "pay",
            "provider_set",
            "--provider",
            "portone",
            "--enabled",
            "true",
            "--client-key",
            CHANNEL,
            "--secret-key",
            SECRET,
        ];
        args.extend_from_slice(extra);
        run(data, &api, &args)
    };
    let v = set(&["--store-id", "abc", "--mode", "test"]);
    assert_eq!(v["ok"], false, "{v}");
    assert!(v.to_string().contains("store-"), "{v}");
    let v = set(&["--store-id", STORE]);
    assert_eq!(v["ok"], false, "{v}");
    assert!(v.to_string().contains("사업자 정보"), "{v}");
    let v = set(&["--store-id", STORE, "--mode", "prod"]);
    assert_eq!(v["ok"], false, "{v}");
    let v = set(&["--store-id", STORE, "--mode", "test"]);
    assert_eq!(v["ok"], true, "{v}");
    let p = &v["data"];
    assert_eq!(p["secret_hint"], "…5678", "{p}");
    assert_eq!(p["live"], false, "{p}");
    assert_eq!(p["usable"], true, "{p}");
    assert!(
        p["webhook_url"]
            .as_str()
            .unwrap()
            .ends_with("/m/pay/webhook/portone")
    );
    let listed = giwa(data, &api, &["pay", "provider_list"]);
    assert!(!listed.to_string().contains(SECRET), "{listed}");
    assert_eq!(
        count(
            data,
            "SELECT COUNT(*) FROM audit_log WHERE diff LIKE '%e2eportonesecret%'"
        ),
        0
    );

    let (_server, bind) = start_server(data, &api);

    // 2. 주문 화면에 PortOne 단추. 결제 창은 같은 출처 폼만 열고, PortOne SDK 만 스크립트로 허용한다.
    let (id, k) = new_order(data, &api, "상담비", "15000");
    let page = http(&bind, "GET", &format!("/m/pay/o/{id}?k={k}"), &[], "");
    assert_eq!(page.status, 200);
    assert!(
        page.body.contains(&format!("/m/pay/o/{id}/portone")),
        "{}",
        page.body
    );
    let forged = http(
        &bind,
        "POST",
        &format!("/m/pay/o/{id}/portone"),
        &[FORM, ("Sec-Fetch-Site", "cross-site")],
        &form(&[("k", &k)]),
    );
    assert_eq!(forged.status, 403);
    let win = open(&bind, &id, &k);
    assert_eq!(win.status, 200, "{}", win.body);
    let csp = win.header("content-security-policy");
    assert!(csp.contains("script-src 'self' https:;"), "{csp}");
    let script = csp
        .split(';')
        .find(|d| d.trim().starts_with("script-src"))
        .unwrap();
    assert!(
        !script.contains("unsafe"),
        "인라인 스크립트는 막는다: {csp}"
    );
    assert!(!script.contains("http:"), "{csp}");
    assert!(
        win.body
            .contains("https://cdn.portone.io/v2/browser-sdk.js")
    );
    assert!(
        win.body.contains(&format!("data-payment-id=\"{id}-1\"")),
        "{}",
        win.body
    );
    assert!(win.body.contains(&format!("data-store-id=\"{STORE}\"")));
    assert!(
        win.body
            .contains(&format!("data-channel-key=\"{CHANNEL}\""))
    );
    assert!(win.body.contains("data-amount=\"15000\""));
    assert!(!win.body.contains(SECRET), "시크릿은 화면에 나가지 않는다");

    // 3. 실패로 돌아오면 결제사 문장을 이스케이프해 보이고 주문은 그대로다.
    let fail = back(
        &bind,
        &id,
        &k,
        &format!(
            "paymentId={id}-1&code=FAILURE_TYPE_PG&message={}",
            enc("<b>카드 한도</b> 초과")
        ),
    );
    assert_eq!(fail.status, 200);
    assert!(fail.body.contains("&lt;b&gt;카드 한도"), "{}", fail.body);
    assert_eq!(order(data, &api, &id)["order"]["status"], "pending");

    // 4. 결제 창에서 결제했다(가짜 PortOne 에 결제 건이 생긴다). 돌아오면 조회해서 결제 완료.
    portone.pay(&format!("{id}-1"), STORE, 15000, "PAID");
    let ok = back(&bind, &id, &k, &format!("paymentId={id}-1"));
    assert_eq!(ok.status, 303, "{}", ok.body);
    assert_eq!(ok.header("location"), format!("/m/pay/o/{id}?k={k}"));
    let g = portone.last("GET", "/payments/");
    assert_eq!(g.path, format!("/payments/{id}-1"));
    assert_eq!(g.auth, format!("PortOne {SECRET}"));
    let o = order(data, &api, &id);
    assert_eq!(o["order"]["status"], "paid", "{o}");
    assert_eq!(o["order"]["provider"], "portone");
    assert_eq!(o["method"], "카드");
    assert_eq!(o["payment_key"], format!("{id}-1"));
    // 새로고침해도 그대로.
    assert_eq!(
        back(&bind, &id, &k, &format!("paymentId={id}-1")).status,
        303
    );

    // 5. 웹훅이 두 번 와도 결제 완료 기록은 하나다. 모르는 type·남의 결제는 넘긴다.
    for _ in 0..2 {
        assert_eq!(
            webhook(&bind, "Transaction.Paid", &format!("{id}-1")).status,
            200
        );
    }
    assert_eq!(
        webhook(&bind, "Billing.Unknown", &format!("{id}-1")).status,
        200
    );
    assert_eq!(webhook(&bind, "Transaction.Paid", "nobody-1").status, 200);
    assert_eq!(
        count(
            data,
            "SELECT COUNT(*) FROM audit_log WHERE action = 'pay.paid'"
        ),
        1
    );

    // 6. 금액이 다른 결제, 다른 상점의 결제는 받지 않는다.
    let (id2, k2) = new_order(data, &api, "수강료", "20000");
    assert_eq!(open(&bind, &id2, &k2).status, 200);
    portone.pay(&format!("{id2}-1"), STORE, 1000, "PAID");
    let bad = back(&bind, &id2, &k2, &format!("paymentId={id2}-1"));
    assert_eq!(bad.status, 409, "{}", bad.body);
    let o2 = order(data, &api, &id2);
    assert_eq!(o2["order"]["status"], "pending");
    assert!(
        o2["last_error"].as_str().unwrap().contains("맞지 않습니다"),
        "{o2}"
    );
    let (id3, k3) = new_order(data, &api, "후원", "30000");
    assert_eq!(open(&bind, &id3, &k3).status, 200);
    portone.pay(&format!("{id3}-1"), "store-other", 30000, "PAID");
    assert_eq!(
        back(&bind, &id3, &k3, &format!("paymentId={id3}-1")).status,
        409
    );
    assert_eq!(order(data, &api, &id3)["order"]["status"], "pending");

    // 7. 웹훅 본문은 믿지 않는다. 결제사에서 아직 READY 면 결제 완료가 아니다.
    let (id4, k4) = new_order(data, &api, "예약금", "40000");
    assert_eq!(open(&bind, &id4, &k4).status, 200);
    portone.pay(&format!("{id4}-1"), STORE, 40000, "READY");
    assert_eq!(
        webhook(&bind, "Transaction.Paid", &format!("{id4}-1")).status,
        200
    );
    assert_eq!(order(data, &api, &id4)["order"]["status"], "pending");
    // 결제 창을 다시 열면 지난 결제 번호로는 돌아올 수 없다.
    assert_eq!(open(&bind, &id4, &k4).status, 200);
    portone.set(&format!("{id4}-1"), "PAID", 0);
    let old = back(&bind, &id4, &k4, &format!("paymentId={id4}-1"));
    assert_eq!(old.status, 409);
    assert!(old.body.contains("다시 연"), "{}", old.body);
    // 돌아오지 못했어도 sync 로 결제사에 물어 맞춘다(결제 키 = 결제사 주문 번호).
    portone.pay(&format!("{id4}-2"), STORE, 40000, "PAID");
    let s = giwa(data, &api, &["pay", "sync", "--id", &id4]);
    assert_eq!(s["changed"], true, "{s}");
    assert_eq!(s["order"]["status"], "paid", "{s}");

    // 8. 부분 환불: 우리가 아는 남은 금액을 currentCancellableAmount 로 보낸다.
    let r = giwa(
        data,
        &api,
        &[
            "pay",
            "refund",
            "--order",
            &id,
            "--amount",
            "5000",
            "--reason",
            "시험 환불",
        ],
    );
    assert_eq!(r["status"], "partial_refunded", "{r}");
    let c = portone.last("POST", "/payments/");
    assert_eq!(c.path, format!("/payments/{id}-1/cancel"));
    assert_eq!(c.body["amount"], 5000);
    assert_eq!(c.body["currentCancellableAmount"], 15000);
    assert_eq!(c.body["reason"], "시험 환불");
    let o = order(data, &api, &id);
    assert_eq!(o["order"]["refunded"], 5000, "{o}");

    // 9. 결제사가 취소를 접수만 하면(REQUESTED) 주문은 그대로 두고, 그동안 또 환불하지 않는다.
    portone.requested.store(true, Ordering::SeqCst);
    let r = giwa(
        data,
        &api,
        &[
            "pay",
            "refund",
            "--order",
            &id,
            "--reason",
            "남은 금액 환불",
        ],
    );
    assert_eq!(r["requested"], true, "{r}");
    assert_eq!(
        portone.last("POST", "/payments/").body["currentCancellableAmount"],
        10000
    );
    let o = order(data, &api, &id);
    assert_eq!(o["order"]["status"], "partial_refunded", "{o}");
    assert_eq!(o["refunds"][1]["status"], "requested", "{o}");
    let again = run(
        data,
        &api,
        &["pay", "refund", "--order", &id, "--reason", "한 번 더"],
    );
    assert_eq!(again["ok"], false, "{again}");
    assert!(again.to_string().contains("앞선 취소"), "{again}");
    let cancels = portone.count("POST", "/payments/");
    assert_eq!(cancels, 2, "접수 중에는 결제사에 다시 보내지 않는다");
    // 결제사에서 취소가 끝나고 웹훅이 오면 주문과 환불 기록이 함께 끝난다.
    portone.set(&format!("{id}-1"), "CANCELLED", 15000);
    assert_eq!(
        webhook(&bind, "Transaction.Cancelled", &format!("{id}-1")).status,
        200
    );
    let o = order(data, &api, &id);
    assert_eq!(o["order"]["status"], "refunded", "{o}");
    assert_eq!(o["order"]["refunded"], 15000, "{o}");
    assert_eq!(o["refunds"][1]["status"], "done", "{o}");
    assert_eq!(
        count(
            data,
            "SELECT COUNT(*) FROM audit_log WHERE action = 'pay.refunded'"
        ),
        2,
        "부분 환불 + 전액 환불"
    );
    assert_eq!(
        count(
            data,
            "SELECT COUNT(*) FROM audit_log WHERE diff LIKE '%e2eportonesecret%'"
        ),
        0
    );
}
