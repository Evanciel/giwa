//! GAP C11·C12: 리비전 비교·되돌리기, 글 이동·복사, 일괄 관리 (ADR-0055). 실제 바이너리·실제 포트·실제 DB 파일로
//! CLI 리비전 목록·비교·되돌리기 → DB, 이동하면 공개 주소가 바뀌고 옛 주소는 404, 복사는 초안,
//! 관리 화면 리비전 화면의 비교·되돌리기 POST → DB, 목록에서 두 글을 골라 다른 게시판으로 옮기기 → DB.
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

fn path_of(url: &str) -> String {
    let rest = url.split_once("://").unwrap().1;
    rest[rest.find('/').unwrap()..].to_owned()
}

/// 로그인 링크를 열고 확인 단추까지 눌러 세션 쿠키를 받는다.
fn sign_in_with_link(bind: &str, link_path: &str) -> String {
    let page = http(bind, "GET", link_path, &[], "");
    assert_eq!(page.status, 200, "{}", page.body);
    let pre = page.cookie("maru_csrf").unwrap();
    let cookie = format!("maru_csrf={pre}");
    let res = http(
        bind,
        "POST",
        link_path,
        &[FORM, ("Cookie", &cookie)],
        &form(&[("_csrf", &pre)]),
    );
    assert_eq!(res.status, 303, "{}", res.body);
    res.cookie("maru_session").unwrap()
}

fn board_of(data: &Path, id: &str) -> String {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(async {
        let db = maru_storage::Db::open(&data.join("maru.db"), false)
            .await
            .unwrap();
        let v: String = sqlx::query_scalar(
            "SELECT b.slug FROM posts p JOIN boards b ON b.id = p.board_id WHERE p.id = ?",
        )
        .bind(id)
        .fetch_one(db.read_pool())
        .await
        .unwrap();
        db.close().await;
        v
    })
}

fn new_post(data: &Path, title: &str, slug: &str, body: &str) -> String {
    let p = maru(
        data,
        &[
            "post",
            "new",
            "--title",
            title,
            "--slug",
            slug,
            "--body",
            body,
            "--publish",
        ],
    );
    p["id"].as_str().unwrap().to_owned()
}

#[test]
fn cli_revisions_move_copy_and_bulk() {
    let tmp = tempfile::tempdir().unwrap();
    let data = tmp.path().join("사이트");
    maru(&data, &["init", "--preset", "blog", "--yes"]);
    maru(
        &data,
        &[
            "board", "create", "free", "--name", "자유", "--kind", "free",
        ],
    );
    let id = new_post(&data, "처음 제목", "first", "하나\n둘\n셋");
    maru(
        &data,
        &[
            "post",
            "edit",
            &id,
            "--title",
            "고친 제목",
            "--body",
            "하나\n둘둘\n셋",
        ],
    );
    let revs = maru(&data, &["post", "revisions", &id]);
    let revs = revs.as_array().unwrap();
    assert_eq!(revs.len(), 2);
    let oldest = revs[1]["id"].as_str().unwrap().to_owned();
    let diff = maru(&data, &["post", "diff", &id, &oldest]);
    assert_eq!(diff["title_changed"], true);
    assert_eq!(diff["body"]["added"], 1);
    assert_eq!(diff["body"]["removed"], 1);
    let back = maru(&data, &["post", "revert", &id, &oldest]);
    assert_eq!(back["title"], "처음 제목");
    assert_eq!(
        maru(&data, &["post", "show", &id])["body_md"],
        "하나\n둘\n셋"
    );
    let wrong = run(&data, &["post", "revert", &id, "nope"]);
    assert_eq!(wrong["ok"], false);
    assert_eq!(wrong["error"]["code"], "NOT_FOUND");
    assert!(
        wrong["error"].to_string().contains("maru post revisions"),
        "{wrong}"
    );

    // 옮기면 주소가 새 게시판으로.
    let (_server, started) = start_server(&data);
    let bind = started["bind"].as_str().unwrap().to_owned();
    assert_eq!(http(&bind, "GET", "/b/blog/first", &[], "").status, 200);
    let moved = maru(&data, &["post", "move", &id, "--board", "free"]);
    assert_eq!(moved["id"], id.as_str());
    assert_eq!(board_of(&data, &id), "free");
    assert_eq!(http(&bind, "GET", "/b/free/first", &[], "").status, 200);
    assert_eq!(http(&bind, "GET", "/b/blog/first", &[], "").status, 404);

    // 복사는 초안.
    let copy = maru(&data, &["post", "copy", &id, "--board", "blog"]);
    assert_eq!(copy["status"], "draft");
    assert_ne!(copy["id"], id.as_str());

    // 일괄: 하나는 숨기고 초안은 그대로(실패로 알림).
    let draft = copy["id"].as_str().unwrap().to_owned();
    let bulk = maru(
        &data,
        &["post", "bulk", "hide", "--id", &id, "--id", &draft],
    );
    let items = bulk.as_array().unwrap();
    assert_eq!(items[0]["ok"], true);
    assert_eq!(items[1]["ok"], false);
    assert_eq!(items[1]["error"]["code"], "CONFLICT");
    assert_eq!(maru(&data, &["post", "show", &id])["status"], "hidden");
    let no_board = run(&data, &["post", "bulk", "move", "--id", &id]);
    assert_eq!(no_board["ok"], false);
}

