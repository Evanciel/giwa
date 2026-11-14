//! `/mcp`: 토큰으로만 여는 MCP 입구 (GAP S8, ADR-0117). 토큰 없는 요청은 프로토콜까지 가지 않는다.

use axum::Router;
use axum::extract::{Request, State};
use axum::http::header::{AUTHORIZATION, WWW_AUTHENTICATE};
use axum::http::{HeaderValue, StatusCode};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use giwa_core::services::tokens;

use crate::AppState;
use crate::error::ApiError;

pub fn routes(state: &AppState) -> Router<AppState> {
    Router::new()
        .nest_service(
            "/mcp",
            giwa_agent::http_service(
                state.ctx.clone(),
                state.modules.clone(),
                state.previews.as_ref().map(|p| p.sandbox()),
            ),
        )
        .layer(middleware::from_fn_with_state(state.clone(), require_token))
}

async fn require_token(State(state): State<AppState>, req: Request, next: Next) -> Response {
    let secret = req
        .headers()
        .get(AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .unwrap_or_default()
        .trim();
    match tokens::authenticate(&state.ctx, secret).await {
        Ok(_) => next.run(req).await,
        Err(e) => {
            let mut res = ApiError(e).into_response();
            if res.status() == StatusCode::UNAUTHORIZED {
                res.headers_mut()
                    .insert(WWW_AUTHENTICATE, HeaderValue::from_static("Bearer"));
            }
            res
        }
    }
}
