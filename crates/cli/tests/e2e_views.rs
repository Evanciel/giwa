//! GAP C6·C7: 조회수, 이전·다음 글, 관련 글 (ADR-0049). 실제 바이너리·실제 포트·실제 DB 파일로
//! 같은 사람 다시 보기·봇·운영자를 세지 않는지, 모은 수가 DB 에 닿는지, 화면·REST·CLI 가 같은 답을 주는지 본다.
#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use serde_json::Value;

const BIN: &str = env!("CARGO_BIN_EXE_giwa");
const CHROME: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) Chrome/130.0";
const FIREFOX: &str = "Mozilla/5.0 (Windows NT 10.0; rv:131.0) Gecko/20100101 Firefox/131.0";

struct Server(Child);

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn giwa(data: &Path, args: &[&str]) -> Value {
    let out = Command::new(BIN)
        .arg("--data")
        .arg(data)
        .args(args)
        .arg("--json")
        .output()
        .expect("giwa 실행");
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

/// 글 화면을 연다. 조회수 표시(`조회 N`)를 돌려준다(없으면 None).
fn open_post(bind: &str, path: &str, headers: &[(&str, &str)]) -> (Res, Option<i64>) {
    let res = http(bind, "GET", path, headers, "");
    assert_eq!(res.status, 200, "{}", res.body);
    let shown = res.body.find("class=\"views\">조회 ").map(|at| {
        let rest = &res.body[at + "class=\"views\">조회 ".len()..];
        rest[..rest.find('<').unwrap()].parse().unwrap()
    });
    (res, shown)
}

/// 로그인 링크를 열고 확인 버튼까지 눌러 세션 쿠키를 받는다.
fn sign_in(bind: &str, link_url: &str) -> String {
    let rest = link_url.split_once("://").unwrap().1;
    let path = &rest[rest.find('/').unwrap()..];
    let page = http(bind, "GET", path, &[], "");
    assert_eq!(page.status, 200, "{}", page.body);
    let csrf = page.cookie("giwa_csrf").unwrap();
    let cookie = format!("giwa_csrf={csrf}");
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
    res.cookie("giwa_session").unwrap()
}

fn db_views(data: &Path, slug: &str) -> i64 {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(async {
        let db = giwa_storage::Db::open(&data.join("giwa.db"), false)
            .await
            .unwrap();
        let v: i64 = sqlx::query_scalar("SELECT view_count FROM posts WHERE slug = ?")
            .bind(slug)
            .fetch_one(db.read_pool())
            .await
            .unwrap();
        db.close().await;
        v
    })
}

#[test]
fn views_count_people_once_and_reach_db_and_neighbors_agree() {
    let tmp = tempfile::tempdir().unwrap();
    let data = tmp.path().join("사이트");
    let init = giwa(
        &data,
        &["init", "--no-samples", "--preset", "blog", "--yes"],
    );
    let token = init["token"]["secret"].as_str().unwrap().to_owned();
    let bearer = format!("Bearer {token}");
    let link = giwa(&data, &["login-link"]);

    let new = |title: &str, slug: &str, tag: Option<&str>| {
        let mut args = vec!["post", "new", "--title", title, "--slug", slug, "--publish"];
        if let Some(t) = tag {
            args.extend(["--tag", t]);
        }
        giwa(&data, &args)
    };
    new("하나", "one", Some("러스트"));
    let two = new("둘", "two", Some("러스트"));
    new("셋", "three", None);
    let two_id = two["id"].as_str().unwrap().to_owned();

    let (_server, bind) = start_server(&data);
    let path = "/b/blog/two";

    // 같은 사람은 한 번, 다른 브라우저는 따로.
    let (page, shown) = open_post(&bind, path, &[("User-Agent", CHROME)]);
    assert_eq!(shown, Some(1));
    assert_eq!(
        open_post(&bind, path, &[("User-Agent", CHROME)]).1,
        Some(1),
        "새로고침"
    );
    assert_eq!(
        open_post(&bind, path, &[("User-Agent", FIREFOX)]).1,
        Some(2)
    );
    // 봇·User-Agent 없음·HEAD 는 세지 않는다.
    assert_eq!(
        open_post(
            &bind,
            path,
            &[("User-Agent", "Mozilla/5.0 (compatible; Googlebot/2.1)")]
        )
        .1,
        Some(2)
    );
    assert_eq!(open_post(&bind, path, &[]).1, Some(2));
    let head = http(
        &bind,
        "HEAD",
        path,
        &[("User-Agent", "Mozilla/5.0 Safari/605")],
        "",
    );
    assert_eq!(head.status, 200);
    // 로그인한 운영자도 세지 않는다.
    let session = sign_in(&bind, link["url"].as_str().unwrap());
    let cookie = format!("giwa_session={session}");
    let staff = [
        ("User-Agent", "Mozilla/5.0 Edg/130.0"),
        ("Cookie", cookie.as_str()),
    ];
    assert_eq!(open_post(&bind, path, &staff).1, Some(2));

    // 이전(더 오래된)·다음(더 새) 글과 관련 글.
    let nav = &page.body[page
        .body
        .find("class=\"post-nav\"")
        .expect("이전·다음 글 줄")..];
    assert!(
        nav.contains("href=\"/b/blog/one\"") && nav.contains("이전 글"),
        "{nav}"
    );
    assert!(
        nav.contains("href=\"/b/blog/three\"") && nav.contains("다음 글"),
        "{nav}"
    );
    let related = &page.body[page.body.find("class=\"related\"").expect("관련 글")..];
    let one_at = related.find("/b/blog/one").expect("태그가 겹치는 글");
    let three_at = related.find("/b/blog/three").expect("같은 게시판 글");
    assert!(one_at < three_at, "태그가 겹치는 글이 먼저");

    // REST: 아직 DB 에 안 닿은 수까지 글 화면과 같은 값, 이웃도 같다.
    let res = http(
        &bind,
        "GET",
        &format!("/api/v1/posts/{two_id}"),
        &[("Authorization", &bearer)],
        "",
    );
    assert_eq!(res.status, 200, "{}", res.body);
    assert_eq!(res.json()["data"]["view_count"], 2);
    let res = http(
        &bind,
        "GET",
        "/api/v1/boards/blog/posts/two/nearby",
        &[("Authorization", &bearer)],
        "",
    );
    assert_eq!(res.status, 200, "{}", res.body);
    let near = res.json()["data"].clone();
    assert_eq!(near["prev"]["slug"], "one");
    assert_eq!(near["next"]["slug"], "three");
    assert_eq!(near["related"][0]["slug"], "one");
    // CLI 도 같은 서비스로 같은 답.
    let cli = giwa(&data, &["post", "nearby", "blog", "two"]);
    assert_eq!(cli["prev"]["slug"], near["prev"]["slug"]);
    assert_eq!(cli["next"]["slug"], near["next"]["slug"]);
    assert_eq!(cli["related"], near["related"]);

    // 모은 수는 주기(20초)마다 DB 에 더해진다.
    let started = Instant::now();
    while db_views(&data, "two") != 2 {
        assert!(
            started.elapsed() < Duration::from_secs(45),
            "조회수가 DB 에 닿지 않음"
        );
        std::thread::sleep(Duration::from_millis(500));
    }
    assert_eq!(db_views(&data, "one"), 0, "열지 않은 글은 그대로");

    // 목록에도 보이고, 설정으로 숨기면 공개 화면에서만 사라진다(세기는 계속).
    let list = http(&bind, "GET", "/b/blog", &[("User-Agent", CHROME)], "");
    assert!(
        list.body.contains("class=\"views\">조회 2<"),
        "{}",
        list.body
    );
    let s = giwa(&data, &["site", "set", "--show-views", "false"]);
    assert_eq!(s["show_views"], false);
    let (_, hidden) = open_post(&bind, "/b/blog/one", &[("User-Agent", CHROME)]);
    assert_eq!(hidden, None);
    let list = http(&bind, "GET", "/b/blog", &[("User-Agent", CHROME)], "");
    assert!(!list.body.contains("class=\"views\""));
    let res = http(
        &bind,
        "GET",
        "/api/v1/boards/blog/posts/one/nearby",
        &[("Authorization", &bearer)],
        "",
    );
    assert_eq!(res.status, 200);

    // 운영자가 조회수를 고친다: REST 는 관리 권한 없는 토큰을 거절, CLI(로컬)는 된다.
    let res = http(
        &bind,
        "PUT",
        &format!("/api/v1/posts/{two_id}/views"),
        &[
            ("Authorization", &bearer),
            ("Content-Type", "application/json"),
        ],
        r#"{"view_count":0}"#,
    );
    assert_eq!(res.status, 403, "{}", res.body);
    let fixed = giwa(&data, &["post", "views", &two_id, "--set", "0"]);
    assert_eq!(fixed["view_count"], 0);
    assert_eq!(db_views(&data, "two"), 0);

    // 글 아래 게시판 목록 (GAP Q19, ADR-0104): 켜면 첫 쪽 목록이 붙고 이전·다음 글 줄은 숨는다.
    let page = http(&bind, "GET", path, &[], "").body;
    assert!(!page.contains("class=\"board-under\""), "기본은 끔");
    assert!(page.contains("class=\"post-back\""), "휴대폰 뒤로가기 줄");
    let s = giwa(&data, &["site", "set", "--list-under-post", "true"]);
    assert_eq!(s["list_under_post"], true, "{s}");
    giwa(&data, &["board", "update", "blog", "--layout", "list"]);
    let page = http(&bind, "GET", path, &[], "").body;
    let under = &page[page.find("class=\"board-under\"").expect("글 아래 목록")..];
    assert!(
        under.contains("href=\"/b/blog/one\"") && under.contains("href=\"/b/blog/three\""),
        "{under}"
    );
    let current = &under[under.find("aria-current=\"page\"").expect("지금 글 표시")..];
    assert!(
        current[..current.find("</li>").unwrap()].contains("href=\"/b/blog/two\""),
        "{current}"
    );
    assert!(!page.contains("class=\"post-nav\""), "{page}");
    assert!(page.contains("class=\"related\""), "관련 글은 그대로");
    // 떠 있는 단추에 공유(스크립트가 보인다) (GAP Q20, ADR-0105)
    let float = &page[page.find("class=\"float-nav\"").expect("떠 있는 단추")..];
    assert!(
        float.contains("<button type=\"button\" data-share hidden>공유</button>"),
        "{float}"
    );
    // 글 머리 공유 아이콘 (GAP Q30): 휴대폰에서만 보이는 같은 단추.
    assert!(
        page.contains("class=\"head-share\" data-share hidden aria-label=\"공유\""),
        "{float}"
    );
    // 갤러리 칸: 제목·글쓴이·수치가 그림 위 덮개 안에
    giwa(&data, &["board", "update", "blog", "--layout", "gallery"]);
    let list = http(&bind, "GET", "/b/blog", &[], "").body;
    let cap = &list[list.find("class=\"tile-cap\"").expect("덮개")..];
    assert!(
        cap.contains("<h2>") && cap.contains("class=\"meta\""),
        "{cap}"
    );
    // 빠른 보기 주소(?quick=)는 목록을 그대로 준다
    let quick = http(&bind, "GET", "/b/blog?quick=two", &[], "");
    assert_eq!(quick.status, 200);
}
