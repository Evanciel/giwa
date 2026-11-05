//! 링크 카드용 외부 페이지 읽기 (GAP E12, ADR-0084).
//!
//! 서버가 남의 주소에 접속하는 유일한 곳이라 막을 것을 먼저 막는다.
//! - http·https 만. 사설·되돌이·링크 로컬·CGNAT·문서용 등 공개 인터넷이 아닌 주소는 이름을 풀 때와
//!   주소를 바로 쓸 때 모두 거부한다(리디렉션 뒤에도 같은 검사). 운영체제 프록시는 쓰지 않는다.
//! - 시간(연결 4초·전체 8초)·크기(본문 512KB, 그림 5MB)·리디렉션(3번) 제한.
//! - 본문은 `<head>` 의 `og:*`·`twitter:*`·`<title>`·`description` 만 읽는다. 글자 집합은 응답 머리·
//!   `<meta charset>` 을 따른다(국내 사이트의 EUC-KR 대응).

use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;

use maru_core::services::links::{Fetched, FetchedImage, LinkFetcher};
use reqwest::dns::{Addrs, Name, Resolve, Resolving};
use reqwest::{Client, Url, redirect};

const CONNECT_TIMEOUT: Duration = Duration::from_secs(4);
const TOTAL_TIMEOUT: Duration = Duration::from_secs(8);
const MAX_PAGE: usize = 512 * 1024;
const MAX_IMAGE: usize = 5 * 1024 * 1024;
const MAX_REDIRECTS: usize = 3;
const USER_AGENT: &str = concat!("Maru-LinkCard/", env!("CARGO_PKG_VERSION"));

/// 공개 인터넷 주소인가
pub fn is_public_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(a) => {
            let o = a.octets();
            !(a.is_private()
                || a.is_loopback()
                || a.is_link_local()
                || a.is_broadcast()
                || a.is_documentation()
                || a.is_unspecified()
                || a.is_multicast()
                || o[0] == 0
                || o[0] >= 240
                || (o[0] == 100 && (o[1] & 0xc0) == 64) // 100.64.0.0/10 통신사 NAT
                || (o[0] == 192 && o[1] == 0 && o[2] == 0) // 192.0.0.0/24
                || (o[0] == 198 && (o[1] & 0xfe) == 18)) // 198.18.0.0/15 성능 시험
        }
        IpAddr::V6(a) => {
            if let Some(v4) = a.to_ipv4_mapped() {
                return is_public_ip(IpAddr::V4(v4));
            }
            let s = a.segments();
            // 64:ff9b::/96 은 IPv4 를 품는다
            if s[0] == 0x64 && s[1] == 0xff9b && s[2..6] == [0, 0, 0, 0] {
                let v4 = std::net::Ipv4Addr::new(
                    (s[6] >> 8) as u8,
                    s[6] as u8,
                    (s[7] >> 8) as u8,
                    s[7] as u8,
                );
                return is_public_ip(IpAddr::V4(v4));
            }
            !(a.is_loopback()
                || a.is_unspecified()
                || a.is_multicast()
                || (s[0] & 0xfe00) == 0xfc00 // 고유 로컬
                || (s[0] & 0xffc0) == 0xfe80 // 링크 로컬
                || (s[0] == 0x2001 && s[1] == 0x0db8)) // 문서용
        }
    }
}

/// 시험(디버그 빌드)에서만 이 컴퓨터 주소를 허락한다. 배포 빌드는 이 값을 보지 않는다.
fn allow_loopback() -> bool {
    cfg!(debug_assertions)
        && std::env::var("MARU_TEST_ALLOW_LOOPBACK_FETCH").is_ok_and(|v| v == "1")
}

fn ip_allowed(ip: IpAddr) -> bool {
    is_public_ip(ip) || (allow_loopback() && ip.is_loopback())
}

