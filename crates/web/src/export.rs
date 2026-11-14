//! `giwa export static` (SPEC §12, ADR-0131). 손님이 보는 화면을 파일로 내려 정적 호스팅에 올린다.
//!
//! 라우터를 이 프로세스 안에서 직접 불러 첫 화면과 sitemap 에서 링크를 따라간다. 쿠키 없는 손님으로
//! 묻기 때문에 회원 전용 게시판과 초안은 나오지 않는다. 쿼리 있는 링크 중 `?page=N` 만 파일이 된다.

use std::collections::{HashSet, VecDeque};
use std::net::{Ipv4Addr, SocketAddr};
use std::path::{Component, Path, PathBuf};

use axum::Router;
use axum::body::{Body, to_bytes};
use axum::extract::ConnectInfo;
use axum::http::{Request, StatusCode, header};
use giwa_core::actor::Actor;
use giwa_core::services::redirects;
use giwa_core::{Error, Result};
use percent_encoding::{AsciiSet, CONTROLS, percent_decode_str, utf8_percent_encode};
use serde::Serialize;
use tower::ServiceExt;

use crate::{AppState, router};

const MAX_URLS: usize = 100_000;
const MAX_BODY: usize = 1024 * 1024 * 1024;
const SKIPPED_SHOWN: usize = 50;
/// 집계에 들어가지 않게 봇으로 밝힌다.
const USER_AGENT: &str = "maru-export-bot/1.0";
/// 저장소가 있어야 동작하는 화면. 파일로 내려도 쓸모가 없다.
const DYNAMIC: [&str; 12] = [
    "/admin",
    "/api/",
    "/login",
    "/logout",
    "/signup",
    "/me",
    "/m/",
    "/mcp",
    "/__preview",
    "/search",
    "/password",
    "/verify",
];
/// 쿼리를 떼고 파일로 받는 경로. `?v=` 는 정적 호스팅이 무시한다.
const STATIC_FILES: [&str; 4] = ["/assets/", "/media/", "/files/", "/fonts/"];
const ENCODE: &AsciiSet = &CONTROLS
    .add(b' ')
    .add(b'"')
    .add(b'<')
    .add(b'>')
    .add(b'`')
    .add(b'#');

#[derive(Debug, Clone, Serialize)]
pub struct ExportReport {
    pub out: String,
    pub pages: usize,
    pub files: usize,
    pub bytes: u64,
    pub redirects: usize,
    /// 200 이 아니어서 뺀 주소 (로그인이 필요한 곳 등). 앞쪽 일부만
    pub skipped: Vec<String>,
    pub skipped_total: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct Link {
    /// 디코딩한 경로
    path: String,
    page: Option<u32>,
}

impl Link {
    fn of(path: &str) -> Self {
        Self {
            path: path.to_owned(),
            page: None,
        }
    }

