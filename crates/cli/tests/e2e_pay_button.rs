//! 결제 모듈 2단계 (ADR-0167). 실제 바이너리·실제 포트·실제 DB 파일로
//! 사업자 정보(검사·바닥글·운영 키 잠금), 약관 틀, 결제 단추 위젯(정한 금액·구매자가 적는 금액·동의·위조 폼),
//! 결제 관리 화면(로그인·입금 확인·환불·결제 요청 링크·결제 방법 저장), 결제 확인 편지를 본다.
#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::path::Path;
use std::process::{Child, Command, Stdio};

use serde_json::Value;

const BIN: &str = env!("CARGO_BIN_EXE_giwa");
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
    let link = giwa(data, &["login-link"]);
    let rest = link["url"].as_str().unwrap().split_once("://").unwrap().1;
    let path = &rest[rest.find('/').unwrap()..];
    let first = http(bind, "GET", path, &[], "");
    let pre = first.cookie("giwa_csrf").unwrap();
    let cookie = format!("giwa_csrf={pre}");
    let res = http(
        bind,
        "POST",
        path,
        &[FORM, ("Cookie", cookie.as_str())],
        &form(&[("_csrf", &pre)]),
    );
    let session = res
        .cookie("giwa_session")
        .unwrap_or_else(|| panic!("로그인 실패: {}", res.body));
    format!("giwa_session={session}")
}

/// `/m/pay/o/<id>?k=<값>` 에서 (주문, 1회용 값).
fn order_of(location: &str) -> (String, String) {
    let path = &location[location.find("/m/pay/o/").unwrap() + 9..];
    let (id, k) = path.split_once("?k=").unwrap();
    (id.to_owned(), k.to_owned())
}

