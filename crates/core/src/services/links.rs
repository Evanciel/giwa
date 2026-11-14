//! 링크 카드 (GAP E12·E22, ADR-0084).
//!
//! - 본문의 한 줄 블록 `::link[주소]` 를 제목·설명·사이트 이름·그림이 있는 상자로 그린다.
//! - 정보는 화면을 그릴 때 가져오지 않는다. 처음 보는 주소는 「가져올 차례」로 적어 두고
//!   서버가 주기마다 몇 개씩 가져온다(`fetch_pending`). 그 전·실패 때는 주소와 도메인만 보인다.
//! - SNS 게시물(X·인스타그램·틱톡·스레드·페이스북)은 가져오지 않는다 — 로그인 벽이고, 외부 스크립트 없이
//!   어느 서비스의 게시물인지와 주소만 보여 준다(E22).
//! - 가져온 그림은 미디어로 다시 저장해 방문자 브라우저가 남의 서버에 접속하지 않게 한다.
//! - 사이트 설정 `link_cards` 를 끄면 가져오기도, 새로 적기도 하지 않는다.

use std::collections::HashMap;

use schemars::JsonSchema;
use serde::Serialize;
use serde_json::json;

use crate::actor::{Actor, Permission};
use crate::audit::Target;
use crate::clock;
use crate::error::{Code, Error, Result};
use crate::model::{LinkPreview, LinkStatus};
use crate::services::{media, settings};
use crate::store::{Ctx, Tx};

pub const MAX_ATTEMPTS: i64 = 3;
/// 한 주기에 가져올 주소 수
pub const BATCH: i64 = 5;
/// 한 화면에서 새로 적을 주소 수(본문 하나가 수백 개를 쌓지 못하게)
pub const NOTE_LIMIT: usize = 20;

/// 외부 페이지에서 읽은 것
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Fetched {
    pub title: Option<String>,
    pub description: Option<String>,
    pub site_name: Option<String>,
    pub image: Option<FetchedImage>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FetchedImage {
    pub filename: String,
    pub bytes: Vec<u8>,
}

/// 외부 페이지를 읽는 곳. 사설 주소 차단·시간·크기 제한은 구현이 맡는다.
#[async_trait::async_trait]
pub trait LinkFetcher: Send + Sync {
    async fn fetch(&self, url: &str) -> std::result::Result<Fetched, String>;
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, JsonSchema)]
pub struct FetchReport {
    pub ok: u32,
    pub retry: u32,
    pub failed: u32,
}

/// SNS 게시물 주소면 서비스 이름
pub fn sns_label(url: &str) -> Option<&'static str> {
    let host = host_of(url)?;
    let host = host.strip_prefix("www.").unwrap_or(&host);
    let host = host.strip_prefix("m.").unwrap_or(host);
    Some(match host {
        "x.com" | "twitter.com" => "X",
        "instagram.com" => "인스타그램",
        "tiktok.com" | "vm.tiktok.com" => "틱톡",
        "threads.net" | "threads.com" => "스레드",
        "facebook.com" | "fb.com" | "fb.watch" => "페이스북",
        _ => return None,
    })
}

/// 주소의 호스트(소문자). 사용자 정보·포트는 뺀다.
pub fn host_of(url: &str) -> Option<String> {
    let rest = url
        .strip_prefix("https://")
        .or_else(|| url.strip_prefix("http://"))?;
    let authority = rest.split(['/', '?', '#']).next()?;
    let authority = authority.rsplit('@').next()?;
    let host = if authority.starts_with('[') {
        authority.split(']').next().map(|h| format!("{h}]"))?
    } else {
        authority.split(':').next()?.to_owned()
    };
    (!host.is_empty()).then(|| host.to_ascii_lowercase())
}

/// 화면에 그릴 카드 정보. 처음 보는 주소(SNS 제외)는 가져올 차례로 적는다 — 감사 대상이 아닌 시스템 기록.
pub async fn cards_for(ctx: &Ctx, urls: &[String]) -> Result<HashMap<String, LinkPreview>> {
    if urls.is_empty() || !settings::site(ctx).await?.link_cards {
        return Ok(HashMap::new());
    }
    let wanted: Vec<String> = urls
        .iter()
        .filter(|u| sns_label(u).is_none())
        .take(NOTE_LIMIT)
        .cloned()
        .collect();
    if wanted.is_empty() {
        return Ok(HashMap::new());
    }
    ctx.store.note_links(&wanted, &clock::now()).await?;
    Ok(ctx
        .store
        .link_previews(&wanted)
        .await?
        .into_iter()
        .map(|p| (p.url.clone(), p))
        .collect())
}

/// 가져올 차례인 주소를 몇 개 가져온다. 서버가 주기마다 부른다.
pub async fn fetch_pending(ctx: &Ctx, fetcher: &dyn LinkFetcher) -> Result<FetchReport> {
    let mut report = FetchReport::default();
    if !settings::site(ctx).await?.link_cards {
        return Ok(report);
    }
    for mut p in ctx.store.pending_links(BATCH).await? {
        p.attempts += 1;
        match fetcher.fetch(&p.url).await {
            Ok(f) => {
                p.title = f.title;
                p.description = f.description;
                p.site_name = f.site_name;
                p.image_url = match f.image {
                    Some(img) => thumbnail(ctx, &p.url, p.title.as_deref(), img).await,
                    None => None,
                };
                p.status = LinkStatus::Ok;
                p.last_error = None;
                p.fetched_at = Some(clock::now());
                report.ok += 1;
            }
            Err(e) => {
                p.last_error = Some(e.chars().take(300).collect());
                if p.attempts >= MAX_ATTEMPTS {
                    p.status = LinkStatus::Failed;
                    p.fetched_at = Some(clock::now());
                    report.failed += 1;
                } else {
                    report.retry += 1;
                }
            }
        }
        ctx.store.update_link(&p).await?;
    }
    Ok(report)
}

/// 그림을 미디어로 보관한다. 못 읽는 그림이면 카드에 그림 없이 둔다.
async fn thumbnail(ctx: &Ctx, url: &str, title: Option<&str>, img: FetchedImage) -> Option<String> {
    let alt = title.map(|t| t.chars().take(120).collect::<String>());
    match media::store_link_thumbnail(ctx, &img.filename, &img.bytes, alt, url).await {
        Ok(m) => Some(m.url),
        // 그림을 못 읽으면 그림 없는 카드로 둔다
        Err(_) => None,
    }
}

pub async fn list(
    ctx: &Ctx,
    actor: &Actor,
    status: Option<LinkStatus>,
    limit: i64,
) -> Result<Vec<LinkPreview>> {
    actor.require(Permission::PostWrite)?;
    ctx.store.links(status, limit.clamp(1, 200)).await
}

/// 다시 가져오게 한다(제목이 바뀐 페이지 등). 다음 주기에 가져온다. 여러 번 불러도 같다.
pub async fn refresh(ctx: &Ctx, actor: &Actor, url: &str) -> Result<LinkPreview> {
    actor.require(Permission::PostWrite)?;
    let mut tx = Tx::begin(ctx).await?;
    let found = tx.reset_link(url).await?;
    let Some(p) = found else {
        return Err(Error::new(Code::NotFound)
            .with_message("error.link_not_found")
            .arg("url", url)
            .next("giwa link list"));
    };
    tx.audit(actor, "link.refresh", Target::new("link", url), json!({}))
        .await?;
    tx.commit().await?;
    Ok(p)
}