    fn request_uri(&self) -> String {
        let path = encoded(&self.path);
        match self.page {
            Some(n) => format!("{path}?page={n}"),
            None => path,
        }
    }
}

fn encoded(path: &str) -> String {
    utf8_percent_encode(path, ENCODE).to_string()
}

fn is_dynamic(path: &str) -> bool {
    DYNAMIC.iter().any(|d| {
        let base = d.trim_end_matches('/');
        path == base || path.starts_with(&format!("{base}/"))
    })
}

/// 링크 하나를 이 사이트의 경로로 푼다. 바깥 주소, 동적 화면, 다른 쿼리는 `None`.
fn resolve(raw: &str, from: &str, site: &str) -> Option<Link> {
    let raw = raw.trim().replace("&amp;", "&");
    let raw = raw.split('#').next().unwrap_or("");
    if raw.is_empty() {
        return None;
    }
    let site = site.trim_end_matches('/');
    let local = match raw.strip_prefix(site) {
        Some(rest) if !site.is_empty() && (rest.is_empty() || rest.starts_with(['/', '?'])) => {
            if rest.is_empty() {
                "/"
            } else {
                rest
            }
        }
        _ => raw,
    };
    if local.starts_with("//")
        || local.contains("://")
        || local.contains(':') && !local.starts_with('/')
    {
        return None;
    }
    let (path, query) = local.split_once('?').unwrap_or((local, ""));
    let path = if path.is_empty() {
        from.to_owned()
    } else if path.starts_with('/') {
        path.to_owned()
    } else {
        let parent = from.rsplit_once('/').map_or("", |(p, _)| p);
        format!("{parent}/{path}")
    };
    let path = percent_decode_str(&path).decode_utf8().ok()?.into_owned();
    if path.split('/').any(|s| s == "..") || is_dynamic(&path) {
        return None;
    }
    if STATIC_FILES.iter().any(|p| path.starts_with(p)) {
        return Some(Link::of(&path));
    }
    if query.is_empty() {
        return Some(Link::of(&path));
    }
    let n: u32 = query.strip_prefix("page=")?.parse().ok()?;
    Some(Link {
        path,
        page: (n > 1).then_some(n),
    })
}

/// `?page=N` 링크를 파일 경로로 바꾼 값.
fn static_href(link: &Link) -> String {
    let base = encoded(link.path.trim_end_matches('/'));
    match link.page {
        Some(n) => format!("{base}/page-{n}/"),
        None => format!("{base}/"),
    }
}

/// 다음 링크 속성의 위치와 속성.
fn next_attr(s: &str) -> Option<(usize, &'static str)> {
    [" href=\"", " src=\"", " srcset=\"", " poster=\""]
        .into_iter()
        .filter_map(|a| s.find(a).map(|at| (at, a)))
        .min_by_key(|(at, _)| *at)
}

/// HTML 의 링크를 모으고 `?page=N` 을 파일 경로로 바꾼다.
fn scan_html(html: &str, from: &str, site: &str, found: &mut Vec<Link>) -> String {
    let mut out = String::with_capacity(html.len());
    let mut rest = html;
    while let Some((at, attr)) = next_attr(rest) {
        let start = at + attr.len();
        let Some(len) = rest[start..].find('"') else {
            break;
        };
        out.push_str(&rest[..start]);
        let value = &rest[start..start + len];
        if attr == " srcset=\"" {
            for one in value.split(',') {
                if let Some(link) = one
                    .split_whitespace()
                    .next()
                    .and_then(|u| resolve(u, from, site))
                {
                    found.push(link);
                }
            }
            out.push_str(value);
        } else if let Some(link) = resolve(value, from, site) {
            if value.contains("page=") && !STATIC_FILES.iter().any(|p| link.path.starts_with(p)) {
                out.push_str(&static_href(&link));
            } else {
                out.push_str(value);
            }
            found.push(link);
        } else {
            out.push_str(value);
        }
        rest = &rest[start + len..];
    }
    out.push_str(rest);
    scan_css(html, from, site, found);
    out
}

/// CSS 와 `<style>` 의 `url(...)`.
fn scan_css(css: &str, from: &str, site: &str, found: &mut Vec<Link>) {
    for part in css.split("url(").skip(1) {
        let Some((value, _)) = part.split_once(')') else {
            continue;
        };
        let value = value.trim().trim_matches(['"', '\'']);
        if let Some(link) = resolve(value, from, site) {
            found.push(link);
        }
    }
}

/// `<loc>` 를 모은다.
fn scan_sitemap(xml: &str, site: &str, found: &mut Vec<Link>) {
    for part in xml.split("<loc>").skip(1) {
        if let Some((value, _)) = part.split_once("</loc>")
            && let Some(link) = resolve(value, "/", site)
        {
            found.push(link);
        }
    }
}

/// 내릴 파일 위치. 경로를 벗어나거나 파일 이름으로 쓸 수 없으면 `None`.
fn file_path(out: &Path, link: &Link, html: bool) -> Option<PathBuf> {
    let mut rel = PathBuf::new();
    for seg in link.path.split('/').filter(|s| !s.is_empty()) {
        let bad = seg == "." || seg == ".." || seg.ends_with(['.', ' ']);
        if bad
            || seg
                .chars()
                .any(|c| c.is_control() || "\\:*?\"<>|".contains(c))
        {
            return None;
        }
        rel.push(seg);
    }
    if html {
        if let Some(n) = link.page {
            rel.push(format!("page-{n}"));
        }
        rel.push("index.html");
    } else if rel.as_os_str().is_empty() {
        return None;
    }
    if rel.components().any(|c| !matches!(c, Component::Normal(_))) {
        return None;
    }
    Some(out.join(rel))
}

fn write_file(path: &Path, bytes: &[u8]) -> Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(Error::internal)?;
    }
    std::fs::write(path, bytes).map_err(Error::internal)
}

