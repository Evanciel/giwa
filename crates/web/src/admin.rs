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
use maru_core::actor::{Actor, Permission};
use maru_core::clock;
use maru_core::model::{PageReq, Post, PostStatus};
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

const SESSION_COOKIE: &str = "maru_session";
const CSRF_COOKIE: &str = "maru_csrf";

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/admin", get(home))
        .route("/admin/login", get(login_form).post(login))
        .route("/admin/link/{token}", get(link_form).post(link_use))
        .route("/admin/logout", post(logout))
        .route("/admin/password", get(password_form).post(password))
        .route("/admin/posts/new", get(new_form).post(create))
        .route("/admin/posts/{id}", get(edit_form).post(save))
        .route("/admin/posts/{id}/{action}", post(action))
        .route("/admin/media", post(upload).layer(crate::upload_limit()))
        .route(
            "/admin/files",
            post(upload_file).layer(crate::upload_limit()),
        )
        .route("/admin/fonts", get(fonts_page))
        .route(
            "/admin/fonts/upload",
            post(upload_font).layer(DefaultBodyLimit::max(crate::FONT_UPLOAD_LIMIT)),
        )
        .route("/admin/fonts/{slug}/license", post(font_license))
        .route("/admin/fonts/{slug}/{action}", post(font_action))
        .route("/admin/templates", get(list_templates).post(save_template))
        .route("/admin/preview", post(preview))
        .route("/admin/slug-check", get(check_slug))
        .route("/admin/categories", post(create_category))
        .route("/admin/assets/{*path}", get(asset))
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

fn cookie(headers: &HeaderMap, name: &str) -> Option<String> {
    headers
        .get_all(header::COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(';'))
        .filter_map(|p| p.trim().split_once('='))
        .find(|(k, _)| *k == name)
        .map(|(_, v)| v.to_owned())
}

fn set_cookie(state: &AppState, name: &str, value: &str, max_age: Option<i64>) -> HeaderValue {
    let mut c = format!("{name}={value}; HttpOnly; SameSite=Lax; Path=/");
    if let Some(age) = max_age {
        c.push_str(&format!("; Max-Age={age}"));
    }
    if state.ctx.site_url.starts_with("https://") {
        c.push_str("; Secure");
    }
    HeaderValue::from_str(&c).unwrap_or_else(|_| HeaderValue::from_static(""))
}

fn is_secret(v: &str) -> bool {
    v.len() == 64 && v.bytes().all(|b| b.is_ascii_hexdigit())
}

/// 로그인 전 폼의 CSRF 값. 쿠키가 없으면 새로 만들어 내보낸다.
fn pre_csrf(state: &AppState, headers: &HeaderMap) -> Result<(String, Option<HeaderValue>), Error> {
    match cookie(headers, CSRF_COOKIE) {
        Some(v) if is_secret(&v) => Ok((v, None)),
        _ => {
            let v = auth::new_secret()?;
            let set = set_cookie(state, CSRF_COOKIE, &v, None);
            Ok((v, Some(set)))
        }
    }
}

fn pre_csrf_ok(headers: &HeaderMap, sent: &str) -> bool {
    cookie(headers, CSRF_COOKIE).is_some_and(|c| is_secret(&c) && auth::same_secret(&c, sent))
}

fn session_csrf_ok(who: &SignedIn, sent: &str) -> bool {
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

fn encode(s: &str) -> String {
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

async fn current(state: &AppState, headers: &HeaderMap) -> Result<Option<SignedIn>, Error> {
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

fn with_session(state: &AppState, new: &NewSession, to: &str) -> Response {
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
        _ => return None,
    })
}

fn status_label(s: PostStatus) -> &'static str {
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
    let listing = match posts::list(&state.ctx, &who.actor, None, None, page).await {
        Ok(l) => l,
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
    let ctx = merge(
        base(&state, Some(&who), "posts").await,
        json!({
            "notice": notice(&q.done),
            "posts": rows,
            "total": listing.total,
            "page": page.page,
            "pages": pages.max(1),
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
}

impl PostForm {
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
            "boards": boards.iter().map(|b| json!({ "slug": b.slug, "name": b.name })).collect::<Vec<_>>(),
            "post": post.map(|p| json!({
                "id": p.id,
                "status": p.status,
                "status_label": status_label(p.status),
                "scheduled": p.scheduled_at.as_deref().map(|s| clock::display_datetime(s, tz)),
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
