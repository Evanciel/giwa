//! Maru 공개 사이트, REST API, 관리 화면
//!
//! 모든 핸들러는 `maru_core::services` 만 부른다. 권한 판단은 서비스 레이어가 한다.

mod account;
mod admin;
mod api;
mod board_write;
mod comments;
mod error;
mod etag;
mod feeds;
mod field_form;
mod mcp;
mod modules;
mod public;
mod ratelimit;
mod widgets;

use std::future::Future;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;

use axum::Router;
use axum::extract::{ConnectInfo, Request, State};
use axum::http::{HeaderMap, HeaderValue, Method, StatusCode, header};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use maru_core::Actor;
use maru_core::actor::Permission;
use maru_core::model::Media;
use maru_core::services::backups::Archiver;
use maru_core::services::links::{self, LinkFetcher};
use maru_core::services::mail::{self, Mailer};
use maru_core::services::stats::{self, StatCounter};
use maru_core::services::views::{self, ViewCounter};
use maru_core::services::{auth, fonts, media, posts, retention, signals};
use maru_core::{Code, Ctx, Error, Result};
use maru_modules::Registry;
use maru_themes::{Admin, Theme};
use tokio::net::TcpListener;
use tokio::sync::Semaphore;
use tower_http::limit::RequestBodyLimitLayer;
use tower_http::timeout::TimeoutLayer;

pub use ratelimit::{Bucket, RateLimiter};

/// 요청 본문 상한. 가장 큰 업로드(서체 20MB) + 여유. JSON·폼 추출은 axum 기본값(2MB)이 따로 막는다.
const BODY_LIMIT: usize = fonts::MAX_BYTES + 256 * 1024;
/// 그림·첨부 업로드 경로의 상한. 이 경로들은 요청 머리(Content-Length)만 보고도 413 으로 막는다.
pub(crate) const UPLOAD_LIMIT: usize = media::MAX_BYTES + 256 * 1024;
/// 서체 파일 업로드 경로의 추출 상한.
pub(crate) const FONT_UPLOAD_LIMIT: usize = BODY_LIMIT;
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
/// 만료 세션·링크·요청 제한 키 정리 주기.
const PURGE_EVERY: Duration = Duration::from_secs(10 * 60);
/// 예약 발행 확인 주기. 예약 시각보다 최대 이만큼 늦게 발행된다.
const SCHEDULE_EVERY: Duration = Duration::from_secs(20);
/// 쌓인 편지를 배달하는 주기 (ADR-0051).
const MAIL_EVERY: Duration = Duration::from_secs(5);
/// 링크 카드 정보를 가져오는 주기 (ADR-0084). 한 번에 몇 개만.
const LINKS_EVERY: Duration = Duration::from_secs(10);
/// 모은 조회수를 DB 에 더하는 주기 (ADR-0049). 조회마다 쓰지 않는다.
const VIEWS_EVERY: Duration = Duration::from_secs(20);
/// 모듈 이벤트를 넘기는 주기.
const MODULES_EVERY: Duration = Duration::from_secs(2);
/// argon2 를 동시에 계산하는 최대 수.
const LOGIN_CONCURRENCY: usize = 2;
/// 이미지 디코딩·인코딩을 동시에 하는 최대 수.
const UPLOAD_CONCURRENCY: usize = 2;

#[derive(Clone)]
pub struct AppState {
    pub ctx: Ctx,
    pub theme: Arc<Theme>,
    pub admin: Arc<Admin>,
    pub limiter: Arc<RateLimiter>,
    pub login_gate: Arc<Semaphore>,
    pub upload_gate: Arc<Semaphore>,
    /// 조회수 계수기. 서버를 끌 때 남은 수를 반영한다.
    pub views: Arc<ViewCounter>,
    /// 방문자 통계 계수기 (ADR-0061). 조회수와 같은 주기에 반영한다.
    pub stats: Arc<StatCounter>,
    /// 편지 전송 방식. 없으면 편지는 편지함에 쌓이기만 한다.
    pub mailer: Option<Arc<dyn Mailer>>,
    /// 링크 카드용 외부 페이지 읽기 (ADR-0084). 없으면 카드는 주소와 도메인만 보인다.
    pub link_fetcher: Option<Arc<dyn LinkFetcher>>,
    /// 백업 파일 보관소 (ADR-0063). 없으면 백업 경로가 「쓸 수 없음」으로 답한다.
    pub archiver: Option<Arc<dyn Archiver>>,
    /// 이 바이너리에 든 모듈 (ADR-0066). 켜짐은 DB 에 있다.
    pub modules: Arc<Registry>,
}

