//! `maru_core::Store` 의 SQLite 구현. 쿼리는 모두 컴파일 타임 검증 매크로로 쓴다.

use async_trait::async_trait;
use maru_core::actor::{AuthorType, Permission, Scope};
use maru_core::audit::AuditRecord;
use maru_core::model::{Board, BoardPolicy, Post, PostKind, PostStatus, Revision, Role, Token};
use maru_core::store::{PostFilter, PublishedQuery, Store, WriteTx};
use maru_core::{Code, Error, Result};
use sqlx::{Sqlite, SqliteExecutor, Transaction};

use crate::db::Db;

#[derive(Debug, Clone)]
pub struct SqliteStore {
    db: Db,
}

impl SqliteStore {
    pub fn new(db: Db) -> Self {
        Self { db }
    }

    pub fn db(&self) -> &Db {
        &self.db
    }
}

pub struct SqliteWriteTx {
    tx: Transaction<'static, Sqlite>,
}

// ---- 행 타입과 변환 ----

struct BoardRow {
    id: String,
    slug: String,
    name: String,
    kind: String,
    policy: String,
    sort_order: i64,
    created_at: String,
    updated_at: String,
}

impl TryFrom<BoardRow> for Board {
    type Error = Error;

    fn try_from(r: BoardRow) -> Result<Self> {
        let policy: BoardPolicy = serde_json::from_str(&r.policy).map_err(Error::internal)?;
        Ok(Board {
            id: r.id,
            slug: r.slug,
            name: r.name,
            kind: r.kind,
            policy,
            sort_order: r.sort_order,
            created_at: r.created_at,
            updated_at: r.updated_at,
        })
    }
}

struct PostRow {
    id: String,
    board_id: String,
    kind: String,
    status: String,
    title: String,
    slug: String,
    body_md: String,
    excerpt: Option<String>,
    author_id: Option<String>,
    actor_type: String,
    published_at: Option<String>,
    created_at: String,
    updated_at: String,
}

impl TryFrom<PostRow> for Post {
    type Error = Error;

    fn try_from(r: PostRow) -> Result<Self> {
        Ok(Post {
            kind: PostKind::parse(&r.kind).ok_or_else(|| unknown("posts.kind", &r.kind))?,
            status: PostStatus::parse(&r.status)
                .ok_or_else(|| unknown("posts.status", &r.status))?,
            actor_type: AuthorType::parse(&r.actor_type)
                .ok_or_else(|| unknown("posts.actor_type", &r.actor_type))?,
            id: r.id,
            board_id: r.board_id,
            title: r.title,
            slug: r.slug,
            body_md: r.body_md,
            excerpt: r.excerpt,
            author_id: r.author_id,
            published_at: r.published_at,
            created_at: r.created_at,
            updated_at: r.updated_at,
        })
    }
}

struct RevisionRow {
    id: String,
    post_id: Option<String>,
    page_id: Option<String>,
    title: String,
    body_md: String,
    actor_type: String,
    actor_id: Option<String>,
    created_at: String,
}

impl TryFrom<RevisionRow> for Revision {
    type Error = Error;

    fn try_from(r: RevisionRow) -> Result<Self> {
        Ok(Revision {
            actor_type: AuthorType::parse(&r.actor_type)
                .ok_or_else(|| unknown("revisions.actor_type", &r.actor_type))?,
            id: r.id,
            post_id: r.post_id,
            page_id: r.page_id,
            title: r.title,
            body_md: r.body_md,
            actor_id: r.actor_id,
            created_at: r.created_at,
        })
    }
}

struct TokenRow {
    id: String,
    name: String,
    token_hash: String,
    scopes: String,
    board_ids: Option<String>,
    expires_at: Option<String>,
    last_used_at: Option<String>,
    revoked_at: Option<String>,
    created_at: String,
    updated_at: String,
}

impl TryFrom<TokenRow> for Token {
    type Error = Error;

    fn try_from(r: TokenRow) -> Result<Self> {
        let scopes: Vec<Scope> = serde_json::from_str(&r.scopes).map_err(Error::internal)?;
        let board_ids = match r.board_ids {
            Some(s) => Some(serde_json::from_str(&s).map_err(Error::internal)?),
            None => None,
        };
        Ok(Token {
            id: r.id,
            name: r.name,
            token_hash: r.token_hash,
            scopes,
            board_ids,
            expires_at: r.expires_at,
            last_used_at: r.last_used_at,
            revoked_at: r.revoked_at,
            created_at: r.created_at,
            updated_at: r.updated_at,
        })
    }
}

