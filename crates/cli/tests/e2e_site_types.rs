//! 유형별 프리셋 (ADR-0125, ADR-0126, ADR-0127). 실제 바이너리·실제 포트·실제 DB 파일로
//! 프리셋 한 번에 첫 화면과 메뉴가 뜨는지, 폼에 낸 내용이 DB·받은 내용·툴까지 가는지 본다.
#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::path::Path;
use std::process::{Child, Command, Stdio};

use serde_json::Value;

const BIN: &str = env!("CARGO_BIN_EXE_maru");
const TYPES: [&str; 20] = [
    "docs",
    "cafe",
    "company",
    "landing",
    "portfolio",
    "org",
    "magazine",
    "event",
    "jobs",
    "course",
    "shop",
    "platform",
    "clinic",
    "academy",
    "stay",
    "realty",
    "church",
    "club",
    "linkbio",
    "office",
];

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
        .env_remove("MARU_TOKEN")
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

fn request(bind: &str, raw: &str) -> (u16, String, String) {
    let mut s = TcpStream::connect(bind).unwrap();
    s.write_all(raw.as_bytes()).unwrap();
    let mut out = Vec::new();
    s.read_to_end(&mut out).unwrap();
    let text = String::from_utf8(out).unwrap();
    let (head, body) = text.split_once("\r\n\r\n").unwrap_or((&text, ""));
    (
        head[9..12].parse().unwrap(),
        head.to_owned(),
        body.to_owned(),
    )
}

fn get(bind: &str, path: &str) -> (u16, String, String) {
    request(
        bind,
        &format!("GET {path} HTTP/1.1\r\nHost: {bind}\r\nConnection: close\r\n\r\n"),
    )
}

fn request_post_status(bind: &str, path: &str) -> u16 {
    let raw =
        format!("POST {path} HTTP/1.1@@Host: {bind}@@Connection: close@@Content-Length: 0@@@@");
    request(bind, &raw.replace("@@", "\r\n")).0
}

/// 폼 제출. `origin` 이 비어 있지 않으면 그 출처에서 보낸 것처럼 보낸다.
fn post_form(bind: &str, path: &str, origin: &str, body: &str) -> (u16, String, String) {
    let body = body
        .replace('%', "%25")
        .replace(' ', "+")
        .replace('@', "%40");
    request(
        bind,
        &format!(
            "POST {path} HTTP/1.1\r\nHost: {bind}\r\nOrigin: {origin}\r\nConnection: close\r\n\
             Content-Type: application/x-www-form-urlencoded\r\nContent-Length: {}\r\n\r\n{body}",
            body.len()
        ),
    )
}

/// 화면 안의 같은 사이트 주소들. 쪽 안 이동(`#`)과 관리·정적 파일은 뺀다.
fn links(html: &str) -> Vec<String> {
    let mut out: Vec<String> = html
        .split("href=\"/")
        .skip(1)
        .filter_map(|part| part.split('"').next())
        .map(|p| format!("/{}", p.split('#').next().unwrap_or_default()))
        .filter(|p| {
            !p.starts_with("/static")
                && !p.starts_with("/media")
                && !p.starts_with("/feed")
                && !p.starts_with("/rss")
                && !p.starts_with("/sitemap")
        })
        .collect();
    out.sort();
    out.dedup();
    out
}

fn rows(data: &Path, form: &str) -> Vec<String> {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(async {
        let db = maru_storage::Db::open(&data.join("maru.db"), false)
            .await
            .unwrap();
        let v: Vec<String> =
            sqlx::query_scalar("SELECT data FROM mod_forms_submission WHERE form = ? ORDER BY id")
                .bind(form)
                .fetch_all(db.read_pool())
                .await
                .unwrap();
        db.close().await;
        v
    })
}