impl AppState {
    pub fn new(ctx: Ctx, theme: Theme) -> Self {
        Self {
            ctx,
            theme: Arc::new(theme),
            admin: Arc::new(Admin::new()),
            limiter: Arc::new(RateLimiter::default()),
            login_gate: Arc::new(Semaphore::new(LOGIN_CONCURRENCY)),
            upload_gate: Arc::new(Semaphore::new(UPLOAD_CONCURRENCY)),
            views: Arc::new(ViewCounter::new()),
            stats: Arc::new(StatCounter::new()),
            mailer: None,
            link_fetcher: None,
            archiver: None,
            modules: Arc::new(Registry::default()),
        }
    }

    pub fn with_modules(mut self, modules: Arc<Registry>) -> Self {
        self.modules = modules;
        self
    }

    pub fn with_mailer(mut self, mailer: Arc<dyn Mailer>) -> Self {
        self.mailer = Some(mailer);
        self
    }

    pub fn with_link_fetcher(mut self, fetcher: Arc<dyn LinkFetcher>) -> Self {
        self.link_fetcher = Some(fetcher);
        self
    }

    pub fn with_archiver(mut self, archiver: Arc<dyn Archiver>) -> Self {
        self.archiver = Some(archiver);
        self
    }

    /// 백업 보관소. 없으면 내부 오류(서버를 이 저장소 없이 띄운 경우).
    pub(crate) fn archiver(&self) -> Result<&dyn Archiver> {
        self.archiver
            .as_deref()
            .ok_or_else(|| Error::internal("백업 보관소가 연결되지 않았습니다"))
    }
}

pub fn router(state: AppState) -> Router {
    Router::new()
        .merge(public::routes())
        .merge(admin::routes())
        .merge(account::routes())
        .merge(comments::routes())
        .merge(board_write::routes())
        .merge(modules::routes(&state))
        .merge(mcp::routes(&state))
        .nest("/api/v1", api::routes())
        .layer(middleware::from_fn_with_state(state.clone(), rate_limit))
        .layer(middleware::from_fn_with_state(state.clone(), count_visit))
        .with_state(state)
        .layer(middleware::map_response(security_headers))
        .layer(RequestBodyLimitLayer::new(BODY_LIMIT))
        .layer(TimeoutLayer::with_status_code(
            StatusCode::REQUEST_TIMEOUT,
            REQUEST_TIMEOUT,
        ))
}

/// 연결 주소로만 센다. 전달 헤더(`X-Forwarded-For`)는 보지 않는다.
async fn rate_limit(State(state): State<AppState>, req: Request, next: Next) -> Response {
    let ip = req
        .extensions()
        .get::<ConnectInfo<SocketAddr>>()
        .map(|c| c.0.ip())
        .unwrap_or(IpAddr::V4(Ipv4Addr::UNSPECIFIED));
    let path = req.uri().path();
    // 비밀번호·링크를 받는 폼은 따로 더 좁게 센다 (가입·찾기 포함, ADR-0051).
    let login = req.method() == Method::POST
        && (path == "/admin/login"
            || path.starts_with("/admin/link/")
            || matches!(
                path,
                "/login" | "/login/totp" | "/signup" | "/forgot" | "/me/password" | "/me/withdraw"
            )
            || path.starts_with("/me/totp/")
            || path.starts_with("/reset/")
            || path.starts_with("/verify/")
            || path.starts_with("/c/")
            || path.ends_with("/comments")
            || path.starts_with("/write/")
            || (path.starts_with("/b/") && (path.ends_with("/edit") || path.ends_with("/delete"))));
    // 모듈의 단추(반응·스크랩)는 로그인 시도처럼 좁힐 일은 아니지만 읽기보다는 좁힌다.
    let action = req.method() == Method::POST && path.starts_with("/m/");
    let allowed = state.limiter.check(ip, Bucket::All)
        && (!login || state.limiter.check(ip, Bucket::Login))
        && (!action || state.limiter.check(ip, Bucket::Action));
    if allowed {
        return next.run(req).await;
    }
    tracing::warn!(%ip, path, "요청 제한");
    let err = Error::new(Code::RateLimited);
    let mut res = if path.starts_with("/api/") {
        error::ApiError(err).into_response()
    } else {
        (
            StatusCode::TOO_MANY_REQUESTS,
            [(header::CONTENT_TYPE, "text/plain; charset=utf-8")],
            err.message(),
        )
            .into_response()
    };
    res.headers_mut()
        .insert(header::RETRY_AFTER, HeaderValue::from_static("60"));
    res
}

