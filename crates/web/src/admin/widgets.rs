//! 위젯 관리 화면 (ADR-0137). 설정은 JSON 대신 사람 말로 보이고, 고치는 칸은 위젯 종류의
//! JSON Schema 를 `/admin/assets/admin-ui.js` 가 읽어 그린다. JS 가 없으면 「설정 원문」 칸에 JSON 을 쓴다.
//! 서비스는 REST·CLI·MCP 와 같은 `widgets` 를 부른다.

use axum::Router;
use axum::extract::{Form, Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Redirect, Response};
use axum::routing::{get, post};
use giwa_core::model::{PageStatus, WIDGET_REGIONS, Widget};
use giwa_core::services::auth::SignedIn;
use giwa_core::services::widgets::{self, NewWidget, WidgetPatch, WidgetType};
use giwa_core::services::{boards, pages, settings};
use giwa_core::urls;
use giwa_core::{Code, Error};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::{base, fail, merge, render, require, require_post};
use crate::AppState;

pub(super) fn routes() -> Router<AppState> {
    Router::new()
        .route("/admin/widgets", get(page).post(place))
        .route("/admin/widgets/{id}/edit", post(edit))
        .route("/admin/widgets/{id}/{action}", post(action))
}

#[derive(Debug, Default, Deserialize)]
struct PageQuery {
    #[serde(default)]
    done: String,
    /// 「여기에 놓기」로 왔을 때 미리 고를 영역
    #[serde(default)]
    region: String,
}

/// 놓기·고치기 폼의 값. 실패하면 그대로 다시 채워 보인다.
#[derive(Debug, Default, Deserialize, Serialize)]
struct WidgetForm {
    #[serde(default, rename = "_csrf", skip_serializing)]
    csrf: String,
    #[serde(default)]
    kind: String,
    #[serde(default)]
    region: String,
    #[serde(default)]
    scope: String,
    #[serde(default)]
    title: String,
    #[serde(default)]
    config: String,
}

async fn page(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<PageQuery>,
) -> Response {
    let who = match require(&state, &headers, "/admin/widgets").await {
        Ok(w) => w,
        Err(r) => return *r,
    };
    let notice = match q.done.as_str() {
        "placed" => Some("위젯을 놓았습니다. 사이트에 바로 보입니다."),
        "edited" => Some("위젯을 고쳤습니다."),
        "enable" => Some("위젯을 켰습니다."),
        "disable" => Some("위젯을 껐습니다. 지우지 않았으니 언제든 다시 켤 수 있습니다."),
        "remove" => Some("위젯을 뺐습니다."),
        "up" | "down" => Some("순서를 바꿨습니다."),
        _ => None,
    };
    let form = WidgetForm {
        region: if WIDGET_REGIONS.contains(&q.region.as_str()) {
            q.region
        } else {
            String::new()
        },
        ..WidgetForm::default()
    };
    render_page(&state, &who, StatusCode::OK, notice, None, &form, None).await
}