/// 주소 모양 검사: http·https, 사용자 정보 없음, 호스트가 IP 면 공개 주소
fn url_allowed(url: &Url) -> Result<(), String> {
    if !matches!(url.scheme(), "http" | "https") {
        return Err("http·https 주소만 읽습니다".into());
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err("주소에 로그인 정보가 들어 있습니다".into());
    }
    let host = url.host_str().ok_or("호스트가 없습니다")?;
    let bare = host.trim_start_matches('[').trim_end_matches(']');
    let refused = match bare.parse::<IpAddr>() {
        Ok(ip) => !ip_allowed(ip),
        Err(_) => {
            let lower = bare.to_ascii_lowercase();
            (lower == "localhost" || lower.ends_with(".localhost")) && !allow_loopback()
        }
    };
    if refused {
        return Err("공개 인터넷 주소가 아닙니다".into());
    }
    Ok(())
}

/// 이름을 풀어 공개 주소만 남긴다. 하나도 없으면 실패.
struct PublicOnly;

impl Resolve for PublicOnly {
    fn resolve(&self, name: Name) -> Resolving {
        let host = name.as_str().to_owned();
        Box::pin(async move {
            let found: Vec<SocketAddr> = tokio::net::lookup_host((host.as_str(), 0))
                .await?
                .filter(|a| ip_allowed(a.ip()))
                .collect();
            if found.is_empty() {
                return Err(format!("{host}: 공개 인터넷 주소가 아닙니다").into());
            }
            let addrs: Addrs = Box::new(found.into_iter());
            Ok(addrs)
        })
    }
}

pub struct HttpFetcher {
    client: Client,
}

impl HttpFetcher {
    pub fn new() -> Result<Self, String> {
        crate::tls::install_provider();
        let policy = redirect::Policy::custom(|attempt| {
            if attempt.previous().len() >= MAX_REDIRECTS {
                attempt.error("리디렉션이 너무 많습니다")
            } else if let Err(e) = url_allowed(attempt.url()) {
                attempt.error(e)
            } else {
                attempt.follow()
            }
        });
        let client = Client::builder()
            .no_proxy()
            .dns_resolver(Arc::new(PublicOnly))
            .redirect(policy)
            .connect_timeout(CONNECT_TIMEOUT)
            .timeout(TOTAL_TIMEOUT)
            .user_agent(USER_AGENT)
            .build()
            .map_err(|e| e.to_string())?;
        Ok(Self { client })
    }

    /// 크기 제한을 지키며 받는다. 돌려주는 값: (최종 주소, Content-Type, 바이트)
    async fn get(
        &self,
        url: &Url,
        max: usize,
        accept: &str,
    ) -> Result<(Url, String, Vec<u8>), String> {
        url_allowed(url)?;
        let mut res = self
            .client
            .get(url.clone())
            .header(reqwest::header::ACCEPT, accept)
            .header(reqwest::header::ACCEPT_LANGUAGE, "ko,en;q=0.7")
            .send()
            .await
            .map_err(|e| describe(&e))?;
        if !res.status().is_success() {
            return Err(format!("응답 {}", res.status().as_u16()));
        }
        if res.content_length().is_some_and(|n| n > max as u64) {
            return Err("너무 큽니다".into());
        }
        let ctype = res
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or_default()
            .to_ascii_lowercase();
        let final_url = res.url().clone();
        let mut body = Vec::new();
        while let Some(chunk) = res.chunk().await.map_err(|e| describe(&e))? {
            if body.len() + chunk.len() > max {
                // 페이지는 앞부분(head)만 있으면 된다. 그림은 잘리면 못 쓴다.
                if accept.starts_with("text/html") {
                    body.extend_from_slice(&chunk[..max - body.len()]);
                    break;
                }
                return Err("너무 큽니다".into());
            }
            body.extend_from_slice(&chunk);
        }
        Ok((final_url, ctype, body))
    }
}

fn describe(e: &reqwest::Error) -> String {
    use std::error::Error as _;
    let mut s = e.to_string();
    let mut src = e.source();
    while let Some(inner) = src {
        s.push_str(": ");
        s.push_str(&inner.to_string());
        src = inner.source();
    }
    s.chars().take(300).collect()
}