async fn security_headers(mut res: Response) -> Response {
    let h = res.headers_mut();
    h.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    h.entry(header::REFERRER_POLICY)
        .or_insert(HeaderValue::from_static("strict-origin-when-cross-origin"));
    h.entry(header::CONTENT_SECURITY_POLICY)
        .or_insert(HeaderValue::from_static(
            "default-src 'self'; img-src 'self' https: data:; style-src 'self'; \
         script-src 'self'; object-src 'none'; base-uri 'self'; frame-ancestors 'self'; \
         frame-src https://www.youtube-nocookie.com https://player.vimeo.com",
        ));
    res
}

/// 이미 바인딩된 리스너로 서버를 돌린다. `shutdown` 이 끝나면 진행 중인 요청을 마치고 멈춘다.
pub async fn serve(
    state: AppState,
    listener: TcpListener,
    shutdown: impl Future<Output = ()> + Send + 'static,
) -> Result<()> {
    let purge = tokio::spawn(purge_loop(state.clone()));
    let scheduled = tokio::spawn(schedule_loop(state.clone()));
    let counted = tokio::spawn(views_loop(state.clone()));
    let mailed = tokio::spawn(mail_loop(state.clone()));
    let fetched = tokio::spawn(links_loop(state.clone()));
    let delivered = tokio::spawn(modules_loop(state.clone()));
    let res = axum::serve(
        listener,
        router(state.clone()).into_make_service_with_connect_info::<SocketAddr>(),
    )
    .with_graceful_shutdown(shutdown)
    .await
    .map_err(maru_core::Error::internal);
    purge.abort();
    scheduled.abort();
    counted.abort();
    mailed.abort();
    fetched.abort();
    delivered.abort();
    flush_views(&state).await;
    res
}

/// 밀린 이벤트를 모듈에 넘긴다 (ADR-0066). 모듈이 없으면 아무것도 하지 않는다.
async fn modules_loop(state: AppState) {
    if state.modules.iter().next().is_none() {
        return;
    }
    let mut tick = tokio::time::interval(MODULES_EVERY);
    loop {
        tick.tick().await;
        match maru_modules::delivery::deliver_once(&state.ctx, &state.modules).await {
            Ok(0) => {}
            Ok(n) => tracing::debug!(events = n, "모듈 이벤트 배달"),
            Err(e) => tracing::warn!(error = ?e, "모듈 이벤트 배달 실패, 다음 주기에 다시 시도"),
        }
    }
}

/// 쌓인 편지를 주기마다 배달한다. 전송 방식이 없으면 아무것도 하지 않는다.
async fn mail_loop(state: AppState) {
    let Some(mailer) = state.mailer.clone() else {
        return;
    };
    let mut tick = tokio::time::interval(MAIL_EVERY);
    loop {
        tick.tick().await;
        match mail::deliver_pending(&state.ctx, mailer.as_ref()).await {
            Ok(r) if r.sent + r.retry + r.failed == 0 => {}
            Ok(r) => tracing::info!(
                sent = r.sent,
                retry = r.retry,
                failed = r.failed,
                kind = mailer.kind(),
                "편지 배달"
            ),
            Err(e) => tracing::warn!(error = ?e, "편지 배달 확인 실패"),
        }
    }
}

/// 링크 카드 정보를 주기마다 몇 개씩 가져온다. 읽을 곳이 없으면 아무것도 하지 않는다.
async fn links_loop(state: AppState) {
    let Some(fetcher) = state.link_fetcher.clone() else {
        return;
    };
    let mut tick = tokio::time::interval(LINKS_EVERY);
    loop {
        tick.tick().await;
        match links::fetch_pending(&state.ctx, fetcher.as_ref()).await {
            Ok(r) if r.ok + r.retry + r.failed == 0 => {}
            Ok(r) => tracing::info!(
                ok = r.ok,
                retry = r.retry,
                failed = r.failed,
                "링크 카드 가져오기"
            ),
            Err(e) => tracing::warn!(error = ?e, "링크 카드 가져오기 확인 실패"),
        }
    }
}

/// 모은 조회수를 주기마다 DB 에 더한다.
async fn views_loop(state: AppState) {
    let mut tick = tokio::time::interval(VIEWS_EVERY);
    tick.tick().await;
    loop {
        tick.tick().await;
        flush_views(&state).await;
    }
}

