//! 공개 사이트의 회원 화면 (GAP M1·M2·M3, ADR-0051): 로그인·가입·비밀번호 찾기·메일 인증·내 정보.
//!
//! - 테마 틀(`account.html`·`me.html`)로 그린다. 세션 쿠키·CSRF 는 관리 화면과 같다(`admin.rs`).
//! - 인증·재설정 링크는 GET 으로 열면 확인 단추만 보인다. 메일 미리보기가 링크를 열어도 쓰이지 않는다.
//! - 가입·찾기의 답은 계정이 있든 없든 같다. 서비스(`members`)가 정한다.
//! - 모든 응답은 `no-store`·`no-referrer` (주소에 1회용 링크가 든다).

use axum::Router;
use axum::extract::{Form, Path, Query, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::middleware;
use axum::response::{Html, IntoResponse, Redirect, Response};
use axum::routing::{get, post};
use maru_core::actor::Permission;
use maru_core::clock;
use maru_core::model::{CaptchaMode, PageReq, PostStatus, SignupMode};
use maru_core::services::auth::{self, SignedIn};
use maru_core::services::members::{self, ProfilePatch, SignupInput};
use maru_core::services::{captcha, posts, settings};
use maru_core::urls::{self, Target};
use maru_core::{Code, Error};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::AppState;
use crate::admin::{
    SESSION_COOKIE, current, encode, is_secret, pre_csrf, pre_csrf_ok, session_csrf_ok, set_cookie,
    with_session,
};

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/login", get(login_form).post(login))
        .route("/logout", post(logout))
        .route("/signup", get(signup_form).post(signup))
        .route("/forgot", get(forgot_form).post(forgot))
        .route("/reset/{token}", get(reset_form).post(reset))
        .route("/verify/{token}", get(verify_form).post(verify))
        .route("/me", get(me))
        .route("/me/profile", post(profile))
        .route("/me/password", post(password))
        .route("/me/withdraw", post(withdraw))
        .layer(middleware::map_response(account_headers))
}

async fn account_headers(mut res: Response) -> Response {
    let h = res.headers_mut();
    h.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    h.insert(
        header::REFERRER_POLICY,
        HeaderValue::from_static("no-referrer"),
    );
    res
}

// ---- 렌더링 ----

/// `account.html` 한 장으로 로그인·가입·찾기·재설정·인증·안내를 그린다 (`account.view`).
async fn page(
    state: &AppState,
    headers: &HeaderMap,
    status: StatusCode,
    title: &str,
    account: Value,
) -> Response {
    let (csrf, set) = match pre_csrf(state, headers) {
        Ok(v) => v,
        Err(e) => return crate::error::page(state, &e).await,
    };
    let site = settings::site(&state.ctx).await.unwrap_or_default();
    let mut account = account;
    account["csrf"] = json!(csrf);
    let ctx = json!({
        "site": crate::public::site_json(state).await,
        "meta": { "title": format!("{title} · {}", site.name), "noindex": true },
        "signup_open": site.signup == SignupMode::Open,
        "account": account,
    });
    let mut res = render(state, status, "account.html", &ctx).await;
    if let Some(set) = set {
        res.headers_mut().append(header::SET_COOKIE, set);
    }
    res
}

async fn render(state: &AppState, status: StatusCode, name: &str, ctx: &Value) -> Response {
    match state.theme.render(name, ctx) {
        Ok(html) => (status, Html(html)).into_response(),
        Err(e) => crate::error::page(state, &e).await,
    }
}

/// 제목·안내 문장 한 장 (메일을 보냈다, 링크가 만료됐다, 탈퇴했다 …).
async fn notice(
    state: &AppState,
    headers: &HeaderMap,
    status: StatusCode,
    heading: &str,
    body: &str,
    links: Value,
) -> Response {
    page(
        state,
        headers,
        status,
        heading,
        json!({ "view": "notice", "heading": heading, "body": body, "links": links }),
    )
    .await
}

