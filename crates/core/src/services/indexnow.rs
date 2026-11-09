//! IndexNow (ADR-0143). 공개한 사이트의 글·페이지가 바뀌면 그 주소를 검색엔진에 바로 알린다.
//!
//! - 감사 기록을 읽은 자리(`indexnow.state` 의 `cursor`)부터 글·페이지 동작만 모아 한 번에 보낸다.
//! - 손님이 읽을 수 있는 게시판의 글(비밀글 제외)과 페이지만. 지운 글도 알린다.
//! - 사이트 주소가 https 공개 도메인일 때만 보낸다. 내 컴퓨터에서만 쓰는 사이트는 아무것도 보내지 않는다.
//! - 실패하면 읽은 자리를 넘기지 않고 [`RETRY_MINUTES`] 뒤에 다시 보낸다.

use std::collections::HashSet;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::actor::{Actor, Permission};
use crate::audit::Target as AuditTarget;
use crate::clock;
use crate::error::{Error, Result};
use crate::services::{anonymous_actor, auth, boards, settings};
use crate::store::{Ctx, Tx};
use crate::urls::{self, Target};

/// indexnow.org 참여 목록에 있는 네이버의 받는 곳. 참여 검색엔진끼리 받은 주소를 나눈다.
pub const ENDPOINT: &str = "https://searchadvisor.naver.com/indexnow";
/// 주소를 바꾸는 동작.
pub const ACTIONS: [&str; 14] = [
    "post.publish",
    "post.publish_scheduled",
    "post.update",
    "post.revert",
    "post.restore",
    "post.move",
    "post.hide",
    "post.moderate",
    "post.delete",
    "page.publish",
    "page.update",
    "page.restore",
    "page.hide",
    "page.delete",
];
/// 실패한 뒤 다시 보낼 때까지(분).
pub const RETRY_MINUTES: i64 = 30;
/// 한 번에 보내는 주소 수 (IndexNow 문서의 상한).
pub const MAX_URLS: usize = 10_000;
const KEY: &str = "indexnow.state";
const BATCH: i64 = 500;

