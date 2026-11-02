//! Maru 공개 사이트, REST API, 관리 화면
//!
//! 모든 핸들러는 `maru_core::services` 만 부른다. 권한 판단은 서비스 레이어가 한다.

mod admin;
mod api;
mod error;
mod feeds;
mod public;
mod ratelimit;

use std::future::Future;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;

use axum::Router;
use axum::extract::{ConnectInfo, Request, State};
use axum::http::{HeaderValue, Method, StatusCode, header};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use maru_core::Actor;
use maru_core::model::Media;
use maru_core::services::{auth, media};
use maru_core::{Code, Ctx, Error, Result};
use maru_themes::{Admin, Theme};
use tokio::net::TcpListener;
use tokio::sync::Semaphore;
use tower_http::limit::RequestBodyLimitLayer;
use tower_http::timeout::TimeoutLayer;

pub use ratelimit::{Bucket, RateLimiter};

/// 요청 본문 상한. 이미지 업로드(10MB) + 여유. JSON·폼 추출은 axum 기본값(2MB)이 따로 막는다.
const BODY_LIMIT: usize = media::MAX_BYTES + 256 * 1024;
/// 업로드 경로만 axum 추출 상한을 이 값으로 올린다.
pub(crate) const UPLOAD_LIMIT: usize = BODY_LIMIT;
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
/// 만료 세션·링크·요청 제한 키 정리 주기.
const PURGE_EVERY: Duration = Duration::from_secs(10 * 60);
/// argon2 를 동시에 계산하는 최대 수.
const LOGIN_CONCURRENCY: usize = 2;
/// 이미지 디코딩·인코딩을 동시에 하는 최대 수.
const UPLOAD_CONCURRENCY: usize = 2;

#[derive(Clone)]
pub struct AppState {
    pub ctx: Ctx,
    pub theme: Arc<Theme>,
    pub admin: Arc<Admin>,
    pub limiter: Arc<RateLimiter>,
    pub login_gate: Arc<Semaphore>,
    pub upload_gate: Arc<Semaphore>,
}

impl AppState {
    pub fn new(ctx: Ctx, theme: Theme) -> Self {
        Self {
            ctx,
            theme: Arc::new(theme),
            admin: Arc::new(Admin::new()),
            limiter: Arc::new(RateLimiter::default()),
            login_gate: Arc::new(Semaphore::new(LOGIN_CONCURRENCY)),
            upload_gate: Arc::new(Semaphore::new(UPLOAD_CONCURRENCY)),
        }
    }
}

pub fn router(state: AppState) -> Router {
    Router::new()
        .merge(public::routes())
        .merge(admin::routes())
        .nest("/api/v1", api::routes())
        .layer(middleware::from_fn_with_state(state.clone(), rate_limit))
        .with_state(state)
        .layer(middleware::map_response(security_headers))
        .layer(RequestBodyLimitLayer::new(BODY_LIMIT))
        .layer(TimeoutLayer::with_status_code(
            StatusCode::REQUEST_TIMEOUT,
            REQUEST_TIMEOUT,
        ))
}

/// 연결 주소로만 센다. 전달 헤더(`X-Forwarded-For`)는 보지 않는다.
async fn rate_limit(State(state): State<AppState>, req: Request, next: Next) -> Response {
    let ip = req
        .extensions()
        .get::<ConnectInfo<SocketAddr>>()
        .map(|c| c.0.ip())
        .unwrap_or(IpAddr::V4(Ipv4Addr::UNSPECIFIED));
    let path = req.uri().path();
    let login = req.method() == Method::POST
        && (path == "/admin/login" || path.starts_with("/admin/link/"));
    let allowed =
        state.limiter.check(ip, Bucket::All) && (!login || state.limiter.check(ip, Bucket::Login));
    if allowed {
        return next.run(req).await;
    }
    tracing::warn!(%ip, path, "요청 제한");
    let err = Error::new(Code::RateLimited);
    let mut res = if path.starts_with("/api/") {
        error::ApiError(err).into_response()
    } else {
        (
            StatusCode::TOO_MANY_REQUESTS,
            [(header::CONTENT_TYPE, "text/plain; charset=utf-8")],
            err.message(),
        )
            .into_response()
    };
    res.headers_mut()
        .insert(header::RETRY_AFTER, HeaderValue::from_static("60"));
    res
}

async fn security_headers(mut res: Response) -> Response {
    let h = res.headers_mut();
    h.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    h.entry(header::REFERRER_POLICY)
        .or_insert(HeaderValue::from_static("strict-origin-when-cross-origin"));
    h.entry(header::CONTENT_SECURITY_POLICY)
        .or_insert(HeaderValue::from_static(
            "default-src 'self'; img-src 'self' https: data:; style-src 'self'; \
         script-src 'self'; object-src 'none'; base-uri 'self'; frame-ancestors 'self'",
        ));
    res
}

/// 이미 바인딩된 리스너로 서버를 돌린다. `shutdown` 이 끝나면 진행 중인 요청을 마치고 멈춘다.
pub async fn serve(
    state: AppState,
    listener: TcpListener,
    shutdown: impl Future<Output = ()> + Send + 'static,
) -> Result<()> {
    let purge = tokio::spawn(purge_loop(state.clone()));
    let res = axum::serve(
        listener,
        router(state).into_make_service_with_connect_info::<SocketAddr>(),
    )
    .with_graceful_shutdown(shutdown)
    .await
    .map_err(maru_core::Error::internal);
    purge.abort();
    res
}

async fn purge_loop(state: AppState) {
    let mut tick = tokio::time::interval(PURGE_EVERY);
    loop {
        tick.tick().await;
        match auth::purge_expired(&state.ctx).await {
            Ok(n) if n > 0 => tracing::info!(removed = n, "만료된 세션·링크 정리"),
            Ok(_) => {}
            Err(e) => tracing::warn!(error = ?e, "세션 정리 실패"),
        }
        state.limiter.purge();
    }
}

/// 업로드는 CPU 를 오래 쓰므로 비동기 작업 스레드 밖에서, 동시에 몇 개까지만 돌린다.
/// REST 와 관리 화면이 같은 서비스(`media::upload`)를 이 경로로 부른다.
pub(crate) async fn upload_media(
    state: &AppState,
    actor: Actor,
    input: media::Upload,
) -> Result<Media> {
    let _permit = state
        .upload_gate
        .acquire()
        .await
        .map_err(maru_core::Error::internal)?;
    let ctx = state.ctx.clone();
    let handle = tokio::runtime::Handle::current();
    tokio::task::spawn_blocking(move || handle.block_on(media::upload(&ctx, &actor, input)))
        .await
        .map_err(maru_core::Error::internal)?
}