fn unknown(column: &str, value: &str) -> Error {
    Error::internal(format!("{column} 에 알 수 없는 값: {value}"))
}

fn db_error(e: sqlx::Error) -> Error {
    if e.as_database_error()
        .is_some_and(|d| d.is_unique_violation())
    {
        return Error::new(Code::Conflict).with_source(e);
    }
    Error::internal(e)
}

fn json<T: serde::Serialize>(v: &T) -> Result<String> {
    serde_json::to_string(v).map_err(Error::internal)
}

fn convert<R, T: TryFrom<R, Error = Error>>(rows: Vec<R>) -> Result<Vec<T>> {
    rows.into_iter().map(T::try_from).collect()
}

// ---- 읽기·쓰기 공용 쿼리 ----

async fn board_by_id<'e>(e: impl SqliteExecutor<'e>, id: &str) -> Result<Option<Board>> {
    sqlx::query_as!(
        BoardRow,
        "SELECT id, slug, name, kind, policy, sort_order, created_at, updated_at
         FROM boards WHERE id = ?",
        id
    )
    .fetch_optional(e)
    .await
    .map_err(db_error)?
    .map(Board::try_from)
    .transpose()
}

async fn board_by_slug<'e>(e: impl SqliteExecutor<'e>, slug: &str) -> Result<Option<Board>> {
    sqlx::query_as!(
        BoardRow,
        "SELECT id, slug, name, kind, policy, sort_order, created_at, updated_at
         FROM boards WHERE slug = ?",
        slug
    )
    .fetch_optional(e)
    .await
    .map_err(db_error)?
    .map(Board::try_from)
    .transpose()
}

async fn post_by_id<'e>(e: impl SqliteExecutor<'e>, id: &str) -> Result<Option<Post>> {
    sqlx::query_as!(
        PostRow,
        "SELECT id, board_id, kind, status, title, slug, body_md, excerpt, author_id,
                actor_type, published_at, created_at, updated_at
         FROM posts WHERE id = ?",
        id
    )
    .fetch_optional(e)
    .await
    .map_err(db_error)?
    .map(Post::try_from)
    .transpose()
}

#[async_trait]
impl Store for SqliteStore {
    async fn begin_write(&self) -> Result<Box<dyn WriteTx>> {
        let tx = self
            .db
            .write
            .begin_with("BEGIN IMMEDIATE")
            .await
            .map_err(db_error)?;
        Ok(Box::new(SqliteWriteTx { tx }))
    }

    async fn role_by_name(&self, name: &str) -> Result<Option<Role>> {
        let row = sqlx::query!(
            "SELECT id, name, rank, permissions FROM roles WHERE name = ?",
            name
        )
        .fetch_optional(&self.db.read)
        .await
        .map_err(db_error)?;
        let Some(r) = row else { return Ok(None) };
        let names: Vec<String> = serde_json::from_str(&r.permissions).map_err(Error::internal)?;
        Ok(Some(Role {
            id: r.id,
            name: r.name,
            rank: r.rank,
            permissions: names.iter().filter_map(|n| Permission::parse(n)).collect(),
        }))
    }

    async fn token_by_hash(&self, hash: &str) -> Result<Option<Token>> {
        sqlx::query_as!(
            TokenRow,
            "SELECT id, name, token_hash, scopes, board_ids, expires_at, last_used_at,
                    revoked_at, created_at, updated_at
             FROM tokens WHERE token_hash = ?",
            hash
        )
        .fetch_optional(&self.db.read)
        .await
        .map_err(db_error)?
        .map(Token::try_from)
        .transpose()
    }

    async fn touch_token(&self, id: &str, at: &str) -> Result<()> {
        sqlx::query!("UPDATE tokens SET last_used_at = ? WHERE id = ?", at, id)
            .execute(&self.db.write)
            .await
            .map_err(db_error)?;
        Ok(())
    }

    async fn board_by_id(&self, id: &str) -> Result<Option<Board>> {
        board_by_id(&self.db.read, id).await
    }

    async fn board_by_slug(&self, slug: &str) -> Result<Option<Board>> {
        board_by_slug(&self.db.read, slug).await
    }

    async fn list_boards(&self) -> Result<Vec<Board>> {
        let rows = sqlx::query_as!(
            BoardRow,
            "SELECT id, slug, name, kind, policy, sort_order, created_at, updated_at
             FROM boards ORDER BY sort_order, slug"
        )
        .fetch_all(&self.db.read)
        .await
        .map_err(db_error)?;
        convert(rows)
    }

    async fn post_by_id(&self, id: &str) -> Result<Option<Post>> {
        post_by_id(&self.db.read, id).await
    }