/// `failed` 는 고치기에 실패한 위젯 id. 그 카드의 고치기 칸을 연 채로 보낸 값을 다시 채운다.
async fn render_page(
    state: &AppState,
    who: &SignedIn,
    status: StatusCode,
    notice: Option<&str>,
    error: Option<String>,
    form: &WidgetForm,
    failed: Option<&str>,
) -> Response {
    let list = match widgets::list(&state.ctx, &who.actor).await {
        Ok(l) => l,
        Err(e) => return fail(state, Some(who), &e).await,
    };
    let types = widgets::all_types(&state.ctx).await;
    let board_list = boards::list(&state.ctx, &who.actor)
        .await
        .unwrap_or_default();
    let page_list: Vec<_> = pages::list(&state.ctx, &who.actor, None)
        .await
        .unwrap_or_default()
        .into_iter()
        .filter(|p| p.status != PageStatus::Deleted)
        .collect();
    let boards_named: Vec<(String, String)> = board_list
        .iter()
        .map(|b| (b.slug.clone(), b.name.clone()))
        .collect();
    let pages_named: Vec<(String, String)> = page_list
        .iter()
        .map(|p| (p.slug.clone(), p.title.clone()))
        .collect();
    let names = Names {
        boards: &boards_named,
        pages: &pages_named,
    };

    let regions: Vec<Value> = WIDGET_REGIONS
        .iter()
        .map(|r| {
            let items: Vec<Value> = list
                .iter()
                .filter(|w| w.region == *r)
                .map(|w| card(w, &types, &names, form, failed))
                .collect();
            let (label, hint) = region_text(r);
            json!({ "id": r, "label": label, "hint": hint, "count": items.len(), "items": items })
        })
        .collect();

    let mut scopes = vec![
        json!({ "id": "all", "label": "모든 화면" }),
        json!({ "id": "home", "label": "첫 화면에서만" }),
        json!({ "id": "boards", "label": "게시판·글 화면에서만" }),
    ];
    scopes.extend(
        boards_named
            .iter()
            .map(|(s, n)| json!({ "id": format!("board:{s}"), "label": format!("게시판 「{n}」에서만"), "group": "board" })),
    );
    scopes.extend(
        pages_named
            .iter()
            .map(|(s, n)| json!({ "id": format!("page:{s}"), "label": format!("페이지 「{n}」에서만"), "group": "page" })),
    );
    let tz = settings::site(&state.ctx)
        .await
        .map(|s| s.tz_offset_min)
        .unwrap_or(540);
    // admin-ui.js 가 설정 칸을 그릴 재료. HTML 안 JSON 이라 `<` 를 이스케이프한다.
    let schema_data = json!({
        "types": types.iter().map(|t| (t.id.to_string(), t.config_schema.clone())).collect::<serde_json::Map<_, _>>(),
        "boards": boards_named.iter().map(|(s, n)| json!({ "id": s, "label": n })).collect::<Vec<_>>(),
        "tz": tz,
    });
    let ctx = merge(
        base(state, Some(who), "widgets").await,
        json!({
            "notice": notice,
            "error": error,
            "regions": regions,
            "total": list.len(),
            "types": types.iter().map(|t| json!({ "id": t.id, "name": t.name, "description": t.description, "icon": kind_icon(t.id) })).collect::<Vec<_>>(),
            "region_choices": WIDGET_REGIONS.iter().map(|r| json!({ "id": r, "label": region_text(r).0 })).collect::<Vec<_>>(),
            "scopes": scopes,
            "form": form,
            "place_failed": failed.is_none() && error.is_some(),
            "schema_json": script_json(&schema_data),
        }),
    );
    render(state, status, "widgets.html", &ctx)
}

struct Names<'a> {
    boards: &'a [(String, String)],
    pages: &'a [(String, String)],
}

fn card(
    w: &Widget,
    types: &[WidgetType],
    names: &Names<'_>,
    form: &WidgetForm,
    failed: Option<&str>,
) -> Value {
    let ty = types.iter().find(|t| t.id == w.kind);
    let reopen = failed == Some(w.id.as_str());
    json!({
        "id": w.id,
        "kind": w.kind,
        "kind_name": ty.map_or(w.kind.as_str(), |t| t.name),
        "icon": kind_icon(&w.kind),
        // 모듈이 꺼졌거나 지워지면 공개 화면에서 조용히 빠진다 (SPEC §7.3).
        "orphan": ty.is_none(),
        "title": w.title,
        "scope": w.scope,
        "where": scope_text(&w.scope, names),
        "enabled": w.enabled,
        "hide_on": w.hide_on,
        "summary": ty.map(|t| summary(&w.config, &t.config_schema, names)).unwrap_or_default(),
        "open": reopen,
        "edit": if reopen {
            json!({ "title": form.title, "scope": form.scope, "config": form.config })
        } else {
            json!({
                "title": w.title,
                "scope": w.scope,
                "config": serde_json::to_string_pretty(&w.config).unwrap_or_default(),
            })
        },
    })
}

