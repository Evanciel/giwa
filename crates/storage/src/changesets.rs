//! 변경 묶음 `changesets` (ADR-0121).

use giwa_core::model::{Changeset, ChangesetStatus};
use giwa_core::{Error, Result};
use serde_json::Value;
use sqlx::SqliteExecutor;

use crate::sqlite::db_error;

struct Row {
    id: String,
    title: String,
    actor_type: String,
    actor_id: Option<String>,
    status: String,
    ops: String,
    inverse: String,
    error: Option<String>,
    created_at: String,
    applied_at: Option<String>,
    reverted_at: Option<String>,
}

impl TryFrom<Row> for Changeset {
    type Error = Error;

    fn try_from(r: Row) -> Result<Self> {
        let list = |text: &str| serde_json::from_str::<Vec<Value>>(text).map_err(Error::internal);
        Ok(Changeset {
            status: ChangesetStatus::parse(&r.status).ok_or_else(|| {
                Error::internal(format!("알 수 없는 변경 묶음 상태: {}", r.status))
            })?,
            ops: list(&r.ops)?,
            inverse: list(&r.inverse)?,
            id: r.id,
            title: r.title,
            actor_type: r.actor_type,
            actor_id: r.actor_id,
            error: r.error,
            created_at: r.created_at,
            applied_at: r.applied_at,
            reverted_at: r.reverted_at,
        })
    }
}

pub(crate) async fn by_id<'e>(e: impl SqliteExecutor<'e>, id: &str) -> Result<Option<Changeset>> {
    sqlx::query_as!(
        Row,
        "SELECT id, title, actor_type, actor_id, status, ops, inverse, error, created_at,
                applied_at, reverted_at
         FROM changesets WHERE id = ?",
        id
    )
    .fetch_optional(e)
    .await
    .map_err(db_error)?
    .map(Changeset::try_from)
    .transpose()
}

pub(crate) async fn list<'e>(
    e: impl SqliteExecutor<'e>,
    status: Option<ChangesetStatus>,
    limit: i64,
) -> Result<Vec<Changeset>> {
    let status = status.map(ChangesetStatus::as_str);
    sqlx::query_as!(
        Row,
        "SELECT id, title, actor_type, actor_id, status, ops, inverse, error, created_at,
                applied_at, reverted_at
         FROM changesets WHERE (?1 IS NULL OR status = ?1) ORDER BY id DESC LIMIT ?2",
        status,
        limit
    )
    .fetch_all(e)
    .await
    .map_err(db_error)?
    .into_iter()
    .map(Changeset::try_from)
    .collect()
}

pub(crate) async fn save<'e>(e: impl SqliteExecutor<'e>, cs: &Changeset) -> Result<()> {
    let status = cs.status.as_str();
    let ops = serde_json::to_string(&cs.ops).map_err(Error::internal)?;
    let inverse = serde_json::to_string(&cs.inverse).map_err(Error::internal)?;
    sqlx::query!(
        "INSERT INTO changesets (id, title, actor_type, actor_id, status, ops, inverse, error,
                                 created_at, applied_at, reverted_at)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
         ON CONFLICT (id) DO UPDATE SET status = excluded.status, ops = excluded.ops,
             inverse = excluded.inverse, error = excluded.error,
             applied_at = excluded.applied_at, reverted_at = excluded.reverted_at",
        cs.id,
        cs.title,
        cs.actor_type,
        cs.actor_id,
        status,
        ops,
        inverse,
        cs.error,
        cs.created_at,
        cs.applied_at,
        cs.reverted_at
    )
    .execute(e)
    .await
    .map_err(db_error)?;
    Ok(())
}

pub(crate) async fn stamp_audit<'e>(
    e: impl SqliteExecutor<'e>,
    id: &str,
    since: &str,
    actor_type: &str,
    actor_id: Option<&str>,
) -> Result<()> {
    sqlx::query!(
        "UPDATE audit_log SET changeset_id = ?
         WHERE at >= ? AND actor_type = ? AND actor_id IS ? AND changeset_id IS NULL",
        id,
        since,
        actor_type,
        actor_id
    )
    .execute(e)
    .await
    .map_err(db_error)?;
    Ok(())
}

pub(crate) async fn set_preview_token<'e>(
    e: impl SqliteExecutor<'e>,
    id: &str,
    hash: &str,
    expires_at: &str,
) -> Result<()> {
    sqlx::query!(
        "UPDATE changesets SET preview_token_hash = ?, preview_token_expires = ? WHERE id = ?",
        hash,
        expires_at,
        id
    )
    .execute(e)
    .await
    .map_err(db_error)?;
    Ok(())
}

pub(crate) async fn take_preview_token<'e>(
    e: impl SqliteExecutor<'e>,
    id: &str,
    hash: &str,
    now: &str,
) -> Result<bool> {
    let done = sqlx::query!(
        "UPDATE changesets SET preview_token_hash = NULL, preview_token_expires = NULL
         WHERE id = ? AND preview_token_hash = ? AND preview_token_expires > ?",
        id,
        hash,
        now
    )
    .execute(e)
    .await
    .map_err(db_error)?;
    Ok(done.rows_affected() == 1)
}