#[test]
fn 유형_프리셋은_한_번에_첫_화면과_메뉴가_뜨고_두_번_실행해도_그대로다() {
    for id in TYPES {
        let tmp = tempfile::tempdir().unwrap();
        let data = tmp.path().join("사이트");
        maru(&data, &["init", "--preset", id, "--yes"]);
        let widgets = maru(&data, &["widget", "list"]);
        let again = maru(&data, &["init", "--preset", id, "--yes"]);
        assert_eq!(again["created"], false, "{id}: {again}");
        assert_eq!(
            maru(&data, &["widget", "list"]).as_array().unwrap().len(),
            widgets.as_array().unwrap().len(),
            "{id}: 다시 실행하면 블록이 늘면 안 된다"
        );

        let (_server, bind) = start_server(&data);
        let (status, _, home) = get(&bind, "/");
        assert_eq!(status, 200, "{id}: {home}");
        assert_eq!(home.matches("<h1").count(), 1, "{id}: h1 은 하나: {home}");
        // 잡지는 첫 화면이 글 목록이다.
        if id != "magazine" {
            assert!(
                home.contains("class=\"sec "),
                "{id}: 섹션이 그려져야: {home}"
            );
        }
        assert!(!home.contains("undefined"), "{id}: {home}");

        // 메뉴와 버튼의 주소가 모두 살아 있어야 한다. 회원 전용 게시판은 막혀도 된다.
        for link in links(&home) {
            let (status, _, body) = get(&bind, &link);
            assert!(
                status < 400 || status == 401 || status == 403,
                "{id}: {link} → {status}: {body}"
            );
        }
    }
}

#[test]
fn 첫_화면_페이지는_바로가기와_사이트맵이_겹치지_않고_숨기면_목록으로_돌아간다() {
    let tmp = tempfile::tempdir().unwrap();
    let data = tmp.path().join("사이트");
    maru(&data, &["init", "--preset", "company", "--yes"]);
    let (_server, bind) = start_server(&data);

    let (status, head, _) = get(&bind, "/p/home");
    assert!((300..400).contains(&status), "{status} {head}");
    assert!(head.to_lowercase().contains("location: /\r\n"), "{head}");

    let (_, _, map) = get(&bind, "/sitemap.xml");
    assert!(!map.contains("/p/home"), "{map}");
    assert!(map.contains("/p/about"), "{map}");

    let bad = run(&data, &["site", "set", "--home-page", "없는주소"]);
    assert_eq!(bad["ok"], false, "{bad}");

    // 첫 화면 페이지를 내리면 최신 글 목록으로 돌아간다. 깨진 첫 화면은 없다.
    maru(&data, &["site", "set", "--home-page", ""]);
    let (status, _, home) = get(&bind, "/");
    assert_eq!(status, 200, "{home}");
    assert!(!home.contains("page-blocks"), "{home}");
    let (status, _, _) = get(&bind, "/p/home");
    assert_eq!(status, 200);
}

#[test]
fn 폼에_낸_내용은_db_와_툴까지_가고_막아야_할_제출은_남지_않는다() {
    let tmp = tempfile::tempdir().unwrap();
    let data = tmp.path().join("사이트");
    maru(&data, &["init", "--preset", "company", "--yes"]);
    let (_server, bind) = start_server(&data);
    let origin = format!("http://{bind}");
    let action = "/m/forms/submit/contact";

    let (status, _, page) = get(&bind, "/p/contact");
    assert_eq!(status, 200, "{page}");
    assert!(page.contains(action), "{page}");
    assert!(page.contains("개인정보"), "동의 문구: {page}");

    let good = "f_name=시험 이름&f_contact=test@example.com&f_message=견적 부탁합니다&consent=1";

    // 다른 사이트에서 보낸 것
    let (status, _, _) = post_form(&bind, action, "https://evil.example", good);
    assert_eq!(status, 403);
    // 동의 없음
    let (status, _, body) = post_form(
        &bind,
        action,
        &origin,
        "f_name=시험 이름&f_contact=test@example.com&f_message=견적",
    );
    assert_eq!(status, 422, "{body}");
    assert!(body.contains("동의"), "{body}");
    assert!(body.contains("시험 이름"), "쓴 내용은 남아야: {body}");
    // 함정 칸을 채운 자동 프로그램은 받은 척만 한다
    let (status, _, _) = post_form(&bind, action, &origin, &format!("{good}&website=spam"));
    assert_eq!(status, 200);
    assert!(rows(&data, "contact").is_empty());

    // 확인 질문을 켜 둔 사이트는 질문을 답하기 전에 저장하지 않는다.
    maru(&data, &["site", "set", "--captcha", "guest"]);
    let (status, _, body) = post_form(&bind, action, &origin, good);
    assert_eq!(status, 200, "{body}");
    assert!(body.contains("captcha_token"), "{body}");
    assert!(body.contains("견적 부탁합니다"), "쓴 내용은 남아야: {body}");
    assert!(rows(&data, "contact").is_empty());

    maru(&data, &["site", "set", "--captcha", "off"]);
    let (status, _, body) = post_form(&bind, action, &origin, good);
    assert_eq!(status, 200, "{body}");
    assert!(body.contains("받았습니다"), "{body}");
    let saved = rows(&data, "contact");
    assert_eq!(saved.len(), 1, "{saved:?}");
    assert!(saved[0].contains("견적 부탁합니다"), "{saved:?}");

    // 툴은 바깥 사람이 쓴 글이라고 표시한다 (원칙 6).
    let listed = maru(&data, &["forms", "list"]);
    assert_eq!(listed["untrusted"], true, "{listed}");
    let items = listed["items"].as_array().unwrap();
    assert_eq!(items.len(), 1, "{listed}");

    // 받은 내용 화면은 로그인 없이 보이지 않는다.
    let (status, head, _) = get(&bind, "/m/forms");
    assert!(
        status == 303 || status == 302 || status == 403,
        "{status} {head}"
    );

    // 감사 기록에 내용이 실리지 않는다.
    let audit = maru(&data, &["audit", "list"]).to_string();
    assert!(!audit.contains("견적 부탁합니다"), "{audit}");

    let id = items[0]["id"].as_str().unwrap();
    maru(&data, &["forms", "delete", "--id", id]);
    assert!(rows(&data, "contact").is_empty());
}

