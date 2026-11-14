//! 내 블록 `saved_blocks` (ADR-0159). 화면 편집기 보관함.

use giwa_core::model::SavedBlock;
use giwa_core::{Error, Result};
use sqlx::SqliteExecutor;

use crate::sqlite::db_error;

struct Row {
    id: String,
    name: String,
    kind: String,
    region: String,
    title: String,
    config: String,
    created_by: Option<String>,
    created_at: String,
}

impl TryFrom<Row> for SavedBlock {
    type Error = Error;

    fn try_from(r: Row) -> Result<Self> {
        Ok(SavedBlock {
            config: serde_json::from_str(&r.config).map_err(Error::internal)?,
            id: r.id,
            name: r.name,
            kind: r.kind,
            region: r.region,
            title: r.title,
            created_by: r.created_by,
            created_at: r.created_at,
        })
    }
}

/// 새것부터.
pub(crate) async fn list<'e>(e: impl SqliteExecutor<'e>) -> Result<Vec<SavedBlock>> {
    sqlx::query_as!(
        Row,
        "SELECT id, name, kind, region, title, config, created_by, created_at FROM saved_blocks
         ORDER BY created_at DESC, id DESC"
    )
    .fetch_all(e)
    .await
    .map_err(db_error)?
    .into_iter()
    .map(SavedBlock::try_from)
    .collect()
}

pub(crate) async fn get<'e>(e: impl SqliteExecutor<'e>, id: &str) -> Result<Option<SavedBlock>> {
    sqlx::query_as!(
        Row,
        "SELECT id, name, kind, region, title, config, created_by, created_at FROM saved_blocks
         WHERE id = ?",
        id
    )
    .fetch_optional(e)
    .await
    .map_err(db_error)?
    .map(SavedBlock::try_from)
    .transpose()
}

pub(crate) async fn insert<'e>(e: impl SqliteExecutor<'e>, b: &SavedBlock) -> Result<()> {
    let config = serde_json::to_string(&b.config).map_err(Error::internal)?;
    sqlx::query!(
        "INSERT INTO saved_blocks (id, name, kind, region, title, config, created_by, created_at)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
        b.id,
        b.name,
        b.kind,
        b.region,
        b.title,
        config,
        b.created_by,
        b.created_at
    )
    .execute(e)
    .await
    .map_err(db_error)?;
    Ok(())
}

pub(crate) async fn delete<'e>(e: impl SqliteExecutor<'e>, id: &str) -> Result<()> {
    sqlx::query!("DELETE FROM saved_blocks WHERE id = ?", id)
        .execute(e)
        .await
        .map_err(db_error)?;
    Ok(())
}
