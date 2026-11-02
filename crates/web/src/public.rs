//! 공개 사이트. 비로그인 Actor 로 공개 조회 서비스(`posts::public_*`)만 부른다.

use axum::Router;
use axum::extract::{Path, Query, State};
use axum::http::{StatusCode, header};
use axum::response::{Html, IntoResponse, Response};
use axum::routing::get;
use maru_core::model::{Board, Listing, PageReq, Post};
use maru_core::services::{anonymous_actor, boards, posts};
use maru_core::urls::{self, Target};
use maru_core::{Code, Error, Result, clock, markdown};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::{AppState, error};

/// 사이트 시간대 (분). 설정 화면은 U9.
const TZ_OFFSET_MIN: i32 = 9 * 60;
const SUMMARY_CHARS: usize = 160;
const PER_PAGE: u32 = 10;

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/", get(home))
        .route("/b/{board}", get(board))
        .route("/b/{board}/{post}", get(post))
        .route("/assets/{*path}", get(asset))
        .fallback(not_found)
}

#[derive(Debug, Deserialize)]
struct PageQuery {
    page: Option<u32>,
}

pub fn site_json(state: &AppState) -> Value {
    json!({
        "name": "Maru",
        "description": "",
        "lang": "ko",
        "url": state.ctx.site_url,
        "home_url": urls::path(&Target::Home),
        "rss_url": urls::path(&Target::Rss),
    })
}

fn render(state: &AppState, name: &str, ctx: &Value) -> Response {
    match state.theme.render(name, ctx) {
        Ok(html) => Html(html).into_response(),
        Err(e) => error::page(state, &e),
    }
}

async fn home(State(state): State<AppState>, Query(q): Query<PageQuery>) -> Response {
    match home_inner(&state, q).await {
        Ok(v) => render(&state, "home.html", &v),
        Err(e) => error::page(&state, &e),
    }
}

async fn home_inner(state: &AppState, q: PageQuery) -> Result<Value> {
    let actor = anonymous_actor(&state.ctx).await?;
    let page = PageReq::new(q.page.unwrap_or(1), PER_PAGE);
    let list = posts::public_posts(&state.ctx, &actor, None, page).await?;
    let all_boards = state.ctx.store.list_boards().await?;
    let items = list
        .items
        .iter()
        .map(|p| {
            let slug = all_boards
                .iter()
                .find(|b| b.id == p.board_id)
                .map(|b| b.slug.as_str())
                .unwrap_or_default();
            post_summary(slug, p)
        })
        .collect::<Vec<_>>();
    Ok(json!({
        "site": site_json(state),
        "canonical": urls::absolute(&state.ctx.site_url, &Target::Home),
        "posts": items,
        "pagination": pagination(&list, &Target::Home),
    }))
}

async fn board(
    State(state): State<AppState>,
    Path(slug): Path<String>,
    Query(q): Query<PageQuery>,
) -> Response {
    match board_inner(&state, &slug, q).await {
        Ok(v) => render(&state, "board.html", &v),
        Err(e) => error::page(&state, &e),
    }
}

async fn board_inner(state: &AppState, slug: &str, q: PageQuery) -> Result<Value> {
    let actor = anonymous_actor(&state.ctx).await?;
    let board = boards::readable(&state.ctx, &actor, slug).await?;
    let page = PageReq::new(q.page.unwrap_or(1), PER_PAGE);
    let list = posts::public_posts(&state.ctx, &actor, Some(&board), page).await?;
    let target = Target::Board { board: &board.slug };
    Ok(json!({
        "site": site_json(state),
        "canonical": urls::absolute(&state.ctx.site_url, &target),
        "board": board_json(&board),
        "posts": list.items.iter().map(|p| post_summary(&board.slug, p)).collect::<Vec<_>>(),
        "pagination": pagination(&list, &target),
    }))
}

async fn post(
    State(state): State<AppState>,
    Path((board_slug, post_slug)): Path<(String, String)>,
) -> Response {
    match post_inner(&state, &board_slug, &post_slug).await {
        Ok(v) => render(&state, "post.html", &v),
        Err(e) => error::page(&state, &e),
    }
}

async fn post_inner(state: &AppState, board_slug: &str, post_slug: &str) -> Result<Value> {
    let actor = anonymous_actor(&state.ctx).await?;
    let (board, post) = posts::public_post(&state.ctx, &actor, board_slug, post_slug).await?;
    let target = Target::Post {
        board: &board.slug,
        post: &post.slug,
    };
    let mut view = post_summary(&board.slug, &post);
    view["body_html"] = Value::String(markdown::render(&post.body_md).into_string());
    Ok(json!({
        "site": site_json(state),
        "canonical": urls::absolute(&state.ctx.site_url, &target),
        "board": board_json(&board),
        "post": view,
    }))
}

async fn asset(State(state): State<AppState>, Path(path): Path<String>) -> Response {
    match state.theme.asset(&path) {
        Some((bytes, mime)) => (
            [
                (header::CONTENT_TYPE, mime),
                (header::CACHE_CONTROL, "public, max-age=3600"),
            ],
            bytes.into_owned(),
        )
            .into_response(),
        None => (StatusCode::NOT_FOUND, "not found").into_response(),
    }
}

async fn not_found(State(state): State<AppState>) -> Response {
    error::page(&state, &Error::new(Code::NotFound))
}

fn board_json(board: &Board) -> Value {
    json!({
        "slug": board.slug,
        "name": board.name,
        "kind": board.kind,
        "url": urls::path(&Target::Board { board: &board.slug }),
    })
}

fn post_summary(board_slug: &str, p: &Post) -> Value {
    let published = p.published_at.clone().unwrap_or_default();
    let summary = p
        .excerpt
        .clone()
        .unwrap_or_else(|| markdown::plain_text(&p.body_md, SUMMARY_CHARS));
    json!({
        "id": p.id,
        "title": p.title,
        "slug": p.slug,
        "url": urls::path(&Target::Post { board: board_slug, post: &p.slug }),
        "published_at": published,
        "date": clock::display_date(&published, TZ_OFFSET_MIN),
        "summary": summary,
    })
}

fn pagination<T>(list: &Listing<T>, target: &Target<'_>) -> Value {
    let base = urls::path(target);
    let total_pages = list.total_pages();
    let page = i64::from(list.page);
    let link = |n: i64| {
        if n == 1 {
            base.clone()
        } else {
            format!("{base}?page={n}")
        }
    };
    json!({
        "page": page,
        "total_pages": total_pages,
        "prev_url": (page > 1).then(|| link(page - 1)),
        "next_url": (page < total_pages).then(|| link(page + 1)),
    })
}