/// 영역 이름과, 사이트에서 어디쯤인지 한 줄.
pub(super) fn region_text(region: &str) -> (&'static str, &'static str) {
    match region {
        "header" => ("머리글 아래", "메뉴 바로 밑을 가로지르는 띠"),
        "hero" => ("큰 배너 자리", "첫 화면 맨 위 넓은 자리"),
        "main" => ("본문", "가운데 큰 칸. 첫 화면은 여기에 쌓입니다"),
        "sidebar" => ("오른쪽 곁", "본문 옆 좁은 칸"),
        "popup" => ("팝업", "화면 위에 뜨는 알림 창"),
        "masthead" => ("사이트 이름 옆", "머리글 오른쪽 작은 자리"),
        "sponsor" => ("바닥글 위 띠", "후원·제휴 로고 자리, 접을 수 있음"),
        "rail" => ("넓은 화면 양옆", "큰 모니터에서만 보이는 바깥 여백"),
        "sticky" => ("휴대폰 아래 고정", "휴대폰 화면 맨 아래에 붙는 띠"),
        _ => ("바닥글", "맨 아래 회사 정보·링크 자리"),
    }
}

/// 범위 값을 사람 말로.
fn scope_text(scope: &str, names: &Names<'_>) -> String {
    let find = |list: &[(String, String)], slug: &str| {
        list.iter()
            .find(|(s, _)| s == slug)
            .map_or_else(|| slug.to_owned(), |(_, n)| n.clone())
    };
    match scope {
        "all" => "모든 화면".into(),
        "home" => "첫 화면".into(),
        "boards" => "게시판·글 화면".into(),
        s => match s.split_once(':') {
            Some(("board", slug)) => format!("게시판 「{}」", find(names.boards, slug)),
            Some(("page", slug)) => format!("페이지 「{}」", find(names.pages, slug)),
            _ => s.to_owned(),
        },
    }
}

pub(super) fn kind_icon(kind: &str) -> &'static str {
    match kind {
        "latest_posts" | "popular_posts" | "post_tabs" => "posts",
        "gallery_grid" | "image_banner" | "image_text" | "hero" | "video" | "gallery" | "logos" => {
            "image"
        }
        "menu" => "menus",
        "search" => "eye",
        "account_box" | "team" => "members",
        "stats" => "stats",
        "popup" | "notice_bar" | "countdown" => "bell",
        "social" => "menus",
        "markdown_block" | "faq" | "testimonials" | "quote" => "comments",
        _ => "widgets",
    }
}

/// 설정 칸 이름을 사람 말로. 모르는 칸은 schema 설명이나 이름 그대로 보인다.
/// admin-ui.js 의 이름표와 같은 말을 쓴다.
pub(super) fn key_label(key: &str) -> Option<&'static str> {
    Some(match key {
        "board" => "게시판",
        "limit" => "보일 개수",
        "view" | "style" => "모양",
        "days" => "기간",
        "pages" => "넘겨 볼 쪽",
        "auto" => "저절로 넘기기",
        "show_date" => "날짜 보이기",
        "columns" => "한 줄 칸 수",
        "location" => "메뉴",
        "text" => "글",
        "title" => "제목",
        "body" => "내용",
        "link" => "링크",
        "tone" => "바탕",
        "items" | "rows" | "groups" | "nodes" => "항목",
        "slides" => "슬라이드",
        "tabs" => "탭",
        "plans" => "요금제",
        "links" => "바로가기",
        "position" => "위치",
        "starts_at" => "시작",
        "ends_at" => "끝",
        "address" => "주소",
        "phone" => "전화",
        "intro" => "소개 글",
        "placeholder" => "검색 칸 안내 글",
        _ => return None,
    })
}

