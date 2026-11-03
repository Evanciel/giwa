//! REST API `/api/v1`. 인증은 bearer 토큰만 받는다 (쿠키 인증이 없어 CSRF 표면이 없다).

use axum::body::Bytes;
use axum::extract::rejection::JsonRejection;
use axum::extract::{DefaultBodyLimit, FromRequestParts, Path, Query, State};
use axum::http::StatusCode;
use axum::http::header::AUTHORIZATION;
use axum::http::request::Parts;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use maru_core::model::{MenuItem, PageReq, PageStatus, PostStatus};
use maru_core::output::Success;
use maru_core::services::{
    anonymous_actor, boards, categories, design, files, fonts, media, menus, pages, posts,
    settings, tags, templates, tokens,
};
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
        .route("/boards/{slug}/posts", get(board_posts))
        .route(
            "/boards/{slug}/categories",
            get(list_categories).post(create_category),
        )
        .route(
            "/boards/{slug}/categories/{category}",
            axum::routing::patch(update_category).delete(delete_category),
        )
        .route("/tags", get(list_tags))
        .route("/tags/{slug}", get(tag_posts))
        .route("/posts", get(list_posts).post(create_post))
        .route("/posts/{id}", get(get_post).patch(update_post))
        .route("/posts/{id}/{action}", post(post_action))
        .route("/pages", get(list_pages).post(create_page))
        .route("/pages/{slug}", get(get_page).patch(update_page))
        .route("/pages/{slug}/{action}", post(page_action))
        .route("/menus/{location}", get(get_menu).put(put_menu))
        .route("/settings", get(get_settings).patch(update_settings))
        .route("/design/tokens", get(get_tokens).patch(update_tokens))
        .route("/fonts", get(list_fonts).post(add_font))
        .route(
            "/fonts/{slug}",
            get(get_font).patch(update_font).delete(remove_font),
        )
        .route(
            "/fonts/{slug}/files",
            post(upload_font_file).layer(DefaultBodyLimit::max(crate::FONT_UPLOAD_LIMIT)),
        )
        .route(
            "/fonts/{slug}/files/{weight}",
            axum::routing::delete(remove_font_file),
        )
        .route(
            "/media",
            get(list_media)
                .post(upload_media)
                .layer(crate::upload_limit()),
        )
        .route(
            "/media/{id}",
            get(get_media).patch(update_media).delete(delete_media),
        )
        .route(
            "/files",
            get(list_files)
                .post(upload_file)
                .layer(crate::upload_limit()),
        )
        .route("/files/{id}", get(get_file).delete(delete_file))
        .route("/templates", get(list_templates).post(save_template))
        .route("/templates/{id}", axum::routing::delete(delete_template))
        .route("/preview", post(preview))
        .route("/slug-check", get(check_slug))
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

async fn list_fonts(State(state): State<AppState>) -> Result<Response, ApiError> {
    Ok(ok(StatusCode::OK, fonts::list(&state.ctx).await?))
}

async fn get_font(
    State(state): State<AppState>,
    Path(slug): Path<String>,
) -> Result<Response, ApiError> {
    Ok(ok(StatusCode::OK, fonts::get(&state.ctx, &slug).await?))
}

/// 서체 만들기 (JSON). 같은 slug·이름이 있으면 그 서체를 맞춰 돌려준다.
async fn add_font(
    State(state): State<AppState>,
    ApiActor(actor): ApiActor,
    body: Result<Json<fonts::NewFont>, JsonRejection>,
) -> Result<Response, ApiError> {
    let Json(input) = body.map_err(bad_json)?;
    Ok(ok(
        StatusCode::CREATED,
        fonts::add(&state.ctx, &actor, input).await?,
    ))
}

async fn update_font(
    State(state): State<AppState>,
    ApiActor(actor): ApiActor,
    Path(slug): Path<String>,
    body: Result<Json<fonts::FontPatch>, JsonRejection>,
) -> Result<Response, ApiError> {
    let Json(patch) = body.map_err(bad_json)?;
    Ok(ok(
        StatusCode::OK,
        fonts::update(&state.ctx, &actor, &slug, patch).await?,
    ))
}

async fn remove_font(
    State(state): State<AppState>,
    ApiActor(actor): ApiActor,
    Path(slug): Path<String>,
) -> Result<Response, ApiError> {
    Ok(ok(
        StatusCode::OK,
        fonts::remove(&state.ctx, &actor, &slug).await?,
    ))
}

#[derive(Debug, Deserialize)]
struct FontFileQuery {
    weight: Option<i64>,
    #[serde(default)]
    italic: Option<u8>,
}

/// `POST /fonts/{slug}/files?weight=700&italic=1`, 본문은 서체 파일 바이트 그대로 (`/media` 와 같은 방식).
async fn upload_font_file(
    State(state): State<AppState>,
    ApiActor(actor): ApiActor,
    Path(slug): Path<String>,
    Query(q): Query<FontFileQuery>,
    body: Bytes,
) -> Result<Response, ApiError> {
    let input = fonts::FontUpload {
        weight: q.weight.unwrap_or(400),
        italic: q.italic.unwrap_or(0) != 0,
        bytes: body.to_vec(),
    };
    Ok(ok(
        StatusCode::CREATED,
        fonts::add_file(&state.ctx, &actor, &slug, input).await?,
    ))
}

