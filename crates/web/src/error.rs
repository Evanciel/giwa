//! 오류 응답. REST 는 SPEC §10.1 JSON, 공개 화면은 테마의 `error.html`.
//! `INTERNAL` 은 요청 ID 만 내보내고 원인은 로그에 남긴다.

use axum::Json;
use axum::http::StatusCode;
use axum::response::{Html, IntoResponse, Response};
use maru_core::output::Failure;
use maru_core::{Code, Error, clock};
use serde_json::json;

use crate::AppState;

fn status(err: &Error) -> StatusCode {
    StatusCode::from_u16(err.code().http_status()).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR)
}

fn request_id(err: &Error) -> Option<String> {
    if err.code() != Code::Internal {
        return None;
    }
    let id = clock::new_id();
    tracing::error!(request_id = %id, error = ?err, source = ?std::error::Error::source(err), "내부 오류");
    Some(id)
}

/// REST 오류.
pub struct ApiError(pub Error);

impl From<Error> for ApiError {
    fn from(e: Error) -> Self {
        Self(e)
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let rid = request_id(&self.0);
        let body = Failure::new(self.0.body(rid.as_deref()));
        (status(&self.0), Json(body)).into_response()
    }
}

/// 공개 화면 오류 페이지.
pub async fn page(state: &AppState, err: &Error) -> Response {
    let site = crate::public::site_json(state).await;
    let rid = request_id(err);
    let body = err.body(rid.as_deref());
    let code = status(err);
    let message = match err.code() {
        Code::NotFound => "페이지를 찾을 수 없습니다.".to_owned(),
        _ => body.message.clone(),
    };
    let html = state
        .theme
        .render(
            "error.html",
            &json!({
                "meta": { "title": format!("{} · {}", code.as_u16(), site["name"].as_str().unwrap_or("Maru")), "noindex": true },
                "site": site,
                "status": code.as_u16(),
                "message": message,
                "request_id": body.request_id,
            }),
        )
        .unwrap_or_else(|_| format!("<h1>{}</h1>", code.as_u16()));
    (code, Html(html)).into_response()
}