/// 요청 `n` 번째의 연결 주소. 분당 요청 제한(주소별 600)에 걸리지 않게 500번마다 루프백 주소를 바꾼다.
fn caller(n: usize) -> SocketAddr {
    let step = u32::try_from(n / 500).unwrap_or(u32::MAX >> 8);
    SocketAddr::from((Ipv4Addr::from(u32::from(Ipv4Addr::LOCALHOST) + step), 0))
}

async fn fetch(app: &Router, uri: &str, n: usize) -> Result<(StatusCode, String, Vec<u8>, String)> {
    let mut req = Request::builder()
        .uri(uri)
        .header(header::USER_AGENT, USER_AGENT)
        .header(header::HOST, "localhost")
        .body(Body::empty())
        .map_err(Error::internal)?;
    req.extensions_mut().insert(ConnectInfo(caller(n)));
    let res = app.clone().oneshot(req).await.map_err(Error::internal)?;
    let status = res.status();
    let head = |name| {
        res.headers()
            .get(name)
            .and_then(|v: &header::HeaderValue| v.to_str().ok())
            .unwrap_or("")
            .to_owned()
    };
    let kind = head(header::CONTENT_TYPE);
    let location = head(header::LOCATION);
    let bytes = to_bytes(res.into_body(), MAX_BODY)
        .await
        .map_err(Error::internal)?;
    Ok((status, kind, bytes.to_vec(), location))
}

