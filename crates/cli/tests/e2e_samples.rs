//! 예시 콘텐츠 (ADR-0142). 실제 바이너리·실제 포트·실제 DB 파일로 사이트 종류를 고르면
//! 빈 게시판에 예시 글과 그림이 들어가 첫 화면이 채워지고, 고치지 않은 예시만 한 번에 지워지며,
//! 변경 묶음으로 넣은 예시는 되돌리기로 깨끗이 빠지는지 본다.
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

fn items(v: &Value) -> Vec<Value> {
    v.get("items")
        .unwrap_or(v)
        .as_array()
        .cloned()
        .unwrap_or_default()
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

fn get(bind: &str, path: &str) -> (u16, String) {
    let mut s = TcpStream::connect(bind).unwrap();
    s.write_all(
        format!("GET {path} HTTP/1.1\r\nHost: {bind}\r\nConnection: close\r\n\r\n").as_bytes(),
    )
    .unwrap();
    let mut out = Vec::new();
    s.read_to_end(&mut out).unwrap();
    let text = String::from_utf8_lossy(&out).into_owned();
    let (head, body) = text.split_once("\r\n\r\n").unwrap_or((&text, ""));
    (head[9..12].parse().unwrap(), body.to_owned())
}

fn post_id(data: &Path, board: &str, title: &str) -> String {
    let list = maru(data, &["post", "list", "--board", board]);
    items(&list)
        .into_iter()
        .find(|p| p["title"] == title)
        .unwrap_or_else(|| panic!("{title} 없음: {list}"))["id"]
        .as_str()
        .unwrap()
        .to_owned()
}

#[test]
fn 예시_글이_첫_화면을_채우고_고치지_않은_것만_한_번에_지워진다() {
    let tmp = tempfile::tempdir().unwrap();
    let data = tmp.path().join("site");
    maru(&data, &["init", "--preset", "shop", "--yes"]);

    let s = maru(&data, &["sample", "status"]);
    assert_eq!(s["posts"], 17, "{s}");
    assert_eq!(s["untouched"], 17, "{s}");
    let media = maru(&data, &["media", "list"]);
    assert_eq!(media["total"], 13, "상품 8·이야기 3·후기 2 그림: {media}");

    {
        let (_server, bind) = start_server(&data);
        let (code, home) = get(&bind, "/");
        assert_eq!(code, 200);
        assert!(home.contains("린넨 셔츠 오트밀"), "첫 화면 상품 칸");
        assert!(home.contains("/media/"), "예시 그림");
        assert!(home.contains("원두를 볶는 아침"), "룩북·이야기 칸");
        let (_, list) = get(&bind, "/b/products");
        let first = list.find("린넨 셔츠 오트밀").expect("첫 상품");
        let last = list.find("유리 물병 1L").expect("마지막 상품");
        assert!(first < last, "파일의 첫 글이 가장 새 글");
        let (_, sale) = get(&bind, "/b/products?f.category=sale");
        assert!(sale.contains("면 에코백 큰 크기") && !sale.contains("린넨 셔츠 오트밀"));
        let (_, reviews) = get(&bind, "/b/reviews");
        assert!(
            reviews.contains("예시 후기:"),
            "후기 예시는 예시라고 밝힌다"
        );
    }

    let linen = post_id(&data, "products", "린넨 셔츠 오트밀");
    maru(
        &data,
        &["post", "edit", &linen, "--title", "린넨 셔츠 우리 가게"],
    );
    let s = maru(&data, &["sample", "status"]);
    assert_eq!(
        (s["posts"].clone(), s["untouched"].clone()),
        (17.into(), 16.into())
    );

    let refused = run(&data, &["sample", "clear"]);
    assert_eq!(
        refused["ok"], false,
        "--yes 없이는 지우지 않는다: {refused}"
    );
    assert!(refused["error"]["next"].to_string().contains("--yes"));

    let r = maru(&data, &["sample", "clear", "--yes"]);
    assert_eq!(r["removed"], 16, "{r}");
    assert_eq!(r["kept"], 1, "{r}");
    assert_eq!(r["media_removed"], 12, "고친 글의 그림은 남긴다: {r}");
    let left = maru(&data, &["post", "list"]);
    assert_eq!(left["total"], 1, "{left}");
    assert_eq!(maru(&data, &["media", "list"])["total"], 1);
    assert_eq!(maru(&data, &["sample", "status"])["posts"], 0);

    // 지운 뒤 사이트 종류를 다시 적용해도 예시가 돌아오지 않는다.
    maru(&data, &["init", "--preset", "shop", "--yes"]);
    assert_eq!(maru(&data, &["post", "list"])["total"], 1);
    assert_eq!(maru(&data, &["sample", "status"])["posts"], 0);
}

#[test]
fn 변경_묶음으로_넣은_예시는_되돌리면_게시판과_함께_빠진다() {
    let tmp = tempfile::tempdir().unwrap();
    let data = tmp.path().join("site");
    maru(
        &data,
        &["init", "--no-samples", "--preset", "blog", "--yes"],
    );
    assert_eq!(maru(&data, &["post", "list"])["total"], 0);

    let ops = r#"[
      {"op":"preset.step","step":{"op":"board.ensure","slug":"trial","name":"시험 게시판","kind":"gallery"}},
      {"op":"preset.step","step":{"op":"posts.ensure","board":"trial","items":[
        {"title":"예시 하나","image":"desk","body_md":"본문"},
        {"title":"예시 둘","image":"landscape"}
      ]}}
    ]"#;
    let cs = maru(
        &data,
        &["changeset", "new", "--title", "예시 시험", "--ops", ops],
    );
    let id = cs["id"].as_str().unwrap().to_owned();
    maru(&data, &["changeset", "apply", &id, "--yes"]);
    assert_eq!(
        maru(&data, &["post", "list", "--board", "trial"])["total"],
        2
    );
    assert_eq!(maru(&data, &["sample", "status"])["posts"], 2);
    assert_eq!(maru(&data, &["media", "list"])["total"], 2);

    maru(&data, &["changeset", "revert", &id, "--yes"]);
    let boards = maru(&data, &["board", "list"]);
    assert!(
        !items(&boards).iter().any(|b| b["slug"] == "trial"),
        "예시 글이 있어도 게시판까지 되돌린다: {boards}"
    );
    assert_eq!(maru(&data, &["sample", "status"])["posts"], 0);
    assert_eq!(maru(&data, &["media", "list"])["total"], 0);

    // 사본에서 그린 그림은 사본과 함께 지워져 미디어 폴더에 남지 않는다.
    let previews = data.join("previews");
    if previews.exists() {
        let left: Vec<_> = std::fs::read_dir(&previews)
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| n.ends_with("-media"))
            .collect();
        assert!(left.is_empty(), "{left:?}");
    }
    let files = walk(&data.join("media"));
    assert!(files.is_empty(), "되돌린 뒤 남은 그림 파일: {files:?}");
}

fn walk(dir: &Path) -> Vec<String> {
    let mut out = Vec::new();
    let Ok(rd) = std::fs::read_dir(dir) else {
        return out;
    };
    for e in rd.flatten() {
        let p = e.path();
        if p.is_dir() {
            out.extend(walk(&p));
        } else {
            out.push(p.display().to_string());
        }
    }
    out
}
