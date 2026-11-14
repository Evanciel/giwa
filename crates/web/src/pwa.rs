//! 홈 화면에 추가 (ADR-0162): 앱 정보(`/manifest.webmanifest`), 아이콘, 오프라인 안내 한 쪽만 담는 서비스 워커.
//! 서비스 워커는 화면 이동이 끊겼을 때 `/offline` 을 보여 줄 뿐, 다른 응답은 담아 두지 않는다
//! (로그인·관리 화면·개인 응답이 기기에 남지 않는다).

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use axum::extract::State;
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use giwa_core::design::Tokens;
use giwa_core::model::SiteSettings;
use giwa_core::sample_art;
use giwa_core::services::{design, settings};
use serde_json::json;

use crate::AppState;

/// 서버가 그린 아이콘 `(사이트 이름·색, 크기 → PNG)`. 이름이나 색이 바뀌면 다시 그린다.
pub type Icons = Mutex<Option<(String, HashMap<u32, Arc<Vec<u8>>>)>>;

/// 지금 설정과 디자인 토큰. 못 읽으면 기본값으로 답한다.
async fn current(state: &AppState) -> (SiteSettings, Tokens) {
    let s = settings::site(&state.ctx).await.unwrap_or_default();
    let t = design::tokens(&state.ctx).await.unwrap_or_default();
    (s, t)
}

/// 같은 이름·색이면 같은 무늬가 나오도록 하는 씨앗.
fn seed(name: &str, primary: &str) -> u32 {
    name.bytes()
        .chain(primary.bytes())
        .fold(2_166_136_261u32, |a, b| {
            (a ^ u32::from(b)).wrapping_mul(16_777_619)
        })
}

async fn off(state: &AppState) -> Response {
    crate::error::page(state, &giwa_core::Error::new(giwa_core::Code::NotFound)).await
}

/// `/manifest.webmanifest`. `site.pwa` 를 끄면 없는 주소다.
pub(crate) async fn manifest(State(state): State<AppState>) -> Response {
    let (s, t) = current(&state).await;
    if !s.pwa {
        return off(&state).await;
    }
    let body = json!({
        "id": "/",
        "name": s.name,
        "short_name": s.name.chars().take(12).collect::<String>(),
        "description": s.description,
        "lang": s.lang,
        "start_url": "/",
        "scope": "/",
        "display": "standalone",
        "background_color": t.color.bg,
        "theme_color": t.color.primary,
        "icons": [
            { "src": "/icon.svg", "sizes": "any", "type": "image/svg+xml" },
            { "src": "/icon-192.png", "sizes": "192x192", "type": "image/png" },
            { "src": "/icon-512.png", "sizes": "512x512", "type": "image/png" },
            { "src": "/icon-512.png", "sizes": "512x512", "type": "image/png", "purpose": "maskable" },
        ],
    });
    (
        [
            (header::CONTENT_TYPE, "application/manifest+json"),
            (header::CACHE_CONTROL, "no-cache"),
        ],
        body.to_string(),
    )
        .into_response()
}

/// 아이콘 PNG. 크기는 정해진 것만 (192·512, 애플 180, 주소창 64).
async fn icon_png(state: &AppState, size: u32) -> Response {
    let (s, t) = current(state).await;
    let key = format!("{}\n{}", s.name, t.color.primary);
    let cached = state.icons.lock().ok().and_then(|c| {
        c.as_ref()
            .filter(|(k, _)| *k == key)
            .and_then(|(_, m)| m.get(&size).cloned())
    });
    let bytes = match cached {
        Some(b) => b,
        None => {
            let (n, color) = (seed(&s.name, &t.color.primary), t.color.primary.clone());
            let drawn =
                tokio::task::spawn_blocking(move || sample_art::icon(n, &color, size)).await;
            let Ok(Ok(png)) = drawn else {
                return off(state).await;
            };
            let b = Arc::new(png);
            if let Ok(mut c) = state.icons.lock() {
                match c.as_mut().filter(|(k, _)| *k == key) {
                    Some((_, m)) => {
                        m.insert(size, b.clone());
                    }
                    None => *c = Some((key, HashMap::from([(size, b.clone())]))),
                }
            }
            b
        }
    };
    (
        [
            (header::CONTENT_TYPE, "image/png"),
            (header::CACHE_CONTROL, "public, max-age=3600"),
        ],
        bytes.as_ref().clone(),
    )
        .into_response()
}

