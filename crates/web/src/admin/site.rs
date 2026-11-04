//! 사이트 구조·회원·설정·디자인 관리 화면 (GAP S4·M4, ADR-0064).
//!
//! 서비스·REST·CLI 는 이미 있다. 이 파일은 폼을 서비스 입력으로 바꾸고, 검증에 걸리면
//! 쓴 값을 그대로 다시 채워 오류와 함께 보인다(422). 권한은 서비스가 본다.

use std::collections::HashMap;

use axum::Router;
use axum::extract::{Form, Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Redirect, Response};
use axum::routing::{get, post};
use maru_core::design::{FONTS, Tokens};
use maru_core::fields::FieldDef;
use maru_core::model::{
    Approval, BOARD_KINDS, BOARD_LAYOUTS, BOARD_ORDERS, Board, BoardPolicy, CaptchaMode,
    MENU_LOCATIONS, MemberStatus, MenuItem, MenuLink, PAGE_LAYOUTS, Page, PageReq, PageStatus,
    SignupMode, SiteSettings,
};
use maru_core::services::boards::{self, BoardPatch, NewBoard};
use maru_core::services::pages::{self, NewPage, PagePatch};
use maru_core::services::settings::{self, SettingsPatch};
use maru_core::services::{design, fonts, members, menus};
use maru_core::urls::{self, Target};
use maru_core::{Code, Error};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::{SignedIn, base, encode, fail, merge, render, require, require_post};
use crate::AppState;

pub(super) fn routes() -> Router<AppState> {
    Router::new()
        .route("/admin/boards", get(boards_page).post(board_create))
        .route("/admin/boards/{slug}", get(board_page).post(board_save))
        .route("/admin/boards/{slug}/delete", post(board_delete))
        .route("/admin/pages", get(pages_page).post(page_create))
        .route("/admin/pages/{slug}", get(page_page).post(page_save))
        .route("/admin/pages/{slug}/{action}", post(page_action))
        .route("/admin/menus", get(menus_page))
        .route("/admin/menus/{location}", post(menu_save))
        .route("/admin/members", get(members_page))
        .route("/admin/members/{id}/{action}", post(member_action))
        .route("/admin/settings", get(settings_page).post(settings_save))
        .route("/admin/design", get(design_page).post(design_save))
        .route("/admin/design/reset", post(design_reset))
}

#[derive(Debug, Default, Deserialize)]
struct DoneQuery {
    #[serde(default)]
    done: String,
    #[serde(default)]
    page: Option<u32>,
}

#[derive(Debug, Default, Deserialize)]
struct CsrfOnly {
    #[serde(rename = "_csrf", default)]
    csrf: String,
}

/// 422 로 다시 그릴 오류인가 (값을 고치면 되는 것).
fn fixable(e: &Error) -> bool {
    matches!(e.code(), Code::Validation | Code::Conflict)
}

fn number<T: std::str::FromStr>(field: &str, value: &str) -> Result<T, Error> {
    value.trim().parse::<T>().map_err(|_| {
        Error::new(Code::Validation)
            .with_message("error.form_number")
            .arg("field", field.to_owned())
            .arg("value", value.chars().take(20).collect::<String>())
    })
}

fn choices(items: &[(&str, &str)]) -> Value {
    Value::Array(
        items
            .iter()
            .map(|(id, label)| json!({ "id": id, "label": label }))
            .collect(),
    )
}

// ---- 게시판 ----

fn kind_label(kind: &str) -> &'static str {
    match kind {
        "blog" => "블로그",
        "notice" => "공지",
        "gallery" => "갤러리",
        "free" => "자유 게시판",
        "qna" => "질문·답변",
        "faq" => "자주 묻는 질문",
        "forum" => "포럼",
        _ => "기타",
    }
}

fn layout_label(layout: &str) -> &'static str {
    match layout {
        "list" => "목록",
        "webzine" => "웹진",
        "card" => "카드",
        "gallery" => "갤러리",
        _ => "종류 기본값",
    }
}

fn order_label(order: &str) -> &'static str {
    match order {
        "latest" => "최신 글 순",
        "activity" => "활동 순(새 댓글이 달린 글이 위로)",
        _ => "종류 기본값",
    }
}

const ROLES: [(&str, &str); 5] = [
    ("guest", "손님(누구나)"),
    ("member", "회원"),
    ("editor", "편집자"),
    ("admin", "운영자"),
    ("owner", "소유자"),
];

fn approval_label(a: Approval) -> &'static str {
    match a {
        Approval::Off => "바로 보임",
        Approval::Guest => "비회원 것만 승인 뒤",
        Approval::All => "모두 승인 뒤",
    }
}

/// 게시판 폼의 값. 실패하면 그대로 다시 채운다.
#[derive(Debug, Default, Deserialize, Serialize)]
struct BoardForm {
    #[serde(rename = "_csrf", default, skip_serializing)]
    csrf: String,
    #[serde(default)]
    slug: String,
    #[serde(default)]
    name: String,
    #[serde(default)]
    kind: String,
    #[serde(default)]
    layout: String,
    #[serde(default)]
    list_order: String,
    #[serde(default)]
    sort_order: String,
    #[serde(default)]
    p_list: String,
    #[serde(default)]
    p_read: String,
    #[serde(default)]
    p_write: String,
    #[serde(default)]
    p_comment: String,
    #[serde(default)]
    p_reply: String,
    #[serde(default)]
    p_moderate: String,
    #[serde(default)]
    comment_approval: String,
    #[serde(default)]
    write_approval: String,
    #[serde(default)]
    field_schema: String,
}

impl BoardForm {
    fn of(b: &Board) -> Self {
        let p = &b.policy;
        Self {
            csrf: String::new(),
            slug: b.slug.clone(),
            name: b.name.clone(),
            kind: b.kind.clone(),
            layout: b.layout.clone(),
            list_order: b.list_order.clone(),
            sort_order: b.sort_order.to_string(),
            p_list: p.list.clone(),
            p_read: p.read.clone(),
            p_write: p.write.clone(),
            p_comment: p.comment.clone(),
            p_reply: p.reply.clone(),
            p_moderate: p.moderate.clone(),
            comment_approval: p.comment_approval.as_str().into(),
            write_approval: p.write_approval.as_str().into(),
            field_schema: if b.field_schema.is_empty() {
                String::new()
            } else {
                serde_json::to_string_pretty(&b.field_schema).unwrap_or_default()
            },
        }
    }

