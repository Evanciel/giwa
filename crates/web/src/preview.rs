//! changeset 미리보기 (SPEC §6, ADR-0122). 사본 DB 를 쓰는 같은 라우터를 하나 더 만들어 둔다.
//!
//! `/__preview/<id>` 에서 들어오면 쿠키를 받고, 그 쿠키가 있는 동안 공개 화면은 사본에서 온다.
//! 링크가 그대로 통하게 주소를 바꾸지 않는다. 관리 화면·API 는 언제나 실제 사이트다.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use axum::Router;
use axum::body::{Body, to_bytes};
use axum::extract::{ConnectInfo, Path, Query, Request, State};
use axum::http::{HeaderMap, HeaderValue, Method, StatusCode, header};
use axum::middleware::Next;
use axum::response::{IntoResponse, Redirect, Response};
use axum::routing::get;
use maru_core::model::{Changeset, ChangesetStatus};
use maru_core::services::auth;
use maru_core::services::changesets::{self, Sandbox};
use maru_core::services::stats::StatCounter;
use maru_core::services::views::ViewCounter;
use maru_core::{Error, Result};
use serde::Deserialize;
use tokio::sync::Mutex as AsyncMutex;
use tower::Service;

use crate::admin::{self, cookie, set_cookie};
use crate::{AppState, router};

pub(crate) const COOKIE: &str = "maru_preview";
/// 사본은 파일이고 연결을 쥔다. 넘치면 오래된 것부터 닫는다.
const MAX_OPEN: usize = 3;
const GRANT_LIFE: Duration = Duration::from_secs(60 * 60);
const MAX_HTML: usize = 8 * 1024 * 1024;
/// 언제나 실제 사이트로 가는 경로.
const REAL: [&str; 4] = ["/admin", "/api/", "/mcp", "/__preview"];

const BAND_CSS: &str = ".maru-preview-band{position:sticky;top:0;z-index:9999;display:flex;\
flex-wrap:wrap;gap:.25rem .75rem;align-items:center;padding:.5rem 1rem;background:#1f2933;\
color:#fff;font:600 .875rem/1.5 system-ui,sans-serif}.maru-preview-band strong{color:#ffd166}\
.maru-preview-band span{font-weight:400;min-width:0;overflow-wrap:anywhere}\
.maru-preview-band a{margin-left:auto;color:#fff;text-decoration:underline}\n@media (max-width:640px){.maru-preview-band{position:static}}";

struct Open {
    id: String,
    ops: Vec<serde_json::Value>,
    router: Router,
}

struct Grant {
    id: String,
    until: Instant,
}

#[derive(Default)]
struct Inner {
    open: Vec<Open>,
    grants: HashMap<String, Grant>,
}

pub struct Previews {
    sandbox: Arc<dyn Sandbox>,
    inner: Mutex<Inner>,
    /// 사본 만들기는 한 번에 하나만.
    building: AsyncMutex<()>,
}

fn key(id: &str) -> String {
    format!("preview-{id}")
}

impl Previews {
    pub fn new(sandbox: Arc<dyn Sandbox>) -> Self {
        Self {
            sandbox,
            inner: Mutex::new(Inner::default()),
            building: AsyncMutex::new(()),
        }
    }

