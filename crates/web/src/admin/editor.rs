//! 화면 편집기 (ADR-0138). 실제 공개 화면 위에서 블록(위젯·섹션)을 끌어다 놓고 고친다.
//!
//! - `/admin/editor`: 편집기 화면. 캔버스 iframe 을 담으려고 자기 CSP(`frame-src 'self'`)를 쓴다.
//! - `/admin/editor/frame`: 캔버스. 공개 라우터로 그 화면을 그리되, 초안이 오면 DB 대신 초안 위젯을 그린다.
//!   응답은 공개 화면 CSP(`frame-ancestors 'self'`) 그대로다.
//! - `/admin/editor/publish`: 초안과 지금 위젯을 견주어 연산을 만들고 변경 묶음 하나로 적용한다(원칙 4).
//!
//! 서비스는 REST·CLI·MCP 와 같은 `widgets`·`changesets` 를 부른다.

use std::collections::{HashMap, HashSet};
use std::net::SocketAddr;
use std::sync::Arc;

use axum::body::Body;
use axum::extract::rejection::JsonRejection;
use axum::extract::{ConnectInfo, Query, Request, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router, middleware};
use maru_core::clock;
use maru_core::model::{PageStatus, WIDGET_REGIONS, Widget};
use maru_core::output::Success;
use maru_core::services::auth::SignedIn;
use maru_core::services::changesets as change;
use maru_core::services::stats::StatCounter;
use maru_core::services::views::ViewCounter;
use maru_core::services::widgets;
use maru_core::services::{boards, pages, settings};
use maru_core::urls::{self, Target};
use maru_core::{Code, Error};
use serde::Deserialize;
use serde_json::{Map, Value, json};
use tower::Service;

use super::widgets::{kind_icon, region_text, script_json};
use super::{base, json_session, merge, render, require};
use crate::AppState;
use crate::error::ApiError;
use crate::widgets::{EDITING, Editing};

/// 초안 하나에 담을 수 있는 블록 수. 위젯 전체 한도보다 넉넉하지 않게.
const MAX_DRAFT: usize = 300;

pub(super) fn routes() -> Router<AppState> {
    Router::new()
        .route(
            "/admin/editor",
            get(page).layer(middleware::map_response(editor_headers)),
        )
        .route("/admin/editor/frame", get(frame_get).post(frame_post))
        .route(
            "/admin/editor/publish",
            post(publish).layer(middleware::map_response(editor_headers)),
        )
}

/// 관리 화면 머리글과 같고, 캔버스(같은 출처 iframe)만 더 허락한다.
async fn editor_headers(mut res: Response) -> Response {
    let h = res.headers_mut();
    h.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    h.insert(
        header::REFERRER_POLICY,
        HeaderValue::from_static("no-referrer"),
    );
    h.insert(
        header::CONTENT_SECURITY_POLICY,
        HeaderValue::from_static(
            "default-src 'self'; img-src 'self' data:; style-src 'self'; script-src 'self'; \
             connect-src 'self'; object-src 'none'; base-uri 'none'; form-action 'self'; \
             frame-ancestors 'none'; frame-src 'self'",
        ),
    );
    res
}

// ---- 화면 고르기 ----

/// 편집할 화면. `home`, `page:<주소>`, `board:<주소>`.
#[derive(Debug, Clone, PartialEq, Eq)]
enum EditScreen {
    Home,
    Page(String),
    Board(String),
}

impl EditScreen {
    fn parse(s: &str) -> Option<Self> {
        let ok = |slug: &str| {
            !slug.is_empty()
                && slug.len() <= 120
                && slug
                    .chars()
                    .all(|c| c.is_alphanumeric() || c == '-' || c == '_')
        };
        match s.split_once(':') {
            None if s.is_empty() || s == "home" => Some(Self::Home),
            Some(("page", slug)) if ok(slug) => Some(Self::Page(slug.to_owned())),
            Some(("board", slug)) if ok(slug) => Some(Self::Board(slug.to_owned())),
            _ => None,
        }
    }

    fn id(&self) -> String {
        match self {
            Self::Home => "home".into(),
            Self::Page(s) => format!("page:{s}"),
            Self::Board(s) => format!("board:{s}"),
        }
    }