#[async_trait::async_trait]
impl LinkFetcher for HttpFetcher {
    async fn fetch(&self, url: &str) -> Result<Fetched, String> {
        let url = Url::parse(url).map_err(|e| format!("주소를 읽지 못했습니다: {e}"))?;
        let (final_url, ctype, body) = self
            .get(
                &url,
                MAX_PAGE,
                "text/html,application/xhtml+xml;q=0.9,*/*;q=0.1",
            )
            .await?;
        if !(ctype.starts_with("text/html") || ctype.starts_with("application/xhtml")) {
            return Err(format!("웹 페이지가 아닙니다 ({ctype})"));
        }
        let text = decode(&body, &ctype);
        let meta = parse_head(&text);
        let mut fetched = Fetched {
            title: meta.title,
            description: meta.description,
            site_name: meta.site_name,
            image: None,
        };
        if let Some(src) = meta.image
            && let Ok(img_url) = final_url.join(&src)
            && let Ok((_, ictype, bytes)) = self
                .get(
                    &img_url,
                    MAX_IMAGE,
                    "image/avif,image/webp,image/png,image/jpeg,image/gif;q=0.8",
                )
                .await
            && let Some(ext) = image_ext(&ictype)
        {
            fetched.image = Some(FetchedImage {
                filename: format!("link-card.{ext}"),
                bytes,
            });
        }
        Ok(fetched)
    }
}

fn image_ext(ctype: &str) -> Option<&'static str> {
    match ctype.split(';').next()?.trim() {
        "image/jpeg" | "image/jpg" => Some("jpg"),
        "image/png" => Some("png"),
        "image/gif" => Some("gif"),
        "image/webp" => Some("webp"),
        _ => None,
    }
}

/// 응답 머리의 charset, 없으면 문서 앞부분의 `<meta charset>`, 그래도 없으면 UTF-8
pub fn decode(body: &[u8], ctype: &str) -> String {
    let label = charset_of(ctype).or_else(|| {
        let head = &body[..body.len().min(4096)];
        let ascii: String = head
            .iter()
            .map(|&b| if b.is_ascii() { b as char } else { ' ' })
            .collect();
        let lower = ascii.to_ascii_lowercase();
        let at = lower.find("charset=")?;
        let rest = &lower[at + 8..];
        let rest = rest.trim_start_matches(['"', '\'']);
        let end = rest
            .find(|c: char| !(c.is_ascii_alphanumeric() || c == '-' || c == '_'))
            .unwrap_or(rest.len());
        Some(rest[..end].to_owned())
    });
    let enc = label
        .and_then(|l| encoding_rs::Encoding::for_label(l.as_bytes()))
        .unwrap_or(encoding_rs::UTF_8);
    enc.decode(body).0.into_owned()
}

fn charset_of(ctype: &str) -> Option<String> {
    let at = ctype.find("charset=")?;
    let v = ctype[at + 8..].trim_matches(['"', '\'', ' ']);
    let end = v.find(';').unwrap_or(v.len());
    Some(v[..end].trim().to_owned())
}

#[derive(Debug, Default, PartialEq, Eq)]
pub struct HeadMeta {
    pub title: Option<String>,
    pub description: Option<String>,
    pub site_name: Option<String>,
    pub image: Option<String>,
}