    fn locked(&self) -> Result<std::sync::MutexGuard<'_, Inner>> {
        self.inner
            .lock()
            .map_err(|_| Error::internal("미리보기 목록 잠금 실패"))
    }

    fn grant_of(&self, raw: &str) -> Option<String> {
        let inner = self.inner.lock().ok()?;
        let grant = inner.grants.get(raw)?;
        (grant.until > Instant::now()).then(|| grant.id.clone())
    }

    fn new_grant(&self, id: &str) -> Result<String> {
        let raw = auth::new_secret()?;
        let mut inner = self.locked()?;
        let now = Instant::now();
        inner.grants.retain(|_, g| g.until > now);
        inner.grants.insert(
            raw.clone(),
            Grant {
                id: id.to_owned(),
                until: now + GRANT_LIFE,
            },
        );
        Ok(raw)
    }

    /// 사본을 닫고 이 묶음의 쿠키를 무효로 한다.
    pub async fn close(&self, id: &str) {
        if let Ok(mut inner) = self.inner.lock() {
            inner.open.retain(|o| o.id != id);
            inner.grants.retain(|_, g| g.id != id);
        }
        self.sandbox.close(&key(id)).await;
    }

    pub async fn close_all(&self) {
        let ids: Vec<String> = match self.inner.lock() {
            Ok(inner) => inner.open.iter().map(|o| o.id.clone()).collect(),
            Err(_) => Vec::new(),
        };
        for id in ids {
            self.close(&id).await;
        }
    }

    /// 이 묶음의 라우터. 없거나 연산이 바뀌었으면, 또는 `fresh` 면 새로 만든다.
    async fn router(&self, state: &AppState, cs: &Changeset, fresh: bool) -> Result<Router> {
        let _building = self.building.lock().await;
        if !fresh
            && let Some(o) = self.locked()?.open.iter().find(|o| o.id == cs.id)
            && o.ops == cs.ops
        {
            return Ok(o.router.clone());
        }
        let copy =
            changesets::sandboxed(&state.ctx, self.sandbox.as_ref(), &key(&cs.id), cs).await?;
        let mut inner_state = state.clone();
        inner_state.ctx = copy;
        inner_state.previews = None;
        // 사본에서 일어난 일이 실제 통계·편지로 새지 않는다.
        inner_state.views = Arc::new(ViewCounter::new());
        inner_state.stats = Arc::new(StatCounter::new());
        inner_state.mailer = None;
        inner_state.link_fetcher = None;
        let built = router(inner_state);
        let evicted = {
            let mut inner = self.locked()?;
            inner.open.retain(|o| o.id != cs.id);
            inner.open.push(Open {
                id: cs.id.clone(),
                ops: cs.ops.clone(),
                router: built.clone(),
            });
            if inner.open.len() > MAX_OPEN {
                Some(inner.open.remove(0).id)
            } else {
                None
            }
        };
        if let Some(old) = evicted {
            self.sandbox.close(&key(&old)).await;
        }
        Ok(built)
    }
}

pub(crate) fn routes() -> Router<AppState> {
    Router::new()
        .route("/__preview/band.css", get(band_css))
        .route("/__preview/exit", get(exit))
        .route("/__preview/{id}", get(enter))
}

async fn band_css() -> Response {
    (
        [(header::CONTENT_TYPE, "text/css; charset=utf-8")],
        BAND_CSS,
    )
        .into_response()
}

fn with_cleared_cookie(state: &AppState, mut res: Response) -> Response {
    res.headers_mut()
        .append(header::SET_COOKIE, set_cookie(state, COOKIE, "", Some(0)));
    res
}

async fn exit(State(state): State<AppState>) -> Response {
    with_cleared_cookie(&state, Redirect::to("/").into_response())
}

fn plain(status: StatusCode, message: String) -> Response {
    (
        status,
        [(header::CONTENT_TYPE, "text/plain; charset=utf-8")],
        message,
    )
        .into_response()
}

fn refused(e: &Error) -> Response {
    let status =
        StatusCode::from_u16(e.code().http_status()).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
    plain(status, e.message())
}

#[derive(Deserialize)]
struct EnterQuery {
    token: Option<String>,
}

/// 미리보기에 들어간다. 소유자·관리자 세션 또는 1회용 토큰.
async fn enter(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Query(query): Query<EnterQuery>,
    headers: HeaderMap,
) -> Response {
    let Some(previews) = state.previews.clone() else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let cs = match &query.token {
        Some(token) => changesets::redeem_preview_token(&state.ctx, &id, token).await,
        None => {
            let Some(actor) = admin::viewer(&state, &headers).await else {
                return Redirect::to(&format!("/admin/login?next=/__preview/{id}")).into_response();
            };
            changesets::for_preview(&state.ctx, &actor, &id).await
        }
    };
    let cs = match cs {
        Ok(cs) => cs,
        Err(e) => return refused(&e),
    };
    // 지금 사이트로 사본을 새로 만든다. 연산이 틀렸으면 여기서 알려 준다.
    if let Err(e) = previews.router(&state, &cs, true).await {
        return refused(&e);
    }
    let raw = match previews.new_grant(&id) {
        Ok(raw) => raw,
        Err(e) => return refused(&e),
    };
    let mut res = Redirect::to("/").into_response();
    res.headers_mut().append(
        header::SET_COOKIE,
        set_cookie(&state, COOKIE, &raw, Some(GRANT_LIFE.as_secs() as i64)),
    );
    res
}