    fn patch(&self) -> Result<BoardPatch, Error> {
        let approval = |field: &str, v: &str| {
            Approval::parse(v).ok_or_else(|| {
                Error::new(Code::Validation)
                    .with_message("error.form_choice")
                    .arg("field", field.to_owned())
                    .arg("value", v.chars().take(20).collect::<String>())
            })
        };
        let field_schema: Vec<FieldDef> = if self.field_schema.trim().is_empty() {
            Vec::new()
        } else {
            serde_json::from_str(&self.field_schema).map_err(|e| {
                Error::new(Code::Validation)
                    .with_message("error.json_body")
                    .arg("detail", e.to_string())
            })?
        };
        Ok(BoardPatch {
            name: Some(self.name.clone()),
            kind: Some(self.kind.clone()),
            policy: Some(BoardPolicy {
                list: self.p_list.clone(),
                read: self.p_read.clone(),
                write: self.p_write.clone(),
                moderate: self.p_moderate.clone(),
                comment: self.p_comment.clone(),
                comment_approval: approval("comment_approval", &self.comment_approval)?,
                write_approval: approval("write_approval", &self.write_approval)?,
                reply: self.p_reply.clone(),
            }),
            sort_order: Some(if self.sort_order.trim().is_empty() {
                0
            } else {
                number("sort_order", &self.sort_order)?
            }),
            layout: Some(self.layout.clone()),
            field_schema: Some(field_schema),
            list_order: Some(self.list_order.clone()),
        })
    }
}

fn board_choices() -> Value {
    let roles_none: Vec<(&str, &str)> = ROLES
        .iter()
        .copied()
        .chain([("none", "받지 않음")])
        .collect();
    let approvals: Vec<(&str, &str)> = Approval::ALL
        .iter()
        .map(|a| (a.as_str(), approval_label(*a)))
        .collect();
    let kinds: Vec<(&str, &str)> = BOARD_KINDS.iter().map(|k| (*k, kind_label(k))).collect();
    let layouts: Vec<(&str, &str)> = std::iter::once(("", layout_label("")))
        .chain(BOARD_LAYOUTS.iter().map(|l| (*l, layout_label(l))))
        .collect();
    let orders: Vec<(&str, &str)> = std::iter::once(("", order_label("")))
        .chain(BOARD_ORDERS.iter().map(|o| (*o, order_label(o))))
        .collect();
    json!({
        "kinds": choices(&kinds),
        "layouts": choices(&layouts),
        "orders": choices(&orders),
        "roles": choices(&ROLES),
        "roles_none": choices(&roles_none),
        "approvals": choices(&approvals),
    })
}

async fn boards_page(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<DoneQuery>,
) -> Response {
    let who = match require(&state, &headers, "/admin/boards").await {
        Ok(w) => w,
        Err(r) => return *r,
    };
    let notice = match q.done.as_str() {
        "deleted" => Some("게시판을 지웠습니다."),
        _ => None,
    };
    boards_render(
        &state,
        &who,
        StatusCode::OK,
        notice,
        None,
        &BoardForm {
            kind: "free".into(),
            ..BoardForm::default()
        },
    )
    .await
}

async fn boards_render(
    state: &AppState,
    who: &SignedIn,
    status: StatusCode,
    notice: Option<&str>,
    error: Option<String>,
    form: &BoardForm,
) -> Response {
    let list = match boards::list(&state.ctx, &who.actor).await {
        Ok(l) => l,
        Err(e) => return fail(state, Some(who), &e).await,
    };
    let items = list
        .iter()
        .map(|b| {
            json!({
                "slug": b.slug,
                "name": b.name,
                "kind": kind_label(&b.kind),
                "layout": layout_label(b.effective_layout()),
                "write": role_name(&b.policy.write),
                "fields": b.field_schema.len(),
                "url": urls::path(&Target::Board { board: &b.slug }),
            })
        })
        .collect::<Vec<_>>();
    let ctx = merge(
        base(state, Some(who), "boards").await,
        json!({ "notice": notice, "error": error, "items": items, "form": form, "choices": board_choices() }),
    );
    render(state, status, "boards.html", &ctx)
}

fn role_name(role: &str) -> &'static str {
    ROLES
        .iter()
        .find(|(id, _)| *id == role)
        .map(|(_, l)| *l)
        .unwrap_or("받지 않음")
}

async fn board_create(
    State(state): State<AppState>,
    headers: HeaderMap,
    Form(f): Form<BoardForm>,
) -> Response {
    let who = match require_post(&state, &headers, &f.csrf).await {
        Ok(w) => w,
        Err(r) => return *r,
    };
    let input = NewBoard {
        slug: f.slug.trim().to_owned(),
        name: f.name.clone(),
        kind: f.kind.clone(),
        policy: None,
        sort_order: 0,
        layout: None,
        field_schema: None,
        list_order: None,
    };
    match boards::create(&state.ctx, &who.actor, input).await {
        Ok(b) => {
            Redirect::to(&format!("/admin/boards/{}?done=created", encode(&b.slug))).into_response()
        }
        Err(e) if fixable(&e) => {
            boards_render(
                &state,
                &who,
                StatusCode::UNPROCESSABLE_ENTITY,
                None,
                Some(e.message()),
                &f,
            )
            .await
        }
        Err(e) => fail(&state, Some(&who), &e).await,
    }
}

