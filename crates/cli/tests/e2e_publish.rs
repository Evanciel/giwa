//! E2E-1 (PHASE1_PLAN §8): init → REST 로 한글 초안 → 공개 URL 404 → CLI 발행 → 실행 중인 서버에 노출
//! → DB 를 직접 읽어 상태·리비전·감사 로그의 Actor 를 대조한다.
#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::path::Path;
use std::process::{Child, Command, Stdio};

use serde_json::Value;

const BIN: &str = env!("CARGO_BIN_EXE_maru");

/// 테스트가 실패해도 서버 프로세스를 남기지 않는다.
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
    let v: Value = serde_json::from_str(stdout.trim()).unwrap_or_else(|e| {
        panic!(
            "JSON 아님: {e}\nstdout: {stdout}\nstderr: {}",
            String::from_utf8_lossy(&out.stderr)
        )
    });
    assert_eq!(v["ok"], true, "{v}");
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
    let bind = v["data"]["bind"].as_str().unwrap().to_owned();
    (server, bind)
}

/// 최소 HTTP/1.1 클라이언트. 응답 상태와 본문을 돌려준다.
fn http(
    bind: &str,
    method: &str,
    path: &str,
    headers: &[(&str, &str)],
    body: &str,
) -> (u16, String) {
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
    let status: u16 = text[9..12].parse().unwrap();
    let body = text
        .split_once("\r\n\r\n")
        .map(|(_, b)| b.to_owned())
        .unwrap_or_default();
    (status, body)
}

fn encode(segment: &str) -> String {
    segment
        .bytes()
        .map(|b| {
            if b.is_ascii_alphanumeric() || b == b'-' {
                (b as char).to_string()
            } else {
                format!("%{b:02X}")
            }
        })
        .collect()
}

#[test]
fn e2e1_init_rest_draft_cli_publish_public_page() {
    let tmp = tempfile::tempdir().unwrap();
    let data = tmp.path().join("내 블로그");

    // 1. init
    let init = maru(&data, &["init", "--preset", "blog", "--yes"]);
    assert_eq!(init["created"], true);
    let token = init["token"]["secret"].as_str().unwrap().to_owned();
    assert!(token.starts_with("maru_"));
    // 다시 실행해도 안전하고 토큰이 늘지 않는다.
    let again = maru(&data, &["init", "--preset", "blog", "--yes"]);
    assert_eq!(again["created"], false);
    assert!(again["token"].is_null());

    let (_server, bind) = start_server(&data);

    // 2. 발급된 토큰으로 REST 초안
    let auth = format!("Bearer {token}");
    let body = r#"{"board":"blog","title":"러스트로 만든 블로그","body_md":"안녕하세요, **마루**입니다."}"#;
    let (status, res) = http(
        &bind,
        "POST",
        "/api/v1/posts",
        &[
            ("Authorization", &auth),
            ("Content-Type", "application/json"),
        ],
        body,
    );
    assert_eq!(status, 201, "{res}");
    let created: Value = serde_json::from_str(&res).unwrap();
    let id = created["data"]["id"].as_str().unwrap().to_owned();
    let slug = created["data"]["slug"].as_str().unwrap().to_owned();
    assert_eq!(created["data"]["status"], "draft");
    let public_path = format!("/b/blog/{}", encode(&slug));

    // 잘못된 토큰은 401, 기본 토큰은 발행 403
    let (status, _) = http(
        &bind,
        "POST",
        "/api/v1/posts",
        &[("Authorization", "Bearer maru_nope")],
        body,
    );
    assert_eq!(status, 401);
    let (status, res) = http(
        &bind,
        "POST",
        &format!("/api/v1/posts/{id}/publish"),
        &[("Authorization", &auth)],
        "",
    );
    assert_eq!(status, 403, "{res}");
    assert!(res.contains("\"FORBIDDEN\""));

    // 3. 초안의 공개 URL 은 404
    let (status, _) = http(&bind, "GET", &public_path, &[], "");
    assert_eq!(status, 404);

    // 4. CLI 로 발행 (서버 실행 중, 다른 프로세스)
    let published = maru(&data, &["post", "publish", &id]);
    assert_eq!(published["status"], "published");
    assert!(published["url"].as_str().unwrap().ends_with(&public_path));

    // 5. 실행 중인 서버에 바로 보인다 (프로세스 캐시 없음)
    let (status, html) = http(&bind, "GET", &public_path, &[], "");
    assert_eq!(status, 200);
    assert!(html.contains("러스트로 만든 블로그"));
    assert!(html.contains("<strong>마루</strong>"));
    let (status, home) = http(&bind, "GET", "/", &[], "");
    assert_eq!(status, 200);
    assert!(home.contains("러스트로 만든 블로그"));

    // 6. DB 직접 대조
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(async {
        let db = maru_storage::Db::open(&data.join("maru.db"), false).await.unwrap();
        let status: String = sqlx::query_scalar("SELECT status FROM posts WHERE id = ?")
            .bind(&id)
            .fetch_one(db.read_pool())
            .await
            .unwrap();
        assert_eq!(status, "published");
        let revisions: i64 = sqlx::query_scalar("SELECT count(*) FROM revisions WHERE post_id = ?")
            .bind(&id)
            .fetch_one(db.read_pool())
            .await
            .unwrap();
        assert_eq!(revisions, 1);
        let token_id = init["token"]["id"].as_str().unwrap().to_owned();
        let audits: Vec<(String, Option<String>, String)> = sqlx::query_as(
            "SELECT actor_type, actor_id, action FROM audit_log WHERE target_id = ? ORDER BY at, id",
        )
        .bind(&id)
        .fetch_all(db.read_pool())
        .await
        .unwrap();
        assert_eq!(
            audits,
            vec![
                ("agent".into(), Some(token_id), "post.create".into()),
                ("local".into(), None, "post.publish".into()),
            ]
        );
        db.close().await;
    });
}

