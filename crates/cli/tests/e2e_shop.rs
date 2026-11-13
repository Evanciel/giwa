//! 쇼핑 모듈 (ADR-0170). 실제 바이너리·실제 포트·실제 DB 파일로
//! 장바구니 쿠키, 결제 전에 바뀐 가격(새 가격으로 결제), 배송비·무료 배송, 무통장 입금 확인 뒤 재고 차감,
//! 마지막 재고를 두 사람이 결제(한 사람은 재고 모자람 + 환불 신호), 품절이면 주문 못 함, 발송·배송 완료 편지,
//! 취소하면 재고 복구, 주문 조회(틀린 전자우편은 없는 주문), 위조 폼·함정 칸·동의, 관리 화면, 감사 기록에 구매자 정보 없음을 본다.
#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::path::Path;
use std::process::{Child, Command, Stdio};

use serde_json::Value;

const BIN: &str = env!("CARGO_BIN_EXE_maru");
const FORM: (&str, &str) = ("Content-Type", "application/x-www-form-urlencoded");
const SAME: (&str, &str) = ("Sec-Fetch-Site", "same-origin");
const CROSS: (&str, &str) = ("Sec-Fetch-Site", "cross-site");

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

fn db<T>(data: &Path, f: impl AsyncFnOnce(&maru_storage::Db) -> T) -> T {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(async {
        let db = maru_storage::Db::open(&data.join("maru.db"), false)
            .await
            .unwrap();
        let v = f(&db).await;
        db.close().await;
        v
    })
}

/// 쌓인 편지 (받는 주소, 제목, 본문).
fn mails(data: &Path) -> Vec<(String, String, String)> {
    db(data, async |db| {
        sqlx::query_as::<_, (String, String, String)>(
            "SELECT to_addr, subject, body FROM mail_outbox ORDER BY created_at",
        )
        .fetch_all(db.read_pool())
        .await
        .unwrap()
    })
}

/// 로그인 링크로 운영자 세션 쿠키를 받는다.
fn owner_cookie(data: &Path, bind: &str) -> String {
    let link = maru(data, &["login-link"]);
    let rest = link["url"].as_str().unwrap().split_once("://").unwrap().1;
    let path = &rest[rest.find('/').unwrap()..];
    let first = http(bind, "GET", path, &[], "");
    let pre = first.cookie("maru_csrf").unwrap();
    let cookie = format!("maru_csrf={pre}");
    let res = http(
        bind,
        "POST",
        path,
        &[FORM, ("Cookie", cookie.as_str())],
        &form(&[("_csrf", &pre)]),
    );
    let session = res
        .cookie("maru_session")
        .unwrap_or_else(|| panic!("로그인 실패: {}", res.body));
    format!("maru_session={session}")
}

/// 제목에 `subject` 가 든 마지막 편지 본문.
fn letter(data: &Path, subject: &str) -> String {
    mails(data)
        .into_iter()
        .rev()
        .find(|(_, s, _)| s.contains(subject))
        .unwrap_or_else(|| panic!("편지 없음: {subject}"))
        .2
}

/// 새 장바구니에 담고 쿠키를 받는다.
fn add_new(bind: &str, product: &str, option: &str, qty: &str) -> String {
    let res = http(
        bind,
        "POST",
        "/m/shop/cart",
        &[FORM, SAME],
        &form(&[
            ("do", "add"),
            ("product", product),
            ("option", option),
            ("qty", qty),
        ]),
    );
    assert_eq!(res.status, 303, "{}", res.body);
    let key = res.cookie("maru_cart").expect("장바구니 쿠키");
    format!("maru_cart={key}")
}

fn add(bind: &str, cookie: &str, product: &str, option: &str, qty: &str) -> Res {
    http(
        bind,
        "POST",
        "/m/shop/cart",
        &[FORM, SAME, ("Cookie", cookie)],
        &form(&[
            ("do", "add"),
            ("product", product),
            ("option", option),
            ("qty", qty),
        ]),
    )
}

fn buyer() -> Vec<(&'static str, &'static str)> {
    vec![
        ("name", "쇼핑시험"),
        ("email", "Buyer@Example.test"),
        ("phone", "010-0000-0000"),
        ("zip", "00000"),
        ("address", "시험시 시험로 1"),
        ("address2", "101호"),
        ("memo", "문 앞에"),
        ("agree", "1"),
    ]
}