pub async fn export_static(state: AppState, out: &Path) -> Result<ExportReport> {
    let site = state.ctx.site_url.clone();
    let actor = Actor::local();
    let moved = redirects::list(&state.ctx, &actor).await?;
    let app = router(state);
    let mut report = ExportReport {
        out: out.display().to_string(),
        pages: 0,
        files: 0,
        bytes: 0,
        redirects: moved.len(),
        skipped: Vec::new(),
        skipped_total: 0,
    };
    let mut queue: VecDeque<Link> = ["/", "/sitemap.xml", "/rss.xml", "/robots.txt"]
        .into_iter()
        .map(Link::of)
        .collect();
    let mut seen: HashSet<Link> = queue.iter().cloned().collect();
    let mut asked = 0;
    while let Some(link) = queue.pop_front() {
        if seen.len() > MAX_URLS {
            break;
        }
        let uri = link.request_uri();
        let (status, kind, bytes, location) = fetch(&app, &uri, asked).await?;
        asked += 1;
        let mut found = Vec::new();
        if status.is_redirection() {
            if let Some(next) = resolve(&location, &link.path, &site) {
                found.push(next);
            }
        } else if status != StatusCode::OK {
            report.skipped_total += 1;
            if report.skipped.len() < SKIPPED_SHOWN {
                report.skipped.push(format!("{uri} ({})", status.as_u16()));
            }
        } else {
            let html = kind.starts_with("text/html");
            let Some(path) = file_path(out, &link, html) else {
                report.skipped_total += 1;
                continue;
            };
            let body = if html {
                let text = String::from_utf8_lossy(&bytes);
                scan_html(&text, &link.path, &site, &mut found).into_bytes()
            } else {
                if kind.starts_with("text/css") {
                    scan_css(
                        &String::from_utf8_lossy(&bytes),
                        &link.path,
                        &site,
                        &mut found,
                    );
                } else if link.path == "/sitemap.xml" {
                    scan_sitemap(&String::from_utf8_lossy(&bytes), &site, &mut found);
                }
                bytes
            };
            write_file(&path, &body)?;
            report.bytes += body.len() as u64;
            if html {
                report.pages += 1;
            } else {
                report.files += 1;
            }
        }
        for next in found {
            if seen.insert(next.clone()) {
                queue.push_back(next);
            }
        }
    }
    let (status, kind, bytes, _) = fetch(&app, "/__maru_export_missing", asked).await?;
    if status == StatusCode::NOT_FOUND && kind.starts_with("text/html") {
        write_file(&out.join("404.html"), &bytes)?;
    }
    if !moved.is_empty() {
        let lines: String = moved
            .iter()
            .map(|r| format!("{} {} 301\n", encoded(&r.from_path), encoded(&r.to_path)))
            .collect();
        write_file(&out.join("_redirects"), lines.as_bytes())?;
    }
    Ok(report)
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    const SITE: &str = "https://example.test";

    #[test]
    fn 이_사이트의_화면과_파일만_푼다() {
        let r = |raw: &str| resolve(raw, "/b/free", SITE);
        assert_eq!(r("/p/about"), Some(Link::of("/p/about")));
        assert_eq!(
            r("https://example.test/p/about#top"),
            Some(Link::of("/p/about"))
        );
        assert_eq!(r("/p/%ED%9A%8C%EC%82%AC"), Some(Link::of("/p/회사")));
        assert_eq!(
            r("/assets/site.css?v=abc"),
            Some(Link::of("/assets/site.css"))
        );
        for raw in [
            "https://other.test/x",
            "//other.test/x",
            "mailto:a@example.test",
            "javascript:alert(1)",
            "/admin/pages",
            "/login?next=/",
            "/api/v1/posts",
            "/b/free?sort=hot",
            "/p/../admin",
            "#top",
        ] {
            assert_eq!(r(raw), None, "{raw}");
        }
    }

    #[test]
    fn 쪽_링크는_파일_경로가_된다() {
        let mut found = Vec::new();
        let out = scan_html(
            r#"<a href="?page=2">2</a> <a href="/b/free?page=3">3</a> <a href="/p/x">x</a>"#,
            "/b/free",
            SITE,
            &mut found,
        );
        assert!(out.contains(r#"href="/b/free/page-2/""#), "{out}");
        assert!(out.contains(r#"href="/b/free/page-3/""#), "{out}");
        assert!(out.contains(r#"href="/p/x""#), "{out}");
        assert_eq!(found.len(), 3);
        assert_eq!(found[0].page, Some(2));
    }

    #[test]
    fn 파일_이름은_밖으로_나가지_않는다() {
        let out = Path::new("out");
        assert_eq!(
            file_path(out, &Link::of("/"), true).unwrap(),
            out.join("index.html")
        );
        assert_eq!(
            file_path(out, &Link::of("/p/회사"), true).unwrap(),
            out.join("p").join("회사").join("index.html")
        );
        assert_eq!(
            file_path(out, &Link::of("/assets/a.css"), false).unwrap(),
            out.join("assets").join("a.css")
        );
        for bad in ["/../x", "/a/../../x", "/a:b", "/a\\b", "/x."] {
            assert!(file_path(out, &Link::of(bad), true).is_none(), "{bad}");
        }
    }

    #[test]
    fn 동적_화면은_앞부분이_같아도_가른다() {
        assert!(is_dynamic("/admin"));
        assert!(is_dynamic("/admin/pages"));
        assert!(is_dynamic("/me"));
        assert!(!is_dynamic("/media/a.jpg"));
        assert!(!is_dynamic("/menu"));
    }
}
