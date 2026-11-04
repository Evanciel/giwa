//! 관리 화면 (`/admin`). 사람 로그인 세션으로만 들어온다.
//!
//! 글 작성·발행은 REST·CLI 와 같은 `maru_core::services` 를 부른다. 이 파일은 폼을 서비스 입력으로
//! 바꾸고 결과를 화면으로 돌려줄 뿐이다.
//!
//! - 세션 쿠키 `maru_session`: `HttpOnly; SameSite=Lax; Path=/`, `site_url` 이 https 면 `Secure`.
//! - 로그인 전 폼(비밀번호, 로그인 링크)은 `maru_csrf` 쿠키와 숨은 필드를 비교한다.
//! - 로그인 뒤 폼은 세션에 묶인 CSRF 값과 비교한다. 둘 다 상수 시간 비교.

use axum::Json;
use axum::body::Bytes;
use axum::extract::rejection::JsonRejection;
use axum::extract::{DefaultBodyLimit, Form, Path, Query, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{Html, IntoResponse, Redirect, Response};
use axum::routing::{get, post};
use axum::{Router, middleware};
use maru_core::actor::{Actor, AuthorType, Permission};
use maru_core::clock;
use maru_core::model::{Board, PageReq, Post, PostStatus};
use maru_core::output::Success;
use maru_core::services::auth::{self, NewSession, SignedIn};
use maru_core::services::posts::{self, NewPost, PostPatch};
use maru_core::services::{boards, categories, design, files, fonts, media, settings, templates};
use maru_core::urls::{self, Target};
use maru_core::{Code, Error};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::AppState;
use crate::error::ApiError;
use crate::field_form;

mod site;

pub(crate) const SESSION_COOKIE: &str = "maru_session";
const CSRF_COOKIE: &str = "maru_csrf";

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/admin", get(home))
        .route("/admin/login", get(login_form).post(login))
        .route("/admin/link/{token}", get(link_form).post(link_use))
        .route("/admin/logout", post(logout))
        .route("/admin/password", get(password_form).post(password))
        .route("/admin/posts/new", get(new_form).post(create))
        .route("/admin/posts/bulk", post(bulk))
        .route("/admin/posts/{id}/revisions", get(revisions_page))
        .route("/admin/posts/{id}/revisions/{rev}/revert", post(revert))
        .route("/admin/posts/{id}/move", post(move_post))
        .route("/admin/posts/{id}/reply", post(reply_post))
        .route("/admin/posts/{id}/lock", post(lock_post))
        .route("/admin/posts/{id}", get(edit_form).post(save))
        .route("/admin/posts/{id}/{action}", post(action))
        .route("/admin/media", post(upload).layer(crate::upload_limit()))
        .route(
            "/admin/files",
            post(upload_file).layer(crate::upload_limit()),
        )
        .route("/admin/comments", get(comments_page))
        .route("/admin/comments/{id}/{action}", post(comment_action))
        .route("/admin/fonts", get(fonts_page))
        .route(
            "/admin/fonts/upload",
            post(upload_font).layer(DefaultBodyLimit::max(crate::FONT_UPLOAD_LIMIT)),
        )
        .route("/admin/fonts/{slug}/license", post(font_license))
        .route("/admin/fonts/{slug}/{action}", post(font_action))
        .route("/admin/templates", get(list_templates).post(save_template))
        .route("/admin/audit", get(audit_page))
        .route("/admin/backups", get(backups_page).post(backup_create))
        .route("/admin/backups/{name}", get(backup_download))
        .route("/admin/backups/{name}/delete", post(backup_delete))
        .route("/admin/stats", get(stats_page))
        .route("/admin/stats.csv", get(stats_csv))
        .route("/admin/widgets", get(widgets_page).post(widget_place))
        .route("/admin/widgets/{id}/{action}", post(widget_action))
        .route("/admin/preview", post(preview))
        .route("/admin/slug-check", get(check_slug))
        .route("/admin/categories", post(create_category))
        .route("/admin/assets/{*path}", get(asset))
        .merge(site::routes())
        .layer(middleware::map_response(admin_headers))
}

/// 관리 화면은 캐시·프레임·리퍼러를 모두 막는다. 로그인 링크가 Referer 로 새지 않게 한다.
async fn admin_headers(mut res: Response) -> Response {
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
             frame-ancestors 'none'; frame-src https://www.youtube-nocookie.com https://player.vimeo.com",
        ),
    );
    res
}

// ---- 쿠키·CSRF ----

pub(crate) fn cookie(headers: &HeaderMap, name: &str) -> Option<String> {
    headers
        .get_all(header::COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(';'))
        .filter_map(|p| p.trim().split_once('='))
        .find(|(k, _)| *k == name)
        .map(|(_, v)| v.to_owned())
}

pub(crate) fn set_cookie(
    state: &AppState,
    name: &str,
    value: &str,
    max_age: Option<i64>,
) -> HeaderValue {
    let mut c = format!("{name}={value}; HttpOnly; SameSite=Lax; Path=/");
    if let Some(age) = max_age {
        c.push_str(&format!("; Max-Age={age}"));
    }
    if state.ctx.site_url.starts_with("https://") {
        c.push_str("; Secure");
    }
    HeaderValue::from_str(&c).unwrap_or_else(|_| HeaderValue::from_static(""))
}

pub(crate) fn is_secret(v: &str) -> bool {
    v.len() == 64 && v.bytes().all(|b| b.is_ascii_hexdigit())
}

/// 로그인 전 폼의 CSRF 값. 쿠키가 없으면 새로 만들어 내보낸다.
pub(crate) fn pre_csrf(
    state: &AppState,
    headers: &HeaderMap,
) -> Result<(String, Option<HeaderValue>), Error> {
    match cookie(headers, CSRF_COOKIE) {
        Some(v) if is_secret(&v) => Ok((v, None)),
        _ => {
            let v = auth::new_secret()?;
            let set = set_cookie(state, CSRF_COOKIE, &v, None);
            Ok((v, Some(set)))
        }
    }
}

pub(crate) fn pre_csrf_ok(headers: &HeaderMap, sent: &str) -> bool {
    cookie(headers, CSRF_COOKIE).is_some_and(|c| is_secret(&c) && auth::same_secret(&c, sent))
}

pub(crate) fn session_csrf_ok(who: &SignedIn, sent: &str) -> bool {
    auth::same_secret(&who.session.csrf_token, sent)
}

/// `next` 는 `/` 로 시작하는 내부 경로만. `//host`, `/\host` 는 외부로 나가므로 거부한다.
pub fn safe_next(next: &str) -> &str {
    let ok = next.starts_with('/')
        && !next.starts_with("//")
        && !next.starts_with("/\\")
        && next.len() <= 512
        && !next.chars().any(|c| c.is_control() || c == '\\');
    if ok { next } else { "/admin" }
}

pub(crate) fn encode(s: &str) -> String {
    s.bytes()
        .map(|b| {
            if b.is_ascii_alphanumeric() || b"-_./~".contains(&b) {
                (b as char).to_string()
            } else {
                format!("%{b:02X}")
            }
        })
        .collect()
}

/// 공개 화면에서 로그인한 사람 (조회수에서 운영자를 빼는 데만 쓴다). 쿠키가 없거나 읽지 못하면 손님.
pub(crate) async fn viewer(state: &AppState, headers: &HeaderMap) -> Option<Actor> {
    match current(state, headers).await {
        Ok(who) => who.map(|w| w.actor),
        Err(e) => {
            tracing::warn!(error = ?e, "세션 확인 실패, 손님으로 봅니다");
            None
        }
    }
}

pub(crate) async fn current(
    state: &AppState,
    headers: &HeaderMap,
) -> Result<Option<SignedIn>, Error> {
    match cookie(headers, SESSION_COOKIE) {
        Some(raw) => auth::signed_in(&state.ctx, &raw).await,
        None => Ok(None),
    }
}

/// 로그인한 사람만. 아니면 로그인 화면으로 보낸다.
async fn require(
    state: &AppState,
    headers: &HeaderMap,
    back: &str,
) -> Result<SignedIn, Box<Response>> {
    match current(state, headers).await {
        Ok(Some(who)) => Ok(who),
        Ok(None) => Err(Box::new(
            Redirect::to(&format!("/admin/login?next={}", encode(back))).into_response(),
        )),
        Err(e) => Err(Box::new(fail(state, None, &e).await)),
    }
}

/// 로그인한 사람의 폼 전송만. CSRF 값이 틀리면 거절 화면.
async fn require_post(
    state: &AppState,
    headers: &HeaderMap,
    csrf: &str,
) -> Result<SignedIn, Box<Response>> {
    let who = match current(state, headers).await {
        Ok(Some(w)) => w,
        Ok(None) => return Err(Box::new(Redirect::to("/admin/login").into_response())),
        Err(e) => return Err(Box::new(fail(state, None, &e).await)),
    };
    if !session_csrf_ok(&who, csrf) {
        return Err(Box::new(csrf_failed(state, Some(&who)).await));
    }
    Ok(who)
}

pub(crate) fn with_session(state: &AppState, new: &NewSession, to: &str) -> Response {
    let mut res = Redirect::to(to).into_response();
    let age = auth::SESSION_DAYS * 24 * 60 * 60;
    res.headers_mut().append(
        header::SET_COOKIE,
        set_cookie(state, SESSION_COOKIE, &new.secret, Some(age)),
    );
    res
}

// ---- 렌더링 ----

async fn base(state: &AppState, who: Option<&SignedIn>, nav: &str) -> Value {
    json!({
        "site": crate::public::site_json(state).await,
        "me": who.map(|w| json!({ "email": w.member.email, "display_name": w.member.display_name })),
        "csrf": who.map(|w| w.session.csrf_token.clone()),
        "nav": nav,
    })
}

fn merge(mut base: Value, extra: Value) -> Value {
    if let (Some(b), Value::Object(e)) = (base.as_object_mut(), extra) {
        b.extend(e);
    }
    base
}

fn render(state: &AppState, status: StatusCode, name: &str, ctx: &Value) -> Response {
    match state.admin.render(name, ctx) {
        Ok(html) => (status, Html(html)).into_response(),
        Err(e) => {
            let id = maru_core::clock::new_id();
            tracing::error!(request_id = %id, error = ?e, "관리 화면 렌더 실패");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("관리 화면을 그리지 못했습니다. 요청 ID: {id}"),
            )
                .into_response()
        }
    }
}

