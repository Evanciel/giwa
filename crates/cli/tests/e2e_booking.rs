//! 예약 모듈 (ADR-0169). 실제 바이너리·실제 포트·실제 DB 파일로
//! 칸(영업 시간·막은 시간), 정원 경쟁(동시 두 요청), 1회용 취소 링크와 마감, 예약금(무통장 입금 확인 뒤 확정),
//! 15분 지난 자리 풀림, 위조 폼·함정 칸, 관리 화면, 감사 기록에 손님 정보 없음을 본다.
#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::time::{SystemTime, UNIX_EPOCH};

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

/// 고정 문장에 ID 하나를 묶어 돌린다.
fn sql(data: &Path, q: &'static str, id: &str) {
    let id = id.to_owned();
    db(data, async move |db| {
        sqlx::query(q)
            .bind(id)
            .execute(db.write_pool())
            .await
            .unwrap();
    });
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

/// 제목에 `subject` 가 든 마지막 편지의 취소 링크 경로 (`/m/booking/c/<id>?k=…`).
fn cancel_link(data: &Path, subject: &str) -> String {
    let (_, _, body) = mails(data)
        .into_iter()
        .rev()
        .find(|(_, s, _)| s.contains(subject))
        .unwrap_or_else(|| panic!("편지 없음: {subject}"));
    let line = body
        .lines()
        .find(|l| l.contains("/m/booking/c/"))
        .unwrap_or_else(|| panic!("취소 링크 없음: {body}"));
    line[line.find("/m/booking/c/").unwrap()..]
        .trim()
        .to_owned()
}

/// 한국 시간 오늘부터 `plus` 날 뒤 `YYYY-MM-DD`.
fn kst_day(plus: i64) -> String {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64;
    let z = (secs + 9 * 3600).div_euclid(86_400) + plus + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!("{y:04}-{m:02}-{d:02}")
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

fn starts(slots: &Value) -> Vec<String> {
    slots["slots"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| format!("{}:{}", s["start"].as_str().unwrap(), s["left"]))
        .collect()
}

#[test]
fn slots_race_cancel_link_deposit_and_release() {
    let tmp = tempfile::tempdir().unwrap();
    let data = tmp.path().join("사이트");
    let data = data.as_path();
    giwa(data, &["init", "--no-samples", "--preset", "blog", "--yes"]);
    giwa(data, &["module", "enable", "booking"]);

    // 매일 10:00~12:00, 30분짜리 정원 1 상담과 60분짜리 예약금 진료.
    let hours = giwa(
        data,
        &[
            "booking",
            "set_hours",
            "--days",
            "all",
            "--open",
            "10:00",
            "--close",
            "12:00",
        ],
    );
    assert_eq!(hours["hours"][0]["open"], "10:00");
    let bad = run(
        data,
        &[
            "booking",
            "set_hours",
            "--days",
            "weekdays",
            "--open",
            "12:00",
            "--close",
            "10:00",
        ],
    );
    assert_eq!(bad["ok"], false, "닫는 시간이 먼저면 거절: {bad}");
    let made = giwa(
        data,
        &[
            "booking",
            "service_set",
            "--id",
            "consult",
            "--name",
            "첫 상담",
            "--minutes",
            "30",
            "--price",
            "30000",
        ],
    );
    assert_eq!(made["created"], true);
    assert_eq!(made["service"]["capacity"], 1);
    giwa(
        data,
        &[
            "booking",
            "service_set",
            "--id",
            "visit",
            "--name",
            "진료",
            "--minutes",
            "60",
            "--deposit",
            "10000",
        ],
    );
    let nameless = run(
        data,
        &["booking", "service_set", "--id", "x", "--minutes", "30"],
    );
    assert_eq!(
        nameless["ok"], false,
        "새 서비스는 이름이 있어야 한다: {nameless}"
    );

    let day = kst_day(7);
    let next = kst_day(8);
    let slots = giwa(
        data,
        &["booking", "slots", "--service", "consult", "--day", &day],
    );
    assert_eq!(starts(&slots), ["10:00:1", "10:30:1", "11:00:1", "11:30:1"]);
    // 막은 시간과 하루 쉬는 날.
    let blocked = giwa(
        data,
        &[
            "booking", "block", "--day", &day, "--start", "11:00", "--end", "11:30", "--reason",
            "점심",
        ],
    );
    assert_eq!(blocked["existing_bookings"], 0);
    let slots = giwa(
        data,
        &["booking", "slots", "--service", "consult", "--day", &day],
    );
    assert_eq!(starts(&slots), ["10:00:1", "10:30:1", "11:30:1"]);
    giwa(data, &["booking", "block", "--day", &next]);
    let closed = giwa(
        data,
        &["booking", "slots", "--service", "consult", "--day", &next],
    );
    assert_eq!(closed["slots"].as_array().unwrap().len(), 0);
    assert_eq!(closed["reason"], "쉬는 날입니다.");
    let past = giwa(
        data,
        &[
            "booking",
            "slots",
            "--service",
            "consult",
            "--day",
            &kst_day(-1),
        ],
    );
    assert_eq!(past["reason"], "지난 날짜입니다.");

    giwa(
        data,
        &[
            "widget",
            "place",
            "booking_form",
            "--region",
            "main",
            "--scope",
            "home",
            "--config",
            r#"{"service":"consult","anchor":"booking","note":"편한 시간을 고르세요"}"#,
        ],
    );
    let missing = run(
        data,
        &[
            "widget",
            "place",
            "booking_form",
            "--region",
            "main",
            "--config",
            r#"{"service":"nope"}"#,
        ],
    );
    assert_eq!(missing["ok"], false, "없는 서비스는 거절: {missing}");

    let (_server, bind) = start_server(data);
    let bind = bind.as_str();
    let home = http(bind, "GET", "/", &[], "");
    assert!(home.body.contains(r#"id="s-booking""#), "{}", home.body);
    assert!(home.body.contains(r#"action="/m/booking/new""#));
    assert!(home.body.contains("첫 상담"));

    let page = http(
        bind,
        "GET",
        &format!("/m/booking/new?service=consult&day={day}"),
        &[],
        "",
    );
    assert_eq!(page.status, 200);
    assert!(page.body.contains(r#"value="10:30""#), "{}", page.body);
    assert!(!page.body.contains(r#"value="11:00""#), "막은 칸은 없다");

    let guest = |start: &str| {
        vec![
            ("service", "consult".to_owned()),
            ("day", day.clone()),
            ("start", start.to_owned()),
            ("name", "예약시험".to_owned()),
            ("email", "Guest@Example.test".to_owned()),
            ("phone", "010-0000-0000".to_owned()),
            ("memo", "처음 갑니다".to_owned()),
            ("agree", "1".to_owned()),
        ]
    };
    let post = |pairs: &[(&str, String)], same: bool| {
        let pairs: Vec<(&str, &str)> = pairs.iter().map(|(k, v)| (*k, v.as_str())).collect();
        let headers = if same { [FORM, SAME] } else { [FORM, CROSS] };
        http(bind, "POST", "/m/booking/new", &headers, &form(&pairs))
    };
    // 다른 사이트의 폼, 동의 없음, 함정 칸.
    assert_eq!(post(&guest("10:00"), false).status, 403);
    let no_agree: Vec<_> = guest("10:00")
        .into_iter()
        .filter(|(k, _)| *k != "agree")
        .collect();
    let res = post(&no_agree, true);
    assert_eq!(res.status, 422, "{}", res.body);
    assert!(res.body.contains("예약시험"), "적은 값이 남는다");
    let mut trap = guest("10:00");
    trap.push(("website", "x".into()));
    assert_eq!(post(&trap, true).status, 303);
    assert_eq!(
        giwa(data, &["booking", "list", "--day", &day])["items"]
            .as_array()
            .unwrap()
            .len(),
        0
    );
    // 막은 칸·없는 칸은 받지 않는다.
    assert_eq!(post(&guest("11:00"), true).status, 422);

    // 정원 1 칸에 동시에 두 사람.
    let mut codes: Vec<u16> = std::thread::scope(|s| {
        let a = s.spawn(|| post(&guest("10:00"), true).status);
        let b = s.spawn(|| post(&guest("10:00"), true).status);
        vec![a.join().unwrap(), b.join().unwrap()]
    });
    codes.sort_unstable();
    assert_eq!(codes, [303, 409], "한 사람만 들어간다");
    let list = giwa(data, &["booking", "list", "--day", &day]);
    let items = list["items"].as_array().unwrap();
    assert_eq!(items.len(), 1);
    assert_eq!(list["untrusted"][0], "guest");
    let first = items[0]["id"].as_str().unwrap().to_owned();
    assert_eq!(items[0]["status"], "requested");
    assert_eq!(items[0]["guest"]["email"], "guest@example.test");
    assert_eq!(items[0]["guest"]["phone"], "01000000000");
    let done = http(bind, "GET", &format!("/m/booking/done/{first}"), &[], "");
    assert_eq!(done.status, 200);
    assert!(done.body.contains("예약 신청을 받았습니다"));
    assert!(
        !done.body.contains("guest@example.test"),
        "접수 화면에 손님 정보 없음"
    );

    // 접수 편지의 취소 링크. 확정하면 새 링크가 가고 옛 링크는 쓸 수 없다.
    let old = cancel_link(data, "예약 신청");
    assert_eq!(http(bind, "GET", &old, &[], "").status, 200);
    let confirmed = giwa(data, &["booking", "confirm", "--id", &first]);
    assert_eq!(confirmed["changed"], true);
    assert_eq!(
        giwa(data, &["booking", "confirm", "--id", &first])["changed"],
        false
    );
    assert_eq!(http(bind, "GET", &old, &[], "").status, 404);
    let link = cancel_link(data, "예약 확정");
    let view = http(bind, "GET", &link, &[], "");
    assert_eq!(view.status, 200);
    assert!(view.body.contains("이 예약을 취소합니다"), "{}", view.body);
    let (path, k) = link.split_once("?k=").unwrap();
    assert_eq!(
        http(bind, "POST", path, &[FORM, CROSS], &form(&[("k", k)])).status,
        403
    );
    let res = http(bind, "POST", path, &[FORM, SAME], &form(&[("k", k)]));
    assert_eq!(res.status, 200, "{}", res.body);
    assert!(res.body.contains("예약을 취소했습니다"));
    // 1회용: 다시 쓰면 없는 예약처럼 보인다.
    assert_eq!(
        http(bind, "POST", path, &[FORM, SAME], &form(&[("k", k)])).status,
        404
    );
    let slots = giwa(
        data,
        &["booking", "slots", "--service", "consult", "--day", &day],
    );
    assert_eq!(starts(&slots)[0], "10:00:1", "취소하면 자리가 돌아온다");
    assert!(mails(data).iter().any(|(_, s, _)| s.contains("예약 취소")));

    // 취소 마감이 지나면 링크로 취소하지 못한다.
    giwa(
        data,
        &["module", "set", "booking", "--field", "cancel_hours=720"],
    );
    assert_eq!(post(&guest("10:30"), true).status, 303);
    let late = cancel_link(data, "예약 신청");
    let view = http(bind, "GET", &late, &[], "");
    assert!(
        view.body.contains("직접 취소할 수 없습니다"),
        "{}",
        view.body
    );
    let (path, k) = late.split_once("?k=").unwrap();
    assert_eq!(
        http(bind, "POST", path, &[FORM, SAME], &form(&[("k", k)])).status,
        409
    );
    giwa(
        data,
        &["module", "set", "booking", "--field", "cancel_hours=24"],
    );

    // 예약금: 결제 모듈이 없으면 예약하지 않고 자리를 바로 푼다.
    let visit = |start: &str| {
        let mut g = guest(start);
        g[0].1 = "visit".into();
        g
    };
    let res = post(&visit("10:00"), true);
    assert_eq!(res.status, 409, "{}", res.body);
    assert!(res.body.contains("예약금을 받을 수 없어"), "{}", res.body);
    let slots = giwa(
        data,
        &["booking", "slots", "--service", "visit", "--day", &day],
    );
    assert_eq!(starts(&slots)[0], "10:00:1");

    giwa(data, &["module", "enable", "pay"]);
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
    let res = post(&visit("10:00"), true);
    assert_eq!(res.status, 303, "{}", res.body);
    let location = res.header("location");
    assert!(location.starts_with("/m/pay/o/"), "{location}");
    let held = giwa(
        data,
        &["booking", "list", "--day", &day, "--status", "held"],
    );
    let held = &held["items"][0];
    let bid = held["id"].as_str().unwrap().to_owned();
    let order = held["order_id"].as_str().unwrap().to_owned();
    let o = giwa(data, &["pay", "order", "--id", &order]);
    assert_eq!(o["order"]["amount"], 10000, "예약금은 서비스 설정에서");
    assert_eq!(o["order"]["owner"], "booking");
    let slots = giwa(
        data,
        &["booking", "slots", "--service", "visit", "--day", &day],
    );
    assert_eq!(starts(&slots)[0], "10:00:0", "예약금 대기도 자리를 잡는다");
    // 무통장 입금을 고르면 입금 기한까지 자리를 잡는다.
    let k = location.split_once("?k=").unwrap().1;
    let res = http(
        bind,
        "POST",
        &format!("/m/pay/o/{order}/bank"),
        &[FORM, SAME],
        &form(&[("k", k), ("depositor", "예약시험")]),
    );
    assert_eq!(res.status, 303, "{}", res.body);
    giwa(data, &["module", "deliver"]);
    let b = &giwa(
        data,
        &["booking", "list", "--day", &day, "--status", "held"],
    )["items"][0];
    assert!(
        b["hold_until"].as_str().unwrap()[..10] >= *kst_day(2).as_str(),
        "입금 기한까지: {b}"
    );
    // 입금 확인 → 확정, 확정 편지에 예약금.
    giwa(data, &["pay", "mark_paid", "--id", &order]);
    giwa(data, &["module", "deliver"]);
    giwa(data, &["module", "deliver"]);
    let list = giwa(
        data,
        &["booking", "list", "--day", &day, "--status", "confirmed"],
    );
    let b = list["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|b| b["id"] == bid.as_str())
        .unwrap_or_else(|| panic!("확정 안 됨: {list}"));
    assert_eq!(b["paid"], true);
    let letter = mails(data)
        .into_iter()
        .rev()
        .find(|(_, s, _)| s.contains("예약 확정: 진료"))
        .unwrap();
    assert!(
        letter.2.contains("예약금: 10,000원 (결제함)"),
        "{}",
        letter.2
    );
    assert!(letter.2.contains("/m/booking/c/"));
    let done = http(bind, "GET", &format!("/m/booking/done/{bid}"), &[], "");
    assert!(done.body.contains("예약이 확정됐습니다"));

    // 15분 안에 결제하지 않으면 자리를 푼다.
    let other = kst_day(9);
    let mut later = visit("10:00");
    later[1].1 = other.clone();
    let res = post(&later, true);
    assert_eq!(res.status, 303, "{}", res.body);
    let late = giwa(
        data,
        &["booking", "list", "--day", &other, "--status", "held"],
    );
    let late_id = late["items"][0]["id"].as_str().unwrap().to_owned();
    let late_order = late["items"][0]["order_id"].as_str().unwrap().to_owned();
    sql(
        data,
        "UPDATE mod_booking_booking SET hold_until = '2000-01-01T00:00:00.000000Z' WHERE id = ?",
        &late_id,
    );
    let slots = giwa(
        data,
        &["booking", "slots", "--service", "visit", "--day", &other],
    );
    assert_eq!(starts(&slots)[0], "10:00:1", "{slots}");
    sql(
        data,
        "UPDATE mod_pay_order SET expires_at = '2000-01-01T00:00:00.000000Z' WHERE id = ?",
        &late_order,
    );
    let done = http(bind, "GET", &format!("/m/booking/done/{late_id}"), &[], "");
    assert!(done.body.contains("기한이 지났습니다"), "{}", done.body);
    let expired = giwa(
        data,
        &["booking", "list", "--day", &other, "--status", "expired"],
    );
    assert_eq!(expired["items"][0]["id"], late_id.as_str());

    // 운영자 취소: 예약금 받은 예약이면 환불 안내, 사유 필수.
    let no_reason = run(data, &["booking", "cancel", "--id", &bid, "--reason", ""]);
    assert_eq!(no_reason["ok"], false);
    let canceled = giwa(
        data,
        &["booking", "cancel", "--id", &bid, "--reason", "원장 휴진"],
    );
    assert_eq!(canceled["refund_needed"], true);
    let letter = mails(data)
        .into_iter()
        .rev()
        .find(|(_, s, _)| s.contains("예약 취소 안내"))
        .unwrap();
    assert!(letter.2.contains("사유: 원장 휴진"));
    assert!(letter.2.contains("돌려드립니다"));
    let signals = giwa(data, &["signals"]);
    assert!(
        signals.to_string().contains("booking_refund"),
        "예약금 환불 신호: {signals}"
    );

    // 관리 화면: 손님은 로그인으로, 운영자는 날짜별 목록, 위조 폼 거절.
    let anon = http(bind, "GET", "/m/booking", &[], "");
    assert_eq!(anon.status, 303);
    let cookie = owner_cookie(data, bind);
    let admin = http(
        bind,
        "GET",
        &format!("/m/booking?day={day}"),
        &[("Cookie", cookie.as_str())],
        "",
    );
    assert_eq!(admin.status, 200, "{}", admin.body);
    assert!(admin.body.contains("예약시험"));
    assert!(admin.body.contains("결제 관리에서 환불하기"));
    let forged = http(
        bind,
        "POST",
        "/m/booking/admin/service",
        &[FORM, CROSS, ("Cookie", cookie.as_str())],
        &form(&[("id", "evil"), ("name", "x"), ("minutes", "30")]),
    );
    assert_eq!(forged.status, 403);
    let saved = http(
        bind,
        "POST",
        "/m/booking/admin/hours",
        &[FORM, SAME, ("Cookie", cookie.as_str())],
        &form(&[
            ("open_mon", "09:00"),
            ("close_mon", "18:00"),
            ("closed_sun", "1"),
        ]),
    );
    assert_eq!(saved.status, 303, "{}", saved.body);
    let hours = giwa(data, &["booking", "hours"]);
    assert_eq!(hours["hours"][0]["open"], "09:00");
    assert_eq!(hours["hours"][6]["closed"], true);
    assert_eq!(hours["hours"][1]["closed"], true, "빈 칸은 쉬는 요일");

    // 감사 기록에 손님 정보가 없다.
    let diffs = db(data, async |db| {
        sqlx::query_as::<_, (String,)>("SELECT diff FROM audit_log WHERE action LIKE 'booking.%'")
            .fetch_all(db.read_pool())
            .await
            .unwrap()
    });
    assert!(!diffs.is_empty());
    for (d,) in diffs {
        assert!(
            !d.contains("guest@example.test") && !d.contains("01000000000"),
            "{d}"
        );
    }
}
