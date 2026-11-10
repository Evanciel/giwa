//! 화면 편집기 (ADR-0138). 실제 공개 화면 위에서 블록(위젯·섹션)을 끌어다 놓고 고친다.
//!
//! - `/admin/editor`: 편집기 화면. 캔버스 iframe 을 담으려고 자기 CSP(`frame-src 'self'`)를 쓴다.
//! - `/admin/editor/frame`: 캔버스. 공개 라우터로 그 화면을 그리되, 초안이 오면 DB 대신 초안 위젯을 그린다.
//!   응답은 공개 화면 CSP(`frame-ancestors 'self'`) 그대로다.
//! - `/admin/editor/publish`: 초안과 지금 위젯을 견주어 연산을 만들고 변경 묶음 하나로 적용한다(원칙 4).
//! - `/admin/editor/media`: 그림 판(ADR-0147)이 보일 올린 그림 목록. 올리기는 `/admin/media` 그대로다.
//! - `/admin/editor/pages`: 틀을 골라 공개 전 페이지를 만든다(ADR-0149). 발행하면 `page.publish` 가 같은 묶음에 든다.
//! - `/admin/editor/apps/{id}`: 꺼진 앱을 그 자리에서 켜고 새 블록 종류를 돌려준다(ADR-0151).
//! - `/admin/editor/draft`: 고치던 초안을 서버에 둔다(ADR-0152). 다른 기기에서 이어 고치고, 발행하면 지운다.
//! - `/admin/editor/tokens`: 디자인 판(ADR-0148)의 토큰 초안을 저장하지 않고 검사해 CSS 로 돌려준다.
//!   발행하면 `tokens.set` 연산이 블록·사이트 연산과 같은 변경 묶음에 든다.
//!
//! 초안에는 블록 말고 사이트 공통 부분(이름·설명·메뉴, ADR-0145)도 실을 수 있다. 발행하면 `settings.set`·`menu.set` 이
//! 같은 변경 묶음에 들어간다.
//!
//! 서비스는 REST·CLI·MCP 와 같은 `widgets`·`changesets` 를 부른다.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::net::SocketAddr;
use std::sync::Arc;

use axum::body::Body;
use axum::extract::rejection::JsonRejection;
use axum::extract::{ConnectInfo, Path, Query, Request, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post, put};
use axum::{Json, Router, middleware};
use maru_core::clock;
use maru_core::design::FONTS;
use maru_core::model::{MENU_LOCATIONS, MenuItem, PageReq, PageStatus, WIDGET_REGIONS, Widget};
use maru_core::output::Success;
use maru_core::services::auth::SignedIn;
use maru_core::services::changesets as change;
use maru_core::services::presets::Op;
use maru_core::services::stats::StatCounter;
use maru_core::services::views::ViewCounter;
use maru_core::services::widgets;
use maru_core::services::{boards, design, editor_drafts, fonts, media, menus, pages, settings};
use maru_core::urls::{self, Target};
use maru_core::{Code, Error, Permission};
use maru_modules::service as modules;
use serde::Deserialize;
use serde_json::{Map, Value, json};
use tower::Service;

use super::widgets::{kind_icon, region_text, script_json};
use super::{base, json_session, merge, render, require};
use crate::AppState;
use crate::error::ApiError;
use crate::widgets::{EDITING, Editing, SiteDraft};

/// 초안 하나에 담을 수 있는 블록 수. 위젯 전체 한도보다 넉넉하지 않게.
const MAX_DRAFT: usize = 300;

