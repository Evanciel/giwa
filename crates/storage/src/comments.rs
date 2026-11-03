//! 댓글 `comments` 와 캡차 도전 `challenges` (GAP M5·M6·M10, ADR-0052).
//! 상태 목록·글 ID 목록은 JSON 배열 하나로 묶어 `json_each` 로 푼다 (컴파일 타임 검증 매크로 유지).

use maru_core::model::{Comment, CommentAuthor, CommentStatus};
use maru_core::{Error, Result};
use sqlx::{Sqlite, SqliteExecutor, Transaction};

use crate::sqlite::db_error;

pub(crate) use maru_core::store::CommentQuery;

trait BoardsJson {
    fn boards_json(&self) -> Option<String>;
}

impl BoardsJson for CommentQuery<'_> {
    fn boards_json(&self) -> Option<String> {
        self.board_ids
            .map(|ids| serde_json::to_string(ids).unwrap_or_else(|_| "[]".into()))
    }
}

struct Row {
    id: String,
    post_id: String,
    parent_id: Option<String>,
    reply_to_id: Option<String>,
    status: String,
    body: String,
    author_id: Option<String>,
    guest_name: Option<String>,
    guest_pw_hash: Option<String>,
    actor_type: String,
    created_at: String,
    updated_at: String,
}

impl TryFrom<Row> for Comment {
    type Error = Error;

    fn try_from(r: Row) -> Result<Self> {
        Ok(Comment {
            status: CommentStatus::parse(&r.status)
                .ok_or_else(|| Error::internal(format!("알 수 없는 댓글 상태: {}", r.status)))?,
            actor_type: CommentAuthor::parse(&r.actor_type).ok_or_else(|| {
                Error::internal(format!("알 수 없는 댓글 작성자: {}", r.actor_type))
            })?,
            id: r.id,
            post_id: r.post_id,
            parent_id: r.parent_id,
            reply_to_id: r.reply_to_id,
            body: r.body,
            author_id: r.author_id,
            guest_name: r.guest_name,
            guest_pw_hash: r.guest_pw_hash,
            created_at: r.created_at,
            updated_at: r.updated_at,
        })
    }
}

fn rows(rows: Vec<Row>) -> Result<Vec<Comment>> {
    rows.into_iter().map(Comment::try_from).collect()
}

fn statuses_json(statuses: &[CommentStatus]) -> String {
    let names: Vec<&str> = statuses.iter().map(|s| s.as_str()).collect();
    serde_json::to_string(&names).unwrap_or_else(|_| "[]".into())
}

pub(crate) async fn by_id<'e>(e: impl SqliteExecutor<'e>, id: &str) -> Result<Option<Comment>> {
    sqlx::query_as!(
        Row,
        "SELECT id, post_id, parent_id, reply_to_id, status, body, author_id, guest_name,
                guest_pw_hash, actor_type, created_at, updated_at
         FROM comments WHERE id = ?",
        id
    )
    .fetch_optional(e)
    .await
    .map_err(db_error)?
    .map(Comment::try_from)
    .transpose()
}

pub(crate) async fn of_post<'e>(
    e: impl SqliteExecutor<'e>,
    post_id: &str,
    statuses: &[CommentStatus],
) -> Result<Vec<Comment>> {
    let statuses = statuses_json(statuses);
    rows(
        sqlx::query_as!(
            Row,
            "SELECT id, post_id, parent_id, reply_to_id, status, body, author_id, guest_name,
                    guest_pw_hash, actor_type, created_at, updated_at
             FROM comments
             WHERE post_id = ? AND status IN (SELECT value FROM json_each(?))
             ORDER BY created_at, id",
            post_id,
            statuses
        )
        .fetch_all(e)
        .await
        .map_err(db_error)?,
    )
}

pub(crate) async fn list<'e>(
    e: impl SqliteExecutor<'e>,
    q: &CommentQuery<'_>,
    limit: i64,
    offset: i64,
) -> Result<Vec<Comment>> {
    let status = q.status.map(CommentStatus::as_str);
    let boards = q.boards_json();
    rows(
        sqlx::query_as!(
            Row,
            "SELECT c.id, c.post_id, c.parent_id, c.reply_to_id, c.status, c.body, c.author_id,
                    c.guest_name, c.guest_pw_hash, c.actor_type, c.created_at, c.updated_at
             FROM comments c JOIN posts p ON p.id = c.post_id
             WHERE (?1 IS NULL OR c.status = ?1) AND (?2 IS NULL OR c.post_id = ?2)
               AND (?3 IS NULL OR p.board_id IN (SELECT value FROM json_each(?3)))
             ORDER BY c.created_at DESC, c.id DESC LIMIT ?4 OFFSET ?5",
            status,
            q.post_id,
            boards,
            limit,
            offset
        )
        .fetch_all(e)
        .await
        .map_err(db_error)?,
    )
}