async fn board_page(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(slug): Path<String>,
    Query(q): Query<DoneQuery>,
) -> Response {
    let who = match require(&state, &headers, &format!("/admin/boards/{slug}")).await {
        Ok(w) => w,
        Err(r) => return *r,
    };
    let board = match boards::show(&state.ctx, &who.actor, &slug).await {
        Ok(b) => b,
        Err(e) => return fail(&state, Some(&who), &e).await,
    };
    let notice = match q.done.as_str() {
        "created" => Some("게시판을 만들었습니다. 이어서 정책·형태를 고르세요."),
        "saved" => Some("저장했습니다."),
        _ => None,
    };
    board_render(
        &state,
        &who,
        StatusCode::OK,
        notice,
        None,
        &BoardForm::of(&board),
    )
    .await
}

async fn board_render(
    state: &AppState,
    who: &SignedIn,
    status: StatusCode,
    notice: Option<&str>,
    error: Option<String>,
    form: &BoardForm,
) -> Response {
    let ctx = merge(
        base(state, Some(who), "boards").await,
        json!({
            "notice": notice,
            "error": error,
            "form": form,
            "choices": board_choices(),
            "url": urls::path(&Target::Board { board: &form.slug }),
        }),
    );
    render(state, status, "board.html", &ctx)
}

async fn board_save(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(slug): Path<String>,
    Form(mut f): Form<BoardForm>,
) -> Response {
    let who = match require_post(&state, &headers, &f.csrf).await {
        Ok(w) => w,
        Err(r) => return *r,
    };
    f.slug = slug.clone();
    let res = match f.patch() {
        Ok(p) => boards::update(&state.ctx, &who.actor, &slug, p).await,
        Err(e) => Err(e),
    };
    match res {
        Ok(b) => {
            Redirect::to(&format!("/admin/boards/{}?done=saved", encode(&b.slug))).into_response()
        }
        Err(e) if fixable(&e) => {
            board_render(
                &state,
                &who,
                StatusCode::UNPROCESSABLE_ENTITY,
                None,
                Some(e.message()),
                &f,
            )
            .await
        }
        Err(e) => fail(&state, Some(&who), &e).await,
    }
}

async fn board_delete(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(slug): Path<String>,
    Form(f): Form<CsrfOnly>,
) -> Response {
    let who = match require_post(&state, &headers, &f.csrf).await {
        Ok(w) => w,
        Err(r) => return *r,
    };
    match boards::delete(&state.ctx, &who.actor, &slug).await {
        Ok(_) => Redirect::to("/admin/boards?done=deleted").into_response(),
        Err(e) => fail(&state, Some(&who), &e).await,
    }
}

// ---- 페이지 ----

fn page_status_label(s: PageStatus) -> &'static str {
    match s {
        PageStatus::Draft => "초안",
        PageStatus::Published => "발행",
        PageStatus::Hidden => "숨김",
        PageStatus::Deleted => "지움",
    }
}

/// 상태마다 누를 수 있는 동작.
fn page_actions(s: PageStatus) -> Vec<(&'static str, &'static str)> {
    match s {
        PageStatus::Draft | PageStatus::Hidden => vec![("publish", "발행"), ("delete", "지우기")],
        PageStatus::Published => vec![("hide", "숨기기"), ("delete", "지우기")],
        PageStatus::Deleted => vec![("restore", "되살리기")],
    }
}

#[derive(Debug, Default, Deserialize, Serialize)]
struct PageForm {
    #[serde(rename = "_csrf", default, skip_serializing)]
    csrf: String,
    #[serde(default)]
    slug: String,
    #[serde(default)]
    title: String,
    #[serde(default)]
    body_md: String,
    #[serde(default)]
    layout: String,
    #[serde(default)]
    sort_order: String,
}

impl PageForm {
    fn of(p: &Page) -> Self {
        Self {
            csrf: String::new(),
            slug: p.slug.clone(),
            title: p.title.clone(),
            body_md: p.body_md.clone(),
            layout: p.layout.clone(),
            sort_order: p.sort_order.to_string(),
        }
    }
}

fn page_json(p: &Page, tz: i32) -> Value {
    json!({
        "slug": p.slug,
        "title": p.title,
        "status": p.status.as_str(),
        "status_label": page_status_label(p.status),
        "updated": maru_core::clock::display_datetime(&p.updated_at, tz),
        "url": urls::path(&Target::Page { page: &p.slug }),
        "actions": page_actions(p.status).into_iter().map(|(id, label)| json!({ "id": id, "label": label })).collect::<Vec<_>>(),
    })
}

async fn tz(state: &AppState) -> i32 {
    settings::site(&state.ctx)
        .await
        .map(|s| s.tz_offset_min)
        .unwrap_or(540)
}

async fn pages_page(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<DoneQuery>,
) -> Response {
    let who = match require(&state, &headers, "/admin/pages").await {
        Ok(w) => w,
        Err(r) => return *r,
    };
    let notice = match q.done.as_str() {
        "publish" => Some("발행했습니다."),
        "hide" => Some("숨겼습니다."),
        "delete" => Some("지웠습니다. 목록에서 되살릴 수 있습니다."),
        "restore" => Some("되살렸습니다."),
        _ => None,
    };
    pages_render(
        &state,
        &who,
        StatusCode::OK,
        notice,
        None,
        &PageForm::default(),
    )
    .await
}

async fn pages_render(
    state: &AppState,
    who: &SignedIn,
    status: StatusCode,
    notice: Option<&str>,
    error: Option<String>,
    form: &PageForm,
) -> Response {
    let list = match pages::list(&state.ctx, &who.actor, None).await {
        Ok(l) => l,
        Err(e) => return fail(state, Some(who), &e).await,
    };
    let tz = tz(state).await;
    let items = list.iter().map(|p| page_json(p, tz)).collect::<Vec<_>>();
    let ctx = merge(
        base(state, Some(who), "pages").await,
        json!({ "notice": notice, "error": error, "items": items, "form": form }),
    );
    render(state, status, "pages.html", &ctx)
}