    fn path(&self) -> String {
        urls::path(&match self {
            Self::Home => Target::Home,
            Self::Page(page) => Target::Page { page },
            Self::Board(board) => Target::Board { board },
        })
    }
}

#[derive(Debug, Default, Deserialize)]
struct ScreenQuery {
    #[serde(default)]
    screen: String,
    /// 발행하고 돌아왔을 때 그 변경 묶음 id
    #[serde(default)]
    done: String,
}

fn bad_screen(screen: &str) -> Error {
    Error::new(Code::Validation)
        .with_message("error.validation")
        .arg("field", "screen")
        .arg("value", screen)
}

// ---- 편집기 화면 ----

async fn page(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<ScreenQuery>,
) -> Response {
    let back = format!("/admin/editor?screen={}", super::encode(&q.screen));
    let who = match require(&state, &headers, &back).await {
        Ok(w) => w,
        Err(r) => return *r,
    };
    let Some(screen) = EditScreen::parse(&q.screen) else {
        return super::fail(&state, Some(&who), &bad_screen(&q.screen)).await;
    };
    let list = match widgets::list(&state.ctx, &who.actor).await {
        Ok(l) => l,
        Err(e) => return super::fail(&state, Some(&who), &e).await,
    };
    let data = match editor_data(&state, &who, &screen, &list).await {
        Ok(d) => d,
        Err(e) => return super::fail(&state, Some(&who), &e).await,
    };
    let label = data["screens"]
        .as_array()
        .and_then(|a| a.iter().find(|s| s["id"] == screen.id()))
        .and_then(|s| s["label"].as_str())
        .unwrap_or("화면")
        .to_owned();
    let ctx = merge(
        base(&state, Some(&who), "editor").await,
        json!({
            "screen": screen.id(),
            "screen_label": label,
            "done": (!q.done.is_empty() && q.done.len() <= 64 && q.done.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')).then_some(q.done.as_str()),
            "screens": data["screens"],
            "editor_json": script_json(&data),
        }),
    );
    render(&state, StatusCode::OK, "editor.html", &ctx)
}

/// page-editor.js 가 읽을 재료. 위젯 값은 사용자·에이전트가 넣은 데이터다.
async fn editor_data(
    state: &AppState,
    who: &SignedIn,
    screen: &EditScreen,
    list: &[Widget],
) -> maru_core::Result<Value> {
    let ctx = &state.ctx;
    let types = widgets::all_types(ctx).await;
    let board_list = boards::list(ctx, &who.actor).await.unwrap_or_default();
    let page_list = pages::list(ctx, &who.actor, None).await.unwrap_or_default();
    let mut screens = vec![json!({ "id": "home", "label": "첫 화면", "group": "첫 화면" })];
    screens.extend(
        page_list
            .iter()
            .filter(|p| p.status == PageStatus::Published)
            .map(|p| json!({ "id": format!("page:{}", p.slug), "label": p.title, "group": "페이지" })),
    );
    screens.extend(
        board_list.iter().map(
            |b| json!({ "id": format!("board:{}", b.slug), "label": b.name, "group": "게시판" }),
        ),
    );
    let names: HashMap<String, String> = board_list
        .iter()
        .map(|b| {
            (
                format!("board:{}", b.slug),
                format!("게시판 「{}」", b.name),
            )
        })
        .chain(page_list.iter().map(|p| {
            (
                format!("page:{}", p.slug),
                format!("페이지 「{}」", p.title),
            )
        }))
        .collect();
    let tz = settings::site(ctx)
        .await
        .map(|s| s.tz_offset_min)
        .unwrap_or(540);
    Ok(json!({
        "screen": screen.id(),
        "screens": screens,
        "scope_names": names,
        "widgets": list.iter().map(draft_of).collect::<Vec<_>>(),
        "kinds": types.iter().map(|t| json!({
            "id": t.id,
            "name": t.name,
            "description": t.description,
            "icon": kind_icon(t.id),
            "group": kind_group(t.id),
        })).collect::<Vec<_>>(),
        "types": types.iter().map(|t| (t.id.to_string(), t.config_schema.clone())).collect::<Map<_, _>>(),
        "boards": board_list.iter().map(|b| json!({ "id": b.slug, "label": b.name })).collect::<Vec<_>>(),
        "regions": WIDGET_REGIONS.iter().map(|r| {
            let (label, hint) = region_text(r);
            json!({ "id": r, "label": label, "hint": hint })
        }).collect::<Vec<_>>(),
        "tz": tz,
    }))
}

/// 왼쪽 「더하기」 판의 묶음.
fn kind_group(kind: &str) -> &'static str {
    match kind {
        "hero" | "image_banner" | "notice_bar" | "cta" | "slider" => "첫 인상",
        "features" | "image_text" | "stats" | "testimonials" | "team" | "timeline"
        | "org_chart" | "faq" => "소개",
        "latest_posts" | "popular_posts" | "post_tabs" | "gallery_grid" => "글 목록",
        "hours" | "visit" | "price_list" | "plans" => "가게·연락",
        "markdown_block" | "menu" | "search" | "account_box" | "popup" => "기본",
        _ => "더 보기",
    }
}