pub(super) fn routes() -> Router<AppState> {
    Router::new()
        .route(
            "/admin/editor",
            get(page).layer(middleware::map_response(editor_headers)),
        )
        .route("/admin/editor/frame", get(frame_get).post(frame_post))
        .route("/admin/editor/media", get(media_list))
        .route(
            "/admin/editor/apps/{id}",
            post(enable_app).layer(middleware::map_response(editor_headers)),
        )
        .route(
            "/admin/editor/pages",
            post(new_page).layer(middleware::map_response(editor_headers)),
        )
        .route("/admin/editor/tokens", post(tokens_css))
        .route(
            "/admin/editor/draft",
            put(draft_put)
                .delete(draft_delete)
                .layer(middleware::map_response(editor_headers)),
        )
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
    /// 새 페이지 틀 (ADR-0149). 공개 전이고 블록이 없는 페이지에서만 초안으로 싣는다.
    #[serde(default)]
    starter: String,
    /// `1` 이면 새 페이지를 위 메뉴 초안에 넣는다.
    #[serde(default)]
    menu: String,
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
    let mut data = match editor_data(&state, &who, &screen, &list).await {
        Ok(d) => d,
        Err(e) => return super::fail(&state, Some(&who), &e).await,
    };
    if let EditScreen::Page(slug) = &screen
        && !q.starter.is_empty()
        && let Err(e) = starter_draft(&state, &who, slug, &q, &list, &mut data).await
    {
        return super::fail(&state, Some(&who), &e).await;
    }
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
            "starters": maru_themes::starters().unwrap_or_default(),
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
            .filter(|p| p.status != PageStatus::Deleted)
            .map(|p| {
                let label = match p.status {
                    PageStatus::Published => p.title.clone(),
                    PageStatus::Hidden => format!("{} (숨김)", p.title),
                    _ => format!("{} (공개 전)", p.title),
                };
                json!({ "id": format!("page:{}", p.slug), "label": label, "group": "페이지" })
            }),
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
    let site = settings::site(ctx).await?;
    let tokens = design::tokens(ctx).await?;
    let mut font_list = vec![
        json!({ "id": FONTS[0].0, "label": "프리텐다드 (고딕)" }),
        json!({ "id": FONTS[1].0, "label": "기기 기본 고딕" }),
        json!({ "id": FONTS[2].0, "label": "명조" }),
    ];
    for f in fonts::list(ctx).await? {
        font_list.push(json!({ "id": f.slug, "label": format!("{} (올린 서체)", f.name) }));
    }
    let mut menu_map = Map::new();
    for loc in MENU_LOCATIONS {
        menu_map.insert(loc.into(), json!(menus::get(ctx, loc).await?));
    }
    let page_status = match screen {
        EditScreen::Page(slug) => page_list
            .iter()
            .find(|p| &p.slug == slug)
            .map(|p| p.status.as_str()),
        _ => None,
    };
    let draft = editor_drafts::get(ctx, &who.actor, &screen.id())
        .await?
        .map(|d| json!({ "body": d.body, "base": d.base, "updated_at": d.updated_at }));
    Ok(json!({
        "screen": screen.id(),
        "page_status": page_status,
        "draft": draft,
        "screens": screens,
        "scope_names": names,
        "widgets": list.iter().map(draft_of).collect::<Vec<_>>(),
        "kinds": kinds_json(&types),
        "types": types_json(&types),
        "bundles": bundles(
            &types.iter().map(|t| t.id).collect(),
            &board_list.iter().map(|b| b.slug.clone()).collect::<Vec<_>>(),
        ),
        "apps": off_apps(state).await,
        "can_enable": who.actor.has(Permission::ModuleManage),
        "boards": board_list.iter().map(|b| json!({ "id": b.slug, "label": b.name })).collect::<Vec<_>>(),
        "regions": WIDGET_REGIONS.iter().map(|r| {
            let (label, hint) = region_text(r);
            json!({ "id": r, "label": label, "hint": hint })
        }).collect::<Vec<_>>(),
        "tz": site.tz_offset_min,
        "site": { "name": site.name, "description": site.description, "menus": menu_map },
        "design": {
            "primary": tokens.color.primary,
            "font_body": tokens.font.body,
            "font_heading": tokens.font.heading,
            "radius": tokens.radius,
            "density": tokens.layout.density,
        },
        "fonts": font_list,
        "pages": page_list
            .iter()
            .filter(|p| p.status == PageStatus::Published)
            .map(|p| json!({ "id": p.slug, "label": p.title }))
            .collect::<Vec<_>>(),
    }))
}