async fn page_create(
    State(state): State<AppState>,
    headers: HeaderMap,
    Form(f): Form<PageForm>,
) -> Response {
    let who = match require_post(&state, &headers, &f.csrf).await {
        Ok(w) => w,
        Err(r) => return *r,
    };
    let input = NewPage {
        slug: f.slug.trim().to_owned(),
        title: f.title.clone(),
        body_md: String::new(),
        layout: "default".into(),
        sort_order: 0,
    };
    match pages::create(&state.ctx, &who.actor, input).await {
        Ok(p) => {
            Redirect::to(&format!("/admin/pages/{}?done=created", encode(&p.slug))).into_response()
        }
        Err(e) if fixable(&e) => {
            pages_render(
                &state,
                &who,
                StatusCode::UNPROCESSABLE_ENTITY,
                None,
                Some(e.message()),
                &f,
            )
            .await
        }
        Err(e) => fail(&state, Some(&who), &e).await,
    }
}

async fn page_page(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(slug): Path<String>,
    Query(q): Query<DoneQuery>,
) -> Response {
    let who = match require(&state, &headers, &format!("/admin/pages/{slug}")).await {
        Ok(w) => w,
        Err(r) => return *r,
    };
    let page = match pages::get(&state.ctx, &who.actor, &slug).await {
        Ok(p) => p,
        Err(e) => return fail(&state, Some(&who), &e).await,
    };
    let notice = match q.done.as_str() {
        "created" => Some("페이지를 만들었습니다. 본문을 쓰고 발행하세요."),
        "saved" => Some("저장했습니다."),
        _ => None,
    };
    page_render(
        &state,
        &who,
        StatusCode::OK,
        notice,
        None,
        &page,
        &PageForm::of(&page),
    )
    .await
}

async fn page_render(
    state: &AppState,
    who: &SignedIn,
    status: StatusCode,
    notice: Option<&str>,
    error: Option<String>,
    page: &Page,
    form: &PageForm,
) -> Response {
    let tz = tz(state).await;
    let layouts: Vec<(&str, &str)> = PAGE_LAYOUTS
        .iter()
        .map(|l| (*l, if *l == "wide" { "넓게" } else { "보통" }))
        .collect();
    let ctx = merge(
        base(state, Some(who), "pages").await,
        json!({
            "notice": notice,
            "error": error,
            "page": page_json(page, tz),
            "form": form,
            "layouts": choices(&layouts),
        }),
    );
    render(state, status, "page.html", &ctx)
}

async fn page_save(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(slug): Path<String>,
    Form(f): Form<PageForm>,
) -> Response {
    let who = match require_post(&state, &headers, &f.csrf).await {
        Ok(w) => w,
        Err(r) => return *r,
    };
    let patch = match number::<i64>(
        "sort_order",
        if f.sort_order.trim().is_empty() {
            "0"
        } else {
            &f.sort_order
        },
    ) {
        Ok(n) => Ok(PagePatch {
            title: Some(f.title.clone()),
            slug: Some(f.slug.trim().to_owned()),
            body_md: Some(f.body_md.clone()),
            layout: Some(f.layout.clone()),
            sort_order: Some(n),
        }),
        Err(e) => Err(e),
    };
    let res = match patch {
        Ok(p) => pages::update(&state.ctx, &who.actor, &slug, p).await,
        Err(e) => Err(e),
    };
    match res {
        Ok(p) => {
            Redirect::to(&format!("/admin/pages/{}?done=saved", encode(&p.slug))).into_response()
        }
        Err(e) if fixable(&e) => match pages::get(&state.ctx, &who.actor, &slug).await {
            Ok(page) => {
                page_render(
                    &state,
                    &who,
                    StatusCode::UNPROCESSABLE_ENTITY,
                    None,
                    Some(e.message()),
                    &page,
                    &f,
                )
                .await
            }
            Err(e) => fail(&state, Some(&who), &e).await,
        },
        Err(e) => fail(&state, Some(&who), &e).await,
    }
}

async fn page_action(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((slug, action)): Path<(String, String)>,
    Form(f): Form<CsrfOnly>,
) -> Response {
    let who = match require_post(&state, &headers, &f.csrf).await {
        Ok(w) => w,
        Err(r) => return *r,
    };
    let (ctx, actor) = (&state.ctx, &who.actor);
    let res = match action.as_str() {
        "publish" => pages::publish(ctx, actor, &slug).await,
        "hide" => pages::hide(ctx, actor, &slug).await,
        "delete" => pages::delete(ctx, actor, &slug).await,
        "restore" => pages::restore(ctx, actor, &slug).await,
        _ => return fail(&state, Some(&who), &Error::new(Code::NotFound)).await,
    };
    match res {
        Ok(_) => {
            Redirect::to(&urls::with_query("/admin/pages", &[("done", &action)])).into_response()
        }
        Err(e) => fail(&state, Some(&who), &e).await,
    }
}

// ---- 메뉴 ----

/// 메뉴 한 줄. 폼 칸 이름은 `r{번호}_label` 처럼 줄 번호를 붙인다.
#[derive(Debug, Default, Clone, Serialize)]
struct MenuRow {
    label: String,
    kind: String,
    target: String,
    child: bool,
}

const MENU_BLANK_ROWS: usize = 4;
const MENU_MAX_ROWS: usize = 60;

fn menu_rows(items: &[MenuItem]) -> Vec<MenuRow> {
    fn row(item: &MenuItem, child: bool) -> MenuRow {
        let (kind, target) = match &item.link {
            MenuLink::Home => ("home", String::new()),
            MenuLink::Board { slug } => ("board", slug.clone()),
            MenuLink::Page { slug } => ("page", slug.clone()),
            MenuLink::Url { href } => ("url", href.clone()),
        };
        MenuRow {
            label: item.label.clone(),
            kind: kind.into(),
            target,
            child,
        }
    }
    let mut out = Vec::new();
    for item in items {
        out.push(row(item, false));
        for c in &item.children {
            out.push(row(c, true));
        }
    }
    out
}