/// 남은 조회수와 방문자 통계를 지금 반영한다. 서버를 강제로 멈출 때도 부른다.
pub async fn flush_views(state: &AppState) {
    match views::flush(&state.ctx, &state.views).await {
        Ok(0) => {}
        Ok(n) => tracing::debug!(posts = n, "조회수 반영"),
        Err(e) => tracing::warn!(error = ?e, "조회수 반영 실패, 다음 주기에 다시 시도"),
    }
    match stats::flush(&state.ctx, &state.stats).await {
        Ok(0) => {}
        Ok(n) => tracing::debug!(days = n, "방문자 통계 반영"),
        Err(e) => tracing::warn!(error = ?e, "방문자 통계 반영 실패, 다음 주기에 다시 시도"),
    }
}

/// 이 앞으로 시작하는 경로는 공개 화면이 아니다 (통계에서 미리 거른다).
const NOT_PAGES: [&str; 7] = [
    "/admin",
    "/api/",
    "/assets/",
    "/media/",
    "/files/",
    "/fonts/",
    "/__preview",
];

/// 공개 화면 방문을 통계에 센다 (ADR-0061). `render` 가 표시를 단 200·304 응답만 센다.
async fn count_visit(State(state): State<AppState>, req: Request, next: Next) -> Response {
    let path = req.uri().path();
    if req.method() != Method::GET || NOT_PAGES.iter().any(|p| path.starts_with(p)) {
        return next.run(req).await;
    }
    let path = path.to_owned();
    let headers = req.headers().clone();
    let ip = req
        .extensions()
        .get::<ConnectInfo<SocketAddr>>()
        .map(|c| c.0.ip().to_string())
        .unwrap_or_default();
    let res = next.run(req).await;
    if res.extensions().get::<public::Counted>().is_some()
        && matches!(res.status(), StatusCode::OK | StatusCode::NOT_MODIFIED)
    {
        record_visit(&state, &path, &headers, &ip).await;
    }
    res
}

/// 봇·추적 거부(`DNT`·`Sec-GPC`)·미리 읽기·발행 권한이 있는 사람은 빼고 센다.
async fn record_visit(state: &AppState, path: &str, headers: &HeaderMap, ip: &str) {
    let h = |name: &str| {
        headers
            .get(name)
            .and_then(|v| v.to_str().ok())
            .unwrap_or_default()
    };
    let ua = h("user-agent");
    let prefetch = [h("sec-purpose"), h("purpose")]
        .iter()
        .any(|v| v.to_ascii_lowercase().contains("prefetch"));
    if views::is_bot(ua) || h("dnt").trim() == "1" || h("sec-gpc").trim() == "1" || prefetch {
        return;
    }
    if admin::viewer(state, headers)
        .await
        .is_some_and(|a| a.has(Permission::PostPublish))
    {
        return;
    }
    let site_host = state
        .ctx
        .site_url
        .split_once("://")
        .map_or("", |(_, rest)| rest.split('/').next().unwrap_or_default());
    let own = [h("host"), site_host];
    state.stats.record(&stats::Hit {
        path,
        referrer: h("referer"),
        own_hosts: &own,
        ip,
        user_agent: ua,
    });
}

/// 예약 시각이 지난 글을 발행한다. 서버를 켤 때 한 번, 그 뒤로 주기마다.
/// 꺼져 있던 동안 지난 예약도 켜자마자 발행된다.
async fn schedule_loop(state: AppState) {
    let mut tick = tokio::time::interval(SCHEDULE_EVERY);
    loop {
        tick.tick().await;
        match posts::publish_due(&state.ctx).await {
            Ok(results) => {
                for (id, res) in results {
                    match res {
                        Ok(p) => tracing::info!(post = %id, title = %p.title, "예약 글 발행"),
                        Err(e) => tracing::warn!(post = %id, error = ?e, "예약 발행 실패"),
                    }
                }
            }
            Err(e) => tracing::warn!(error = ?e, "예약 발행 확인 실패"),
        }
    }
}

async fn purge_loop(state: AppState) {
    let mut tick = tokio::time::interval(PURGE_EVERY);
    let mut retained_day = None;
    loop {
        tick.tick().await;
        // 보존 정리(휴면·편지·모듈 기록)는 하루 한 번. 서버를 켠 뒤 첫 바퀴에도 한다 (ADR-0073).
        let day = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_secs() / 86_400);
        if retained_day != Some(day) {
            match retention::run(&state.ctx, &Actor::local()).await {
                Ok(r) => {
                    retained_day = Some(day);
                    tracing::info!(
                        notified = r.notified,
                        dormant = r.dormant,
                        mails = r.mails_removed,
                        modules = r.module_removed,
                        "보존 정리"
                    );
                }
                Err(e) => tracing::warn!(error = ?e, "보존 정리 실패"),
            }
        }
        match auth::purge_expired(&state.ctx).await {
            Ok(n) if n > 0 => tracing::info!(removed = n, "만료된 세션·링크 정리"),
            Ok(_) => {}
            Err(e) => tracing::warn!(error = ?e, "세션 정리 실패"),
        }
        // 새 운영 신호는 관리자·소유자에게 메일로 (ADR-0074). 같은 신호는 한 번만.
        match signals::alert(&state.ctx).await {
            Ok(n) if n > 0 => tracing::info!(signals = n, "운영 신호 알림"),
            Ok(_) => {}
            Err(e) => tracing::warn!(error = ?e, "운영 신호 알림 실패"),
        }
        state.limiter.purge();
    }
}