async fn message(
    state: &AppState,
    who: Option<&SignedIn>,
    status: StatusCode,
    heading: &str,
    body: &str,
    command: Option<&str>,
) -> Response {
    let ctx = merge(
        base(state, who, "").await,
        json!({ "heading": heading, "body": body, "command": command }),
    );
    render(state, status, "message.html", &ctx)
}

/// 서비스 오류를 화면으로. `INTERNAL` 은 요청 ID 만 보여 준다.
async fn fail(state: &AppState, who: Option<&SignedIn>, err: &Error) -> Response {
    let status =
        StatusCode::from_u16(err.code().http_status()).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
    if err.code() == Code::Internal {
        let id = maru_core::clock::new_id();
        tracing::error!(request_id = %id, error = ?err, "관리 화면 내부 오류");
        return message(
            state,
            who,
            status,
            "문제가 생겼습니다",
            &format!("잠시 뒤 다시 시도하세요. 요청 ID: {id}"),
            None,
        )
        .await;
    }
    let heading = match err.code() {
        Code::NotFound => "찾을 수 없습니다",
        Code::Forbidden => "권한이 없습니다",
        Code::Unauthorized => "로그인이 필요합니다",
        _ => "처리하지 못했습니다",
    };
    message(state, who, status, heading, &err.message(), None).await
}

async fn csrf_failed(state: &AppState, who: Option<&SignedIn>) -> Response {
    message(
        state,
        who,
        StatusCode::FORBIDDEN,
        "요청을 확인하지 못했습니다",
        "화면이 오래됐거나 다른 곳에서 보낸 요청입니다. 화면을 새로 고친 뒤 다시 시도하세요.",
        None,
    )
    .await
}

async fn asset(State(state): State<AppState>, Path(path): Path<String>) -> Response {
    match state.admin.asset(&path) {
        Some((bytes, mime)) => ([(header::CONTENT_TYPE, mime)], bytes.into_owned()).into_response(),
        None => StatusCode::NOT_FOUND.into_response(),
    }
}

// ---- 로그인 ----

#[derive(Debug, Default, Deserialize)]
struct NextQuery {
    #[serde(default)]
    next: String,
}

async fn login_form(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<NextQuery>,
) -> Response {
    if let Ok(Some(_)) = current(&state, &headers).await {
        return Redirect::to(safe_next(&q.next)).into_response();
    }
    login_page(
        &state,
        &headers,
        StatusCode::OK,
        safe_next(&q.next),
        "",
        None,
    )
    .await
}

async fn login_page(
    state: &AppState,
    headers: &HeaderMap,
    status: StatusCode,
    next: &str,
    email: &str,
    error: Option<String>,
) -> Response {
    let (csrf, set) = match pre_csrf(state, headers) {
        Ok(v) => v,
        Err(e) => return fail(state, None, &e).await,
    };
    let ctx = merge(
        base(state, None, "login").await,
        json!({ "csrf": csrf, "next": next, "email": email, "error": error }),
    );
    let mut res = render(state, status, "login.html", &ctx);
    if let Some(set) = set {
        res.headers_mut().append(header::SET_COOKIE, set);
    }
    res
}

#[derive(Debug, Default, Deserialize)]
struct LoginForm {
    #[serde(default, rename = "_csrf")]
    csrf: String,
    #[serde(default)]
    email: String,
    #[serde(default)]
    password: String,
    #[serde(default)]
    next: String,
}

async fn login(
    State(state): State<AppState>,
    headers: HeaderMap,
    Form(f): Form<LoginForm>,
) -> Response {
    if !pre_csrf_ok(&headers, &f.csrf) {
        return csrf_failed(&state, None).await;
    }
    // argon2 는 무겁다. 동시에 계산하는 수를 제한한다.
    let Ok(_permit) = state.login_gate.acquire().await else {
        return fail(&state, None, &Error::internal("login gate closed")).await;
    };
    match auth::login_password(&state.ctx, &f.email, &f.password).await {
        Ok(new) => with_session(&state, &new, safe_next(&f.next)),
        Err(e) if e.code() == Code::Unauthorized => {
            login_page(
                &state,
                &headers,
                StatusCode::UNAUTHORIZED,
                safe_next(&f.next),
                &f.email,
                Some(e.message()),
            )
            .await
        }
        Err(e) => fail(&state, None, &e).await,
    }
}

async fn link_invalid(state: &AppState) -> Response {
    message(
        state,
        None,
        StatusCode::UNAUTHORIZED,
        "링크를 쓸 수 없습니다",
        "로그인 링크가 만료됐거나 이미 쓰였습니다. 새 링크를 받으세요.",
        Some("maru login-link"),
    )
    .await
}

async fn link_form(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(token): Path<String>,
) -> Response {
    if !is_secret(&token) {
        return link_invalid(&state).await;
    }
    let member = match auth::peek_link(&state.ctx, &token).await {
        Ok(m) => m,
        Err(_) => return link_invalid(&state).await,
    };
    let (csrf, set) = match pre_csrf(&state, &headers) {
        Ok(v) => v,
        Err(e) => return fail(&state, None, &e).await,
    };
    let ctx = merge(
        base(&state, None, "login").await,
        json!({ "csrf": csrf, "email": member.email, "action": format!("/admin/link/{token}") }),
    );
    let mut res = render(&state, StatusCode::OK, "link.html", &ctx);
    if let Some(set) = set {
        res.headers_mut().append(header::SET_COOKIE, set);
    }
    res
}

#[derive(Debug, Default, Deserialize)]
struct CsrfForm {
    #[serde(default, rename = "_csrf")]
    csrf: String,
}

async fn link_use(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(token): Path<String>,
    Form(f): Form<CsrfForm>,
) -> Response {
    if !pre_csrf_ok(&headers, &f.csrf) {
        return csrf_failed(&state, None).await;
    }
    if !is_secret(&token) {
        return link_invalid(&state).await;
    }
    match auth::consume_link(&state.ctx, &token).await {
        Ok(new) => {
            let to = if new.member.has_password() {
                "/admin"
            } else {
                "/admin/password"
            };
            with_session(&state, &new, to)
        }
        Err(e) if e.code() == Code::Unauthorized => link_invalid(&state).await,
        Err(e) => fail(&state, None, &e).await,
    }
}

async fn logout(
    State(state): State<AppState>,
    headers: HeaderMap,
    Form(f): Form<CsrfForm>,
) -> Response {
    let who = match current(&state, &headers).await {
        Ok(Some(w)) => w,
        Ok(None) => return Redirect::to("/admin/login").into_response(),
        Err(e) => return fail(&state, None, &e).await,
    };
    if !session_csrf_ok(&who, &f.csrf) {
        return csrf_failed(&state, Some(&who)).await;
    }
    if let Err(e) = auth::logout(&state.ctx, &who).await {
        return fail(&state, Some(&who), &e).await;
    }
    let mut res = Redirect::to("/admin/login").into_response();
    res.headers_mut().append(
        header::SET_COOKIE,
        set_cookie(&state, SESSION_COOKIE, "", Some(0)),
    );
    res
}

// ---- 비밀번호 ----

async fn password_form(State(state): State<AppState>, headers: HeaderMap) -> Response {
    let who = match require(&state, &headers, "/admin/password").await {
        Ok(w) => w,
        Err(r) => return *r,
    };
    password_page(&state, &who, StatusCode::OK, None).await
}

async fn password_page(
    state: &AppState,
    who: &SignedIn,
    status: StatusCode,
    error: Option<String>,
) -> Response {
    let ctx = merge(
        base(state, Some(who), "password").await,
        json!({
            "has_password": who.member.has_password(),
            "min": auth::MIN_PASSWORD_CHARS,
            "error": error,
        }),
    );
    render(state, status, "password.html", &ctx)
}

#[derive(Debug, Default, Deserialize)]
struct PasswordForm {
    #[serde(default, rename = "_csrf")]
    csrf: String,
    #[serde(default)]
    password: String,
    #[serde(default)]
    confirm: String,
}

async fn password(
    State(state): State<AppState>,
    headers: HeaderMap,
    Form(f): Form<PasswordForm>,
) -> Response {
    let who = match require(&state, &headers, "/admin/password").await {
        Ok(w) => w,
        Err(r) => return *r,
    };
    if !session_csrf_ok(&who, &f.csrf) {
        return csrf_failed(&state, Some(&who)).await;
    }
    if f.password != f.confirm {
        return password_page(
            &state,
            &who,
            StatusCode::BAD_REQUEST,
            Some("두 비밀번호가 다릅니다.".into()),
        )
        .await;
    }
    let Ok(_permit) = state.login_gate.acquire().await else {
        return fail(&state, Some(&who), &Error::internal("login gate closed")).await;
    };
    match auth::set_password(&state.ctx, &who, &f.password).await {
        Ok(new) => with_session(&state, &new, "/admin?done=password"),
        Err(e) if e.code() == Code::Validation => {
            password_page(&state, &who, StatusCode::BAD_REQUEST, Some(e.message())).await
        }
        Err(e) => fail(&state, Some(&who), &e).await,
    }
}

// ---- 글 ----

/// 완료 알림은 정해진 목록에서만 고른다. 쿼리 문자열을 화면에 그대로 비추지 않는다.
#[derive(Debug, Default, Deserialize)]
struct CommentsQuery {
    #[serde(default)]
    status: String,
    #[serde(default)]
    page: Option<u32>,
    #[serde(default)]
    done: String,
}

