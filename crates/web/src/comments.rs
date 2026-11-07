//! 공개 사이트의 댓글 쓰기·확인 질문·고치기·지우기 (GAP M5·M6·M10, ADR-0052).
//!
//! - 글 화면은 손님으로 렌더한다(ETag 유지). 그래서 댓글 폼에는 화면별 CSRF 값이 없다.
//!   세션 쿠키는 SameSite=Lax 라 다른 사이트의 POST 에 실리지 않고, 여기서 `Sec-Fetch-Site`·`Origin`
//!   으로 같은 출처인지 한 번 더 본다. 손님 요청은 신원이 없어 CSRF 대상이 아니고 확인 질문이 막는다.
//! - 손님이 쓰면 확인 질문 화면(`comment.html`, no-store)을 한 번 거친다. 글 화면 캐시를 깨지 않으려고.
//! - 고치기·지우기 화면은 진짜 세션으로 렌더한다. 로그인했으면 세션 CSRF, 아니면 로그인 전 CSRF 쿠키.

use axum::body::Bytes;
use axum::extract::{Form, Path, Query, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::middleware;
use axum::response::{Html, IntoResponse, Redirect, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use maru_core::Actor;
use maru_core::output::Success;
use maru_core::services::comments::{self, CommentRules, Editable, GuestAuthor, NewComment};
use maru_core::services::{anonymous_actor, captcha, posts, settings};
use maru_core::urls::{self, Target};
use maru_core::{Code, Error};
use maru_themes::tr;
use serde::Deserialize;
use serde_json::{Value, json};

use crate::AppState;
use crate::admin::{current, pre_csrf, pre_csrf_ok, session_csrf_ok};
use crate::error::ApiError;

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/b/{board}/{post}/comments", post(create))
        .route(
            "/write/{board}/comment-image",
            get(image_allowed)
                .post(attach_image)
                .layer(crate::upload_limit()),
        )
        .route("/c/{id}/edit", get(edit_form).post(edit))
        .route("/c/{id}/delete", get(delete_form).post(delete))
        .route("/c/{id}/accept", get(accept_form).post(accept))
        .layer(middleware::map_response(no_store))
}

pub(crate) async fn no_store(mut res: Response) -> Response {
    let h = res.headers_mut();
    h.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    h.insert(
        header::REFERRER_POLICY,
        HeaderValue::from_static("same-origin"),
    );
    res
}

/// 브라우저가 알려 준 출처가 이 사이트인가. 알려 주지 않는 오래된 브라우저는 통과시킨다
/// (그때도 세션 쿠키는 SameSite=Lax 라 다른 사이트의 POST 에 실리지 않는다).
pub(crate) fn same_origin(headers: &HeaderMap) -> bool {
    if let Some(site) = headers.get("sec-fetch-site").and_then(|v| v.to_str().ok()) {
        return matches!(site, "same-origin" | "none");
    }
    let Some(origin) = headers.get(header::ORIGIN).and_then(|v| v.to_str().ok()) else {
        return true;
    };
    let host = headers
        .get(header::HOST)
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default();
    origin
        .strip_prefix("http://")
        .or_else(|| origin.strip_prefix("https://"))
        .is_some_and(|o| !host.is_empty() && o == host)
}

#[derive(Debug, Default, Deserialize)]
struct CommentForm {
    #[serde(default)]
    body: String,
    /// 올려 둔 그림 주소 (ADR-0116)
    #[serde(default)]
    image: String,
    #[serde(default)]
    reply_to: String,
    #[serde(default)]
    name: String,
    #[serde(default)]
    password: String,
    /// 사람에게는 보이지 않는 함정 칸. 채워 오면 봇이다
    #[serde(default)]
    website: String,
    #[serde(default)]
    captcha_token: String,
    #[serde(default)]
    captcha_answer: String,
}

pub(crate) async fn viewer(
    state: &AppState,
    headers: &HeaderMap,
) -> Result<(Actor, Option<maru_core::services::auth::SignedIn>), Error> {
    match current(state, headers).await? {
        Some(who) => Ok((who.actor.clone(), Some(who))),
        None => Ok((anonymous_actor(&state.ctx).await?, None)),
    }
}

async fn create(
    State(state): State<AppState>,
    Path((board_slug, post_slug)): Path<(String, String)>,
    headers: HeaderMap,
    Form(f): Form<CommentForm>,
) -> Response {
    if !same_origin(&headers) || !f.website.is_empty() {
        return refused(&state, &headers).await;
    }
    let (actor, who) = match viewer(&state, &headers).await {
        Ok(v) => v,
        Err(e) => return crate::error::page(&state, &e).await,
    };
    let (board, post) = match posts::public_post(&state.ctx, &actor, &board_slug, &post_slug).await
    {
        Ok(v) => v,
        Err(e) => return crate::error::page(&state, &e).await,
    };
    let site = settings::site(&state.ctx).await.unwrap_or_default();
    let rules = comments::rules(&board, site.captcha);
    let back = urls::path(&Target::Post {
        board: &board.slug,
        post: &post.slug,
    });
    let guest = who.is_none();
    if guest && !rules.guest {
        return members_only(&state, &headers, &back).await;
    }
    // 손님은 확인 질문을 한 번 거친다.
    if guest && rules.captcha && f.captcha_token.is_empty() {
        return compose(
            &state,
            &headers,
            StatusCode::OK,
            &back,
            &post.title,
            &rules,
            &f,
            None,
        )
        .await;
    }
    let input = NewComment {
        post_id: post.id.clone(),
        reply_to: Some(f.reply_to.trim().to_owned()).filter(|s| !s.is_empty()),
        body: f.body.clone(),
        image: Some(f.image.clone()),
        guest: guest.then(|| GuestAuthor {
            name: f.name.clone(),
            password: f.password.clone(),
        }),
        captcha: (guest && rules.captcha).then(|| captcha::Answer {
            token: f.captcha_token.clone(),
            answer: f.captcha_answer.clone(),
        }),
    };
    match comments::create(&state.ctx, &actor, input).await {
        Ok(c) if c.status == maru_core::model::CommentStatus::Published => {
            Redirect::to(&format!("{back}#comment-{}", c.id)).into_response()
        }
        Ok(_) => Redirect::to(&format!("{back}?comment=pending#comments")).into_response(),
        Err(e) if e.code() == Code::Unauthorized => members_only(&state, &headers, &back).await,
        Err(e)
            if matches!(
                e.code(),
                Code::Validation | Code::Forbidden | Code::NotFound | Code::RateLimited
            ) =>
        {
            let status =
                StatusCode::from_u16(e.code().http_status()).unwrap_or(StatusCode::BAD_REQUEST);
            compose(
                &state,
                &headers,
                status,
                &back,
                &post.title,
                &rules,
                &f,
                Some(e.message()),
            )
            .await
        }
        Err(e) => crate::error::page(&state, &e).await,
    }
}

#[derive(Debug, Deserialize)]
struct ImageQuery {
    #[serde(default)]
    filename: String,
}

/// 글 화면은 손님 기준으로 같아서(ETag), 칸을 보일지는 스크립트가 여기에 묻는다 (ADR-0116).
async fn image_allowed(
    State(state): State<AppState>,
    Path(board): Path<String>,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    let (actor, _) = viewer(&state, &headers).await?;
    let allowed = comments::may_attach_image(&state.ctx, &actor, &board).await?;
    Ok(Json(Success::new(json!({ "allowed": allowed }))).into_response())
}

/// 댓글에 붙일 그림을 올린다. 글 화면에는 CSRF 값이 없어 댓글 쓰기처럼 같은 출처만 본다.
async fn attach_image(
    State(state): State<AppState>,
    Path(board): Path<String>,
    Query(q): Query<ImageQuery>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Response, ApiError> {
    if !same_origin(&headers) {
        return Err(ApiError(
            Error::new(Code::Forbidden).with_message("error.csrf"),
        ));
    }
    let (actor, _) = viewer(&state, &headers).await?;
    let out = comments::attach_image(&state.ctx, &actor, &board, q.filename, body.to_vec()).await?;
    Ok((StatusCode::CREATED, Json(Success::new(out))).into_response())
}

/// 댓글 폼을 다시 그린다: 손님 확인 질문, 또는 오류와 함께 적은 내용 그대로.
#[allow(clippy::too_many_arguments)]
async fn compose(
    state: &AppState,
    headers: &HeaderMap,
    status: StatusCode,
    back: &str,
    title: &str,
    rules: &CommentRules,
    f: &CommentForm,
    error: Option<String>,
) -> Response {
    let guest = current(state, headers).await.ok().flatten().is_none();
    let challenge = if guest && rules.captcha {
        match captcha::issue(&state.ctx).await {
            Ok(c) => Some(c),
            Err(e) => return crate::error::page(state, &e).await,
        }
    } else {
        None
    };
    let view = json!({
        "view": "compose",
        "action": format!("{back}/comments"),
        "back": back,
        "post_title": title,
        "guest": guest,
        "body": f.body,
        "image": f.image,
        "reply_to": f.reply_to,
        "name": f.name,
        // 같은 요청의 답으로만 돌려준다 (no-store). 손님이 다시 치지 않게.
        "password": f.password,
        "captcha": challenge,
        "max_chars": rules.max_chars,
        "error": error,
    });
    page(state, headers, status, tr("댓글 쓰기"), view, None).await
}

async fn members_only(state: &AppState, headers: &HeaderMap, back: &str) -> Response {
    let login = urls::with_query("/login", &[("next", back)]);
    let view = json!({
        "view": "notice",
        "heading": tr("회원만 댓글을 쓸 수 있습니다"),
        "body": tr("로그인한 뒤 다시 써 주세요."),
        "links": [{ "label": tr("로그인"), "href": login }, { "label": tr("글로 돌아가기"), "href": back }],
    });
    page(
        state,
        headers,
        StatusCode::UNAUTHORIZED,
        tr("댓글"),
        view,
        None,
    )
    .await
}

async fn refused(state: &AppState, headers: &HeaderMap) -> Response {
    let view = json!({
        "view": "notice",
        "heading": tr("요청을 확인하지 못했습니다"),
        "body": tr("다른 곳에서 보낸 요청이거나 자동 입력으로 보입니다. 글 화면에서 다시 써 주세요."),
        "links": [],
    });
    page(
        state,
        headers,
        StatusCode::FORBIDDEN,
        tr("댓글"),
        view,
        None,
    )
    .await
}

/// `comment.html` 한 장. 로그인했으면 세션 CSRF, 아니면 로그인 전 CSRF 쿠키를 쓴다.
async fn page(
    state: &AppState,
    headers: &HeaderMap,
    status: StatusCode,
    title: &str,
    mut view: Value,
    session_csrf: Option<&str>,
) -> Response {
    let mut set = None;
    let csrf = match session_csrf {
        Some(c) => c.to_owned(),
        None => match pre_csrf(state, headers) {
            Ok((c, s)) => {
                set = s;
                c
            }
            Err(e) => return crate::error::page(state, &e).await,
        },
    };
    view["csrf"] = json!(csrf);
    let site = settings::site(&state.ctx).await.unwrap_or_default();
    let ctx = json!({
        "site": crate::public::site_json(state).await,
        "meta": { "title": format!("{title} · {}", site.name), "noindex": true },
        "comment": view,
    });
    let mut res = match state.theme.render("comment.html", &ctx) {
        Ok(html) => (status, Html(html)).into_response(),
        Err(e) => return crate::error::page(state, &e).await,
    };
    if let Some(set) = set {
        res.headers_mut().append(header::SET_COOKIE, set);
    }
    res
}

// ---- 고치기·지우기 ----

#[derive(Debug, Default, Deserialize)]
struct EditForm {
    #[serde(default, rename = "_csrf")]
    csrf: String,
    #[serde(default)]
    body: String,
    /// 없으면 그대로, 비어 있으면 뺀다 (ADR-0116)
    #[serde(default)]
    image: Option<String>,
    #[serde(default)]
    password: String,
}

fn post_url(e: &Editable) -> String {
    urls::path(&Target::Post {
        board: &e.board_slug,
        post: &e.post_slug,
    })
}

async fn edit_form(
    State(state): State<AppState>,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> Response {
    form_page(&state, &headers, &id, "edit", StatusCode::OK, None, None).await
}

async fn delete_form(
    State(state): State<AppState>,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> Response {
    form_page(&state, &headers, &id, "delete", StatusCode::OK, None, None).await
}

async fn form_page(
    state: &AppState,
    headers: &HeaderMap,
    id: &str,
    mode: &str,
    status: StatusCode,
    body: Option<&str>,
    error: Option<String>,
) -> Response {
    let (actor, who) = match viewer(state, headers).await {
        Ok(v) => v,
        Err(e) => return crate::error::page(state, &e).await,
    };
    let e = match comments::editable(&state.ctx, &actor, id).await {
        Ok(e) => e,
        Err(err) if err.code() == Code::Unauthorized => {
            let next = format!("/c/{id}/{mode}");
            return Redirect::to(&urls::with_query("/login", &[("next", &next)])).into_response();
        }
        Err(err) => return crate::error::page(state, &err).await,
    };
    let may_attach = comments::may_attach_image(&state.ctx, &actor, &e.board_slug)
        .await
        .unwrap_or(false);
    let view = json!({
        "view": mode,
        "image": e.comment.image,
        "image_url": may_attach.then(|| format!("/write/{}/comment-image", e.board_slug)),
        "action": format!("/c/{id}/{mode}"),
        "back": format!("{}#comment-{id}", post_url(&e)),
        "body": body.unwrap_or(&e.comment.body),
        "author_name": e.comment.author_name,
        "needs_password": !e.direct,
        "max_chars": comments::MAX_BODY_CHARS,
        "error": error,
    });
    let heading = if mode == "edit" {
        tr("댓글 고치기")
    } else {
        tr("댓글 지우기")
    };
    let csrf = who.as_ref().map(|w| w.session.csrf_token.clone());
    page(state, headers, status, heading, view, csrf.as_deref()).await
}

// ---- 답변 채택 (ADR-0058) ----

#[derive(Debug, Default, Deserialize)]
struct AcceptForm {
    #[serde(default, rename = "_csrf")]
    csrf: String,
}

async fn accept_form(
    State(state): State<AppState>,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> Response {
    accept_page(&state, &headers, &id, StatusCode::OK, None).await
}

async fn accept_page(
    state: &AppState,
    headers: &HeaderMap,
    id: &str,
    status: StatusCode,
    error: Option<String>,
) -> Response {
    let (actor, who) = match viewer(state, headers).await {
        Ok(v) => v,
        Err(e) => return crate::error::page(state, &e).await,
    };
    let Some(who) = who else {
        let next = format!("/c/{id}/accept");
        return Redirect::to(&urls::with_query("/login", &[("next", &next)])).into_response();
    };
    let t = match comments::accept_target(&state.ctx, &actor, id).await {
        Ok(t) => t,
        Err(e) => return crate::error::page(state, &e).await,
    };
    let back = urls::path(&Target::Post {
        board: &t.board_slug,
        post: &t.post_slug,
    });
    let view = json!({
        "view": "accept",
        "action": format!("/c/{id}/accept"),
        "back": format!("{back}#comment-{id}"),
        "post_title": t.post_title,
        "author_name": t.comment.author_name,
        "body": t.comment.body,
        "accepted": t.accepted,
        "error": error,
    });
    let heading = if t.accepted {
        tr("채택 취소")
    } else {
        tr("답변 채택")
    };
    page(
        state,
        headers,
        status,
        heading,
        view,
        Some(&who.session.csrf_token),
    )
    .await
}

async fn accept(
    State(state): State<AppState>,
    Path(id): Path<String>,
    headers: HeaderMap,
    Form(f): Form<AcceptForm>,
) -> Response {
    let (actor, who) = match viewer(&state, &headers).await {
        Ok(v) => v,
        Err(e) => return crate::error::page(&state, &e).await,
    };
    let Some(w) = who.as_ref() else {
        return refused(&state, &headers).await;
    };
    if !session_csrf_ok(w, &f.csrf) || !same_origin(&headers) {
        return refused(&state, &headers).await;
    }
    let t = match comments::accept_target(&state.ctx, &actor, &id).await {
        Ok(t) => t,
        Err(e) => return crate::error::page(&state, &e).await,
    };
    let target = (!t.accepted).then_some(id.as_str());
    match posts::accept(&state.ctx, &actor, &t.post_id, target).await {
        Ok(_) => {
            let back = urls::path(&Target::Post {
                board: &t.board_slug,
                post: &t.post_slug,
            });
            Redirect::to(&format!("{back}#comment-{id}")).into_response()
        }
        Err(e) if matches!(e.code(), Code::Validation | Code::Forbidden) => {
            let status =
                StatusCode::from_u16(e.code().http_status()).unwrap_or(StatusCode::BAD_REQUEST);
            accept_page(&state, &headers, &id, status, Some(e.message())).await
        }
        Err(e) => crate::error::page(&state, &e).await,
    }
}

fn csrf_ok(
    headers: &HeaderMap,
    who: Option<&maru_core::services::auth::SignedIn>,
    sent: &str,
) -> bool {
    match who {
        Some(w) => session_csrf_ok(w, sent),
        None => pre_csrf_ok(headers, sent),
    }
}

async fn edit(
    State(state): State<AppState>,
    Path(id): Path<String>,
    headers: HeaderMap,
    Form(f): Form<EditForm>,
) -> Response {
    let (actor, who) = match viewer(&state, &headers).await {
        Ok(v) => v,
        Err(e) => return crate::error::page(&state, &e).await,
    };
    if !csrf_ok(&headers, who.as_ref(), &f.csrf) {
        return refused(&state, &headers).await;
    }
    let pw = Some(f.password.as_str()).filter(|p| !p.is_empty());
    match comments::update_with_image(&state.ctx, &actor, &id, &f.body, f.image.as_deref(), pw)
        .await
    {
        Ok(_) => match comments::editable(&state.ctx, &actor, &id).await {
            Ok(e) => Redirect::to(&format!("{}#comment-{id}", post_url(&e))).into_response(),
            // 비밀번호로 고친 손님은 다시 열 권한이 없다. 그래도 고쳐졌다.
            Err(_) => Redirect::to("/").into_response(),
        },
        Err(e) if matches!(e.code(), Code::Validation | Code::Forbidden) => {
            let status =
                StatusCode::from_u16(e.code().http_status()).unwrap_or(StatusCode::BAD_REQUEST);
            form_page(
                &state,
                &headers,
                &id,
                "edit",
                status,
                Some(&f.body),
                Some(e.message()),
            )
            .await
        }
        Err(e) => crate::error::page(&state, &e).await,
    }
}

async fn delete(
    State(state): State<AppState>,
    Path(id): Path<String>,
    headers: HeaderMap,
    Form(f): Form<EditForm>,
) -> Response {
    let (actor, who) = match viewer(&state, &headers).await {
        Ok(v) => v,
        Err(e) => return crate::error::page(&state, &e).await,
    };
    if !csrf_ok(&headers, who.as_ref(), &f.csrf) {
        return refused(&state, &headers).await;
    }
    // 지우기 전에 돌아갈 글 주소를 알아 둔다.
    let back = match comments::editable(&state.ctx, &actor, &id).await {
        Ok(e) => post_url(&e),
        Err(e) => return crate::error::page(&state, &e).await,
    };
    let pw = Some(f.password.as_str()).filter(|p| !p.is_empty());
    match comments::delete(&state.ctx, &actor, &id, pw).await {
        Ok(_) => Redirect::to(&format!("{back}#comments")).into_response(),
        Err(e) if e.code() == Code::Forbidden => {
            form_page(
                &state,
                &headers,
                &id,
                "delete",
                StatusCode::FORBIDDEN,
                None,
                Some(e.message()),
            )
            .await
        }
        Err(e) => crate::error::page(&state, &e).await,
    }
}