/// 새 페이지 틀 (ADR-0149). 아직 공개하지 않았고 블록이 하나도 없는 페이지에만 틀의 블록을 초안으로 싣는다.
/// 다시 열어도 이미 블록이 있으면 싣지 않는다.
async fn starter_draft(
    state: &AppState,
    who: &SignedIn,
    slug: &str,
    q: &ScreenQuery,
    list: &[Widget],
    data: &mut Value,
) -> maru_core::Result<()> {
    let page = pages::editable_page(&state.ctx, &who.actor, slug).await?;
    let scope = format!("page:{}", page.slug);
    if page.status == PageStatus::Published || list.iter().any(|w| w.scope == scope) {
        return Ok(());
    }
    let starter = maru_themes::starter(&q.starter)?;
    let drafts: Vec<Value> = starter
        .widgets(&page.slug, &page.title)
        .into_iter()
        .map(|w| {
            json!({
                "id": "", "kind": w.kind, "region": w.region, "scope": w.scope,
                "title": w.title, "config": w.config, "enabled": true, "hide_on": [],
            })
        })
        .collect();
    data["starter"] = json!(drafts);
    data["add_menu"] = json!(q.menu == "1");
    data["page_title"] = json!(page.title);
    Ok(())
}

/// 더하기 판의 블록 카드.
fn kinds_json(types: &[widgets::WidgetType]) -> Vec<Value> {
    types
        .iter()
        .map(|t| {
            json!({
                "id": t.id,
                "name": t.name,
                "description": t.description,
                "icon": kind_icon(t.id),
                "group": kind_group(t.id),
            })
        })
        .collect()
}

/// 빈 화면에서 고를 시작 묶음 (ADR-0153). 사이트 종류 프리셋의 첫 화면 본문 블록을 그대로 쓴다.
/// 이 사이트에서 쓸 수 없는 블록은 빼고, 없는 게시판을 가리키는 곳은 [`fit_boards`] 로 고친다.
/// 처음 보일 셋([`BUNDLE_PICKS`])을 앞에 둔다.
fn bundles(kinds: &HashSet<&str>, boards: &[String]) -> Vec<Value> {
    let mut out: Vec<Value> = maru_themes::preset_ids()
        .iter()
        .filter_map(|id| maru_themes::preset(id).ok())
        .filter_map(|p| {
            let blocks: Vec<Value> = p
                .ops
                .iter()
                .filter_map(|op| match op {
                    Op::WidgetsEnsure { items } => Some(items),
                    _ => None,
                })
                .flatten()
                .filter(|w| w.region == "main" && w.scope == "home" && kinds.contains(w.kind.as_str()))
                .filter_map(|w| {
                    let mut config = w.config.clone();
                    fit_boards(&mut config, boards).then(|| {
                        json!({ "kind": w.kind, "region": w.region, "title": w.title, "config": config })
                    })
                })
                .collect();
            (blocks.len() >= 2).then(|| {
                json!({ "id": p.id, "name": p.name, "description": p.description, "blocks": blocks })
            })
        })
        .collect();
    out.sort_by_key(|b| {
        BUNDLE_PICKS
            .iter()
            .position(|p| b["id"] == *p)
            .unwrap_or(BUNDLE_PICKS.len())
    });
    out
}

/// 빈 화면에 먼저 보일 시작 묶음. 나머지는 「다른 종류」에서 고른다.
const BUNDLE_PICKS: [&str; 3] = ["company", "cafe", "portfolio"];