/// 댓글 관리: 승인 대기부터 (ADR-0052). 본문은 사용자 입력이라 자동 이스케이프로만 보인다.
async fn comments_page(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<CommentsQuery>,
) -> Response {
    use maru_core::model::CommentStatus;
    use maru_core::services::comments;
    let who = match require(&state, &headers, "/admin/comments").await {
        Ok(w) => w,
        Err(r) => return *r,
    };
    let status = match q.status.as_str() {
        "" => Some(CommentStatus::Pending),
        "all" => None,
        s => CommentStatus::parse(s).or(Some(CommentStatus::Pending)),
    };
    let page = PageReq::new(q.page.unwrap_or(1), 50);
    let list = match comments::admin_list(&state.ctx, &who.actor, status, None, page).await {
        Ok(l) => l,
        Err(e) => return fail(&state, Some(&who), &e).await,
    };
    let tz = settings::site(&state.ctx)
        .await
        .map(|s| s.tz_offset_min)
        .unwrap_or(540);
    let mut rows = Vec::with_capacity(list.items.len());
    for c in &list.items {
        let post = state.ctx.store.post_by_id(&c.post_id).await.ok().flatten();
        let board = match &post {
            Some(p) => state
                .ctx
                .store
                .board_by_id(&p.board_id)
                .await
                .ok()
                .flatten(),
            None => None,
        };
        let url = match (&post, &board) {
            (Some(p), Some(b)) if p.is_public() => Some(format!(
                "{}#comment-{}",
                urls::path(&urls::Target::Post {
                    board: &b.slug,
                    post: &p.slug
                }),
                c.id
            )),
            _ => None,
        };
        rows.push(json!({
            "id": c.id,
            "status": c.status,
            "status_label": match c.status {
                CommentStatus::Published => "공개",
                CommentStatus::Pending => "승인 대기",
                CommentStatus::Hidden => "숨김",
                CommentStatus::Spam => "스팸",
                CommentStatus::Deleted => "지움",
            },
            "author_name": c.author_name,
            "is_guest": c.is_guest,
            "body": c.body,
            "date": maru_core::clock::display_date(&c.created_at, tz),
            "post_title": post.as_ref().map(|p| p.title.clone()).unwrap_or_default(),
            "board_name": board.as_ref().map(|b| b.name.clone()).unwrap_or_default(),
            "url": url,
        }));
    }
    let tab = status.map(|s| s.as_str()).unwrap_or("all");
    let pages = (list.total + i64::from(page.per_page) - 1) / i64::from(page.per_page);
    let at = |n: u32| {
        let n = n.to_string();
        urls::with_query("/admin/comments", &[("status", tab), ("page", &n)])
    };
    let ctx = merge(
        base(&state, Some(&who), "comments").await,
        json!({
            "notice": match q.done.as_str() {
                "approve" => Some("보이게 했습니다."),
                "hide" => Some("숨겼습니다."),
                "spam" => Some("스팸으로 두었습니다."),
                "delete" => Some("지웠습니다."),
                _ => None,
            },
            "comments": rows,
            "total": list.total,
            "tab": tab,
            "tabs": [
                { "key": "pending", "label": "승인 대기" },
                { "key": "published", "label": "공개" },
                { "key": "hidden", "label": "숨김" },
                { "key": "spam", "label": "스팸" },
                { "key": "all", "label": "전부" },
            ],
            "prev_url": (page.page > 1).then(|| at(page.page - 1)),
            "next_url": (i64::from(page.page) < pages).then(|| at(page.page + 1)),
        }),
    );
    render(&state, StatusCode::OK, "comments.html", &ctx)
}

#[derive(Debug, Default, Deserialize)]
struct CommentActionForm {
    #[serde(default, rename = "_csrf")]
    csrf: String,
    #[serde(default)]
    tab: String,
}

async fn comment_action(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((id, action)): Path<(String, String)>,
    Form(f): Form<CommentActionForm>,
) -> Response {
    use maru_core::services::comments::{self, Moderation};
    let who = match require(&state, &headers, "/admin/comments").await {
        Ok(w) => w,
        Err(r) => return *r,
    };
    if !session_csrf_ok(&who, &f.csrf) {
        return fail(
            &state,
            Some(&who),
            &Error::new(Code::Forbidden).with_message("error.csrf"),
        )
        .await;
    }
    let Some(m) = Moderation::parse(&action) else {
        return fail(&state, Some(&who), &Error::new(Code::NotFound)).await;
    };
    match comments::moderate(&state.ctx, &who.actor, &id, m).await {
        Ok(_) => {
            let tab = if f.tab.is_empty() {
                "pending"
            } else {
                f.tab.as_str()
            };
            Redirect::to(&urls::with_query(
                "/admin/comments",
                &[("status", tab), ("done", &action)],
            ))
            .into_response()
        }
        Err(e) => fail(&state, Some(&who), &e).await,
    }
}

fn notice(done: &str) -> Option<&'static str> {
    Some(match done {
        "password" => "비밀번호를 저장했습니다. 다른 기기의 로그인은 끊겼습니다.",
        "saved" => "저장했습니다.",
        "published" => "발행했습니다.",
        "scheduled" => "예약했습니다. 그 시각이 되면 서버가 발행합니다.",
        "unscheduled" => "예약을 취소했습니다.",
        "hidden" => "숨겼습니다.",
        "deleted" => "삭제했습니다.",
        "restored" => "되살렸습니다.",
        "reverted" => "리비전으로 되돌렸습니다.",
        "moved" => "다른 게시판으로 옮겼습니다.",
        "copied" => "복사해 초안으로 만들었습니다. 지금 보는 것이 복사본입니다.",
        "replied" => "답변 초안을 만들었습니다. 다듬은 뒤 발행하세요.",
        "locked" => "글을 잠갔습니다. 관리자 말고는 댓글·답변·고치기를 못 합니다.",
        "unlocked" => "글 잠금을 풀었습니다.",
        _ => return None,
    })
}

pub(crate) fn status_label(s: PostStatus) -> &'static str {
    match s {
        PostStatus::Draft => "초안",
        PostStatus::Review => "검토 대기",
        PostStatus::Published => "발행됨",
        PostStatus::Hidden => "숨김",
        PostStatus::Pending => "승인 대기",
        PostStatus::Spam => "스팸",
        PostStatus::Deleted => "삭제됨",
    }
}

#[derive(Debug, Default, Deserialize)]
struct HomeQuery {
    #[serde(default)]
    done: String,
    #[serde(default)]
    page: Option<u32>,
    /// 제목·내용·태그 검색 (초안·비밀글 포함, GAP C5)
    #[serde(default)]
    q: String,
    /// 일괄 동작 결과 (`done=bulk`)
    #[serde(default)]
    ok: Option<usize>,
    #[serde(default)]
    failed: Option<usize>,
}

fn post_row(p: &Post, board_slug: &str, board_name: &str, tz: i32) -> Value {
    let public = p.is_public().then(|| {
        urls::path(&Target::Post {
            board: board_slug,
            post: &p.slug,
        })
    });
    json!({
        "id": p.id,
        "title": p.title,
        "status": p.status,
        "status_label": status_label(p.status),
        "board": board_name,
        "updated": p.updated_at.get(..10).unwrap_or(&p.updated_at),
        "public_url": public,
        "scheduled": p.scheduled_at.as_deref().map(|s| clock::display_datetime(s, tz)),
    })
}

async fn home(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<HomeQuery>,
) -> Response {
    let who = match require(&state, &headers, "/admin").await {
        Ok(w) => w,
        Err(r) => return *r,
    };
    let page = PageReq::new(q.page.unwrap_or(1), 50);
    let search = q.q.trim();
    let mut search_error = None;
    let listing = match posts::list(
        &state.ctx,
        &who.actor,
        None,
        None,
        Some(search).filter(|s| !s.is_empty()),
        page,
    )
    .await
    {
        Ok(l) => l,
        // 잘못된 검색어는 오류 화면 대신 목록 위에 알린다.
        Err(e) if e.code() == Code::Validation && !search.is_empty() => {
            search_error = Some(e.message());
            maru_core::model::Listing {
                items: vec![],
                total: 0,
                page: page.page,
                per_page: page.per_page,
            }
        }
        Err(e) => return fail(&state, Some(&who), &e).await,
    };
    let boards = boards::list(&state.ctx, &who.actor)
        .await
        .unwrap_or_default();
    let tz = settings::site(&state.ctx)
        .await
        .map(|s| s.tz_offset_min)
        .unwrap_or(540);
    let rows: Vec<Value> = listing
        .items
        .iter()
        .map(|p| {
            let b = boards.iter().find(|b| b.id == p.board_id);
            let mut row = post_row(
                p,
                b.map(|b| b.slug.as_str()).unwrap_or_default(),
                b.map(|b| b.name.as_str()).unwrap_or_default(),
                tz,
            );
            // 아직 DB 에 더하지 않은 수까지 (공개 글 화면과 같은 값).
            row["views"] = json!(state.views.shown(p));
            row
        })
        .collect();
    let pages = (listing.total + i64::from(page.per_page) - 1) / i64::from(page.per_page);
    let at = |n: u32| {
        let n = n.to_string();
        urls::with_query(
            "/admin",
            &[("q", search), ("page", if n == "1" { "" } else { &n })],
        )
    };
    let ctx = merge(
        base(&state, Some(&who), "posts").await,
        json!({
            "notice": match (q.done.as_str(), q.ok, q.failed) {
                ("bulk", Some(ok), failed) => Some(match failed.unwrap_or(0) {
                    0 => format!("{ok}개를 처리했습니다."),
                    f => format!("{ok}개를 처리했고 {f}개는 권한이나 상태 때문에 그대로 두었습니다."),
                }),
                _ => notice(&q.done).map(str::to_owned),
            },
            "posts": rows,
            "boards": boards.iter().map(|b| json!({ "slug": b.slug, "name": b.name })).collect::<Vec<_>>(),
            "total": listing.total,
            "page": page.page,
            "pages": pages.max(1),
            "q": search,
            "search_error": search_error,
            "prev_url": (page.page > 1).then(|| at(page.page - 1)),
            "next_url": (i64::from(page.page) < pages).then(|| at(page.page + 1)),
            "needs_password": !who.member.has_password(),
        }),
    );
    render(&state, StatusCode::OK, "home.html", &ctx)
}

#[derive(Debug, Default, Deserialize)]
struct PostForm {
    #[serde(default, rename = "_csrf")]
    csrf: String,
    #[serde(default)]
    board: String,
    #[serde(default)]
    title: String,
    #[serde(default)]
    slug: String,
    #[serde(default)]
    body_md: String,
    #[serde(default)]
    excerpt: String,
    #[serde(default)]
    cover: String,
    /// 브라우저 `datetime-local` 값 (사이트 시간대)
    #[serde(default)]
    scheduled_at: String,
    /// `draft`, `publish`, `schedule`, `unschedule`
    #[serde(default)]
    intent: String,
    /// 분류 슬러그 (빈 값 = 분류 없음). 칸이 없으면 `None` 이고 건드리지 않는다.
    #[serde(default)]
    category: Option<String>,
    /// 쉼표로 이은 태그. 칸이 없으면 `None` 이고 건드리지 않는다.
    #[serde(default)]
    tags: Option<String>,
    /// 체크하면 `on`
    #[serde(default)]
    notice: Option<String>,
    /// 공지 칸을 보여 줬을 때만 `1`. 없으면 공지 여부를 건드리지 않는다(발행 권한 없는 사람).
    #[serde(default)]
    notice_shown: String,
    /// `public`, `secret`, `private`(= 숨김)
    #[serde(default)]
    visibility: String,
    /// 여분 칸 `field.<게시판>.<키>` (ADR-0057)
    #[serde(flatten)]
    extra: std::collections::HashMap<String, String>,
}

