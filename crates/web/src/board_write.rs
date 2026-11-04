//! 공개 사이트의 게시판 글쓰기·고치기·지우기 (GAP M6, ADR-0053).
//!
//! - 화면은 모두 no-store 이고 진짜 세션으로 렌더한다. 로그인했으면 세션 CSRF, 아니면 로그인 전 CSRF 쿠키.
//! - 손님은 폼 안에서 확인 질문을 푼다(오래 쓰는 동안 풀리지 않게 60분). 함정 칸·같은 출처 확인은 댓글과 같다.
//! - 새 글은 `/write/{board}` — `/b/{board}/{post}` 와 겹치지 않게 게시판 주소 밖에 둔다.

use axum::Json;
use axum::Router;
use axum::body::Bytes;
use axum::extract::{Form, Path, Query, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::middleware;
use axum::response::{Html, IntoResponse, Redirect, Response};
use axum::routing::{get, post};
use maru_core::actor::Permission;
use maru_core::model::{Board, Post, PostStatus};
use maru_core::output::Success;
use maru_core::services::auth::SignedIn;
use maru_core::services::board_posts::{self, BoardEdit, BoardWrite, Editable};
use maru_core::services::comments::GuestAuthor;
use maru_core::services::{boards, captcha, categories, settings};
use maru_core::urls::{self, Target};
use maru_core::{Actor, Code};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::AppState;
use crate::admin::{CSRF_HEADER, pre_csrf, pre_csrf_ok, session_csrf_ok};
use crate::comments::{no_store, same_origin, viewer};
use crate::error::ApiError;
use crate::field_form;

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/write/{board}", get(new_form).post(create))
        .route(
            "/write/{board}/files",
            post(attach).layer(crate::upload_limit()),
        )
        .route("/b/{board}/{post}/edit", get(edit_form).post(edit))
        .route("/b/{board}/{post}/delete", get(delete_form).post(delete))
        .layer(middleware::map_response(no_store))
}

#[derive(Debug, Default, Deserialize)]
struct WriteForm {
    #[serde(default, rename = "_csrf")]
    csrf: String,
    #[serde(default)]
    title: String,
    #[serde(default)]
    body: String,
    #[serde(default)]
    category: String,
    /// 쉼표로 나눈 태그
    #[serde(default)]
    tags: String,
    #[serde(default)]
    secret: Option<String>,
    #[serde(default)]
    name: String,
    #[serde(default)]
    password: String,
    /// 사람에게는 보이지 않는 함정 칸
    #[serde(default)]
    website: String,
    #[serde(default)]
    captcha_token: String,
    #[serde(default)]
    captcha_answer: String,
    /// 답변글이면 위 글 id (ADR-0056)
    #[serde(default)]
    reply_to: String,
    /// 여분 칸 `field.<키>` (ADR-0057)
    #[serde(flatten)]
    extra: std::collections::HashMap<String, String>,
    /// 고치기 화면을 처음 열 때 채울 저장된 값
    #[serde(skip)]
    stored: serde_json::Map<String, Value>,
}

#[derive(Debug, Default, Deserialize)]
struct NewQuery {
    /// 답변을 달 위 글 id
    #[serde(default)]
    reply: Option<String>,
}

impl WriteForm {
    fn tags(&self) -> Vec<String> {
        self.tags
            .split([',', '，'])
            .map(|t| t.trim().trim_start_matches('#').trim().to_owned())
            .filter(|t| !t.is_empty())
            .collect()
    }

    /// 여분 칸 값. 칸을 그리지 않은 폼이면 `None`.
    fn meta(&self, board: &Board) -> Option<serde_json::Map<String, Value>> {
        field_form::collect(&board.field_schema, &self.extra, &field_form::prefix(None))
    }

    fn reply_to(&self) -> Option<String> {
        Some(self.reply_to.trim().to_owned()).filter(|r| !r.is_empty())
    }