#[test]
fn serve_reports_port_in_use() {
    let tmp = tempfile::tempdir().unwrap();
    let data = tmp.path().join("site");
    maru(&data, &["init", "--yes"]);
    let busy = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = busy.local_addr().unwrap().port();
    let out = Command::new(BIN)
        .arg("--data")
        .arg(&data)
        .args(["serve", "--bind", &format!("127.0.0.1:{port}"), "--json"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(4));
    let v: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v["error"]["code"], "PORT_IN_USE");
    assert!(v["error"]["next"][0].as_str().unwrap().contains("--bind"));
}

#[test]
fn uninitialized_dir_points_to_init() {
    let tmp = tempfile::tempdir().unwrap();
    let out = Command::new(BIN)
        .arg("--data")
        .arg(tmp.path().join("없음"))
        .args(["post", "list", "--json"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(1));
    let v: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v["error"]["code"], "NOT_FOUND");
    assert!(
        v["error"]["next"][0]
            .as_str()
            .unwrap()
            .starts_with("maru init")
    );
}

#[test]
fn cli_post_lifecycle_and_input_paths() {
    let tmp = tempfile::tempdir().unwrap();
    let data = tmp.path().join("site");
    maru(&data, &["init", "--yes"]);

    // UTF-8 BOM 이 붙은 파일
    let md = tmp.path().join("글.md");
    std::fs::write(&md, "\u{feff}# 제목\n\n한글 본문").unwrap();
    let p = maru(
        &data,
        &[
            "post",
            "new",
            "--title",
            "파일 글",
            "--from",
            md.to_str().unwrap(),
        ],
    );
    assert_eq!(p["body_md"], "# 제목\n\n한글 본문");
    let id = p["id"].as_str().unwrap().to_owned();

    // UTF-8 이 아닌 파일은 원인과 함께 거부
    let bad = tmp.path().join("cp949.md");
    std::fs::write(&bad, [0xC7u8, 0xD1, 0xB1, 0xDB]).unwrap();
    let out = Command::new(BIN)
        .arg("--data")
        .arg(&data)
        .args([
            "post",
            "new",
            "--title",
            "x",
            "--from",
            bad.to_str().unwrap(),
            "--json",
        ])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(2));
    let v: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v["error"]["code"], "VALIDATION");
    assert!(v["error"]["hint"].as_str().unwrap().contains("UTF-8"));

    // 표준 입력
    let mut child = Command::new(BIN)
        .arg("--data")
        .arg(&data)
        .args(["post", "new", "--title", "입력 글", "--from", "-", "--json"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all("표준 입력 본문".as_bytes())
        .unwrap();
    let out = child.wait_with_output().unwrap();
    let v: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v["data"]["body_md"], "표준 입력 본문");

    // 고치기 → 발행 → 삭제 → 되돌리기
    let edited = maru(&data, &["post", "edit", &id, "--title", "고친 제목"]);
    assert_eq!(edited["title"], "고친 제목");
    maru(&data, &["post", "publish", &id]);
    assert_eq!(maru(&data, &["post", "delete", &id])["status"], "deleted");
    assert_eq!(
        maru(&data, &["post", "restore", &id])["status"],
        "published"
    );

    // --if-not-exists 는 여러 번 실행해도 글이 하나
    let args = [
        "post",
        "new",
        "--title",
        "소개",
        "--slug",
        "about",
        "--if-not-exists",
    ];
    assert_eq!(maru(&data, &args)["created"], true);
    assert_eq!(maru(&data, &args)["created"], false);
    let b = [
        "board",
        "create",
        "notice",
        "--name",
        "공지",
        "--kind",
        "notice",
        "--if-not-exists",
    ];
    assert_eq!(maru(&data, &b)["created"], true);
    assert_eq!(maru(&data, &b)["created"], false);
    let list = maru(&data, &["board", "list"]);
    assert_eq!(list.as_array().unwrap().len(), 2);
}