fn with_cookie(bind: &str, path: &str, cookie: &str) -> (u16, String, String) {
    request(
        bind,
        &format!(
            "GET {path} HTTP/1.1\r\nHost: {bind}\r\nCookie: {cookie}\r\nConnection: close\r\n\r\n"
        ),
    )
}

/// 로그인 링크를 열고 확인 버튼을 눌러 세션 쿠키를 받는다.
fn sign_in(bind: &str, url: &str) -> String {
    let rest = url.split_once("://").unwrap().1;
    let path = &rest[rest.find('/').unwrap()..];
    let (status, head, body) = get(bind, path);
    assert_eq!(status, 200, "{body}");
    let cookie_of = |head: &str, name: &str| {
        head.lines()
            .filter_map(|l| l.split_once(':'))
            .filter(|(k, _)| k.eq_ignore_ascii_case("set-cookie"))
            .find_map(|(_, v)| {
                let first = v.trim().split(';').next()?;
                let (k, v) = first.split_once('=')?;
                (k == name && !v.is_empty()).then(|| v.to_owned())
            })
    };
    let csrf = cookie_of(&head, "maru_csrf").unwrap();
    let form = format!("_csrf={csrf}");
    let (status, head, body) = request(
        bind,
        &format!(
            "POST {path} HTTP/1.1\r\nHost: {bind}\r\nCookie: maru_csrf={csrf}\r\nConnection: close\r\n\
             Content-Type: application/x-www-form-urlencoded\r\nContent-Length: {}\r\n\r\n{form}",
            form.len()
        ),
    );
    assert_eq!(status, 303, "{body}");
    format!("maru_session={}", cookie_of(&head, "maru_session").unwrap())
}

/// ADR-0129: 회원 전용 게시판은 손님을 로그인으로 보내고, 로그인하면 읽힌다.
#[test]
fn 회원_전용_게시판은_로그인하면_읽히고_손님_화면과_목록에_새지_않는다() {
    let tmp = tempfile::tempdir().unwrap();
    let data = tmp.path().join("사이트");
    maru(&data, &["init", "--preset", "course", "--yes"]);
    let posted = maru(
        &data,
        &[
            "post",
            "new",
            "--publish",
            "--board",
            "notes",
            "--title",
            "1주차 노트",
            "--body",
            "회원만 보는 내용",
        ],
    );
    let slug = posted["slug"].as_str().unwrap().to_owned();
    let (_server, bind) = start_server(&data);
    let list = "/b/notes";
    let one = format!("/b/notes/{slug}");

    for path in [list, one.as_str()] {
        let (status, head, body) = get(&bind, path);
        assert_eq!(status, 303, "{path}: {body}");
        assert!(head.contains("/login?next="), "{head}");
        assert!(!body.contains("회원만 보는"), "{body}");
    }
    // 없는 게시판은 그대로 없다.
    assert_eq!(get(&bind, "/b/nothing").0, 404);
    // 사이트맵·전체 글 목록에 새지 않는다.
    let (_, _, map) = get(&bind, "/sitemap.xml");
    assert!(!map.contains(&one), "{map}");
    let (_, _, rss) = get(&bind, "/rss.xml");
    assert!(!rss.contains("회원만 보는"), "{rss}");

    let link = maru(&data, &["login-link"]);
    let cookie = sign_in(&bind, link["url"].as_str().unwrap());
    let (status, head, body) = with_cookie(&bind, list, &cookie);
    assert_eq!(status, 200, "{body}");
    assert!(body.contains("1주차 노트"), "{body}");
    assert!(head.to_lowercase().contains("no-store"), "{head}");
    assert!(body.contains("noindex"), "{body}");
    let (status, head, body) = with_cookie(&bind, &one, &cookie);
    assert_eq!(status, 200, "{body}");
    assert!(body.contains("회원만 보는 내용"), "{body}");
    assert!(head.to_lowercase().contains("no-store"), "{head}");

    // 손님에게 열린 공지는 로그인해도 손님과 같은 화면이고 캐시를 막지 않는다.
    let (status, head, _) = with_cookie(&bind, "/b/notice", &cookie);
    assert_eq!(status, 200);
    assert!(!head.to_lowercase().contains("no-store"), "{head}");
}