    fn category(&self) -> Option<String> {
        Some(self.category.trim().to_owned()).filter(|c| !c.is_empty())
    }

    fn edit(&self) -> BoardEdit {
        BoardEdit {
            title: self.title.clone(),
            body_md: self.body.clone(),
            category: self.category(),
            tags: self.tags(),
            secret: self.secret.is_some(),
            meta: Default::default(),
        }
    }

    fn of(post: &Post, category: Option<String>) -> Self {
        Self {
            title: post.title.clone(),
            body: post.body_md.clone(),
            category: category.unwrap_or_default(),
            tags: post.tags.join(", "),
            secret: post.is_secret.then(String::new),
            ..Default::default()
        }
    }
}

fn csrf_ok(headers: &HeaderMap, who: Option<&SignedIn>, sent: &str) -> bool {
    match who {
        Some(w) => session_csrf_ok(w, sent),
        None => pre_csrf_ok(headers, sent),
    }
}

fn to_login(next: &str) -> Response {
    Redirect::to(&urls::with_query("/login", &[("next", next)])).into_response()
}

fn post_url(board: &Board, post: &Post) -> String {
    urls::path(&Target::Post {
        board: &board.slug,
        post: &post.slug,
    })
}

// ---- 새 글 ----

async fn new_form(
    State(state): State<AppState>,
    Path(slug): Path<String>,
    Query(q): Query<NewQuery>,
    headers: HeaderMap,
) -> Response {
    let (actor, who) = match viewer(&state, &headers).await {
        Ok(v) => v,
        Err(e) => return crate::error::page(&state, &e).await,
    };
    let board = match boards::readable(&state.ctx, &actor, &slug).await {
        Ok(b) => b,
        Err(e) => return crate::error::page(&state, &e).await,
    };
    if let Err(r) = may_write(&state, &actor, who.as_ref(), &board).await {
        return *r;
    }
    let mut f = WriteForm::default();
    if let Some(id) = q.reply.as_deref().filter(|r| !r.is_empty()) {
        match board_posts::reply_target(&state.ctx, &actor, &board, id).await {
            Ok(parent) => {
                f.title = format!("RE: {}", parent.title.trim_start_matches("RE: "));
                f.reply_to = parent.id;
            }
            Err(e) => return crate::error::page(&state, &e).await,
        }
    }
    form(
        &state,
        &headers,
        who.as_ref(),
        &board,
        Mode::New,
        StatusCode::OK,
        &f,
        None,
    )
    .await
}

/// 폼을 열기 전에: 손님이 못 쓰는 게시판이면 로그인으로, 역할이 모자라면 안내.
async fn may_write(
    state: &AppState,
    actor: &Actor,
    who: Option<&SignedIn>,
    board: &Board,
) -> Result<(), Box<Response>> {
    let next = format!("/write/{}", board.slug);
    if who.is_none() {
        if board.policy.write == "guest" {
            return Ok(());
        }
        return Err(Box::new(to_login(&next)));
    }
    let ok = actor.require(Permission::PostWrite).is_ok()
        && actor
            .require_board(Permission::PostWrite, &board.id)
            .is_ok()
        && actor
            .require_rank(Permission::PostWrite, &board.policy.write)
            .is_ok();
    if ok {
        return Ok(());
    }
    let view = json!({
        "view": "notice",
        "heading": "이 게시판에는 글을 쓸 수 없습니다",
        "body": "게시판 관리자가 정한 역할만 글을 씁니다.",
        "links": [{ "label": "게시판으로", "href": urls::path(&Target::Board { board: &board.slug }) }],
    });
    Err(Box::new(
        page(state, StatusCode::FORBIDDEN, "글쓰기", view, who, None).await,
    ))
}

#[derive(Debug, Default, Deserialize)]
struct AttachQuery {
    #[serde(default)]
    filename: String,
}