/// 폼 줄을 메뉴 나무로. 이름이 빈 줄은 뺀다(지우기). 하위 줄은 바로 위 첫 단계 항목에 붙는다.
fn menu_items(rows: &[MenuRow]) -> Result<Vec<MenuItem>, Error> {
    let mut items: Vec<MenuItem> = Vec::new();
    for r in rows.iter().filter(|r| !r.label.trim().is_empty()) {
        let target = r.target.trim().to_owned();
        let link = match r.kind.as_str() {
            "home" => MenuLink::Home,
            "board" => MenuLink::Board { slug: target },
            "page" => MenuLink::Page { slug: target },
            "url" => MenuLink::Url { href: target },
            other => {
                return Err(Error::new(Code::Validation)
                    .with_message("error.form_choice")
                    .arg("field", "kind")
                    .arg("value", other.chars().take(20).collect::<String>()));
            }
        };
        let item = MenuItem {
            label: r.label.trim().to_owned(),
            link,
            children: Vec::new(),
        };
        if r.child {
            match items.last_mut() {
                Some(parent) => parent.children.push(item),
                None => {
                    return Err(Error::new(Code::Validation)
                        .with_message("error.menu_child_first")
                        .arg("label", item.label));
                }
            }
        } else {
            items.push(item);
        }
    }
    Ok(items)
}

fn menu_location_label(location: &str) -> &'static str {
    match location {
        "header" => "머리글 메뉴",
        _ => "바닥글 메뉴",
    }
}

async fn menus_page(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<DoneQuery>,
) -> Response {
    let who = match require(&state, &headers, "/admin/menus").await {
        Ok(w) => w,
        Err(r) => return *r,
    };
    let notice = (q.done == "saved").then_some("메뉴를 저장했습니다.");
    menus_render(&state, &who, StatusCode::OK, notice, None, None).await
}

/// `edited` 가 있으면 그 위치는 보낸 줄을 그대로 다시 보인다.
async fn menus_render(
    state: &AppState,
    who: &SignedIn,
    status: StatusCode,
    notice: Option<&str>,
    error: Option<String>,
    edited: Option<(&str, &[MenuRow])>,
) -> Response {
    let mut locations = Vec::new();
    for loc in MENU_LOCATIONS {
        let mut rows = match edited {
            Some((l, rows)) if l == loc => rows.to_vec(),
            _ => match menus::get(&state.ctx, loc).await {
                Ok(items) => menu_rows(&items),
                Err(e) => return fail(state, Some(who), &e).await,
            },
        };
        rows.retain(|r| !r.label.trim().is_empty());
        rows.extend((0..MENU_BLANK_ROWS).map(|_| MenuRow {
            kind: "url".into(),
            ..MenuRow::default()
        }));
        locations.push(json!({ "id": loc, "label": menu_location_label(loc), "rows": rows }));
    }
    let boards = boards::list(&state.ctx, &who.actor)
        .await
        .unwrap_or_default()
        .into_iter()
        .map(|b| json!({ "slug": b.slug, "name": b.name }))
        .collect::<Vec<_>>();
    let pages = pages::list(&state.ctx, &who.actor, None)
        .await
        .unwrap_or_default()
        .into_iter()
        .filter(|p| p.status != PageStatus::Deleted)
        .map(|p| json!({ "slug": p.slug, "name": p.title }))
        .collect::<Vec<_>>();
    let kinds = choices(&[
        ("home", "첫 화면"),
        ("board", "게시판"),
        ("page", "페이지"),
        ("url", "주소"),
    ]);
    let ctx = merge(
        base(state, Some(who), "menus").await,
        json!({
            "notice": notice,
            "error": error,
            "locations": locations,
            "kinds": kinds,
            "boards": boards,
            "pages": pages,
        }),
    );
    render(state, status, "menus.html", &ctx)
}

async fn menu_save(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(location): Path<String>,
    Form(pairs): Form<Vec<(String, String)>>,
) -> Response {
    let fields: HashMap<String, String> = pairs.into_iter().collect();
    let csrf = fields.get("_csrf").cloned().unwrap_or_default();
    let who = match require_post(&state, &headers, &csrf).await {
        Ok(w) => w,
        Err(r) => return *r,
    };
    if !MENU_LOCATIONS.contains(&location.as_str()) {
        return fail(&state, Some(&who), &Error::new(Code::NotFound)).await;
    }
    let mut rows: Vec<(i64, usize, MenuRow)> = Vec::new();
    for i in 0..MENU_MAX_ROWS {
        let get = |k: &str| fields.get(&format!("r{i}_{k}")).cloned();
        let Some(label) = get("label") else { continue };
        let order = get("order")
            .and_then(|o| o.trim().parse::<i64>().ok())
            .unwrap_or(i64::try_from(i).unwrap_or(0) * 10);
        rows.push((
            order,
            i,
            MenuRow {
                label,
                kind: get("kind").unwrap_or_default(),
                target: get("target").unwrap_or_default(),
                child: get("child").is_some(),
            },
        ));
    }
    rows.sort_by_key(|(order, i, _)| (*order, *i));
    let rows: Vec<MenuRow> = rows.into_iter().map(|(_, _, r)| r).collect();
    let res = match menu_items(&rows) {
        Ok(items) => menus::set(&state.ctx, &who.actor, &location, items).await,
        Err(e) => Err(e),
    };
    match res {
        Ok(_) => Redirect::to("/admin/menus?done=saved").into_response(),
        Err(e) if fixable(&e) => {
            menus_render(
                &state,
                &who,
                StatusCode::UNPROCESSABLE_ENTITY,
                None,
                Some(format!(
                    "{}: {}",
                    menu_location_label(&location),
                    e.message()
                )),
                Some((location.as_str(), &rows)),
            )
            .await
        }
        Err(e) => fail(&state, Some(&who), &e).await,
    }
}

// ---- 회원 ----

const MEMBERS_PER_PAGE: u32 = 50;

fn role_label(role: &str) -> &'static str {
    match role {
        "owner" => "소유자",
        "admin" => "운영자",
        "editor" => "편집자",
        _ => "회원",
    }
}