/// `<head>` 안의 링크 카드 정보. og → twitter → 기본 태그 순으로 고른다.
pub fn parse_head(html: &str) -> HeadMeta {
    let lower = html.to_ascii_lowercase();
    let end = lower.find("</head").unwrap_or(lower.len());
    let (html, lower) = (&html[..end], &lower[..end]);
    let mut og = HeadMeta::default();
    let mut tw = HeadMeta::default();
    let mut plain = HeadMeta::default();
    let mut from = 0;
    while let Some(at) = lower[from..].find("<meta") {
        let start = from + at;
        let Some(close) = lower[start..].find('>') else {
            break;
        };
        let tag = &html[start + 5..start + close];
        from = start + close + 1;
        let attrs = attributes(tag);
        let key = attrs
            .iter()
            .find(|(k, _)| k == "property" || k == "name")
            .map(|(_, v)| v.to_ascii_lowercase());
        let Some(content) = attrs
            .iter()
            .find(|(k, _)| k == "content")
            .map(|(_, v)| clean(v))
        else {
            continue;
        };
        if content.is_empty() {
            continue;
        }
        let slot = match key.as_deref() {
            Some("og:title") => &mut og.title,
            Some("og:description") => &mut og.description,
            Some("og:site_name") => &mut og.site_name,
            Some("og:image" | "og:image:url" | "og:image:secure_url") => &mut og.image,
            Some("twitter:title") => &mut tw.title,
            Some("twitter:description") => &mut tw.description,
            Some("twitter:image" | "twitter:image:src") => &mut tw.image,
            Some("description") => &mut plain.description,
            _ => continue,
        };
        if slot.is_none() {
            *slot = Some(content);
        }
    }
    if let Some(t) = lower.find("<title")
        && let Some(gt) = lower[t..].find('>')
    {
        let s = t + gt + 1;
        if let Some(e) = lower[s..].find("</title") {
            let v = clean(&html[s..s + e]);
            if !v.is_empty() {
                plain.title = Some(v);
            }
        }
    }
    HeadMeta {
        title: og.title.or(tw.title).or(plain.title).map(|v| cut(&v, 200)),
        description: og
            .description
            .or(tw.description)
            .or(plain.description)
            .map(|v| cut(&v, 300)),
        site_name: og.site_name.map(|v| cut(&v, 80)),
        image: og.image.or(tw.image).filter(|v| v.len() <= 2048),
    }
}

/// 태그 안 속성들(이름은 소문자)
fn attributes(tag: &str) -> Vec<(String, String)> {
    let b = tag.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < b.len() {
        while i < b.len() && (b[i].is_ascii_whitespace() || b[i] == b'/') {
            i += 1;
        }
        let ns = i;
        while i < b.len() && !b[i].is_ascii_whitespace() && b[i] != b'=' && b[i] != b'/' {
            i += 1;
        }
        if ns == i {
            i += 1;
            continue;
        }
        let name = tag[ns..i].to_ascii_lowercase();
        while i < b.len() && b[i].is_ascii_whitespace() {
            i += 1;
        }
        if i < b.len() && b[i] == b'=' {
            i += 1;
            while i < b.len() && b[i].is_ascii_whitespace() {
                i += 1;
            }
            let value = if i < b.len() && (b[i] == b'"' || b[i] == b'\'') {
                let q = b[i];
                let vs = i + 1;
                let ve = tag[vs..]
                    .bytes()
                    .position(|c| c == q)
                    .map_or(b.len(), |p| vs + p);
                i = (ve + 1).min(b.len());
                &tag[vs..ve.min(b.len())]
            } else {
                let vs = i;
                while i < b.len() && !b[i].is_ascii_whitespace() {
                    i += 1;
                }
                &tag[vs..i]
            };
            out.push((name, value.to_owned()));
        } else {
            out.push((name, String::new()));
        }
    }
    out
}

/// 엔터티를 풀고 공백을 한 칸으로, 제어 문자는 뺀다
fn clean(s: &str) -> String {
    let s = unescape(s);
    s.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .chars()
        .filter(|c| !c.is_control())
        .collect()
}

fn cut(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_owned();
    }
    let mut out: String = s.chars().take(max - 1).collect();
    out.push('…');
    out
}