pub(crate) async fn count<'e>(e: impl SqliteExecutor<'e>, q: &CommentQuery<'_>) -> Result<i64> {
    let status = q.status.map(CommentStatus::as_str);
    let boards = q.boards_json();
    sqlx::query_scalar!(
        "SELECT count(*) FROM comments c JOIN posts p ON p.id = c.post_id
         WHERE (?1 IS NULL OR c.status = ?1) AND (?2 IS NULL OR c.post_id = ?2)
           AND (?3 IS NULL OR p.board_id IN (SELECT value FROM json_each(?3)))",
        status,
        q.post_id,
        boards
    )
    .fetch_one(e)
    .await
    .map_err(db_error)
}

/// 글마다 공개 댓글 수. 댓글이 없는 글은 빠진다.
pub(crate) async fn counts<'e>(
    e: impl SqliteExecutor<'e>,
    post_ids: &[String],
) -> Result<Vec<(String, i64)>> {
    let ids = serde_json::to_string(post_ids).unwrap_or_else(|_| "[]".into());
    let rows = sqlx::query!(
        r#"SELECT post_id AS "post_id!", count(*) AS "n!: i64" FROM comments
           WHERE status = 'published' AND post_id IN (SELECT value FROM json_each(?))
           GROUP BY post_id"#,
        ids
    )
    .fetch_all(e)
    .await
    .map_err(db_error)?;
    Ok(rows.into_iter().map(|r| (r.post_id, r.n)).collect())
}

pub(crate) async fn insert(tx: &mut Transaction<'static, Sqlite>, c: &Comment) -> Result<()> {
    let status = c.status.as_str();
    let actor = c.actor_type.as_str();
    sqlx::query!(
        "INSERT INTO comments (id, post_id, parent_id, reply_to_id, status, body, author_id,
                               guest_name, guest_pw_hash, actor_type, created_at, updated_at)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        c.id,
        c.post_id,
        c.parent_id,
        c.reply_to_id,
        status,
        c.body,
        c.author_id,
        c.guest_name,
        c.guest_pw_hash,
        actor,
        c.created_at,
        c.updated_at
    )
    .execute(&mut **tx)
    .await
    .map_err(db_error)?;
    Ok(())
}

/// 바뀔 수 있는 것은 상태·본문·고친 때뿐이다.
pub(crate) async fn update(tx: &mut Transaction<'static, Sqlite>, c: &Comment) -> Result<()> {
    let status = c.status.as_str();
    let done = sqlx::query!(
        "UPDATE comments SET status = ?, body = ?, updated_at = ? WHERE id = ?",
        status,
        c.body,
        c.updated_at,
        c.id
    )
    .execute(&mut **tx)
    .await
    .map_err(db_error)?;
    if done.rows_affected() == 0 {
        return Err(Error::internal(format!("고칠 댓글이 없음: {}", c.id)));
    }
    Ok(())
}

/// 지우지 않은 대댓글 수 (숨김·대기·스팸 포함).
pub(crate) async fn live_replies(
    tx: &mut Transaction<'static, Sqlite>,
    parent_id: &str,
) -> Result<i64> {
    sqlx::query_scalar!(
        "SELECT count(*) FROM comments WHERE parent_id = ? AND status <> 'deleted'",
        parent_id
    )
    .fetch_one(&mut **tx)
    .await
    .map_err(db_error)
}

pub(crate) async fn insert_challenge<'e>(
    e: impl SqliteExecutor<'e> + Copy,
    id_hash: &str,
    answer: &str,
    now: &str,
    expires_at: &str,
) -> Result<()> {
    sqlx::query!("DELETE FROM challenges WHERE expires_at <= ?", now)
        .execute(e)
        .await
        .map_err(db_error)?;
    sqlx::query!(
        "INSERT INTO challenges (id_hash, answer, issued_at, expires_at) VALUES (?, ?, ?, ?)",
        id_hash,
        answer,
        now,
        expires_at
    )
    .execute(e)
    .await
    .map_err(db_error)?;
    Ok(())
}

/// 유효한 도전을 꺼내며 지운다. 같은 값을 두 번 내도 한 번만 `Some((답, 낸 때))`.
pub(crate) async fn take_challenge<'e>(
    e: impl SqliteExecutor<'e>,
    id_hash: &str,
    now: &str,
) -> Result<Option<(String, String)>> {
    let row = sqlx::query!(
        "DELETE FROM challenges WHERE id_hash = ? AND expires_at > ? RETURNING answer, issued_at",
        id_hash,
        now
    )
    .fetch_optional(e)
    .await
    .map_err(db_error)?;
    Ok(row.map(|r| (r.answer, r.issued_at)))
}