async fn members_page(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<DoneQuery>,
) -> Response {
    let who = match require(&state, &headers, "/admin/members").await {
        Ok(w) => w,
        Err(r) => return *r,
    };
    let page = q.page.unwrap_or(1).max(1);
    let list =
        match members::list(&state.ctx, &who.actor, PageReq::new(page, MEMBERS_PER_PAGE)).await {
            Ok(l) => l,
            Err(e) => return fail(&state, Some(&who), &e).await,
        };
    let tz = tz(&state).await;
    let items = list
        .items
        .iter()
        .map(|v| {
            let m = &v.member;
            let top = v
                .roles
                .iter()
                .max_by_key(|r| maru_core::actor::rank::of_role(r).unwrap_or(0))
                .cloned()
                .unwrap_or_else(|| "member".into());
            json!({
                "id": m.id,
                "email": m.email,
                "name": m.display_name,
                "role": top,
                "role_label": role_label(&top),
                "owner": top == "owner",
                "me": m.id == who.member.id,
                "active": m.status == MemberStatus::Active,
                "withdrawn": m.withdrawn_at.is_some(),
                "verified": m.email_verified_at.is_some(),
                "joined": maru_core::clock::display_datetime(&m.created_at, tz),
            })
        })
        .collect::<Vec<_>>();
    let per = i64::from(MEMBERS_PER_PAGE);
    let pages = ((list.total + per - 1) / per).max(1);
    let notice = match q.done.as_str() {
        "role" => Some("역할을 바꿨습니다."),
        "disable" => Some("사용을 멈췄습니다. 그 회원의 로그인이 모두 풀렸습니다."),
        "enable" => Some("다시 쓸 수 있게 했습니다."),
        _ => None,
    };
    let ctx = merge(
        base(&state, Some(&who), "members").await,
        json!({
            "notice": notice,
            "items": items,
            "total": list.total,
            "page": page,
            "pages": pages,
            "roles": choices(&[("member", "회원"), ("editor", "편집자"), ("admin", "운영자")]),
        }),
    );
    render(&state, StatusCode::OK, "members.html", &ctx)
}

#[derive(Debug, Default, Deserialize)]
struct MemberForm {
    #[serde(rename = "_csrf", default)]
    csrf: String,
    #[serde(default)]
    role: String,
    #[serde(default)]
    page: Option<u32>,
}

async fn member_action(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((id, action)): Path<(String, String)>,
    Form(f): Form<MemberForm>,
) -> Response {
    let who = match require_post(&state, &headers, &f.csrf).await {
        Ok(w) => w,
        Err(r) => return *r,
    };
    let (ctx, actor) = (&state.ctx, &who.actor);
    let res = match action.as_str() {
        "role" => members::set_role(ctx, actor, &id, &f.role).await,
        "disable" => members::set_status(ctx, actor, &id, MemberStatus::Disabled).await,
        "enable" => members::set_status(ctx, actor, &id, MemberStatus::Active).await,
        _ => return fail(&state, Some(&who), &Error::new(Code::NotFound)).await,
    };
    let page = f.page.unwrap_or(1).max(1).to_string();
    match res {
        Ok(_) => Redirect::to(&urls::with_query(
            "/admin/members",
            &[("page", &page), ("done", &action)],
        ))
        .into_response(),
        Err(e) => fail(&state, Some(&who), &e).await,
    }
}

// ---- 설정 ----

#[derive(Debug, Default, Deserialize, Serialize)]
struct SettingsForm {
    #[serde(rename = "_csrf", default, skip_serializing)]
    csrf: String,
    #[serde(default)]
    name: String,
    #[serde(default)]
    description: String,
    #[serde(default)]
    lang: String,
    #[serde(default)]
    tz_offset_min: String,
    #[serde(default)]
    posts_per_page: String,
    #[serde(default)]
    toc_min_headings: String,
    #[serde(default)]
    show_views: Option<String>,
    #[serde(default)]
    show_search: Option<String>,
    #[serde(default)]
    signup: String,
    #[serde(default)]
    signup_verify: Option<String>,
    #[serde(default)]
    captcha: String,
    #[serde(default)]
    naver_verification: String,
    #[serde(default)]
    google_verification: String,
    #[serde(default)]
    stats_retention_days: String,
}

fn on(b: bool) -> Option<String> {
    b.then(|| "1".to_owned())
}

impl SettingsForm {
    fn of(s: &SiteSettings) -> Self {
        Self {
            csrf: String::new(),
            name: s.name.clone(),
            description: s.description.clone(),
            lang: s.lang.clone(),
            tz_offset_min: s.tz_offset_min.to_string(),
            posts_per_page: s.posts_per_page.to_string(),
            toc_min_headings: s.toc_min_headings.to_string(),
            show_views: on(s.show_views),
            show_search: on(s.show_search),
            signup: s.signup.as_str().into(),
            signup_verify: on(s.signup_verify),
            captcha: s.captcha.as_str().into(),
            naver_verification: s.naver_verification.clone(),
            google_verification: s.google_verification.clone(),
            stats_retention_days: s.stats_retention_days.to_string(),
        }
    }

    /// 지금 값과 다른 칸만 담는다. 회원 관리 권한이 없는 사람도 다른 칸은 저장할 수 있다.
    fn patch(&self, now: &SiteSettings) -> Result<SettingsPatch, Error> {
        fn changed<T: PartialEq>(new: T, old: T) -> Option<T> {
            (new != old).then_some(new)
        }
        Ok(SettingsPatch {
            name: changed(self.name.clone(), now.name.clone()),
            description: changed(self.description.clone(), now.description.clone()),
            lang: changed(self.lang.trim().to_owned(), now.lang.clone()),
            tz_offset_min: changed(
                number("tz_offset_min", &self.tz_offset_min)?,
                now.tz_offset_min,
            ),
            posts_per_page: changed(
                number("posts_per_page", &self.posts_per_page)?,
                now.posts_per_page,
            ),
            naver_verification: changed(
                self.naver_verification.trim().to_owned(),
                now.naver_verification.clone(),
            ),
            google_verification: changed(
                self.google_verification.trim().to_owned(),
                now.google_verification.clone(),
            ),
            toc_min_headings: changed(
                number("toc_min_headings", &self.toc_min_headings)?,
                now.toc_min_headings,
            ),
            show_views: changed(self.show_views.is_some(), now.show_views),
            show_search: changed(self.show_search.is_some(), now.show_search),
            signup: changed(self.signup.clone(), now.signup.as_str().to_owned()),
            signup_verify: changed(self.signup_verify.is_some(), now.signup_verify),
            captcha: changed(self.captcha.clone(), now.captcha.as_str().to_owned()),
            stats_retention_days: changed(
                number("stats_retention_days", &self.stats_retention_days)?,
                now.stats_retention_days,
            ),
        })
    }
}