// ---- 초안 ----

/// 편집기가 보내는 블록 하나. 순서는 배열 순서(자리마다)다.
#[derive(Debug, Clone, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
struct DraftWidget {
    /// 저장된 위젯은 그 id, 새 블록은 편집기가 만든 `new-…`
    id: String,
    kind: String,
    region: String,
    scope: String,
    #[serde(default)]
    title: String,
    #[serde(default)]
    config: Value,
    #[serde(default = "yes")]
    enabled: bool,
}

fn yes() -> bool {
    true
}

fn draft_of(w: &Widget) -> Value {
    json!({
        "id": w.id,
        "kind": w.kind,
        "region": w.region,
        "scope": w.scope,
        "title": w.title,
        "config": w.config,
        "enabled": w.enabled,
    })
}

#[derive(Debug, Deserialize)]
struct DraftInput {
    widgets: Vec<DraftWidget>,
}

#[derive(Debug, Deserialize)]
struct PublishInput {
    screen: String,
    widgets: Vec<DraftWidget>,
}

fn check_ids(list: &[DraftWidget]) -> maru_core::Result<()> {
    if list.len() > MAX_DRAFT {
        return Err(Error::new(Code::Validation)
            .with_message("error.widget_too_many")
            .arg("max", MAX_DRAFT.to_string()));
    }
    let mut seen = HashSet::new();
    for w in list {
        let ok = !w.id.is_empty()
            && w.id.len() <= 64
            && w.id
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
            && seen.insert(w.id.as_str());
        if !ok {
            return Err(Error::new(Code::Validation)
                .with_message("error.validation")
                .arg("field", "id")
                .arg("value", w.id.as_str()));
        }
    }
    Ok(())
}

/// 초안을 그릴 위젯 목록으로. 설정이 맞지 않는 블록은 빼고 그 id 를 돌려준다.
async fn to_widgets(state: &AppState, list: &[DraftWidget]) -> (Vec<Widget>, Vec<String>) {
    let now = clock::now();
    let mut order: HashMap<&str, i64> = HashMap::new();
    let mut out = Vec::new();
    let mut invalid = Vec::new();
    for d in list {
        let n = order.entry(d.region.as_str()).or_insert(0);
        let sort_order = *n;
        *n += 1;
        let scope = widgets::check_scope(&d.scope);
        let config = widgets::check_draft(&state.ctx, &d.kind, &d.region, d.config.clone()).await;
        match (scope, config) {
            (Ok(scope), Ok(config)) => out.push(Widget {
                id: d.id.clone(),
                kind: d.kind.clone(),
                region: d.region.clone(),
                scope,
                title: d.title.chars().take(200).collect(),
                config,
                sort_order,
                enabled: d.enabled,
                created_at: now.clone(),
                updated_at: now.clone(),
            }),
            _ => invalid.push(d.id.clone()),
        }
    }
    (out, invalid)
}

// ---- 캔버스 ----

async fn frame_get(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<ScreenQuery>,
) -> Response {
    let who = match require(&state, &headers, "/admin/editor").await {
        Ok(w) => w,
        Err(r) => return *r,
    };
    if let Err(e) = widgets::list(&state.ctx, &who.actor).await {
        return super::fail(&state, Some(&who), &e).await;
    }
    let Some(screen) = EditScreen::parse(&q.screen) else {
        return super::fail(&state, Some(&who), &bad_screen(&q.screen)).await;
    };
    draw(&state, &screen, None, &[]).await
}