#[test]
fn business_info_button_admin_and_letters() {
    let tmp = tempfile::tempdir().unwrap();
    let data = tmp.path().join("사이트");
    let data = data.as_path();
    giwa(data, &["init", "--no-samples", "--preset", "blog", "--yes"]);
    giwa(data, &["module", "enable", "pay"]);

    // 에이전트는 공통 칸을 명령 뒤에 붙이기 쉽다. 모듈 툴도 --data 를 어디서나 읽는다.
    let out = Command::new(BIN)
        .args(["pay", "provider_list", "--json", "--data"])
        .arg(data)
        .current_dir(tmp.path())
        .env_remove("GIWA_DATA")
        .output()
        .unwrap();
    let v: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v["ok"], true, "{v}");
    assert!(v["data"]["providers"].is_array(), "{v}");

    // 운영 키는 사업자 정보가 비어 있으면 받지 않고, 빈 칸과 다음 명령을 알려 준다.
    let live = [
        "pay",
        "provider_set",
        "--provider",
        "toss",
        "--client-key",
        "live_gck_x1",
        "--secret-key",
        "live_gsk_x2",
    ];
    let v = run(data, &live);
    assert_eq!(v["ok"], false, "{v}");
    assert!(v.to_string().contains("사업자 정보"), "{v}");
    assert!(v.to_string().contains("--business-reg-no"), "{v}");

    // 사업자 정보: 등록번호 모양 검사와 정리.
    let bad = run(data, &["site", "set", "--business-reg-no", "12345"]);
    assert_eq!(bad["ok"], false, "{bad}");
    assert!(bad.to_string().contains("사업자등록번호"), "{bad}");
    let s = giwa(
        data,
        &[
            "site",
            "set",
            "--business-name",
            "시험상점",
            "--business-owner",
            "대표자시험",
            "--business-address",
            "시험시 시험구 시험로 1",
            "--business-phone",
            "02-000-0000",
            "--business-email",
            "shop@example.test",
            "--business-reg-no",
            "1234567890",
            "--business-hosting",
            "직접 운영",
        ],
    );
    assert_eq!(s["business"]["reg_no"], "123-45-67890", "{s}");
    // 사업자 정보가 다 차도 사이트 주소가 https 가 아니면 운영 키는 받지 않는다.
    let v = run(data, &live);
    assert_eq!(v["ok"], false, "{v}");
    assert!(v.to_string().contains("https"), "{v}");

    // 약관 틀: 글로 된 보통 페이지가 초안으로 생긴다.
    let page = giwa(
        data,
        &[
            "page",
            "new",
            "terms",
            "--title",
            "이용약관",
            "--starter",
            "terms",
        ],
    );
    assert_eq!(page["layout"], "default", "{page}");
    assert!(
        page["body_md"].as_str().unwrap().contains("제1조"),
        "{page}"
    );
    let refund_page = giwa(
        data,
        &[
            "page",
            "new",
            "refund",
            "--title",
            "환불 규정",
            "--starter",
            "refund",
        ],
    );
    assert!(
        refund_page["body_md"]
            .as_str()
            .unwrap()
            .contains("청약철회")
    );

    giwa(
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
    // 결제 단추 두 개: 정한 금액, 구매자가 적는 금액(최소·최대).
    let fixed = giwa(
        data,
        &[
            "widget",
            "place",
            "pay_button",
            "--region",
            "main",
            "--scope",
            "home",
            "--config",
            r#"{"key":"consult","name":"첫 상담 30분","amount":30000,"ask_phone":true,"policy":"/refund","return_to":"/","anchor":"pay"}"#,
        ],
    );
    assert_eq!(fixed["config"]["key"], "consult");
    let custom = giwa(
        data,
        &[
            "widget",
            "place",
            "pay_button",
            "--region",
            "main",
            "--scope",
            "home",
            "--config",
            r#"{"name":"후원","amount":0,"min":1000,"max":50000,"label":"후원하기"}"#,
        ],
    );
    let custom_key = custom["config"]["key"].as_str().unwrap().to_owned();
    assert!(!custom_key.is_empty(), "빈 이름은 서버가 채운다: {custom}");
    let bad = run(
        data,
        &[
            "widget",
            "place",
            "pay_button",
            "--region",
            "main",
            "--config",
            r#"{"name":"x","amount":0,"min":5000,"max":100}"#,
        ],
    );
    assert_eq!(bad["ok"], false, "최소가 최대보다 크면 거절: {bad}");

    let (_server, bind) = start_server(data);
    let bind = bind.as_str();

    // 첫 화면: 바닥글의 사업자 정보와 단추 폼.
    let home = http(bind, "GET", "/", &[], "");
    assert_eq!(home.status, 200);
    assert!(home.body.contains(r#"class="biz-info""#), "{}", home.body);
    assert!(home.body.contains("123-45-67890"));
    assert!(home.body.contains("시험상점"));
    assert!(home.body.contains(r#"action="/m/pay/button/consult""#));
    // 구역 이름을 주면 첫 인상 단추가 /#s-pay 로 그 자리에 내려온다.
    assert!(home.body.contains(r#"id="s-pay""#), "{}", home.body);
    assert!(home.body.contains("30,000원"));
    assert!(home.body.contains(r#"href="/refund""#));
    assert!(home.body.contains("후원하기"));

    let buy = |key: &str, pairs: &[(&str, &str)], same: bool| {
        let headers = if same { [FORM, SAME] } else { [FORM, CROSS] };
        http(
            bind,
            "POST",
            &format!("/m/pay/button/{key}"),
            &headers,
            &form(pairs),
        )
    };
    let good = [
        ("buyer_name", "구매자시험"),
        ("buyer_email", "Buyer@Example.test"),
        ("buyer_phone", "010-0000-0000"),
        ("agree", "1"),
    ];
    // 다른 사이트에서 보낸 폼은 받지 않는다.
    assert_eq!(buy("consult", &good, false).status, 403);
    // 동의하지 않으면 다시 확인 화면, 적은 값은 남는다.
    let res = buy("consult", &good[..3], true);
    assert_eq!(res.status, 422, "{}", res.body);
    assert!(res.body.contains("동의"), "{}", res.body);
    assert!(res.body.contains("구매자시험"));
    // 없는 단추.
    assert_eq!(buy("nope", &good, true).status, 404);
    // 자동 프로그램(함정 칸)은 주문을 만들지 않는다.
    let mut trap = good.to_vec();
    trap.push(("website", "x"));
    assert_eq!(buy("consult", &trap, true).status, 303);
    assert_eq!(giwa(data, &["pay", "orders"])["total"], 0);

    // 브라우저가 금액을 보내도 정한 금액을 쓴다.
    let mut forged = good.to_vec();
    forged.push(("amount", "1"));
    let res = buy("consult", &forged, true);
    assert_eq!(res.status, 303, "{}", res.body);
    let location = res.header("location");
    let (id, k) = order_of(&location);
    let o = giwa(data, &["pay", "order", "--id", &id]);
    assert_eq!(o["order"]["amount"], 30000, "{o}");
    assert_eq!(o["order"]["ref_id"], "button:consult");
    assert_eq!(o["buyer"]["email"], "buyer@example.test");
    assert_eq!(o["buyer"]["phone"], "01000000000");
    let page = http(bind, "GET", &location, &[], "");
    assert_eq!(page.status, 200);
    assert!(page.body.contains("첫 상담 30분"));

    // 구매자가 적는 금액: 범위 밖은 거절, 안이면 그 금액.
    let mut low = good.to_vec();
    low.push(("amount", "500"));
    assert_eq!(buy(&custom_key, &low, true).status, 422);
    let mut ok = good.to_vec();
    ok.push(("amount", "12,000"));
    let res = buy(&custom_key, &ok, true);
    assert_eq!(res.status, 303, "{}", res.body);
    let (gift, _) = order_of(&res.header("location"));
    assert_eq!(
        giwa(data, &["pay", "order", "--id", &gift])["order"]["amount"],
        12000
    );

    // 무통장 입금을 고르면 입금 안내 편지가 쌓인다(감사 기록 이벤트).
    let res = http(
        bind,
        "POST",
        &format!("/m/pay/o/{id}/bank"),
        &[FORM, SAME],
        &form(&[("k", &k), ("depositor", "구매자시험")]),
    );
    assert_eq!(res.status, 303, "{}", res.body);
    // 단추 주문은 한 시간 안에 결제 방법을 고르게 하지만, 무통장 입금은 사흘을 기다린다.
    let o = giwa(data, &["pay", "order", "--id", &id]);
    let rfc = &time::format_description::well_known::Rfc3339;
    let expires =
        time::OffsetDateTime::parse(o["order"]["expires_at"].as_str().unwrap(), rfc).unwrap();
    assert!(
        expires > time::OffsetDateTime::now_utc() + time::Duration::days(2),
        "{o}"
    );
    giwa(data, &["module", "deliver"]);
    let m = mails(data);
    assert_eq!(m.len(), 1, "{m:?}");
    assert_eq!(m[0].0, "buyer@example.test");
    assert!(m[0].1.contains("입금 안내"), "{m:?}");
    assert!(m[0].2.contains("시험은행 000-000-000000"), "{m:?}");
    assert!(m[0].2.contains(&id), "{m:?}");
    assert!(m[0].2.contains("123-45-67890"), "판매자 정보: {m:?}");
    // 다시 배달해도 한 번만.
    giwa(data, &["module", "deliver"]);
    assert_eq!(mails(data).len(), 1);

    // 결제 관리 화면: 손님은 로그인으로, 운영자는 본다.
    let res = http(bind, "GET", "/m/pay", &[], "");
    assert_eq!(res.status, 303);
    assert!(res.header("location").starts_with("/login"));
    let cookie = owner_cookie(data, bind);
    let admin = http(bind, "GET", "/m/pay", &[("Cookie", &cookie)], "");
    assert_eq!(admin.status, 200, "{}", admin.body);
    assert!(admin.body.contains("구매자시험"));
    assert!(admin.body.contains(&format!("/m/pay/admin/{id}/paid")));
    assert!(!admin.body.contains("사업자 정보가 비어 있습니다"));
    let auth = [FORM, SAME, ("Cookie", cookie.as_str())];

    // 위조 폼(다른 사이트)은 운영자 쿠키가 있어도 거절.
    let res = http(
        bind,
        "POST",
        &format!("/m/pay/admin/{id}/paid"),
        &[FORM, CROSS, ("Cookie", cookie.as_str())],
        "",
    );
    assert_eq!(res.status, 403);
    let res = http(bind, "POST", &format!("/m/pay/admin/{id}/paid"), &auth, "");
    assert_eq!(res.status, 303, "{}", res.body);
    assert_eq!(
        giwa(data, &["pay", "order", "--id", &id])["order"]["status"],
        "paid"
    );
    // 부분 환불(사유 없으면 거절), 그다음 남은 금액 전부.
    let res = http(
        bind,
        "POST",
        &format!("/m/pay/admin/{id}/refund"),
        &auth,
        &form(&[("amount", "10000"), ("reason", "")]),
    );
    assert_eq!(res.status, 422, "{}", res.body);
    let res = http(
        bind,
        "POST",
        &format!("/m/pay/admin/{id}/refund"),
        &auth,
        &form(&[("amount", "10,000"), ("reason", "일정 변경")]),
    );
    assert_eq!(res.status, 303, "{}", res.body);
    let o = giwa(data, &["pay", "order", "--id", &id]);
    assert_eq!(o["order"]["status"], "partial_refunded", "{o}");
    assert_eq!(o["order"]["refunded"], 10000);
    giwa(data, &["module", "deliver"]);
    let m = mails(data);
    let subjects: Vec<&str> = m.iter().map(|x| x.1.as_str()).collect();
    assert!(
        subjects.iter().any(|s| s.contains("결제 완료")),
        "{subjects:?}"
    );
    assert!(
        subjects.iter().any(|s| s.contains("환불 안내")),
        "{subjects:?}"
    );
    assert!(
        m.iter().any(|x| x.2.contains("10,000원을 환불했습니다")),
        "{m:?}"
    );

    // 결제 요청 링크를 화면에서 만들면 주소를 한 번 보여 준다.
    let res = http(
        bind,
        "POST",
        "/m/pay/admin/new",
        &auth,
        &form(&[("name", "추가 상담"), ("amount", "50,000")]),
    );
    assert_eq!(res.status, 200, "{}", res.body);
    assert!(res.body.contains("/m/pay/o/"), "{}", res.body);
    assert!(res.body.contains("50,000원"));

    // 결제 방법 저장: 시크릿 키를 비우면 그대로 두고, 화면에는 끝 네 글자만.
    let res = http(
        bind,
        "POST",
        "/m/pay/admin/provider/toss",
        &auth,
        &form(&[
            ("client_key", "test_gck_screen1"),
            ("secret_key", "test_gsk_screen7777"),
            ("enabled", "1"),
        ]),
    );
    assert_eq!(res.status, 303, "{}", res.body);
    let res = http(
        bind,
        "POST",
        "/m/pay/admin/provider/toss",
        &auth,
        &form(&[("client_key", "test_gck_screen1"), ("enabled", "1")]),
    );
    assert_eq!(res.status, 303, "{}", res.body);
    let admin = http(bind, "GET", "/m/pay", &[("Cookie", &cookie)], "");
    assert!(admin.body.contains("…7777"));
    assert!(!admin.body.contains("test_gsk_screen7777"));
    let listed = giwa(data, &["pay", "provider_list"]);
    assert!(listed.to_string().contains("…7777"), "{listed}");

    // 편지에도 감사 기록에도 시크릿 키는 없다.
    let leaked = db(data, async |db| {
        sqlx::query_scalar::<_, i64>(
            "SELECT COUNT(*) FROM audit_log WHERE diff LIKE '%screen7777%' OR diff LIKE '%buyer@example%'",
        )
        .fetch_one(db.read_pool())
        .await
        .unwrap()
    });
    assert_eq!(leaked, 0);
}
