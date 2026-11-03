//! GAP C10: 여분 필드 (ADR-0057). 실제 바이너리·실제 포트·실제 DB 파일로 CLI 칸 정의 → CLI `--field` 값이
//! 타입대로 DB `posts.meta` 에 남고, 공개 글 화면 표·공개 글쓰기 칸에서 같은 규칙으로 검사되는지.
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

fn start_server(data: &Path) -> (Server, Value) {
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
    (server, v["data"].clone())
}

struct Res {
    status: u16,
    head: String,
    body: String,
}

impl Res {
    fn header(&self, name: &str) -> Option<String> {
        let name = name.to_ascii_lowercase();
        self.head.lines().find_map(|l| {
            let (k, v) = l.split_once(':')?;
            (k.trim().to_ascii_lowercase() == name).then(|| v.trim().to_owned())
        })
    }

    /// `Set-Cookie` 에서 이름에 맞는 값.
    fn cookie(&self, name: &str) -> Option<String> {
        self.head.lines().find_map(|l| {
            let (k, v) = l.split_once(':')?;
            if !k.trim().eq_ignore_ascii_case("set-cookie") {
                return None;
            }
            let first = v.trim().split(';').next()?;
            let (k, v) = first.split_once('=')?;
            (k == name && !v.is_empty()).then(|| v.to_owned())
        })
    }

    /// 화면의 첫 `_csrf` 숨은 필드 값.
    fn csrf(&self) -> String {
        let at = self
            .body
            .find("name=\"_csrf\" value=\"")
            .expect("_csrf 필드");
        let rest = &self.body[at + 20..];
        rest[..rest.find('"').unwrap()].to_owned()
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
    let text = String::from_utf8(raw).unwrap();
    let (head, body) = text.split_once("\r\n\r\n").unwrap_or((&text, ""));
    Res {
        status: head[9..12].parse().unwrap(),
        head: head.to_owned(),
        body: body.to_owned(),
    }
}

const FORM: (&str, &str) = ("Content-Type", "application/x-www-form-urlencoded");

fn form(pairs: &[(&str, &str)]) -> String {
    pairs
        .iter()
        .map(|(k, v)| format!("{}={}", enc(k), enc(v)))
        .collect::<Vec<_>>()
        .join("&")
}

fn enc(s: &str) -> String {
    s.bytes()
        .map(|b| {
            if b.is_ascii_alphanumeric() || b"-_.~".contains(&b) {
                (b as char).to_string()
            } else {
                format!("%{b:02X}")
            }
        })
        .collect()
}

const SAME: (&str, &str) = ("Sec-Fetch-Site", "same-origin");

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

fn meta_of(data: &Path, id: &str) -> Value {
    let id = id.to_owned();
    let raw: String = db(data, async move |db| {
        sqlx::query_scalar("SELECT meta FROM posts WHERE id = ?")
            .bind(id)
            .fetch_one(db.read_pool())
            .await
            .unwrap()
    });
    serde_json::from_str(&raw).unwrap()
}

fn newest_id(data: &Path) -> String {
    db(data, async |db| {
        sqlx::query_scalar("SELECT id FROM posts ORDER BY created_at DESC, id DESC LIMIT 1")
            .fetch_one(db.read_pool())
            .await
            .unwrap()
    })
}

const FIELDS: &str = r#"[
  {"key": "price", "label": "가격", "type": "number", "required": true, "help": "원 단위"},
  {"key": "area", "label": "지역", "type": "select", "options": [{"value": "seoul", "label": "서울"}, {"value": "busan", "label": "부산"}]}
]"#;