async fn remove_font_file(
    State(state): State<AppState>,
    ApiActor(actor): ApiActor,
    Path((slug, weight)): Path<(String, i64)>,
    Query(q): Query<FontFileQuery>,
) -> Result<Response, ApiError> {
    let italic = q.italic.unwrap_or(0) != 0;
    Ok(ok(
        StatusCode::OK,
        fonts::remove_file(&state.ctx, &actor, &slug, weight, italic).await?,
    ))
}

#[derive(Debug, Deserialize)]
struct UploadQuery {
    filename: String,
    alt: Option<String>,
}

/// `POST /media?filename=<이름>&alt=<대체 텍스트>`, 본문은 파일 바이트 그대로.
/// multipart 대신 원본 바이트를 받는다: `curl --data-binary @a.jpg` 와 브라우저 `fetch(file)` 이 같다.
async fn upload_media(
    State(state): State<AppState>,
    ApiActor(actor): ApiActor,
    Query(q): Query<UploadQuery>,
    body: Bytes,
) -> Result<Response, ApiError> {
    let input = media::Upload {
        filename: q.filename,
        bytes: body.to_vec(),
        alt_text: q.alt,
    };
    let m = crate::upload_media(&state, actor, input).await?;
    Ok(ok(StatusCode::CREATED, m))
}

#[derive(Debug, Deserialize)]
struct MediaListQuery {
    page: Option<u32>,
    per_page: Option<u32>,
}

async fn list_media(
    State(state): State<AppState>,
    ApiActor(actor): ApiActor,
    Query(q): Query<MediaListQuery>,
) -> Result<Response, ApiError> {
    let page = PageReq::new(q.page.unwrap_or(1), q.per_page.unwrap_or(20));
    Ok(ok(
        StatusCode::OK,
        media::list(&state.ctx, &actor, page).await?,
    ))
}

async fn get_media(
    State(state): State<AppState>,
    ApiActor(actor): ApiActor,
    Path(id): Path<String>,
) -> Result<Response, ApiError> {
    Ok(ok(
        StatusCode::OK,
        media::get(&state.ctx, &actor, &id).await?,
    ))
}

async fn update_media(
    State(state): State<AppState>,
    ApiActor(actor): ApiActor,
    Path(id): Path<String>,
    body: Result<Json<media::MediaPatch>, JsonRejection>,
) -> Result<Response, ApiError> {
    let Json(patch) = body.map_err(bad_json)?;
    Ok(ok(
        StatusCode::OK,
        media::update(&state.ctx, &actor, &id, patch).await?,
    ))
}

async fn delete_media(
    State(state): State<AppState>,
    ApiActor(actor): ApiActor,
    Path(id): Path<String>,
) -> Result<Response, ApiError> {
    Ok(ok(
        StatusCode::OK,
        media::delete(&state.ctx, &actor, &id).await?,
    ))
}

async fn list_templates(
    State(state): State<AppState>,
    ApiActor(actor): ApiActor,
) -> Result<Response, ApiError> {
    Ok(ok(
        StatusCode::OK,
        templates::list(&state.ctx, &actor).await?,
    ))
}

#[derive(Debug, Deserialize)]
struct PublicListQuery {
    category: Option<String>,
    page: Option<u32>,
    per_page: Option<u32>,
}

/// `GET /boards/{slug}/posts?category=` 공개 화면과 같은 목록(공지 따로, 비밀글은 제목만).
async fn board_posts(
    State(state): State<AppState>,
    ApiActor(actor): ApiActor,
    Path(slug): Path<String>,
    Query(q): Query<PublicListQuery>,
) -> Result<Response, ApiError> {
    let board = boards::readable(&state.ctx, &actor, &slug).await?;
    let page = PageReq::new(q.page.unwrap_or(1), q.per_page.unwrap_or(20));
    let out = posts::public_board(&state.ctx, &actor, &board, q.category.as_deref(), page).await?;
    Ok(ok(StatusCode::OK, out))
}

