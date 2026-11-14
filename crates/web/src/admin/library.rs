//! 미디어 보관함 (G7 #14 Media Manager, ADR-0172). 올린 그림·동영상을 한곳에서 보고, 쓰는 곳을 확인하고,
//! 대체 텍스트를 고치고, 안 쓰는 것을 지운다. 목록·쓰는 곳·지우기는 REST·CLI·MCP 와 같은 `media` 서비스.

use axum::Router;
use axum::extract::{Form, Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Redirect, Response};
use axum::routing::post;
use maru_core::actor::Permission;
use maru_core::clock;
use maru_core::model::PageReq;
use maru_core::services::media::{self, LibraryQuery, MediaKind, MediaPatch, MediaUse};
use maru_core::services::settings;
use serde::Deserialize;
use serde_json::{Value, json};

use super::{SignedIn, base, encode, fail, merge, require, require_post};
use crate::AppState;

const PER_PAGE: u32 = 24;

pub(super) fn routes() -> Router<AppState> {
    Router::new()
        .route("/admin/media/{id}/alt", post(alt))
        .route("/admin/media/{id}/delete", post(delete))
}

#[derive(Debug, Default, Deserialize)]
pub(super) struct PageQuery {
    #[serde(default)]
    kind: String,
    #[serde(default)]
    q: String,
    #[serde(default)]
    unused: Option<String>,
    #[serde(default)]
    page: Option<u32>,
    #[serde(default)]
    done: Option<String>,
}

impl PageQuery {
    fn unused(&self) -> bool {
        self.unused
            .as_deref()
            .is_some_and(|v| !v.is_empty() && v != "0")
    }

    /// 조건을 그대로 붙인 주소. `page`·`done` 은 뺀다.
    fn link(&self, kind: &str) -> String {
        let mut s = String::from("/admin/media?");
        if !kind.is_empty() {
            s.push_str(&format!("kind={}&", encode(kind)));
        }
        if !self.q.trim().is_empty() {
            s.push_str(&format!("q={}&", encode(self.q.trim())));
        }
        if self.unused() {
            s.push_str("unused=1&");
        }
        s
    }
}

fn place_label(u: &MediaUse) -> String {
    match u.place.as_str() {
        "post" => "글".to_owned(),
        "page" => "페이지".to_owned(),
        "widget" => "위젯·블록".to_owned(),
        "setting" => "사이트 설정".to_owned(),
        "saved_block" => "내 블록".to_owned(),
        "editor_draft" => "화면 편집 초안".to_owned(),
        other => format!("기능 표 {other}"),
    }
}

fn place_href(u: &MediaUse) -> Option<String> {
    match u.place.as_str() {
        "post" => Some(format!("/admin/posts/{}", encode(&u.id))),
        "page" => Some("/admin/pages".to_owned()),
        "widget" => Some("/admin/widgets".to_owned()),
        "setting" => Some("/admin/settings".to_owned()),
        "saved_block" | "editor_draft" => Some("/admin/editor".to_owned()),
        _ => None,
    }
}

pub(super) async fn page(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<PageQuery>,
) -> Response {
    let who = match require(&state, &headers, "/admin/media").await {
        Ok(w) => w,
        Err(r) => return *r,
    };
    render_page(&state, &who, &q, StatusCode::OK, None).await
}