impl PostForm {
    /// 이 게시판의 여분 칸 값. 칸 묶음이 오지 않았으면 `None`(건드리지 않음).
    fn meta(&self, board: &Board) -> Option<serde_json::Map<String, Value>> {
        field_form::collect(
            &board.field_schema,
            &self.extra,
            &field_form::prefix(Some(&board.slug)),
        )
    }

    fn tag_list(&self) -> Option<Vec<String>> {
        self.tags.as_deref().map(|t| {
            t.split(',')
                .map(str::trim)
                .filter(|t| !t.is_empty())
                .map(str::to_owned)
                .collect()
        })
    }

    fn notice_value(&self) -> Option<bool> {
        (self.notice_shown == "1").then(|| self.notice.is_some())
    }

    fn secret_value(&self) -> Option<bool> {
        (!self.visibility.is_empty()).then(|| self.visibility == "secret")
    }
}

/// 폼의 예약 의도를 서비스 입력으로. `None` 이면 예약을 건드리지 않는다.
async fn schedule_value(state: &AppState, f: &PostForm) -> Result<Option<String>, Error> {
    match f.intent.as_str() {
        "unschedule" => Ok(Some(String::new())),
        "schedule" => {
            let tz = settings::site(&state.ctx).await?.tz_offset_min;
            maru_core::clock::from_local_input(&f.scheduled_at, tz)
                .map(Some)
                .ok_or_else(|| Error::new(Code::Validation).with_message("error.schedule_pick"))
        }
        _ => Ok(None),
    }
}

async fn edit_page(
    state: &AppState,
    who: &SignedIn,
    status: StatusCode,
    post: Option<&Post>,
    form: Option<&PostForm>,
    notice_text: Option<&str>,
    error: Option<String>,
) -> Response {
    let boards = boards::list(&state.ctx, &who.actor)
        .await
        .unwrap_or_default();
    let tz = settings::site(&state.ctx)
        .await
        .map(|s| s.tz_offset_min)
        .unwrap_or(540);
    let board_slug = post
        .and_then(|p| boards.iter().find(|b| b.id == p.board_id))
        .map(|b| b.slug.clone());
    let public_url = post.filter(|p| p.is_public()).and_then(|p| {
        board_slug.as_deref().map(|b| {
            urls::path(&Target::Post {
                board: b,
                post: &p.slug,
            })
        })
    });
    // 분류: 고치는 글은 그 게시판 것만, 새 글은 게시판을 바꿀 수 있어 전부(스크립트가 게시판별로 거른다).
    let mut cats = Vec::new();
    for b in boards
        .iter()
        .filter(|b| board_slug.as_deref().is_none_or(|s| s == b.slug))
    {
        for c in categories::list(&state.ctx, &who.actor, &b.slug)
            .await
            .unwrap_or_default()
        {
            cats.push((b.slug.clone(), c));
        }
    }
    // 여분 칸 묶음: 분류와 같이 고치는 글은 그 게시판 것만, 새 글은 전부(스크립트가 고른 게시판 것만 남긴다).
    let field_groups = boards
        .iter()
        .filter(|b| board_slug.as_deref().is_none_or(|s| s == b.slug))
        .filter(|b| !b.field_schema.is_empty())
        .map(|b| {
            let prefix = field_form::prefix(Some(&b.slug));
            let values = match (form, post) {
                (Some(f), _) => f.meta(b).unwrap_or_default(),
                (None, Some(p)) => p.meta.clone(),
                (None, None) => serde_json::Map::new(),
            };
            json!({
                "board": b.slug,
                "name": b.name,
                "shown": field_form::shown_name(&prefix),
                "fields": field_form::view(&b.field_schema, &values, &prefix),
            })
        })
        .collect::<Vec<_>>();
    let category_slug = |id: Option<&str>| {
        id.and_then(|id| cats.iter().find(|(_, c)| c.id == id))
            .map(|(_, c)| c.slug.clone())
    };
    // 실패해서 다시 그릴 때는 사용자가 보낸 값을 그대로 돌려준다.
    let values = match (form, post) {
        (Some(f), _) => json!({
            "board": f.board, "title": f.title, "slug": f.slug,
            "body_md": f.body_md, "excerpt": f.excerpt,
            "cover": f.cover, "scheduled_at": f.scheduled_at,
            "category": f.category, "tags": f.tags,
            "notice": f.notice.is_some(), "visibility": f.visibility,
        }),
        (None, Some(p)) => json!({
            "board": board_slug, "title": p.title, "slug": p.slug,
            "body_md": p.body_md, "excerpt": p.excerpt, "cover": p.cover,
            "scheduled_at": p.scheduled_at.as_deref().map(|s| clock::to_local_input(s, tz)),
            "category": category_slug(p.category_id.as_deref()),
            "tags": p.tags.join(", "),
            "notice": p.is_notice,
            "visibility": if p.status == PostStatus::Hidden {
                "private"
            } else if p.is_secret {
                "secret"
            } else {
                "public"
            },
        }),
        (None, None) => json!({ "board": boards.first().map(|b| b.slug.clone()) }),
    };
    let ctx = merge(
        base(
            state,
            Some(who),
            if post.is_some() { "posts" } else { "new" },
        )
        .await,
        json!({
            "boards": boards.iter().map(|b| json!({ "slug": b.slug, "name": b.name, "reply": b.policy.reply != "none" })).collect::<Vec<_>>(),
            "field_groups": field_groups,
            "post": post.map(|p| json!({
                "id": p.id,
                "status": p.status,
                "status_label": status_label(p.status),
                "scheduled": p.scheduled_at.as_deref().map(|s| clock::display_datetime(s, tz)),
                "locked": p.is_locked,
            })),
            "values": values,
            "categories": cats.iter().map(|(b, c)| json!({ "board": b, "slug": c.slug, "name": c.name })).collect::<Vec<_>>(),
            // 칸을 보일지만 정한다. 실제 권한 검사는 서비스가 한다.
            "can_notice": who.actor.has(Permission::PostPublish),
            "can_add_category": who.actor.has(Permission::StructureEdit),
            // 서체 메뉴에 붙일 올린 서체 (ADR-0048). 속성 값으로 넣으므로 템플릿이 HTML 이스케이프한다.
            "fonts_json": Value::Array(
                fonts::list(&state.ctx).await.unwrap_or_default().iter()
                    .map(|f| json!({ "slug": f.slug, "name": f.name }))
                    .collect(),
            ).to_string(),
            "board_slug": board_slug,
            "public_url": public_url,
            "notice": notice_text,
            "error": error,
        }),
    );
    render(state, status, "edit.html", &ctx)
}

async fn new_form(State(state): State<AppState>, headers: HeaderMap) -> Response {
    let who = match require(&state, &headers, "/admin/posts/new").await {
        Ok(w) => w,
        Err(r) => return *r,
    };
    edit_page(&state, &who, StatusCode::OK, None, None, None, None).await
}

fn opt(s: &str) -> Option<String> {
    let s = s.trim();
    (!s.is_empty()).then(|| s.to_owned())
}

async fn create(
    State(state): State<AppState>,
    headers: HeaderMap,
    Form(f): Form<PostForm>,
) -> Response {
    let who = match require(&state, &headers, "/admin/posts/new").await {
        Ok(w) => w,
        Err(r) => return *r,
    };
    if !session_csrf_ok(&who, &f.csrf) {
        return csrf_failed(&state, Some(&who)).await;
    }
    let meta = match boards::show(&state.ctx, &who.actor, &f.board).await {
        Ok(b) => f.meta(&b).unwrap_or_default(),
        Err(_) => serde_json::Map::new(),
    };
    let input = NewPost {
        board: f.board.clone(),
        title: f.title.clone(),
        slug: opt(&f.slug),
        body_md: f.body_md.clone(),
        excerpt: opt(&f.excerpt),
        cover: opt(&f.cover),
        category: f.category.as_deref().and_then(opt),
        tags: f.tag_list().unwrap_or_default(),
        notice: f.notice_value().unwrap_or(false),
        secret: f.secret_value().unwrap_or(false),
        meta,
    };
    let post = match posts::create_draft(&state.ctx, &who.actor, input).await {
        Ok(p) => p,
        Err(e) if matches!(e.code(), Code::Validation | Code::Conflict | Code::NotFound) => {
            return edit_page(
                &state,
                &who,
                StatusCode::BAD_REQUEST,
                None,
                Some(&f),
                None,
                Some(e.message()),
            )
            .await;
        }
        Err(e) => return fail(&state, Some(&who), &e).await,
    };
    finish_save(&state, &who, post, &f).await
}