fn place(bind: &str, cookie: &str, pairs: &[(&str, &str)], same: bool) -> Res {
    let site = if same { SAME } else { CROSS };
    http(
        bind,
        "POST",
        "/m/shop/checkout",
        &[FORM, site, ("Cookie", cookie)],
        &form(pairs),
    )
}

/// 주문서를 내고 무통장 입금을 고른다. (결제 주문 id, 쇼핑 주문 id)
fn order_by_bank(data: &Path, bind: &str, cookie: &str) -> (String, String) {
    let res = place(bind, cookie, &buyer(), true);
    assert_eq!(res.status, 303, "{}", res.body);
    let location = res.header("location");
    assert!(location.starts_with("/m/pay/o/"), "{location}");
    let (path, k) = location.split_once("?k=").unwrap();
    let pay_id = path.trim_start_matches("/m/pay/o/").to_owned();
    let o = maru(data, &["pay", "order", "--id", &pay_id]);
    let shop_id = o["order"]["ref_id"].as_str().unwrap().to_owned();
    assert_eq!(o["order"]["owner"], "shop");
    let res = http(
        bind,
        "POST",
        &format!("/m/pay/o/{pay_id}/bank"),
        &[FORM, SAME],
        &form(&[("k", k), ("depositor", "쇼핑시험")]),
    );
    assert_eq!(res.status, 303, "{}", res.body);
    (pay_id, shop_id)
}

fn paid(data: &Path, pay_id: &str) {
    maru(data, &["pay", "mark_paid", "--id", pay_id]);
    maru(data, &["module", "deliver"]);
    maru(data, &["module", "deliver"]);
}

fn order(data: &Path, id: &str) -> Value {
    maru(data, &["shop", "order", "--id", id])["order"].clone()
}

fn stock(data: &Path, id: &str) -> Value {
    maru(data, &["shop", "products"])["products"]
        .as_array()
        .unwrap()
        .iter()
        .find(|p| p["id"] == id)
        .unwrap()["stock"]
        .clone()
}