async fn list_categories(
    State(state): State<AppState>,
    ApiActor(actor): ApiActor,
    Path(slug): Path<String>,
) -> Result<Response, ApiError> {
    Ok(ok(
        StatusCode::OK,
        categories::list(&state.ctx, &actor, &slug).await?,
    ))
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct CategoryBody {
    name: String,
    #[serde(default)]
    slug: Option<String>,
}

/// `POST /boards/{slug}/categories` 같은 주소가 있으면 그것을 돌려준다(멱등).
async fn create_category(
    State(state): State<AppState>,
    ApiActor(actor): ApiActor,
    Path(board): Path<String>,
    body: Result<Json<CategoryBody>, JsonRejection>,
) -> Result<Response, ApiError> {
    let Json(b) = body.map_err(bad_json)?;
    let input = categories::NewCategory {
        board,
        name: b.name,
        slug: b.slug,
    };
    Ok(ok(
        StatusCode::OK,
        categories::create(&state.ctx, &actor, input).await?,
    ))
}

async fn update_category(
    State(state): State<AppState>,
    ApiActor(actor): ApiActor,
    Path((board, category)): Path<(String, String)>,
    body: Result<Json<categories::CategoryPatch>, JsonRejection>,
) -> Result<Response, ApiError> {
    let Json(patch) = body.map_err(bad_json)?;
    Ok(ok(
        StatusCode::OK,
        categories::update(&state.ctx, &actor, &board, &category, patch).await?,
    ))
}

async fn delete_category(
    State(state): State<AppState>,
    ApiActor(actor): ApiActor,
    Path((board, category)): Path<(String, String)>,
) -> Result<Response, ApiError> {
    Ok(ok(
        StatusCode::OK,
        categories::delete(&state.ctx, &actor, &board, &category).await?,
    ))
}

async fn list_tags(
    State(state): State<AppState>,
    ApiActor(actor): ApiActor,
) -> Result<Response, ApiError> {
    Ok(ok(StatusCode::OK, tags::list(&state.ctx, &actor).await?))
}

#[derive(Debug, serde::Serialize)]
struct TagPage {
    tag: maru_core::model::Tag,
    posts: maru_core::model::Listing<maru_core::model::Post>,
}

/// `GET /tags/{slug}` 태그가 달린 공개 글 (비밀글 제외).
async fn tag_posts(
    State(state): State<AppState>,
    ApiActor(actor): ApiActor,
    Path(slug): Path<String>,
    Query(q): Query<PublicListQuery>,
) -> Result<Response, ApiError> {
    let page = PageReq::new(q.page.unwrap_or(1), q.per_page.unwrap_or(20));
    let (tag, posts) = posts::public_tag(&state.ctx, &actor, &slug, page).await?;
    Ok(ok(StatusCode::OK, TagPage { tag, posts }))
}

/// `POST /templates` 같은 이름이 있으면 덮어쓴다.
async fn save_template(
    State(state): State<AppState>,
    ApiActor(actor): ApiActor,
    body: Result<Json<templates::SaveTemplate>, JsonRejection>,
) -> Result<Response, ApiError> {
    let Json(input) = body.map_err(bad_json)?;
    Ok(ok(
        StatusCode::OK,
        templates::save(&state.ctx, &actor, input).await?,
    ))
}

/// `DELETE /templates/{id 또는 이름}`
async fn delete_template(
    State(state): State<AppState>,
    ApiActor(actor): ApiActor,
    Path(id): Path<String>,
) -> Result<Response, ApiError> {
    Ok(ok(
        StatusCode::OK,
        templates::delete(&state.ctx, &actor, &id).await?,
    ))
}

#[derive(Debug, Deserialize)]
struct FileQuery {
    filename: String,
}

/// `POST /files?filename=<이름>`, 본문은 파일 바이트 그대로 (`/media` 와 같은 방식).
async fn upload_file(
    State(state): State<AppState>,
    ApiActor(actor): ApiActor,
    Query(q): Query<FileQuery>,
    body: Bytes,
) -> Result<Response, ApiError> {
    let input = files::Upload {
        filename: q.filename,
        bytes: body.to_vec(),
    };
    let f = files::upload(&state.ctx, &actor, input).await?;
    Ok(ok(StatusCode::CREATED, f))
}

async fn list_files(
    State(state): State<AppState>,
    ApiActor(actor): ApiActor,
    Query(q): Query<MediaListQuery>,
) -> Result<Response, ApiError> {
    let page = PageReq::new(q.page.unwrap_or(1), q.per_page.unwrap_or(20));
    Ok(ok(
        StatusCode::OK,
        files::list(&state.ctx, &actor, page).await?,
    ))
}

async fn get_file(
    State(state): State<AppState>,
    ApiActor(actor): ApiActor,
    Path(id): Path<String>,
) -> Result<Response, ApiError> {
    Ok(ok(
        StatusCode::OK,
        files::get(&state.ctx, &actor, &id).await?,
    ))
}

async fn delete_file(
    State(state): State<AppState>,
    ApiActor(actor): ApiActor,
    Path(id): Path<String>,
) -> Result<Response, ApiError> {
    Ok(ok(
        StatusCode::OK,
        files::delete(&state.ctx, &actor, &id).await?,
    ))
}

/// 글 주소가 게시판에서 비어 있는지 (저장 전 확인, 바꾸는 것 없음).
async fn check_slug(
    State(state): State<AppState>,
    ApiActor(actor): ApiActor,
    Query(input): Query<posts::SlugCheckInput>,
) -> Result<Response, ApiError> {
    Ok(ok(
        StatusCode::OK,
        posts::check_slug(&state.ctx, &actor, input).await?,
    ))
}

async fn preview(
    State(state): State<AppState>,
    ApiActor(actor): ApiActor,
    body: Result<Json<posts::PreviewInput>, JsonRejection>,
) -> Result<Response, ApiError> {
    let Json(input) = body.map_err(bad_json)?;
    Ok(ok(
        StatusCode::OK,
        posts::preview(&state.ctx, &actor, input)?,
    ))
}
