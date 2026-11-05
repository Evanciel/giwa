//! 공개 사이트의 회원 화면 (GAP M1·M2·M3, ADR-0051): 로그인·가입·비밀번호 찾기·메일 인증·내 정보.
//! 2단계 인증(GAP M16, ADR-0072): 코드 확인 화면(`/login/totp`)과 내 정보의 켜기·끄기·복구 코드.
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
use maru_core::model::{CaptchaMode, CommentStatus, PageReq, PostStatus, SignupMode};
use maru_core::services::auth::{self, Login, SignedIn};
use maru_core::services::members::{self, ProfilePatch, SignupInput};
use maru_core::services::{captcha, comments, posts, settings, totp};
use maru_core::urls::{self, Target};
use maru_core::{Code, Error};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::AppState;
use crate::admin::{
    SESSION_COOKIE, cookie, current, encode, is_secret, pre_csrf, pre_csrf_ok, session_csrf_ok,
    set_cookie, with_session,
};

/// 코드를 기다리는 로그인의 값. 주소에 넣지 않고 5분짜리 쿠키로만 건넨다.
const TOTP_COOKIE: &str = "maru_totp";

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/login", get(login_form).post(login))
        .route("/login/totp", get(totp_form).post(totp_submit))
        .route("/logout", post(logout))
        .route("/signup", get(signup_form).post(signup))
        .route("/forgot", get(forgot_form).post(forgot))
        .route("/reset/{token}", get(reset_form).post(reset))
        .route("/verify/{token}", get(verify_form).post(verify))
        .route("/me", get(me))
        .route("/me/comments", get(my_comments_page))
        .route("/me/bar", get(account_bar))
        .route("/me/profile", post(profile))
        .route("/me/password", post(password))
        .route("/me/withdraw", post(withdraw))
        .route("/me/totp/{action}", post(totp_action))
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
        Ok(login) => finish_login(&state, &login, next_or_me(&f.next)),
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

/// 세션을 붙이거나, 2단계 인증을 켠 회원이면 코드 확인 화면으로 보낸다.
pub(crate) fn finish_login(state: &AppState, login: &Login, next: &str) -> Response {
    match login {
        Login::Session(new) => with_session(state, new, next),
        Login::Totp(p) => {
            let mut res =
                Redirect::to(&format!("/login/totp?next={}", encode(next))).into_response();
            let age = totp::CHALLENGE_MINUTES * 60;
            res.headers_mut().append(
                header::SET_COOKIE,
                set_cookie(state, TOTP_COOKIE, &p.secret, Some(age)),
            );
            res
        }
    }
}

fn clear_totp(state: &AppState, mut res: Response) -> Response {
    res.headers_mut().append(
        header::SET_COOKIE,
        set_cookie(state, TOTP_COOKIE, "", Some(0)),
    );
    res
}

async fn totp_expired(state: &AppState, headers: &HeaderMap, message: Option<String>) -> Response {
    let res = login_page(
        state,
        headers,
        StatusCode::UNAUTHORIZED,
        "",
        "",
        Some(message.unwrap_or_else(|| {
            Error::new(Code::Unauthorized)
                .with_message("error.totp_expired")
                .message()
        })),
    )
    .await;
    clear_totp(state, res)
}

async fn totp_page(
    state: &AppState,
    headers: &HeaderMap,
    status: StatusCode,
    next: &str,
    error: Option<String>,
) -> Response {
    page(
        state,
        headers,
        status,
        "2단계 인증",
        json!({ "view": "totp", "next": next_or_me(next), "error": error }),
    )
    .await
}

async fn totp_form(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<NextQuery>,
) -> Response {
    let raw = cookie(&headers, TOTP_COOKIE).unwrap_or_default();
    match totp::peek_challenge(&state.ctx, &raw).await {
        Ok(true) => totp_page(&state, &headers, StatusCode::OK, &q.next, None).await,
        Ok(false) => totp_expired(&state, &headers, None).await,
        Err(e) => crate::error::page(&state, &e).await,
    }
}