#[test]
fn cart_price_stock_race_shipping_cancel_and_lookup() {
    let tmp = tempfile::tempdir().unwrap();
    let data = tmp.path().join("사이트");
    let data = data.as_path();
    maru(data, &["init", "--no-samples", "--preset", "blog", "--yes"]);
    maru(data, &["module", "enable", "shop"]);
    maru(data, &["module", "enable", "pay"]);
    maru(
        data,
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
            "000-000-000000",
            "--holder",
            "시험상점",
        ],
    );

    let made = maru(
        data,
        &[
            "shop",
            "product_set",
            "--id",
            "tee",
            "--name",
            "무지 티셔츠",
            "--price",
            "10000",
            "--stock",
            "5",
            "--option-label",
            "크기",
            "--options",
            "S, M ,L",
        ],
    );
    assert_eq!(made["created"], true);
    assert_eq!(
        made["product"]["options"],
        serde_json::json!(["S", "M", "L"])
    );
    maru(
        data,
        &[
            "shop",
            "product_set",
            "--id",
            "last",
            "--name",
            "마지막 한 점",
            "--price",
            "60000",
            "--stock",
            "1",
        ],
    );
    maru(
        data,
        &[
            "shop",
            "product_set",
            "--id",
            "free",
            "--name",
            "스티커",
            "--price",
            "1000",
            "--stock",
            "-1",
        ],
    );
    assert!(stock(data, "free").is_null(), "-1 은 무제한");
    let nameless = run(data, &["shop", "product_set", "--id", "x", "--price", "1"]);
    assert_eq!(
        nameless["ok"], false,
        "새 상품은 이름이 있어야 한다: {nameless}"
    );
    let bad_id = run(
        data,
        &[
            "shop",
            "product_set",
            "--id",
            "Bad Id",
            "--name",
            "x",
            "--price",
            "1",
        ],
    );
    assert_eq!(bad_id["ok"], false, "{bad_id}");

    maru(
        data,
        &[
            "widget",
            "place",
            "product_grid",
            "--region",
            "main",
            "--scope",
            "home",
            "--config",
            r#"{"limit":4,"label":"상품 더 보기","anchor":"shop"}"#,
        ],
    );

    let (_server, bind) = start_server(data);
    let bind = bind.as_str();
    let home = http(bind, "GET", "/", &[], "");
    assert!(
        home.body.contains(r#"href="/m/shop/p/tee""#),
        "{}",
        home.body
    );
    assert!(home.body.contains("무지 티셔츠"));
    let list = http(bind, "GET", "/m/shop", &[], "");
    assert_eq!(list.status, 200);
    assert!(list.body.contains("60,000원"), "{}", list.body);
    let page = http(bind, "GET", "/m/shop/p/tee", &[], "");
    assert_eq!(page.status, 200);
    assert!(page.body.contains(r#"<option value="M""#), "{}", page.body);
    assert_eq!(http(bind, "GET", "/m/shop/p/nope", &[], "").status, 404);

    // 다른 사이트의 폼, 없는 옵션, 옵션 빠짐.
    let forged = http(
        bind,
        "POST",
        "/m/shop/cart",
        &[FORM, CROSS],
        &form(&[
            ("do", "add"),
            ("product", "tee"),
            ("option", "M"),
            ("qty", "1"),
        ]),
    );
    assert_eq!(forged.status, 403);
    let cookie = add_new(bind, "tee", "M", "2");
    let cookie = cookie.as_str();
    assert_eq!(add(bind, cookie, "tee", "XL", "1").status, 422);
    assert_eq!(add(bind, cookie, "tee", "", "1").status, 422);
    let too_many = add(bind, cookie, "tee", "S", "9");
    assert_eq!(too_many.status, 409, "재고보다 많이: {}", too_many.body);
    assert!(too_many.body.contains("남았습니다"), "{}", too_many.body);
    let cart = http(bind, "GET", "/m/shop/cart", &[("Cookie", cookie)], "");
    assert!(cart.body.contains("20,000원"), "{}", cart.body);
    assert!(cart.body.contains("3,000원"), "배송비: {}", cart.body);

    // 담은 뒤 가격이 올랐다: 주문은 새 가격으로.
    maru(
        data,
        &["shop", "product_set", "--id", "tee", "--price", "12000"],
    );
    let sheet = http(bind, "GET", "/m/shop/checkout", &[("Cookie", cookie)], "");
    assert_eq!(sheet.status, 200);
    assert!(sheet.body.contains("27,000원"), "{}", sheet.body);
    // 다른 사이트의 폼, 동의 없음, 함정 칸.
    assert_eq!(place(bind, cookie, &buyer(), false).status, 403);
    let no_agree: Vec<_> = buyer().into_iter().filter(|(k, _)| *k != "agree").collect();
    let res = place(bind, cookie, &no_agree, true);
    assert_eq!(res.status, 422, "{}", res.body);
    assert!(res.body.contains("쇼핑시험"), "적은 값이 남는다");
    let mut trap = buyer();
    trap.push(("website", "x"));
    assert_eq!(place(bind, cookie, &trap, true).status, 303);
    let mut no_address = buyer();
    no_address.retain(|(k, _)| *k != "address");
    assert_eq!(place(bind, cookie, &no_address, true).status, 422);
    assert_eq!(
        maru(data, &["shop", "orders"])["items"]
            .as_array()
            .unwrap()
            .len(),
        0,
        "함정 칸·빈 주소는 주문을 만들지 않는다"
    );

    let (pay_id, first) = order_by_bank(data, bind, cookie);
    let p = maru(data, &["pay", "order", "--id", &pay_id]);
    assert_eq!(
        p["order"]["amount"], 27000,
        "새 가격 12,000 × 2 + 배송비 3,000"
    );
    let o = order(data, &first);
    assert_eq!(o["status"], "pending");
    assert_eq!(o["buyer"]["email"], "buyer@example.test");
    assert_eq!(o["buyer"]["phone"], "01000000000");
    assert_eq!(stock(data, "tee"), 5, "결제 전에는 재고를 줄이지 않는다");
    let done = http(bind, "GET", &format!("/m/shop/o/{first}"), &[], "");
    assert_eq!(done.status, 200);
    assert!(done.body.contains("결제를 기다립니다"), "{}", done.body);
    assert!(
        !done.body.contains("buyer@example.test"),
        "주문 화면에 구매자 정보 없음"
    );

    // 입금 확인 → 결제됨, 재고 차감, 장바구니 비움, 접수 편지.
    paid(data, &pay_id);
    let o = order(data, &first);
    assert_eq!(o["status"], "paid", "{o}");
    assert_eq!(o["stock_taken"], true);
    assert_eq!(stock(data, "tee"), 3);
    let cart = http(bind, "GET", "/m/shop/cart", &[("Cookie", cookie)], "");
    assert!(cart.body.contains("장바구니가 비었습니다"), "{}", cart.body);
    let body = letter(data, "주문 접수");
    assert!(body.contains("무지 티셔츠 (M) × 2 = 24,000원"), "{body}");
    assert!(body.contains("합계: 27,000원"), "{body}");
    assert!(body.contains("/m/shop/lookup"));
    let done = http(bind, "GET", &format!("/m/shop/o/{first}"), &[], "");
    assert!(done.body.contains("주문이 접수됐습니다"), "{}", done.body);

    // 마지막 한 점을 두 사람이 주문하고 둘 다 입금: 한 사람만 재고를 받는다.
    let a = add_new(bind, "last", "", "1");
    let b = add_new(bind, "last", "", "1");
    let (pay_a, order_a) = order_by_bank(data, bind, &a);
    let (pay_b, order_b) = order_by_bank(data, bind, &b);
    assert_eq!(
        maru(data, &["pay", "order", "--id", &pay_a])["order"]["amount"],
        60000,
        "5만 원 넘으면 배송비 무료"
    );
    std::thread::scope(|s| {
        s.spawn(|| maru(data, &["pay", "mark_paid", "--id", &pay_a]));
        s.spawn(|| maru(data, &["pay", "mark_paid", "--id", &pay_b]));
    });
    maru(data, &["module", "deliver"]);
    maru(data, &["module", "deliver"]);
    let (oa, ob) = (order(data, &order_a), order(data, &order_b));
    assert_eq!(
        (oa["status"].as_str(), ob["status"].as_str()),
        (Some("paid"), Some("paid"))
    );
    let mut shorts = [
        oa["short"].as_bool().unwrap(),
        ob["short"].as_bool().unwrap(),
    ];
    shorts.sort_unstable();
    assert_eq!(shorts, [false, true], "한 사람만 재고를 받는다: {oa} {ob}");
    assert_eq!(stock(data, "last"), 0);
    let short_id = if ob["short"] == true {
        &order_b
    } else {
        &order_a
    };
    let signals = maru(data, &["signals"]).to_string();
    assert!(
        signals.contains("shop_refund"),
        "재고 모자람 환불 신호: {signals}"
    );
    assert!(signals.contains("shop_to_ship"), "{signals}");
    assert!(letter(data, &format!("주문 접수: {short_id}")).contains("재고가 모자라게"));
    let blocked = run(
        data,
        &[
            "shop",
            "set_status",
            "--id",
            short_id,
            "--status",
            "shipped",
            "--carrier",
            "시험택배",
            "--tracking",
            "0000-1111",
        ],
    );
    assert_eq!(
        blocked["ok"], false,
        "재고 모자란 주문은 보내지 못한다: {blocked}"
    );

    // 품절이면 담지도 주문하지도 못한다.
    let c = add_new(bind, "free", "", "1");
    let late = add(bind, &c, "last", "", "1");
    assert_eq!(late.status, 409, "{}", late.body);
    assert!(late.body.contains("품절"), "{}", late.body);

    // 배송 상태와 편지. 앞으로만 간다.
    let bad = run(
        data,
        &[
            "shop",
            "set_status",
            "--id",
            &first,
            "--status",
            "shipped",
            "--carrier",
            "시험택배",
            "--tracking",
            "x",
        ],
    );
    assert_eq!(bad["ok"], false, "운송장 번호 모양: {bad}");
    let back = run(
        data,
        &[
            "shop",
            "set_status",
            "--id",
            &first,
            "--status",
            "delivered",
        ],
    );
    assert_eq!(back["ok"], false, "보내기 전에 배송 완료 없음: {back}");
    maru(
        data,
        &[
            "shop",
            "set_status",
            "--id",
            &first,
            "--status",
            "preparing",
        ],
    );
    let shipped = maru(
        data,
        &[
            "shop",
            "set_status",
            "--id",
            &first,
            "--status",
            "shipped",
            "--carrier",
            "시험택배",
            "--tracking",
            "0000-1111-2222",
        ],
    );
    assert_eq!(shipped["changed"], true);
    let body = letter(data, "상품 발송");
    assert!(body.contains("운송장 번호: 0000-1111-2222"), "{body}");
    assert_eq!(
        maru(
            data,
            &[
                "shop",
                "set_status",
                "--id",
                &first,
                "--status",
                "delivered"
            ]
        )["changed"],
        true
    );
    assert_eq!(
        maru(
            data,
            &[
                "shop",
                "set_status",
                "--id",
                &first,
                "--status",
                "delivered"
            ]
        )["changed"],
        false,
        "같은 상태는 다시 바꾸지 않는다"
    );
    assert!(letter(data, "배송 완료").contains("배송이 끝났습니다"));
    let late_cancel = run(
        data,
        &["shop", "cancel", "--id", &first, "--reason", "늦은 취소"],
    );
    assert_eq!(
        late_cancel["ok"], false,
        "배송 끝난 주문은 취소하지 않는다: {late_cancel}"
    );

    // 운영자 취소: 사유 필수, 재고 복구, 환불 안내.
    let winner = if ob["short"] == true {
        &order_a
    } else {
        &order_b
    };
    assert_eq!(
        run(data, &["shop", "cancel", "--id", winner, "--reason", ""])["ok"],
        false
    );
    let canceled = maru(
        data,
        &["shop", "cancel", "--id", winner, "--reason", "상품 흠"],
    );
    assert_eq!(canceled["refund_needed"], true);
    assert_eq!(stock(data, "last"), 1, "취소하면 재고가 돌아온다");
    let body = letter(data, "주문 취소 안내");
    assert!(
        body.contains("사유: 상품 흠") && body.contains("돌려드립니다"),
        "{body}"
    );

    // 주문 조회: 번호와 전자우편이 맞아야.
    let found = http(
        bind,
        "POST",
        "/m/shop/lookup",
        &[FORM, SAME],
        &form(&[("id", &first), ("email", "buyer@EXAMPLE.test")]),
    );
    assert_eq!(found.status, 200, "{}", found.body);
    assert!(found.body.contains("시험시 시험로 1") && found.body.contains("0000-1111-2222"));
    let wrong = http(
        bind,
        "POST",
        "/m/shop/lookup",
        &[FORM, SAME],
        &form(&[("id", &first), ("email", "other@example.test")]),
    );
    assert_eq!(wrong.status, 404);
    assert!(!wrong.body.contains("시험시 시험로 1"));

    // 관리 화면: 손님은 로그인으로, 운영자는 주문 목록, 위조 폼 거절, 폼으로 상품 저장.
    assert_eq!(http(bind, "GET", "/m/shop/admin", &[], "").status, 303);
    let owner = owner_cookie(data, bind);
    let admin = http(
        bind,
        "GET",
        "/m/shop/admin",
        &[("Cookie", owner.as_str())],
        "",
    );
    assert_eq!(admin.status, 200, "{}", admin.body);
    assert!(
        admin.body.contains("쇼핑시험") && admin.body.contains("재고 모자람"),
        "{}",
        admin.body
    );
    assert!(admin.body.contains("결제 관리에서 환불하기"));
    let forged = http(
        bind,
        "POST",
        "/m/shop/admin/product",
        &[FORM, CROSS, ("Cookie", owner.as_str())],
        &form(&[("id", "evil"), ("name", "x"), ("price", "1")]),
    );
    assert_eq!(forged.status, 403);
    let saved = http(
        bind,
        "POST",
        "/m/shop/admin/product",
        &[FORM, SAME, ("Cookie", owner.as_str())],
        &form(&[
            ("id", "mug"),
            ("name", "머그잔"),
            ("price", "8000"),
            ("stock", ""),
            ("enabled", "1"),
            ("position", "3"),
        ]),
    );
    assert_eq!(saved.status, 303, "{}", saved.body);
    assert!(stock(data, "mug").is_null(), "빈 재고는 무제한");

    // 감사 기록에 구매자 정보가 없다.
    let diffs = db(data, async |db| {
        sqlx::query_as::<_, (String,)>("SELECT diff FROM audit_log WHERE action LIKE 'shop.%'")
            .fetch_all(db.read_pool())
            .await
            .unwrap()
    });
    assert!(!diffs.is_empty());
    for (d,) in diffs {
        assert!(
            !d.contains("buyer@example.test")
                && !d.contains("01000000000")
                && !d.contains("시험로"),
            "{d}"
        );
    }
}