async fn frame_post(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<ScreenQuery>,
    body: Result<Json<DraftInput>, JsonRejection>,
) -> Result<Response, ApiError> {
    let who = json_session(&state, &headers).await?;
    widgets::list(&state.ctx, &who.actor).await?;
    let screen = EditScreen::parse(&q.screen).ok_or_else(|| bad_screen(&q.screen))?;
    let Json(input) = body.map_err(rejected)?;
    check_ids(&input.widgets)?;
    let (list, invalid) = to_widgets(&state, &input.widgets).await;
    Ok(draw(&state, &screen, Some(list), &invalid).await)
}

fn rejected(e: JsonRejection) -> ApiError {
    ApiError(
        Error::new(Code::Validation)
            .with_message("error.validation")
            .arg("field", "body")
            .arg("value", e.body_text()),
    )
}

/// 공개 라우터로 그 화면을 손님 눈으로 그린다. 통계·조회수·편지는 사본으로 돌려 세지 않는다.
async fn draw(
    state: &AppState,
    screen: &EditScreen,
    draft: Option<Vec<Widget>>,
    invalid: &[String],
) -> Response {
    let mut inner = state.clone();
    inner.views = Arc::new(ViewCounter::new());
    inner.stats = Arc::new(StatCounter::new());
    inner.mailer = None;
    inner.link_fetcher = None;
    inner.previews = None;
    let mut app = crate::public::routes().with_state(inner);
    let mut req = Request::new(Body::empty());
    *req.uri_mut() = match screen.path().parse() {
        Ok(u) => u,
        Err(_) => return StatusCode::BAD_REQUEST.into_response(),
    };
    req.extensions_mut()
        .insert(ConnectInfo(SocketAddr::from(([127, 0, 0, 1], 0))));
    let editing = Editing {
        draft: draft.map(Arc::new),
    };
    let mut res = match EDITING.scope(editing, app.call(req)).await {
        Ok(r) => r,
        Err(never) => match never {},
    };
    let h = res.headers_mut();
    // 공개 화면 CSP(frame-ancestors 'self')는 바깥 층이 붙인다. 캐시는 하지 않는다.
    h.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    h.remove(header::ETAG);
    h.remove(header::LAST_MODIFIED);
    if !invalid.is_empty()
        && let Ok(v) = HeaderValue::from_str(&invalid.join(","))
    {
        h.insert("x-maru-invalid", v);
    }
    res
}

// ---- 발행 ----

async fn publish(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Result<Json<PublishInput>, JsonRejection>,
) -> Result<Response, ApiError> {
    let who = json_session(&state, &headers).await?;
    let (ctx, actor) = (&state.ctx, &who.actor);
    let Json(input) = body.map_err(rejected)?;
    let screen = EditScreen::parse(&input.screen).ok_or_else(|| bad_screen(&input.screen))?;
    check_ids(&input.widgets)?;
    let current = widgets::list(ctx, actor).await?;
    let ops = plan_ops(&current, &input.widgets);
    if ops.is_empty() {
        return Ok(Json(Success::new(json!({ "changeset": null, "ops": 0 }))).into_response());
    }
    let label = screen_label(&state, &who, &screen).await;
    let count = ops.len();
    let cs = change::create(ctx, actor, &format!("화면 편집: {label}"), ops).await?;
    let sandbox = state.sandbox()?;
    let applied = change::apply(ctx, actor, sandbox.as_ref(), &cs.id).await;
    state.close_preview(&cs.id).await;
    let cs = applied?;
    Ok(Json(Success::new(json!({
        "changeset": cs.id,
        "url": format!("/admin/changesets/{}", cs.id),
        "ops": count,
    })))
    .into_response())
}

async fn screen_label(state: &AppState, who: &SignedIn, screen: &EditScreen) -> String {
    match screen {
        EditScreen::Home => "첫 화면".into(),
        EditScreen::Page(slug) => pages::list(&state.ctx, &who.actor, None)
            .await
            .ok()
            .and_then(|l| l.into_iter().find(|p| &p.slug == slug))
            .map_or_else(|| slug.clone(), |p| format!("페이지 {}", p.title)),
        EditScreen::Board(slug) => boards::list(&state.ctx, &who.actor)
            .await
            .ok()
            .and_then(|l| l.into_iter().find(|b| &b.slug == slug))
            .map_or_else(|| slug.clone(), |b| format!("게시판 {}", b.name)),
    }
}