/// ADR-0128: 문서는 목록·위치·앞뒤가 이어지고, 계층에 고리를 만들 수 없다.
#[test]
fn 문서_프리셋은_목록과_위치와_앞뒤로_이어진다() {
    let tmp = tempfile::tempdir().unwrap();
    let data = tmp.path().join("사이트");
    maru(&data, &["init", "--preset", "docs", "--yes"]);
    let (_server, bind) = start_server(&data);

    let (status, _, body) = get(&bind, "/p/install");
    assert_eq!(status, 200, "{body}");
    assert!(body.contains("class=\"crumbs\""), "위치 표시: {body}");
    assert!(
        body.contains("aria-current=\"page\">설치"),
        "지금 문서: {body}"
    );
    assert!(body.contains("자주 하는 일 하나"), "문서 목록: {body}");
    assert!(body.contains("rel=\"prev\""), "{body}");
    assert!(body.contains("/p/start"), "{body}");
    assert!(body.contains("rel=\"next\""), "{body}");
    assert!(body.contains("/p/first-steps"), "{body}");

    // 위 페이지가 자기 아래가 될 수 없다.
    let bad = run(&data, &["page", "edit", "start", "--parent", "install"]);
    assert_eq!(bad["ok"], false, "{bad}");
    let bad = run(&data, &["page", "edit", "start", "--parent", "없는주소"]);
    assert_eq!(bad["ok"], false, "{bad}");

    // 맨 위로 옮기면 위치 표시가 사라진다.
    maru(&data, &["page", "edit", "install", "--parent", ""]);
    let (_, _, body) = get(&bind, "/p/install");
    assert!(!body.contains("class=\"crumbs\""), "{body}");
}