#[test]
fn board_fields_flow_from_cli_to_db_and_public_screens() {
    let tmp = tempfile::tempdir().unwrap();
    let data = tmp.path().join("사이트");
    maru(&data, &["init", "--preset", "blog", "--yes"]);
    let file = tmp.path().join("fields.json");
    std::fs::write(&file, FIELDS).unwrap();
    let file = file.to_str().unwrap();
    maru(
        &data,
        &[
            "board", "create", "market", "--name", "장터", "--kind", "free", "--fields", file,
        ],
    );
    let b = maru(
        &data,
        &[
            "board",
            "update",
            "market",
            "--write",
            "guest",
            "--write-approval",
            "off",
        ],
    );
    assert_eq!(b["field_schema"][0]["key"], "price");
    assert_eq!(b["field_schema"][1]["options"][1]["label"], "부산");
    maru(&data, &["site", "set", "--captcha", "off"]);

    // 잘못된 정의는 원인과 함께 거부
    let bad = tmp.path().join("bad.json");
    std::fs::write(&bad, r#"[{"key":"Price","label":"가격","type":"number"}]"#).unwrap();
    let v = run(
        &data,
        &[
            "board",
            "update",
            "market",
            "--fields",
            bad.to_str().unwrap(),
        ],
    );
    assert_eq!(v["error"]["code"], "VALIDATION");

    // CLI 글: --field 값이 타입대로 DB 에 남는다.
    let p = maru(
        &data,
        &[
            "post",
            "new",
            "--board",
            "market",
            "--title",
            "자전거 팝니다",
            "--slug",
            "bike",
            "--field",
            "price=120,000",
            "--field",
            "area=busan",
            "--publish",
        ],
    );
    let id = p["id"].as_str().unwrap();
    assert_eq!(
        meta_of(&data, id),
        serde_json::json!({"price": 120000, "area": "busan"})
    );
    let v = run(
        &data,
        &[
            "post",
            "new",
            "--board",
            "market",
            "--title",
            "값 틀림",
            "--field",
            "price=열",
        ],
    );
    assert_eq!(v["error"]["code"], "VALIDATION");
    assert_eq!(
        v["error"]["message"],
        "가격 칸 값이 올바르지 않습니다 (숫자)."
    );
    // 고치기: 준 칸만 바꾸고, 빈 값은 지운다
    maru(&data, &["post", "edit", id, "--field", "area="]);
    assert_eq!(meta_of(&data, id), serde_json::json!({"price": 120000}));
    maru(&data, &["post", "edit", id, "--field", "area=seoul"]);

    let (_server, started) = start_server(&data);
    let bind = started["bind"].as_str().unwrap().to_owned();
    let page = http(&bind, "GET", "/b/market/bike", &[], "");
    assert_eq!(page.status, 200);
    assert!(
        page.body.contains("<dt>가격</dt><dd>120,000</dd>"),
        "가격 표"
    );
    assert!(
        page.body.contains("<dt>지역</dt><dd>서울</dd>"),
        "선택지 이름"
    );

    // 공개 글쓰기 폼에 칸이 있고, 손님 글도 같은 규칙으로 검사한다.
    let res = http(&bind, "GET", "/write/market", &[], "");
    assert_eq!(res.status, 200);
    assert!(res.body.contains("name=\"field.price\""));
    assert!(res.body.contains("원 단위"), "도움말");
    let pre = res.cookie("maru_csrf").unwrap();
    let cookie = format!("maru_csrf={pre}");
    let csrf = res.csrf();
    let base = [
        ("_csrf", csrf.as_str()),
        ("title", "책상 팝니다"),
        ("body", "튼튼합니다"),
        ("name", "손님"),
        ("password", "test-pass-1"),
        ("field._shown", "1"),
        ("field.area", "busan"),
    ];
    let res = http(
        &bind,
        "POST",
        "/write/market",
        &[FORM, SAME, ("Cookie", &cookie)],
        &form(&base),
    );
    assert_eq!(res.status, 400);
    assert!(res.body.contains("가격 칸을 채워 주세요."));
    assert!(
        res.body.contains("value=\"busan\" selected"),
        "보낸 값을 다시 채운다"
    );
    let mut with_price = base.to_vec();
    with_price.push(("field.price", "30000"));
    let res = http(
        &bind,
        "POST",
        "/write/market",
        &[FORM, SAME, ("Cookie", &cookie)],
        &form(&with_price),
    );
    assert_eq!(res.status, 303, "{}", res.body);
    assert!(
        res.header("location").unwrap().starts_with("/b/market/"),
        "바로 공개"
    );
    let id = newest_id(&data);
    assert_eq!(
        meta_of(&data, &id),
        serde_json::json!({"price": 30000, "area": "busan"})
    );
}