/// 저장 뒤 `intent=publish` 면 발행, `schedule`·`unschedule` 이면 예약까지. 실패해도 저장은 남는다.
async fn finish_save(state: &AppState, who: &SignedIn, post: Post, f: &PostForm) -> Response {
    // 비공개 = 숨김. 발행·예약보다 먼저 본다: 공개 글은 숨기고, 공개 전 글은 예약을 풀고 저장만 한다.
    if f.visibility == "private" {
        let res = if post.status == PostStatus::Published {
            posts::hide(&state.ctx, &who.actor, &post.id)
                .await
                .map(|p| (p, "hidden"))
        } else if post.scheduled_at.is_some() {
            let patch = PostPatch {
                scheduled_at: Some(String::new()),
                ..Default::default()
            };
            posts::update(&state.ctx, &who.actor, &post.id, patch)
                .await
                .map(|p| (p, "saved"))
        } else {
            Ok((post.clone(), "saved"))
        };
        return match res {
            Ok((p, done)) => {
                Redirect::to(&format!("/admin/posts/{}?done={done}", p.id)).into_response()
            }
            Err(e) if e.code() != Code::Internal => {
                edit_page(
                    state,
                    who,
                    StatusCode::from_u16(e.code().http_status()).unwrap_or(StatusCode::BAD_REQUEST),
                    Some(&post),
                    None,
                    Some("저장했지만 비공개로 바꾸지 못했습니다."),
                    Some(e.message()),
                )
                .await
            }
            Err(e) => fail(state, Some(who), &e).await,
        };
    }
    if matches!(f.intent.as_str(), "schedule" | "unschedule") {
        let res = match schedule_value(state, f).await {
            Ok(at) => {
                let patch = PostPatch {
                    scheduled_at: at,
                    ..Default::default()
                };
                posts::update(&state.ctx, &who.actor, &post.id, patch).await
            }
            Err(e) => Err(e),
        };
        return match res {
            Ok(p) => {
                let done = if p.scheduled_at.is_some() {
                    "scheduled"
                } else {
                    "unscheduled"
                };
                Redirect::to(&format!("/admin/posts/{}?done={done}", p.id)).into_response()
            }
            Err(e) if e.code() != Code::Internal => {
                edit_page(
                    state,
                    who,
                    StatusCode::from_u16(e.code().http_status()).unwrap_or(StatusCode::BAD_REQUEST),
                    Some(&post),
                    None,
                    Some("저장했지만 예약하지 못했습니다."),
                    Some(e.message()),
                )
                .await
            }
            Err(e) => fail(state, Some(who), &e).await,
        };
    }
    if f.intent == "publish" && post.status != PostStatus::Published {
        return match posts::publish(&state.ctx, &who.actor, &post.id).await {
            Ok(p) => Redirect::to(&format!("/admin/posts/{}?done=published", p.id)).into_response(),
            Err(e) if e.code() != Code::Internal => {
                edit_page(
                    state,
                    who,
                    StatusCode::from_u16(e.code().http_status()).unwrap_or(StatusCode::BAD_REQUEST),
                    Some(&post),
                    None,
                    Some("저장했지만 발행하지 못했습니다."),
                    Some(e.message()),
                )
                .await
            }
            Err(e) => fail(state, Some(who), &e).await,
        };
    }
    Redirect::to(&format!("/admin/posts/{}?done=saved", post.id)).into_response()
}

#[derive(Debug, Default, Deserialize)]
struct DoneQuery {
    #[serde(default)]
    done: String,
}

async fn edit_form(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Query(q): Query<DoneQuery>,
) -> Response {
    let who = match require(&state, &headers, &format!("/admin/posts/{id}")).await {
        Ok(w) => w,
        Err(r) => return *r,
    };
    match posts::get(&state.ctx, &who.actor, &id).await {
        Ok(p) => {
            edit_page(
                &state,
                &who,
                StatusCode::OK,
                Some(&p),
                None,
                notice(&q.done),
                None,
            )
            .await
        }
        Err(e) => fail(&state, Some(&who), &e).await,
    }
}

async fn save(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Form(f): Form<PostForm>,
) -> Response {
    let who = match require(&state, &headers, &format!("/admin/posts/{id}")).await {
        Ok(w) => w,
        Err(r) => return *r,
    };
    if !session_csrf_ok(&who, &f.csrf) {
        return csrf_failed(&state, Some(&who)).await;
    }
    let meta = match posts::get(&state.ctx, &who.actor, &id).await {
        Ok(p) => match state.ctx.store.board_by_id(&p.board_id).await {
            Ok(Some(b)) => f.meta(&b),
            _ => None,
        },
        Err(_) => None,
    };
    let patch = PostPatch {
        title: Some(f.title.clone()),
        slug: opt(&f.slug),
        body_md: Some(f.body_md.clone()),
        excerpt: Some(f.excerpt.clone()),
        cover: Some(f.cover.clone()),
        scheduled_at: None,
        category: f.category.clone(),
        tags: f.tag_list(),
        notice: f.notice_value(),
        secret: f.secret_value(),
        meta,
    };
    match posts::update(&state.ctx, &who.actor, &id, patch).await {
        Ok(p) => finish_save(&state, &who, p, &f).await,
        Err(e) if matches!(e.code(), Code::Validation | Code::Conflict) => {
            match posts::get(&state.ctx, &who.actor, &id).await {
                Ok(p) => {
                    edit_page(
                        &state,
                        &who,
                        StatusCode::BAD_REQUEST,
                        Some(&p),
                        Some(&f),
                        None,
                        Some(e.message()),
                    )
                    .await
                }
                Err(e) => fail(&state, Some(&who), &e).await,
            }
        }
        Err(e) => fail(&state, Some(&who), &e).await,
    }
}

async fn action(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((id, action)): Path<(String, String)>,
    Form(f): Form<CsrfForm>,
) -> Response {
    let who = match require(&state, &headers, &format!("/admin/posts/{id}")).await {
        Ok(w) => w,
        Err(r) => return *r,
    };
    if !session_csrf_ok(&who, &f.csrf) {
        return csrf_failed(&state, Some(&who)).await;
    }
    let (res, done) = match action.as_str() {
        "publish" => (
            posts::publish(&state.ctx, &who.actor, &id).await,
            "published",
        ),
        "hide" => (posts::hide(&state.ctx, &who.actor, &id).await, "hidden"),
        "delete" => (posts::delete(&state.ctx, &who.actor, &id).await, "deleted"),
        "restore" => (
            posts::restore(&state.ctx, &who.actor, &id).await,
            "restored",
        ),
        _ => {
            let e = Error::new(Code::NotFound).with_message("error.not_found");
            return fail(&state, Some(&who), &e).await;
        }
    };
    match res {
        Ok(p) if p.status == PostStatus::Deleted => {
            Redirect::to(&format!("/admin?done={done}")).into_response()
        }
        Ok(p) => Redirect::to(&format!("/admin/posts/{}?done={done}", p.id)).into_response(),
        Err(e) => fail(&state, Some(&who), &e).await,
    }
}

// ---- 리비전·이동·일괄 (ADR-0055) ----

#[derive(Debug, Default, Deserialize)]
struct RevisionQuery {
    #[serde(default)]
    rev: Option<String>,
}

async fn revisions_page(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Query(q): Query<RevisionQuery>,
) -> Response {
    let who = match require(&state, &headers, &format!("/admin/posts/{id}/revisions")).await {
        Ok(w) => w,
        Err(r) => return *r,
    };
    let ctx = &state.ctx;
    let (post, mut list) = match async {
        let post = posts::get(ctx, &who.actor, &id).await?;
        let list = posts::revisions(ctx, &who.actor, &id).await?;
        Ok::<_, Error>((post, list))
    }
    .await
    {
        Ok(v) => v,
        Err(e) => return fail(&state, Some(&who), &e).await,
    };
    list.reverse();
    let tz = site_tz(&state).await;
    // 지금 글과 같은 가장 새 리비전이 「지금」. 고르지 않았으면 그 바로 앞 것을 비교한다.
    let current = list
        .iter()
        .position(|r| r.title == post.title && r.body_md == post.body_md);
    let selected = q
        .rev
        .filter(|r| list.iter().any(|x| &x.id == r))
        .or_else(|| {
            let next = current.map_or(0, |c| c + 1);
            list.get(next).or(list.first()).map(|r| r.id.clone())
        });
    let mut names: std::collections::HashMap<String, String> = std::collections::HashMap::new();
    let mut rows = Vec::with_capacity(list.len());
    for (i, r) in list.iter().enumerate() {
        let who_label = match (&r.actor_id, r.actor_type) {
            (Some(mid), _) => {
                if !names.contains_key(mid)
                    && let Ok(Some(m)) = ctx.store.member_by_id(mid).await
                {
                    names.insert(mid.clone(), m.display_name);
                }
                names.get(mid).cloned().unwrap_or_else(|| "회원".into())
            }
            (None, t) => author_label(t).to_owned(),
        };
        rows.push(json!({
            "id": r.id,
            "created_at": r.created_at,
            "date": clock::display_datetime(&r.created_at, tz),
            "who": who_label,
            "current": Some(i) == current,
            "selected": selected.as_deref() == Some(r.id.as_str()),
        }));
    }
    let compare = match &selected {
        Some(rev) => match posts::compare(ctx, &who.actor, &id, rev).await {
            Ok(c) => Some(json!({
                "id": c.revision.id,
                "date": clock::display_datetime(&c.revision.created_at, tz),
                "title": c.revision.title,
                "title_changed": c.title_changed,
                "same": c.body.added == 0 && c.body.removed == 0 && !c.title_changed,
                "added": c.body.added,
                "removed": c.body.removed,
                "coarse": c.body.coarse,
                "lines": c.body.lines,
            })),
            Err(e) => return fail(&state, Some(&who), &e).await,
        },
        None => None,
    };
    let view = merge(
        base(&state, Some(&who), "posts").await,
        json!({
            "post": { "id": post.id, "title": post.title },
            "revisions": rows,
            "compare": compare,
        }),
    );
    render(&state, StatusCode::OK, "revisions.html", &view)
}

async fn revert(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((id, rev)): Path<(String, String)>,
    Form(f): Form<CsrfForm>,
) -> Response {
    let who = match require(&state, &headers, &format!("/admin/posts/{id}/revisions")).await {
        Ok(w) => w,
        Err(r) => return *r,
    };
    if !session_csrf_ok(&who, &f.csrf) {
        return csrf_failed(&state, Some(&who)).await;
    }
    match posts::revert(&state.ctx, &who.actor, &id, &rev).await {
        Ok(p) => Redirect::to(&format!("/admin/posts/{}?done=reverted", p.id)).into_response(),
        Err(e) => fail(&state, Some(&who), &e).await,
    }
}

#[derive(Debug, Default, Deserialize)]
struct MoveForm {
    #[serde(default, rename = "_csrf")]
    csrf: String,
    #[serde(default)]
    board: String,
    /// `move` 또는 `copy`
    #[serde(default)]
    how: String,
}

async fn move_post(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Form(f): Form<MoveForm>,
) -> Response {
    let who = match require(&state, &headers, &format!("/admin/posts/{id}")).await {
        Ok(w) => w,
        Err(r) => return *r,
    };
    if !session_csrf_ok(&who, &f.csrf) {
        return csrf_failed(&state, Some(&who)).await;
    }
    let res = if f.how == "copy" {
        posts::copy_to(
            &state.ctx,
            &who.actor,
            &id,
            posts::CopyInput { board: f.board },
        )
        .await
        .map(|p| (p, "copied"))
    } else {
        let input = posts::MoveInput {
            board: f.board,
            category: None,
        };
        posts::move_to(&state.ctx, &who.actor, &id, input)
            .await
            .map(|p| (p, "moved"))
    };
    match res {
        Ok((p, done)) => {
            Redirect::to(&format!("/admin/posts/{}?done={done}", p.id)).into_response()
        }
        Err(e) => fail(&state, Some(&who), &e).await,
    }
}