async fn settings_page(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<DoneQuery>,
) -> Response {
    let who = match require(&state, &headers, "/admin/settings").await {
        Ok(w) => w,
        Err(r) => return *r,
    };
    let now = match settings::site(&state.ctx).await {
        Ok(s) => s,
        Err(e) => return fail(&state, Some(&who), &e).await,
    };
    let notice = (q.done == "saved").then_some("설정을 저장했습니다.");
    settings_render(
        &state,
        &who,
        StatusCode::OK,
        notice,
        None,
        &SettingsForm::of(&now),
    )
    .await
}

async fn settings_render(
    state: &AppState,
    who: &SignedIn,
    status: StatusCode,
    notice: Option<&str>,
    error: Option<String>,
    form: &SettingsForm,
) -> Response {
    let signup: Vec<(&str, &str)> = SignupMode::ALL
        .iter()
        .map(|m| {
            (
                m.as_str(),
                match m {
                    SignupMode::Off => "받지 않음",
                    SignupMode::Open => "누구나",
                },
            )
        })
        .collect();
    let captcha: Vec<(&str, &str)> = CaptchaMode::ALL
        .iter()
        .map(|m| {
            (
                m.as_str(),
                match m {
                    CaptchaMode::Off => "내지 않음",
                    CaptchaMode::Guest => "비회원 댓글·회원 가입",
                },
            )
        })
        .collect();
    let ctx = merge(
        base(state, Some(who), "settings").await,
        json!({
            "notice": notice,
            "error": error,
            "form": form,
            "signup_modes": choices(&signup),
            "captcha_modes": choices(&captcha),
        }),
    );
    render(state, status, "settings.html", &ctx)
}

async fn settings_save(
    State(state): State<AppState>,
    headers: HeaderMap,
    Form(f): Form<SettingsForm>,
) -> Response {
    let who = match require_post(&state, &headers, &f.csrf).await {
        Ok(w) => w,
        Err(r) => return *r,
    };
    let res = match settings::site(&state.ctx).await {
        Ok(now) => match f.patch(&now) {
            Ok(p) => settings::update(&state.ctx, &who.actor, p).await,
            Err(e) => Err(e),
        },
        Err(e) => Err(e),
    };
    match res {
        Ok(_) => Redirect::to("/admin/settings?done=saved").into_response(),
        Err(e) if fixable(&e) => {
            settings_render(
                &state,
                &who,
                StatusCode::UNPROCESSABLE_ENTITY,
                None,
                Some(e.message()),
                &f,
            )
            .await
        }
        Err(e) => fail(&state, Some(&who), &e).await,
    }
}

// ---- 디자인 ----

const COLOR_KEYS: [(&str, &str); 6] = [
    ("bg", "바탕"),
    ("fg", "글자"),
    ("primary", "강조"),
    ("muted", "흐린 글자"),
    ("surface", "면"),
    ("border", "테두리"),
];

/// 디자인 폼의 값. 이름은 토큰 경로의 `.` 을 `__` 로 바꾼 것(`color__bg`).
type DesignForm = HashMap<String, String>;

/// 3자리 색(`#abc`)을 색 고르개가 받는 6자리로.
fn hex6(v: &str) -> String {
    let h = v.trim_start_matches('#');
    if h.len() == 3 {
        let mut out = String::from("#");
        for c in h.chars() {
            out.push(c);
            out.push(c);
        }
        out.to_ascii_lowercase()
    } else {
        v.to_ascii_lowercase()
    }
}

fn design_form(t: &Tokens) -> DesignForm {
    let mut f = DesignForm::new();
    let v = serde_json::to_value(t).unwrap_or(Value::Null);
    let mut put = |path: &str, val: String| {
        f.insert(path.replace('.', "__"), val);
    };
    for mode in ["color", "color_dark"] {
        for (k, _) in COLOR_KEYS {
            put(
                &format!("{mode}.{k}"),
                hex6(v[mode][k].as_str().unwrap_or("#000000")),
            );
        }
    }
    for (group, keys) in [
        ("font", &["body", "heading", "mono"][..]),
        ("type_scale", &["base", "ratio"][..]),
        ("radius", &["sm", "md", "lg"][..]),
        ("space", &["unit", "section"][..]),
        ("layout", &["max_width", "measure", "header", "density"][..]),
        ("motion", &["level"][..]),
    ] {
        for k in keys {
            let s = match &v[group][k] {
                Value::String(s) => s.clone(),
                other => other.to_string(),
            };
            put(&format!("{group}.{k}"), s);
        }
    }
    f
}

/// 폼을 토큰 부분 JSON 으로. 숫자 칸은 숫자로 바꾼다.
fn design_patch(f: &DesignForm) -> Result<Value, Error> {
    let get = |path: &str| {
        f.get(&path.replace('.', "__"))
            .map(|s| s.trim().to_owned())
            .unwrap_or_default()
    };
    let int = |path: &str| number::<i64>(path, &get(path)).map(Value::from);
    let mut colors = serde_json::Map::new();
    for mode in ["color", "color_dark"] {
        let mut p = serde_json::Map::new();
        for (k, _) in COLOR_KEYS {
            p.insert(k.into(), json!(get(&format!("{mode}.{k}"))));
        }
        colors.insert(mode.into(), Value::Object(p));
    }
    let mut out = json!({
        "font": { "body": get("font.body"), "heading": get("font.heading"), "mono": get("font.mono") },
        "type_scale": { "base": int("type_scale.base")?, "ratio": number::<f64>("type_scale.ratio", &get("type_scale.ratio"))? },
        "radius": { "sm": int("radius.sm")?, "md": int("radius.md")?, "lg": int("radius.lg")? },
        "space": { "unit": int("space.unit")?, "section": int("space.section")? },
        "layout": {
            "max_width": int("layout.max_width")?,
            "measure": int("layout.measure")?,
            "header": get("layout.header"),
            "density": get("layout.density"),
        },
        "motion": { "level": get("motion.level") },
    });
    if let Some(o) = out.as_object_mut() {
        o.extend(colors);
    }
    Ok(out)
}