/// 글쓰기 화면의 「파일 붙이기」 (ADR-0065). 파일 내용을 그대로 본문으로 받아 본문에 넣을 마크다운을 돌려준다.
/// 게시판 역할은 서비스가 본다. POST `/write/` 는 로그인과 같은 좁은 요청 제한을 받는다.
async fn attach(
    State(state): State<AppState>,
    Path(slug): Path<String>,
    Query(q): Query<AttachQuery>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Response, ApiError> {
    let (actor, who) = viewer(&state, &headers).await?;
    let sent = headers
        .get(CSRF_HEADER)
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default();
    if !same_origin(&headers) || !csrf_ok(&headers, who.as_ref(), sent) {
        return Err(ApiError(
            maru_core::Error::new(Code::Forbidden).with_message("error.csrf"),
        ));
    }
    let out = board_posts::attach(&state.ctx, &actor, &slug, q.filename, body.to_vec()).await?;
    Ok((StatusCode::CREATED, Json(Success::new(out))).into_response())
}

async fn create(
    State(state): State<AppState>,
    Path(slug): Path<String>,
    headers: HeaderMap,
    Form(f): Form<WriteForm>,
) -> Response {
    let (actor, who) = match viewer(&state, &headers).await {
        Ok(v) => v,
        Err(e) => return crate::error::page(&state, &e).await,
    };
    if !same_origin(&headers) || !f.website.is_empty() || !csrf_ok(&headers, who.as_ref(), &f.csrf)
    {
        return refused(&state, &headers, who.as_ref()).await;
    }
    let board = match boards::readable(&state.ctx, &actor, &slug).await {
        Ok(b) => b,
        Err(e) => return crate::error::page(&state, &e).await,
    };
    let guest = who.is_none();
    let input = BoardWrite {
        board: board.slug.clone(),
        title: f.title.clone(),
        body_md: f.body.clone(),
        category: f.category(),
        tags: f.tags(),
        secret: f.secret.is_some(),
        guest: guest.then(|| GuestAuthor {
            name: f.name.clone(),
            password: f.password.clone(),
        }),
        captcha: (guest && !f.captcha_token.is_empty()).then(|| captcha::Answer {
            token: f.captcha_token.clone(),
            answer: f.captcha_answer.clone(),
        }),
        reply_to: f.reply_to(),
        meta: f.meta(&board).unwrap_or_default(),
    };
    match board_posts::write(&state.ctx, &actor, input).await {
        Ok(p) if p.status == PostStatus::Published => {
            Redirect::to(&post_url(&board, &p)).into_response()
        }
        Ok(_) => {
            let view = json!({
                "view": "notice",
                "heading": "글을 받았습니다",
                "body": "관리자가 확인한 뒤에 목록에 보입니다.",
                "links": [{ "label": "게시판으로", "href": urls::path(&Target::Board { board: &board.slug }) }],
            });
            page(
                &state,
                StatusCode::OK,
                "글쓰기",
                view,
                who.as_ref(),
                Some(&headers),
            )
            .await
        }
        Err(e) if e.code() == Code::Unauthorized => to_login(&format!("/write/{}", board.slug)),
        Err(e)
            if matches!(
                e.code(),
                Code::Validation | Code::Forbidden | Code::NotFound
            ) =>
        {
            let status =
                StatusCode::from_u16(e.code().http_status()).unwrap_or(StatusCode::BAD_REQUEST);
            form(
                &state,
                &headers,
                who.as_ref(),
                &board,
                Mode::New,
                status,
                &f,
                Some(e.message()),
            )
            .await
        }
        Err(e) => crate::error::page(&state, &e).await,
    }
}

// ---- 고치기·지우기 ----

#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    New,
    Edit,
}