/// 업로드는 CPU 를 오래 쓰므로 비동기 작업 스레드 밖에서, 동시에 몇 개까지만 돌린다.
/// REST 와 관리 화면이 같은 서비스(`media::upload`)를 이 경로로 부른다.
pub(crate) async fn upload_media(
    state: &AppState,
    actor: Actor,
    input: media::Upload,
) -> Result<Media> {
    let _permit = state
        .upload_gate
        .acquire()
        .await
        .map_err(maru_core::Error::internal)?;
    let ctx = state.ctx.clone();
    let handle = tokio::runtime::Handle::current();
    tokio::task::spawn_blocking(move || handle.block_on(media::upload(&ctx, &actor, input)))
        .await
        .map_err(maru_core::Error::internal)?
}

/// 사진 돌리기·자르기 (GAP E23). 업로드처럼 CPU 를 써서 같은 문을 지난다.
pub(crate) async fn transform_media(
    state: &AppState,
    actor: Actor,
    id: String,
    t: media::Transform,
) -> Result<Media> {
    let _permit = state
        .upload_gate
        .acquire()
        .await
        .map_err(maru_core::Error::internal)?;
    let ctx = state.ctx.clone();
    let handle = tokio::runtime::Handle::current();
    tokio::task::spawn_blocking(move || handle.block_on(media::transform(&ctx, &actor, &id, t)))
        .await
        .map_err(maru_core::Error::internal)?
}

/// 그림·첨부 업로드 경로의 상한. 요청 머리(Content-Length)만 보고도 413 으로 막고(tower-http),
/// 추출 상한(axum)도 같은 값으로 올린다. 전체 상한은 서체(20MB) 때문에 더 크다.
pub(crate) fn upload_limit() -> (RequestBodyLimitLayer, axum::extract::DefaultBodyLimit) {
    (
        RequestBodyLimitLayer::new(UPLOAD_LIMIT),
        axum::extract::DefaultBodyLimit::max(UPLOAD_LIMIT),
    )
}

/// 백업 파일 내려받기 응답. 큰 파일이라 통째로 읽지 않고 흘려보낸다.
pub(crate) async fn backup_file_response(path: &std::path::Path, name: &str) -> Result<Response> {
    let file = tokio::fs::File::open(path).await.map_err(|e| {
        Error::new(Code::NotFound)
            .with_message("error.backup_not_found")
            .arg("name", name.to_owned())
            .with_source(e)
    })?;
    let len = file.metadata().await.map_err(Error::internal)?.len();
    let body = axum::body::Body::from_stream(FileStream {
        file,
        buf: vec![0; 64 * 1024].into_boxed_slice(),
    });
    let mut res = Response::new(body);
    let h = res.headers_mut();
    h.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/gzip"),
    );
    h.insert(header::CONTENT_LENGTH, HeaderValue::from(len));
    if let Ok(v) = HeaderValue::from_str(&format!("attachment; filename=\"{name}\"")) {
        h.insert(header::CONTENT_DISPOSITION, v);
    }
    h.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    Ok(res)
}

/// 파일을 64KB 씩 읽어 내보내는 흐름.
struct FileStream {
    file: tokio::fs::File,
    buf: Box<[u8]>,
}

impl futures_core::Stream for FileStream {
    type Item = std::io::Result<axum::body::Bytes>;

    fn poll_next(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<Self::Item>> {
        use std::task::Poll;
        use tokio::io::{AsyncRead, ReadBuf};
        let this = &mut *self;
        let mut rb = ReadBuf::new(&mut this.buf);
        match std::pin::Pin::new(&mut this.file).poll_read(cx, &mut rb) {
            Poll::Ready(Ok(())) if rb.filled().is_empty() => Poll::Ready(None),
            Poll::Ready(Ok(())) => {
                Poll::Ready(Some(Ok(axum::body::Bytes::copy_from_slice(rb.filled()))))
            }
            Poll::Ready(Err(e)) => Poll::Ready(Some(Err(e))),
            Poll::Pending => Poll::Pending,
        }
    }
}