/// 쿠키가 있으면 공개 화면을 사본에서 낸다.
pub(crate) async fn mode(State(state): State<AppState>, mut req: Request, next: Next) -> Response {
    let Some(previews) = state.previews.clone() else {
        return next.run(req).await;
    };
    let path = req.uri().path();
    if REAL.iter().any(|p| path.starts_with(p)) {
        return next.run(req).await;
    }
    let Some(raw) = cookie(req.headers(), COOKIE) else {
        return next.run(req).await;
    };
    let Some(id) = previews.grant_of(&raw) else {
        return with_cleared_cookie(&state, next.run(req).await);
    };
    let cs = match state.ctx.store.changeset(&id).await {
        Ok(Some(cs)) if cs.status == ChangesetStatus::Preview => cs,
        Ok(_) => {
            // 그 사이 적용했거나 버렸다.
            previews.close(&id).await;
            return with_cleared_cookie(&state, next.run(req).await);
        }
        Err(e) => return refused(&e),
    };
    if !matches!(*req.method(), Method::GET | Method::HEAD) {
        return plain(
            StatusCode::FORBIDDEN,
            "미리보기 중에는 글쓰기·로그인 같은 일을 할 수 없습니다. 위쪽 띠에서 미리보기를 끝내세요."
                .to_owned(),
        );
    }
    let mut copy = match previews.router(&state, &cs, false).await {
        Ok(router) => router,
        Err(e) => return with_cleared_cookie(&state, refused(&e)),
    };
    // 실제 화면의 캐시와 섞이지 않게 한다.
    req.headers_mut().remove(header::IF_NONE_MATCH);
    req.headers_mut().remove(header::IF_MODIFIED_SINCE);
    let res = match copy.call(fresh_request(req)).await {
        Ok(res) => res,
        Err(never) => match never {},
    };
    mark(res, &cs).await
}

/// 바깥 라우터가 찾아 둔 경로 값이 안쪽 라우터의 값에 겹치지 않게 확장을 비운다. 연결 주소만 옮긴다.
fn fresh_request(req: Request) -> Request {
    let (parts, body) = req.into_parts();
    let mut out = Request::new(body);
    *out.method_mut() = parts.method;
    *out.uri_mut() = parts.uri;
    *out.version_mut() = parts.version;
    *out.headers_mut() = parts.headers;
    if let Some(peer) = parts.extensions.get::<ConnectInfo<SocketAddr>>() {
        out.extensions_mut().insert(*peer);
    }
    out
}

fn escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

fn band(cs: &Changeset) -> String {
    format!(
        "<link rel=\"stylesheet\" href=\"/__preview/band.css\">\
         <div class=\"maru-preview-band\" role=\"status\"><strong>미리보기</strong>\
         <span>{}. 아직 사이트에 반영되지 않았습니다.</span>\
         <a href=\"/__preview/exit\">미리보기 끝내기</a></div>",
        escape(&cs.title)
    )
}

/// `<body ...>` 바로 뒤. 없으면 맨 앞.
fn insert_band(html: &str, band: &str) -> String {
    let at = html
        .find("<body")
        .and_then(|i| html[i..].find('>').map(|j| i + j + 1))
        .unwrap_or(0);
    format!("{}{band}{}", &html[..at], &html[at..])
}

/// 검색·브라우저 캐시에 남지 않게 하고, HTML 이면 위쪽 띠를 붙인다.
async fn mark(res: Response, cs: &Changeset) -> Response {
    let (mut parts, body) = res.into_parts();
    parts.headers.remove(header::ETAG);
    parts.headers.remove(header::LAST_MODIFIED);
    parts
        .headers
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    parts.headers.insert(
        header::HeaderName::from_static("x-robots-tag"),
        HeaderValue::from_static("noindex"),
    );
    let is_html = parts
        .headers
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.starts_with("text/html"));
    if !is_html {
        return Response::from_parts(parts, body);
    }
    let bytes = match to_bytes(body, MAX_HTML).await {
        Ok(bytes) => bytes,
        Err(e) => return plain(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    };
    parts.headers.remove(header::CONTENT_LENGTH);
    let out = insert_band(&String::from_utf8_lossy(&bytes), &band(cs));
    Response::from_parts(parts, Body::from(out))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 띠는_body_바로_뒤에_들어간다() {
        assert_eq!(
            insert_band("<html><body class=\"a\"><p>b</p></body>", "<i>"),
            "<html><body class=\"a\"><i><p>b</p></body>"
        );
        assert_eq!(insert_band("<p>b</p>", "<i>"), "<i><p>b</p>");
    }

    #[test]
    fn 제목은_글자로_넣는다() {
        assert_eq!(
            escape("<a href=\"x\">&"),
            "&lt;a href=&quot;x&quot;&gt;&amp;"
        );
    }
}
