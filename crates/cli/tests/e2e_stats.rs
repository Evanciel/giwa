//! GAP S3: 방문자 통계 (ADR-0061). 실제 바이너리·실제 포트·실제 DB 파일로 쿠키 없이 사람을 하루 한 번 세고,
//! 봇·추적 거부·미리 읽기·운영자를 빼며, CLI·REST·CSV·관리 화면이 같은 답을 주는지 본다.
#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use serde_json::Value;

const BIN: &str = env!("CARGO_BIN_EXE_maru");
const CHROME: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) Chrome/130.0";
const FIREFOX: &str = "Mozilla/5.0 (Windows NT 10.0; rv:131.0) Gecko/20100101 Firefox/131.0";
const FIREFOX2: &str = "Mozilla/5.0 (X11; Linux x86_64; rv:132.0) Gecko/20100101 Firefox/132.0";
const EDGE: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) Chrome/130.0 Edg/130.0";

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
    let v: Value =
        serde_json::from_str(stdout.trim()).unwrap_or_else(|e| panic!("JSON 아님: {e}\n{stdout}"));
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

    fn json(&self) -> Value {
        serde_json::from_str(&self.body).unwrap_or_else(|e| panic!("JSON 아님: {e}\n{}", self.body))
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

fn header<'a>(res: &'a Res, name: &str) -> Option<&'a str> {
    res.head.lines().find_map(|l| {
        let (k, v) = l.split_once(':')?;
        k.trim().eq_ignore_ascii_case(name).then(|| v.trim())
    })
}