async fn link_invalid(state: &AppState, headers: &HeaderMap) -> Response {
    notice(
        state,
        headers,
        StatusCode::UNAUTHORIZED,
        "링크를 쓸 수 없습니다",
        "링크가 만료됐거나 이미 쓰였습니다. 다시 요청하세요.",
        json!([{ "label": "비밀번호 찾기", "href": "/forgot" }, { "label": "로그인", "href": "/login" }]),
    )
    .await
}

async fn csrf_failed(state: &AppState, headers: &HeaderMap) -> Response {
    notice(
        state,
        headers,
        StatusCode::FORBIDDEN,
        "요청을 확인하지 못했습니다",
        "화면이 오래됐거나 다른 곳에서 보낸 요청입니다. 화면을 새로 고친 뒤 다시 시도하세요.",
        json!([]),
    )
    .await
}

/// 내부 경로만 (`admin::safe_next` 와 같은 규칙). 비었거나 바깥이면 `/me`.
fn next_or_me(next: &str) -> &str {
    if !next.is_empty() && crate::admin::safe_next(next) == next {
        next
    } else {
        "/me"
    }
}

fn clear_session(state: &AppState, mut res: Response) -> Response {
    res.headers_mut().append(
        header::SET_COOKIE,
        set_cookie(state, SESSION_COOKIE, "", Some(0)),
    );
    res
}

// ---- 로그인·로그아웃 ----

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
        return Redirect::to(next_or_me(&q.next)).into_response();
    }
    login_page(&state, &headers, StatusCode::OK, &q.next, "", None).await
}

async fn login_page(
    state: &AppState,
    headers: &HeaderMap,
    status: StatusCode,
    next: &str,
    email: &str,
    error: Option<String>,
) -> Response {
    let next = next_or_me(next);
    page(
        state,
        headers,
        status,
        "로그인",
        json!({ "view": "login", "next": next, "email": email, "error": error }),
    )
    .await
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
        return csrf_failed(&state, &headers).await;
    }
    let Ok(_permit) = state.login_gate.acquire().await else {
        return crate::error::page(&state, &Error::internal("login gate closed")).await;
    };
    match auth::login_password(&state.ctx, &f.email, &f.password).await {
        Ok(new) => with_session(&state, &new, next_or_me(&f.next)),
        Err(e) if matches!(e.code(), Code::Unauthorized | Code::Forbidden) => {
            let status =
                StatusCode::from_u16(e.code().http_status()).unwrap_or(StatusCode::UNAUTHORIZED);
            login_page(
                &state,
                &headers,
                status,
                &f.next,
                &f.email,
                Some(e.message()),
            )
            .await
        }
        Err(e) => crate::error::page(&state, &e).await,
    }
}

#[derive(Debug, Default, Deserialize)]
struct CsrfForm {
    #[serde(default, rename = "_csrf")]
    csrf: String,
}

async fn logout(
    State(state): State<AppState>,
    headers: HeaderMap,
    Form(f): Form<CsrfForm>,
) -> Response {
    let who = match current(&state, &headers).await {
        Ok(Some(w)) => w,
        Ok(None) => return Redirect::to("/").into_response(),
        Err(e) => return crate::error::page(&state, &e).await,
    };
    if !session_csrf_ok(&who, &f.csrf) {
        return csrf_failed(&state, &headers).await;
    }
    if let Err(e) = auth::logout(&state.ctx, &who).await {
        return crate::error::page(&state, &e).await;
    }
    clear_session(&state, Redirect::to("/").into_response())
}

// ---- 가입 ----

async fn signup_open(state: &AppState) -> bool {
    settings::site(&state.ctx)
        .await
        .is_ok_and(|s| s.signup == SignupMode::Open)
}

async fn closed(state: &AppState) -> Response {
    crate::error::page(
        state,
        &Error::new(Code::NotFound).with_message("error.signup_closed"),
    )
    .await
}

async fn signup_form(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if !signup_open(&state).await {
        return closed(&state).await;
    }
    if let Ok(Some(_)) = current(&state, &headers).await {
        return Redirect::to("/me").into_response();
    }
    signup_page(&state, &headers, StatusCode::OK, "", "", None).await
}