/// 주소의 게시판·글로 고칠 수 있는 글을 찾는다. 로그인이 필요하면 로그인으로.
async fn target(
    state: &AppState,
    actor: &Actor,
    board: &str,
    post: &str,
    mode: &str,
) -> Result<Editable, Box<Response>> {
    match board_posts::editable_at(&state.ctx, actor, board, post).await {
        Ok(e) => Ok(e),
        Err(e) if e.code() == Code::Unauthorized => {
            Err(Box::new(to_login(&format!("/b/{board}/{post}/{mode}"))))
        }
        Err(e) => Err(Box::new(crate::error::page(state, &e).await)),
    }
}

async fn category_slug(state: &AppState, post: &Post) -> Option<String> {
    let id = post.category_id.as_ref()?;
    state
        .ctx
        .store
        .category_by_id(id)
        .await
        .ok()
        .flatten()
        .map(|c| c.slug)
}

async fn edit_form(
    State(state): State<AppState>,
    Path((board, post)): Path<(String, String)>,
    headers: HeaderMap,
) -> Response {
    let (actor, who) = match viewer(&state, &headers).await {
        Ok(v) => v,
        Err(e) => return crate::error::page(&state, &e).await,
    };
    let e = match target(&state, &actor, &board, &post, "edit").await {
        Ok(e) => e,
        Err(r) => return *r,
    };
    let mut f = WriteForm::of(&e.post, category_slug(&state, &e.post).await);
    f.stored = e.post.meta.clone();
    form_for(&state, &headers, who.as_ref(), &e, StatusCode::OK, &f, None).await
}

async fn edit(
    State(state): State<AppState>,
    Path((board, post)): Path<(String, String)>,
    headers: HeaderMap,
    Form(f): Form<WriteForm>,
) -> Response {
    let (actor, who) = match viewer(&state, &headers).await {
        Ok(v) => v,
        Err(e) => return crate::error::page(&state, &e).await,
    };
    if !same_origin(&headers) || !csrf_ok(&headers, who.as_ref(), &f.csrf) {
        return refused(&state, &headers, who.as_ref()).await;
    }
    let e = match target(&state, &actor, &board, &post, "edit").await {
        Ok(e) => e,
        Err(r) => return *r,
    };
    let pw = Some(f.password.as_str()).filter(|p| !p.is_empty());
    let mut input = f.edit();
    input.meta = f.meta(&e.board).unwrap_or_else(|| e.post.meta.clone());
    match board_posts::edit(&state.ctx, &actor, &e.post.id, input, pw).await {
        Ok(p) => Redirect::to(&post_url(&e.board, &p)).into_response(),
        Err(err)
            if matches!(
                err.code(),
                Code::Validation | Code::Forbidden | Code::NotFound
            ) =>
        {
            let status =
                StatusCode::from_u16(err.code().http_status()).unwrap_or(StatusCode::BAD_REQUEST);
            form_for(
                &state,
                &headers,
                who.as_ref(),
                &e,
                status,
                &f,
                Some(err.message()),
            )
            .await
        }
        Err(err) => crate::error::page(&state, &err).await,
    }
}

async fn delete_form(
    State(state): State<AppState>,
    Path((board, post)): Path<(String, String)>,
    headers: HeaderMap,
) -> Response {
    let (actor, who) = match viewer(&state, &headers).await {
        Ok(v) => v,
        Err(e) => return crate::error::page(&state, &e).await,
    };
    let e = match target(&state, &actor, &board, &post, "delete").await {
        Ok(e) => e,
        Err(r) => return *r,
    };
    delete_page(&state, &headers, who.as_ref(), &e, StatusCode::OK, None).await
}

async fn delete_page(
    state: &AppState,
    headers: &HeaderMap,
    who: Option<&SignedIn>,
    e: &Editable,
    status: StatusCode,
    error: Option<String>,
) -> Response {
    let url = post_url(&e.board, &e.post);
    let view = json!({
        "view": "delete",
        "action": format!("{url}/delete"),
        "back": url,
        "post_title": e.post.title,
        "needs_password": e.by_password,
        "error": error,
    });
    page(state, status, "글 지우기", view, who, Some(headers)).await
}