#[derive(Debug, Deserialize)]
struct ReplyForm {
    #[serde(default, rename = "_csrf")]
    csrf: String,
    #[serde(default)]
    title: String,
    #[serde(default)]
    body_md: String,
}

/// 답변 초안을 만들고 그 편집 화면으로 간다 (ADR-0056).
async fn reply_post(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Form(f): Form<ReplyForm>,
) -> Response {
    let who = match require(&state, &headers, &format!("/admin/posts/{id}")).await {
        Ok(w) => w,
        Err(r) => return *r,
    };
    if !session_csrf_ok(&who, &f.csrf) {
        return csrf_failed(&state, Some(&who)).await;
    }
    let input = posts::ReplyInput {
        title: f.title,
        body_md: f.body_md,
        slug: None,
        tags: Vec::new(),
        secret: false,
        meta: Default::default(),
    };
    match posts::reply(&state.ctx, &who.actor, &id, input).await {
        Ok(p) => Redirect::to(&format!("/admin/posts/{}?done=replied", p.id)).into_response(),
        Err(e) => fail(&state, Some(&who), &e).await,
    }
}

#[derive(Debug, Default, Deserialize)]
struct LockForm {
    #[serde(default, rename = "_csrf")]
    csrf: String,
    /// `on` 이면 잠그고 그 밖이면 푼다
    #[serde(default)]
    lock: String,
}

/// 글 잠금·풀기 (ADR-0058).
async fn lock_post(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Form(f): Form<LockForm>,
) -> Response {
    let who = match require(&state, &headers, &format!("/admin/posts/{id}")).await {
        Ok(w) => w,
        Err(r) => return *r,
    };
    if !session_csrf_ok(&who, &f.csrf) {
        return csrf_failed(&state, Some(&who)).await;
    }
    let locked = f.lock == "on";
    match posts::lock(&state.ctx, &who.actor, &id, locked).await {
        Ok(p) => {
            let done = if locked { "locked" } else { "unlocked" };
            Redirect::to(&format!("/admin/posts/{}?done={done}", p.id)).into_response()
        }
        Err(e) => fail(&state, Some(&who), &e).await,
    }
}

/// 목록 일괄 동작. 고르기 칸은 `pick.<글 id>` 라 같은 이름이 여러 번 오지 않는다.
async fn bulk(
    State(state): State<AppState>,
    headers: HeaderMap,
    Form(f): Form<std::collections::HashMap<String, String>>,
) -> Response {
    let who = match require(&state, &headers, "/admin").await {
        Ok(w) => w,
        Err(r) => return *r,
    };
    let field = |k: &str| f.get(k).map(String::as_str).unwrap_or_default();
    if !session_csrf_ok(&who, field("_csrf")) {
        return csrf_failed(&state, Some(&who)).await;
    }
    let ids: Vec<String> = f
        .keys()
        .filter_map(|k| k.strip_prefix("pick."))
        .map(str::to_owned)
        .collect();
    let action = match field("action") {
        "publish" => posts::BulkAction::Publish,
        "hide" => posts::BulkAction::Hide,
        "delete" => posts::BulkAction::Delete,
        "restore" => posts::BulkAction::Restore,
        "move" => posts::BulkAction::Move {
            board: field("board").to_owned(),
            category: None,
        },
        other => {
            let e = Error::new(Code::Validation)
                .with_message("error.bulk_action")
                .arg("value", other.to_owned());
            return fail(&state, Some(&who), &e).await;
        }
    };
    match posts::bulk(&state.ctx, &who.actor, posts::BulkInput { ids, action }).await {
        Ok(items) => {
            let ok = items.iter().filter(|i| i.ok).count();
            let failed = items.len() - ok;
            Redirect::to(&format!("/admin?done=bulk&ok={ok}&failed={failed}")).into_response()
        }
        Err(e) => fail(&state, Some(&who), &e).await,
    }
}

async fn site_tz(state: &AppState) -> i32 {
    settings::site(&state.ctx)
        .await
        .map(|s| s.tz_offset_min)
        .unwrap_or(540)
}

fn author_label(t: AuthorType) -> &'static str {
    match t {
        AuthorType::Human => "사람",
        AuthorType::Agent => "에이전트",
        AuthorType::Local => "이 컴퓨터(CLI)",
    }
}

// ---- 서체 (ADR-0048) ----

#[derive(Debug, Default, Deserialize)]
struct FontsQuery {
    #[serde(default)]
    done: Option<String>,
}

async fn fonts_page(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<FontsQuery>,
) -> Response {
    let who = match require(&state, &headers, "/admin/fonts").await {
        Ok(w) => w,
        Err(r) => return *r,
    };
    fonts_render(&state, &who, StatusCode::OK, q.done.as_deref(), None).await
}

async fn fonts_render(
    state: &AppState,
    who: &SignedIn,
    status: StatusCode,
    done: Option<&str>,
    error: Option<String>,
) -> Response {
    let list = match fonts::list(&state.ctx).await {
        Ok(l) => l,
        Err(e) => return fail(state, Some(who), &e).await,
    };
    let tokens = design::tokens(&state.ctx).await.unwrap_or_default();
    let notice = match done {
        Some("uploaded") => Some("서체를 올렸습니다."),
        Some("body") => Some("사이트 본문 서체로 정했습니다."),
        Some("heading") => Some("사이트 제목 서체로 정했습니다."),
        Some("removed") => Some("서체를 지웠습니다. 이 서체를 쓴 글은 기본 서체로 보입니다."),
        Some("file-removed") => Some("파일을 뗐습니다."),
        _ => None,
    };
    let ctx = merge(
        base(state, Some(who), "fonts").await,
        json!({
            "fonts": list.iter().map(|f| json!({
                "slug": f.slug,
                "name": f.name,
                "license": f.license,
                "license_url": f.license_url,
                "fallback": f.fallback,
                "files": f.files.iter().map(|x| json!({
                    "weight": x.weight,
                    "italic": x.italic,
                    "format": x.format,
                    "kb": x.size / 1024,
                })).collect::<Vec<_>>(),
                "has_bold": f.files.iter().any(|x| x.weight >= 600),
                "is_body": tokens.font.body == f.slug,
                "is_heading": tokens.font.heading == f.slug,
            })).collect::<Vec<_>>(),
            "licenses": fonts::LICENSES,
            "max_mb": fonts::MAX_BYTES / 1024 / 1024,
            "can_edit": who.actor.has(Permission::DesignEdit),
            "notice": notice,
            "error": error,
        }),
    );
    render(state, status, "fonts.html", &ctx)
}

#[derive(Debug, Deserialize)]
struct FontUploadQuery {
    name: String,
    #[serde(default)]
    slug: Option<String>,
    license: String,
    #[serde(default)]
    fallback: Option<String>,
    #[serde(default)]
    weight: Option<i64>,
    #[serde(default)]
    italic: Option<u8>,
    #[serde(default)]
    rights: Option<u8>,
}

/// 서체 올리기. 본문은 파일 바이트 그대로(`/admin/media` 와 같은 방식), 나머지는 쿼리.
async fn upload_font(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<FontUploadQuery>,
    body: Bytes,
) -> Result<Response, ApiError> {
    let who = json_session(&state, &headers).await?;
    let input = fonts::NewFont {
        name: q.name,
        slug: q.slug.filter(|s| !s.is_empty()),
        fallback: q.fallback.filter(|s| !s.is_empty()),
        license: q.license,
        license_text: None,
        rights_confirmed: q.rights.unwrap_or(0) != 0,
    };
    let upload = fonts::FontUpload {
        weight: q.weight.unwrap_or(400),
        italic: q.italic.unwrap_or(0) != 0,
        bytes: body.to_vec(),
    };
    let up = fonts::add_with_file(&state.ctx, &who.actor, input, upload).await?;
    Ok((StatusCode::CREATED, Json(Success::new(up))).into_response())
}

#[derive(Debug, Deserialize)]
struct LicenseBody {
    license_text: String,
}

/// 올린 뒤 라이선스 전문을 붙인다 (JSON).
async fn font_license(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(slug): Path<String>,
    body: Result<Json<LicenseBody>, JsonRejection>,
) -> Result<Response, ApiError> {
    let who = json_session(&state, &headers).await?;
    let Json(b) = body.map_err(|e| {
        ApiError(
            Error::new(Code::Validation)
                .with_message("error.json_body")
                .arg("detail", e.body_text()),
        )
    })?;
    let patch = fonts::FontPatch {
        license_text: Some(b.license_text),
        ..fonts::FontPatch::default()
    };
    let font = fonts::update(&state.ctx, &who.actor, &slug, patch).await?;
    Ok(Json(Success::new(font)).into_response())
}

#[derive(Debug, Default, Deserialize)]
struct FontActionForm {
    #[serde(default, rename = "_csrf")]
    csrf: String,
    #[serde(default)]
    weight: Option<i64>,
    #[serde(default)]
    italic: Option<u8>,
}

