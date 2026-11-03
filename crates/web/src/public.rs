//! 공개 사이트. 비로그인 Actor 로 공개 조회 서비스(`posts::public_*`, `pages::public_*`)만 부른다.

use axum::Router;
use axum::extract::{Path, Query, State};
use axum::http::{StatusCode, header};
use axum::response::{Html, IntoResponse, Response};
use axum::routing::get;
use maru_core::model::{Board, Listing, MenuItem, MenuLink, Page, PageReq, Post, SiteSettings};
use maru_core::services::{
    anonymous_actor, boards, design, files, media, menus, pages, posts, settings,
};
use maru_core::urls::{self, Target};
use maru_core::{Code, Error, Result, blobs, clock, markdown};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::{AppState, error};

const SUMMARY_CHARS: usize = 160;

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/", get(home))
        .route("/b/{board}", get(board))
        .route("/b/{board}/{post}", get(post))
        .route("/p/{page}", get(page))
        .route("/sitemap.xml", get(crate::feeds::sitemap))
        .route("/rss.xml", get(crate::feeds::rss))
        .route("/robots.txt", get(crate::feeds::robots))
        .route("/assets/tokens.css", get(tokens_css))
        .route("/assets/{*path}", get(asset))
        .route("/media/{*path}", get(media_file))
        .route("/files/{id}", get(file_download))
        .fallback(not_found)
}

#[derive(Debug, Deserialize)]
struct PageQuery {
    page: Option<u32>,
}

/// 템플릿에 넘기는 사이트 공통 값. 설정·메뉴를 못 읽어도 화면은 기본값으로 뜬다.
pub async fn site_json(state: &AppState) -> Value {
    let s = settings::site(&state.ctx).await.unwrap_or_else(|e| {
        tracing::warn!(error = ?e, "사이트 설정을 못 읽어 기본값을 씁니다");
        SiteSettings::default()
    });
    site_value(state, &s).await
}

async fn menu_value(state: &AppState, location: &str) -> Value {
    menus::get(&state.ctx, location)
        .await
        .map(|items| menu_json(&items))
        .unwrap_or_else(|_| json!([]))
}

async fn site_value(state: &AppState, s: &SiteSettings) -> Value {
    json!({
        "name": s.name,
        "description": s.description,
        "lang": s.lang,
        "url": state.ctx.site_url,
        "home_url": urls::path(&Target::Home),
        "rss_url": urls::path(&Target::Rss),
        "naver_verification": s.naver_verification,
        "google_verification": s.google_verification,
        "menus": {
            "header": menu_value(state, "header").await,
            "footer": menu_value(state, "footer").await,
        },
    })
}

fn menu_json(items: &[MenuItem]) -> Value {
    Value::Array(
        items
            .iter()
            .map(|i| {
                let (href, external) = match &i.link {
                    MenuLink::Home => (urls::path(&Target::Home), false),
                    MenuLink::Board { slug } => (urls::path(&Target::Board { board: slug }), false),
                    MenuLink::Page { slug } => (urls::path(&Target::Page { page: slug }), false),
                    MenuLink::Url { href } => (href.clone(), !href.starts_with('/')),
                };
                json!({
                    "label": i.label,
                    "href": href,
                    "external": external,
                    "children": menu_json(&i.children),
                })
            })
            .collect(),
    )
}

/// 요청마다 한 번 읽는 사이트 설정과 템플릿용 값.
struct Site {
    settings: SiteSettings,
    json: Value,
}

async fn load_site(state: &AppState) -> Result<Site> {
    let settings = settings::site(&state.ctx).await?;
    let json = site_value(state, &settings).await;
    Ok(Site { settings, json })
}

async fn render(state: &AppState, name: &str, ctx: Result<Value>) -> Response {
    let rendered = match ctx {
        Ok(v) => state.theme.render(name, &v),
        Err(e) => Err(e),
    };
    match rendered {
        Ok(html) => Html(html).into_response(),
        Err(e) => error::page(state, &e).await,
    }
}

async fn home(State(state): State<AppState>, Query(q): Query<PageQuery>) -> Response {
    let v = home_inner(&state, q).await;
    render(&state, "home.html", v).await
}