    async fn post_by_slug(&self, board_id: &str, slug: &str) -> Result<Option<Post>> {
        sqlx::query_as!(
            PostRow,
            "SELECT id, board_id, kind, status, title, slug, body_md, excerpt, author_id,
                    actor_type, published_at, created_at, updated_at
             FROM posts WHERE board_id = ? AND slug = ?",
            board_id,
            slug
        )
        .fetch_optional(&self.db.read)
        .await
        .map_err(db_error)?
        .map(Post::try_from)
        .transpose()
    }

    // 공개 범위의 SQL 은 이 두 쿼리에만 있다 (PHASE1_PLAN §4.6).
    // 게시판 read 정책의 역할 이름을 roles 와 조인해 서열을 비교한다. 모르는 역할이면 빠진다.
    async fn list_published(&self, q: &PublishedQuery) -> Result<Vec<Post>> {
        let rows = sqlx::query_as!(
            PostRow,
            r#"SELECT p.id AS "id!", p.board_id AS "board_id!", p.kind AS "kind!",
                      p.status AS "status!", p.title AS "title!", p.slug AS "slug!",
                      p.body_md AS "body_md!", p.excerpt, p.author_id,
                      p.actor_type AS "actor_type!", p.published_at,
                      p.created_at AS "created_at!", p.updated_at AS "updated_at!"
               FROM posts p
               JOIN boards b ON b.id = p.board_id
               JOIN roles r ON r.name = json_extract(b.policy, '$.read')
               WHERE p.status = 'published' AND p.kind = 'post'
                 AND r.rank <= ?1 AND (?2 IS NULL OR p.board_id = ?2)
               ORDER BY p.published_at DESC, p.id DESC
               LIMIT ?3 OFFSET ?4"#,
            q.max_read_rank,
            q.board_id,
            q.limit,
            q.offset
        )
        .fetch_all(&self.db.read)
        .await
        .map_err(db_error)?;
        convert(rows)
    }

    async fn count_published(&self, q: &PublishedQuery) -> Result<i64> {
        sqlx::query_scalar!(
            r#"SELECT count(*) AS "n!: i64"
               FROM posts p
               JOIN boards b ON b.id = p.board_id
               JOIN roles r ON r.name = json_extract(b.policy, '$.read')
               WHERE p.status = 'published' AND p.kind = 'post'
                 AND r.rank <= ?1 AND (?2 IS NULL OR p.board_id = ?2)"#,
            q.max_read_rank,
            q.board_id
        )
        .fetch_one(&self.db.read)
        .await
        .map_err(db_error)
    }

    async fn list_posts(&self, q: &PostFilter) -> Result<Vec<Post>> {
        let status = q.status.map(PostStatus::as_str);
        let rows = sqlx::query_as!(
            PostRow,
            "SELECT id, board_id, kind, status, title, slug, body_md, excerpt, author_id,
                    actor_type, published_at, created_at, updated_at
             FROM posts
             WHERE kind = 'post' AND (?1 IS NULL OR board_id = ?1) AND (?2 IS NULL OR status = ?2)
             ORDER BY updated_at DESC, id DESC
             LIMIT ?3 OFFSET ?4",
            q.board_id,
            status,
            q.limit,
            q.offset
        )
        .fetch_all(&self.db.read)
        .await
        .map_err(db_error)?;
        convert(rows)
    }

    async fn count_posts(&self, q: &PostFilter) -> Result<i64> {
        let status = q.status.map(PostStatus::as_str);
        sqlx::query_scalar!(
            r#"SELECT count(*) AS "n!: i64" FROM posts
               WHERE kind = 'post' AND (?1 IS NULL OR board_id = ?1) AND (?2 IS NULL OR status = ?2)"#,
            q.board_id,
            status
        )
        .fetch_one(&self.db.read)
        .await
        .map_err(db_error)
    }

    async fn revisions_of_post(&self, post_id: &str) -> Result<Vec<Revision>> {
        let rows = sqlx::query_as!(
            RevisionRow,
            "SELECT id, post_id, page_id, title, body_md, actor_type, actor_id, created_at
             FROM revisions WHERE post_id = ? ORDER BY created_at, id",
            post_id
        )
        .fetch_all(&self.db.read)
        .await
        .map_err(db_error)?;
        convert(rows)
    }
}