/// 이 사이트에 없는 게시판을 가리키는 곳을 고친다. `board` 칸은 첫 게시판으로, `/b/…` 링크는 첫 게시판
/// (없으면 첫 화면)으로. 게시판이 하나도 없는데 `board` 칸이 있으면 `false` 다(그 블록은 뺀다).
fn fit_boards(v: &mut Value, boards: &[String]) -> bool {
    let known = |slug: &str| boards.iter().any(|b| b == slug);
    match v {
        Value::Object(map) => map.iter_mut().all(|(k, x)| {
            let text = x.as_str().map(str::to_owned);
            match (k.as_str(), text.as_deref()) {
                ("board", Some(b)) if !known(b) => match boards.first() {
                    Some(first) => {
                        *x = json!(first);
                        true
                    }
                    None => false,
                },
                ("link", Some(l)) => {
                    let slug = l
                        .strip_prefix("/b/")
                        .map(|r| r.split(['/', '?', '#']).next().unwrap_or_default());
                    if slug.is_some_and(|s| !known(s)) {
                        *x = json!(boards.first().map_or("/".to_owned(), |f| format!("/b/{f}")));
                    }
                    true
                }
                _ => fit_boards(x, boards),
            }
        }),
        Value::Array(a) => a.iter_mut().all(|x| fit_boards(x, boards)),
        _ => true,
    }
}

/// 블록 종류마다 설정 형식.
fn types_json(types: &[widgets::WidgetType]) -> Map<String, Value> {
    types
        .iter()
        .map(|t| (t.id.to_string(), t.config_schema.clone()))
        .collect()
}

/// 꺼진 앱이 주는 블록 (ADR-0151). 「앱 더하기」 묶음에 흐린 카드로 보인다.
async fn off_apps(state: &AppState) -> Vec<Value> {
    let on: Vec<&str> = state
        .modules
        .enabled(&state.ctx)
        .await
        .iter()
        .map(|m| m.manifest().id)
        .collect();
    state
        .modules
        .iter()
        .filter(|m| !on.contains(&m.manifest().id))
        .flat_map(|m| {
            let man = m.manifest();
            m.widget_types().into_iter().map(move |t| {
                json!({
                    "app": man.id,
                    "app_name": man.name,
                    "id": t.id,
                    "name": t.name,
                    "description": t.description,
                })
            })
        })
        .collect()
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
    /// 이 기기에서만 숨김 (ADR-0146)
    #[serde(default)]
    hide_on: Vec<String>,
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
        "hide_on": w.hide_on,
    })
}

/// 사이트 공통 부분 초안 (ADR-0145). 메뉴는 고친 위치만 실어도 된다.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct SiteInput {
    name: String,
    #[serde(default)]
    description: String,
    #[serde(default)]
    menus: BTreeMap<String, Vec<MenuItem>>,
}

#[derive(Debug, Deserialize)]
struct DraftInput {
    widgets: Vec<DraftWidget>,
    #[serde(default)]
    site: Option<SiteInput>,
}

#[derive(Debug, Deserialize)]
struct PublishInput {
    screen: String,
    widgets: Vec<DraftWidget>,
    #[serde(default)]
    site: Option<SiteInput>,
    /// 디자인 판의 토큰 초안 (ADR-0148). 지금 값과 다르면 `tokens.set` 연산이 된다.
    #[serde(default)]
    tokens: Option<Value>,
}

/// 사이트 공통 부분 초안을 그릴 값으로. 검사에 걸린 칸은 저장된 값으로 그리고 `site:<칸>` 으로 알린다.
async fn to_site(
    state: &AppState,
    input: &SiteInput,
) -> maru_core::Result<(SiteDraft, Vec<String>)> {
    let saved = settings::site(&state.ctx).await?;
    let mut invalid = Vec::new();
    let name = settings::check_name(&input.name).unwrap_or_else(|_| {
        invalid.push("site:name".to_owned());
        saved.name.clone()
    });
    let description = settings::check_description(&input.description).unwrap_or_else(|_| {
        invalid.push("site:description".to_owned());
        saved.description.clone()
    });
    let mut out = Vec::new();
    for (loc, items) in &input.menus {
        match menus::check_location(loc).and_then(|()| menus::check(items.clone())) {
            Ok(items) => out.push((loc.clone(), items)),
            Err(_) => invalid.push(format!("site:menu.{loc}")),
        }
    }
    Ok((
        SiteDraft {
            name,
            description,
            menus: out,
        },
        invalid,
    ))
}

