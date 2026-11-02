//! REST API `/api/v1`. 인증은 bearer 토큰만 받는다 (쿠키 인증이 없어 CSRF 표면이 없다).

use axum::extract::rejection::JsonRejection;
use axum::extract::{FromRequestParts, Path, Query, State};
use axum::http::StatusCode;
use axum::http::header::AUTHORIZATION;
use axum::http::request::Parts;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use maru_core::model::{MenuItem, PageReq, PageStatus, PostStatus};
use maru_core::output::Success;
use maru_core::services::{anonymous_actor, boards, design, menus, pages, posts, settings, tokens};
use maru_core::{Actor, Code, Error};
use serde::Deserialize;

use crate::AppState;
use crate::error::ApiError;

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/boards", get(list_boards).post(create_board))
        .route(
            "/boards/{slug}",
            get(get_board).patch(update_board).delete(delete_board),
        )
        .route("/posts", get(list_posts).post(create_post))
        .route("/posts/{id}", get(get_post).patch(update_post))
        .route("/posts/{id}/{action}", post(post_action))
        .route("/pages", get(list_pages).post(create_page))
        .route("/pages/{slug}", get(get_page).patch(update_page))
        .route("/pages/{slug}/{action}", post(page_action))
        .route("/menus/{location}", get(get_menu).put(put_menu))
        .route("/settings", get(get_settings).patch(update_settings))
        .route("/design/tokens", get(get_tokens).patch(update_tokens))
        .fallback(|| async { ApiError(Error::new(Code::NotFound)) })
}

/// `Authorization: Bearer <토큰>` 이 있으면 에이전트, 없으면 비로그인.
pub struct ApiActor(pub Actor);

impl FromRequestParts<AppState> for ApiActor {
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, state: &AppState) -> Result<Self, ApiError> {
        match parts.headers.get(AUTHORIZATION) {
            None => Ok(Self(anonymous_actor(&state.ctx).await?)),
            Some(value) => {
                let secret = value
                    .to_str()
                    .ok()
                    .and_then(|v| v.strip_prefix("Bearer "))
                    .unwrap_or_default()
                    .trim();
                Ok(Self(tokens::authenticate(&state.ctx, secret).await?))
            }
        }
    }
}

fn ok<T: serde::Serialize>(status: StatusCode, data: T) -> Response {
    (status, Json(Success::new(data))).into_response()
}

fn bad_json(e: JsonRejection) -> ApiError {
    ApiError(
        Error::new(Code::Validation)
            .with_message("error.json_body")
            .arg("detail", e.body_text()),
    )
}

async fn list_boards(
    State(state): State<AppState>,
    ApiActor(actor): ApiActor,
) -> Result<Response, ApiError> {
    let list = boards::list(&state.ctx, &actor).await?;
    Ok(ok(StatusCode::OK, list))
}

#[derive(Debug, Deserialize)]
struct ListQuery {
    board: Option<String>,
    status: Option<String>,
    page: Option<u32>,
    per_page: Option<u32>,
}

async fn list_posts(
    State(state): State<AppState>,
    ApiActor(actor): ApiActor,
    Query(q): Query<ListQuery>,
) -> Result<Response, ApiError> {
    let status = match q.status.as_deref() {
        Some(s) => Some(PostStatus::parse(s).ok_or_else(|| {
            Error::new(Code::Validation)
                .with_message("error.post_status")
                .arg("value", s)
        })?),
        None => None,
    };
    let page = PageReq::new(q.page.unwrap_or(1), q.per_page.unwrap_or(20));
    let list = posts::list(&state.ctx, &actor, q.board.as_deref(), status, page).await?;
    Ok(ok(StatusCode::OK, list))
}

async fn create_post(
    State(state): State<AppState>,
    ApiActor(actor): ApiActor,
    body: Result<Json<posts::NewPost>, JsonRejection>,
) -> Result<Response, ApiError> {
    let Json(input) = body.map_err(bad_json)?;
    let post = posts::create_draft(&state.ctx, &actor, input).await?;
    Ok(ok(StatusCode::CREATED, post))
}

async fn get_post(
    State(state): State<AppState>,
    ApiActor(actor): ApiActor,
    Path(id): Path<String>,
) -> Result<Response, ApiError> {
    let post = posts::get(&state.ctx, &actor, &id).await?;
    Ok(ok(StatusCode::OK, post))
}

/// `POST /posts/{id}/{publish|submit|hide|delete|restore}`
async fn post_action(
    State(state): State<AppState>,
    ApiActor(actor): ApiActor,
    Path((id, action)): Path<(String, String)>,
) -> Result<Response, ApiError> {
    let ctx = &state.ctx;
    let post = match action.as_str() {
        "publish" => posts::publish(ctx, &actor, &id).await?,
        "submit" => posts::submit(ctx, &actor, &id).await?,
        "hide" => posts::hide(ctx, &actor, &id).await?,
        "delete" => posts::delete(ctx, &actor, &id).await?,
        "restore" => posts::restore(ctx, &actor, &id).await?,
        _ => return Err(ApiError(Error::new(Code::NotFound))),
    };
    Ok(ok(StatusCode::OK, post))
}

async fn update_post(
    State(state): State<AppState>,
    ApiActor(actor): ApiActor,
    Path(id): Path<String>,
    body: Result<Json<posts::PostPatch>, JsonRejection>,
) -> Result<Response, ApiError> {
    let Json(patch) = body.map_err(bad_json)?;
    let post = posts::update(&state.ctx, &actor, &id, patch).await?;
    Ok(ok(StatusCode::OK, post))
}