async fn signup_page(
    state: &AppState,
    headers: &HeaderMap,
    status: StatusCode,
    email: &str,
    name: &str,
    error: Option<String>,
) -> Response {
    // 확인 질문은 가입 화면을 열 때마다 새로 낸다 (이 화면은 no-store).
    let challenge = match settings::site(&state.ctx).await {
        Ok(s) if s.captcha == CaptchaMode::Guest => match captcha::issue(&state.ctx).await {
            Ok(c) => Some(c),
            Err(e) => return crate::error::page(state, &e).await,
        },
        _ => None,
    };
    page(
        state,
        headers,
        status,
        "회원 가입",
        json!({
            "view": "signup",
            "email": email,
            "display_name": name,
            "error": error,
            "min_password": auth::MIN_PASSWORD_CHARS,
            "min_name": members::MIN_NAME_CHARS,
            "max_name": members::MAX_NAME_CHARS,
            "captcha": challenge,
        }),
    )
    .await
}

#[derive(Debug, Default, Deserialize)]
struct SignupForm {
    #[serde(default, rename = "_csrf")]
    csrf: String,
    #[serde(default)]
    email: String,
    #[serde(default)]
    display_name: String,
    #[serde(default)]
    password: String,
    #[serde(default)]
    password2: String,
    #[serde(default)]
    captcha_token: String,
    #[serde(default)]
    captcha_answer: String,
    /// 함정 칸
    #[serde(default)]
    website: String,
}

async fn signup(
    State(state): State<AppState>,
    headers: HeaderMap,
    Form(f): Form<SignupForm>,
) -> Response {
    if !pre_csrf_ok(&headers, &f.csrf) || !f.website.is_empty() {
        return csrf_failed(&state, &headers).await;
    }
    if f.password != f.password2 {
        return signup_page(
            &state,
            &headers,
            StatusCode::BAD_REQUEST,
            &f.email,
            &f.display_name,
            Some("두 비밀번호가 다릅니다.".into()),
        )
        .await;
    }
    let Ok(_permit) = state.login_gate.acquire().await else {
        return crate::error::page(&state, &Error::internal("login gate closed")).await;
    };
    let input = SignupInput {
        email: f.email.clone(),
        display_name: f.display_name.clone(),
        password: f.password,
        captcha: (!f.captcha_token.is_empty()).then_some(captcha::Answer {
            token: f.captcha_token,
            answer: f.captcha_answer,
        }),
    };
    match members::signup(&state.ctx, input).await {
        Ok(out) if out.verify_required => {
            notice(
                &state,
                &headers,
                StatusCode::OK,
                "메일을 확인하세요",
                "적어 주신 주소로 확인 메일을 보냈습니다. 메일의 링크를 누르면 가입이 끝납니다. 이미 가입한 주소라면 안내 메일이 갑니다.",
                json!([{ "label": "로그인", "href": "/login" }]),
            )
            .await
        }
        Ok(_) => {
            notice(
                &state,
                &headers,
                StatusCode::OK,
                "가입을 받았습니다",
                "이제 로그인할 수 있습니다. 이미 가입한 주소라면 안내 메일이 갑니다.",
                json!([{ "label": "로그인", "href": "/login" }]),
            )
            .await
        }
        Err(e) if e.code() == Code::NotFound => closed(&state).await,
        Err(e) if matches!(e.code(), Code::Validation | Code::Conflict) => {
            let status = StatusCode::from_u16(e.code().http_status()).unwrap_or(StatusCode::BAD_REQUEST);
            signup_page(&state, &headers, status, &f.email, &f.display_name, Some(e.message())).await
        }
        Err(e) => crate::error::page(&state, &e).await,
    }
}

// ---- 비밀번호 찾기·재설정 ----

async fn forgot_form(State(state): State<AppState>, headers: HeaderMap) -> Response {
    page(
        &state,
        &headers,
        StatusCode::OK,
        "비밀번호 찾기",
        json!({ "view": "forgot" }),
    )
    .await
}