#[derive(Debug, Default, Deserialize)]
struct TotpForm {
    #[serde(default, rename = "_csrf")]
    csrf: String,
    #[serde(default)]
    code: String,
    #[serde(default)]
    next: String,
}

async fn totp_submit(
    State(state): State<AppState>,
    headers: HeaderMap,
    Form(f): Form<TotpForm>,
) -> Response {
    if !pre_csrf_ok(&headers, &f.csrf) {
        return csrf_failed(&state, &headers).await;
    }
    let raw = cookie(&headers, TOTP_COOKIE).unwrap_or_default();
    match totp::finish(&state.ctx, &raw, &f.code).await {
        Ok(new) => clear_totp(&state, with_session(&state, &new, next_or_me(&f.next))),
        Err(e) if e.code() == Code::Unauthorized => {
            // 아직 쓸 수 있으면 다시 묻고, 끝났으면(만료·횟수 다 씀) 처음부터.
            match totp::peek_challenge(&state.ctx, &raw).await {
                Ok(true) => {
                    totp_page(
                        &state,
                        &headers,
                        StatusCode::UNAUTHORIZED,
                        &f.next,
                        Some(e.message()),
                    )
                    .await
                }
                Ok(false) => totp_expired(&state, &headers, Some(e.message())).await,
                Err(e) => crate::error::page(&state, &e).await,
            }
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
        Ok(login) => finish_login(&state, &login, "/me?done=password"),
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
        Ok(login) => finish_login(&state, &login, "/me?done=verified"),
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

/// `/me/bar` 의 `hidden_members` 최대 수
const MAX_HIDDEN_MEMBERS: usize = 1000;
const MAX_FAVORITE_BOARDS: usize = 30;

/// 공개 화면 윗줄 회원 막대 (ADR-0081). 공개 HTML 은 모두에게 같아 캐시하므로
/// 로그인한 사람의 이름·알림 수는 여기서 따로 준다. `no-store` 는 이 라우터 전체에 걸려 있다.
async fn account_bar(State(state): State<AppState>, headers: HeaderMap) -> Response {
    let who = match crate::admin::current(&state, &headers).await {
        Ok(w) => w,
        Err(e) => return crate::error::page(&state, &e).await,
    };
    let Some(who) = who else {
        let site = settings::site(&state.ctx).await.unwrap_or_default();
        let signup = (site.signup == SignupMode::Open).then_some("/signup");
        return axum::Json(json!({
            "signed_in": false,
            "login_url": "/login",
            "signup_url": signup,
        }))
        .into_response();
    };
    let slots = crate::modules::bar_slots(&state, &who.actor).await;
    // 회원 이름 메뉴 항목: 사이트 안 주소에 이름을 붙여 쓴다 (ADR-0082)
    let action_ok = |a: &&Value| {
        a["label"].is_string()
            && a["url"]
                .as_str()
                .is_some_and(|u| u.starts_with('/') && !u.starts_with("//"))
    };
    // 화면에서 접을 회원 이름 (GAP Q18, ADR-0103). 모듈이 준 것을 모아 겹친 것을 뺀다.
    let mut hidden: Vec<&str> = slots
        .iter()
        .filter_map(|s| s["data"]["hide_members"].as_array())
        .flatten()
        .filter_map(Value::as_str)
        .filter(|n| !n.is_empty())
        .collect();
    hidden.sort_unstable();
    hidden.dedup();
    hidden.truncate(MAX_HIDDEN_MEMBERS);
    // 즐겨찾는 게시판과 별 단추 주소 (GAP Q26, ADR-0108). 사이트 안 주소만.
    let site_url = |u: &str| u.starts_with('/') && !u.starts_with("//");
    let favorite_boards: Vec<Value> = slots
        .iter()
        .filter_map(|s| s["data"]["favorite_boards"].as_array())
        .flatten()
        .filter(|b| b["name"].is_string() && b["url"].as_str().is_some_and(site_url))
        .take(MAX_FAVORITE_BOARDS)
        .map(|b| json!({ "name": b["name"], "url": b["url"] }))
        .collect();
    let board_star = slots
        .iter()
        .find_map(|s| s["data"]["board_star"].as_str().filter(|u| site_url(u)));
    let items: Vec<Value> = slots
        .iter()
        .filter_map(|s| {
            let d = &s["data"];
            Some(json!({
                "module": s["module"],
                "label": d["label"].as_str()?,
                "url": d["url"].as_str()?,
                "count": d["count"].as_i64().unwrap_or(0),
                "member_action": d.get("member_action").filter(action_ok),
                "member_actions": d
                    .get("member_action")
                    .into_iter()
                    .chain(d["member_actions"].as_array().into_iter().flatten())
                    .filter(action_ok)
                    .collect::<Vec<_>>(),
                // 막대에서 펼쳐 보는 목록 주소(사이트 안 JSON), 이름 메뉴로 갈 항목, 덧붙일 한 줄 (GAP Q14, ADR-0099)
                "panel": d["panel"].as_str().filter(|u| u.starts_with('/') && !u.starts_with("//")),
                "menu": d["menu"].as_bool().unwrap_or(false),
                "note": d["note"].as_str(),
            }))
        })
        .collect();
    axum::Json(json!({
        "signed_in": true,
        "name": who.member.display_name,
        "me_url": "/me",
        "profile_url": urls::path(&Target::Member { name: &who.member.display_name }),
        "comments_url": "/me/comments",
        // 이름 메뉴의 로그아웃 폼. 같은 사이트 스크립트만 읽는 응답이다(no-store, CORS 없음).
        "logout": { "url": "/logout", "csrf": who.session.csrf_token },
        "items": items,
        "hidden_members": hidden,
        "favorite_boards": favorite_boards,
        "board_star": board_star,
    }))
    .into_response()
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
        "profile" => Some("닉네임과 자기소개를 저장했습니다."),
        "password" => Some("비밀번호를 바꿨습니다. 다른 기기의 로그인은 모두 끊었습니다."),
        "verified" => Some("메일 인증이 끝났습니다. 환영합니다."),
        "totp_start" => Some("인증 앱에 아래 값을 넣고, 앱이 보여 주는 6자리 코드로 확인하세요."),
        "totp_off" => Some("2단계 인증을 껐습니다."),
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
    me_page_with(state, who, status, notice, error, None).await
}

/// `codes` 는 방금 만든 복구 코드 (한 번만 보인다).
async fn me_page_with(
    state: &AppState,
    who: &SignedIn,
    status: StatusCode,
    notice: Option<&str>,
    error: Option<(&str, String)>,
    codes: Option<Vec<String>>,
) -> Response {
    let site = settings::site(&state.ctx).await.unwrap_or_default();
    let two_factor = match totp::status(&state.ctx, who).await {
        Ok(t) => t,
        Err(e) => return crate::error::page(state, &e).await,
    };
    let setup = two_factor.setup.as_ref().map(|s| {
        let grouped = s
            .secret
            .as_bytes()
            .chunks(4)
            .map(|c| String::from_utf8_lossy(c).into_owned())
            .collect::<Vec<_>>()
            .join(" ");
        json!({ "secret": grouped, "uri": s.uri, "qr": totp_qr_svg(&s.uri) })
    });
    let posts = my_posts(state, who, site.tz_offset_min).await;
    let comments = my_comments(state, who, site.tz_offset_min, PageReq::new(1, 5)).await;
    let (error_in, error) = error.map_or((None, None), |(k, m)| (Some(k), Some(m)));
    let ctx = json!({
        "site": crate::public::site_json(state).await,
        "meta": { "title": format!("내 정보 · {}", site.name), "noindex": true },
        "me": {
            "csrf": who.session.csrf_token,
            "email": who.member.email,
            "display_name": who.member.display_name,
            "bio": state.ctx.store.member_bio(&who.member.id).await.ok().flatten().unwrap_or_default(),
            "max_bio": members::MAX_BIO_CHARS,
            "signature": state.ctx.store.member_signature(&who.member.id).await.ok().flatten().unwrap_or_default(),
            "max_signature": members::MAX_SIGNATURE_CHARS,
            "max_signature_lines": members::MAX_SIGNATURE_LINES,
            "profile_url": maru_core::urls::path(&maru_core::urls::Target::Member { name: &who.member.display_name }),
            "joined": clock::display_date(&who.member.created_at, site.tz_offset_min),
            "is_owner": who.actor.has(Permission::SiteAdmin),
            "admin_url": who.actor.has(Permission::PostModerate).then_some("/admin"),
            "posts": posts,
            "comments": comments.as_ref().map(|(items, _)| items.clone()).unwrap_or_default(),
            "comments_total": comments.map_or(0, |(_, l)| l.total),
            "notice": notice,
            "error": error,
            "error_in": error_in,
            "min_password": auth::MIN_PASSWORD_CHARS,
            "totp": {
                "enabled": two_factor.enabled,
                "setup": setup,
                "recovery_left": two_factor.recovery_left,
                "codes": codes,
            },
        },
        "slots": crate::modules::me_slots(state, &who.actor).await,
    });
    render(state, status, "me.html", &ctx).await
}

#[derive(Debug, Default, Deserialize)]
struct PageQuery {
    #[serde(default)]
    page: Option<u32>,
}

/// 내가 쓴 댓글 전부, 20개씩 (GAP M3).
async fn my_comments_page(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<PageQuery>,
) -> Response {
    let who = match require(&state, &headers).await {
        Ok(w) => w,
        Err(res) => return *res,
    };
    let site = settings::site(&state.ctx).await.unwrap_or_default();
    let page = PageReq::new(q.page.unwrap_or(1), 20);
    let Some((items, list)) = my_comments(&state, &who, site.tz_offset_min, page).await else {
        return crate::error::page(&state, &Error::new(Code::Internal)).await;
    };
    let ctx = json!({
        "site": crate::public::site_json(&state).await,
        "meta": { "title": format!("내가 쓴 댓글 · {}", site.name), "noindex": true },
        "me": { "display_name": who.member.display_name, "total": list.total, "comments": items },
        "pagination": crate::public::pagination(&list, "/me/comments"),
    });
    render(&state, StatusCode::OK, "me_comments.html", &ctx).await
}

/// 내가 쓴 댓글을 화면용으로. 본문은 한 줄 80자로 줄인다.
async fn my_comments(
    state: &AppState,
    who: &SignedIn,
    tz: i32,
    page: PageReq,
) -> Option<(Vec<Value>, maru_core::model::Listing<comments::MyComment>)> {
    let list = comments::mine(&state.ctx, &who.actor, page).await.ok()?;
    let items = list
        .items
        .iter()
        .map(|c| {
            let line = c.body.split_whitespace().collect::<Vec<_>>().join(" ");
            let short: String = line.chars().take(80).collect();
            let excerpt = if short.len() < line.len() {
                format!("{short}…")
            } else {
                short
            };
            let status = match c.status {
                CommentStatus::Published => None,
                CommentStatus::Pending => Some("승인 대기"),
                CommentStatus::Hidden => Some("숨김"),
                CommentStatus::Spam => Some("스팸 처리"),
                CommentStatus::Deleted => Some("지움"),
            };
            json!({
                "excerpt": excerpt,
                "post_title": c.post_title,
                "board": c.board_name,
                "status": status,
                "date": clock::display_date(&c.created_at, tz),
                "url": c.url,
            })
        })
        .collect();
    Some((items, list))
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
    #[serde(default)]
    bio: String,
    /// 없으면(옛 폼) 서명은 그대로 둔다
    #[serde(default)]
    signature: Option<String>,
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
        bio: Some(f.bio),
        signature: f.signature,
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

// ---- 2단계 인증 ----

#[derive(Debug, Default, Deserialize)]
struct CodeForm {
    #[serde(default, rename = "_csrf")]
    csrf: String,
    #[serde(default)]
    code: String,
}

/// `/me/totp/{start,enable,disable,recovery}`
async fn totp_action(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(action): Path<String>,
    Form(f): Form<CodeForm>,
) -> Response {
    let who = match require(&state, &headers).await {
        Ok(w) => w,
        Err(res) => return *res,
    };
    if !session_csrf_ok(&who, &f.csrf) {
        return csrf_failed(&state, &headers).await;
    }
    let ctx = &state.ctx;
    let res = match action.as_str() {
        "start" => totp::start(ctx, &who)
            .await
            .map(|_| Redirect::to("/me?done=totp_start#me-totp").into_response()),
        "enable" => match totp::enable(ctx, &who, &f.code).await {
            Ok(done) => {
                // 다른 기기의 로그인은 끊겼다. 새 세션으로 그리고 쿠키를 바꾼다.
                let fresh = SignedIn {
                    actor: who.actor.clone(),
                    session: done.session.session.clone(),
                    member: done.session.member.clone(),
                };
                let mut res = me_page_with(
                    &state,
                    &fresh,
                    StatusCode::OK,
                    Some("2단계 인증을 켰습니다. 다른 기기의 로그인은 모두 끊었습니다."),
                    None,
                    Some(done.recovery_codes),
                )
                .await;
                let age = auth::SESSION_DAYS * 24 * 60 * 60;
                res.headers_mut().append(
                    header::SET_COOKIE,
                    set_cookie(&state, SESSION_COOKIE, &done.session.secret, Some(age)),
                );
                Ok(res)
            }
            Err(e) => Err(e),
        },
        "disable" => totp::disable(ctx, &who, &f.code)
            .await
            .map(|()| Redirect::to("/me?done=totp_off").into_response()),
        "recovery" => match totp::regenerate(ctx, &who, &f.code).await {
            Ok(codes) => Ok(me_page_with(
                &state,
                &who,
                StatusCode::OK,
                Some("복구 코드를 새로 만들었습니다. 이전 코드는 이제 쓸 수 없습니다."),
                None,
                Some(codes),
            )
            .await),
            Err(e) => Err(e),
        },
        _ => return crate::error::page(&state, &Error::new(Code::NotFound)).await,
    };
    match res {
        Ok(r) => r,
        Err(e) if matches!(e.code(), Code::Unauthorized | Code::Conflict) => {
            me_page(
                &state,
                &who,
                StatusCode::BAD_REQUEST,
                None,
                Some(("totp", e.message())),
            )
            .await
        }
        Err(e) => crate::error::page(&state, &e).await,
    }
}

/// 인증 앱 등록용 QR 그림(SVG). 외부 QR 서비스에 비밀 키를 보내지 않으려고 서버에서 그린다.
/// 그림 안에는 경로 좌표만 있고 주소 글자는 들어가지 않는다. 실패하면 키 직접 입력만 보인다.
fn totp_qr_svg(uri: &str) -> Option<String> {
    let code = qrcode::QrCode::with_error_correction_level(uri, qrcode::EcLevel::M).ok()?;
    let svg = code
        .render::<qrcode::render::svg::Color>()
        .min_dimensions(180, 180)
        .dark_color(qrcode::render::svg::Color("#000000"))
        .light_color(qrcode::render::svg::Color("#ffffff"))
        .build();
    // 문서 안에 넣으므로 XML 선언은 뺀다
    let start = svg.find("<svg")?;
    Some(svg[start..].replacen("<svg ", "<svg aria-hidden=\"true\" focusable=\"false\" ", 1))
}
