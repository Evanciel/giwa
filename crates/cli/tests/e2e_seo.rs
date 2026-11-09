//! 검색 노출 보강 (ADR-0143). 실제 바이너리·실제 포트·실제 DB 파일로 구조화 데이터, 설명·공유 그림 채우기,
//! 서버가 그린 공유 그림, 공유 그림 설정, 내 컴퓨터 주소에서는 IndexNow 를 보내지 않는 것을 본다.
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

/// (상태, 머리, 본문 바이트)
fn get_raw(bind: &str, path: &str) -> (u16, String, Vec<u8>) {
    let mut s = TcpStream::connect(bind).unwrap();
    s.write_all(
        format!("GET {path} HTTP/1.1\r\nHost: {bind}\r\nConnection: close\r\n\r\n").as_bytes(),
    )
    .unwrap();
    let mut out = Vec::new();
    s.read_to_end(&mut out).unwrap();
    let split = out
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .unwrap_or(out.len());
    let head = String::from_utf8_lossy(&out[..split]).to_lowercase();
    let body = out.get(split + 4..).unwrap_or_default().to_vec();
    (head[9..12].parse().unwrap(), head, body)
}

fn get(bind: &str, path: &str) -> (u16, String) {
    let (status, _, body) = get_raw(bind, path);
    (status, String::from_utf8_lossy(&body).into_owned())
}

/// 화면의 JSON-LD `@graph`.
fn graph(html: &str) -> Vec<Value> {
    let open = "<script type=\"application/ld+json\">";
    let start = html.find(open).unwrap_or_else(|| panic!("JSON-LD 없음")) + open.len();
    let end = start + html[start..].find("</script>").unwrap();
    let raw = &html[start..end];
    assert!(!raw.contains('<'), "블록 안에 < 가 그대로 있다: {raw}");
    let v: Value = serde_json::from_str(raw).unwrap();
    assert_eq!(v["@context"], "https://schema.org");
    v["@graph"].as_array().unwrap().clone()
}

fn of_type<'a>(g: &'a [Value], t: &str) -> &'a Value {
    g.iter()
        .find(|n| n["@type"] == t)
        .unwrap_or_else(|| panic!("{t} 없음: {g:?}"))
}

fn meta<'a>(html: &'a str, attr: &str) -> Option<&'a str> {
    let at = html.find(&format!("{attr} content=\""))? + attr.len() + 10;
    Some(&html[at..at + html[at..].find('"')?])
}

/// JPEG 의 (폭, 높이). SOF0·SOF2 머리를 찾는다.
fn jpeg_size(b: &[u8]) -> (u32, u32) {
    assert_eq!(&b[..2], &[0xFF, 0xD8], "JPEG 가 아니다");
    let mut i = 2;
    while i + 9 < b.len() {
        if b[i] != 0xFF {
            i += 1;
            continue;
        }
        let marker = b[i + 1];
        let len = usize::from(u16::from_be_bytes([b[i + 2], b[i + 3]]));
        if marker == 0xC0 || marker == 0xC2 {
            let h = u32::from(u16::from_be_bytes([b[i + 5], b[i + 6]]));
            let w = u32::from(u16::from_be_bytes([b[i + 7], b[i + 8]]));
            return (w, h);
        }
        i += 2 + len;
    }
    panic!("크기를 찾지 못했다")
}