async fn render_page(
    state: &AppState,
    who: &SignedIn,
    q: &PageQuery,
    status: StatusCode,
    error: Option<String>,
) -> Response {
    let kind = match q.kind.as_str() {
        "image" => Some(MediaKind::Image),
        "video" => Some(MediaKind::Video),
        _ => None,
    };
    let query = LibraryQuery {
        kind,
        q: Some(q.q.clone()),
        unused: q.unused(),
    };
    let req = PageReq::new(q.page.unwrap_or(1), PER_PAGE);
    let list = match media::library(&state.ctx, &who.actor, &query, req).await {
        Ok(l) => l,
        Err(e) => return fail(state, Some(who), &e).await,
    };
    let tz = settings::site(&state.ctx)
        .await
        .map(|s| s.tz_offset_min)
        .unwrap_or(540);
    let items: Vec<Value> = list
        .items
        .iter()
        .map(|i| {
            let m = &i.media;
            let thumb = m
                .variants
                .iter()
                .filter(|v| v.width >= 320)
                .min_by_key(|v| v.width)
                .map_or(m.url.as_str(), |v| v.url.as_str());
            let name = m.path.rsplit('/').next().unwrap_or(&m.path);
            json!({
                "id": m.id,
                "url": m.url,
                "thumb": thumb,
                "name": name,
                "video": m.mime.starts_with("video/"),
                "width": m.width,
                "height": m.height,
                "size": if m.size >= 1024 * 1024 {
                    format!("{:.1}MB", m.size as f64 / 1024.0 / 1024.0)
                } else {
                    format!("{}KB", (m.size / 1024).max(1))
                },
                "alt": m.alt_text,
                "date": clock::display_date(&m.created_at, tz),
                "uses": i.uses,
                "used_in": i.used_in.iter().map(|u| json!({
                    "place": place_label(u),
                    "label": u.label.clone().unwrap_or_else(|| u.id.clone()),
                    "href": place_href(u),
                })).collect::<Vec<_>>(),
            })
        })
        .collect();
    let kind_now = match kind {
        Some(MediaKind::Image) => "image",
        Some(MediaKind::Video) => "video",
        None => "",
    };
    let kinds: Vec<Value> = [("", "전체"), ("image", "그림"), ("video", "동영상")]
        .iter()
        .map(|(k, label)| json!({ "label": label, "href": q.link(k), "on": kind_now == *k }))
        .collect();
    let notice = match q.done.as_deref() {
        Some("alt") => Some("대체 텍스트를 바꿨습니다."),
        Some("deleted") => Some("지웠습니다."),
        _ => None,
    };
    let ctx = merge(
        base(state, Some(who), "media").await,
        json!({
            "items": items,
            "total": list.total,
            "page": list.page,
            "pages": list.total_pages(),
            "kinds": kinds,
            "q": q.q,
            "kind": kind_now,
            "unused": q.unused(),
            "base_query": q.link(kind_now),
            "can_edit": who.actor.has(Permission::PostUpload),
            "can_delete": who.actor.has(Permission::PostModerate),
            "notice": notice,
            "error": error,
        }),
    );
    super::render(state, status, "media.html", &ctx)
}

#[derive(Debug, Deserialize)]
struct AltForm {
    #[serde(rename = "_csrf")]
    csrf: String,
    #[serde(default)]
    alt: String,
    /// 돌아갈 목록 주소 (`/admin/media?…` 만)
    #[serde(default)]
    back: String,
}

#[derive(Debug, Deserialize)]
struct DeleteForm {
    #[serde(rename = "_csrf")]
    csrf: String,
    #[serde(default)]
    force: Option<String>,
    #[serde(default)]
    back: String,
}

/// 돌아갈 주소는 보관함 목록만 받는다 (열린 이동 방지).
fn back_to(back: &str, done: &str) -> String {
    let base = if back.starts_with("/admin/media?") || back == "/admin/media" {
        back.trim_end_matches(['&', '?'])
    } else {
        "/admin/media"
    };
    let sep = if base.contains('?') { '&' } else { '?' };
    format!("{base}{sep}done={done}")
}

async fn alt(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Form(f): Form<AltForm>,
) -> Response {
    let who = match require_post(&state, &headers, &f.csrf).await {
        Ok(w) => w,
        Err(r) => return *r,
    };
    let patch = MediaPatch {
        alt_text: Some(f.alt),
    };
    match media::update(&state.ctx, &who.actor, &id, patch).await {
        Ok(_) => Redirect::to(&back_to(&f.back, "alt")).into_response(),
        Err(e) => failed(&state, &who, e).await,
    }
}

async fn delete(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Form(f): Form<DeleteForm>,
) -> Response {
    let who = match require_post(&state, &headers, &f.csrf).await {
        Ok(w) => w,
        Err(r) => return *r,
    };
    let force = f
        .force
        .as_deref()
        .is_some_and(|v| !v.is_empty() && v != "0");
    match media::delete(&state.ctx, &who.actor, &id, force).await {
        Ok(_) => Redirect::to(&back_to(&f.back, "deleted")).into_response(),
        Err(e) => failed(&state, &who, e).await,
    }
}

/// 권한·없음은 오류 화면, 그 밖(쓰는 곳 있음·길이 초과)은 목록 위에 알린다.
async fn failed(state: &AppState, who: &SignedIn, e: maru_core::Error) -> Response {
    use maru_core::Code;
    match e.code() {
        Code::Forbidden | Code::NotFound => fail(state, Some(who), &e).await,
        code => {
            let status = if code == Code::Conflict {
                StatusCode::CONFLICT
            } else {
                StatusCode::UNPROCESSABLE_ENTITY
            };
            render_page(state, who, &PageQuery::default(), status, Some(e.message())).await
        }
    }
}

#[cfg(test)]
mod tests {
    use super::back_to;

    #[test]
    fn back_link_stays_in_the_library() {
        assert_eq!(
            back_to("/admin/media?kind=image&", "alt"),
            "/admin/media?kind=image&done=alt"
        );
        assert_eq!(
            back_to("/admin/media", "deleted"),
            "/admin/media?done=deleted"
        );
        assert_eq!(
            back_to("https://evil.test/", "alt"),
            "/admin/media?done=alt"
        );
        assert_eq!(back_to("/admin/media?", "alt"), "/admin/media?done=alt");
    }
}
