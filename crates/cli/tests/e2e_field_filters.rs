//! GAP Q4: 여분 필드 목록 표시와 거르기 (ADR-0089). 실제 바이너리·실제 포트·실제 DB 파일로 핫딜 게시판 칸 정의 →
//! 목록 줄의 판매처·가격·「종료」 → 종료 빼기·판매처 거르기·쪽 넘김 유지·REST 를 확인한다.
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

fn get(bind: &str, path: &str) -> (u16, String) {
    let mut s = TcpStream::connect(bind).unwrap();
    let req = format!("GET {path} HTTP/1.1\r\nHost: {bind}\r\nConnection: close\r\n\r\n");
    s.write_all(req.as_bytes()).unwrap();
    let mut raw = Vec::new();
    s.read_to_end(&mut raw).unwrap();
    let text = String::from_utf8(raw).unwrap();
    let (head, body) = text.split_once("\r\n\r\n").unwrap_or((&text, ""));
    (head[9..12].parse().unwrap(), body.to_owned())
}

const DEAL_FIELDS: &str = r#"[
  {"key": "store", "label": "판매처", "type": "select", "list": true, "filter": true,
   "options": [{"value": "mall_a", "label": "가몰"}, {"value": "mall_b", "label": "나몰"}]},
  {"key": "price", "label": "가격", "type": "number", "list": true},
  {"key": "shipping", "label": "배송", "type": "text", "list": true},
  {"key": "link", "label": "구매 링크", "type": "url"},
  {"key": "ended", "label": "종료", "type": "boolean", "list": true, "filter": true}
]"#;

fn deal(data: &Path, slug: &str, title: &str, store: &str, price: &str, ended: bool) {
    let mut args = vec![
        "post",
        "new",
        "--board",
        "deal",
        "--title",
        title,
        "--slug",
        slug,
        "--publish",
    ];
    let store = format!("store={store}");
    let price = format!("price={price}");
    args.extend([
        "--field",
        &store,
        "--field",
        &price,
        "--field",
        "shipping=무료",
    ]);
    if ended {
        args.extend(["--field", "ended=true"]);
    }
    maru(data, &args);
}

/// 목록 HTML 에서 글 제목(주소 조각)만 뽑는다.
fn listed(html: &str) -> Vec<String> {
    html.match_indices("href=\"/b/deal/")
        .map(|(i, m)| {
            let rest = &html[i + m.len()..];
            rest[..rest.find('"').unwrap()].to_owned()
        })
        .collect()
}

#[test]
fn deal_board_lists_fields_and_filters_by_them() {
    let tmp = tempfile::tempdir().unwrap();
    let data = tmp.path().join("사이트");
    maru(&data, &["init", "--preset", "blog", "--yes"]);
    maru(&data, &["site", "set", "--posts-per-page", "2"]);

    // 거르기는 고르는 칸에만.
    let bad = tmp.path().join("bad.json");
    std::fs::write(
        &bad,
        r#"[{"key": "shipping", "label": "배송", "type": "text", "filter": true}]"#,
    )
    .unwrap();
    let refused = run(
        &data,
        &[
            "board",
            "create",
            "deal",
            "--name",
            "핫딜",
            "--kind",
            "free",
            "--fields",
            bad.to_str().unwrap(),
        ],
    );
    assert_eq!(refused["error"]["code"], "VALIDATION", "{refused}");
    assert!(
        refused["error"]["message"].to_string().contains("거르기"),
        "{refused}"
    );

    let file = tmp.path().join("deal.json");
    std::fs::write(&file, DEAL_FIELDS).unwrap();
    maru(
        &data,
        &[
            "board",
            "create",
            "deal",
            "--name",
            "핫딜",
            "--kind",
            "free",
            "--fields",
            file.to_str().unwrap(),
        ],
    );
    let shown = maru(&data, &["board", "show", "deal"]);
    assert!(shown.to_string().contains("\"filter\":true"), "{shown}");

    deal(&data, "a1", "가몰 이어폰", "mall_a", "12900", false);
    deal(&data, "b1", "나몰 키보드", "mall_b", "45000", true);
    deal(&data, "a2", "가몰 마우스", "mall_a", "9900", true);

    let (_server, info) = start_server(&data);
    let bind = info["bind"].as_str().unwrap();

    // 목록 줄: 판매처 이름·가격·배송, 끝난 글엔 「종료」 표시. 링크 칸은 목록에 없다.
    let (status, page) = get(bind, "/b/deal");
    assert_eq!(status, 200);
    assert_eq!(listed(&page), ["a2", "b1"], "한 쪽에 2개, 최신부터");
    assert!(
        page.contains("<span class=\"list-field\" title=\"판매처\">가몰</span>"),
        "{page}"
    );
    assert!(page.contains("title=\"가격\">9,900</span>"), "{page}");
    assert!(page.contains("title=\"배송\">무료</span>"), "{page}");
    assert_eq!(
        page.matches("<span class=\"field-flag\">종료</span>")
            .count(),
        2
    );
    assert!(!page.contains("구매 링크"));
    // 거르기 단추
    assert!(
        page.contains("aria-label=\"판매처(으)로 거르기\""),
        "{page}"
    );
    assert!(
        page.contains(">종료 빼기</a>") && page.contains(">종료만</a>"),
        "{page}"
    );

    // 종료 빼기
    let (_, page) = get(bind, "/b/deal?f.ended=0");
    assert_eq!(listed(&page), ["a1"]);
    assert!(
        page.contains("aria-current=\"page\">종료 빼기</a>"),
        "{page}"
    );
    // 판매처 + 종료만, 둘 다 건 주소에서 다른 칸 단추는 지금 조건을 유지한다
    let (_, page) = get(bind, "/b/deal?f.store=mall_a&f.ended=1");
    assert_eq!(listed(&page), ["a2"]);
    assert!(
        page.contains("href=\"/b/deal?f.store=mall_a&amp;f.ended=0\""),
        "판매처를 유지한 채 종료 조건만 바꾼다: {page}"
    );
    // 쪽 넘김은 조건을 유지한다
    let (_, page) = get(bind, "/b/deal?f.store=mall_a");
    assert_eq!(listed(&page), ["a2", "a1"]);
    let (_, page) = get(bind, "/b/deal");
    assert!(page.contains("page=2"), "{page}");
    let (_, page) = get(bind, "/b/deal?f.ended=1&page=1");
    assert_eq!(listed(&page), ["a2", "b1"]);
    // 거를 수 없는 칸·없는 선택지는 버린다(오래된 링크도 열린다)
    let (status, page) = get(bind, "/b/deal?f.store=nowhere&f.price=9900");
    assert_eq!(status, 200);
    assert_eq!(listed(&page), ["a2", "b1"]);

    // REST 도 같은 서비스: 실제로 건 조건을 돌려준다.
    let (status, body) = get(bind, "/api/v1/boards/deal/posts?f.ended=0&f.store=nowhere");
    assert_eq!(status, 200, "{body}");
    let v: Value = serde_json::from_str(&body).unwrap();
    assert_eq!(v["data"]["posts"]["total"], 1);
    assert_eq!(v["data"]["filters"], serde_json::json!([["ended", "0"]]));
}