#[test]
fn stats_count_people_without_cookies_and_skip_bots_dnt_and_staff() {
    let tmp = tempfile::tempdir().unwrap();
    let data = tmp.path().join("사이트");
    let init = maru(&data, &["init", "--preset", "blog", "--yes"]);
    // 기본 토큰(read, draft)은 통계를 못 본다. 운영 데이터라 publish 스코프가 필요하다(SPEC §9 운영).
    let reader = format!("Bearer {}", init["token"]["secret"].as_str().unwrap());
    let ops = maru(
        &data,
        &[
            "token", "create", "--name", "통계", "--scope", "read", "--scope", "publish",
        ],
    );
    let bearer = format!("Bearer {}", ops["secret"].as_str().unwrap());
    let link = maru(&data, &["login-link"]);
    maru(
        &data,
        &[
            "post",
            "new",
            "--title",
            "통계 글",
            "--slug",
            "hello",
            "--publish",
        ],
    );
    // 프리셋의 소개 페이지는 초안으로 만들어진다. 조회수가 없어 본문이 그대로라 304 를 받는다.
    maru(&data, &["page", "publish", "about"]);
    let (_server, bind) = start_server(&data);
    let session = sign_in(&bind, link["url"].as_str().unwrap());

    let get = |path: &str, headers: &[(&str, &str)]| http(&bind, "GET", path, headers, "");
    // 세는 방문 6: 같은 사람 둘(쿼리만 다름), 검색에서 온 사람, 내부 이동, 새 사람이 받고 304 로 다시 받기
    let first = get("/", &[("User-Agent", CHROME)]);
    assert_eq!(first.status, 200);
    assert!(
        header(&first, "set-cookie").is_none(),
        "통계는 쿠키를 심지 않는다"
    );
    get("/?utm_source=x", &[("User-Agent", CHROME)]);
    let post = get(
        "/b/blog/hello",
        &[
            ("User-Agent", FIREFOX),
            (
                "Referer",
                "https://search.naver.com/search.naver?query=%EB%B9%84%EB%B0%80",
            ),
        ],
    );
    assert_eq!(post.status, 200);
    let internal = format!("http://{bind}/");
    get(
        "/b/blog/hello",
        &[("User-Agent", CHROME), ("Referer", &internal)],
    );
    let about = get("/p/about", &[("User-Agent", EDGE)]);
    assert_eq!(about.status, 200);
    let etag = header(&about, "etag").unwrap().to_owned();
    let again = get(
        "/p/about",
        &[("User-Agent", EDGE), ("If-None-Match", &etag)],
    );
    assert_eq!(again.status, 304, "304 도 방문이다");
    // 세지 않는 것: 봇, 추적 거부, 미리 읽기, 자산, 없는 화면, 로그인한 운영자
    get(
        "/",
        &[("User-Agent", "Mozilla/5.0 (compatible; Googlebot/2.1)")],
    );
    get("/", &[("User-Agent", FIREFOX2), ("DNT", "1")]);
    get("/", &[("User-Agent", FIREFOX2), ("Sec-GPC", "1")]);
    get(
        "/",
        &[("User-Agent", FIREFOX2), ("Sec-Purpose", "prefetch")],
    );
    assert_eq!(
        get("/assets/site.css", &[("User-Agent", FIREFOX2)]).status,
        200
    );
    assert_eq!(get("/b/nope", &[("User-Agent", FIREFOX2)]).status, 404);
    let cookie = format!("maru_session={session}");
    get("/", &[("User-Agent", FIREFOX2), ("Cookie", &cookie)]);

    // 모은 수는 주기(20초)마다 DB 에 더해진다.
    let started = Instant::now();
    let s = loop {
        let s = maru(&data, &["stats", "--days", "1"]);
        if s["views"].as_i64().unwrap() >= 6 {
            break s;
        }
        assert!(
            started.elapsed() < Duration::from_secs(45),
            "통계가 DB 에 닿지 않음: {s}"
        );
        std::thread::sleep(Duration::from_millis(500));
    };
    assert_eq!(s["views"], 6, "{s}");
    assert_eq!(s["visitors"], 3, "{s}");
    assert_eq!(s["pages"][0]["path"], "/b/blog/hello", "{s}");
    assert_eq!(s["pages"][0]["title"], "통계 글");
    assert_eq!(
        (
            s["pages"][0]["views"].as_i64(),
            s["pages"][0]["visitors"].as_i64()
        ),
        (Some(2), Some(2))
    );
    assert_eq!(s["pages"][1]["path"], "/");
    assert_eq!(
        (
            s["pages"][1]["views"].as_i64(),
            s["pages"][1]["visitors"].as_i64()
        ),
        (Some(2), Some(1))
    );
    assert_eq!(s["pages"][2]["path"], "/p/about", "{s}");
    assert!(!s["pages"][2]["title"].as_str().unwrap().is_empty());
    assert_eq!(s["pages"].as_array().unwrap().len(), 3, "{s}");
    assert_eq!(
        s["referrers"],
        serde_json::json!([{ "host": "search.naver.com", "visits": 1 }])
    );

    // REST 는 같은 서비스로 같은 답, 손님은 못 본다.
    let api = get("/api/v1/stats?days=1", &[("Authorization", &bearer)]);
    assert_eq!(api.status, 200, "{}", api.body);
    assert_eq!(api.json()["data"]["views"], 6);
    assert_eq!(get("/api/v1/stats", &[]).status, 401);
    assert_eq!(
        get("/api/v1/stats", &[("Authorization", &reader)]).status,
        403
    );
    let bad = get("/api/v1/stats?days=999", &[("Authorization", &bearer)]);
    assert_eq!(bad.status, 400, "{}", bad.body);
    let csv = get("/api/v1/stats.csv?days=1", &[("Authorization", &bearer)]);
    assert_eq!(csv.status, 200);
    assert!(
        header(&csv, "content-type")
            .unwrap()
            .starts_with("text/csv")
    );
    assert!(
        header(&csv, "content-disposition")
            .unwrap()
            .contains("attachment")
    );
    assert!(csv.body.starts_with('\u{feff}'), "엑셀용 BOM");
    assert!(
        csv.body.contains("/b/blog/hello,통계 글,2,2"),
        "{}",
        csv.body
    );

    // CLI CSV 파일
    let out = tmp.path().join("통계.csv");
    maru(
        &data,
        &["stats", "--days", "1", "--csv", out.to_str().unwrap()],
    );
    let text = std::fs::read_to_string(&out).unwrap();
    assert!(text.contains("search.naver.com,1"), "{text}");

    // 관리 화면
    let page = get("/admin/stats?days=7", &[("Cookie", &cookie)]);
    assert_eq!(page.status, 200, "{}", page.body);
    assert!(
        page.body.contains("<meter"),
        "막대는 meter (CSP 가 style 속성을 막는다)"
    );
    assert!(page.body.contains("통계 글"));
    assert!(page.body.contains("search.naver.com"));
    let file = get("/admin/stats.csv?days=7", &[("Cookie", &cookie)]);
    assert_eq!(file.status, 200);
    assert!(file.body.contains("날짜,조회,방문자"));
}