pub(crate) async fn icon_192(State(state): State<AppState>) -> Response {
    icon_png(&state, 192).await
}

pub(crate) async fn icon_512(State(state): State<AppState>) -> Response {
    icon_png(&state, 512).await
}

pub(crate) async fn apple_icon(State(state): State<AppState>) -> Response {
    icon_png(&state, 180).await
}

pub(crate) async fn favicon(State(state): State<AppState>) -> Response {
    icon_png(&state, 64).await
}

/// 같은 무늬의 SVG (주소창 아이콘). 사용자 글자를 넣지 않아 그대로 열어도 안전하다.
pub(crate) async fn icon_svg(State(state): State<AppState>) -> Response {
    let (s, t) = current(&state).await;
    let svg = sample_art::icon_svg(seed(&s.name, &t.color.primary), &t.color.primary);
    (
        [
            (header::CONTENT_TYPE, "image/svg+xml"),
            (header::CACHE_CONTROL, "public, max-age=3600"),
            (header::CONTENT_SECURITY_POLICY, "default-src 'none'"),
        ],
        svg,
    )
        .into_response()
}

/// `/sw.js`. 담아 둘 것은 오프라인 안내와 그 안내가 쓰는 스타일뿐이다.
/// 사이트 이름·색·스타일이 바뀌면 판 번호가 바뀌어 브라우저가 새로 받는다.
pub(crate) async fn worker(State(state): State<AppState>) -> Response {
    let (s, t) = current(&state).await;
    if !s.pwa {
        return off(&state).await;
    }
    let site_css = state.theme.asset("site.css").map(|(b, _)| b.into_owned());
    let version = [
        env!("CARGO_PKG_VERSION").as_bytes(),
        s.name.as_bytes(),
        s.lang.as_bytes(),
        t.to_css(&[]).as_bytes(),
        site_css.as_deref().unwrap_or_default(),
    ]
    .iter()
    .flat_map(|b| b.iter())
    .fold(2_166_136_261u32, |a, b| {
        (a ^ u32::from(*b)).wrapping_mul(16_777_619)
    });
    let script = WORKER.replace("{version}", &format!("{version:08x}"));
    let mut res = (
        [
            (header::CONTENT_TYPE, "text/javascript; charset=utf-8"),
            (header::CACHE_CONTROL, "no-cache"),
        ],
        script,
    )
        .into_response();
    res.headers_mut()
        .insert("service-worker-allowed", HeaderValue::from_static("/"));
    res
}

const WORKER: &str = r#"// 홈 화면에 추가 (ADR-0162). 화면 이동이 끊겼을 때 오프라인 안내만 보여 준다.
// 다른 응답은 담아 두지 않는다: 로그인·관리 화면·개인 응답이 이 기기에 남지 않는다.
const CACHE = "maru-offline-{version}";
const KEEP = ["/offline", "/assets/tokens.css", "/assets/site.css"];
self.addEventListener("install", (e) => {
  e.waitUntil(caches.open(CACHE).then((c) => c.addAll(KEEP)).then(() => self.skipWaiting()));
});
self.addEventListener("activate", (e) => {
  e.waitUntil(
    caches.keys()
      .then((ks) => Promise.all(ks.filter((k) => k.startsWith("maru-offline-") && k !== CACHE).map((k) => caches.delete(k))))
      .then(() => self.clients.claim())
  );
});
self.addEventListener("fetch", (e) => {
  const r = e.request;
  if (r.method !== "GET") return;
  if (r.mode === "navigate") {
    e.respondWith(fetch(r).catch(() => caches.match("/offline")));
    return;
  }
  const u = new URL(r.url);
  if (u.origin === self.location.origin && KEEP.includes(u.pathname) && u.pathname !== "/offline") {
    e.respondWith(fetch(r).catch(() => caches.match(u.pathname)));
  }
});
"#;

/// `/offline`: 서비스 워커가 담아 두는 안내 한 쪽. 방문 통계에 세지 않는다.
pub(crate) async fn offline(State(state): State<AppState>) -> Response {
    let site = crate::public::site_json(&state).await;
    match state.theme.render("offline.html", &json!({ "site": site })) {
        Ok(html) => (
            [
                (header::CONTENT_TYPE, "text/html; charset=utf-8"),
                (header::CACHE_CONTROL, "no-cache"),
            ],
            html,
        )
            .into_response(),
        Err(e) => {
            tracing::error!(error = ?e, "오프라인 안내를 그리지 못했습니다");
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        }
    }
}