/// 사이트 공통 부분의 발행 연산. 저장된 값과 다른 것만 만든다. 틀린 값은 발행을 막는다.
async fn site_ops(state: &AppState, input: &SiteInput) -> maru_core::Result<Vec<Value>> {
    let ctx = &state.ctx;
    let saved = settings::site(ctx).await?;
    let mut ops = Vec::new();
    let mut values = Map::new();
    let name = settings::check_name(&input.name)?;
    if name != saved.name {
        values.insert("name".into(), json!(name));
    }
    let description = settings::check_description(&input.description)?;
    if description != saved.description {
        values.insert("description".into(), json!(description));
    }
    if !values.is_empty() {
        ops.push(json!({ "op": "settings.set", "values": values }));
    }
    for (loc, items) in &input.menus {
        menus::check_location(loc)?;
        let items = menus::check(items.clone())?;
        if items != menus::get(ctx, loc).await? {
            ops.push(json!({ "op": "menu.set", "location": loc, "items": items }));
        }
    }
    Ok(ops)
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
        let hide_on = widgets::check_hide_on(&d.hide_on);
        match (scope, config, hide_on) {
            (Ok(scope), Ok(config), Ok(hide_on)) => out.push(Widget {
                id: d.id.clone(),
                kind: d.kind.clone(),
                region: d.region.clone(),
                scope,
                title: d.title.chars().take(200).collect(),
                config,
                sort_order,
                enabled: d.enabled,
                hide_on,
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
    draw(&state, &who, &screen, None, None, &[]).await
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
    let (list, mut invalid) = to_widgets(&state, &input.widgets).await;
    let site = match &input.site {
        Some(s) => {
            let (draft, bad) = to_site(&state, s).await?;
            invalid.extend(bad);
            Some(draft)
        }
        None => None,
    };
    Ok(draw(&state, &who, &screen, Some(list), site, &invalid).await)
}

#[derive(Deserialize)]
struct MediaQuery {
    #[serde(default = "first_page")]
    page: u32,
}

fn first_page() -> u32 {
    1
}

/// 그림 판 한 쪽의 그림 수.
const MEDIA_PER_PAGE: u32 = 24;

/// 그림 판(ADR-0147) 목록. 최근 것부터. 쿠키 세션과 CSRF 머리글을 받는다(캔버스 POST 와 같다).
async fn media_list(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<MediaQuery>,
) -> Result<Response, ApiError> {
    let who = json_session(&state, &headers).await?;
    let list = media::list(&state.ctx, &who.actor, PageReq::new(q.page, MEDIA_PER_PAGE)).await?;
    let shown =
        i64::from(list.page.saturating_sub(1)) * i64::from(list.per_page) + list.items.len() as i64;
    Ok(Json(Success::new(json!({
        "items": list.items,
        "more": shown < list.total,
    })))
    .into_response())
}

#[derive(Deserialize)]
struct TokensInput {
    tokens: Value,
}

/// 디자인 판(ADR-0148): 토큰 초안을 저장하지 않고 검사해, 캔버스에 덧입힐 CSS 를 돌려준다.
async fn tokens_css(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Result<Json<TokensInput>, JsonRejection>,
) -> Result<Response, ApiError> {
    let who = json_session(&state, &headers).await?;
    let Json(input) = body.map_err(rejected)?;
    let tokens = design::preview(&state.ctx, &who.actor, input.tokens).await?;
    let uploaded = fonts::list(&state.ctx).await?;
    Ok((
        [
            (header::CONTENT_TYPE, "text/css; charset=utf-8"),
            (header::CACHE_CONTROL, "no-store"),
        ],
        tokens.to_css(&uploaded),
    )
        .into_response())
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
    who: &SignedIn,
    screen: &EditScreen,
    draft: Option<Vec<Widget>>,
    site: Option<SiteDraft>,
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
        site: site.map(Arc::new),
        actor: Some(who.actor.clone()),
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

// ---- 새 페이지 (ADR-0149) ----

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct NewPageInput {
    title: String,
    slug: String,
    starter: String,
}

/// 틀을 골라 초안 페이지를 만든다. 공개되지 않고, 블록은 편집기 초안으로 들어간다(`?starter=`).
/// 같은 주소가 있으면 만들지 않고 알린다. 발행하면 `page.publish` 가 블록과 같은 묶음에 든다.
async fn new_page(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Result<Json<NewPageInput>, JsonRejection>,
) -> Result<Response, ApiError> {
    let who = json_session(&state, &headers).await?;
    let Json(input) = body.map_err(rejected)?;
    let starter = maru_themes::starter(&input.starter)?;
    let page = pages::create(
        &state.ctx,
        &who.actor,
        pages::starter_page(&input.slug, &input.title),
    )
    .await?;
    let screen = EditScreen::Page(page.slug.clone()).id();
    Ok(Json(Success::new(json!({
        "screen": screen,
        "url": format!(
            "/admin/editor?screen={}&starter={}",
            super::encode(&screen),
            super::encode(&starter.id)
        ),
    })))
    .into_response())
}

// ---- 앱 켜기 (ADR-0151) ----

/// 꺼진 앱을 켠다. 발행 묶음에 넣지 않고 바로 켠다. 켜져야 앱의 표와 화면이 생겨 캔버스가 그 블록을 그린다.
/// 모듈 관리 권한이 있어야 하고(서비스가 본다), 감사 기록에 남으며 관리 → 앱에서 끈다.
/// 켠 뒤의 블록 종류와 설정 형식을 돌려준다. 편집기는 그 블록을 초안에 넣는다.
async fn enable_app(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<Response, ApiError> {
    let who = json_session(&state, &headers).await?;
    modules::enable(&state.ctx, &who.actor, state.modules.as_ref(), &id).await?;
    let types = widgets::all_types(&state.ctx).await;
    Ok(Json(Success::new(json!({
        "kinds": kinds_json(&types),
        "types": types_json(&types),
        "apps": off_apps(&state).await,
    })))
    .into_response())
}

// ---- 초안 (ADR-0152) ----

#[derive(Deserialize)]
struct SavedDraftInput {
    body: Value,
    #[serde(default)]
    base: String,
}

/// 고치던 초안을 덮어쓴다. 편집기가 고칠 때마다 2초 묶어 부른다.
async fn draft_put(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<ScreenQuery>,
    body: Result<Json<SavedDraftInput>, JsonRejection>,
) -> Result<Response, ApiError> {
    let who = json_session(&state, &headers).await?;
    let Json(input) = body.map_err(rejected)?;
    let screen = EditScreen::parse(&q.screen).ok_or_else(|| bad_screen(&q.screen))?;
    let d = editor_drafts::save(
        &state.ctx,
        &who.actor,
        &screen.id(),
        input.body,
        &input.base,
    )
    .await?;
    Ok(Json(Success::new(json!({ "updated_at": d.updated_at }))).into_response())
}

/// 초안을 버린다. 없어도 같다(멱등).
async fn draft_delete(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<ScreenQuery>,
) -> Result<Response, ApiError> {
    let who = json_session(&state, &headers).await?;
    let screen = EditScreen::parse(&q.screen).ok_or_else(|| bad_screen(&q.screen))?;
    let gone = editor_drafts::discard(&state.ctx, &who.actor, &screen.id()).await?;
    Ok(Json(Success::new(json!({ "deleted": gone }))).into_response())
}

/// 발행한 화면의 초안은 지운다. 발행은 이미 됐으므로 실패해도 기록만 남긴다.
async fn forget_draft(state: &AppState, who: &SignedIn, screen: &EditScreen) {
    if let Err(e) = editor_drafts::discard(&state.ctx, &who.actor, &screen.id()).await {
        tracing::warn!(error = ?e, "발행한 화면의 초안을 지우지 못했습니다");
    }
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
    let mut ops = match &input.site {
        Some(s) => site_ops(&state, s).await?,
        None => Vec::new(),
    };
    if let Some(patch) = input
        .tokens
        .filter(|p| p.as_object().is_some_and(|o| !o.is_empty()))
        && design::preview(ctx, actor, patch.clone()).await? != design::tokens(ctx).await?
    {
        ops.push(json!({ "op": "tokens.set", "tokens": patch }));
    }
    ops.extend(plan_ops(&current, &input.widgets));
    // 아직 공개하지 않은 페이지는 발행하면 공개된다. 묶음을 되돌리면 다시 숨는다 (ADR-0149).
    if let EditScreen::Page(slug) = &screen {
        let page = pages::editable_page(ctx, actor, slug).await?;
        if page.status != PageStatus::Published {
            ops.insert(0, json!({ "op": "page.publish", "slug": page.slug }));
        }
    }
    if ops.is_empty() {
        forget_draft(&state, &who, &screen).await;
        return Ok(Json(Success::new(json!({ "changeset": null, "ops": 0 }))).into_response());
    }
    let label = screen_label(&state, &who, &screen).await;
    let count = ops.len();
    let cs = change::create(ctx, actor, &format!("화면 편집: {label}"), ops).await?;
    let sandbox = state.sandbox()?;
    let applied = change::apply(ctx, actor, sandbox.as_ref(), &cs.id).await;
    state.close_preview(&cs.id).await;
    let cs = applied?;
    forget_draft(&state, &who, &screen).await;
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
                    "hide_on": d.hide_on,
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
                if w.hide_on != d.hide_on {
                    patch.insert("hide_on".into(), json!(d.hide_on));
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
            hide_on: Vec::new(),
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
            hide_on: w.hide_on.clone(),
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
    fn bundles_fit_this_site() {
        let boards = vec!["free".to_owned()];
        let mut v = json!({ "board": "notice", "button": { "link": "/b/notice/1" },
            "items": [{ "link": "/b/free" }, { "link": "/p/about" }] });
        assert!(fit_boards(&mut v, &boards));
        assert_eq!(v["board"], "free");
        assert_eq!(v["button"]["link"], "/b/free");
        assert_eq!(v["items"][1]["link"], "/p/about");
        let mut v = json!({ "tabs": [{ "board": "notice" }] });
        assert!(!fit_boards(&mut v, &[]), "게시판이 없으면 그 블록은 뺀다");
        let mut v = json!({ "link": "/b/notice" });
        assert!(fit_boards(&mut v, &[]));
        assert_eq!(v["link"], "/");

        let kinds: HashSet<&str> = ["hero", "features", "visit", "hours", "price_list"].into();
        let list = bundles(&kinds, &boards);
        assert_eq!(list[0]["id"], "company", "먼저 보일 묶음이 앞");
        assert!(list.iter().any(|b| b["id"] == "cafe"));
        for b in &list {
            let blocks = b["blocks"].as_array().unwrap();
            assert!(blocks.len() >= 2);
            assert!(
                blocks
                    .iter()
                    .all(|w| kinds.contains(w["kind"].as_str().unwrap()))
            );
            assert!(!b.to_string().contains("/b/notice"), "{}", b["id"]);
        }
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