/// 설정을 한 줄 요약으로: 「게시판 공지사항 · 5개 · 제목 줄」. 값은 사용자·에이전트가 넣은 데이터다.
fn summary(config: &Value, schema: &Value, names: &Names<'_>) -> Vec<String> {
    let Some(obj) = config.as_object() else {
        return Vec::new();
    };
    let props = &schema["properties"];
    let mut out = Vec::new();
    for (key, value) in obj {
        let label = key_label(key).unwrap_or(key.as_str());
        let part = match (key.as_str(), value) {
            ("board", Value::String(s)) if s.is_empty() => "모든 게시판".to_owned(),
            ("board", Value::String(s)) => {
                let name = names
                    .boards
                    .iter()
                    .find(|(slug, _)| slug == s)
                    .map_or(s.as_str(), |(_, n)| n.as_str());
                format!("게시판 {name}")
            }
            ("limit", Value::Number(n)) => format!("{n}개"),
            ("days", Value::Number(n)) if n.as_u64() == Some(0) => "전체 기간".to_owned(),
            ("days", Value::Number(n)) => format!("최근 {n}일"),
            ("pages", Value::Number(n)) if n.as_u64() == Some(1) => continue,
            ("pages", Value::Number(n)) => format!("{n}쪽 넘겨 보기"),
            ("columns", Value::Number(n)) => format!("한 줄에 {n}칸"),
            ("tone", Value::String(s)) => match s.as_str() {
                "soft" => "옅은 바탕".to_owned(),
                "strong" => "진한 바탕".to_owned(),
                _ => continue,
            },
            (_, Value::Bool(true)) => label.to_owned(),
            (_, Value::Bool(false)) => continue,
            (_, Value::String(s)) if s.trim().is_empty() => continue,
            (_, Value::String(s)) => match enum_label(&props[key]["description"], s) {
                Some(l) => l,
                None => {
                    let short: String = s.chars().take(24).collect();
                    let more = if s.chars().count() > 24 { "…" } else { "" };
                    format!("{label} 「{short}{more}」")
                }
            },
            (_, Value::Array(a)) if a.is_empty() => continue,
            (_, Value::Array(a)) => format!("{label} {}개", a.len()),
            (_, Value::Number(n)) => format!("{label} {n}"),
            _ => continue,
        };
        out.push(part);
        if out.len() == 5 {
            break;
        }
    }
    out
}

/// schema 설명의 「`list`(제목 줄)」 꼴에서 값의 이름을 찾는다.
fn enum_label(description: &Value, value: &str) -> Option<String> {
    let d = description.as_str()?;
    let at = d.find(&format!("`{value}`("))?;
    let rest = &d[at + value.len() + 3..];
    let end = rest.find(')')?;
    Some(rest[..end].to_owned())
}

/// `<script type="application/json">` 안에 넣을 JSON. `</script>` 로 빠져나가지 못하게 한다.
pub(super) fn script_json(v: &Value) -> String {
    v.to_string()
        .replace('<', "\\u003c")
        .replace('>', "\\u003e")
        .replace('&', "\\u0026")
}

fn parse_config(text: &str) -> giwa_core::Result<Value> {
    if text.trim().is_empty() {
        return Ok(Value::Null);
    }
    serde_json::from_str::<Value>(text).map_err(|e| {
        Error::new(Code::Validation)
            .with_message("error.json_body")
            .arg("detail", e.to_string())
    })
}

fn scope_or_all(scope: &str) -> String {
    if scope.trim().is_empty() {
        "all".into()
    } else {
        scope.trim().to_owned()
    }
}

async fn place(
    State(state): State<AppState>,
    headers: HeaderMap,
    Form(f): Form<WidgetForm>,
) -> Response {
    let who = match require_post(&state, &headers, &f.csrf).await {
        Ok(w) => w,
        Err(r) => return *r,
    };
    let placed = match parse_config(&f.config) {
        Ok(config) => {
            widgets::place(
                &state.ctx,
                &who.actor,
                NewWidget {
                    kind: f.kind.clone(),
                    region: f.region.clone(),
                    scope: scope_or_all(&f.scope),
                    title: f.title.clone(),
                    config,
                    position: None,
                    enabled: true,
                    hide_on: Vec::new(),
                },
            )
            .await
        }
        Err(e) => Err(e),
    };
    match placed {
        Ok(_) => Redirect::to("/admin/widgets?done=placed").into_response(),
        Err(e) if e.code() == Code::Validation || e.code() == Code::NotFound => {
            let status = StatusCode::UNPROCESSABLE_ENTITY;
            render_page(&state, &who, status, None, Some(e.message()), &f, None).await
        }
        Err(e) => fail(&state, Some(&who), &e).await,
    }
}

