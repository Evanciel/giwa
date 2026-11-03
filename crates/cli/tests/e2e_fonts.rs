//! 올린 서체 (ADR-0048)를 실제 바이너리·실제 포트·실제 DB 파일로 확인한다.
//! CLI 로 올리고 → 공개 CSS·파일 → 글 안 꾸밈 → REST(같은 서비스) → 디자인 토큰 → 지우기.
#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::path::Path;
use std::process::{Child, Command, Stdio};

use serde_json::Value;

const BIN: &str = env!("CARGO_BIN_EXE_maru");
const PEN: &[u8] = include_bytes!("../../themes/builtin/static/fonts/maru-pen-1.woff2");
const OFL: &str = include_str!("../../themes/builtin/static/fonts/OFL.txt");

struct Server(Child);

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// `--json` 응답 전체(`ok` 포함).
fn maru(data: &Path, args: &[&str]) -> Value {
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

fn ok(data: &Path, args: &[&str]) -> Value {
    let v = maru(data, args);
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
    body: Vec<u8>,
}

impl Res {
    fn header(&self, name: &str) -> Option<String> {
        let name = name.to_ascii_lowercase();
        self.head.lines().find_map(|l| {
            let (k, v) = l.split_once(':')?;
            (k.trim().to_ascii_lowercase() == name).then(|| v.trim().to_owned())
        })
    }

    fn text(&self) -> String {
        String::from_utf8_lossy(&self.body).into_owned()
    }

    fn json(&self) -> Value {
        serde_json::from_slice(&self.body)
            .unwrap_or_else(|e| panic!("JSON 아님 ({e}): {}", self.text()))
    }

    fn cookie(&self, name: &str) -> Option<String> {
        self.head.lines().find_map(|l| {
            let (k, v) = l.split_once(':')?;
            if !k.trim().eq_ignore_ascii_case("set-cookie") {
                return None;
            }
            let (ck, cv) = v.trim().split(';').next()?.split_once('=')?;
            (ck == name).then(|| cv.to_owned())
        })
    }

    fn csrf_field(&self) -> String {
        let body = self.text();
        let at = body.find("name=\"_csrf\" value=\"").expect("_csrf 필드");
        let rest = &body[at + 20..];
        rest[..rest.find('"').unwrap()].to_owned()
    }
}

fn path_of(url: &str) -> String {
    let rest = url.split_once("://").unwrap().1;
    rest[rest.find('/').unwrap()..].to_owned()
}

/// 로그인 링크로 운영자 세션을 받고, 서체 화면에서 세션 CSRF 값을 읽는다.
fn admin_session(data: &Path, bind: &str) -> (String, String) {
    let link = ok(data, &["login-link"]);
    let link_path = path_of(link["url"].as_str().unwrap());
    let page = http(bind, "GET", &link_path, &[], b"");
    let pre = page.cookie("maru_csrf").unwrap();
    let res = http(
        bind,
        "POST",
        &link_path,
        &[
            ("Content-Type", "application/x-www-form-urlencoded"),
            ("Cookie", &format!("maru_csrf={pre}")),
        ],
        format!("_csrf={pre}").as_bytes(),
    );
    assert_eq!(res.status, 303, "{}", res.text());
    let cookie = format!("maru_session={}", res.cookie("maru_session").unwrap());
    let page = http(bind, "GET", "/admin/fonts", &[("Cookie", &cookie)], b"");
    assert_eq!(page.status, 200);
    (cookie, page.csrf_field())
}

fn http(bind: &str, method: &str, path: &str, headers: &[(&str, &str)], body: &[u8]) -> Res {
    let mut s = TcpStream::connect(bind).unwrap();
    let mut req = format!("{method} {path} HTTP/1.1\r\nHost: {bind}\r\nConnection: close\r\n");
    for (k, v) in headers {
        req.push_str(&format!("{k}: {v}\r\n"));
    }
    req.push_str(&format!("Content-Length: {}\r\n\r\n", body.len()));
    s.write_all(req.as_bytes()).unwrap();
    let _ = s.write_all(body);
    let mut raw = Vec::new();
    let _ = s.read_to_end(&mut raw);
    let split = raw
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .expect("응답 머리");
    let head = String::from_utf8(raw[..split].to_vec()).unwrap();
    Res {
        status: head[9..12].parse().unwrap(),
        head,
        body: raw[split + 4..].to_vec(),
    }
}

/// 표 하나짜리 최소 sfnt (굵게 파일 대신).
fn ttf() -> Vec<u8> {
    let mut b = vec![0, 1, 0, 0, 0, 1, 0, 16, 0, 0, 0, 0];
    b.extend_from_slice(b"glyf");
    b.extend_from_slice(&[0, 0, 0, 0]);
    b.extend_from_slice(&28u32.to_be_bytes());
    b.extend_from_slice(&8u32.to_be_bytes());
    b.extend_from_slice(&[7; 8]);
    b
}

#[test]
fn uploaded_font_flows_from_cli_to_public_pages_and_back() {
    let tmp = tempfile::tempdir().unwrap();
    let data = tmp.path().join("사이트");
    let init = ok(&data, &["init", "--yes"]);
    let writer = format!("Bearer {}", init["token"]["secret"].as_str().unwrap());
    let design = ok(
        &data,
        &[
            "token",
            "create",
            "--name",
            "디자인",
            "--scope",
            "read",
            "--scope",
            "design",
        ],
    );
    let designer = format!("Bearer {}", design["secret"].as_str().unwrap());

    let file = tmp.path().join("회사 고딕.woff2");
    std::fs::write(&file, PEN).unwrap();
    let license = tmp.path().join("OFL.txt");
    std::fs::write(&license, OFL).unwrap();
    let file_arg = file.to_str().unwrap();
    let base = [
        "font",
        "add",
        file_arg,
        "--name",
        "회사 고딕",
        "--slug",
        "company-gothic",
        "--license",
        "OFL-1.1",
        "--license-file",
        license.to_str().unwrap(),
    ];

    // 배포 권리 확인(--yes)이 없으면 거절하고 다음 명령을 알려 준다. 빈 서체도 남지 않는다.
    let refused = maru(&data, &base);
    assert_eq!(refused["ok"], false, "{refused}");
    assert_eq!(refused["error"]["code"], "VALIDATION");
    assert!(refused.to_string().contains("--yes"), "{refused}");
    assert_eq!(ok(&data, &["font", "list"]).as_array().unwrap().len(), 0);

    let mut args = base.to_vec();
    args.push("--yes");
    let font = ok(&data, &args);
    assert_eq!(font["slug"], "company-gothic");
    assert_eq!(font["license_url"], "/fonts/company-gothic/LICENSE.txt");
    let url = font["files"][0]["url"].as_str().unwrap().to_owned();
    assert!(url.starts_with("/fonts/company-gothic/400-") && url.ends_with(".woff2"));
    // 다시 올려도 같은 파일 (멱등)
    let again = ok(&data, &args);
    assert_eq!(again["files"][0]["id"], font["files"][0]["id"]);
    // 글꼴이 아닌 파일
    let fake = tmp.path().join("가짜.ttf");
    std::fs::write(&fake, b"<svg onload=alert(1)>").unwrap();
    let bad = maru(
        &data,
        &[
            "font",
            "add",
            fake.to_str().unwrap(),
            "--name",
            "가짜",
            "--license",
            "MIT",
            "--yes",
        ],
    );
    assert_eq!(bad["error"]["code"], "VALIDATION", "{bad}");
    assert_eq!(ok(&data, &["font", "list"]).as_array().unwrap().len(), 1);

    // 글 안 꾸밈
    ok(
        &data,
        &[
            "post",
            "new",
            "--title",
            "서체 글",
            "--slug",
            "font-post",
            "--body",
            "가나다 :font[회사 서체]{company-gothic} 라마",
            "--publish",
            "--yes",
        ],
    );

    let (_server, bind) = start_server(&data);

    // 공개 CSS 와 파일
    let css = http(&bind, "GET", "/assets/fonts.css", &[], b"");
    assert_eq!(css.status, 200);
    assert_eq!(
        css.header("content-type").as_deref(),
        Some("text/css; charset=utf-8")
    );
    let text = css.text();
    assert!(
        text.contains(&format!(
            "font-family: \"maru-u-company-gothic\"; src: url(\"{url}\") format(\"woff2\")"
        )),
        "{text}"
    );
    assert!(text.contains(".ff-company-gothic { font-family: \"maru-u-company-gothic\", "));
    let res = http(&bind, "GET", &url, &[], b"");
    assert_eq!(res.status, 200);
    assert_eq!(res.header("content-type").as_deref(), Some("font/woff2"));
    assert!(res.header("cache-control").unwrap().contains("immutable"));
    assert_eq!(
        res.header("x-content-type-options").as_deref(),
        Some("nosniff")
    );
    assert_eq!(res.body, PEN);
    let lic = http(&bind, "GET", "/fonts/company-gothic/LICENSE.txt", &[], b"");
    assert_eq!(lic.status, 200);
    assert!(lic.text().contains("SIL OPEN FONT LICENSE"));
    for path in [
        "/fonts/company-gothic/nope.woff2",
        "/fonts/Company/x.woff2",
        "/fonts/company-gothic/..%2F..%2Fmaru.db",
    ] {
        assert_eq!(http(&bind, "GET", path, &[], b"").status, 404, "{path}");
    }
    let page = http(&bind, "GET", "/b/blog/font-post", &[], b"");
    assert_eq!(page.status, 200);
    let html = page.text();
    assert!(html.contains("<link rel=\"stylesheet\" href=\"/assets/fonts.css\">"));
    assert!(
        html.contains("<span class=\"ff-company-gothic\">회사 서체</span>"),
        "{html}"
    );

    // REST: CLI 와 같은 답. 굵게 파일은 design 권한이 있어야 올린다.
    let list = http(&bind, "GET", "/api/v1/fonts", &[], b"").json();
    assert_eq!(list["data"][0]["slug"], "company-gothic");
    assert_eq!(list["data"][0]["files"][0]["url"], url.as_str());
    let path = "/api/v1/fonts/company-gothic/files?weight=700";
    assert_eq!(http(&bind, "POST", path, &[], &ttf()).status, 401);
    let denied = http(&bind, "POST", path, &[("Authorization", &writer)], &ttf());
    assert_eq!(denied.status, 403);
    let up = http(&bind, "POST", path, &[("Authorization", &designer)], &ttf());
    assert_eq!(up.status, 201, "{}", up.text());
    assert_eq!(up.json()["data"]["font"]["files"][1]["weight"], 700);
    let text = http(&bind, "GET", "/assets/fonts.css", &[], b"").text();
    assert!(
        text.contains("format(\"truetype\"); font-weight: 700"),
        "{text}"
    );

    // 사이트 본문 서체로
    ok(
        &data,
        &["design", "set", "--set", "font.body=company-gothic"],
    );
    let tokens = http(&bind, "GET", "/assets/tokens.css", &[], b"").text();
    assert!(
        tokens.contains("--font-body: \"maru-u-company-gothic\", "),
        "{tokens}"
    );
    let missing = maru(
        &data,
        &["design", "set", "--set", "font.heading=no-such-font"],
    );
    assert_eq!(missing["error"]["code"], "VALIDATION", "{missing}");

    // 토큰이 쓰는 동안은 지울 수 없다 → 되돌리고 지운다. 글은 깨지지 않는다.
    let in_use = maru(&data, &["font", "remove", "company-gothic"]);
    assert_eq!(in_use["error"]["code"], "CONFLICT", "{in_use}");
    assert!(
        in_use
            .to_string()
            .contains("maru design set --set font.body=Pretendard")
    );
    ok(&data, &["design", "set", "--set", "font.body=Pretendard"]);
    let removed = ok(&data, &["font", "remove", "company-gothic"]);
    assert_eq!(removed["used_in_posts"], 1);
    assert_eq!(http(&bind, "GET", &url, &[], b"").status, 404);
    assert!(
        !http(&bind, "GET", "/assets/fonts.css", &[], b"")
            .text()
            .contains("company-gothic")
    );
    let page = http(&bind, "GET", "/b/blog/font-post", &[], b"");
    assert_eq!(page.status, 200);
    assert!(page.text().contains("회사 서체"));
}

#[test]
fn admin_screen_uploads_lists_and_feeds_the_editor_menu() {
    let tmp = tempfile::tempdir().unwrap();
    let data = tmp.path().join("사이트");
    ok(&data, &["init", "--yes"]);
    let (_server, bind) = start_server(&data);
    let (cookie, csrf) = admin_session(&data, &bind);

    // 비로그인은 로그인 화면으로
    let res = http(&bind, "GET", "/admin/fonts", &[], b"");
    assert_eq!(res.status, 303);

    let q = "/admin/fonts/upload?name=%ED%9A%8C%EC%82%AC%20%EA%B3%A0%EB%94%95&slug=&license=OFL-1.1&fallback=serif&weight=400&italic=0&rights=1";
    // CSRF 헤더가 없으면 거절
    let res = http(&bind, "POST", q, &[("Cookie", &cookie)], PEN);
    assert_eq!(res.status, 403, "{}", res.text());
    // 권리 확인이 없으면 거절
    let res = http(
        &bind,
        "POST",
        &q.replace("rights=1", "rights=0"),
        &[("Cookie", &cookie), ("X-CSRF-Token", &csrf)],
        PEN,
    );
    assert_eq!(res.status, 400, "{}", res.text());
    let res = http(
        &bind,
        "POST",
        q,
        &[("Cookie", &cookie), ("X-CSRF-Token", &csrf)],
        PEN,
    );
    assert_eq!(res.status, 201, "{}", res.text());
    let slug = res.json()["data"]["font"]["slug"]
        .as_str()
        .unwrap()
        .to_owned();
    assert_eq!(slug, "font-1");
    let res = http(
        &bind,
        "POST",
        &format!("/admin/fonts/{slug}/license"),
        &[
            ("Cookie", &cookie),
            ("X-CSRF-Token", &csrf),
            ("Content-Type", "application/json"),
        ],
        br#"{"license_text":"SIL OPEN FONT LICENSE Version 1.1"}"#,
    );
    assert_eq!(res.status, 200, "{}", res.text());
    // 결과는 CLI 와 같은 DB 에 남는다.
    let list = ok(&data, &["font", "list"]);
    assert_eq!(list[0]["name"], "회사 고딕");
    assert_eq!(list[0]["fallback"], "serif");
    assert_eq!(list[0]["license_url"], "/fonts/font-1/LICENSE.txt");

    // 서체 화면: 그 서체로 그린 미리보기, 폼 단추
    let page = http(&bind, "GET", "/admin/fonts", &[("Cookie", &cookie)], b"").text();
    assert!(
        page.contains("<p class=\"font-sample ff-font-1\">"),
        "{page}"
    );
    assert!(page.contains("href=\"/assets/fonts.css\""));
    assert!(page.contains("/admin/fonts/font-1/use-body"));

    // 글쓰기 화면: 서체 메뉴용 목록과 서체 CSS
    let edit = http(
        &bind,
        "GET",
        "/admin/posts/new",
        &[("Cookie", &cookie)],
        b"",
    )
    .text();
    assert!(
        edit.contains("data-fonts=\"[{&quot;name&quot;:&quot;회사 고딕&quot;,&quot;slug&quot;:&quot;font-1&quot;}]\""),
        "{edit}"
    );
    assert!(edit.contains("href=\"/assets/fonts.css\""));

    // 폼 단추: 본문 서체로 → 지우기 거절(409) → 되돌리고 지우기
    let form = |action: &str| {
        http(
            &bind,
            "POST",
            &format!("/admin/fonts/{slug}/{action}"),
            &[
                ("Cookie", &cookie),
                ("Content-Type", "application/x-www-form-urlencoded"),
            ],
            format!("_csrf={csrf}").as_bytes(),
        )
    };
    let res = form("use-body");
    assert_eq!(res.status, 303, "{}", res.text());
    let tokens = ok(&data, &["design", "show"]);
    assert_eq!(tokens["font"]["body"], "font-1");
    let res = form("remove");
    assert_eq!(res.status, 409, "{}", res.text());
    assert!(res.text().contains("font.body"));
    ok(&data, &["design", "set", "--set", "font.body=Pretendard"]);
    assert_eq!(form("remove").status, 303);
    assert_eq!(ok(&data, &["font", "list"]).as_array().unwrap().len(), 0);
}