#[test]
fn admin_revision_screen_reverts_and_list_moves_in_bulk() {
    let tmp = tempfile::tempdir().unwrap();
    let data = tmp.path().join("사이트");
    maru(&data, &["init", "--preset", "blog", "--yes"]);
    maru(
        &data,
        &[
            "board", "create", "free", "--name", "자유", "--kind", "free",
        ],
    );
    let a = new_post(&data, "가 글", "a", "옛 줄\n같은 줄");
    let b = new_post(&data, "나 글", "b", "본문");
    maru(&data, &["post", "edit", &a, "--body", "새 줄\n같은 줄"]);
    let (_server, started) = start_server(&data);
    let bind = started["bind"].as_str().unwrap().to_owned();
    let link = maru(&data, &["login-link"]);
    let session = sign_in_with_link(&bind, &path_of(link["url"].as_str().unwrap()));
    let cookie = format!("maru_session={session}");

    let edit = http(
        &bind,
        "GET",
        &format!("/admin/posts/{a}"),
        &[("Cookie", &cookie)],
        "",
    );
    assert!(
        edit.body
            .contains(&format!("href=\"/admin/posts/{a}/revisions\""))
    );
    assert!(edit.body.contains("name=\"how\" value=\"copy\""));
    // 기본으로 「지금」 바로 앞 리비전과 비교한다.
    let page = http(
        &bind,
        "GET",
        &format!("/admin/posts/{a}/revisions"),
        &[("Cookie", &cookie)],
        "",
    );
    assert_eq!(page.status, 200, "{}", page.body);
    assert!(page.body.contains("class=\"diff-del\""), "{}", page.body);
    assert!(page.body.contains("옛 줄"));
    assert!(page.body.contains("class=\"diff-add\""));
    let csrf = page.csrf();
    let at = page.body.find("/revert\"").unwrap();
    let start = page.body[..at].rfind("action=\"").unwrap() + 8;
    let action = format!("{}/revert", &page.body[start..at]);
    // CSRF 없으면 거부.
    let bad = http(&bind, "POST", &action, &[FORM, ("Cookie", &cookie)], "");
    assert_ne!(bad.status, 303);
    let res = http(
        &bind,
        "POST",
        &action,
        &[FORM, ("Cookie", &cookie)],
        &form(&[("_csrf", &csrf)]),
    );
    assert_eq!(res.status, 303, "{}", res.body);
    assert!(res.header("location").unwrap().ends_with("?done=reverted"));
    assert_eq!(
        maru(&data, &["post", "show", &a])["body_md"],
        "옛 줄\n같은 줄"
    );

    // 목록에서 두 글을 골라 옮긴다.
    let home = http(&bind, "GET", "/admin", &[("Cookie", &cookie)], "");
    assert!(home.body.contains(&format!("name=\"pick.{a}\"")));
    let pick_a = format!("pick.{a}");
    let pick_b = format!("pick.{b}");
    let res = http(
        &bind,
        "POST",
        "/admin/posts/bulk",
        &[FORM, ("Cookie", &cookie)],
        &form(&[
            ("_csrf", &home.csrf()),
            ("action", "move"),
            ("board", "free"),
            (&pick_a, "1"),
            (&pick_b, "1"),
        ]),
    );
    assert_eq!(res.status, 303, "{}", res.body);
    let to = res.header("location").unwrap();
    assert!(to.contains("done=bulk&ok=2&failed=0"), "{to}");
    assert_eq!(board_of(&data, &a), "free");
    assert_eq!(board_of(&data, &b), "free");
    let after = http(&bind, "GET", &to, &[("Cookie", &cookie)], "");
    assert!(after.body.contains("2개를 처리했습니다."));
}