/// 놓인 위젯의 제목·보일 곳·설정을 고친다.
async fn edit(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Form(f): Form<WidgetForm>,
) -> Response {
    let who = match require_post(&state, &headers, &f.csrf).await {
        Ok(w) => w,
        Err(r) => return *r,
    };
    let done = match parse_config(&f.config) {
        Ok(config) => {
            let patch = WidgetPatch {
                scope: Some(scope_or_all(&f.scope)),
                title: Some(f.title.clone()),
                config: Some(config),
                ..WidgetPatch::default()
            };
            widgets::configure(&state.ctx, &who.actor, &id, patch).await
        }
        Err(e) => Err(e),
    };
    match done {
        Ok(_) => Redirect::to("/admin/widgets?done=edited").into_response(),
        Err(e) if e.code() == Code::Validation => {
            let status = StatusCode::UNPROCESSABLE_ENTITY;
            render_page(&state, &who, status, None, Some(e.message()), &f, Some(&id)).await
        }
        Err(e) => fail(&state, Some(&who), &e).await,
    }
}

#[derive(Debug, Default, Deserialize)]
struct ActionForm {
    #[serde(default, rename = "_csrf")]
    csrf: String,
}

async fn action(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((id, action)): Path<(String, String)>,
    Form(f): Form<ActionForm>,
) -> Response {
    let who = match require_post(&state, &headers, &f.csrf).await {
        Ok(w) => w,
        Err(r) => return *r,
    };
    let ctx = &state.ctx;
    let done = match action.as_str() {
        "enable" | "disable" => {
            let patch = WidgetPatch {
                enabled: Some(action == "enable"),
                ..WidgetPatch::default()
            };
            widgets::configure(ctx, &who.actor, &id, patch)
                .await
                .map(|_| ())
        }
        "remove" => widgets::remove(ctx, &who.actor, &id).await.map(|_| ()),
        "up" | "down" => swap(&state, &who, &id, action == "up").await,
        _ => return fail(&state, Some(&who), &Error::new(Code::NotFound)).await,
    };
    match done {
        Ok(()) => {
            Redirect::to(&urls::with_query("/admin/widgets", &[("done", &action)])).into_response()
        }
        Err(e) => fail(&state, Some(&who), &e).await,
    }
}

/// 같은 영역의 바로 앞(뒤) 위젯과 자리를 바꾼다. 순서 값을 0, 1, 2… 로 다시 매긴다.
async fn swap(state: &AppState, who: &SignedIn, id: &str, up: bool) -> giwa_core::Result<()> {
    let all = widgets::list(&state.ctx, &who.actor).await?;
    let me = widgets::get(&state.ctx, &who.actor, id).await?;
    let mut same: Vec<_> = all.into_iter().filter(|w| w.region == me.region).collect();
    let Some(i) = same.iter().position(|w| w.id == me.id) else {
        return Ok(());
    };
    let j = if up { i.checked_sub(1) } else { Some(i + 1) };
    let Some(j) = j.filter(|j| *j < same.len()) else {
        return Ok(());
    };
    same.swap(i, j);
    for (n, w) in same.iter().enumerate() {
        let n = i64::try_from(n).unwrap_or(i64::MAX);
        if w.sort_order != n {
            let patch = WidgetPatch {
                position: Some(n),
                ..WidgetPatch::default()
            };
            widgets::configure(&state.ctx, &who.actor, &w.id, patch).await?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn summary_reads_like_words() {
        let schema = json!({ "properties": { "view": { "description": "`list`(제목 줄), `card`(사진 카드)" } } });
        let boards = vec![("notice".to_owned(), "공지사항".to_owned())];
        let names = Names {
            boards: &boards,
            pages: &[],
        };
        let config = json!({ "board": "notice", "limit": 5, "view": "card", "show_date": true, "auto": false, "pages": 1 });
        let s = summary(&config, &schema, &names);
        assert_eq!(s, ["게시판 공지사항", "5개", "날짜 보이기", "사진 카드"]);
        assert_eq!(scope_text("board:notice", &names), "게시판 「공지사항」");
        assert_eq!(scope_text("home", &names), "첫 화면");
    }

    #[test]
    fn script_json_cannot_close_the_tag() {
        let s = script_json(&json!({ "x": "</script><b>" }));
        assert!(!s.contains('<') && !s.contains('>'));
    }
}
