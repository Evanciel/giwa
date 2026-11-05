//! 공개 프로필 (GAP Q2, ADR-0087): 자기소개 `member_profiles` 와 회원의 공개 글.
//! 「공개」 기준은 게시판 목록과 같다: 발행된 글, 비밀글 아님, 게시판 read 서열 이하.

use maru_core::Result;
use sqlx::SqliteExecutor;

use crate::sqlite::db_error;

pub(crate) async fn bio<'e>(e: impl SqliteExecutor<'e>, member_id: &str) -> Result<Option<String>> {
    sqlx::query_scalar!(
        "SELECT bio FROM member_profiles WHERE member_id = ?",
        member_id
    )
    .fetch_optional(e)
    .await
    .map_err(db_error)
}

/// 비운 자기소개는 줄을 지운다.
pub(crate) async fn set_bio<'e>(
    e: impl SqliteExecutor<'e>,
    member_id: &str,
    bio: &str,
    now: &str,
) -> Result<()> {
    if bio.is_empty() {
        sqlx::query!("DELETE FROM member_profiles WHERE member_id = ?", member_id)
            .execute(e)
            .await
            .map_err(db_error)?;
    } else {
        sqlx::query!(
            "INSERT INTO member_profiles (member_id, bio, updated_at) VALUES (?1, ?2, ?3)
             ON CONFLICT (member_id) DO UPDATE SET bio = ?2, updated_at = ?3",
            member_id,
            bio,
            now
        )
        .execute(e)
        .await
        .map_err(db_error)?;
    }
    Ok(())
}

/// 회원 서명 (GAP Q17)
pub(crate) async fn signature<'e>(
    e: impl SqliteExecutor<'e>,
    member_id: &str,
) -> Result<Option<String>> {
    sqlx::query_scalar!(
        "SELECT signature FROM member_signatures WHERE member_id = ?",
        member_id
    )
    .fetch_optional(e)
    .await
    .map_err(db_error)
}

/// 비운 서명은 줄을 지운다.
pub(crate) async fn set_signature<'e>(
    e: impl SqliteExecutor<'e>,
    member_id: &str,
    signature: &str,
    now: &str,
) -> Result<()> {
    if signature.is_empty() {
        sqlx::query!(
            "DELETE FROM member_signatures WHERE member_id = ?",
            member_id
        )
        .execute(e)
        .await
        .map_err(db_error)?;
    } else {
        sqlx::query!(
            "INSERT INTO member_signatures (member_id, signature, updated_at) VALUES (?1, ?2, ?3)
             ON CONFLICT (member_id) DO UPDATE SET signature = ?2, updated_at = ?3",
            member_id,
            signature,
            now
        )
        .execute(e)
        .await
        .map_err(db_error)?;
    }
    Ok(())
}

/// 회원의 공개 글 ID, 새것부터.
pub(crate) async fn public_post_ids<'e>(
    e: impl SqliteExecutor<'e>,
    member_id: &str,
    max_read_rank: i64,
    limit: i64,
) -> Result<Vec<String>> {
    sqlx::query_scalar!(
        r#"SELECT p.id AS "id!" FROM posts p
           JOIN boards b ON b.id = p.board_id
           JOIN roles r ON r.name = json_extract(b.policy, '$.read')
           WHERE p.author_id = ?1 AND p.guest_name IS NULL
             AND p.status = 'published' AND p.kind = 'post' AND p.is_secret = 0
             AND r.rank <= ?2
           ORDER BY p.published_at DESC, p.id DESC LIMIT ?3"#,
        member_id,
        max_read_rank,
        limit
    )
    .fetch_all(e)
    .await
    .map_err(db_error)
}

pub(crate) async fn count_public_posts<'e>(
    e: impl SqliteExecutor<'e>,
    member_id: &str,
    max_read_rank: i64,
) -> Result<i64> {
    sqlx::query_scalar!(
        r#"SELECT count(*) AS "n!: i64" FROM posts p
           JOIN boards b ON b.id = p.board_id
           JOIN roles r ON r.name = json_extract(b.policy, '$.read')
           WHERE p.author_id = ?1 AND p.guest_name IS NULL
             AND p.status = 'published' AND p.kind = 'post' AND p.is_secret = 0
             AND r.rank <= ?2"#,
        member_id,
        max_read_rank
    )
    .fetch_one(e)
    .await
    .map_err(db_error)
}
