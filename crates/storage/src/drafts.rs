//! 화면 편집기 초안 `editor_drafts` (ADR-0152). 사람마다 화면마다 한 줄.
//! 그 사람의 작업 중 사본이라 감사 대상이 아니다. 발행이 변경 묶음과 감사 기록에 남는다.

use giwa_core::model::EditorDraft;
use giwa_core::{Error, Result};
use sqlx::SqliteExecutor;

use crate::sqlite::db_error;

struct Row {
    member_id: String,
    screen: String,
    body: String,
    base: String,
    updated_at: String,
}

impl TryFrom<Row> for EditorDraft {
    type Error = Error;

    fn try_from(r: Row) -> Result<Self> {
        Ok(EditorDraft {
            body: serde_json::from_str(&r.body).map_err(Error::internal)?,
            member_id: r.member_id,
            screen: r.screen,
            base: r.base,
            updated_at: r.updated_at,
        })
    }
}

pub(crate) async fn get<'e>(
    e: impl SqliteExecutor<'e>,
    member_id: &str,
    screen: &str,
) -> Result<Option<EditorDraft>> {
    sqlx::query_as!(
        Row,
        "SELECT member_id, screen, body, base, updated_at FROM editor_drafts
         WHERE member_id = ? AND screen = ?",
        member_id,
        screen
    )
    .fetch_optional(e)
    .await
    .map_err(db_error)?
    .map(EditorDraft::try_from)
    .transpose()
}

/// 이 화면의 모든 사람 초안, 최근 것부터.
pub(crate) async fn for_screen<'e>(
    e: impl SqliteExecutor<'e>,
    screen: &str,
) -> Result<Vec<EditorDraft>> {
    sqlx::query_as!(
        Row,
        "SELECT member_id, screen, body, base, updated_at FROM editor_drafts
         WHERE screen = ? ORDER BY updated_at DESC",
        screen
    )
    .fetch_all(e)
    .await
    .map_err(db_error)?
    .into_iter()
    .map(EditorDraft::try_from)
    .collect()
}

/// 있으면 덮어쓴다.
pub(crate) async fn put<'e>(e: impl SqliteExecutor<'e>, d: &EditorDraft) -> Result<()> {
    let body = serde_json::to_string(&d.body).map_err(Error::internal)?;
    sqlx::query!(
        "INSERT INTO editor_drafts (member_id, screen, body, base, updated_at)
         VALUES (?, ?, ?, ?, ?)
         ON CONFLICT (member_id, screen) DO UPDATE SET
           body = excluded.body, base = excluded.base, updated_at = excluded.updated_at",
        d.member_id,
        d.screen,
        body,
        d.base,
        d.updated_at
    )
    .execute(e)
    .await
    .map_err(db_error)?;
    Ok(())
}

/// 지웠으면 `true`.
pub(crate) async fn delete<'e>(
    e: impl SqliteExecutor<'e>,
    member_id: &str,
    screen: &str,
) -> Result<bool> {
    let done = sqlx::query!(
        "DELETE FROM editor_drafts WHERE member_id = ? AND screen = ?",
        member_id,
        screen
    )
    .execute(e)
    .await
    .map_err(db_error)?;
    Ok(done.rows_affected() > 0)
}