/// 목록의 단추들 (스크립트 없이 폼으로): 본문·제목 서체로 쓰기, 굵기 파일 떼기, 서체 지우기.
async fn font_action(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((slug, action)): Path<(String, String)>,
    Form(f): Form<FontActionForm>,
) -> Response {
    let who = match require(&state, &headers, "/admin/fonts").await {
        Ok(w) => w,
        Err(r) => return *r,
    };
    if !session_csrf_ok(&who, &f.csrf) {
        return csrf_failed(&state, Some(&who)).await;
    }
    let res = match action.as_str() {
        "use-body" | "use-heading" => {
            let field = if action == "use-body" {
                "body"
            } else {
                "heading"
            };
            design::update(&state.ctx, &who.actor, json!({ "font": { field: slug } }))
                .await
                .map(|_| field)
        }
        "remove-file" => fonts::remove_file(
            &state.ctx,
            &who.actor,
            &slug,
            f.weight.unwrap_or(400),
            f.italic.unwrap_or(0) != 0,
        )
        .await
        .map(|_| "file-removed"),
        "remove" => fonts::remove(&state.ctx, &who.actor, &slug)
            .await
            .map(|_| "removed"),
        _ => Err(Error::new(Code::NotFound).with_message("error.not_found")),
    };
    match res {
        Ok(done) => Redirect::to(&format!("/admin/fonts?done={done}")).into_response(),
        Err(e) if e.code() == Code::Forbidden || e.code() == Code::NotFound => {
            fail(&state, Some(&who), &e).await
        }
        Err(e) => {
            let status = if e.code() == Code::Conflict {
                StatusCode::CONFLICT
            } else {
                StatusCode::BAD_REQUEST
            };
            fonts_render(&state, &who, status, None, Some(e.message())).await
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn next_must_be_internal() {
        assert_eq!(safe_next("/admin/posts/1"), "/admin/posts/1");
        assert_eq!(safe_next("/"), "/");
        for bad in [
            "https://evil.test",
            "//evil.test",
            "/\\evil.test",
            "/a\\b",
            "evil",
            "",
            "/\nx",
            "javascript:alert(1)",
        ] {
            assert_eq!(safe_next(bad), "/admin", "{bad:?}");
        }
    }

    #[test]
    fn cookie_parsing_finds_exact_name() {
        let mut h = HeaderMap::new();
        h.insert(
            header::COOKIE,
            HeaderValue::from_static("a=1; maru_session_x=no; maru_session=yes"),
        );
        assert_eq!(cookie(&h, SESSION_COOKIE).as_deref(), Some("yes"));
        assert_eq!(cookie(&h, CSRF_COOKIE), None);
    }

    #[test]
    fn notices_come_from_fixed_list() {
        assert!(notice("saved").is_some());
        assert!(notice("<script>").is_none());
    }
}

// ---- 에디터용 JSON (업로드·미리보기) ----
//
// 에디터 스크립트가 `fetch` 로 부른다. 세션 쿠키(SameSite=Lax)와 `X-CSRF-Token` 헤더를 둘 다 요구한다.
// 응답은 REST 와 같은 `{ok, data}` / `{ok:false, error}` 모양이다.

const CSRF_HEADER: &str = "x-csrf-token";

async fn json_session(state: &AppState, headers: &HeaderMap) -> Result<SignedIn, ApiError> {
    let who = current(state, headers)
        .await?
        .ok_or_else(|| Error::new(Code::Unauthorized))?;
    let sent = headers
        .get(CSRF_HEADER)
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default();
    if !session_csrf_ok(&who, sent) {
        return Err(ApiError(
            Error::new(Code::Forbidden).with_message("error.csrf"),
        ));
    }
    Ok(who)
}

#[derive(Debug, Deserialize)]
struct UploadQuery {
    #[serde(default)]
    filename: String,
    alt: Option<String>,
}

async fn upload(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<UploadQuery>,
    body: Bytes,
) -> Result<Response, ApiError> {
    let who = json_session(&state, &headers).await?;
    let input = media::Upload {
        filename: q.filename,
        bytes: body.to_vec(),
        alt_text: q.alt,
    };
    let m = crate::upload_media(&state, who.actor, input).await?;
    Ok((StatusCode::CREATED, Json(Success::new(m))).into_response())
}

/// 에디터의 글 틀 메뉴. REST `/api/v1/templates` 와 같은 서비스.
async fn list_templates(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    let who = json_session(&state, &headers).await?;
    let list = templates::list(&state.ctx, &who.actor).await?;
    Ok(Json(Success::new(list)).into_response())
}

async fn save_template(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Result<Json<templates::SaveTemplate>, JsonRejection>,
) -> Result<Response, ApiError> {
    let who = json_session(&state, &headers).await?;
    let Json(input) = body.map_err(|e| {
        ApiError(
            Error::new(Code::Validation)
                .with_message("error.json_body")
                .arg("detail", e.body_text()),
        )
    })?;
    let t = templates::save(&state.ctx, &who.actor, input).await?;
    Ok(Json(Success::new(t)).into_response())
}

#[derive(Debug, Deserialize)]
struct FileQuery {
    filename: String,
}

/// 에디터의 파일 첨부. REST `POST /api/v1/files` 와 같은 서비스.
async fn upload_file(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<FileQuery>,
    body: Bytes,
) -> Result<Response, ApiError> {
    let who = json_session(&state, &headers).await?;
    let input = files::Upload {
        filename: q.filename,
        bytes: body.to_vec(),
    };
    let f = files::upload(&state.ctx, &who.actor, input).await?;
    Ok((StatusCode::CREATED, Json(Success::new(f))).into_response())
}

/// 발행 설정의 주소 칸: 입력하는 동안 겹치는지 알려 준다.
async fn check_slug(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(input): Query<posts::SlugCheckInput>,
) -> Result<Response, ApiError> {
    let who = json_session(&state, &headers).await?;
    let out = posts::check_slug(&state.ctx, &who.actor, input).await?;
    Ok(Json(Success::new(out)).into_response())
}

async fn preview(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Result<Json<posts::PreviewInput>, JsonRejection>,
) -> Result<Response, ApiError> {
    let who = json_session(&state, &headers).await?;
    let Json(input) = body.map_err(json_body_error)?;
    let out = posts::preview(&state.ctx, &who.actor, input)?;
    Ok(Json(Success::new(out)).into_response())
}

/// 발행 설정의 「새 분류」: 그 자리에서 분류를 만든다. 같은 주소가 있으면 그것을 돌려준다.
async fn create_category(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Result<Json<categories::NewCategory>, JsonRejection>,
) -> Result<Response, ApiError> {
    let who = json_session(&state, &headers).await?;
    let Json(input) = body.map_err(json_body_error)?;
    let c = categories::create(&state.ctx, &who.actor, input).await?;
    Ok((StatusCode::CREATED, Json(Success::new(c))).into_response())
}

fn json_body_error(e: JsonRejection) -> ApiError {
    ApiError(
        Error::new(Code::Validation)
            .with_message("error.json_body")
            .arg("detail", e.body_text()),
    )
}

// ---- 위젯 (ADR-0059) ----

#[derive(Debug, Default, Deserialize)]
struct BackupsQuery {
    #[serde(default)]
    done: String,
}

#[derive(Debug, Default, Deserialize)]
struct BackupForm {
    #[serde(rename = "_csrf", default)]
    csrf: String,
    #[serde(default)]
    no_secrets: Option<String>,
}

/// 백업 (ADR-0063). 운영자만. 복원은 서버를 끈 뒤 CLI 로만 한다.
async fn backups_page(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<BackupsQuery>,
) -> Response {
    use maru_core::services::backups;
    let who = match require(&state, &headers, "/admin/backups").await {
        Ok(w) => w,
        Err(r) => return *r,
    };
    let archiver = match state.archiver() {
        Ok(a) => a,
        Err(e) => return fail(&state, Some(&who), &e).await,
    };
    let list = match backups::list(&who.actor, archiver).await {
        Ok(l) => l,
        Err(e) => return fail(&state, Some(&who), &e).await,
    };
    let tz = settings::site(&state.ctx)
        .await
        .map(|s| s.tz_offset_min)
        .unwrap_or(540);
    let items = list
        .iter()
        .map(|b| {
            json!({
                "name": b.name,
                "size": human_size(b.size),
                "at": b.created_at,
                "at_display": clock::display_datetime(&b.created_at, tz),
                "secrets": b.secrets,
            })
        })
        .collect::<Vec<_>>();
    let notice = match q.done.as_str() {
        "created" => Some("백업을 만들었습니다."),
        "deleted" => Some("백업을 지웠습니다."),
        _ => None,
    };
    let ctx = merge(
        base(&state, Some(&who), "backups").await,
        json!({ "items": items, "notice": notice, "data_dir": "<데이터 디렉터리>" }),
    );
    render(&state, StatusCode::OK, "backups.html", &ctx)
}

fn human_size(n: u64) -> String {
    const KB: f64 = 1024.0;
    let f = n as f64;
    if f < KB {
        format!("{n} B")
    } else if f < KB * KB {
        format!("{:.1} KB", f / KB)
    } else if f < KB * KB * KB {
        format!("{:.1} MB", f / KB / KB)
    } else {
        format!("{:.2} GB", f / KB / KB / KB)
    }
}

async fn backup_create(
    State(state): State<AppState>,
    headers: HeaderMap,
    Form(form): Form<BackupForm>,
) -> Response {
    use maru_core::services::backups::{self, NewBackup};
    let who = match require_post(&state, &headers, &form.csrf).await {
        Ok(w) => w,
        Err(r) => return *r,
    };
    let res = match state.archiver() {
        Ok(a) => {
            backups::create(
                &state.ctx,
                &who.actor,
                a,
                NewBackup {
                    no_secrets: form.no_secrets.is_some(),
                },
            )
            .await
        }
        Err(e) => Err(e),
    };
    match res {
        Ok(_) => Redirect::to("/admin/backups?done=created").into_response(),
        Err(e) => fail(&state, Some(&who), &e).await,
    }
}

async fn backup_download(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(name): Path<String>,
) -> Response {
    use maru_core::services::backups;
    let who = match require(&state, &headers, "/admin/backups").await {
        Ok(w) => w,
        Err(r) => return *r,
    };
    let res = match state.archiver() {
        Ok(a) => backups::file(&who.actor, a, &name).await,
        Err(e) => Err(e),
    };
    let res = match res {
        Ok(path) => crate::backup_file_response(&path, &name).await,
        Err(e) => Err(e),
    };
    match res {
        Ok(r) => r,
        Err(e) => fail(&state, Some(&who), &e).await,
    }
}

async fn backup_delete(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(name): Path<String>,
    Form(form): Form<BackupForm>,
) -> Response {
    use maru_core::services::backups;
    let who = match require_post(&state, &headers, &form.csrf).await {
        Ok(w) => w,
        Err(r) => return *r,
    };
    let res = match state.archiver() {
        Ok(a) => backups::delete(&state.ctx, &who.actor, a, &name).await,
        Err(e) => Err(e),
    };
    match res {
        Ok(()) => Redirect::to("/admin/backups?done=deleted").into_response(),
        Err(e) => fail(&state, Some(&who), &e).await,
    }
}

#[derive(Debug, Default, Deserialize)]
struct AuditPageQuery {
    #[serde(default)]
    action: String,
    #[serde(default)]
    actor_type: String,
    #[serde(default)]
    target_type: String,
    #[serde(default)]
    target_id: String,
    #[serde(default)]
    page: Option<u32>,
}

fn actor_label(t: &str) -> &'static str {
    match t {
        "human" => "사람",
        "agent" => "에이전트",
        "local" => "로컬 CLI",
        "anonymous" => "손님",
        _ => "시스템",
    }
}

/// 감사 로그 (ADR-0062). 운영자만.
async fn audit_page(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<AuditPageQuery>,
) -> Response {
    use maru_core::services::audit_log::{self, ACTOR_TYPES, AuditQuery};
    let who = match require(&state, &headers, "/admin/audit").await {
        Ok(w) => w,
        Err(r) => return *r,
    };
    let some = |v: &str| (!v.trim().is_empty()).then(|| v.trim().to_owned());
    let query = AuditQuery {
        action: some(&q.action),
        actor_type: some(&q.actor_type),
        target_type: some(&q.target_type),
        target_id: some(&q.target_id),
        page: q.page,
        ..Default::default()
    };
    let list = match audit_log::list(&state.ctx, &who.actor, &query).await {
        Ok(l) => l,
        Err(e) => return fail(&state, Some(&who), &e).await,
    };
    let tz = settings::site(&state.ctx)
        .await
        .map(|s| s.tz_offset_min)
        .unwrap_or(540);
    let items = list
        .items
        .iter()
        .map(|e| {
            let diff = if e.diff.as_object().is_some_and(|o| o.is_empty()) || e.diff.is_null() {
                String::new()
            } else {
                serde_json::to_string_pretty(&e.diff).unwrap_or_default()
            };
            json!({
                "action": e.action,
                "target_type": e.target_type,
                "target_id": e.target_id,
                "at": e.at,
                "at_display": clock::display_datetime(&e.at, tz),
                "actor_label": actor_label(&e.actor_type),
                "actor_name": e.actor_name,
                "diff_text": diff,
            })
        })
        .collect::<Vec<_>>();
    // 쪽 이동 링크에 조건을 그대로 붙인다.
    let mut base_query = String::from("/admin/audit?");
    for (k, v) in [
        ("action", &q.action),
        ("actor_type", &q.actor_type),
        ("target_type", &q.target_type),
        ("target_id", &q.target_id),
    ] {
        if !v.trim().is_empty() {
            base_query.push_str(&format!("{k}={}&", urlencoding_min(v.trim())));
        }
    }
    let filtered = base_query.len() > "/admin/audit?".len();
    let ctx = merge(
        base(&state, Some(&who), "audit").await,
        json!({
            "items": items,
            "total": list.total,
            "page": list.page,
            "pages": list.total_pages(),
            "q": { "action": q.action, "actor_type": q.actor_type, "target_type": q.target_type, "target_id": q.target_id },
            "filtered": filtered,
            "base_query": base_query,
            "actor_types": ACTOR_TYPES.iter().map(|t| [*t, actor_label(t)]).collect::<Vec<_>>(),
        }),
    );
    render(&state, StatusCode::OK, "audit.html", &ctx)
}

/// 쿼리 문자열 값 인코딩 (영숫자·`-._~` 외는 %XX).
fn urlencoding_min(v: &str) -> String {
    v.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                (b as char).to_string()
            }
            _ => format!("%{b:02X}"),
        })
        .collect()
}