#[test]
fn 화면마다_구조화_데이터와_설명_공유_그림이_붙는다() {
    let tmp = tempfile::tempdir().unwrap();
    let data = tmp.path().join("데이터");
    maru(&data, &["init", "--preset", "blog", "--yes"]);
    let site = maru(&data, &["site", "show"]);
    assert_eq!(
        site["description"], "일상과 공부한 것을 기록하는 개인 블로그입니다.",
        "사이트 종류가 예시 설명을 넣는다"
    );
    let (_server, bind) = start_server(&data);

    // 첫 화면: WebSite·Organization, 설명, 서버가 그린 공유 그림
    let (status, home) = get(&bind, "/");
    assert_eq!(status, 200);
    let g = graph(&home);
    let website = of_type(&g, "WebSite");
    assert_eq!(
        website["description"],
        "일상과 공부한 것을 기록하는 개인 블로그입니다."
    );
    assert!(website["url"].as_str().unwrap().ends_with('/'));
    assert_eq!(
        of_type(&g, "Organization")["@id"],
        website["publisher"]["@id"]
    );
    assert_eq!(
        meta(&home, "name=\"description\""),
        Some("일상과 공부한 것을 기록하는 개인 블로그입니다.")
    );
    let og_image = meta(&home, "property=\"og:image\"").unwrap();
    assert!(
        og_image.starts_with("http") && og_image.ends_with("/share.jpg"),
        "{og_image}"
    );
    assert_eq!(meta(&home, "property=\"og:image:width\""), Some("1200"));
    assert!(home.contains("<meta name=\"twitter:card\" content=\"summary_large_image\">"));

    // 공유 그림: 1200×630 JPEG
    let (status, head, jpeg) = get_raw(&bind, "/share.jpg");
    assert_eq!(status, 200);
    assert!(head.contains("content-type: image/jpeg"), "{head}");
    assert_eq!(jpeg_size(&jpeg), (1200, 630));

    // 글: Article 과 홈·게시판·글 BreadcrumbList
    let (_, sitemap) = get(&bind, "/sitemap.xml");
    let post_url = sitemap
        .split("<loc>")
        .filter_map(|s| s.split("</loc>").next())
        .find(|u| u.contains("/b/") && u.matches('/').count() >= 5)
        .expect("사이트맵에 글")
        .to_owned();
    let path = &post_url[post_url.find("/b/").unwrap()..];
    let (status, post) = get(&bind, path);
    assert_eq!(status, 200);
    let g = graph(&post);
    let article = of_type(&g, "Article");
    assert!(article["headline"].as_str().unwrap().chars().count() <= 110);
    assert!(article["datePublished"].is_string() && article["dateModified"].is_string());
    assert_eq!(article["mainEntityOfPage"], post_url.as_str());
    assert!(article["image"][0].as_str().unwrap().starts_with("http"));
    assert!(article["publisher"]["name"].is_string());
    let crumbs = of_type(&g, "BreadcrumbList")["itemListElement"]
        .as_array()
        .unwrap()
        .clone();
    assert_eq!(crumbs.len(), 3);
    assert_eq!(crumbs[2]["item"], post_url.as_str());
    assert_eq!(crumbs[0]["position"], 1);

    // 게시판: 홈·게시판
    let board_path = &path[..path.rfind('/').unwrap()];
    let (_, board) = get(&bind, board_path);
    let g = graph(&board);
    assert_eq!(
        of_type(&g, "BreadcrumbList")["itemListElement"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
    assert!(
        meta(&board, "name=\"description\"").is_some(),
        "게시판도 설명이 있다"
    );

    // 검색 결과처럼 noindex 인 화면은 구조화 데이터를 내지 않는다.
    let (_, found) = get(&bind, "/search?q=%EA%B8%80");
    assert!(found.contains("noindex"));
    assert!(!found.contains("application/ld+json"));
}

#[test]
fn 공유_그림_설정과_이름의_스크립트_닫기를_막는다() {
    let tmp = tempfile::tempdir().unwrap();
    let data = tmp.path().join("데이터");
    maru(&data, &["init", "--no-samples", "--yes"]);
    maru(
        &data,
        &[
            "site",
            "set",
            "--name",
            "가게</script><b>&",
            "--share-image",
            "/media/2026/11/cover.jpg",
        ],
    );
    let bad = run(
        &data,
        &["site", "set", "--share-image", "javascript:alert(1)"],
    );
    assert_eq!(bad["ok"], false, "{bad}");
    assert_eq!(bad["error"]["code"], "VALIDATION");
    let bad = run(
        &data,
        &["site", "set", "--share-image", "/media/../config.toml"],
    );
    assert_eq!(bad["ok"], false, "{bad}");

    let (_server, bind) = start_server(&data);
    let (_, home) = get(&bind, "/");
    let og_image = meta(&home, "property=\"og:image\"").unwrap();
    assert!(og_image.ends_with("/media/2026/11/cover.jpg"), "{og_image}");
    assert!(
        meta(&home, "property=\"og:image:width\"").is_none(),
        "직접 정한 그림은 크기를 모른다"
    );
    // graph() 가 블록 안에 < 가 없는지 본다. 이름은 JSON 으로 되읽으면 그대로다.
    let g = graph(&home);
    assert_eq!(of_type(&g, "WebSite")["name"], "가게</script><b>&");
    assert_eq!(
        home.matches("</script>").count(),
        home.matches("<script").count()
    );

    // 비우면 다시 서버가 그린 그림
    maru(&data, &["site", "set", "--share-image", ""]);
    let (_, home) = get(&bind, "/");
    assert!(
        meta(&home, "property=\"og:image\"")
            .unwrap()
            .ends_with("/share.jpg")
    );
}

#[test]
fn 내_컴퓨터_주소에서는_검색엔진에_알리지_않는다() {
    let tmp = tempfile::tempdir().unwrap();
    let data = tmp.path().join("데이터");
    maru(&data, &["init", "--preset", "blog", "--yes"]);
    let s = maru(&data, &["seo", "status"]);
    assert_eq!(s["enabled"], true);
    assert_eq!(s["public_site"], false);
    assert_eq!(s["endpoint"], "https://searchadvisor.naver.com/indexnow");
    assert!(s["key_url"].is_null());
    assert!(s["last_at"].is_null());

    let (_server, bind) = start_server(&data);
    // 키가 없으니 어떤 .txt 도 키 파일이 아니다. 맨 위 한 조각 주소는 여전히 404 다.
    assert_eq!(get(&bind, "/0123456789abcdef0123456789abcdef.txt").0, 404);
    assert_eq!(get(&bind, "/nothing-here").0, 404);
    assert_eq!(get(&bind, "/robots.txt").0, 200);

    maru(&data, &["site", "set", "--indexnow", "false"]);
    assert_eq!(maru(&data, &["seo", "status"])["enabled"], false);
}