async fn home_inner(state: &AppState, q: PageQuery) -> Result<Value> {
    let site = load_site(state).await?;
    let actor = anonymous_actor(&state.ctx).await?;
    let page = PageReq::new(q.page.unwrap_or(1), site.settings.posts_per_page);
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
            post_summary(slug, p, &site.settings)
        })
        .collect::<Vec<_>>();
    let items = with_thumbs(state, items, &list.items).await?;
    Ok(json!({
        "meta": json!({ "title": site.settings.name, "description": site.settings.description }),
        "site": site.json,
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
    let v = board_inner(&state, &slug, q).await;
    render(&state, "board.html", v).await
}

async fn board_inner(state: &AppState, slug: &str, q: PageQuery) -> Result<Value> {
    let site = load_site(state).await?;
    let actor = anonymous_actor(&state.ctx).await?;
    let board = boards::readable(&state.ctx, &actor, slug).await?;
    let page = PageReq::new(q.page.unwrap_or(1), site.settings.posts_per_page);
    let list = posts::public_posts(&state.ctx, &actor, Some(&board), page).await?;
    let target = Target::Board { board: &board.slug };
    let items = list
        .items
        .iter()
        .map(|p| post_summary(&board.slug, p, &site.settings))
        .collect::<Vec<_>>();
    let items = with_thumbs(state, items, &list.items).await?;
    Ok(json!({
        "meta": json!({
            "title": format!("{} · {}", board.name, site.settings.name),
            "description": site.settings.description,
        }),
        "site": site.json,
        "canonical": urls::absolute(&state.ctx.site_url, &target),
        "board": board_json(&board),
        "posts": items,
        "pagination": pagination(&list, &target),
    }))
}

async fn post(
    State(state): State<AppState>,
    Path((board_slug, post_slug)): Path<(String, String)>,
) -> Response {
    let v = post_inner(&state, &board_slug, &post_slug).await;
    render(&state, "post.html", v).await
}

async fn post_inner(state: &AppState, board_slug: &str, post_slug: &str) -> Result<Value> {
    let site = load_site(state).await?;
    let actor = anonymous_actor(&state.ctx).await?;
    let (board, post) = posts::public_post(&state.ctx, &actor, board_slug, post_slug).await?;
    let target = Target::Post {
        board: &board.slug,
        post: &post.slug,
    };
    let mut view = post_summary(&board.slug, &post, &site.settings);
    view["body_html"] = Value::String(body_html(state, &post.body_md).await?);
    let image = cover_image(state, &post).await?;
    Ok(json!({
        "meta": json!({
            "title": format!("{} · {}", post.title, site.settings.name),
            "description": view["summary"],
            "image": image,
            "type": "article",
            "published_time": post.published_at,
            "modified_time": post.updated_at,
        }),
        "site": site.json,
        "canonical": urls::absolute(&state.ctx.site_url, &target),
        "board": board_json(&board),
        "post": view,
    }))
}

async fn page(State(state): State<AppState>, Path(slug): Path<String>) -> Response {
    let v = page_inner(&state, &slug).await;
    render(&state, "page.html", v).await
}

async fn page_inner(state: &AppState, slug: &str) -> Result<Value> {
    let site = load_site(state).await?;
    let actor = anonymous_actor(&state.ctx).await?;
    let p = pages::public_page(&state.ctx, &actor, slug).await?;
    let target = Target::Page { page: &p.slug };
    Ok(json!({
        "meta": json!({
            "title": format!("{} · {}", p.title, site.settings.name),
            "description": markdown::plain_text(&p.body_md, SUMMARY_CHARS),
        }),
        "site": site.json,
        "canonical": urls::absolute(&state.ctx.site_url, &target),
        "page": page_json(&p, body_html(state, &p.body_md).await?),
    }))
}

/// 디자인 토큰 CSS. 토큰은 검증된 값만 들어 있어 그대로 내보낸다.
async fn tokens_css(State(state): State<AppState>) -> Response {
    let tokens = design::tokens(&state.ctx).await.unwrap_or_default();
    (
        [
            (header::CONTENT_TYPE, "text/css; charset=utf-8"),
            (header::CACHE_CONTROL, "no-cache"),
        ],
        tokens.to_css(),
    )
        .into_response()
}

async fn asset(State(state): State<AppState>, Path(path): Path<String>) -> Response {
    match state.theme.asset(&path) {
        Some((bytes, mime)) => (
            [
                (header::CONTENT_TYPE, mime),
                (header::CACHE_CONTROL, "no-cache"),
            ],
            bytes.into_owned(),
        )
            .into_response(),
        None => (StatusCode::NOT_FOUND, "not found").into_response(),
    }
}

/// 업로드한 이미지. 파일 이름에 ID 가 들어 있어 내용이 바뀌지 않으므로 오래 캐시한다.
/// 확장자로 정한 형식만 내보내고, 보관소 경로 규칙에 맞지 않으면 파일 시스템을 보지 않는다.
async fn media_file(State(state): State<AppState>, Path(path): Path<String>) -> Response {
    let mime = match path.rsplit_once('.').map(|(_, e)| e) {
        Some("jpg") => "image/jpeg",
        Some("png") => "image/png",
        _ => return (StatusCode::NOT_FOUND, "not found").into_response(),
    };
    if !blobs::valid_path(&path) {
        return (StatusCode::NOT_FOUND, "not found").into_response();
    }
    match state.ctx.blobs.get(&path).await {
        Ok(Some(bytes)) => (
            [
                (header::CONTENT_TYPE, mime),
                (header::CACHE_CONTROL, "public, max-age=31536000, immutable"),
                (
                    header::CONTENT_SECURITY_POLICY,
                    "default-src 'none'; sandbox",
                ),
            ],
            bytes,
        )
            .into_response(),
        Ok(None) => (StatusCode::NOT_FOUND, "not found").into_response(),
        Err(e) => {
            tracing::error!(error = ?e, "미디어 파일 읽기 실패");
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        }
    }
}

/// 첨부 파일은 항상 내려받기로, 정해진 MIME 으로만 보낸다 (브라우저 안에서 열리지 않는다).
async fn file_download(State(state): State<AppState>, Path(id): Path<String>) -> Response {
    match files::download(&state.ctx, &id).await {
        Ok((file, bytes)) => (
            [
                (header::CONTENT_TYPE, file.mime.clone()),
                (
                    header::CONTENT_DISPOSITION,
                    content_disposition(&file.filename),
                ),
                (header::CACHE_CONTROL, "public, max-age=86400".to_owned()),
                (
                    header::CONTENT_SECURITY_POLICY,
                    "default-src 'none'; sandbox".to_owned(),
                ),
            ],
            bytes,
        )
            .into_response(),
        Err(e) if e.code() == Code::NotFound => error::page(&state, &e).await,
        Err(e) => {
            tracing::error!(error = ?e, "첨부 파일 읽기 실패");
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        }
    }
}

/// `attachment; filename="<ASCII 대체>"; filename*=UTF-8''<퍼센트 인코딩>` (RFC 6266).
fn content_disposition(name: &str) -> String {
    let fallback: String = name
        .chars()
        .map(|c| {
            if c == ' ' || (c.is_ascii_graphic() && !matches!(c, '"' | '\\' | '%')) {
                c
            } else {
                '_'
            }
        })
        .collect();
    let mut encoded = String::new();
    for b in name.bytes() {
        if b.is_ascii_alphanumeric() || b"-._~".contains(&b) {
            encoded.push(char::from(b));
        } else {
            encoded.push_str(&format!("%{b:02X}"));
        }
    }
    format!("attachment; filename=\"{fallback}\"; filename*=UTF-8''{encoded}")
}

async fn not_found(State(state): State<AppState>) -> Response {
    error::page(&state, &Error::new(Code::NotFound)).await
}

fn board_json(board: &Board) -> Value {
    json!({
        "slug": board.slug,
        "name": board.name,
        "kind": board.kind,
        "url": urls::path(&Target::Board { board: &board.slug }),
    })
}

/// 목록 썸네일: 대표 이미지의 가장 작은 축소본(없으면 원본)과 원본 크기.
/// 보관소에서 찾지 못한 사진(지워진 것 등)은 깨진 그림이 되지 않게 빼 둔다.
async fn with_thumbs(
    state: &AppState,
    mut items: Vec<Value>,
    posts: &[Post],
) -> Result<Vec<Value>> {
    for (view, p) in items.iter_mut().zip(posts) {
        let Some(url) = posts::cover_of(p) else {
            continue;
        };
        let Some(info) = media::image_info(&state.ctx, &url).await? else {
            continue;
        };
        let src = info
            .variants
            .iter()
            .min_by_key(|(w, _)| *w)
            .map(|(_, u)| u.clone())
            .unwrap_or(url);
        view["thumb"] = json!({ "src": src, "width": info.width, "height": info.height });
    }
    Ok(items)
}

/// 공유 미리보기(`og:image`)용 절대 주소. 가장 큰 축소본, 없으면 원본.
async fn cover_image(state: &AppState, post: &Post) -> Result<Option<String>> {
    let Some(url) = posts::cover_of(post) else {
        return Ok(None);
    };
    let Some(info) = media::image_info(&state.ctx, &url).await? else {
        return Ok(None);
    };
    let best = info
        .variants
        .iter()
        .max_by_key(|(w, _)| *w)
        .map(|(_, u)| u.clone())
        .unwrap_or(url);
    Ok(Some(format!(
        "{}{best}",
        state.ctx.site_url.trim_end_matches('/')
    )))
}

/// 본문 HTML. 보관소 그림에는 크기와 축소본(srcset)을 붙인다.
async fn body_html(state: &AppState, md: &str) -> Result<String> {
    let images = media::images_in(&state.ctx, md).await?;
    Ok(markdown::render_with(md, &|url| images.get(url).cloned()).into_string())
}

fn page_json(p: &Page, body_html: String) -> Value {
    json!({
        "title": p.title,
        "slug": p.slug,
        "layout": p.layout,
        "url": urls::path(&Target::Page { page: &p.slug }),
        "summary": markdown::plain_text(&p.body_md, SUMMARY_CHARS),
        "body_html": body_html,
    })
}

fn post_summary(board_slug: &str, p: &Post, s: &SiteSettings) -> Value {
    let published = p.published_at.clone().unwrap_or_default();
    let summary = p
        .excerpt
        .clone()
        .filter(|e| !e.trim().is_empty())
        .unwrap_or_else(|| markdown::plain_text(&p.body_md, SUMMARY_CHARS));
    json!({
        "id": p.id,
        "title": p.title,
        "slug": p.slug,
        "url": urls::path(&Target::Post { board: board_slug, post: &p.slug }),
        "published_at": published,
        "date": clock::display_date(&published, s.tz_offset_min),
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
