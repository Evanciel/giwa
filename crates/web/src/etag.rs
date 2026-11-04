//! 공개 응답 조건부 요청 (ETag · 304).
//!
//! 렌더는 그대로 하고 전송만 줄인다. 본문 해시를 약한 ETag 로 달고, 브라우저가 같은 값을
//! `If-None-Match` 로 보내면 본문 없이 304 를 돌려준다. 렌더 결과를 캐시하지 않으므로
//! 무효화할 것이 없다 — 글이 바뀌면 본문이 바뀌고 해시도 바뀐다.

use axum::body::{Body, to_bytes};
use axum::extract::Request;
use axum::http::{HeaderMap, HeaderValue, Method, StatusCode, header};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use sha2::{Digest, Sha256};

/// 이보다 큰 본문은 해시하지 않고 그대로 보낸다 (테마 글꼴 파일 등).
const MAX_BODY: usize = 4 * 1024 * 1024;

pub async fn conditional(req: Request, next: Next) -> Response {
    if !matches!(*req.method(), Method::GET | Method::HEAD) {
        return next.run(req).await;
    }
    let if_none_match = req.headers().get(header::IF_NONE_MATCH).cloned();
    let res = next.run(req).await;
    if !eligible(&res) {
        return res;
    }
    let (mut parts, body) = res.into_parts();
    let bytes = match to_bytes(body, MAX_BODY).await {
        Ok(b) => b,
        Err(e) => {
            // 상한 초과나 본문 스트림 오류. 이미 본문을 소비해서 원래 응답을 되돌릴 수 없다.
            tracing::error!(error = ?e, "ETag 계산용 본문 읽기 실패");
            return StatusCode::INTERNAL_SERVER_ERROR.into_response();
        }
    };
    let tag = tag_of(&bytes);
    if let Ok(v) = HeaderValue::from_str(&tag) {
        parts.headers.insert(header::ETAG, v);
    }
    // 머리가 없으면 브라우저가 추측으로 오래 들고 있을 수 있다. 매번 재확인하게 한다.
    parts
        .headers
        .entry(header::CACHE_CONTROL)
        .or_insert(HeaderValue::from_static("no-cache"));
    if if_none_match.is_some_and(|v| matches(&v, &tag)) {
        let mut not_modified = Response::new(Body::empty());
        *not_modified.status_mut() = StatusCode::NOT_MODIFIED;
        copy_validators(&parts.headers, not_modified.headers_mut());
        // 응답 표시(예: 방문 통계의 공개 화면 표시)는 304 에도 남긴다.
        *not_modified.extensions_mut() = parts.extensions;
        return not_modified;
    }
    Response::from_parts(parts, Body::from(bytes))
}

/// 200 이고, 쿠키를 심지 않으며, 이미 ETag 가 없고, 본문 크기를 알거나 작을 때만.
fn eligible(res: &Response) -> bool {
    let h = res.headers();
    res.status() == StatusCode::OK
        && !h.contains_key(header::SET_COOKIE)
        && !h.contains_key(header::ETAG)
        && h.get(header::CONTENT_LENGTH)
            .and_then(|v| v.to_str().ok()?.parse::<usize>().ok())
            .is_none_or(|n| n <= MAX_BODY)
}

/// `W/"<SHA-256 앞 16바이트 hex>"`. 내용이 같으면 압축·헤더와 무관하게 같다고 보므로 약한 태그.
fn tag_of(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    format!("W/\"{}\"", hex::encode(&digest[..16]))
}

/// RFC 9110 §13.1.2: `If-None-Match` 는 약한 비교. `*` 는 무엇이든 맞는다.
fn matches(header: &HeaderValue, tag: &str) -> bool {
    let Ok(s) = header.to_str() else {
        return false;
    };
    let ours = opaque(tag);
    s.split(',')
        .map(str::trim)
        .any(|t| t == "*" || opaque(t) == ours)
}

fn opaque(tag: &str) -> &str {
    tag.strip_prefix("W/").unwrap_or(tag)
}

/// 304 에도 캐시 판단에 쓰는 머리는 그대로 둔다 (RFC 9110 §15.4.5).
fn copy_validators(from: &HeaderMap, to: &mut HeaderMap) {
    for name in [
        header::ETAG,
        header::CACHE_CONTROL,
        header::CONTENT_LOCATION,
        header::VARY,
    ] {
        if let Some(v) = from.get(&name) {
            to.insert(name, v.clone());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn weak_comparison_and_star() {
        let tag = tag_of(b"hello");
        assert!(tag.starts_with("W/\"") && tag.len() == 3 + 32 + 1);
        let strong = tag.trim_start_matches("W/").to_owned();
        assert!(matches(&HeaderValue::from_str(&tag).unwrap(), &tag));
        assert!(matches(&HeaderValue::from_str(&strong).unwrap(), &tag));
        assert!(matches(
            &HeaderValue::from_str(&format!("\"x\", {tag}")).unwrap(),
            &tag
        ));
        assert!(matches(&HeaderValue::from_static("*"), &tag));
        assert!(!matches(&HeaderValue::from_static("W/\"other\""), &tag));
        assert_ne!(tag_of(b"hello"), tag_of(b"hello!"));
    }
}
