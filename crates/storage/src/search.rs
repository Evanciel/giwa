//! 사이트 검색 색인 (GAP C5, ADR-0050).
//!
//! - 색인 글자는 core `search::Document` 가 만든다(한글 2글자 자르기). 여기서는 넣고 찾기만 한다.
//! - 글·페이지를 쓰는 같은 트랜잭션에서 [`put_post`]·[`put_page`] 로 맞춘다. 색인에는 내용만 넣고,
//!   공개 여부·비밀글·게시판 권한은 찾을 때 `posts`/`pages` 와 이어 붙여 거른다.
//! - DB 를 열 때 [`ensure`] 가 `search_state.version` 을 보고 자르는 규칙이 바뀌었으면 다시 만든다.

use maru_core::Result;
use maru_core::model::{Page, Post};
use maru_core::search::{Document, INDEX_VERSION};
use maru_core::store::{LiteralSearchQuery, LiteralTarget, SearchQuery, SearchRef};
use sqlx::{SqliteConnection, SqlitePool};

use crate::sqlite::db_error;

async fn put(conn: &mut SqliteConnection, id: i64, doc: &Document) -> Result<()> {
    sqlx::query!("DELETE FROM search_fts WHERE rowid = ?", id)
        .execute(&mut *conn)
        .await
        .map_err(db_error)?;
    sqlx::query!(
        "INSERT INTO search_fts (rowid, title, body, tags) VALUES (?, ?, ?, ?)",
        id,
        doc.title,
        doc.body,
        doc.tags
    )
    .execute(&mut *conn)
    .await
    .map_err(db_error)?;
    Ok(())
}

/// 글 한 편의 색인을 맞춘다 (`insert_post`·`update_post` 가 부른다).
pub(crate) async fn put_post(conn: &mut SqliteConnection, p: &Post) -> Result<()> {
    let id = sqlx::query_scalar!(
        r#"INSERT INTO search_docs (post_id) VALUES (?)
           ON CONFLICT (post_id) DO UPDATE SET post_id = excluded.post_id
           RETURNING id AS "id!: i64""#,
        p.id
    )
    .fetch_one(&mut *conn)
    .await
    .map_err(db_error)?;
    put(conn, id, &Document::new(&p.title, &p.body_md, &p.tags)).await
}

/// 페이지 한 편의 색인을 맞춘다.
pub(crate) async fn put_page(conn: &mut SqliteConnection, p: &Page) -> Result<()> {
    let id = sqlx::query_scalar!(
        r#"INSERT INTO search_docs (page_id) VALUES (?)
           ON CONFLICT (page_id) DO UPDATE SET page_id = excluded.page_id
           RETURNING id AS "id!: i64""#,
        p.id
    )
    .fetch_one(&mut *conn)
    .await
    .map_err(db_error)?;
    put(conn, id, &Document::new(&p.title, &p.body_md, &[])).await
}

/// 색인을 처음부터 다시 만들고 버전을 적는다. (글 수, 페이지 수)
pub(crate) async fn rebuild(conn: &mut SqliteConnection) -> Result<(i64, i64)> {
    // FTS5 표는 WHERE 없는 DELETE 로 비운다.
    sqlx::query!("DELETE FROM search_fts WHERE rowid IS NOT NULL")
        .execute(&mut *conn)
        .await
        .map_err(db_error)?;
    sqlx::query!("DELETE FROM search_docs WHERE id IS NOT NULL")
        .execute(&mut *conn)
        .await
        .map_err(db_error)?;
    let posts = sqlx::query!(
        r#"SELECT id, title, body_md,
                  (SELECT json_group_array(t.name ORDER BY pt.position)
                     FROM post_tags pt JOIN tags t ON t.id = pt.tag_id
                    WHERE pt.post_id = posts.id) AS "tags!: String"
           FROM posts ORDER BY id"#
    )
    .fetch_all(&mut *conn)
    .await
    .map_err(db_error)?;
    let mut n_posts = 0i64;
    for r in posts {
        let tags: Vec<String> = serde_json::from_str(&r.tags).unwrap_or_default();
        let id = sqlx::query_scalar!(
            r#"INSERT INTO search_docs (post_id) VALUES (?) RETURNING id AS "id!: i64""#,
            r.id
        )
        .fetch_one(&mut *conn)
        .await
        .map_err(db_error)?;
        put(conn, id, &Document::new(&r.title, &r.body_md, &tags)).await?;
        n_posts += 1;
    }
    let pages = sqlx::query!("SELECT id, title, body_md FROM pages ORDER BY id")
        .fetch_all(&mut *conn)
        .await
        .map_err(db_error)?;
    let mut n_pages = 0i64;
    for r in pages {
        let id = sqlx::query_scalar!(
            r#"INSERT INTO search_docs (page_id) VALUES (?) RETURNING id AS "id!: i64""#,
            r.id
        )
        .fetch_one(&mut *conn)
        .await
        .map_err(db_error)?;
        put(conn, id, &Document::new(&r.title, &r.body_md, &[])).await?;
        n_pages += 1;
    }
    sqlx::query!(
        "UPDATE search_state SET version = ? WHERE id = 1",
        INDEX_VERSION
    )
    .execute(&mut *conn)
    .await
    .map_err(db_error)?;
    Ok((n_posts, n_pages))
}