async fn design_page(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<DoneQuery>,
) -> Response {
    let who = match require(&state, &headers, "/admin/design").await {
        Ok(w) => w,
        Err(r) => return *r,
    };
    let tokens = match design::tokens(&state.ctx).await {
        Ok(t) => t,
        Err(e) => return fail(&state, Some(&who), &e).await,
    };
    let notice = match q.done.as_str() {
        "saved" => Some("디자인을 저장했습니다. 사이트에 바로 반영됩니다."),
        "reset" => Some("기본값으로 되돌렸습니다."),
        _ => None,
    };
    design_render(
        &state,
        &who,
        StatusCode::OK,
        notice,
        None,
        &design_form(&tokens),
    )
    .await
}

async fn design_render(
    state: &AppState,
    who: &SignedIn,
    status: StatusCode,
    notice: Option<&str>,
    error: Option<String>,
    form: &DesignForm,
) -> Response {
    let mut font_choices: Vec<Value> = FONTS
        .iter()
        .map(|(n, _)| json!({ "id": n, "label": n }))
        .collect();
    if let Ok(list) = fonts::list(&state.ctx).await {
        font_choices.extend(
            list.iter()
                .map(|f| json!({ "id": f.slug, "label": format!("{} (올린 서체)", f.name) })),
        );
    }
    let ctx = merge(
        base(state, Some(who), "design").await,
        json!({
            "notice": notice,
            "error": error,
            "form": form,
            "colors": choices(&COLOR_KEYS),
            "fonts": font_choices,
            "headers": choices(&[("sticky", "화면 위에 붙어 따라옴"), ("static", "처음 자리에 그대로")]),
            "densities": choices(&[("compact", "촘촘하게"), ("comfortable", "보통"), ("spacious", "넉넉하게")]),
            "motions": choices(&[("none", "움직임 없음"), ("subtle", "은은하게"), ("normal", "보통")]),
        }),
    );
    render(state, status, "design.html", &ctx)
}

async fn design_save(
    State(state): State<AppState>,
    headers: HeaderMap,
    Form(f): Form<DesignForm>,
) -> Response {
    let csrf = f.get("_csrf").cloned().unwrap_or_default();
    let who = match require_post(&state, &headers, &csrf).await {
        Ok(w) => w,
        Err(r) => return *r,
    };
    let res = match design_patch(&f) {
        Ok(patch) => design::update(&state.ctx, &who.actor, patch).await,
        Err(e) => Err(e),
    };
    match res {
        Ok(_) => Redirect::to("/admin/design?done=saved").into_response(),
        Err(e) if fixable(&e) => {
            design_render(
                &state,
                &who,
                StatusCode::UNPROCESSABLE_ENTITY,
                None,
                Some(e.message()),
                &f,
            )
            .await
        }
        Err(e) => fail(&state, Some(&who), &e).await,
    }
}

async fn design_reset(
    State(state): State<AppState>,
    headers: HeaderMap,
    Form(f): Form<CsrfOnly>,
) -> Response {
    let who = match require_post(&state, &headers, &f.csrf).await {
        Ok(w) => w,
        Err(r) => return *r,
    };
    match design::reset(&state.ctx, &who.actor).await {
        Ok(_) => Redirect::to("/admin/design?done=reset").into_response(),
        Err(e) => fail(&state, Some(&who), &e).await,
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    fn row(label: &str, kind: &str, target: &str, child: bool) -> MenuRow {
        MenuRow {
            label: label.into(),
            kind: kind.into(),
            target: target.into(),
            child,
        }
    }

    #[test]
    fn menu_rows_round_trip_with_children() {
        let rows = vec![
            row("처음", "home", "", false),
            row("게시판", "board", "free", false),
            row("공지", "board", "notice", true),
            row("", "url", "", false),
            row("소개", "page", "about", false),
        ];
        let items = menu_items(&rows).unwrap();
        assert_eq!(items.len(), 3);
        assert_eq!(items[1].children.len(), 1);
        assert_eq!(
            items[1].children[0].link,
            MenuLink::Board {
                slug: "notice".into()
            }
        );
        let back = menu_rows(&items);
        assert_eq!(back.len(), 4, "빈 줄은 빠진다");
        assert!(back[2].child);
    }

    #[test]
    fn child_without_parent_is_rejected() {
        let err = menu_items(&[row("공지", "board", "notice", true)]).unwrap_err();
        assert_eq!(err.code(), Code::Validation);
        let err = menu_items(&[row("x", "ftp", "", false)]).unwrap_err();
        assert_eq!(err.code(), Code::Validation);
    }

    #[test]
    fn design_form_round_trips_to_tokens() {
        let t = Tokens::default();
        let f = design_form(&t);
        assert!(f["color__bg"].starts_with('#') && f["color__bg"].len() == 7);
        let patch = design_patch(&f).unwrap();
        let mut merged = serde_json::to_value(&t).unwrap();
        if let (Some(m), Some(p)) = (merged.as_object_mut(), patch.as_object()) {
            for (k, v) in p {
                m.insert(k.clone(), v.clone());
            }
        }
        let back: Tokens = serde_json::from_value(merged).unwrap();
        back.validate().unwrap();
        let mut bad = f.clone();
        bad.insert("radius__sm".into(), "넓게".into());
        assert_eq!(design_patch(&bad).unwrap_err().code(), Code::Validation);
    }

    #[test]
    fn hex_is_expanded_for_color_inputs() {
        assert_eq!(hex6("#AbC"), "#aabbcc");
        assert_eq!(hex6("#112233"), "#112233");
    }
}