fn unescape(s: &str) -> String {
    if !s.contains('&') {
        return s.to_owned();
    }
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(at) = rest.find('&') {
        out.push_str(&rest[..at]);
        let after = &rest[at + 1..];
        let Some(semi) = after.find(';').filter(|&n| n <= 10) else {
            out.push('&');
            rest = after;
            continue;
        };
        let ent = &after[..semi];
        let ch = match ent {
            "amp" => Some('&'),
            "lt" => Some('<'),
            "gt" => Some('>'),
            "quot" => Some('"'),
            "apos" | "#39" => Some('\''),
            "nbsp" => Some(' '),
            _ => ent
                .strip_prefix("#x")
                .or_else(|| ent.strip_prefix("#X"))
                .and_then(|h| u32::from_str_radix(h, 16).ok())
                .or_else(|| ent.strip_prefix('#').and_then(|d| d.parse().ok()))
                .and_then(char::from_u32),
        };
        match ch {
            Some(c) => {
                out.push(c);
                rest = &after[semi + 1..];
            }
            None => {
                out.push('&');
                rest = after;
            }
        }
    }
    out.push_str(rest);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn private_addresses_are_refused() {
        for ip in [
            "127.0.0.1",
            "10.1.2.3",
            "172.16.0.1",
            "192.168.0.10",
            "169.254.169.254",
            "100.64.0.1",
            "0.0.0.0",
            "255.255.255.255",
            "198.18.0.1",
            "::1",
            "fd00::1",
            "fe80::1",
            "::ffff:127.0.0.1",
            "64:ff9b::a00:1",
            "2001:db8::1",
        ] {
            assert!(!is_public_ip(ip.parse().unwrap()), "{ip}");
        }
        for ip in [
            "8.8.8.8",
            "1.1.1.1",
            "2606:4700:4700::1111",
            "::ffff:8.8.8.8",
        ] {
            assert!(is_public_ip(ip.parse().unwrap()), "{ip}");
        }
    }

    #[test]
    fn url_shape_rules() {
        for bad in [
            "ftp://example.test/a",
            "http://user:pw@example.test/",
            "http://127.0.0.1/",
            "http://[::1]/",
            "http://10.0.0.1:8080/",
        ] {
            assert!(url_allowed(&Url::parse(bad).unwrap()).is_err(), "{bad}");
        }
        assert!(url_allowed(&Url::parse("https://example.test/글").unwrap()).is_ok());
    }

    #[test]
    fn head_prefers_open_graph() {
        let html = r#"<html><head><meta charset="utf-8"><title>문서 &amp; 제목</title>
            <meta name="description" content="기본 설명">
            <meta property="og:title" content="열린 &quot;그래프&quot; 제목">
            <meta content='오지 설명' property='og:description'>
            <meta property="og:site_name" content="예시 사이트">
            <meta property=og:image content=/img/a.png>
            </head><body><meta property="og:title" content="본문의 것은 무시"></body></html>"#;
        let m = parse_head(html);
        assert_eq!(m.title.as_deref(), Some("열린 \"그래프\" 제목"));
        assert_eq!(m.description.as_deref(), Some("오지 설명"));
        assert_eq!(m.site_name.as_deref(), Some("예시 사이트"));
        assert_eq!(m.image.as_deref(), Some("/img/a.png"));
        let m = parse_head(
            "<title>\n  그냥   제목 </title><meta name=description content=\"짧은 &#xAC00;\">",
        );
        assert_eq!(m.title.as_deref(), Some("그냥 제목"));
        assert_eq!(m.description.as_deref(), Some("짧은 가"));
    }

    #[test]
    fn decodes_euc_kr() {
        let (bytes, _, _) =
            encoding_rs::EUC_KR.encode("<meta charset=\"euc-kr\"><title>한글 제목</title>");
        assert_eq!(
            parse_head(&decode(&bytes, "text/html")).title.as_deref(),
            Some("한글 제목")
        );
        let (bytes, _, _) = encoding_rs::EUC_KR.encode("<title>머리</title>");
        assert_eq!(
            parse_head(&decode(&bytes, "text/html; charset=EUC-KR"))
                .title
                .as_deref(),
            Some("머리")
        );
    }
}