/// 지금 위젯과 초안을 견주어 변경 묶음 연산을 만든다: 지우기 → 고치기 → 놓기.
///
/// 순서: 자리마다 원래 있던 블록의 앞뒤가 그대로이고 새로 온 블록이 모두 끝에 붙었으면 원래 번호를 두고
/// 새 블록만 뒤에 잇는다. 아니면 그 자리 전체를 0, 10, 20… 으로 다시 매긴다.
fn plan_ops(current: &[Widget], draft: &[DraftWidget]) -> Vec<Value> {
    let by_id: HashMap<&str, &Widget> = current.iter().map(|w| (w.id.as_str(), w)).collect();
    let kept: HashSet<&str> = draft.iter().map(|d| d.id.as_str()).collect();
    let draft_region: HashMap<&str, &str> = draft
        .iter()
        .map(|d| (d.id.as_str(), d.region.as_str()))
        .collect();
    let mut removes = Vec::new();
    let mut configures = Vec::new();
    let mut places = Vec::new();

    for w in current {
        if !kept.contains(w.id.as_str()) {
            removes.push(json!({ "op": "widget.remove", "id": w.id }));
        }
    }

    // 자리마다 새 순서 번호. None 이면 그대로 둔다.
    let mut position: HashMap<&str, Option<i64>> = HashMap::new();
    let mut regions: Vec<&str> = draft.iter().map(|d| d.region.as_str()).collect();
    regions.sort_unstable();
    regions.dedup();
    for region in regions {
        let here: Vec<&DraftWidget> = draft.iter().filter(|d| d.region == region).collect();
        let mut before: Vec<&Widget> = current
            .iter()
            .filter(|w| w.region == region && draft_region.get(w.id.as_str()) == Some(&region))
            .collect();
        before.sort_by_key(|w| w.sort_order);
        let stayed = |d: &DraftWidget| by_id.get(d.id.as_str()).is_some_and(|w| w.region == region);
        let stayed_ids: Vec<&str> = here
            .iter()
            .filter(|d| stayed(d))
            .map(|d| d.id.as_str())
            .collect();
        let before_ids: Vec<&str> = before.iter().map(|w| w.id.as_str()).collect();
        let tail_only = here.iter().skip_while(|d| stayed(d)).all(|d| !stayed(d));
        if stayed_ids == before_ids && tail_only {
            let mut next = current
                .iter()
                .filter(|w| w.region == region)
                .map(|w| w.sort_order + 1)
                .max()
                .unwrap_or(0);
            for d in here {
                if stayed(d) {
                    position.insert(d.id.as_str(), None);
                } else {
                    position.insert(d.id.as_str(), Some(next));
                    next += 1;
                }
            }
        } else {
            for (i, d) in here.iter().enumerate() {
                let want = i as i64 * 10;
                let same = by_id
                    .get(d.id.as_str())
                    .is_some_and(|w| w.region == region && w.sort_order == want);
                position.insert(d.id.as_str(), (!same).then_some(want));
            }
        }
    }

    for d in draft {
        let pos = position.get(d.id.as_str()).copied().flatten();
        match by_id.get(d.id.as_str()) {
            None => {
                let mut op = json!({
                    "op": "widget.place",
                    "kind": d.kind,
                    "region": d.region,
                    "scope": d.scope,
                    "title": d.title,
                    "config": d.config,
                    "enabled": d.enabled,
                });
                if let Some(p) = pos {
                    op["position"] = json!(p);
                }
                places.push(op);
            }
            Some(w) => {
                let mut patch = Map::new();
                if w.region != d.region {
                    patch.insert("region".into(), json!(d.region));
                }
                if w.scope != d.scope {
                    patch.insert("scope".into(), json!(d.scope));
                }
                if w.title != d.title {
                    patch.insert("title".into(), json!(d.title));
                }
                if w.config != d.config {
                    patch.insert("config".into(), d.config.clone());
                }
                if w.enabled != d.enabled {
                    patch.insert("enabled".into(), json!(d.enabled));
                }
                if let Some(p) = pos {
                    patch.insert("position".into(), json!(p));
                }
                if !patch.is_empty() {
                    configures
                        .push(json!({ "op": "widget.configure", "id": w.id, "patch": patch }));
                }
            }
        }
    }
    removes
        .into_iter()
        .chain(configures)
        .chain(places)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn w(id: &str, region: &str, order: i64) -> Widget {
        Widget {
            id: id.into(),
            kind: "hero".into(),
            region: region.into(),
            scope: "home".into(),
            title: String::new(),
            config: json!({ "title": id }),
            sort_order: order,
            enabled: true,
            created_at: String::new(),
            updated_at: String::new(),
        }
    }

    fn d(w: &Widget) -> DraftWidget {
        DraftWidget {
            id: w.id.clone(),
            kind: w.kind.clone(),
            region: w.region.clone(),
            scope: w.scope.clone(),
            title: w.title.clone(),
            config: w.config.clone(),
            enabled: w.enabled,
        }
    }

    #[test]
    fn same_draft_makes_no_ops() {
        let cur = vec![w("a", "main", 0), w("b", "main", 1), w("c", "hero", 5)];
        let draft: Vec<_> = cur.iter().map(d).collect();
        assert!(plan_ops(&cur, &draft).is_empty());
    }

    #[test]
    fn new_block_at_end_keeps_old_numbers() {
        let cur = vec![w("a", "main", 3), w("b", "main", 7)];
        let mut draft: Vec<_> = cur.iter().map(d).collect();
        let mut n = d(&w("new-1", "main", 0));
        n.config = json!({ "title": "새" });
        draft.push(n);
        let ops = plan_ops(&cur, &draft);
        assert_eq!(ops.len(), 1);
        assert_eq!(ops[0]["op"], "widget.place");
        assert_eq!(ops[0]["position"], 8);
    }

    #[test]
    fn reorder_renumbers_the_region() {
        let cur = vec![w("a", "main", 0), w("b", "main", 1), w("x", "hero", 0)];
        let draft = vec![d(&cur[1]), d(&cur[0]), d(&cur[2])];
        let ops = plan_ops(&cur, &draft);
        // b → 0, a → 10. hero 는 그대로.
        assert_eq!(ops.len(), 2);
        assert_eq!(ops[0]["id"], "b");
        assert_eq!(ops[0]["patch"]["position"], 0);
        assert_eq!(ops[1]["id"], "a");
        assert_eq!(ops[1]["patch"]["position"], 10);
    }

    #[test]
    fn remove_hide_move_and_edit() {
        let cur = vec![w("a", "main", 0), w("b", "main", 1), w("c", "main", 2)];
        let mut b = d(&cur[1]);
        b.enabled = false;
        b.title = "제목".into();
        let mut c = d(&cur[2]);
        c.region = "hero".into();
        let ops = plan_ops(&cur, &[b, c]);
        assert_eq!(ops[0], json!({ "op": "widget.remove", "id": "a" }));
        assert_eq!(ops[1]["patch"]["enabled"], false);
        assert_eq!(ops[1]["patch"]["title"], "제목");
        // 남은 b 는 앞뒤가 그대로라 번호를 두고, 큰 배너 자리로 간 c 는 그 자리 끝에 붙는다.
        assert!(ops[1]["patch"].get("position").is_none());
        assert_eq!(ops[2]["patch"]["region"], "hero");
        assert_eq!(ops[2]["patch"]["position"], 0);
    }

    #[test]
    fn screen_ids_round_trip() {
        for s in ["home", "page:about", "board:free"] {
            assert_eq!(EditScreen::parse(s).map(|x| x.id()), Some(s.to_owned()));
        }
        assert_eq!(EditScreen::parse(""), Some(EditScreen::Home));
        assert_eq!(EditScreen::parse("page:../x"), None);
        assert_eq!(EditScreen::parse("post:a"), None);
        assert_eq!(
            EditScreen::parse("board:free").map(|s| s.path()),
            Some("/b/free".into())
        );
    }

    #[test]
    fn draft_ids_are_checked() {
        let a = d(&w("a", "main", 0));
        assert!(check_ids(std::slice::from_ref(&a)).is_ok());
        assert!(check_ids(&[a.clone(), a.clone()]).is_err());
        let mut bad = a;
        bad.id = "a\"><x".into();
        assert!(check_ids(&[bad]).is_err());
    }
}