#[async_trait]
impl WriteTx for SqliteWriteTx {
    async fn insert_audit(&mut self, rec: &AuditRecord) -> Result<()> {
        let diff = json(&rec.diff)?;
        sqlx::query!(
            "INSERT INTO audit_log (id, actor_type, actor_id, action, target_type, target_id, diff, at)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
            rec.id,
            rec.actor_type,
            rec.actor_id,
            rec.action,
            rec.target_type,
            rec.target_id,
            diff,
            rec.at
        )
        .execute(&mut *self.tx)
        .await
        .map_err(db_error)?;
        Ok(())
    }

    async fn commit(self: Box<Self>) -> Result<()> {
        self.tx.commit().await.map_err(db_error)
    }

    async fn board_by_id(&mut self, id: &str) -> Result<Option<Board>> {
        board_by_id(&mut *self.tx, id).await
    }

    async fn board_by_slug(&mut self, slug: &str) -> Result<Option<Board>> {
        board_by_slug(&mut *self.tx, slug).await
    }

    async fn insert_board(&mut self, b: &Board) -> Result<()> {
        let policy = json(&b.policy)?;
        sqlx::query!(
            "INSERT INTO boards (id, slug, name, kind, policy, sort_order, created_at, updated_at)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
            b.id,
            b.slug,
            b.name,
            b.kind,
            policy,
            b.sort_order,
            b.created_at,
            b.updated_at
        )
        .execute(&mut *self.tx)
        .await
        .map_err(db_error)?;
        Ok(())
    }

    async fn post_by_id(&mut self, id: &str) -> Result<Option<Post>> {
        post_by_id(&mut *self.tx, id).await
    }

    async fn slug_taken(
        &mut self,
        board_id: &str,
        slug: &str,
        except: Option<&str>,
    ) -> Result<bool> {
        let n = sqlx::query_scalar!(
            r#"SELECT count(*) AS "n!: i64" FROM posts
               WHERE board_id = ?1 AND slug = ?2 AND (?3 IS NULL OR id <> ?3)"#,
            board_id,
            slug,
            except
        )
        .fetch_one(&mut *self.tx)
        .await
        .map_err(db_error)?;
        Ok(n > 0)
    }

    async fn insert_post(&mut self, p: &Post) -> Result<()> {
        let (kind, status, actor_type) =
            (p.kind.as_str(), p.status.as_str(), p.actor_type.as_str());
        sqlx::query!(
            "INSERT INTO posts (id, board_id, kind, status, title, slug, body_md, excerpt,
                                author_id, actor_type, published_at, created_at, updated_at)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
            p.id,
            p.board_id,
            kind,
            status,
            p.title,
            p.slug,
            p.body_md,
            p.excerpt,
            p.author_id,
            actor_type,
            p.published_at,
            p.created_at,
            p.updated_at
        )
        .execute(&mut *self.tx)
        .await
        .map_err(db_error)?;
        Ok(())
    }

    async fn update_post(&mut self, p: &Post) -> Result<()> {
        let status = p.status.as_str();
        let res = sqlx::query!(
            "UPDATE posts SET board_id = ?, status = ?, title = ?, slug = ?, body_md = ?,
                              excerpt = ?, published_at = ?, updated_at = ?
             WHERE id = ?",
            p.board_id,
            status,
            p.title,
            p.slug,
            p.body_md,
            p.excerpt,
            p.published_at,
            p.updated_at,
            p.id
        )
        .execute(&mut *self.tx)
        .await
        .map_err(db_error)?;
        if res.rows_affected() != 1 {
            return Err(Error::new(Code::NotFound).arg("id", p.id.clone()));
        }
        Ok(())
    }

    async fn insert_revision(&mut self, r: &Revision) -> Result<()> {
        let actor_type = r.actor_type.as_str();
        sqlx::query!(
            "INSERT INTO revisions (id, post_id, page_id, title, body_md, actor_type, actor_id, created_at)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
            r.id,
            r.post_id,
            r.page_id,
            r.title,
            r.body_md,
            actor_type,
            r.actor_id,
            r.created_at
        )
        .execute(&mut *self.tx)
        .await
        .map_err(db_error)?;
        Ok(())
    }

    async fn insert_token(&mut self, t: &Token) -> Result<()> {
        let scopes = json(&t.scopes)?;
        let board_ids = t.board_ids.as_ref().map(json).transpose()?;
        sqlx::query!(
            "INSERT INTO tokens (id, name, token_hash, scopes, board_ids, expires_at,
                                 last_used_at, revoked_at, created_at, updated_at)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
            t.id,
            t.name,
            t.token_hash,
            scopes,
            board_ids,
            t.expires_at,
            t.last_used_at,
            t.revoked_at,
            t.created_at,
            t.updated_at
        )
        .execute(&mut *self.tx)
        .await
        .map_err(db_error)?;
        Ok(())
    }
}