/// U9: 페이지·메뉴·사이트 설정이 CLI 로 들어가 공개 화면과 REST 에 그대로 보인다.
#[test]
fn site_pages_and_menus_show_on_public_pages() {
    let tmp = tempfile::tempdir().unwrap();
    let data = tmp.path().join("site");
    let init = maru(&data, &["init", "--yes", "--name", "테스트 블로그"]);
    let token = init["token"]["secret"].as_str().unwrap().to_owned();
    // init 이 기본 헤더 메뉴를 깐다.
    let menu = maru(&data, &["menu", "show", "header"]);
    assert_eq!(menu[0]["link"]["kind"], "board");

    // blog 프리셋이 만든 소개 초안을 고쳐서 공개한다.
    maru(
        &data,
        &["page", "edit", "about", "--body", "**안녕하세요**"],
    );
    let about = maru(&data, &["page", "publish", "about"]);
    assert_eq!(about["status"], "published");
    assert!(about["url"].as_str().unwrap().ends_with("/p/about"));
    maru(&data, &["page", "new", "secret", "--title", "비공개"]);
    // 다시 실행해도 안전
    let again = maru(
        &data,
        &["page", "new", "about", "--title", "소개", "--if-not-exists"],
    );
    assert_eq!(again["created"], false);

    maru(
        &data,
        &[
            "menu",
            "set",
            "header",
            "--item",
            "글=board:blog",
            "--item",
            "글>소개=page:about",
            "--item",
            "깃허브=https://github.com",
        ],
    );
    let s = maru(
        &data,
        &[
            "site",
            "set",
            "--description",
            "개발 기록",
            "--posts-per-page",
            "5",
        ],
    );
    assert_eq!(s["name"], "테스트 블로그");
    assert_eq!(s["posts_per_page"], 5);

    let (_server, bind) = start_server(&data);
    let (status, home) = http(&bind, "GET", "/", &[], "");
    assert_eq!(status, 200);
    assert!(home.contains("<title>테스트 블로그</title>"), "{home}");
    assert!(home.contains("개발 기록"));
    assert!(home.contains(r#"href="/p/about""#), "{home}");
    assert!(home.contains(r#"href="https://github.com" rel="noopener noreferrer""#));

    let (status, page) = http(&bind, "GET", "/p/about", &[], "");
    assert_eq!(status, 200);
    assert!(page.contains("<strong>안녕하세요</strong>"));
    let (status, _) = http(&bind, "GET", "/p/secret", &[], "");
    assert_eq!(status, 404, "초안 페이지는 공개되지 않는다");

    let (status, body) = http(&bind, "GET", "/api/v1/settings", &[], "");
    assert_eq!(status, 200);
    assert!(body.contains("테스트 블로그"));
    // 기본 토큰(read, draft)은 메뉴를 못 바꾼다.
    let auth = format!("Bearer {token}");
    let (status, _) = http(
        &bind,
        "PUT",
        "/api/v1/menus/header",
        &[
            ("Authorization", &auth),
            ("Content-Type", "application/json"),
        ],
        "[]",
    );
    assert_eq!(status, 403);
}

/// U10: 프리셋으로 init → 토큰 CSS 가 공개되고, CLI 로 바꾼 토큰이 바로 반영된다.
#[test]
fn design_tokens_flow_to_css() {
    let tmp = tempfile::tempdir().unwrap();
    let data = tmp.path().join("site");
    let init = maru(&data, &["init", "--yes"]);
    assert_eq!(init["preset"], "blog");
    // 블로그 프리셋은 소개 페이지를 초안으로 만든다.
    let pages = maru(&data, &["page", "list"]);
    assert_eq!(pages[0]["slug"], "about");
    assert_eq!(pages[0]["status"], "draft");
    // 다시 init 해도 아무것도 바뀌지 않는다.
    let again = maru(&data, &["init", "--yes"]);
    assert!(
        again["applied"]
            .as_array()
            .unwrap()
            .iter()
            .all(|r| r["changed"] == false)
    );

    let t = maru(
        &data,
        &[
            "design",
            "set",
            "--set",
            "color.primary=#1f4f8f",
            "--set",
            "type_scale.base=18",
        ],
    );
    assert_eq!(t["color"]["primary"], "#1f4f8f");

    // 대비가 부족하면 VALIDATION 으로 거부된다.
    let out = Command::new(BIN)
        .arg("--data")
        .arg(&data)
        .args(["design", "set", "--set", "color.muted=#eeeeee", "--json"])
        .output()
        .unwrap();
    assert!(!out.status.success());
    let v: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v["error"]["code"], "VALIDATION");

    let (_server, bind) = start_server(&data);
    let (status, css) = http(&bind, "GET", "/assets/tokens.css", &[], "");
    assert_eq!(status, 200);
    assert!(css.contains("--primary: #1f4f8f;"), "{css}");
    assert!(css.contains("--fs-base: 1.125rem;"));
    let (_, home) = http(&bind, "GET", "/", &[], "");
    assert!(home.contains(r#"href="/assets/tokens.css""#));
    let (status, _) = http(&bind, "GET", "/assets/site.css", &[], "");
    assert_eq!(status, 200);
}

/// U11: 사이트맵·RSS·robots·메타가 공개 글과 함께 움직이고, 숨기면 동시에 빠진다.
#[test]
fn seo_outputs_follow_public_state() {
    let tmp = tempfile::tempdir().unwrap();
    let data = tmp.path().join("site");
    maru(
        &data,
        &[
            "init",
            "--yes",
            "--name",
            "개발 노트",
            "--site-url",
            "https://blog.example.test",
        ],
    );
    maru(
        &data,
        &[
            "site",
            "set",
            "--description",
            "러스트와 웹",
            "--naver-verification",
            "abc123",
        ],
    );
    let post = maru(
        &data,
        &[
            "post",
            "new",
            "--title",
            "CDATA 시험",
            "--slug",
            "cdata",
            "--body",
            "본문 ]]> <script>alert(1)</script> **굵게**",
            "--publish",
        ],
    );
    let id = post["id"].as_str().unwrap().to_owned();
    let url = "https://blog.example.test/b/blog/cdata";
    assert_eq!(
        post["url"], url,
        "CLI 출력 URL 과 사이트맵 URL 이 같아야 한다"
    );

    let (_server, bind) = start_server(&data);
    let (status, sitemap) = http(&bind, "GET", "/sitemap.xml", &[], "");
    assert_eq!(status, 200);
    assert!(sitemap.starts_with("<?xml"));
    assert!(sitemap.contains(&format!("<loc>{url}</loc>")), "{sitemap}");
    assert!(sitemap.contains("<loc>https://blog.example.test/</loc>"));
    assert!(!sitemap.contains("/p/about"), "초안 페이지는 빠진다");

    let (status, rss) = http(&bind, "GET", "/rss.xml", &[], "");
    assert_eq!(status, 200);
    assert!(rss.contains("<title>개발 노트</title>"));
    assert!(rss.contains(&format!("<link>{url}</link>")));
    assert!(rss.contains("<pubDate>"));
    assert!(!rss.contains("<script>"), "본문은 정화된 HTML 만");
    // 본문의 `]]>` 가 CDATA 를 일찍 닫지 않는다 (렌더러가 `&gt;` 로 바꾸고, 남으면 cdata() 가 쪼갠다).
    assert!(!rss.contains("]]> &lt;script"), "{rss}");
    assert_eq!(rss.matches("<description><![CDATA[").count(), 1);

    let (status, robots) = http(&bind, "GET", "/robots.txt", &[], "");
    assert_eq!(status, 200);
    assert!(robots.contains("Sitemap: https://blog.example.test/sitemap.xml"));

    let (_, page) = http(&bind, "GET", "/b/blog/cdata", &[], "");
    assert!(
        page.contains("<title>CDATA 시험 · 개발 노트</title>"),
        "{page}"
    );
    assert!(page.contains(&format!(r#"<link rel="canonical" href="{url}">"#)));
    assert!(page.contains(r#"<meta property="og:type" content="article">"#));
    assert!(page.contains(r#"<meta name="naver-site-verification" content="abc123">"#));
    let (status, missing) = http(&bind, "GET", "/nope", &[], "");
    assert_eq!(status, 404);
    assert!(missing.contains(r#"<meta name="robots" content="noindex">"#));

    // 숨기면 화면·사이트맵·RSS 에서 함께 빠진다 (서버를 다시 띄우지 않아도).
    maru(&data, &["post", "hide", &id]);
    let (status, _) = http(&bind, "GET", "/b/blog/cdata", &[], "");
    assert_eq!(status, 404);
    let (_, sitemap) = http(&bind, "GET", "/sitemap.xml", &[], "");
    assert!(!sitemap.contains(url));
    let (_, rss) = http(&bind, "GET", "/rss.xml", &[], "");
    assert!(!rss.contains(url));
}