#[derive(Debug, Default, Deserialize)]
struct EmailForm {
    #[serde(default, rename = "_csrf")]
    csrf: String,
    #[serde(default)]
    email: String,
}

async fn forgot(
    State(state): State<AppState>,
    headers: HeaderMap,
    Form(f): Form<EmailForm>,
) -> Response {
    if !pre_csrf_ok(&headers, &f.csrf) {
        return csrf_failed(&state, &headers).await;
    }
    if let Err(e) = members::request_reset(&state.ctx, &f.email).await {
        return crate::error::page(&state, &e).await;
    }
    notice(
        &state,
        &headers,
        StatusCode::OK,
        "메일을 확인하세요",
        "그 주소로 가입돼 있으면 비밀번호를 다시 정하는 메일을 보냈습니다. 링크는 1시간 동안 한 번만 쓸 수 있습니다.",
        json!([{ "label": "로그인", "href": "/login" }]),
    )
    .await
}

async fn reset_form(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(token): Path<String>,
) -> Response {
    if !is_secret(&token) || members::peek_reset(&state.ctx, &token).await.is_err() {
        return link_invalid(&state, &headers).await;
    }
    reset_page(&state, &headers, StatusCode::OK, &token, None).await
}

async fn reset_page(
    state: &AppState,
    headers: &HeaderMap,
    status: StatusCode,
    token: &str,
    error: Option<String>,
) -> Response {
    page(
        state,
        headers,
        status,
        "비밀번호 다시 정하기",
        json!({
            "view": "reset",
            "action": format!("/reset/{token}"),
            "error": error,
            "min_password": auth::MIN_PASSWORD_CHARS,
        }),
    )
    .await
}

#[derive(Debug, Default, Deserialize)]
struct PasswordForm {
    #[serde(default, rename = "_csrf")]
    csrf: String,
    #[serde(default)]
    current: String,
    #[serde(default)]
    password: String,
    #[serde(default)]
    password2: String,
}

async fn reset(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(token): Path<String>,
    Form(f): Form<PasswordForm>,
) -> Response {
    if !pre_csrf_ok(&headers, &f.csrf) {
        return csrf_failed(&state, &headers).await;
    }
    if !is_secret(&token) {
        return link_invalid(&state, &headers).await;
    }
    if f.password != f.password2 {
        return reset_page(
            &state,
            &headers,
            StatusCode::BAD_REQUEST,
            &token,
            Some("두 비밀번호가 다릅니다.".into()),
        )
        .await;
    }
    let Ok(_permit) = state.login_gate.acquire().await else {
        return crate::error::page(&state, &Error::internal("login gate closed")).await;
    };
    match members::reset_password(&state.ctx, &token, &f.password).await {
        Ok(new) => with_session(&state, &new, "/me?done=password"),
        Err(e) if e.code() == Code::Validation => {
            reset_page(
                &state,
                &headers,
                StatusCode::BAD_REQUEST,
                &token,
                Some(e.message()),
            )
            .await
        }
        Err(e) if e.code() == Code::Unauthorized => link_invalid(&state, &headers).await,
        Err(e) => crate::error::page(&state, &e).await,
    }
}

// ---- 메일 인증 ----

async fn verify_form(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(token): Path<String>,
) -> Response {
    let member = match is_secret(&token) {
        true => members::peek_verify(&state.ctx, &token).await.ok(),
        false => None,
    };
    let Some(member) = member else {
        return link_invalid(&state, &headers).await;
    };
    page(
        &state,
        &headers,
        StatusCode::OK,
        "메일 인증",
        json!({ "view": "verify", "action": format!("/verify/{token}"), "display_name": member.display_name }),
    )
    .await
}