/// ADR-0128: 주소를 바꿔도 옛 링크가 살아 있고, 살아 있는 화면이 먼저다.
#[test]
fn 주소를_바꾸면_옛_주소가_새_주소로_간다() {
    let tmp = tempfile::tempdir().unwrap();
    let data = tmp.path().join("사이트");
    maru(&data, &["init", "--preset", "company", "--yes"]);
    let (_server, bind) = start_server(&data);
    let location = |head: &str| {
        head.lines()
            .filter_map(|l| l.split_once(':'))
            .find(|(k, _)| k.eq_ignore_ascii_case("location"))
            .map(|(_, v)| v.trim().to_owned())
            .unwrap_or_default()
    };

    maru(&data, &["page", "edit", "about", "--new-slug", "소개"]);
    let (status, head, _) = get(&bind, "/p/about?x=1");
    assert_eq!(status, 301, "{head}");
    assert_eq!(location(&head), "/p/%EC%86%8C%EA%B0%9C?x=1");
    assert_eq!(get(&bind, "/p/%EC%86%8C%EA%B0%9C").0, 200);

    // 두 번 옮겨도 한 번에 간다.
    maru(&data, &["page", "edit", "소개", "--new-slug", "company"]);
    let (status, head, _) = get(&bind, "/p/about");
    assert_eq!(status, 301, "{head}");
    assert_eq!(location(&head), "/p/company");

    // 옛 주소에 새 페이지를 만들면 그 페이지가 보인다.
    maru(
        &data,
        &["page", "new", "about", "--title", "새 소개", "--publish"],
    );
    let (status, _, body) = get(&bind, "/p/about");
    assert_eq!(status, 200);
    assert!(body.contains("새 소개"), "{body}");

    // 글 슬러그
    let posted = maru(
        &data,
        &[
            "post",
            "new",
            "--publish",
            "--board",
            "notice",
            "--title",
            "공지 하나",
            "--body",
            "본문",
        ],
    );
    let id = posted["id"].as_str().unwrap();
    let old = format!("/b/notice/{}", posted["slug"].as_str().unwrap());
    maru(&data, &["post", "edit", id, "--slug", "first-notice"]);
    let (status, head, _) = get(&bind, &old);
    assert_eq!(status, 301, "{head}");
    assert_eq!(location(&head), "/b/notice/first-notice");

    // 직접 더하기: 다른 사이트에서 옮겨 올 때
    maru(&data, &["redirect", "add", "/old/greeting", "/p/company"]);
    let (status, head, _) = get(&bind, "/old/greeting");
    assert_eq!(status, 301, "{head}");
    assert_eq!(location(&head), "/p/company");
    assert_eq!(request_post_status(&bind, "/old/greeting"), 404);

    for (from, to) in [
        ("/admin/x", "/p/company"),
        ("/api/v1/posts", "/p/company"),
        ("/old/a", "javascript:alert(1)"),
        ("/old/a", "//evil.example"),
        ("/old/greeting", "/old/greeting"),
        ("/p/company", "/old/greeting"),
    ] {
        let v = run(&data, &["redirect", "add", from, to]);
        assert_eq!(v["ok"], false, "{from} -> {to}: {v}");
    }

    let listed = maru(&data, &["redirect", "list"]);
    assert!(listed.to_string().contains("/old/greeting"), "{listed}");
    maru(&data, &["redirect", "remove", "/old/greeting"]);
    assert_eq!(get(&bind, "/old/greeting").0, 404);
}

/// ADR-0128: 옮긴 주소도 변경 묶음으로 담고 한 번에 되돌린다.
#[test]
fn 옮긴_주소는_변경_묶음으로_담고_되돌린다() {
    let tmp = tempfile::tempdir().unwrap();
    let data = tmp.path().join("사이트");
    maru(&data, &["init", "--preset", "docs", "--yes"]);
    let ops = tmp.path().join("ops.json");
    std::fs::write(
        &ops,
        r#"[{"op":"redirect.add","from":"/old","to":"/p/start"},
            {"op":"page.update","slug":"install","patch":{"parent":"guides"}}]"#,
    )
    .unwrap();
    let cs = maru(
        &data,
        &[
            "changeset",
            "new",
            "--title",
            "옮기기",
            "--ops-file",
            ops.to_str().unwrap(),
        ],
    );
    let id = cs["id"].as_str().unwrap();
    maru(&data, &["changeset", "apply", id]);
    assert!(
        maru(&data, &["redirect", "list"])
            .to_string()
            .contains("/old")
    );
    let parent = |d: &Path| {
        let v = maru(d, &["page", "show", "install"]);
        let guides = maru(d, &["page", "show", "guides"]);
        v["parent_id"] == guides["id"]
    };
    assert!(parent(&data));

    maru(&data, &["changeset", "revert", id]);
    assert!(
        !maru(&data, &["redirect", "list"])
            .to_string()
            .contains("/old")
    );
    assert!(!parent(&data), "위 페이지도 돌아가야 한다");
    let start = maru(&data, &["page", "show", "start"]);
    assert_eq!(
        maru(&data, &["page", "show", "install"])["parent_id"],
        start["id"]
    );
}

/// ADR-0139: 새 종류는 메뉴로 가는 화면이 모두 뜨고 화면마다 h1 이 하나다.
#[test]
fn 새_종류의_메뉴_화면은_모두_뜨고_제목은_하나다() {
    for id in &TYPES[10..] {
        let tmp = tempfile::tempdir().unwrap();
        let data = tmp.path().join("사이트");
        maru(&data, &["init", "--preset", id, "--yes"]);
        let (_server, bind) = start_server(&data);
        let (_, _, home) = get(&bind, "/");
        let pages: Vec<String> = links(&home)
            .into_iter()
            .filter(|l| (l.starts_with("/p/") || l.starts_with("/b/")) && !l.contains('?'))
            .collect();
        assert!(!pages.is_empty(), "{id}: 메뉴가 있어야");
        for link in pages {
            let (status, _, body) = get(&bind, &link);
            if status == 200 {
                assert_eq!(body.matches("<h1").count(), 1, "{id} {link}: h1 은 하나");
                assert!(!body.contains("undefined"), "{id} {link}");
            } else {
                assert!(status == 303, "{id}: {link} → {status}");
            }
        }
    }
}

