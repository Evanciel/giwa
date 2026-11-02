//! Maru 공개 사이트, REST API, 관리 화면
//!
//! 모든 핸들러는 `maru_core::services` 만 부른다. 권한 판단은 서비스 레이어가 한다.

mod api;
mod error;
mod feeds;
mod public;

use std::future::Future;
use std::sync::Arc;
use std::time::Duration;

use axum::Router;
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::Response;
use maru_core::{Ctx, Result};
use maru_themes::Theme;
use tokio::net::TcpListener;
use tower_http::limit::RequestBodyLimitLayer;
use tower_http::timeout::TimeoutLayer;

/// 요청 본문 상한. Markdown 본문 1MB + 여유.
const BODY_LIMIT: usize = 2 * 1024 * 1024;
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Clone)]
pub struct AppState {
    pub ctx: Ctx,
    pub theme: Arc<Theme>,
}

pub fn router(state: AppState) -> Router {
    Router::new()
        .merge(public::routes())
        .nest("/api/v1", api::routes())
        .with_state(state)
        .layer(axum::middleware::map_response(security_headers))
        .layer(RequestBodyLimitLayer::new(BODY_LIMIT))
        .layer(TimeoutLayer::with_status_code(
            StatusCode::REQUEST_TIMEOUT,
            REQUEST_TIMEOUT,
        ))
}

async fn security_headers(mut res: Response) -> Response {
    let h = res.headers_mut();
    h.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    h.insert(
        header::REFERRER_POLICY,
        HeaderValue::from_static("strict-origin-when-cross-origin"),
    );
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
    axum::serve(listener, router(state))
        .with_graceful_shutdown(shutdown)
        .await
        .map_err(maru_core::Error::internal)
}
