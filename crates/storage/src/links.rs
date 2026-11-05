//! 링크 카드 정보 `link_previews` (GAP E12, ADR-0084). 주소마다 한 줄, 여러 글이 함께 쓴다.
//! 적기·가져온 결과는 감사 대상이 아닌 시스템 기록이고, 다시 가져오기(`reset`)만 감사와 같은 트랜잭션이다.

use maru_core::model::{LinkPreview, LinkStatus};
use maru_core::{Error, Result};
use sqlx::{Sqlite, SqliteExecutor, Transaction};

use crate::sqlite::db_error;

struct Row {
    url: String,
    status: String,
    title: Option<String>,
    description: Option<String>,
    site_name: Option<String>,
    image_url: Option<String>,
    attempts: i64,
    last_error: Option<String>,
    fetched_at: Option<String>,
    created_at: String,
}

impl TryFrom<Row> for LinkPreview {
    type Error = Error;

    fn try_from(r: Row) -> Result<Self> {
        Ok(LinkPreview {
            status: LinkStatus::parse(&r.status)
                .ok_or_else(|| Error::internal(format!("알 수 없는 링크 상태: {}", r.status)))?,
            url: r.url,
            title: r.title,
            description: r.description,
            site_name: r.site_name,
            image_url: r.image_url,
            attempts: r.attempts,
            last_error: r.last_error,
            fetched_at: r.fetched_at,
            created_at: r.created_at,
        })
    }
}

fn json_list(urls: &[String]) -> Result<String> {
    serde_json::to_string(urls).map_err(Error::internal)
}

/// 처음 보는 주소만 「가져올 차례」로 적는다.
pub(crate) async fn note<'e>(e: impl SqliteExecutor<'e>, urls: &[String], now: &str) -> Result<()> {
    let list = json_list(urls)?;
    sqlx::query!(
        "INSERT INTO link_previews (url, status, attempts, created_at)
         SELECT value, 'pending', 0, ? FROM json_each(?) WHERE true
         ON CONFLICT (url) DO NOTHING",
        now,
        list
    )
    .execute(e)
    .await
    .map_err(db_error)?;
    Ok(())
}

pub(crate) async fn by_urls<'e>(
    e: impl SqliteExecutor<'e>,
    urls: &[String],
) -> Result<Vec<LinkPreview>> {
    let list = json_list(urls)?;
    sqlx::query_as!(
        Row,
        "SELECT url, status, title, description, site_name, image_url, attempts, last_error,
                fetched_at, created_at
         FROM link_previews WHERE url IN (SELECT value FROM json_each(?))",
        list
    )
    .fetch_all(e)
    .await
    .map_err(db_error)?
    .into_iter()
    .map(LinkPreview::try_from)
    .collect()
}

/// 오래 기다린 것부터
pub(crate) async fn pending<'e>(
    e: impl SqliteExecutor<'e>,
    limit: i64,
) -> Result<Vec<LinkPreview>> {
    sqlx::query_as!(
        Row,
        "SELECT url, status, title, description, site_name, image_url, attempts, last_error,
                fetched_at, created_at
         FROM link_previews WHERE status = 'pending'
         ORDER BY created_at, url LIMIT ?",
        limit
    )
    .fetch_all(e)
    .await
    .map_err(db_error)?
    .into_iter()
    .map(LinkPreview::try_from)
    .collect()
}

pub(crate) async fn update<'e>(e: impl SqliteExecutor<'e>, p: &LinkPreview) -> Result<()> {
    let status = p.status.as_str();
    sqlx::query!(
        "UPDATE link_previews SET status = ?, title = ?, description = ?, site_name = ?,
                image_url = ?, attempts = ?, last_error = ?, fetched_at = ?
         WHERE url = ?",
        status,
        p.title,
        p.description,
        p.site_name,
        p.image_url,
        p.attempts,
        p.last_error,
        p.fetched_at,
        p.url
    )
    .execute(e)
    .await
    .map_err(db_error)?;
    Ok(())
}

pub(crate) async fn list<'e>(
    e: impl SqliteExecutor<'e>,
    status: Option<LinkStatus>,
    limit: i64,
) -> Result<Vec<LinkPreview>> {
    let status = status.map(LinkStatus::as_str);
    sqlx::query_as!(
        Row,
        "SELECT url, status, title, description, site_name, image_url, attempts, last_error,
                fetched_at, created_at
         FROM link_previews WHERE (?1 IS NULL OR status = ?1)
         ORDER BY created_at DESC, url LIMIT ?2",
        status,
        limit
    )
    .fetch_all(e)
    .await
    .map_err(db_error)?
    .into_iter()
    .map(LinkPreview::try_from)
    .collect()
}

/// 다시 가져올 차례로 되돌린다. 없는 주소면 `None`.
pub async fn reset(
    tx: &mut Transaction<'static, Sqlite>,
    url: &str,
) -> Result<Option<LinkPreview>> {
    sqlx::query_as!(
        Row,
        "UPDATE link_previews SET status = 'pending', attempts = 0, last_error = NULL
         WHERE url = ?
         RETURNING url, status, title, description, site_name, image_url, attempts, last_error,
                   fetched_at, created_at",
        url
    )
    .fetch_optional(&mut **tx)
    .await
    .map_err(db_error)?
    .map(LinkPreview::try_from)
    .transpose()
}