/// ADR-0139: 매물 칸으로 거르고, 거래 완료는 빼고 본다.
#[test]
fn 부동산_매물은_거래_종류로_거르고_완료한_것을_뺀다() {
    let tmp = tempfile::tempdir().unwrap();
    let data = tmp.path().join("사이트");
    maru(&data, &["init", "--preset", "realty", "--yes"]);
    let common = [
        "--field",
        "type=apt",
        "--field",
        "price=0억 원",
        "--field",
        "address=어느 동",
        "--field",
        "area=00㎡",
    ];
    for (title, deal, closed) in [
        ("매매 시험 매물", "sale", "false"),
        ("전세 시험 매물", "jeonse", "false"),
        ("끝난 시험 매물", "sale", "true"),
    ] {
        let deal = format!("deal={deal}");
        let closed = format!("closed={closed}");
        let mut args = vec![
            "post",
            "new",
            "--publish",
            "--board",
            "listings",
            "--title",
            title,
            "--body",
            "설명",
            "--field",
            &deal,
            "--field",
            &closed,
        ];
        args.extend(common);
        maru(&data, &args);
    }
    // 필수 칸이 빠지면 받지 않는다.
    let bad = run(
        &data,
        &[
            "post",
            "new",
            "--publish",
            "--board",
            "listings",
            "--title",
            "빈 매물",
            "--body",
            "설명",
        ],
    );
    assert_eq!(bad["ok"], false, "{bad}");

    let (_server, bind) = start_server(&data);
    let (status, _, sale) = get(&bind, "/b/listings?f.deal=sale");
    assert_eq!(status, 200, "{sale}");
    assert!(sale.contains("매매 시험 매물"), "{sale}");
    assert!(!sale.contains("전세 시험 매물"), "{sale}");
    let (_, _, open) = get(&bind, "/b/listings?f.closed=0");
    assert!(
        open.contains("매매 시험 매물") && open.contains("전세 시험 매물"),
        "{open}"
    );
    assert!(!open.contains("끝난 시험 매물"), "{open}");
    // 바닥글에 사무소 정보 자리가 있다.
    assert!(sale.contains("중개사무소 등록번호"), "{sale}");
}

/// ADR-0139: 숙박 예약 문의는 받은 내용으로 DB 에 남는다. 병원은 후기 블록이 없다.
#[test]
fn 숙박_예약_문의는_db_에_남고_병원에는_후기가_없다() {
    let tmp = tempfile::tempdir().unwrap();
    let data = tmp.path().join("사이트");
    maru(&data, &["init", "--preset", "stay", "--yes"]);
    // 손님 확인 질문은 기본으로 켜져 있다. 폼 저장만 보려고 끈다.
    maru(&data, &["site", "set", "--captcha", "off"]);
    let (server, bind) = start_server(&data);
    let origin = format!("http://{bind}");
    let action = "/m/forms/submit/booking";
    let (status, _, page) = get(&bind, "/p/booking");
    assert_eq!(status, 200, "{page}");
    assert!(page.contains(action), "{page}");

    let body = "f_name=시험 이름&f_contact=010-0000-0000&f_checkin=2026-12-24&f_nights=2박\
                &f_guests=4명&f_message=바비큐 되나요&consent=1";
    let (status, _, out) = post_form(&bind, action, &origin, body);
    assert_eq!(status, 200, "{out}");
    assert!(out.contains("문의를 받았습니다"), "{out}");
    drop(server);
    let saved = rows(&data, "booking");
    assert_eq!(saved.len(), 1, "{saved:?}");
    assert!(
        saved[0].contains("2026-12-24") && saved[0].contains("2박"),
        "{saved:?}"
    );

    let tmp = tempfile::tempdir().unwrap();
    let data = tmp.path().join("사이트");
    maru(&data, &["init", "--preset", "clinic", "--yes"]);
    let widgets = maru(&data, &["widget", "list"]).to_string();
    assert!(!widgets.contains("testimonials"), "{widgets}");
    assert!(!widgets.contains("후기"), "{widgets}");
}