async fn delete(
    State(state): State<AppState>,
    Path((board, post)): Path<(String, String)>,
    headers: HeaderMap,
    Form(f): Form<WriteForm>,
) -> Response {
    let (actor, who) = match viewer(&state, &headers).await {
        Ok(v) => v,
        Err(e) => return crate::error::page(&state, &e).await,
    };
    if !same_origin(&headers) || !csrf_ok(&headers, who.as_ref(), &f.csrf) {
        return refused(&state, &headers, who.as_ref()).await;
    }
    let e = match target(&state, &actor, &board, &post, "delete").await {
        Ok(e) => e,
        Err(r) => return *r,
    };
    let pw = Some(f.password.as_str()).filter(|p| !p.is_empty());
    match board_posts::remove(&state.ctx, &actor, &e.post.id, pw).await {
        Ok(_) => Redirect::to(&urls::path(&Target::Board {
            board: &e.board.slug,
        }))
        .into_response(),
        Err(err) if err.code() == Code::Forbidden => {
            delete_page(
                &state,
                &headers,
                who.as_ref(),
                &e,
                StatusCode::FORBIDDEN,
                Some(err.message()),
            )
            .await
        }
        Err(err) => crate::error::page(&state, &err).await,
    }
}

// ---- 그리기 ----

/// 고치기 폼: 동작 주소는 글 주소, 손님 글이면 비밀번호 칸.
async fn form_for(
    state: &AppState,
    headers: &HeaderMap,
    who: Option<&SignedIn>,
    e: &Editable,
    status: StatusCode,
    f: &WriteForm,
    error: Option<String>,
) -> Response {
    let url = post_url(&e.board, &e.post);
    render_form(
        state,
        headers,
        who,
        &e.board,
        FormView {
            mode: Mode::Edit,
            action: format!("{url}/edit"),
            back: url,
            needs_password: e.by_password,
            guest_post: e.post.guest_name.is_some(),
        },
        status,
        f,
        error,
    )
    .await
}

struct FormView {
    mode: Mode,
    action: String,
    back: String,
    needs_password: bool,
    /// 손님 글은 비밀글로 바꾸지 못한다
    guest_post: bool,
}

#[allow(clippy::too_many_arguments)]
async fn form(
    state: &AppState,
    headers: &HeaderMap,
    who: Option<&SignedIn>,
    board: &Board,
    mode: Mode,
    status: StatusCode,
    f: &WriteForm,
    error: Option<String>,
) -> Response {
    let back = urls::path(&Target::Board { board: &board.slug });
    let fv = FormView {
        mode,
        action: format!("/write/{}", board.slug),
        back,
        needs_password: false,
        guest_post: false,
    };
    render_form(state, headers, who, board, fv, status, f, error).await
}