#[derive(Debug, Default, Deserialize)]
struct StatsPageQuery {
    #[serde(default)]
    days: Option<u32>,
}

/// 방문자 통계 (ADR-0061). 기간은 7·30·90일 단추, 다른 값도 받는다(1~366).
async fn stats_page(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<StatsPageQuery>,
) -> Response {
    use maru_core::services::stats;
    let who = match require(&state, &headers, "/admin/stats").await {
        Ok(w) => w,
        Err(r) => return *r,
    };
    let s = match stats::summary(&state.ctx, &who.actor, &stats::StatsQuery { days: q.days }).await
    {
        Ok(s) => s,
        Err(e) => return fail(&state, Some(&who), &e).await,
    };
    let days = s.daily.len();
    let max = s.daily.iter().map(|d| d.views).max().unwrap_or(0).max(1);
    let ctx = merge(
        base(&state, Some(&who), "stats").await,
        json!({
            "s": s,
            "days": days,
            "max": max,
            // 최근 날이 위로
            "rows": s.daily.iter().rev().collect::<Vec<_>>(),
            "ranges": [7, 30, 90],
        }),
    );
    render(&state, StatusCode::OK, "stats.html", &ctx)
}

async fn stats_csv(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<StatsPageQuery>,
) -> Response {
    use maru_core::services::stats;
    let who = match require(&state, &headers, "/admin/stats").await {
        Ok(w) => w,
        Err(r) => return *r,
    };
    match stats::summary(&state.ctx, &who.actor, &stats::StatsQuery { days: q.days }).await {
        Ok(s) => crate::api::csv_response(&s),
        Err(e) => fail(&state, Some(&who), &e).await,
    }
}

#[derive(Debug, Default, Deserialize)]
struct WidgetsQuery {
    #[serde(default)]
    done: String,
}

/// 놓기 폼의 값. 실패하면 그대로 다시 채워 보인다.
#[derive(Debug, Default, Deserialize, serde::Serialize)]
struct WidgetPlaceForm {
    #[serde(default, rename = "_csrf")]
    #[serde(skip_serializing)]
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

async fn widgets_page(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<WidgetsQuery>,
) -> Response {
    let who = match require(&state, &headers, "/admin/widgets").await {
        Ok(w) => w,
        Err(r) => return *r,
    };
    let notice = match q.done.as_str() {
        "placed" => Some("위젯을 놓았습니다."),
        "enable" => Some("위젯을 켰습니다."),
        "disable" => Some("위젯을 껐습니다."),
        "remove" => Some("위젯을 뺐습니다."),
        "up" | "down" => Some("순서를 바꿨습니다."),
        _ => None,
    };
    widgets_render(
        &state,
        &who,
        StatusCode::OK,
        notice,
        None,
        &WidgetPlaceForm::default(),
    )
    .await
}

async fn widgets_render(
    state: &AppState,
    who: &SignedIn,
    status: StatusCode,
    notice: Option<&str>,
    error: Option<String>,
    form: &WidgetPlaceForm,
) -> Response {
    use maru_core::model::WIDGET_REGIONS;
    use maru_core::services::widgets;
    let list = match widgets::list(&state.ctx, &who.actor).await {
        Ok(l) => l,
        Err(e) => return fail(state, Some(who), &e).await,
    };
    let regions = WIDGET_REGIONS
        .iter()
        .map(|r| {
            let items = list
                .iter()
                .filter(|w| w.region == *r)
                .map(|w| {
                    json!({
                        "id": w.id,
                        "kind": w.kind,
                        "kind_name": widgets::kind_name(&w.kind),
                        "title": w.title,
                        "scope": w.scope,
                        "enabled": w.enabled,
                        "config": serde_json::to_string(&w.config).unwrap_or_default(),
                    })
                })
                .collect::<Vec<_>>();
            json!({ "id": r, "label": region_label(r), "items": items })
        })
        .collect::<Vec<_>>();
    let types = widgets::types()
        .into_iter()
        .map(|t| json!({ "id": t.id, "name": t.name, "description": t.description }))
        .collect::<Vec<_>>();
    let ctx = merge(
        base(state, Some(who), "widgets").await,
        json!({
            "notice": notice,
            "error": error,
            "regions": regions,
            "total": list.len(),
            "types": types,
            "region_choices": WIDGET_REGIONS.iter().map(|r| json!({ "id": r, "label": region_label(r) })).collect::<Vec<_>>(),
            "form": form,
        }),
    );
    render(state, status, "widgets.html", &ctx)
}

fn region_label(region: &str) -> &'static str {
    match region {
        "header" => "머리글 아래 띠",
        "hero" => "큰 배너 자리",
        "main" => "본문 위",
        "sidebar" => "곁가지",
        "popup" => "팝업",
        _ => "바닥글",
    }
}

async fn widget_place(
    State(state): State<AppState>,
    headers: HeaderMap,
    Form(f): Form<WidgetPlaceForm>,
) -> Response {
    use maru_core::services::widgets;
    let who = match require(&state, &headers, "/admin/widgets").await {
        Ok(w) => w,
        Err(r) => return *r,
    };
    if !session_csrf_ok(&who, &f.csrf) {
        return fail(
            &state,
            Some(&who),
            &Error::new(Code::Forbidden).with_message("error.csrf"),
        )
        .await;
    }
    let config = if f.config.trim().is_empty() {
        Ok(Value::Null)
    } else {
        serde_json::from_str::<Value>(&f.config).map_err(|e| {
            Error::new(Code::Validation)
                .with_message("error.json_body")
                .arg("detail", e.to_string())
        })
    };
    let placed = match config {
        Ok(config) => {
            widgets::place(
                &state.ctx,
                &who.actor,
                widgets::NewWidget {
                    kind: f.kind.clone(),
                    region: f.region.clone(),
                    scope: if f.scope.trim().is_empty() {
                        "all".into()
                    } else {
                        f.scope.clone()
                    },
                    title: f.title.clone(),
                    config,
                    position: None,
                    enabled: true,
                },
            )
            .await
        }
        Err(e) => Err(e),
    };
    match placed {
        Ok(_) => Redirect::to("/admin/widgets?done=placed").into_response(),
        Err(e) if e.code() == Code::Validation || e.code() == Code::NotFound => {
            widgets_render(
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

async fn widget_action(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((id, action)): Path<(String, String)>,
    Form(f): Form<CommentActionForm>,
) -> Response {
    use maru_core::services::widgets::{self, WidgetPatch};
    let who = match require(&state, &headers, "/admin/widgets").await {
        Ok(w) => w,
        Err(r) => return *r,
    };
    if !session_csrf_ok(&who, &f.csrf) {
        return fail(
            &state,
            Some(&who),
            &Error::new(Code::Forbidden).with_message("error.csrf"),
        )
        .await;
    }
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
        "up" | "down" => swap_widget(&state, &who, &id, action == "up").await,
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
async fn swap_widget(
    state: &AppState,
    who: &SignedIn,
    id: &str,
    up: bool,
) -> maru_core::Result<()> {
    use maru_core::services::widgets::{self, WidgetPatch};
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