/// 검색엔진에 보내는 곳. 사설 주소 차단·시간 제한은 구현이 맡는다.
#[async_trait::async_trait]
pub trait IndexNowSender: Send + Sync {
    /// JSON 본문을 보내고 응답 코드를 돌려준다. 접속하지 못하면 `Err`.
    async fn submit(&self, endpoint: &str, body: Vec<u8>) -> std::result::Result<u16, String>;
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct State {
    #[serde(default)]
    key: String,
    #[serde(default)]
    cursor: i64,
    #[serde(default)]
    last_at: Option<String>,
    #[serde(default)]
    last_status: Option<u16>,
    #[serde(default)]
    last_count: u32,
    #[serde(default)]
    last_error: Option<String>,
    #[serde(default)]
    sent_total: u64,
    #[serde(default)]
    retry_at: Option<String>,
}

/// 지금 상태.
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct IndexNowStatus {
    /// 설정 `indexnow`
    pub enabled: bool,
    /// 사이트 주소가 알릴 수 있는 공개 https 도메인인가
    pub public_site: bool,
    pub site_url: String,
    pub endpoint: String,
    /// 키 파일 주소. 아직 한 번도 보내지 않았으면 없다
    pub key_url: Option<String>,
    pub last_at: Option<String>,
    pub last_status: Option<u16>,
    pub last_count: u32,
    pub last_error: Option<String>,
    pub sent_total: u64,
    /// 실패해서 다시 보낼 시각
    pub retry_at: Option<String>,
}

/// 한 번 돌린 결과.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "result", rename_all = "snake_case")]
pub enum SendReport {
    /// 꺼져 있거나 공개 사이트가 아니다
    Off,
    /// 새 동작이 없다
    Idle,
    /// 실패한 뒤 다시 보낼 때를 기다린다
    Waiting,
    /// 알릴 주소 없이 읽은 자리만 넘겼다
    Skipped {
        events: usize,
    },
    Sent {
        urls: usize,
        status: u16,
    },
    Failed {
        urls: usize,
        error: String,
    },
}

/// 알릴 수 있는 사이트 주소면 호스트. https 이고 내 컴퓨터·사설·시험용 이름이 아니어야 한다.
pub fn public_host(site_url: &str) -> Option<String> {
    let rest = site_url.strip_prefix("https://")?;
    let host = rest.split(['/', '?', '#']).next()?.to_ascii_lowercase();
    if host.contains('@') || host.contains(':') || !host.contains('.') {
        return None;
    }
    if host.parse::<std::net::Ipv4Addr>().is_ok() {
        return None;
    }
    const LOCAL: [&str; 7] = [
        ".local",
        ".localhost",
        ".test",
        ".internal",
        ".example",
        ".invalid",
        ".lan",
    ];
    if host == "localhost" || LOCAL.iter().any(|s| host.ends_with(s)) {
        return None;
    }
    Some(host)
}

async fn load(ctx: &Ctx) -> Result<State> {
    Ok(ctx
        .store
        .settings()
        .await?
        .into_iter()
        .find(|(k, _)| k == KEY)
        .and_then(|(_, v)| serde_json::from_value(v).ok())
        .unwrap_or_default())
}

async fn save(ctx: &Ctx, state: &State, diff: serde_json::Value) -> Result<()> {
    let v = serde_json::to_value(state).map_err(Error::internal)?;
    let mut tx = Tx::begin(ctx).await?;
    tx.put_setting(KEY, &v, &clock::now()).await?;
    tx.audit(&Actor::local(), "seo.indexnow", AuditTarget::site(), diff)
        .await?;
    tx.commit().await
}

/// 키 파일(`/{키}.txt`)로 낼 키. 아직 만들지 않았으면 없다.
pub async fn key(ctx: &Ctx) -> Result<Option<String>> {
    Ok(Some(load(ctx).await?.key).filter(|k| !k.is_empty()))
}

pub async fn status(ctx: &Ctx, actor: &Actor) -> Result<IndexNowStatus> {
    actor.require(Permission::PostPublish)?;
    let s = settings::site(ctx).await?;
    let st = load(ctx).await?;
    let host = public_host(&ctx.site_url);
    Ok(IndexNowStatus {
        enabled: s.indexnow,
        public_site: host.is_some(),
        site_url: ctx.site_url.clone(),
        endpoint: ENDPOINT.into(),
        key_url: (!st.key.is_empty()).then(|| key_url(&ctx.site_url, &st.key)),
        last_at: st.last_at,
        last_status: st.last_status,
        last_count: st.last_count,
        last_error: st.last_error,
        sent_total: st.sent_total,
        retry_at: st.retry_at,
    })
}

fn key_url(site_url: &str, key: &str) -> String {
    format!("{}/{key}.txt", site_url.trim_end_matches('/'))
}

/// 밀린 동작을 모아 한 번 보낸다. 서버가 주기마다 부른다.
///
/// `scanned` 는 부르는 쪽이 들고 있는 「알릴 것 없이 읽은 자리」다. 알릴 동작이 없을 때는 저장하지 않으므로
/// (저장하면 감사 기록이 또 쌓인다) 다음 주기에 같은 구간을 다시 읽지 않게 메모리에만 둔다.
pub async fn send_pending(
    ctx: &Ctx,
    sender: &dyn IndexNowSender,
    endpoint: &str,
    scanned: &mut i64,
) -> Result<SendReport> {
    let Some(host) = public_host(&ctx.site_url) else {
        return Ok(SendReport::Off);
    };
    if !settings::site(ctx).await?.indexnow {
        return Ok(SendReport::Off);
    }
    let mut st = load(ctx).await?;
    if st
        .retry_at
        .as_deref()
        .is_some_and(|t| t > clock::now().as_str())
    {
        return Ok(SendReport::Waiting);
    }
    let upto = ctx.store.audit_last_seq().await?;
    let from = st.cursor.max(*scanned);
    if upto <= from {
        return Ok(SendReport::Idle);
    }
    let (urls, events, last) = collect(ctx, from, upto).await?;
    if events == 0 {
        *scanned = last;
        return Ok(SendReport::Idle);
    }
    if urls.is_empty() {
        st.cursor = last;
        *scanned = last;
        save(ctx, &st, json!({ "urls": 0, "events": events })).await?;
        return Ok(SendReport::Skipped { events });
    }
    if st.key.is_empty() {
        // 검색엔진이 키 파일을 읽으러 오기 전에 저장해 둔다.
        st.key = auth::new_secret()?[..32].to_owned();
        save(ctx, &st, json!({ "key": "created" })).await?;
    }
    let body = serde_json::to_vec(&json!({
        "host": host,
        "key": st.key,
        "keyLocation": key_url(&ctx.site_url, &st.key),
        "urlList": urls,
    }))
    .map_err(Error::internal)?;
    let n = urls.len();
    st.last_at = Some(clock::now());
    st.last_count = u32::try_from(n).unwrap_or(u32::MAX);
    let report = match sender.submit(endpoint, body).await {
        Ok(code) if (200..300).contains(&code) => {
            st.cursor = last;
            *scanned = last;
            st.last_status = Some(code);
            st.last_error = None;
            st.retry_at = None;
            st.sent_total += n as u64;
            SendReport::Sent {
                urls: n,
                status: code,
            }
        }
        Ok(code) => {
            st.last_status = Some(code);
            st.last_error = Some(format!("응답 {code}"));
            st.retry_at = Some(clock::minutes_later(RETRY_MINUTES));
            SendReport::Failed {
                urls: n,
                error: format!("응답 {code}"),
            }
        }
        Err(e) => {
            st.last_status = None;
            st.last_error = Some(e.chars().take(200).collect());
            st.retry_at = Some(clock::minutes_later(RETRY_MINUTES));
            SendReport::Failed { urls: n, error: e }
        }
    };
    save(
        ctx,
        &st,
        json!({ "urls": n, "status": st.last_status, "error": st.last_error }),
    )
    .await?;
    Ok(report)
}

/// `after` 다음부터 `upto` 까지 알릴 주소. (주소, 읽은 동작 수, 마지막으로 읽은 순번)
async fn collect(ctx: &Ctx, after: i64, upto: i64) -> Result<(Vec<String>, usize, i64)> {
    let guest = anonymous_actor(ctx).await?;
    let open: Vec<_> = boards::list(ctx, &guest).await?;
    let mut seen = HashSet::new();
    let mut urls = Vec::new();
    let mut events = 0;
    let mut cursor = after;
    'outer: loop {
        let batch = ctx.store.audit_after(cursor, upto, &ACTIONS, BATCH).await?;
        let full = batch.len() == BATCH as usize;
        for ev in &batch {
            if urls.len() >= MAX_URLS {
                break 'outer;
            }
            cursor = ev.seq;
            events += 1;
            let Some(id) = ev.target_id.as_deref() else {
                continue;
            };
            let url = match ev.target_type.as_str() {
                "post" => match ctx.store.post_by_id(id).await? {
                    Some(p) if !p.is_secret => open.iter().find(|b| b.id == p.board_id).map(|b| {
                        urls::absolute(
                            &ctx.site_url,
                            &Target::Post {
                                board: &b.slug,
                                post: &p.slug,
                            },
                        )
                    }),
                    _ => None,
                },
                "page" => ctx
                    .store
                    .page_by_id(id)
                    .await?
                    .map(|p| urls::absolute(&ctx.site_url, &Target::Page { page: &p.slug })),
                _ => None,
            };
            if let Some(u) = url
                && seen.insert(u.clone())
            {
                urls.push(u);
            }
        }
        if !full {
            // 이 구간에 더 읽을 동작이 없다. 끝까지 읽은 것으로 친다.
            cursor = upto;
            break;
        }
    }
    Ok((urls, events, cursor))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_public_https_domains_are_announced() {
        assert_eq!(
            public_host("https://maru.example.co.kr").as_deref(),
            Some("maru.example.co.kr")
        );
        assert_eq!(
            public_host("https://Blog.Site.com/").as_deref(),
            Some("blog.site.com")
        );
        for no in [
            "http://maru.co.kr",
            "https://localhost",
            "https://127.0.0.1",
            "https://127.0.0.1:8080",
            "https://site.test",
            "https://nas.local",
            "https://dev.localhost",
            "https://a.example",
            "https://intranet",
            "https://user@site.com",
            "https://site.com:8443",
            "https://[::1]",
        ] {
            assert_eq!(public_host(no), None, "{no}");
        }
    }
}