#[allow(clippy::too_many_arguments)]
async fn render_form(
    state: &AppState,
    headers: &HeaderMap,
    who: Option<&SignedIn>,
    board: &Board,
    fv: FormView,
    status: StatusCode,
    f: &WriteForm,
    error: Option<String>,
) -> Response {
    let site = settings::site(&state.ctx).await.unwrap_or_default();
    let rules = board_posts::rules(board, site.captcha);
    let new_guest = who.is_none() && fv.mode == Mode::New;
    let challenge = if new_guest && rules.captcha {
        match captcha::issue_for(&state.ctx, board_posts::CAPTCHA_MINUTES).await {
            Ok(c) => Some(c),
            Err(e) => return crate::error::page(state, &e).await,
        }
    } else {
        None
    };
    let actor = match viewer(state, headers).await {
        Ok((a, _)) => a,
        Err(e) => return crate::error::page(state, &e).await,
    };
    // 답변은 위 글의 분류를 따르므로 분류 칸을 감춘다.
    let parent = match f.reply_to() {
        Some(id) if fv.mode == Mode::New => state.ctx.store.post_by_id(&id).await.ok().flatten(),
        _ => None,
    };
    let prefix = field_form::prefix(None);
    let field_values = f.meta(board).unwrap_or_else(|| f.stored.clone());
    let fields = field_form::view(&board.field_schema, &field_values, &prefix);
    let cats = categories::list(&state.ctx, &actor, &board.slug)
        .await
        .unwrap_or_default()
        .into_iter()
        .map(|c| json!({ "slug": c.slug, "name": c.name, "selected": c.slug == f.category.trim() }))
        .collect::<Vec<_>>();
    let view = json!({
        "view": "form",
        "mode": if fv.mode == Mode::New { "new" } else { "edit" },
        "action": fv.action,
        "back": fv.back,
        "board": { "name": board.name, "slug": board.slug },
        "guest": new_guest,
        "needs_password": fv.needs_password,
        "can_secret": who.is_some() && !fv.guest_post,
        "review": if who.is_none() { rules.guest_review } else { rules.member_review },
        "categories": if parent.is_some() { Vec::new() } else { cats },
        "fields": fields,
        "fields_shown": field_form::shown_name(&prefix),
        "reply": parent.as_ref().map(|p| json!({ "id": p.id, "title": p.title, "secret": p.is_secret })),
        "title": f.title,
        "body": f.body,
        "category": f.category,
        "tags": f.tags,
        "secret": f.secret.is_some(),
        "name": f.name,
        // 같은 요청의 답으로만 돌려준다 (no-store). 손님이 다시 치지 않게.
        "password": if new_guest { f.password.as_str() } else { "" },
        "captcha": challenge,
        "upload": board_posts::may_upload(&actor, board).then(|| format!("/write/{}/files", board.slug)),
        "error": error,
    });
    let heading = match (fv.mode, parent.is_some()) {
        (Mode::New, true) => "답변 쓰기",
        (Mode::New, false) => "글쓰기",
        _ => "글 고치기",
    };
    page(state, status, heading, view, who, Some(headers)).await
}

async fn refused(state: &AppState, headers: &HeaderMap, who: Option<&SignedIn>) -> Response {
    let view = json!({
        "view": "notice",
        "heading": "요청을 확인하지 못했습니다",
        "body": "다른 곳에서 보낸 요청이거나 화면을 너무 오래 열어 두었습니다. 글쓰기 화면을 새로 열어 다시 보내 주세요.",
        "links": [],
    });
    page(
        state,
        StatusCode::FORBIDDEN,
        "글쓰기",
        view,
        who,
        Some(headers),
    )
    .await
}

/// `write.html` 한 장. 로그인했으면 세션 CSRF, 아니면 로그인 전 CSRF 쿠키를 쓴다.
async fn page(
    state: &AppState,
    status: StatusCode,
    title: &str,
    mut view: Value,
    who: Option<&SignedIn>,
    headers: Option<&HeaderMap>,
) -> Response {
    let mut set = None;
    let csrf = match (who, headers) {
        (Some(w), _) => w.session.csrf_token.clone(),
        (None, Some(h)) => match pre_csrf(state, h) {
            Ok((c, s)) => {
                set = s;
                c
            }
            Err(e) => return crate::error::page(state, &e).await,
        },
        (None, None) => String::new(),
    };
    view["csrf"] = json!(csrf);
    let site = settings::site(&state.ctx).await.unwrap_or_default();
    let ctx = json!({
        "site": crate::public::site_json(state).await,
        "meta": { "title": format!("{title} · {}", site.name), "noindex": true },
        "write": view,
    });
    let mut res = match state.theme.render("write.html", &ctx) {
        Ok(html) => (status, Html(html)).into_response(),
        Err(e) => return crate::error::page(state, &e).await,
    };
    if let Some(set) = set {
        res.headers_mut().append(header::SET_COOKIE, set);
    }
    res
}