/// 색인 버전이 core 와 다르면 다시 만든다. 마이그레이션 직후에 부른다.
pub(crate) async fn ensure(pool: &SqlitePool) -> Result<()> {
    let version =
        sqlx::query_scalar!(r#"SELECT version AS "v!: i64" FROM search_state WHERE id = 1"#)
            .fetch_optional(pool)
            .await
            .map_err(db_error)?;
    if version == Some(INDEX_VERSION) {
        return Ok(());
    }
    let mut tx = pool.begin_with("BEGIN IMMEDIATE").await.map_err(db_error)?;
    // 기다리는 사이 다른 프로세스가 이미 만들었을 수 있다.
    let version =
        sqlx::query_scalar!(r#"SELECT version AS "v!: i64" FROM search_state WHERE id = 1"#)
            .fetch_optional(&mut *tx)
            .await
            .map_err(db_error)?;
    if version != Some(INDEX_VERSION) {
        let (posts, pages) = rebuild(&mut tx).await?;
        tracing::info!(
            posts,
            pages,
            version = INDEX_VERSION,
            "검색 색인을 다시 만들었습니다"
        );
    }
    tx.commit().await.map_err(db_error)
}

fn refs(rows: Vec<(Option<String>, Option<String>)>) -> Vec<SearchRef> {
    rows.into_iter()
        .filter_map(|(post, page)| match (post, page) {
            (Some(id), _) => Some(SearchRef::Post(id)),
            (None, Some(id)) => Some(SearchRef::Page(id)),
            (None, None) => None,
        })
        .collect()
}

// 공개 기준(게시판 read 서열, 비밀글 제외)은 list_published 와 같은 조인이다.
pub(crate) async fn search(pool: &SqlitePool, q: &SearchQuery) -> Result<(Vec<SearchRef>, i64)> {
    let rows = sqlx::query!(
        r#"WITH m AS (
               SELECT rowid AS rid, bm25(search_fts, 5.0, 1.0, 3.0) AS score
               FROM search_fts WHERE search_fts MATCH ?1
           )
           SELECT d.post_id, d.page_id
           FROM m
           JOIN search_docs d ON d.id = m.rid
           LEFT JOIN posts p ON p.id = d.post_id
           LEFT JOIN boards b ON b.id = p.board_id
           LEFT JOIN roles r ON r.name = json_extract(b.policy, '$.read')
           LEFT JOIN pages g ON g.id = d.page_id
           WHERE (p.id IS NOT NULL AND p.status = 'published' AND p.kind = 'post'
                  AND p.is_secret = 0 AND r.rank <= ?2 AND (?3 IS NULL OR p.board_id = ?3))
              OR (g.id IS NOT NULL AND ?4 AND ?3 IS NULL AND g.status = 'published')
           ORDER BY CASE WHEN ?5 THEN 0.0 ELSE m.score END,
                    coalesce(p.published_at, g.published_at) DESC, d.id DESC
           LIMIT ?6 OFFSET ?7"#,
        q.fts,
        q.max_read_rank,
        q.board_id,
        q.pages,
        q.newest,
        q.limit,
        q.offset
    )
    .fetch_all(pool)
    .await
    .map_err(db_error)?;
    let total = sqlx::query_scalar!(
        r#"SELECT count(*) AS "n!: i64"
           FROM search_fts f
           JOIN search_docs d ON d.id = f.rowid
           LEFT JOIN posts p ON p.id = d.post_id
           LEFT JOIN boards b ON b.id = p.board_id
           LEFT JOIN roles r ON r.name = json_extract(b.policy, '$.read')
           LEFT JOIN pages g ON g.id = d.page_id
           WHERE search_fts MATCH ?1
             AND ((p.id IS NOT NULL AND p.status = 'published' AND p.kind = 'post'
                   AND p.is_secret = 0 AND r.rank <= ?2 AND (?3 IS NULL OR p.board_id = ?3))
              OR (g.id IS NOT NULL AND ?4 AND ?3 IS NULL AND g.status = 'published'))"#,
        q.fts,
        q.max_read_rank,
        q.board_id,
        q.pages
    )
    .fetch_one(pool)
    .await
    .map_err(db_error)?;
    let rows = rows.into_iter().map(|r| (r.post_id, r.page_id)).collect();
    Ok((refs(rows), total))
}

// 댓글·글쓴이 검색 (GAP Q7, ADR-0092). 공개 기준은 search 와 같다. 글자 그대로라 색인이 없고,
// 작은 사이트(SQLite 한 파일) 규모를 전제로 LIKE 로 찾는다.
pub(crate) async fn search_literal(
    pool: &SqlitePool,
    q: &LiteralSearchQuery,
) -> Result<(Vec<(String, Option<String>)>, i64)> {
    match q.target {
        LiteralTarget::Comment => {
            let rows = sqlx::query!(
                r#"SELECT p.id AS "id!",
                          (SELECT c.body FROM comments c
                            WHERE c.post_id = p.id AND c.status = 'published'
                              AND c.body LIKE ?3 ESCAPE '\'
                            ORDER BY c.created_at DESC LIMIT 1) AS "body: String"
                   FROM posts p
                   JOIN boards b ON b.id = p.board_id
                   JOIN roles r ON r.name = json_extract(b.policy, '$.read')
                   WHERE p.status = 'published' AND p.kind = 'post' AND p.is_secret = 0
                     AND r.rank <= ?1 AND (?2 IS NULL OR p.board_id = ?2)
                     AND EXISTS (SELECT 1 FROM comments c
                                  WHERE c.post_id = p.id AND c.status = 'published'
                                    AND c.body LIKE ?3 ESCAPE '\')
                   ORDER BY p.published_at DESC, p.id DESC
                   LIMIT ?4 OFFSET ?5"#,
                q.max_read_rank,
                q.board_id,
                q.pattern,
                q.limit,
                q.offset
            )
            .fetch_all(pool)
            .await
            .map_err(db_error)?;
            let total = sqlx::query_scalar!(
                r#"SELECT count(*) AS "n!: i64"
                   FROM posts p
                   JOIN boards b ON b.id = p.board_id
                   JOIN roles r ON r.name = json_extract(b.policy, '$.read')
                   WHERE p.status = 'published' AND p.kind = 'post' AND p.is_secret = 0
                     AND r.rank <= ?1 AND (?2 IS NULL OR p.board_id = ?2)
                     AND EXISTS (SELECT 1 FROM comments c
                                  WHERE c.post_id = p.id AND c.status = 'published'
                                    AND c.body LIKE ?3 ESCAPE '\')"#,
                q.max_read_rank,
                q.board_id,
                q.pattern
            )
            .fetch_one(pool)
            .await
            .map_err(db_error)?;
            Ok((
                rows.into_iter().map(|r| (r.id, Some(r.body))).collect(),
                total,
            ))
        }
        LiteralTarget::Author => {
            // 탈퇴·비활성 회원의 이름으로는 찾지 않는다.
            let rows = sqlx::query_scalar!(
                r#"SELECT p.id AS "id!"
                   FROM posts p
                   JOIN boards b ON b.id = p.board_id
                   JOIN roles r ON r.name = json_extract(b.policy, '$.read')
                   LEFT JOIN members m ON m.id = p.author_id
                   WHERE p.status = 'published' AND p.kind = 'post' AND p.is_secret = 0
                     AND r.rank <= ?1 AND (?2 IS NULL OR p.board_id = ?2)
                     AND ((p.guest_name IS NOT NULL AND p.guest_name LIKE ?3 ESCAPE '\')
                       OR (p.guest_name IS NULL AND m.status = 'active' AND m.withdrawn_at IS NULL
                           AND m.display_name LIKE ?3 ESCAPE '\'))
                   ORDER BY p.published_at DESC, p.id DESC
                   LIMIT ?4 OFFSET ?5"#,
                q.max_read_rank,
                q.board_id,
                q.pattern,
                q.limit,
                q.offset
            )
            .fetch_all(pool)
            .await
            .map_err(db_error)?;
            let total = sqlx::query_scalar!(
                r#"SELECT count(*) AS "n!: i64"
                   FROM posts p
                   JOIN boards b ON b.id = p.board_id
                   JOIN roles r ON r.name = json_extract(b.policy, '$.read')
                   LEFT JOIN members m ON m.id = p.author_id
                   WHERE p.status = 'published' AND p.kind = 'post' AND p.is_secret = 0
                     AND r.rank <= ?1 AND (?2 IS NULL OR p.board_id = ?2)
                     AND ((p.guest_name IS NOT NULL AND p.guest_name LIKE ?3 ESCAPE '\')
                       OR (p.guest_name IS NULL AND m.status = 'active' AND m.withdrawn_at IS NULL
                           AND m.display_name LIKE ?3 ESCAPE '\'))"#,
                q.max_read_rank,
                q.board_id,
                q.pattern
            )
            .fetch_one(pool)
            .await
            .map_err(db_error)?;
            Ok((rows.into_iter().map(|id| (id, None)).collect(), total))
        }
    }
}