async fn verify(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(token): Path<String>,
    Form(f): Form<CsrfForm>,
) -> Response {
    if !pre_csrf_ok(&headers, &f.csrf) {
        return csrf_failed(&state, &headers).await;
    }
    if !is_secret(&token) {
        return link_invalid(&state, &headers).await;
    }
    match members::verify_email(&state.ctx, &token).await {
        Ok(new) => with_session(&state, &new, "/me?done=verified"),
        Err(e) if e.code() == Code::Unauthorized => link_invalid(&state, &headers).await,
        Err(e) => crate::error::page(&state, &e).await,
    }
}

// ---- 내 정보 ----

#[derive(Debug, Default, Deserialize)]
struct MeQuery {
    #[serde(default)]
    done: String,
}

async fn require(state: &AppState, headers: &HeaderMap) -> Result<SignedIn, Box<Response>> {
    match current(state, headers).await {
        Ok(Some(who)) => Ok(who),
        Ok(None) => Err(Box::new(
            Redirect::to(&format!("/login?next={}", encode("/me"))).into_response(),
        )),
        Err(e) => Err(Box::new(crate::error::page(state, &e).await)),
    }
}

async fn me(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<MeQuery>,
) -> Response {
    let who = match require(&state, &headers).await {
        Ok(w) => w,
        Err(res) => return *res,
    };
    let done = match q.done.as_str() {
        "profile" => Some("닉네임을 바꿨습니다."),
        "password" => Some("비밀번호를 바꿨습니다. 다른 기기의 로그인은 모두 끊었습니다."),
        "verified" => Some("메일 인증이 끝났습니다. 환영합니다."),
        _ => None,
    };
    me_page(&state, &who, StatusCode::OK, done, None).await
}

async fn me_page(
    state: &AppState,
    who: &SignedIn,
    status: StatusCode,
    notice: Option<&str>,
    error: Option<(&str, String)>,
) -> Response {
    let site = settings::site(&state.ctx).await.unwrap_or_default();
    let posts = my_posts(state, who, site.tz_offset_min).await;
    let (error_in, error) = error.map_or((None, None), |(k, m)| (Some(k), Some(m)));
    let ctx = json!({
        "site": crate::public::site_json(state).await,
        "meta": { "title": format!("내 정보 · {}", site.name), "noindex": true },
        "me": {
            "csrf": who.session.csrf_token,
            "email": who.member.email,
            "display_name": who.member.display_name,
            "joined": clock::display_date(&who.member.created_at, site.tz_offset_min),
            "is_owner": who.actor.has(Permission::SiteAdmin),
            "admin_url": who.actor.has(Permission::PostModerate).then_some("/admin"),
            "posts": posts,
            "notice": notice,
            "error": error,
            "error_in": error_in,
            "min_password": auth::MIN_PASSWORD_CHARS,
        },
        "slots": crate::modules::me_slots(state, &who.actor).await,
    });
    render(state, status, "me.html", &ctx).await
}

/// 내가 쓴 글 최근 20개 (초안 포함). 글쓰기 권한이 없으면 빈 목록.
async fn my_posts(state: &AppState, who: &SignedIn, tz: i32) -> Vec<Value> {
    let Ok(list) = posts::list(
        &state.ctx,
        &who.actor,
        None,
        None,
        None,
        PageReq::new(1, 20),
    )
    .await
    else {
        return vec![];
    };
    let boards = state.ctx.store.list_boards().await.unwrap_or_default();
    list.items
        .iter()
        .filter(|p| p.author_id.as_deref() == Some(who.member.id.as_str()))
        .map(|p| {
            let board = boards.iter().find(|b| b.id == p.board_id);
            let url = (p.status == PostStatus::Published)
                .then(|| {
                    board.map(|b| {
                        urls::path(&Target::Post {
                            board: &b.slug,
                            post: &p.slug,
                        })
                    })
                })
                .flatten();
            json!({
                "title": p.title,
                "board": board.map(|b| b.name.clone()),
                "status": crate::admin::status_label(p.status),
                "date": clock::display_date(&p.updated_at, tz),
                "url": url,
            })
        })
        .collect()
}