async fn create_board(
    State(state): State<AppState>,
    ApiActor(actor): ApiActor,
    body: Result<Json<boards::NewBoard>, JsonRejection>,
) -> Result<Response, ApiError> {
    let Json(input) = body.map_err(bad_json)?;
    let board = boards::create(&state.ctx, &actor, input).await?;
    Ok(ok(StatusCode::CREATED, board))
}

async fn get_board(
    State(state): State<AppState>,
    ApiActor(actor): ApiActor,
    Path(slug): Path<String>,
) -> Result<Response, ApiError> {
    Ok(ok(
        StatusCode::OK,
        boards::show(&state.ctx, &actor, &slug).await?,
    ))
}

async fn update_board(
    State(state): State<AppState>,
    ApiActor(actor): ApiActor,
    Path(slug): Path<String>,
    body: Result<Json<boards::BoardPatch>, JsonRejection>,
) -> Result<Response, ApiError> {
    let Json(patch) = body.map_err(bad_json)?;
    Ok(ok(
        StatusCode::OK,
        boards::update(&state.ctx, &actor, &slug, patch).await?,
    ))
}

async fn delete_board(
    State(state): State<AppState>,
    ApiActor(actor): ApiActor,
    Path(slug): Path<String>,
) -> Result<Response, ApiError> {
    Ok(ok(
        StatusCode::OK,
        boards::delete(&state.ctx, &actor, &slug).await?,
    ))
}

#[derive(Debug, Deserialize)]
struct PageListQuery {
    status: Option<String>,
}

async fn list_pages(
    State(state): State<AppState>,
    ApiActor(actor): ApiActor,
    Query(q): Query<PageListQuery>,
) -> Result<Response, ApiError> {
    let status = match q.status.as_deref() {
        Some(s) => Some(PageStatus::parse(s).ok_or_else(|| {
            Error::new(Code::Validation)
                .with_message("error.post_status")
                .arg("value", s)
        })?),
        None => None,
    };
    Ok(ok(
        StatusCode::OK,
        pages::list(&state.ctx, &actor, status).await?,
    ))
}

async fn create_page(
    State(state): State<AppState>,
    ApiActor(actor): ApiActor,
    body: Result<Json<pages::NewPage>, JsonRejection>,
) -> Result<Response, ApiError> {
    let Json(input) = body.map_err(bad_json)?;
    let page = pages::create(&state.ctx, &actor, input).await?;
    Ok(ok(StatusCode::CREATED, page))
}

async fn get_page(
    State(state): State<AppState>,
    ApiActor(actor): ApiActor,
    Path(slug): Path<String>,
) -> Result<Response, ApiError> {
    Ok(ok(
        StatusCode::OK,
        pages::get(&state.ctx, &actor, &slug).await?,
    ))
}

async fn update_page(
    State(state): State<AppState>,
    ApiActor(actor): ApiActor,
    Path(slug): Path<String>,
    body: Result<Json<pages::PagePatch>, JsonRejection>,
) -> Result<Response, ApiError> {
    let Json(patch) = body.map_err(bad_json)?;
    Ok(ok(
        StatusCode::OK,
        pages::update(&state.ctx, &actor, &slug, patch).await?,
    ))
}

/// `POST /pages/{slug}/{publish|hide|delete|restore}`
async fn page_action(
    State(state): State<AppState>,
    ApiActor(actor): ApiActor,
    Path((slug, action)): Path<(String, String)>,
) -> Result<Response, ApiError> {
    let ctx = &state.ctx;
    let page = match action.as_str() {
        "publish" => pages::publish(ctx, &actor, &slug).await?,
        "hide" => pages::hide(ctx, &actor, &slug).await?,
        "delete" => pages::delete(ctx, &actor, &slug).await?,
        "restore" => pages::restore(ctx, &actor, &slug).await?,
        _ => return Err(ApiError(Error::new(Code::NotFound))),
    };
    Ok(ok(StatusCode::OK, page))
}

/// 메뉴는 공개 화면에 그대로 보이므로 읽기에는 권한이 필요 없다.
async fn get_menu(
    State(state): State<AppState>,
    Path(location): Path<String>,
) -> Result<Response, ApiError> {
    Ok(ok(StatusCode::OK, menus::get(&state.ctx, &location).await?))
}

async fn put_menu(
    State(state): State<AppState>,
    ApiActor(actor): ApiActor,
    Path(location): Path<String>,
    body: Result<Json<Vec<MenuItem>>, JsonRejection>,
) -> Result<Response, ApiError> {
    let Json(items) = body.map_err(bad_json)?;
    Ok(ok(
        StatusCode::OK,
        menus::set(&state.ctx, &actor, &location, items).await?,
    ))
}

async fn get_settings(State(state): State<AppState>) -> Result<Response, ApiError> {
    Ok(ok(StatusCode::OK, settings::site(&state.ctx).await?))
}

async fn update_settings(
    State(state): State<AppState>,
    ApiActor(actor): ApiActor,
    body: Result<Json<settings::SettingsPatch>, JsonRejection>,
) -> Result<Response, ApiError> {
    let Json(patch) = body.map_err(bad_json)?;
    Ok(ok(
        StatusCode::OK,
        settings::update(&state.ctx, &actor, patch).await?,
    ))
}

async fn get_tokens(State(state): State<AppState>) -> Result<Response, ApiError> {
    Ok(ok(StatusCode::OK, design::tokens(&state.ctx).await?))
}

/// 부분 JSON 을 현재 토큰에 겹친다.
async fn update_tokens(
    State(state): State<AppState>,
    ApiActor(actor): ApiActor,
    body: Result<Json<serde_json::Value>, JsonRejection>,
) -> Result<Response, ApiError> {
    let Json(patch) = body.map_err(bad_json)?;
    Ok(ok(
        StatusCode::OK,
        design::update(&state.ctx, &actor, patch).await?,
    ))
}