#[derive(Debug, Default, Deserialize)]
struct ProfileForm {
    #[serde(default, rename = "_csrf")]
    csrf: String,
    #[serde(default)]
    display_name: String,
}

async fn profile(
    State(state): State<AppState>,
    headers: HeaderMap,
    Form(f): Form<ProfileForm>,
) -> Response {
    let who = match require(&state, &headers).await {
        Ok(w) => w,
        Err(res) => return *res,
    };
    if !session_csrf_ok(&who, &f.csrf) {
        return csrf_failed(&state, &headers).await;
    }
    let patch = ProfilePatch {
        display_name: Some(f.display_name),
    };
    match members::update_profile(&state.ctx, &who, patch).await {
        Ok(_) => Redirect::to("/me?done=profile").into_response(),
        Err(e) if matches!(e.code(), Code::Validation | Code::Conflict) => {
            me_page(
                &state,
                &who,
                StatusCode::BAD_REQUEST,
                None,
                Some(("profile", e.message())),
            )
            .await
        }
        Err(e) => crate::error::page(&state, &e).await,
    }
}

async fn password(
    State(state): State<AppState>,
    headers: HeaderMap,
    Form(f): Form<PasswordForm>,
) -> Response {
    let who = match require(&state, &headers).await {
        Ok(w) => w,
        Err(res) => return *res,
    };
    if !session_csrf_ok(&who, &f.csrf) {
        return csrf_failed(&state, &headers).await;
    }
    if f.password != f.password2 {
        return me_page(
            &state,
            &who,
            StatusCode::BAD_REQUEST,
            None,
            Some(("password", "두 비밀번호가 다릅니다.".into())),
        )
        .await;
    }
    let Ok(_permit) = state.login_gate.acquire().await else {
        return crate::error::page(&state, &Error::internal("login gate closed")).await;
    };
    match members::change_password(&state.ctx, &who, &f.current, &f.password).await {
        Ok(new) => with_session(&state, &new, "/me?done=password"),
        Err(e) if matches!(e.code(), Code::Validation | Code::Unauthorized) => {
            me_page(
                &state,
                &who,
                StatusCode::BAD_REQUEST,
                None,
                Some(("password", e.message())),
            )
            .await
        }
        Err(e) => crate::error::page(&state, &e).await,
    }
}

#[derive(Debug, Default, Deserialize)]
struct WithdrawForm {
    #[serde(default, rename = "_csrf")]
    csrf: String,
    #[serde(default)]
    password: String,
    #[serde(default)]
    agree: String,
}

async fn withdraw(
    State(state): State<AppState>,
    headers: HeaderMap,
    Form(f): Form<WithdrawForm>,
) -> Response {
    let who = match require(&state, &headers).await {
        Ok(w) => w,
        Err(res) => return *res,
    };
    if !session_csrf_ok(&who, &f.csrf) {
        return csrf_failed(&state, &headers).await;
    }
    if f.agree != "yes" {
        return me_page(
            &state,
            &who,
            StatusCode::BAD_REQUEST,
            None,
            Some(("withdraw", "탈퇴에 동의하는 칸을 골라 주세요.".into())),
        )
        .await;
    }
    let Ok(_permit) = state.login_gate.acquire().await else {
        return crate::error::page(&state, &Error::internal("login gate closed")).await;
    };
    match members::withdraw(&state.ctx, &who, &f.password).await {
        Ok(()) => {
            let res = notice(
                &state,
                &headers,
                StatusCode::OK,
                "탈퇴했습니다",
                "그동안 고마웠습니다. 쓰신 글은 「탈퇴한 회원」 이름으로 남습니다.",
                json!([{ "label": "첫 화면", "href": "/" }]),
            )
            .await;
            clear_session(&state, res)
        }
        Err(e) if matches!(e.code(), Code::Unauthorized | Code::Forbidden) => {
            me_page(
                &state,
                &who,
                StatusCode::BAD_REQUEST,
                None,
                Some(("withdraw", e.message())),
            )
            .await
        }
        Err(e) => crate::error::page(&state, &e).await,
    }
}
