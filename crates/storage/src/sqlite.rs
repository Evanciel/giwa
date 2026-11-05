//! `maru_core::Store` 의 SQLite 구현. 쿼리는 모두 컴파일 타임 검증 매크로로 쓴다.

use async_trait::async_trait;
use maru_core::actor::{AuthorType, Permission, Scope};
use maru_core::audit::AuditRecord;
use maru_core::model::{
    Attachment, AuditEntry, AuditEvent, Board, BoardPolicy, Category, Comment, CommentStatus, Font,
    FontFile, Listing, LoginTicket, MailOut, MailStatus, Media, MediaVariant, Member, MemberStatus,
    ModuleFailure, ModuleRow, Page, PageReq, PageStatus, Post, PostKind, PostStatus, PostTemplate,
    Revision, Role, Session, StatBatch, StatDay, StatPage, StatReferrer, Tag, TicketPurpose, Token,
    TotpChallenge, Widget,
};
use maru_core::model::{LinkPreview, LinkStatus, MemberActivity, MemberTotp};
use maru_core::store::{
    AuditFilter, CommentQuery, LiteralSearchQuery, MenuRow, ModuleMigration, PopularQuery,
    PostFilter, PublishedQuery, SearchQuery, SearchRef, Store, WikiPage, WriteTx,
};
use maru_core::{Code, Error, Result, clock, slug, urls};
use serde_json::Value;
use sqlx::{Sqlite, SqliteConnection, SqliteExecutor, Transaction};

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
    layout: String,
    list_order: String,
    policy: String,
    sort_order: i64,
    field_schema: String,
    description: String,
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
            layout: r.layout,
            list_order: r.list_order,
            policy,
            sort_order: r.sort_order,
            field_schema: serde_json::from_str(&r.field_schema).map_err(Error::internal)?,
            description: r.description,
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
    scheduled_at: Option<String>,
    cover: Option<String>,
    category_id: Option<String>,
    is_notice: i64,
    is_secret: i64,
    /// `json_group_array` 결과 (태그가 없으면 `[]`).
    tags: String,
    view_count: i64,
    guest_name: Option<String>,
    guest_pw_hash: Option<String>,
    parent_id: Option<String>,
    thread_id: Option<String>,
    thread_path: String,
    thread_owner: Option<String>,
    meta: String,
    last_activity_at: Option<String>,
    accepted_comment_id: Option<String>,
    is_locked: i64,
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
            scheduled_at: r.scheduled_at,
            cover: r.cover,
            category_id: r.category_id,
            is_notice: r.is_notice != 0,
            is_secret: r.is_secret != 0,
            tags: serde_json::from_str(&r.tags).map_err(Error::internal)?,
            view_count: r.view_count,
            guest_name: r.guest_name,
            guest_pw_hash: r.guest_pw_hash,
            parent_id: r.parent_id,
            thread_id: r.thread_id,
            thread_path: r.thread_path,
            thread_owner: r.thread_owner,
            meta: serde_json::from_str(&r.meta).map_err(Error::internal)?,
            last_activity_at: r.last_activity_at,
            accepted_comment_id: r.accepted_comment_id,
            is_locked: r.is_locked != 0,
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

pub(crate) fn db_error(e: sqlx::Error) -> Error {
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

struct PageRow {
    id: String,
    slug: String,
    title: String,
    body_md: String,
    status: String,
    layout: String,
    author_id: Option<String>,
    actor_type: String,
    sort_order: i64,
    published_at: Option<String>,
    created_at: String,
    updated_at: String,
}

impl TryFrom<PageRow> for Page {
    type Error = Error;

    fn try_from(r: PageRow) -> Result<Self> {
        Ok(Page {
            status: PageStatus::parse(&r.status)
                .ok_or_else(|| unknown("pages.status", &r.status))?,
            actor_type: AuthorType::parse(&r.actor_type)
                .ok_or_else(|| unknown("pages.actor_type", &r.actor_type))?,
            id: r.id,
            slug: r.slug,
            title: r.title,
            body_md: r.body_md,
            layout: r.layout,
            author_id: r.author_id,
            sort_order: r.sort_order,
            published_at: r.published_at,
            created_at: r.created_at,
            updated_at: r.updated_at,
        })
    }
}

async fn page_by_slug<'e>(e: impl SqliteExecutor<'e>, slug: &str) -> Result<Option<Page>> {
    sqlx::query_as!(
        PageRow,
        "SELECT id, slug, title, body_md, status, layout, author_id, actor_type, sort_order,
                published_at, created_at, updated_at
         FROM pages WHERE slug = ?",
        slug
    )
    .fetch_optional(e)
    .await
    .map_err(db_error)?
    .map(Page::try_from)
    .transpose()
}

async fn page_by_id<'e>(e: impl SqliteExecutor<'e>, id: &str) -> Result<Option<Page>> {
    sqlx::query_as!(
        PageRow,
        "SELECT id, slug, title, body_md, status, layout, author_id, actor_type, sort_order,
                published_at, created_at, updated_at
         FROM pages WHERE id = ?",
        id
    )
    .fetch_optional(e)
    .await
    .map_err(db_error)?
    .map(Page::try_from)
    .transpose()
}

async fn menu_rows<'e>(e: impl SqliteExecutor<'e>, location: &str) -> Result<Vec<MenuRow>> {
    let rows = sqlx::query!(
        "SELECT id, parent_id, label, link, sort_order, created_at
         FROM menus WHERE location = ? ORDER BY sort_order",
        location
    )
    .fetch_all(e)
    .await
    .map_err(db_error)?;
    rows.into_iter()
        .map(|r| {
            Ok(MenuRow {
                link: serde_json::from_str(&r.link).map_err(Error::internal)?,
                id: r.id,
                parent_id: r.parent_id,
                label: r.label,
                sort_order: r.sort_order,
                created_at: r.created_at,
            })
        })
        .collect()
}

struct WidgetRow {
    id: String,
    kind: String,
    region: String,
    scope: String,
    title: String,
    config: String,
    sort_order: i64,
    enabled: i64,
    created_at: String,
    updated_at: String,
}

impl TryFrom<WidgetRow> for Widget {
    type Error = Error;
    fn try_from(r: WidgetRow) -> Result<Self> {
        Ok(Widget {
            config: serde_json::from_str(&r.config).map_err(Error::internal)?,
            id: r.id,
            kind: r.kind,
            region: r.region,
            scope: r.scope,
            title: r.title,
            sort_order: r.sort_order,
            enabled: r.enabled != 0,
            created_at: r.created_at,
            updated_at: r.updated_at,
        })
    }
}

async fn widget_by_id<'e>(e: impl SqliteExecutor<'e>, id: &str) -> Result<Option<Widget>> {
    sqlx::query_as!(
        WidgetRow,
        "SELECT id, kind, region, scope, title, config, sort_order, enabled, created_at, updated_at
         FROM widgets WHERE id = ?",
        id
    )
    .fetch_optional(e)
    .await
    .map_err(db_error)?
    .map(Widget::try_from)
    .transpose()
}

async fn settings<'e>(e: impl SqliteExecutor<'e>) -> Result<Vec<(String, Value)>> {
    let rows = sqlx::query!("SELECT key, value FROM site_settings ORDER BY key")
        .fetch_all(e)
        .await
        .map_err(db_error)?;
    rows.into_iter()
        .map(|r| {
            Ok((
                r.key,
                serde_json::from_str(&r.value).map_err(Error::internal)?,
            ))
        })
        .collect()
}

// ---- 읽기·쓰기 공용 쿼리 ----

async fn board_by_id<'e>(e: impl SqliteExecutor<'e>, id: &str) -> Result<Option<Board>> {
    sqlx::query_as!(
        BoardRow,
        "SELECT id, slug, name, kind, layout, list_order, policy, sort_order, field_schema, description, created_at, updated_at
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
        "SELECT id, slug, name, kind, layout, list_order, policy, sort_order, field_schema, description, created_at, updated_at
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
        r#"SELECT id, board_id, kind, status, title, slug, body_md, excerpt, author_id,
                actor_type, published_at, created_at, updated_at, scheduled_at, cover,
                category_id, is_notice, is_secret, view_count, guest_name, guest_pw_hash,
                    parent_id, thread_id, thread_path, thread_owner, meta,
                    last_activity_at, accepted_comment_id, is_locked,
                (SELECT json_group_array(t.name ORDER BY pt.position)
                   FROM post_tags pt JOIN tags t ON t.id = pt.tag_id
                  WHERE pt.post_id = posts.id) AS "tags!: String"
         FROM posts WHERE id = ?"#,
        id
    )
    .fetch_optional(e)
    .await
    .map_err(db_error)?
    .map(Post::try_from)
    .transpose()
}

/// 감사 기록 한 줄을 이 연결(트랜잭션)에 쓴다. 모듈이 자기 트랜잭션에 함께 남길 때도 쓴다.
pub async fn insert_audit(conn: &mut SqliteConnection, rec: &AuditRecord) -> Result<()> {
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
    .execute(&mut *conn)
    .await
    .map_err(db_error)?;
    Ok(())
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

    async fn module_rows(&self) -> Result<Vec<ModuleRow>> {
        sqlx::query!(
            r#"SELECT id, enabled AS "enabled: bool", version, settings, cursor, installed_at, updated_at
               FROM modules ORDER BY id"#
        )
        .fetch_all(&self.db.read)
        .await
        .map_err(db_error)?
        .into_iter()
        .map(|r| {
            Ok(ModuleRow {
                id: r.id,
                enabled: r.enabled,
                version: r.version,
                settings: serde_json::from_str(&r.settings).map_err(Error::internal)?,
                cursor: r.cursor,
                installed_at: r.installed_at,
                updated_at: r.updated_at,
            })
        })
        .collect()
    }

    async fn module_row(&self, id: &str) -> Result<Option<ModuleRow>> {
        let r = sqlx::query!(
            r#"SELECT id, enabled AS "enabled: bool", version, settings, cursor, installed_at, updated_at
               FROM modules WHERE id = ?"#,
            id
        )
        .fetch_optional(&self.db.read)
        .await
        .map_err(db_error)?;
        r.map(|r| {
            Ok(ModuleRow {
                id: r.id,
                enabled: r.enabled,
                version: r.version,
                settings: serde_json::from_str(&r.settings).map_err(Error::internal)?,
                cursor: r.cursor,
                installed_at: r.installed_at,
                updated_at: r.updated_at,
            })
        })
        .transpose()
    }

    async fn module_apply(&self, id: &str, migrations: &[ModuleMigration]) -> Result<Vec<i64>> {
        let mut applied = Vec::new();
        for m in migrations {
            let mut tx = self
                .db
                .write
                .begin_with("BEGIN IMMEDIATE")
                .await
                .map_err(db_error)?;
            let done = sqlx::query!(
                "SELECT version FROM module_migrations WHERE module = ? AND version = ?",
                id,
                m.version
            )
            .fetch_optional(&mut *tx)
            .await
            .map_err(db_error)?;
            if done.is_some() {
                continue;
            }
            sqlx::raw_sql(sqlx::AssertSqlSafe(m.up))
                .execute(&mut *tx)
                .await
                .map_err(db_error)?;
            let now = maru_core::clock::now();
            sqlx::query!(
                "INSERT INTO module_migrations (module, version, name, applied_at) VALUES (?, ?, ?, ?)",
                id,
                m.version,
                m.name,
                now
            )
            .execute(&mut *tx)
            .await
            .map_err(db_error)?;
            tx.commit().await.map_err(db_error)?;
            applied.push(m.version);
        }
        Ok(applied)
    }

    async fn module_revert(&self, id: &str, migrations: &[ModuleMigration]) -> Result<Vec<i64>> {
        let mut reverted = Vec::new();
        for m in migrations.iter().rev() {
            let mut tx = self
                .db
                .write
                .begin_with("BEGIN IMMEDIATE")
                .await
                .map_err(db_error)?;
            let done = sqlx::query!(
                "SELECT version FROM module_migrations WHERE module = ? AND version = ?",
                id,
                m.version
            )
            .fetch_optional(&mut *tx)
            .await
            .map_err(db_error)?;
            if done.is_none() {
                continue;
            }
            sqlx::raw_sql(sqlx::AssertSqlSafe(m.down))
                .execute(&mut *tx)
                .await
                .map_err(db_error)?;
            sqlx::query!(
                "DELETE FROM module_migrations WHERE module = ? AND version = ?",
                id,
                m.version
            )
            .execute(&mut *tx)
            .await
            .map_err(db_error)?;
            tx.commit().await.map_err(db_error)?;
            reverted.push(m.version);
        }
        Ok(reverted)
    }

    async fn module_cursor_set(&self, id: &str, seq: i64) -> Result<()> {
        sqlx::query!("UPDATE modules SET cursor = ? WHERE id = ?", seq, id)
            .execute(&self.db.write)
            .await
            .map_err(db_error)?;
        Ok(())
    }

    async fn audit_last_seq(&self) -> Result<i64> {
        let r = sqlx::query!(r#"SELECT COALESCE(MAX(rowid), 0) AS "seq!: i64" FROM audit_log"#)
            .fetch_one(&self.db.read)
            .await
            .map_err(db_error)?;
        Ok(r.seq)
    }

    async fn audit_after(
        &self,
        after: i64,
        upto: i64,
        actions: &[&str],
        limit: i64,
    ) -> Result<Vec<AuditEvent>> {
        let list = json(&actions)?;
        sqlx::query!(
            r#"SELECT rowid AS "seq!: i64", id AS "id!: String", actor_type AS "actor_type!: String",
                      actor_id AS "actor_id?: String", action AS "action!: String",
                      target_type AS "target_type!: String", target_id AS "target_id?: String",
                      diff AS "diff!: String", at AS "at!: String"
               FROM audit_log
               WHERE rowid > ? AND rowid <= ? AND action IN (SELECT value FROM json_each(?))
               ORDER BY rowid LIMIT ?"#,
            after,
            upto,
            list,
            limit
        )
        .fetch_all(&self.db.read)
        .await
        .map_err(db_error)?
        .into_iter()
        .map(|r| {
            Ok(AuditEvent {
                seq: r.seq,
                id: r.id,
                actor_type: r.actor_type,
                actor_id: r.actor_id,
                action: r.action,
                target_type: r.target_type,
                target_id: r.target_id,
                diff: serde_json::from_str(&r.diff).map_err(Error::internal)?,
                at: r.at,
            })
        })
        .collect()
    }

    async fn audit_event(&self, id: &str) -> Result<Option<AuditEvent>> {
        let r = sqlx::query!(
            r#"SELECT rowid AS "seq!: i64", id AS "id!: String", actor_type AS "actor_type!: String",
                      actor_id AS "actor_id?: String", action AS "action!: String",
                      target_type AS "target_type!: String", target_id AS "target_id?: String",
                      diff AS "diff!: String", at AS "at!: String"
               FROM audit_log WHERE id = ?"#,
            id
        )
        .fetch_optional(&self.db.read)
        .await
        .map_err(db_error)?;
        r.map(|r| {
            Ok(AuditEvent {
                seq: r.seq,
                id: r.id,
                actor_type: r.actor_type,
                actor_id: r.actor_id,
                action: r.action,
                target_type: r.target_type,
                target_id: r.target_id,
                diff: serde_json::from_str(&r.diff).map_err(Error::internal)?,
                at: r.at,
            })
        })
        .transpose()
    }

    async fn module_failure_put(&self, f: &ModuleFailure) -> Result<()> {
        sqlx::query!(
            "INSERT INTO module_event_failures
                (module, event_id, action, attempts, last_error, next_at, gave_up, first_at)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?)
             ON CONFLICT (module, event_id) DO UPDATE SET attempts = excluded.attempts,
                last_error = excluded.last_error, next_at = excluded.next_at, gave_up = excluded.gave_up",
            f.module,
            f.event_id,
            f.action,
            f.attempts,
            f.last_error,
            f.next_at,
            f.gave_up,
            f.first_at
        )
        .execute(&self.db.write)
        .await
        .map_err(db_error)?;
        Ok(())
    }

    async fn module_failure_delete(&self, module: &str, event_id: &str) -> Result<()> {
        sqlx::query!(
            "DELETE FROM module_event_failures WHERE module = ? AND event_id = ?",
            module,
            event_id
        )
        .execute(&self.db.write)
        .await
        .map_err(db_error)?;
        Ok(())
    }

    async fn module_failures(&self, module: Option<&str>) -> Result<Vec<ModuleFailure>> {
        let module = module.unwrap_or("");
        let rows = sqlx::query!(
            r#"SELECT module, event_id, action, attempts, last_error, next_at,
                      gave_up AS "gave_up: bool", first_at
               FROM module_event_failures WHERE (? = '' OR module = ?)
               ORDER BY first_at DESC LIMIT 100"#,
            module,
            module
        )
        .fetch_all(&self.db.read)
        .await
        .map_err(db_error)?;
        Ok(rows
            .into_iter()
            .map(|r| ModuleFailure {
                module: r.module,
                event_id: r.event_id,
                action: r.action,
                attempts: r.attempts,
                last_error: r.last_error,
                next_at: r.next_at,
                gave_up: r.gave_up,
                first_at: r.first_at,
            })
            .collect())
    }

    async fn module_failures_due(&self, module: &str, now: &str) -> Result<Vec<ModuleFailure>> {
        let rows = sqlx::query!(
            r#"SELECT module, event_id, action, attempts, last_error, next_at,
                      gave_up AS "gave_up: bool", first_at
               FROM module_event_failures
               WHERE module = ? AND gave_up = 0 AND next_at <= ?
               ORDER BY next_at LIMIT 50"#,
            module,
            now
        )
        .fetch_all(&self.db.read)
        .await
        .map_err(db_error)?;
        Ok(rows
            .into_iter()
            .map(|r| ModuleFailure {
                module: r.module,
                event_id: r.event_id,
                action: r.action,
                attempts: r.attempts,
                last_error: r.last_error,
                next_at: r.next_at,
                gave_up: r.gave_up,
                first_at: r.first_at,
            })
            .collect())
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

    async fn list_tokens(&self) -> Result<Vec<Token>> {
        let rows = sqlx::query_as!(
            TokenRow,
            "SELECT id, name, token_hash, scopes, board_ids, expires_at, last_used_at,
                    revoked_at, created_at, updated_at
             FROM tokens ORDER BY created_at, id"
        )
        .fetch_all(&self.db.read)
        .await
        .map_err(db_error)?;
        rows.into_iter().map(Token::try_from).collect()
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
            "SELECT id, slug, name, kind, layout, list_order, policy, sort_order, field_schema, description, created_at, updated_at
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
            r#"SELECT id, board_id, kind, status, title, slug, body_md, excerpt, author_id,
                    actor_type, published_at, created_at, updated_at, scheduled_at, cover,
                    category_id, is_notice, is_secret, view_count, guest_name, guest_pw_hash,
                    parent_id, thread_id, thread_path, thread_owner, meta,
                    last_activity_at, accepted_comment_id, is_locked,
                    (SELECT json_group_array(t.name ORDER BY pt.position)
                       FROM post_tags pt JOIN tags t ON t.id = pt.tag_id
                      WHERE pt.post_id = posts.id) AS "tags!: String"
             FROM posts WHERE board_id = ? AND slug = ?"#,
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
        let notice = q.notices.map(i64::from);
        let rows = sqlx::query_as!(
            PostRow,
            r#"SELECT p.id AS "id!", p.board_id AS "board_id!", p.kind AS "kind!",
                      p.status AS "status!", p.title AS "title!", p.slug AS "slug!",
                      p.body_md AS "body_md!", p.excerpt, p.author_id,
                      p.actor_type AS "actor_type!", p.published_at,
                      p.created_at AS "created_at!", p.updated_at AS "updated_at!",
                      p.scheduled_at, p.cover,
                      p.category_id, p.is_notice AS "is_notice!", p.is_secret AS "is_secret!",
                      p.view_count AS "view_count!", p.guest_name, p.guest_pw_hash,
                      p.parent_id, p.thread_id, p.thread_path AS "thread_path!", p.thread_owner,
                      p.meta AS "meta!", p.last_activity_at, p.accepted_comment_id,
                      p.is_locked AS "is_locked!",
                      (SELECT json_group_array(t.name ORDER BY pt.position)
                         FROM post_tags pt JOIN tags t ON t.id = pt.tag_id
                        WHERE pt.post_id = p.id) AS "tags!: String"
               FROM posts p
               JOIN boards b ON b.id = p.board_id
               JOIN roles r ON r.name = json_extract(b.policy, '$.read')
               WHERE p.status = 'published' AND p.kind = 'post'
                 AND r.rank <= ?1 AND (?2 IS NULL OR p.board_id = ?2)
                 AND (?5 IS NULL OR p.category_id = ?5)
                 AND (?6 IS NULL OR p.is_notice = ?6)
                 AND (?7 IS NULL OR EXISTS (SELECT 1 FROM post_tags x
                                             WHERE x.post_id = p.id AND x.tag_id = ?7))
                 AND (?8 = 0 OR p.is_secret = 0)
                 AND (?10 IS NULL OR NOT EXISTS (
                       SELECT 1 FROM json_each(?10) f
                       WHERE NOT CASE json_extract(f.value, '$.m')
                         WHEN 'eq' THEN coalesce(
                           json_extract(p.meta, json_extract(f.value, '$.p'))
                             = json_extract(f.value, '$.v'), 0)
                         WHEN 'has' THEN EXISTS (
                           SELECT 1 FROM json_each(p.meta, json_extract(f.value, '$.p')) x
                            WHERE x.value = json_extract(f.value, '$.v'))
                         WHEN 'on' THEN coalesce(
                           json_extract(p.meta, json_extract(f.value, '$.p')), 0) = 1
                         WHEN 'off' THEN coalesce(
                           json_extract(p.meta, json_extract(f.value, '$.p')), 0) != 1
                         ELSE 0 END))
               ORDER BY CASE ?11
                          WHEN 'views' THEN p.view_count
                          WHEN 'comments' THEN (SELECT count(*) FROM comments c
                                                 WHERE c.post_id = p.id AND c.status = 'published')
                        END DESC,
                        CASE WHEN ?9 = 1
                          THEN COALESCE((SELECT COALESCE(t.last_activity_at, t.published_at)
                                           FROM posts t WHERE t.id = p.thread_id),
                                        p.last_activity_at, p.published_at)
                          ELSE COALESCE((SELECT t.published_at FROM posts t WHERE t.id = p.thread_id),
                                        p.published_at)
                        END DESC,
                        COALESCE(p.thread_id, p.id) DESC, p.thread_path
               LIMIT ?3 OFFSET ?4"#,
            q.max_read_rank,
            q.board_id,
            q.limit,
            q.offset,
            q.category_id,
            notice,
            q.tag_id,
            q.exclude_secret,
            q.by_activity,
            q.meta_filters,
            q.sort
        )
        .fetch_all(&self.db.read)
        .await
        .map_err(db_error)?;
        convert(rows)
    }

    async fn count_published(&self, q: &PublishedQuery) -> Result<i64> {
        let notice = q.notices.map(i64::from);
        sqlx::query_scalar!(
            r#"SELECT count(*) AS "n!: i64"
               FROM posts p
               JOIN boards b ON b.id = p.board_id
               JOIN roles r ON r.name = json_extract(b.policy, '$.read')
               WHERE p.status = 'published' AND p.kind = 'post'
                 AND r.rank <= ?1 AND (?2 IS NULL OR p.board_id = ?2)
                 AND (?3 IS NULL OR p.category_id = ?3)
                 AND (?4 IS NULL OR p.is_notice = ?4)
                 AND (?5 IS NULL OR EXISTS (SELECT 1 FROM post_tags x
                                             WHERE x.post_id = p.id AND x.tag_id = ?5))
                 AND (?6 = 0 OR p.is_secret = 0)
                 AND (?7 IS NULL OR NOT EXISTS (
                       SELECT 1 FROM json_each(?7) f
                       WHERE NOT CASE json_extract(f.value, '$.m')
                         WHEN 'eq' THEN coalesce(
                           json_extract(p.meta, json_extract(f.value, '$.p'))
                             = json_extract(f.value, '$.v'), 0)
                         WHEN 'has' THEN EXISTS (
                           SELECT 1 FROM json_each(p.meta, json_extract(f.value, '$.p')) x
                            WHERE x.value = json_extract(f.value, '$.v'))
                         WHEN 'on' THEN coalesce(
                           json_extract(p.meta, json_extract(f.value, '$.p')), 0) = 1
                         WHEN 'off' THEN coalesce(
                           json_extract(p.meta, json_extract(f.value, '$.p')), 0) != 1
                         ELSE 0 END))"#,
            q.max_read_rank,
            q.board_id,
            q.category_id,
            notice,
            q.tag_id,
            q.exclude_secret,
            q.meta_filters
        )
        .fetch_one(&self.db.read)
        .await
        .map_err(db_error)
    }

    async fn list_posts(&self, q: &PostFilter) -> Result<Vec<Post>> {
        let status = q.status.map(PostStatus::as_str);
        // 검색어가 있으면 색인에 걸린 글만. `MATCH NULL` 을 피하려고 쿼리를 나눈다.
        if let Some(fts) = &q.search {
            let rows = sqlx::query_as!(
                PostRow,
                r#"SELECT id, board_id, kind, status, title, slug, body_md, excerpt, author_id,
                        actor_type, published_at, created_at, updated_at, scheduled_at, cover,
                        category_id, is_notice, is_secret, view_count, guest_name, guest_pw_hash,
                    parent_id, thread_id, thread_path, thread_owner, meta,
                    last_activity_at, accepted_comment_id, is_locked,
                        (SELECT json_group_array(t.name ORDER BY pt.position)
                           FROM post_tags pt JOIN tags t ON t.id = pt.tag_id
                          WHERE pt.post_id = posts.id) AS "tags!: String"
                 FROM posts
                 WHERE kind = 'post' AND (?1 IS NULL OR board_id = ?1) AND (?2 IS NULL OR status = ?2)
                   AND id IN (SELECT d.post_id FROM search_fts f JOIN search_docs d ON d.id = f.rowid
                               WHERE search_fts MATCH ?5)
                 ORDER BY updated_at DESC, id DESC
                 LIMIT ?3 OFFSET ?4"#,
                q.board_id,
                status,
                q.limit,
                q.offset,
                fts
            )
            .fetch_all(&self.db.read)
            .await
            .map_err(db_error)?;
            return convert(rows);
        }
        let rows = sqlx::query_as!(
            PostRow,
            r#"SELECT id, board_id, kind, status, title, slug, body_md, excerpt, author_id,
                    actor_type, published_at, created_at, updated_at, scheduled_at, cover,
                    category_id, is_notice, is_secret, view_count, guest_name, guest_pw_hash,
                    parent_id, thread_id, thread_path, thread_owner, meta,
                    last_activity_at, accepted_comment_id, is_locked,
                    (SELECT json_group_array(t.name ORDER BY pt.position)
                       FROM post_tags pt JOIN tags t ON t.id = pt.tag_id
                      WHERE pt.post_id = posts.id) AS "tags!: String"
             FROM posts
             WHERE kind = 'post' AND (?1 IS NULL OR board_id = ?1) AND (?2 IS NULL OR status = ?2)
             ORDER BY updated_at DESC, id DESC
             LIMIT ?3 OFFSET ?4"#,
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
        if let Some(fts) = &q.search {
            return sqlx::query_scalar!(
                r#"SELECT count(*) AS "n!: i64" FROM posts
                   WHERE kind = 'post' AND (?1 IS NULL OR board_id = ?1) AND (?2 IS NULL OR status = ?2)
                     AND id IN (SELECT d.post_id FROM search_fts f JOIN search_docs d ON d.id = f.rowid
                                 WHERE search_fts MATCH ?3)"#,
                q.board_id,
                status,
                fts
            )
            .fetch_one(&self.db.read)
            .await
            .map_err(db_error);
        }
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

    async fn page_by_slug(&self, slug: &str) -> Result<Option<Page>> {
        page_by_slug(&self.db.read, slug).await
    }

    async fn page_by_id(&self, id: &str) -> Result<Option<Page>> {
        page_by_id(&self.db.read, id).await
    }

    async fn list_pages(&self, status: Option<PageStatus>) -> Result<Vec<Page>> {
        let status = status.map(PageStatus::as_str);
        let rows = sqlx::query_as!(
            PageRow,
            "SELECT id, slug, title, body_md, status, layout, author_id, actor_type, sort_order,
                    published_at, created_at, updated_at
             FROM pages WHERE (?1 IS NULL OR status = ?1)
             ORDER BY sort_order, title",
            status
        )
        .fetch_all(&self.db.read)
        .await
        .map_err(db_error)?;
        convert(rows)
    }

    async fn menu_rows(&self, location: &str) -> Result<Vec<MenuRow>> {
        menu_rows(&self.db.read, location).await
    }

    async fn list_widgets(&self) -> Result<Vec<Widget>> {
        let rows = sqlx::query_as!(
            WidgetRow,
            "SELECT id, kind, region, scope, title, config, sort_order, enabled, created_at, updated_at
             FROM widgets ORDER BY region, sort_order, created_at, id"
        )
        .fetch_all(&self.db.read)
        .await
        .map_err(db_error)?;
        convert(rows)
    }

    async fn widget_by_id(&self, id: &str) -> Result<Option<Widget>> {
        widget_by_id(&self.db.read, id).await
    }

    async fn list_popular(&self, q: &PopularQuery) -> Result<Vec<Post>> {
        let rows = sqlx::query_as!(
            PostRow,
            r#"SELECT p.id AS "id!", p.board_id AS "board_id!", p.kind AS "kind!",
                      p.status AS "status!", p.title AS "title!", p.slug AS "slug!",
                      p.body_md AS "body_md!", p.excerpt, p.author_id,
                      p.actor_type AS "actor_type!", p.published_at,
                      p.created_at AS "created_at!", p.updated_at AS "updated_at!",
                      p.scheduled_at, p.cover,
                      p.category_id, p.is_notice AS "is_notice!", p.is_secret AS "is_secret!",
                      p.view_count AS "view_count!", p.guest_name, p.guest_pw_hash,
                      p.parent_id, p.thread_id, p.thread_path AS "thread_path!", p.thread_owner,
                      p.meta AS "meta!", p.last_activity_at, p.accepted_comment_id,
                      p.is_locked AS "is_locked!",
                      (SELECT json_group_array(t.name ORDER BY pt.position)
                         FROM post_tags pt JOIN tags t ON t.id = pt.tag_id
                        WHERE pt.post_id = p.id) AS "tags!: String"
               FROM posts p
               JOIN boards b ON b.id = p.board_id
               JOIN roles r ON r.name = json_extract(b.policy, '$.read')
               WHERE p.status = 'published' AND p.kind = 'post' AND p.is_secret = 0
                 AND r.rank <= ?1 AND (?2 IS NULL OR p.board_id = ?2)
                 AND (?3 IS NULL OR p.published_at >= ?3)
               ORDER BY p.view_count DESC,
                        (SELECT count(*) FROM comments c
                          WHERE c.post_id = p.id AND c.status = 'published') DESC,
                        p.published_at DESC, p.id DESC
               LIMIT ?4"#,
            q.max_read_rank,
            q.board_id,
            q.since,
            q.limit
        )
        .fetch_all(&self.db.read)
        .await
        .map_err(db_error)?;
        convert(rows)
    }

    async fn settings(&self) -> Result<Vec<(String, Value)>> {
        settings(&self.db.read).await
    }

    async fn member_by_id(&self, id: &str) -> Result<Option<Member>> {
        member_by_id(&self.db.read, id).await
    }

    async fn member_by_email(&self, email: &str) -> Result<Option<Member>> {
        member_by_email(&self.db.read, email).await
    }

    async fn member_by_name(&self, name: &str) -> Result<Option<Member>> {
        member_by_name(&self.db.read, name).await
    }

    async fn list_members(&self, limit: i64, offset: i64) -> Result<Vec<Member>> {
        sqlx::query_as!(
            MemberRow,
            "SELECT id, email, display_name, password_hash, status, email_verified_at, withdrawn_at,
                    created_at, updated_at
             FROM members ORDER BY created_at, id LIMIT ? OFFSET ?",
            limit,
            offset
        )
        .fetch_all(&self.db.read)
        .await
        .map_err(db_error)?
        .into_iter()
        .map(Member::try_from)
        .collect()
    }

    async fn count_role_members(&self, role: &str) -> Result<i64> {
        sqlx::query_scalar!(
            "SELECT count(*) FROM member_roles mr JOIN roles r ON r.id = mr.role_id
             JOIN members m ON m.id = mr.member_id
             WHERE r.name = ? AND m.status = 'active'",
            role
        )
        .fetch_one(&self.db.read)
        .await
        .map_err(db_error)
    }

    async fn member_activity(&self, member_id: &str) -> Result<MemberActivity> {
        let row = sqlx::query!(
            r#"SELECT m.last_login_at AS "last_login_at?: String", d.dormant_at AS "dormant_at?: String"
               FROM members m LEFT JOIN member_dormant d ON d.member_id = m.id
               WHERE m.id = ?"#,
            member_id
        )
        .fetch_optional(&self.db.read)
        .await
        .map_err(db_error)?;
        Ok(row
            .map(|r| MemberActivity {
                last_login_at: r.last_login_at,
                dormant_at: r.dormant_at,
            })
            .unwrap_or_default())
    }

    async fn members_to_notify(&self, seen_before: &str, limit: i64) -> Result<Vec<Member>> {
        sqlx::query_as!(
            MemberRow,
            "SELECT m.id, m.email, m.display_name, m.password_hash, m.status, m.email_verified_at,
                    m.withdrawn_at, m.created_at, m.updated_at
             FROM members m
             WHERE m.status = 'active' AND m.withdrawn_at IS NULL
               AND COALESCE(m.last_login_at, m.created_at) < ?
               AND NOT EXISTS (SELECT 1 FROM member_dormant d WHERE d.member_id = m.id)
               AND NOT EXISTS (SELECT 1 FROM member_roles mr JOIN roles r ON r.id = mr.role_id
                               WHERE mr.member_id = m.id AND r.rank > 10)
               AND m.dormant_notice_at IS NULL
             ORDER BY m.id LIMIT ?",
            seen_before,
            limit
        )
        .fetch_all(&self.db.read)
        .await
        .map_err(db_error)?
        .into_iter()
        .map(Member::try_from)
        .collect()
    }

    async fn members_to_sleep(
        &self,
        seen_before: &str,
        noticed_before: &str,
        limit: i64,
    ) -> Result<Vec<Member>> {
        sqlx::query_as!(
            MemberRow,
            "SELECT m.id, m.email, m.display_name, m.password_hash, m.status, m.email_verified_at,
                    m.withdrawn_at, m.created_at, m.updated_at
             FROM members m
             WHERE m.status = 'active' AND m.withdrawn_at IS NULL
               AND COALESCE(m.last_login_at, m.created_at) < ?
               AND NOT EXISTS (SELECT 1 FROM member_dormant d WHERE d.member_id = m.id)
               AND NOT EXISTS (SELECT 1 FROM member_roles mr JOIN roles r ON r.id = mr.role_id
                               WHERE mr.member_id = m.id AND r.rank > 10)
               AND m.dormant_notice_at IS NOT NULL AND m.dormant_notice_at < ?
             ORDER BY m.id LIMIT ?",
            seen_before,
            noticed_before,
            limit
        )
        .fetch_all(&self.db.read)
        .await
        .map_err(db_error)?
        .into_iter()
        .map(Member::try_from)
        .collect()
    }

    async fn comment_bursts(
        &self,
        since: &str,
        min: i64,
        limit: i64,
    ) -> Result<Vec<(String, i64)>> {
        Ok(sqlx::query!(
            r#"SELECT post_id AS "post_id!: String", COUNT(*) AS "n!: i64" FROM comments
               WHERE created_at >= ? AND status IN ('published', 'pending')
               GROUP BY post_id HAVING COUNT(*) >= ?
               ORDER BY COUNT(*) DESC, post_id LIMIT ?"#,
            since,
            min,
            limit
        )
        .fetch_all(&self.db.read)
        .await
        .map_err(db_error)?
        .into_iter()
        .map(|r| (r.post_id, r.n))
        .collect())
    }

    async fn staff_members(&self, min_rank: i64) -> Result<Vec<Member>> {
        sqlx::query_as!(
            MemberRow,
            "SELECT m.id, m.email, m.display_name, m.password_hash, m.status, m.email_verified_at,
                    m.withdrawn_at, m.created_at, m.updated_at
             FROM members m
             WHERE m.status = 'active' AND m.withdrawn_at IS NULL
               AND EXISTS (SELECT 1 FROM member_roles mr JOIN roles r ON r.id = mr.role_id
                           WHERE mr.member_id = m.id AND r.rank >= ?)
             ORDER BY m.created_at, m.id",
            min_rank
        )
        .fetch_all(&self.db.read)
        .await
        .map_err(db_error)?
        .into_iter()
        .map(Member::try_from)
        .collect()
    }

    async fn purge_signal_sent(&self, before: &str) -> Result<u64> {
        Ok(sqlx::query!("DELETE FROM signal_sent WHERE at < ?", before)
            .execute(&self.db.write)
            .await
            .map_err(db_error)?
            .rows_affected())
    }

    async fn purge_mails(&self, before: &str) -> Result<u64> {
        Ok(sqlx::query!(
            "DELETE FROM mail_outbox WHERE status != 'queued' AND created_at < ?",
            before
        )
        .execute(&self.db.write)
        .await
        .map_err(db_error)?
        .rows_affected())
    }

    async fn mails(&self, status: Option<MailStatus>, limit: i64) -> Result<Vec<MailOut>> {
        crate::mail::list(&self.db.read, status, limit).await
    }

    async fn mail_by_id(&self, id: &str) -> Result<Option<MailOut>> {
        crate::mail::by_id(&self.db.read, id).await
    }

    async fn update_mail(&self, m: &MailOut) -> Result<()> {
        crate::mail::update(&self.db.write, m).await
    }

    async fn note_links(&self, urls: &[String], now: &str) -> Result<()> {
        crate::links::note(&self.db.write, urls, now).await
    }

    async fn link_previews(&self, urls: &[String]) -> Result<Vec<LinkPreview>> {
        crate::links::by_urls(&self.db.read, urls).await
    }

    async fn pending_links(&self, limit: i64) -> Result<Vec<LinkPreview>> {
        crate::links::pending(&self.db.read, limit).await
    }

    async fn update_link(&self, p: &LinkPreview) -> Result<()> {
        crate::links::update(&self.db.write, p).await
    }

    async fn links(&self, status: Option<LinkStatus>, limit: i64) -> Result<Vec<LinkPreview>> {
        crate::links::list(&self.db.read, status, limit).await
    }

    async fn comment_by_id(&self, id: &str) -> Result<Option<Comment>> {
        crate::comments::by_id(&self.db.read, id).await
    }

    async fn last_member_write(&self, member_id: &str) -> Result<Option<String>> {
        crate::comments::last_member_write(&self.db.read, member_id).await
    }

    async fn last_member_comment(&self, member_id: &str) -> Result<Option<(String, String)>> {
        crate::comments::last_member_comment(&self.db.read, member_id).await
    }

    async fn comments_of_post(
        &self,
        post_id: &str,
        statuses: &[CommentStatus],
    ) -> Result<Vec<Comment>> {
        crate::comments::of_post(&self.db.read, post_id, statuses).await
    }

    async fn list_comments(
        &self,
        q: &CommentQuery<'_>,
        limit: i64,
        offset: i64,
    ) -> Result<Vec<Comment>> {
        crate::comments::list(&self.db.read, q, limit, offset).await
    }

    async fn count_comments(&self, q: &CommentQuery<'_>) -> Result<i64> {
        crate::comments::count(&self.db.read, q).await
    }

    async fn wiki_pages(&self, board_id: &str, limit: i64) -> Result<Vec<WikiPage>> {
        let rows = sqlx::query!(
            r#"SELECT id AS "id!", title AS "title!", slug AS "slug!", body_md AS "body_md!",
                      updated_at AS "updated_at!"
               FROM posts
               WHERE board_id = ? AND status = 'published' AND parent_id IS NULL AND is_secret = 0
               ORDER BY published_at, id LIMIT ?"#,
            board_id,
            limit
        )
        .fetch_all(&self.db.read)
        .await
        .map_err(db_error)?;
        Ok(rows
            .into_iter()
            .map(|r| WikiPage {
                id: r.id,
                title: r.title,
                slug: r.slug,
                body_md: r.body_md,
                updated_at: r.updated_at,
            })
            .collect())
    }

    async fn member_comments(
        &self,
        member_id: &str,
        limit: i64,
        offset: i64,
    ) -> Result<Vec<Comment>> {
        crate::comments::of_member(&self.db.read, member_id, limit, offset).await
    }

    async fn count_member_comments(&self, member_id: &str) -> Result<i64> {
        crate::comments::count_of_member(&self.db.read, member_id).await
    }

    async fn member_bio(&self, member_id: &str) -> Result<Option<String>> {
        crate::profiles::bio(&self.db.read, member_id).await
    }

    async fn public_posts_of(
        &self,
        member_id: &str,
        max_read_rank: i64,
        limit: i64,
    ) -> Result<(Vec<String>, i64)> {
        let ids = crate::profiles::public_post_ids(&self.db.read, member_id, max_read_rank, limit)
            .await?;
        let total =
            crate::profiles::count_public_posts(&self.db.read, member_id, max_read_rank).await?;
        Ok((ids, total))
    }

    async fn public_comments_of(
        &self,
        member_id: &str,
        max_read_rank: i64,
        limit: i64,
    ) -> Result<(Vec<Comment>, i64)> {
        let items =
            crate::comments::public_of_member(&self.db.read, member_id, max_read_rank, limit)
                .await?;
        let total =
            crate::comments::count_public_of_member(&self.db.read, member_id, max_read_rank)
                .await?;
        Ok((items, total))
    }

    async fn comment_counts(&self, post_ids: &[String]) -> Result<Vec<(String, i64)>> {
        crate::comments::counts(&self.db.read, post_ids).await
    }

    async fn insert_challenge(
        &self,
        id_hash: &str,
        answer: &str,
        now: &str,
        expires_at: &str,
    ) -> Result<()> {
        crate::comments::insert_challenge(&self.db.write, id_hash, answer, now, expires_at).await
    }

    async fn take_challenge(&self, id_hash: &str, now: &str) -> Result<Option<(String, String)>> {
        crate::comments::take_challenge(&self.db.write, id_hash, now).await
    }

    async fn count_members(&self) -> Result<i64> {
        sqlx::query_scalar!("SELECT count(*) FROM members")
            .fetch_one(&self.db.read)
            .await
            .map_err(db_error)
    }

    async fn first_member(&self) -> Result<Option<Member>> {
        sqlx::query_as!(
            MemberRow,
            "SELECT id, email, display_name, password_hash, status, email_verified_at, withdrawn_at,
                    created_at, updated_at
             FROM members ORDER BY created_at LIMIT 1"
        )
        .fetch_optional(&self.db.read)
        .await
        .map_err(db_error)?
        .map(Member::try_from)
        .transpose()
    }

    async fn member_roles(&self, member_id: &str) -> Result<Vec<Role>> {
        let rows = sqlx::query!(
            "SELECT r.id, r.name, r.rank, r.permissions
             FROM roles r JOIN member_roles mr ON mr.role_id = r.id
             WHERE mr.member_id = ?",
            member_id
        )
        .fetch_all(&self.db.read)
        .await
        .map_err(db_error)?;
        rows.into_iter()
            .map(|r| {
                let names: Vec<String> =
                    serde_json::from_str(&r.permissions).map_err(Error::internal)?;
                Ok(Role {
                    id: r.id,
                    name: r.name,
                    rank: r.rank,
                    permissions: names.iter().filter_map(|n| Permission::parse(n)).collect(),
                })
            })
            .collect()
    }

    async fn session_by_hash(&self, hash: &str) -> Result<Option<Session>> {
        let row = sqlx::query!(
            "SELECT id, token_hash, member_id, csrf_token, created_at, expires_at
             FROM sessions WHERE token_hash = ?",
            hash
        )
        .fetch_optional(&self.db.read)
        .await
        .map_err(db_error)?;
        Ok(row.map(|r| Session {
            id: r.id,
            token_hash: r.token_hash,
            member_id: r.member_id,
            csrf_token: r.csrf_token,
            created_at: r.created_at,
            expires_at: r.expires_at,
        }))
    }

    async fn member_totp(&self, member_id: &str) -> Result<Option<MemberTotp>> {
        member_totp(&self.db.read, member_id).await
    }

    async fn recovery_codes_left(&self, member_id: &str) -> Result<i64> {
        sqlx::query_scalar!(
            "SELECT count(*) FROM member_recovery_codes WHERE member_id = ? AND used_at IS NULL",
            member_id
        )
        .fetch_one(&self.db.read)
        .await
        .map_err(db_error)
    }

    async fn totp_challenge_by_hash(&self, hash: &str) -> Result<Option<TotpChallenge>> {
        let row = sqlx::query!(
            "SELECT id, token_hash, member_id, method, attempts, created_at, expires_at
             FROM totp_challenges WHERE token_hash = ?",
            hash
        )
        .fetch_optional(&self.db.read)
        .await
        .map_err(db_error)?;
        Ok(row.map(|r| TotpChallenge {
            id: r.id,
            token_hash: r.token_hash,
            member_id: r.member_id,
            method: r.method,
            attempts: r.attempts,
            created_at: r.created_at,
            expires_at: r.expires_at,
        }))
    }

    async fn bump_totp_challenge(&self, id: &str) -> Result<i64> {
        let n = sqlx::query_scalar!(
            "UPDATE totp_challenges SET attempts = attempts + 1 WHERE id = ? RETURNING attempts",
            id
        )
        .fetch_optional(&self.db.write)
        .await
        .map_err(db_error)?;
        Ok(n.unwrap_or(i64::MAX))
    }

    async fn delete_totp_challenge(&self, id: &str) -> Result<()> {
        sqlx::query!("DELETE FROM totp_challenges WHERE id = ?", id)
            .execute(&self.db.write)
            .await
            .map_err(db_error)?;
        Ok(())
    }

    async fn ticket_by_hash(&self, hash: &str) -> Result<Option<LoginTicket>> {
        let row = sqlx::query!(
            "SELECT id, token_hash, member_id, purpose, created_at, expires_at, used_at
             FROM login_tickets WHERE token_hash = ?",
            hash
        )
        .fetch_optional(&self.db.read)
        .await
        .map_err(db_error)?;
        row.map(|r| {
            Ok(LoginTicket {
                purpose: purpose(&r.purpose)?,
                id: r.id,
                token_hash: r.token_hash,
                member_id: r.member_id,
                created_at: r.created_at,
                expires_at: r.expires_at,
                used_at: r.used_at,
            })
        })
        .transpose()
    }

    async fn purge_expired(&self, now: &str) -> Result<u64> {
        let a = sqlx::query!("DELETE FROM sessions WHERE expires_at <= ?", now)
            .execute(&self.db.write)
            .await
            .map_err(db_error)?
            .rows_affected();
        let b = sqlx::query!(
            "DELETE FROM login_tickets WHERE expires_at <= ? OR used_at IS NOT NULL",
            now
        )
        .execute(&self.db.write)
        .await
        .map_err(db_error)?
        .rows_affected();
        let c = sqlx::query!("DELETE FROM totp_challenges WHERE expires_at <= ?", now)
            .execute(&self.db.write)
            .await
            .map_err(db_error)?
            .rows_affected();
        Ok(a + b + c)
    }

    async fn media_by_id(&self, id: &str) -> Result<Option<Media>> {
        media_by_id(&self.db.read, id).await
    }

    async fn list_media(&self, limit: i64, offset: i64) -> Result<Vec<Media>> {
        let rows = sqlx::query_as!(
            MediaRow,
            "SELECT id, path, mime, size, width, height, alt_text, variants, created_at, updated_at
             FROM media ORDER BY created_at DESC, id DESC LIMIT ? OFFSET ?",
            limit,
            offset
        )
        .fetch_all(&self.db.read)
        .await
        .map_err(db_error)?;
        convert(rows)
    }

    async fn due_posts(&self, now: &str, limit: i64) -> Result<Vec<String>> {
        sqlx::query_scalar!(
            r#"SELECT id AS "id!" FROM posts
               WHERE scheduled_at IS NOT NULL AND scheduled_at <= ?1
                 AND kind = 'post' AND status IN ('draft', 'review', 'hidden')
               ORDER BY scheduled_at, id
               LIMIT ?2"#,
            now,
            limit
        )
        .fetch_all(&self.db.read)
        .await
        .map_err(db_error)
    }

    async fn attachment_by_id(&self, id: &str) -> Result<Option<Attachment>> {
        attachment_by_id(&self.db.read, id).await
    }

    async fn list_attachments(&self, limit: i64, offset: i64) -> Result<Vec<Attachment>> {
        let rows = sqlx::query_as!(
            AttachmentRow,
            "SELECT id, path, filename, mime, size, created_at
             FROM files ORDER BY created_at DESC, id DESC LIMIT ? OFFSET ?",
            limit,
            offset
        )
        .fetch_all(&self.db.read)
        .await
        .map_err(db_error)?;
        Ok(rows.into_iter().map(Attachment::from).collect())
    }

    async fn count_attachments(&self) -> Result<i64> {
        sqlx::query_scalar!(r#"SELECT count(*) AS "n!: i64" FROM files"#)
            .fetch_one(&self.db.read)
            .await
            .map_err(db_error)
    }

    async fn list_templates(&self) -> Result<Vec<PostTemplate>> {
        let rows = sqlx::query_as!(
            TemplateRow,
            "SELECT id, name, body_md, created_at, updated_at FROM post_templates ORDER BY name"
        )
        .fetch_all(&self.db.read)
        .await
        .map_err(db_error)?;
        Ok(rows.into_iter().map(PostTemplate::from).collect())
    }

    async fn count_media(&self) -> Result<i64> {
        sqlx::query_scalar!("SELECT COUNT(*) FROM media")
            .fetch_one(&self.db.read)
            .await
            .map_err(db_error)
    }

    async fn categories_of_board(&self, board_id: &str) -> Result<Vec<Category>> {
        categories_of_board(&self.db.read, board_id).await
    }

    async fn category_by_id(&self, id: &str) -> Result<Option<Category>> {
        category_by_id(&self.db.read, id).await
    }

    async fn tag_by_slug(&self, slug: &str) -> Result<Option<Tag>> {
        tag_by_slug(&self.db.read, slug).await
    }

    // 공개 범위 조건은 list_published·count_published 와 같다.
    async fn list_tags(&self, max_read_rank: i64) -> Result<Vec<Tag>> {
        let rows = sqlx::query_as!(
            TagRow,
            r#"SELECT t.id AS "id!", t.name AS "name!", t.slug AS "slug!",
                      t.created_at AS "created_at!", count(*) AS "posts!: i64"
               FROM tags t
               JOIN post_tags pt ON pt.tag_id = t.id
               JOIN posts p ON p.id = pt.post_id
               JOIN boards b ON b.id = p.board_id
               JOIN roles r ON r.name = json_extract(b.policy, '$.read')
               WHERE p.status = 'published' AND p.kind = 'post' AND p.is_secret = 0
                 AND r.rank <= ?
               GROUP BY t.id
               ORDER BY t.name"#,
            max_read_rank
        )
        .fetch_all(&self.db.read)
        .await
        .map_err(db_error)?;
        Ok(rows.into_iter().map(Tag::from).collect())
    }

    async fn list_fonts(&self) -> Result<Vec<Font>> {
        let fonts = font_rows(&self.db.read, None).await?;
        let files = font_file_rows(&self.db.read, None).await?;
        Ok(assemble_fonts(fonts, files))
    }

    async fn font_by_slug(&self, slug: &str) -> Result<Option<Font>> {
        let fonts = font_rows(&self.db.read, Some(slug)).await?;
        let files = font_file_rows(&self.db.read, Some(slug)).await?;
        Ok(assemble_fonts(fonts, files).pop())
    }

    async fn count_font_uses(&self, slug: &str) -> Result<i64> {
        // slug 는 영소문자·숫자·`-` 뿐이라 LIKE 의 `%`·`_` 가 섞이지 않는다.
        let pattern = format!("%]{{{slug}}}%");
        sqlx::query_scalar!(
            r#"SELECT (SELECT count(*) FROM posts WHERE status != 'deleted' AND body_md LIKE ?1)
                    + (SELECT count(*) FROM pages WHERE body_md LIKE ?1) AS "n!: i64""#,
            pattern
        )
        .fetch_one(&self.db.read)
        .await
        .map_err(db_error)
    }

    async fn add_views(&self, counts: &[(String, i64)]) -> Result<()> {
        if counts.is_empty() {
            return Ok(());
        }
        let mut tx = self
            .db
            .write
            .begin_with("BEGIN IMMEDIATE")
            .await
            .map_err(db_error)?;
        for (id, n) in counts {
            sqlx::query!(
                "UPDATE posts SET view_count = view_count + ? WHERE id = ?",
                n,
                id
            )
            .execute(&mut *tx)
            .await
            .map_err(db_error)?;
        }
        tx.commit().await.map_err(db_error)
    }

    async fn list_audit(&self, f: &AuditFilter, page: PageReq) -> Result<Listing<AuditEntry>> {
        // 동작 앞부분: `post` 는 `post` 자신과 `post.…`. LIKE 의 `_`·`%` 는 글자 그대로.
        let like = format!(
            "{}.%",
            f.action
                .replace('\\', "\\\\")
                .replace('%', "\\%")
                .replace('_', "\\_")
        );
        let (limit, offset) = (page.limit(), page.offset());
        let rows = sqlx::query!(
            r#"SELECT a.id AS "id!", a.actor_type AS "actor_type!", a.actor_id,
                      COALESCE(m.display_name, t.name, '') AS "actor_name!: String",
                      a.action AS "action!", a.target_type AS "target_type!", a.target_id,
                      a.diff AS "diff!", a.at AS "at!"
               FROM audit_log a
               LEFT JOIN members m ON a.actor_type = 'human' AND m.id = a.actor_id
               LEFT JOIN tokens t ON a.actor_type = 'agent' AND t.id = a.actor_id
               WHERE (? = '' OR a.actor_type = ?)
                 AND (? = '' OR a.actor_id = ?)
                 AND (? = '' OR a.action = ? OR a.action LIKE ? ESCAPE '\')
                 AND (? = '' OR a.target_type = ?)
                 AND (? = '' OR a.target_id = ?)
                 AND (? = '' OR a.at >= ?)
                 AND (? = '' OR a.at < ?)
               ORDER BY a.at DESC, a.id DESC LIMIT ? OFFSET ?"#,
            f.actor_type,
            f.actor_type,
            f.actor_id,
            f.actor_id,
            f.action,
            f.action,
            like,
            f.target_type,
            f.target_type,
            f.target_id,
            f.target_id,
            f.since,
            f.since,
            f.until,
            f.until,
            limit,
            offset
        )
        .fetch_all(&self.db.read)
        .await
        .map_err(db_error)?;
        let total = sqlx::query_scalar!(
            r#"SELECT count(*) AS "n!: i64" FROM audit_log a
               WHERE (? = '' OR a.actor_type = ?)
                 AND (? = '' OR a.actor_id = ?)
                 AND (? = '' OR a.action = ? OR a.action LIKE ? ESCAPE '\')
                 AND (? = '' OR a.target_type = ?)
                 AND (? = '' OR a.target_id = ?)
                 AND (? = '' OR a.at >= ?)
                 AND (? = '' OR a.at < ?)"#,
            f.actor_type,
            f.actor_type,
            f.actor_id,
            f.actor_id,
            f.action,
            f.action,
            like,
            f.target_type,
            f.target_type,
            f.target_id,
            f.target_id,
            f.since,
            f.since,
            f.until,
            f.until
        )
        .fetch_one(&self.db.read)
        .await
        .map_err(db_error)?;
        let items = rows
            .into_iter()
            .map(|r| {
                Ok(AuditEntry {
                    id: r.id,
                    actor_type: r.actor_type,
                    actor_id: r.actor_id,
                    actor_name: r.actor_name,
                    action: r.action,
                    target_type: r.target_type,
                    target_id: r.target_id,
                    diff: serde_json::from_str(&r.diff).map_err(Error::internal)?,
                    at: r.at,
                })
            })
            .collect::<Result<Vec<_>>>()?;
        Ok(Listing {
            items,
            total,
            page: page.page,
            per_page: page.per_page,
        })
    }

    async fn add_stats(&self, batch: &StatBatch) -> Result<()> {
        let mut tx = self
            .db
            .write
            .begin_with("BEGIN IMMEDIATE")
            .await
            .map_err(db_error)?;
        for d in &batch.days {
            sqlx::query!(
                "INSERT INTO stat_days (day, views, visitors) VALUES (?, ?, ?)
                 ON CONFLICT (day) DO UPDATE SET views = views + excluded.views,
                     visitors = visitors + excluded.visitors",
                d.day,
                d.views,
                d.visitors
            )
            .execute(&mut *tx)
            .await
            .map_err(db_error)?;
        }
        for (day, p) in &batch.pages {
            sqlx::query!(
                "INSERT INTO stat_pages (day, path, views, visitors) VALUES (?, ?, ?, ?)
                 ON CONFLICT (day, path) DO UPDATE SET views = views + excluded.views,
                     visitors = visitors + excluded.visitors",
                day,
                p.path,
                p.views,
                p.visitors
            )
            .execute(&mut *tx)
            .await
            .map_err(db_error)?;
        }
        for (day, r) in &batch.referrers {
            sqlx::query!(
                "INSERT INTO stat_referrers (day, host, visits) VALUES (?, ?, ?)
                 ON CONFLICT (day, host) DO UPDATE SET visits = visits + excluded.visits",
                day,
                r.host,
                r.visits
            )
            .execute(&mut *tx)
            .await
            .map_err(db_error)?;
        }
        tx.commit().await.map_err(db_error)
    }

    async fn purge_stats(&self, before_day: &str) -> Result<()> {
        let mut tx = self
            .db
            .write
            .begin_with("BEGIN IMMEDIATE")
            .await
            .map_err(db_error)?;
        sqlx::query!("DELETE FROM stat_days WHERE day < ?", before_day)
            .execute(&mut *tx)
            .await
            .map_err(db_error)?;
        sqlx::query!("DELETE FROM stat_pages WHERE day < ?", before_day)
            .execute(&mut *tx)
            .await
            .map_err(db_error)?;
        sqlx::query!("DELETE FROM stat_referrers WHERE day < ?", before_day)
            .execute(&mut *tx)
            .await
            .map_err(db_error)?;
        tx.commit().await.map_err(db_error)
    }

    async fn stat_days(&self, from: &str, to: &str) -> Result<Vec<StatDay>> {
        let rows = sqlx::query!(
            r#"SELECT day AS "day!", views AS "views!", visitors AS "visitors!"
               FROM stat_days WHERE day BETWEEN ? AND ? ORDER BY day"#,
            from,
            to
        )
        .fetch_all(self.db.read_pool())
        .await
        .map_err(db_error)?;
        Ok(rows
            .into_iter()
            .map(|r| StatDay {
                day: r.day,
                views: r.views,
                visitors: r.visitors,
            })
            .collect())
    }

    async fn stat_pages(&self, from: &str, to: &str, limit: u32) -> Result<Vec<StatPage>> {
        let rows = sqlx::query!(
            r#"SELECT path AS "path!", SUM(views) AS "views!: i64", SUM(visitors) AS "visitors!: i64"
               FROM stat_pages WHERE day BETWEEN ? AND ?
               GROUP BY path ORDER BY 2 DESC, 3 DESC, path LIMIT ?"#,
            from,
            to,
            limit
        )
        .fetch_all(self.db.read_pool())
        .await
        .map_err(db_error)?;
        Ok(rows
            .into_iter()
            .map(|r| StatPage {
                path: r.path,
                title: String::new(),
                views: r.views,
                visitors: r.visitors,
            })
            .collect())
    }

    async fn stat_referrers(&self, from: &str, to: &str, limit: u32) -> Result<Vec<StatReferrer>> {
        let rows = sqlx::query!(
            r#"SELECT host AS "host!", SUM(visits) AS "visits!: i64"
               FROM stat_referrers WHERE day BETWEEN ? AND ?
               GROUP BY host ORDER BY 2 DESC, host LIMIT ?"#,
            from,
            to,
            limit
        )
        .fetch_all(self.db.read_pool())
        .await
        .map_err(db_error)?;
        Ok(rows
            .into_iter()
            .map(|r| StatReferrer {
                host: r.host,
                visits: r.visits,
            })
            .collect())
    }

    async fn neighbor_posts(&self, post: &Post) -> Result<(Option<Post>, Option<Post>)> {
        let Some(at) = post.published_at.as_deref() else {
            return Ok((None, None));
        };
        let prev = sqlx::query_as!(
            PostRow,
            r#"SELECT id, board_id, kind, status, title, slug, body_md, excerpt, author_id,
                    actor_type, published_at, created_at, updated_at, scheduled_at, cover,
                    category_id, is_notice, is_secret, view_count, guest_name, guest_pw_hash,
                    parent_id, thread_id, thread_path, thread_owner, meta,
                    last_activity_at, accepted_comment_id, is_locked,
                    (SELECT json_group_array(t.name ORDER BY pt.position)
                       FROM post_tags pt JOIN tags t ON t.id = pt.tag_id
                      WHERE pt.post_id = posts.id) AS "tags!: String"
             FROM posts
             WHERE board_id = ?1 AND status = 'published' AND kind = 'post'
               AND (published_at < ?2 OR (published_at = ?2 AND id < ?3))
             ORDER BY published_at DESC, id DESC
             LIMIT 1"#,
            post.board_id,
            at,
            post.id
        )
        .fetch_optional(&self.db.read)
        .await
        .map_err(db_error)?
        .map(Post::try_from)
        .transpose()?;
        let next = sqlx::query_as!(
            PostRow,
            r#"SELECT id, board_id, kind, status, title, slug, body_md, excerpt, author_id,
                    actor_type, published_at, created_at, updated_at, scheduled_at, cover,
                    category_id, is_notice, is_secret, view_count, guest_name, guest_pw_hash,
                    parent_id, thread_id, thread_path, thread_owner, meta,
                    last_activity_at, accepted_comment_id, is_locked,
                    (SELECT json_group_array(t.name ORDER BY pt.position)
                       FROM post_tags pt JOIN tags t ON t.id = pt.tag_id
                      WHERE pt.post_id = posts.id) AS "tags!: String"
             FROM posts
             WHERE board_id = ?1 AND status = 'published' AND kind = 'post'
               AND (published_at > ?2 OR (published_at = ?2 AND id > ?3))
             ORDER BY published_at ASC, id ASC
             LIMIT 1"#,
            post.board_id,
            at,
            post.id
        )
        .fetch_optional(&self.db.read)
        .await
        .map_err(db_error)?
        .map(Post::try_from)
        .transpose()?;
        Ok((prev, next))
    }

    // 공개 기준(게시판 read 서열)은 list_published 와 같은 조인이다.
    async fn related_posts(
        &self,
        post: &Post,
        max_read_rank: i64,
        limit: i64,
    ) -> Result<Vec<Post>> {
        let rows = sqlx::query_as!(
            PostRow,
            r#"SELECT c.id AS "id!", c.board_id AS "board_id!", c.kind AS "kind!",
                      c.status AS "status!", c.title AS "title!", c.slug AS "slug!",
                      c.body_md AS "body_md!", c.excerpt, c.author_id,
                      c.actor_type AS "actor_type!", c.published_at,
                      c.created_at AS "created_at!", c.updated_at AS "updated_at!",
                      c.scheduled_at, c.cover,
                      c.category_id, c.is_notice AS "is_notice!", c.is_secret AS "is_secret!",
                      c.view_count AS "view_count!", c.guest_name, c.guest_pw_hash,
                      c.parent_id, c.thread_id, c.thread_path AS "thread_path!", c.thread_owner,
                      c.meta AS "meta!", c.last_activity_at, c.accepted_comment_id,
                      c.is_locked AS "is_locked!",
                      (SELECT json_group_array(t.name ORDER BY pt.position)
                         FROM post_tags pt JOIN tags t ON t.id = pt.tag_id
                        WHERE pt.post_id = c.id) AS "tags!: String"
               FROM (SELECT p.*,
                            (SELECT count(*) FROM post_tags a
                               JOIN post_tags o ON o.tag_id = a.tag_id AND o.post_id = ?1
                              WHERE a.post_id = p.id) AS shared
                       FROM posts p
                       JOIN boards b ON b.id = p.board_id
                       JOIN roles r ON r.name = json_extract(b.policy, '$.read')
                      WHERE p.status = 'published' AND p.kind = 'post' AND r.rank <= ?2
                        AND p.id != ?1 AND p.is_secret = 0 AND p.is_notice = 0) c
               WHERE c.shared > 0 OR c.board_id = ?3
               ORDER BY c.shared DESC,
                        (?4 IS NOT NULL AND c.category_id IS ?4) DESC,
                        (c.board_id = ?3) DESC,
                        c.published_at DESC, c.id DESC
               LIMIT ?5"#,
            post.id,
            max_read_rank,
            post.board_id,
            post.category_id,
            limit
        )
        .fetch_all(&self.db.read)
        .await
        .map_err(db_error)?;
        convert(rows)
    }

    async fn search(&self, q: &SearchQuery) -> Result<(Vec<SearchRef>, i64)> {
        crate::search::search(&self.db.read, q).await
    }

    async fn search_literal(
        &self,
        q: &LiteralSearchQuery,
    ) -> Result<(Vec<(String, Option<String>)>, i64)> {
        crate::search::search_literal(&self.db.read, q).await
    }
}

// ---- 분류·태그 ----

struct CategoryRow {
    id: String,
    board_id: String,
    name: String,
    slug: String,
    sort_order: i64,
    created_at: String,
    updated_at: String,
}

impl From<CategoryRow> for Category {
    fn from(r: CategoryRow) -> Self {
        Category {
            id: r.id,
            board_id: r.board_id,
            name: r.name,
            slug: r.slug,
            sort_order: r.sort_order,
            created_at: r.created_at,
            updated_at: r.updated_at,
        }
    }
}

struct TagRow {
    id: String,
    name: String,
    slug: String,
    created_at: String,
    posts: i64,
}

impl From<TagRow> for Tag {
    fn from(r: TagRow) -> Self {
        Tag {
            id: r.id,
            name: r.name,
            slug: r.slug,
            created_at: r.created_at,
            posts: r.posts,
        }
    }
}

async fn categories_of_board<'e>(
    e: impl SqliteExecutor<'e>,
    board_id: &str,
) -> Result<Vec<Category>> {
    let rows = sqlx::query_as!(
        CategoryRow,
        "SELECT id, board_id, name, slug, sort_order, created_at, updated_at
         FROM categories WHERE board_id = ? ORDER BY sort_order, name",
        board_id
    )
    .fetch_all(e)
    .await
    .map_err(db_error)?;
    Ok(rows.into_iter().map(Category::from).collect())
}

async fn category_by_id<'e>(e: impl SqliteExecutor<'e>, id: &str) -> Result<Option<Category>> {
    Ok(sqlx::query_as!(
        CategoryRow,
        "SELECT id, board_id, name, slug, sort_order, created_at, updated_at
         FROM categories WHERE id = ?",
        id
    )
    .fetch_optional(e)
    .await
    .map_err(db_error)?
    .map(Category::from))
}

async fn tag_by_slug<'e>(e: impl SqliteExecutor<'e>, slug: &str) -> Result<Option<Tag>> {
    Ok(sqlx::query_as!(
        TagRow,
        r#"SELECT id, name, slug, created_at, 0 AS "posts!: i64" FROM tags WHERE slug = ?"#,
        slug
    )
    .fetch_optional(e)
    .await
    .map_err(db_error)?
    .map(Tag::from))
}

/// 글의 태그를 통째로 바꾼다. 없는 태그는 만든다. 이름은 core 가 정규화해 둔 것.
async fn write_post_tags(
    conn: &mut sqlx::SqliteConnection,
    post_id: &str,
    tags: &[String],
) -> Result<()> {
    sqlx::query!("DELETE FROM post_tags WHERE post_id = ?", post_id)
        .execute(&mut *conn)
        .await
        .map_err(db_error)?;
    for (i, name) in tags.iter().enumerate() {
        let slug = slug::tag(name);
        if slug.is_empty() {
            return Err(Error::internal(format!("주소가 비는 태그: {name}")));
        }
        let (id, now, position) = (clock::new_id(), clock::now(), i as i64);
        sqlx::query!(
            "INSERT INTO tags (id, name, slug, created_at) VALUES (?, ?, ?, ?)
             ON CONFLICT (slug) DO NOTHING",
            id,
            name,
            slug,
            now
        )
        .execute(&mut *conn)
        .await
        .map_err(db_error)?;
        sqlx::query!(
            "INSERT INTO post_tags (post_id, tag_id, position)
             SELECT ?, id, ? FROM tags WHERE slug = ?",
            post_id,
            position,
            slug
        )
        .execute(&mut *conn)
        .await
        .map_err(db_error)?;
    }
    Ok(())
}

#[async_trait]
impl WriteTx for SqliteWriteTx {
    async fn insert_audit(&mut self, rec: &AuditRecord) -> Result<()> {
        insert_audit(&mut self.tx, rec).await
    }

    async fn commit(self: Box<Self>) -> Result<()> {
        self.tx.commit().await.map_err(db_error)
    }

    async fn upsert_module(&mut self, row: &ModuleRow) -> Result<()> {
        let settings = json(&row.settings)?;
        sqlx::query!(
            "INSERT INTO modules (id, enabled, version, settings, cursor, installed_at, updated_at)
             VALUES (?, ?, ?, ?, ?, ?, ?)
             ON CONFLICT (id) DO UPDATE SET enabled = excluded.enabled, version = excluded.version,
                settings = excluded.settings, cursor = excluded.cursor, updated_at = excluded.updated_at",
            row.id,
            row.enabled,
            row.version,
            settings,
            row.cursor,
            row.installed_at,
            row.updated_at
        )
        .execute(&mut *self.tx)
        .await
        .map_err(db_error)?;
        Ok(())
    }

    async fn delete_module(&mut self, id: &str) -> Result<()> {
        sqlx::query!("DELETE FROM module_event_failures WHERE module = ?", id)
            .execute(&mut *self.tx)
            .await
            .map_err(db_error)?;
        sqlx::query!("DELETE FROM modules WHERE id = ?", id)
            .execute(&mut *self.tx)
            .await
            .map_err(db_error)?;
        Ok(())
    }

    async fn board_by_id(&mut self, id: &str) -> Result<Option<Board>> {
        board_by_id(&mut *self.tx, id).await
    }

    async fn board_by_slug(&mut self, slug: &str) -> Result<Option<Board>> {
        board_by_slug(&mut *self.tx, slug).await
    }

    async fn insert_board(&mut self, b: &Board) -> Result<()> {
        let policy = json(&b.policy)?;
        let fields = json(&b.field_schema)?;
        sqlx::query!(
            "INSERT INTO boards (id, slug, name, kind, layout, list_order, policy, sort_order,
                                 field_schema, description, created_at, updated_at)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
            b.id,
            b.slug,
            b.name,
            b.kind,
            b.layout,
            b.list_order,
            policy,
            b.sort_order,
            fields,
            b.description,
            b.created_at,
            b.updated_at
        )
        .execute(&mut *self.tx)
        .await
        .map_err(db_error)?;
        Ok(())
    }

    async fn update_board(&mut self, b: &Board) -> Result<()> {
        let policy = json(&b.policy)?;
        let fields = json(&b.field_schema)?;
        sqlx::query!(
            "UPDATE boards SET name = ?, kind = ?, layout = ?, list_order = ?, policy = ?,
                              sort_order = ?, field_schema = ?, description = ?, updated_at = ?
             WHERE id = ?",
            b.name,
            b.kind,
            b.layout,
            b.list_order,
            policy,
            b.sort_order,
            fields,
            b.description,
            b.updated_at,
            b.id
        )
        .execute(&mut *self.tx)
        .await
        .map_err(db_error)?;
        Ok(())
    }

    async fn delete_board(&mut self, id: &str) -> Result<()> {
        sqlx::query!("DELETE FROM boards WHERE id = ?", id)
            .execute(&mut *self.tx)
            .await
            .map_err(db_error)?;
        Ok(())
    }

    async fn count_board_posts(&mut self, board_id: &str) -> Result<i64> {
        sqlx::query_scalar!(
            r#"SELECT count(*) AS "n!: i64" FROM posts WHERE board_id = ?"#,
            board_id
        )
        .fetch_one(&mut *self.tx)
        .await
        .map_err(db_error)
    }

    async fn post_by_id(&mut self, id: &str) -> Result<Option<Post>> {
        post_by_id(&mut *self.tx, id).await
    }

    async fn touch_post(&mut self, id: &str, at: &str) -> Result<()> {
        sqlx::query!(
            "UPDATE posts SET last_activity_at = ?1
             WHERE id = ?2 AND (last_activity_at IS NULL OR last_activity_at < ?1)",
            at,
            id
        )
        .execute(&mut *self.tx)
        .await
        .map_err(db_error)?;
        Ok(())
    }

    async fn reply_count(&mut self, parent_id: &str) -> Result<i64> {
        sqlx::query_scalar!(
            r#"SELECT count(*) AS "n!: i64" FROM posts WHERE parent_id = ?"#,
            parent_id
        )
        .fetch_one(&mut *self.tx)
        .await
        .map_err(db_error)
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
        let meta = json(&p.meta)?;
        sqlx::query!(
            "INSERT INTO posts (id, board_id, kind, status, title, slug, body_md, excerpt,
                                author_id, actor_type, published_at, created_at, updated_at,
                                scheduled_at, cover, category_id, is_notice, is_secret,
                                guest_name, guest_pw_hash, parent_id, thread_id, thread_path,
                                thread_owner, meta, last_activity_at, accepted_comment_id,
                                is_locked)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?,
                     ?)",
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
            p.updated_at,
            p.scheduled_at,
            p.cover,
            p.category_id,
            p.is_notice,
            p.is_secret,
            p.guest_name,
            p.guest_pw_hash,
            p.parent_id,
            p.thread_id,
            p.thread_path,
            p.thread_owner,
            meta,
            p.last_activity_at,
            p.accepted_comment_id,
            p.is_locked
        )
        .execute(&mut *self.tx)
        .await
        .map_err(db_error)?;
        write_post_tags(&mut self.tx, &p.id, &p.tags).await?;
        crate::search::put_post(&mut self.tx, p).await
    }

    async fn update_post(&mut self, p: &Post) -> Result<()> {
        let status = p.status.as_str();
        let meta = json(&p.meta)?;
        let res = sqlx::query!(
            "UPDATE posts SET board_id = ?, status = ?, title = ?, slug = ?, body_md = ?,
                              excerpt = ?, published_at = ?, updated_at = ?,
                              scheduled_at = ?, cover = ?, category_id = ?,
                              is_notice = ?, is_secret = ?, meta = ?,
                              accepted_comment_id = ?, is_locked = ?
             WHERE id = ?",
            p.board_id,
            status,
            p.title,
            p.slug,
            p.body_md,
            p.excerpt,
            p.published_at,
            p.updated_at,
            p.scheduled_at,
            p.cover,
            p.category_id,
            p.is_notice,
            p.is_secret,
            meta,
            p.accepted_comment_id,
            p.is_locked,
            p.id
        )
        .execute(&mut *self.tx)
        .await
        .map_err(db_error)?;
        if res.rows_affected() != 1 {
            return Err(Error::new(Code::NotFound).arg("id", p.id.clone()));
        }
        write_post_tags(&mut self.tx, &p.id, &p.tags).await?;
        crate::search::put_post(&mut self.tx, p).await
    }

    async fn set_view_count(&mut self, id: &str, n: i64) -> Result<()> {
        let res = sqlx::query!("UPDATE posts SET view_count = ? WHERE id = ?", n, id)
            .execute(&mut *self.tx)
            .await
            .map_err(db_error)?;
        if res.rows_affected() != 1 {
            return Err(Error::new(Code::NotFound).arg("id", id.to_owned()));
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

    async fn token_by_id(&mut self, id: &str) -> Result<Option<Token>> {
        sqlx::query_as!(
            TokenRow,
            "SELECT id, name, token_hash, scopes, board_ids, expires_at, last_used_at,
                    revoked_at, created_at, updated_at
             FROM tokens WHERE id = ?",
            id
        )
        .fetch_optional(&mut *self.tx)
        .await
        .map_err(db_error)?
        .map(Token::try_from)
        .transpose()
    }

    async fn revoke_token(&mut self, id: &str, at: &str) -> Result<()> {
        sqlx::query!(
            "UPDATE tokens SET revoked_at = ?, updated_at = ? WHERE id = ? AND revoked_at IS NULL",
            at,
            at,
            id
        )
        .execute(&mut *self.tx)
        .await
        .map_err(db_error)?;
        Ok(())
    }

    async fn page_by_id(&mut self, id: &str) -> Result<Option<Page>> {
        page_by_id(&mut *self.tx, id).await
    }

    async fn page_by_slug(&mut self, slug: &str) -> Result<Option<Page>> {
        page_by_slug(&mut *self.tx, slug).await
    }

    async fn insert_page(&mut self, p: &Page) -> Result<()> {
        let (status, actor_type) = (p.status.as_str(), p.actor_type.as_str());
        sqlx::query!(
            "INSERT INTO pages (id, slug, title, body_md, status, layout, author_id, actor_type,
                                sort_order, published_at, created_at, updated_at)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
            p.id,
            p.slug,
            p.title,
            p.body_md,
            status,
            p.layout,
            p.author_id,
            actor_type,
            p.sort_order,
            p.published_at,
            p.created_at,
            p.updated_at
        )
        .execute(&mut *self.tx)
        .await
        .map_err(db_error)?;
        crate::search::put_page(&mut self.tx, p).await
    }

    async fn update_page(&mut self, p: &Page) -> Result<()> {
        let status = p.status.as_str();
        sqlx::query!(
            "UPDATE pages SET slug = ?, title = ?, body_md = ?, status = ?, layout = ?,
                              sort_order = ?, published_at = ?, updated_at = ?
             WHERE id = ?",
            p.slug,
            p.title,
            p.body_md,
            status,
            p.layout,
            p.sort_order,
            p.published_at,
            p.updated_at,
            p.id
        )
        .execute(&mut *self.tx)
        .await
        .map_err(db_error)?;
        crate::search::put_page(&mut self.tx, p).await
    }

    async fn rebuild_search(&mut self) -> Result<(i64, i64)> {
        crate::search::rebuild(&mut self.tx).await
    }

    async fn menu_rows(&mut self, location: &str) -> Result<Vec<MenuRow>> {
        menu_rows(&mut *self.tx, location).await
    }

    async fn widget_by_id(&mut self, id: &str) -> Result<Option<Widget>> {
        widget_by_id(&mut *self.tx, id).await
    }

    async fn insert_widget(&mut self, w: &Widget) -> Result<()> {
        let config = json(&w.config)?;
        sqlx::query!(
            "INSERT INTO widgets (id, kind, region, scope, title, config, sort_order, enabled,
                                  created_at, updated_at)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
            w.id,
            w.kind,
            w.region,
            w.scope,
            w.title,
            config,
            w.sort_order,
            w.enabled,
            w.created_at,
            w.updated_at
        )
        .execute(&mut *self.tx)
        .await
        .map_err(db_error)?;
        Ok(())
    }

    async fn update_widget(&mut self, w: &Widget) -> Result<()> {
        let config = json(&w.config)?;
        sqlx::query!(
            "UPDATE widgets SET region = ?, scope = ?, title = ?, config = ?, sort_order = ?,
                                enabled = ?, updated_at = ?
             WHERE id = ?",
            w.region,
            w.scope,
            w.title,
            config,
            w.sort_order,
            w.enabled,
            w.updated_at,
            w.id
        )
        .execute(&mut *self.tx)
        .await
        .map_err(db_error)?;
        Ok(())
    }

    async fn delete_widget(&mut self, id: &str) -> Result<()> {
        sqlx::query!("DELETE FROM widgets WHERE id = ?", id)
            .execute(&mut *self.tx)
            .await
            .map_err(db_error)?;
        Ok(())
    }

    async fn replace_menu(&mut self, location: &str, rows: &[MenuRow]) -> Result<()> {
        sqlx::query!("DELETE FROM menus WHERE location = ?", location)
            .execute(&mut *self.tx)
            .await
            .map_err(db_error)?;
        // 부모가 먼저 오도록 flatten 된 순서 그대로 넣는다.
        for r in rows {
            let link = json(&r.link)?;
            sqlx::query!(
                "INSERT INTO menus (id, location, parent_id, label, link, sort_order, created_at)
                 VALUES (?, ?, ?, ?, ?, ?, ?)",
                r.id,
                location,
                r.parent_id,
                r.label,
                link,
                r.sort_order,
                r.created_at
            )
            .execute(&mut *self.tx)
            .await
            .map_err(db_error)?;
        }
        Ok(())
    }

    async fn settings(&mut self) -> Result<Vec<(String, Value)>> {
        settings(&mut *self.tx).await
    }

    async fn put_setting(&mut self, key: &str, value: &Value, at: &str) -> Result<()> {
        let v = json(value)?;
        sqlx::query!(
            "INSERT INTO site_settings (key, value, updated_at) VALUES (?, ?, ?)
             ON CONFLICT (key) DO UPDATE SET value = excluded.value, updated_at = excluded.updated_at",
            key,
            v,
            at
        )
        .execute(&mut *self.tx)
        .await
        .map_err(db_error)?;
        Ok(())
    }

    async fn member_by_id(&mut self, id: &str) -> Result<Option<Member>> {
        member_by_id(&mut *self.tx, id).await
    }

    async fn member_by_email(&mut self, email: &str) -> Result<Option<Member>> {
        member_by_email(&mut *self.tx, email).await
    }

    async fn insert_member(&mut self, m: &Member) -> Result<()> {
        let status = m.status.as_str();
        sqlx::query!(
            "INSERT INTO members (id, email, display_name, password_hash, status, email_verified_at,
                                  withdrawn_at, created_at, updated_at)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)",
            m.id,
            m.email,
            m.display_name,
            m.password_hash,
            status,
            m.email_verified_at,
            m.withdrawn_at,
            m.created_at,
            m.updated_at
        )
        .execute(&mut *self.tx)
        .await
        .map_err(db_error)?;
        Ok(())
    }

    async fn update_member(&mut self, m: &Member) -> Result<()> {
        let status = m.status.as_str();
        sqlx::query!(
            "UPDATE members SET
                    email = CASE WHEN EXISTS (SELECT 1 FROM member_dormant WHERE member_id = ?)
                                 THEN email ELSE ? END,
                    display_name = ?, password_hash = ?, status = ?,
                    email_verified_at = ?, withdrawn_at = ?, updated_at = ?
             WHERE id = ?",
            m.id,
            m.email,
            m.display_name,
            m.password_hash,
            status,
            m.email_verified_at,
            m.withdrawn_at,
            m.updated_at,
            m.id
        )
        .execute(&mut *self.tx)
        .await
        .map_err(db_error)?;
        Ok(())
    }

    async fn set_member_bio(&mut self, member_id: &str, bio: &str, now: &str) -> Result<()> {
        crate::profiles::set_bio(&mut *self.tx, member_id, bio, now).await
    }

    async fn clear_member_roles(&mut self, member_id: &str) -> Result<()> {
        sqlx::query!("DELETE FROM member_roles WHERE member_id = ?", member_id)
            .execute(&mut *self.tx)
            .await
            .map_err(db_error)?;
        Ok(())
    }

    async fn member_by_name(&mut self, name: &str) -> Result<Option<Member>> {
        member_by_name(&mut *self.tx, name).await
    }

    async fn queue_mail(&mut self, m: &MailOut) -> Result<bool> {
        crate::mail::queue(&mut self.tx, m).await
    }

    async fn comment_by_id(&mut self, id: &str) -> Result<Option<Comment>> {
        crate::comments::by_id(&mut *self.tx, id).await
    }

    async fn insert_comment(&mut self, c: &Comment) -> Result<()> {
        crate::comments::insert(&mut self.tx, c).await
    }

    async fn update_comment(&mut self, c: &Comment) -> Result<()> {
        crate::comments::update(&mut self.tx, c).await
    }

    async fn count_live_replies(&mut self, parent_id: &str) -> Result<i64> {
        crate::comments::live_replies(&mut self.tx, parent_id).await
    }

    async fn add_member_role(&mut self, member_id: &str, role: &str) -> Result<()> {
        let done = sqlx::query!(
            "INSERT OR IGNORE INTO member_roles (member_id, role_id)
             SELECT ?, id FROM roles WHERE name = ?",
            member_id,
            role
        )
        .execute(&mut *self.tx)
        .await
        .map_err(db_error)?;
        if done.rows_affected() == 0 {
            let exists = sqlx::query_scalar!("SELECT count(*) FROM roles WHERE name = ?", role)
                .fetch_one(&mut *self.tx)
                .await
                .map_err(db_error)?;
            if exists == 0 {
                return Err(Error::internal(format!("역할이 없다: {role}")));
            }
        }
        Ok(())
    }

    async fn insert_ticket(&mut self, t: &LoginTicket) -> Result<()> {
        let purpose = t.purpose.as_str();
        sqlx::query!(
            "INSERT INTO login_tickets (id, token_hash, member_id, purpose, created_at, expires_at, used_at)
             VALUES (?, ?, ?, ?, ?, ?, ?)",
            t.id,
            t.token_hash,
            t.member_id,
            purpose,
            t.created_at,
            t.expires_at,
            t.used_at
        )
        .execute(&mut *self.tx)
        .await
        .map_err(db_error)?;
        Ok(())
    }

    async fn delete_open_tickets(
        &mut self,
        member_id: &str,
        purpose: TicketPurpose,
    ) -> Result<u64> {
        let purpose = purpose.as_str();
        Ok(sqlx::query!(
            "DELETE FROM login_tickets WHERE member_id = ? AND purpose = ? AND used_at IS NULL",
            member_id,
            purpose
        )
        .execute(&mut *self.tx)
        .await
        .map_err(db_error)?
        .rows_affected())
    }

    async fn use_ticket(
        &mut self,
        hash: &str,
        purpose: TicketPurpose,
        now: &str,
    ) -> Result<Option<LoginTicket>> {
        let kind = purpose.as_str();
        let row = sqlx::query!(
            "UPDATE login_tickets SET used_at = ?
             WHERE token_hash = ? AND purpose = ? AND used_at IS NULL AND expires_at > ?
             RETURNING id, token_hash, member_id, created_at, expires_at",
            now,
            hash,
            kind,
            now
        )
        .fetch_optional(&mut *self.tx)
        .await
        .map_err(db_error)?;
        Ok(row.map(|r| LoginTicket {
            id: r.id,
            token_hash: r.token_hash,
            member_id: r.member_id,
            purpose,
            created_at: r.created_at,
            expires_at: r.expires_at,
            used_at: Some(now.to_owned()),
        }))
    }

    async fn insert_session(&mut self, s: &Session) -> Result<()> {
        sqlx::query!(
            "INSERT INTO sessions (id, token_hash, member_id, csrf_token, created_at, expires_at)
             VALUES (?, ?, ?, ?, ?, ?)",
            s.id,
            s.token_hash,
            s.member_id,
            s.csrf_token,
            s.created_at,
            s.expires_at
        )
        .execute(&mut *self.tx)
        .await
        .map_err(db_error)?;
        Ok(())
    }

    async fn delete_session(&mut self, id: &str) -> Result<u64> {
        Ok(sqlx::query!("DELETE FROM sessions WHERE id = ?", id)
            .execute(&mut *self.tx)
            .await
            .map_err(db_error)?
            .rows_affected())
    }

    async fn delete_member_sessions(&mut self, member_id: &str) -> Result<u64> {
        Ok(
            sqlx::query!("DELETE FROM sessions WHERE member_id = ?", member_id)
                .execute(&mut *self.tx)
                .await
                .map_err(db_error)?
                .rows_affected(),
        )
    }

    async fn reset_link(&mut self, url: &str) -> Result<Option<LinkPreview>> {
        crate::links::reset(&mut self.tx, url).await
    }

    async fn mark_signal_sent(&mut self, key: &str, at: &str) -> Result<bool> {
        Ok(sqlx::query!(
            "INSERT INTO signal_sent (key, at) VALUES (?, ?) ON CONFLICT (key) DO NOTHING",
            key,
            at
        )
        .execute(&mut *self.tx)
        .await
        .map_err(db_error)?
        .rows_affected()
            > 0)
    }

    async fn touch_login(&mut self, member_id: &str, now: &str) -> Result<()> {
        sqlx::query!(
            "UPDATE members SET last_login_at = ?, dormant_notice_at = NULL WHERE id = ?",
            now,
            member_id
        )
        .execute(&mut *self.tx)
        .await
        .map_err(db_error)?;
        Ok(())
    }

    async fn mark_dormant_notice(&mut self, member_id: &str, now: &str) -> Result<()> {
        sqlx::query!(
            "UPDATE members SET dormant_notice_at = ? WHERE id = ?",
            now,
            member_id
        )
        .execute(&mut *self.tx)
        .await
        .map_err(db_error)?;
        Ok(())
    }

    async fn make_dormant(&mut self, member_id: &str, now: &str) -> Result<bool> {
        let moved = sqlx::query!(
            "INSERT INTO member_dormant (member_id, email, dormant_at)
             SELECT id, email, ? FROM members
             WHERE id = ? AND email NOT LIKE '%@invalid'
               AND NOT EXISTS (SELECT 1 FROM member_dormant WHERE member_id = ?)",
            now,
            member_id,
            member_id
        )
        .execute(&mut *self.tx)
        .await
        .map_err(db_error)?
        .rows_affected();
        if moved == 0 {
            return Ok(false);
        }
        sqlx::query!(
            "UPDATE members SET email = 'dormant+' || lower(id) || '@invalid', dormant_notice_at = NULL
             WHERE id = ?",
            member_id
        )
        .execute(&mut *self.tx)
        .await
        .map_err(db_error)?;
        Ok(true)
    }

    async fn wake_member(&mut self, member_id: &str) -> Result<Option<String>> {
        let email = sqlx::query_scalar!(
            "SELECT email FROM member_dormant WHERE member_id = ?",
            member_id
        )
        .fetch_optional(&mut *self.tx)
        .await
        .map_err(db_error)?;
        if let Some(e) = &email {
            sqlx::query!("DELETE FROM member_dormant WHERE member_id = ?", member_id)
                .execute(&mut *self.tx)
                .await
                .map_err(db_error)?;
            sqlx::query!("UPDATE members SET email = ? WHERE id = ?", e, member_id)
                .execute(&mut *self.tx)
                .await
                .map_err(db_error)?;
        }
        Ok(email)
    }

    async fn member_totp(&mut self, member_id: &str) -> Result<Option<MemberTotp>> {
        member_totp(&mut *self.tx, member_id).await
    }

    async fn put_member_totp(&mut self, t: &MemberTotp) -> Result<()> {
        sqlx::query!(
            "INSERT INTO member_totp (member_id, secret, enabled_at, last_step, created_at)
             VALUES (?, ?, ?, ?, ?)
             ON CONFLICT (member_id) DO UPDATE SET secret = excluded.secret,
                 enabled_at = excluded.enabled_at, last_step = excluded.last_step,
                 created_at = excluded.created_at",
            t.member_id,
            t.secret,
            t.enabled_at,
            t.last_step,
            t.created_at
        )
        .execute(&mut *self.tx)
        .await
        .map_err(db_error)?;
        Ok(())
    }

    async fn advance_totp_step(&mut self, member_id: &str, step: i64) -> Result<bool> {
        Ok(sqlx::query!(
            "UPDATE member_totp SET last_step = ? WHERE member_id = ? AND last_step < ?",
            step,
            member_id,
            step
        )
        .execute(&mut *self.tx)
        .await
        .map_err(db_error)?
        .rows_affected()
            == 1)
    }

    async fn delete_member_totp(&mut self, member_id: &str) -> Result<bool> {
        sqlx::query!(
            "DELETE FROM member_recovery_codes WHERE member_id = ?",
            member_id
        )
        .execute(&mut *self.tx)
        .await
        .map_err(db_error)?;
        sqlx::query!("DELETE FROM totp_challenges WHERE member_id = ?", member_id)
            .execute(&mut *self.tx)
            .await
            .map_err(db_error)?;
        Ok(
            sqlx::query!("DELETE FROM member_totp WHERE member_id = ?", member_id)
                .execute(&mut *self.tx)
                .await
                .map_err(db_error)?
                .rows_affected()
                == 1,
        )
    }

    async fn replace_recovery_codes(&mut self, member_id: &str, hashes: &[String]) -> Result<()> {
        sqlx::query!(
            "DELETE FROM member_recovery_codes WHERE member_id = ?",
            member_id
        )
        .execute(&mut *self.tx)
        .await
        .map_err(db_error)?;
        for h in hashes {
            let id = clock::new_id();
            sqlx::query!(
                "INSERT INTO member_recovery_codes (id, member_id, code_hash) VALUES (?, ?, ?)",
                id,
                member_id,
                h
            )
            .execute(&mut *self.tx)
            .await
            .map_err(db_error)?;
        }
        Ok(())
    }

    async fn use_recovery_code(&mut self, member_id: &str, hash: &str, now: &str) -> Result<bool> {
        Ok(sqlx::query!(
            "UPDATE member_recovery_codes SET used_at = ?
             WHERE member_id = ? AND code_hash = ? AND used_at IS NULL",
            now,
            member_id,
            hash
        )
        .execute(&mut *self.tx)
        .await
        .map_err(db_error)?
        .rows_affected()
            == 1)
    }

    async fn insert_totp_challenge(&mut self, c: &TotpChallenge) -> Result<()> {
        sqlx::query!(
            "INSERT INTO totp_challenges (id, token_hash, member_id, method, attempts, created_at, expires_at)
             VALUES (?, ?, ?, ?, ?, ?, ?)",
            c.id,
            c.token_hash,
            c.member_id,
            c.method,
            c.attempts,
            c.created_at,
            c.expires_at
        )
        .execute(&mut *self.tx)
        .await
        .map_err(db_error)?;
        Ok(())
    }

    async fn take_totp_challenge(&mut self, id: &str) -> Result<bool> {
        Ok(sqlx::query!("DELETE FROM totp_challenges WHERE id = ?", id)
            .execute(&mut *self.tx)
            .await
            .map_err(db_error)?
            .rows_affected()
            == 1)
    }

    async fn media_by_id(&mut self, id: &str) -> Result<Option<Media>> {
        media_by_id(&mut *self.tx, id).await
    }

    async fn insert_media(&mut self, m: &Media) -> Result<()> {
        let variants = variants_json(m)?;
        let size = m.size;
        let (width, height) = (i64::from(m.width), i64::from(m.height));
        sqlx::query!(
            "INSERT INTO media (id, path, mime, size, width, height, alt_text, variants,
                                created_at, updated_at)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
            m.id,
            m.path,
            m.mime,
            size,
            width,
            height,
            m.alt_text,
            variants,
            m.created_at,
            m.updated_at
        )
        .execute(&mut *self.tx)
        .await
        .map_err(db_error)?;
        Ok(())
    }

    async fn update_media(&mut self, m: &Media) -> Result<()> {
        let variants = variants_json(m)?;
        sqlx::query!(
            "UPDATE media SET alt_text = ?, variants = ?, updated_at = ? WHERE id = ?",
            m.alt_text,
            variants,
            m.updated_at,
            m.id
        )
        .execute(&mut *self.tx)
        .await
        .map_err(db_error)?;
        Ok(())
    }

    async fn delete_media(&mut self, id: &str) -> Result<()> {
        sqlx::query!("DELETE FROM media WHERE id = ?", id)
            .execute(&mut *self.tx)
            .await
            .map_err(db_error)?;
        Ok(())
    }

    async fn attachment_by_id(&mut self, id: &str) -> Result<Option<Attachment>> {
        attachment_by_id(&mut *self.tx, id).await
    }

    async fn insert_attachment(&mut self, a: &Attachment) -> Result<()> {
        sqlx::query!(
            "INSERT INTO files (id, path, filename, mime, size, created_at)
             VALUES (?, ?, ?, ?, ?, ?)",
            a.id,
            a.path,
            a.filename,
            a.mime,
            a.size,
            a.created_at
        )
        .execute(&mut *self.tx)
        .await
        .map_err(db_error)?;
        Ok(())
    }

    async fn delete_attachment(&mut self, id: &str) -> Result<()> {
        sqlx::query!("DELETE FROM files WHERE id = ?", id)
            .execute(&mut *self.tx)
            .await
            .map_err(db_error)?;
        Ok(())
    }

    async fn template_by_id(&mut self, id: &str) -> Result<Option<PostTemplate>> {
        let row = sqlx::query_as!(
            TemplateRow,
            "SELECT id, name, body_md, created_at, updated_at FROM post_templates WHERE id = ?",
            id
        )
        .fetch_optional(&mut *self.tx)
        .await
        .map_err(db_error)?;
        Ok(row.map(PostTemplate::from))
    }

    async fn template_by_name(&mut self, name: &str) -> Result<Option<PostTemplate>> {
        let row = sqlx::query_as!(
            TemplateRow,
            "SELECT id, name, body_md, created_at, updated_at FROM post_templates WHERE name = ?",
            name
        )
        .fetch_optional(&mut *self.tx)
        .await
        .map_err(db_error)?;
        Ok(row.map(PostTemplate::from))
    }

    async fn insert_template(&mut self, t: &PostTemplate) -> Result<()> {
        sqlx::query!(
            "INSERT INTO post_templates (id, name, body_md, created_at, updated_at)
             VALUES (?, ?, ?, ?, ?)",
            t.id,
            t.name,
            t.body_md,
            t.created_at,
            t.updated_at
        )
        .execute(&mut *self.tx)
        .await
        .map_err(db_error)?;
        Ok(())
    }

    async fn update_template(&mut self, t: &PostTemplate) -> Result<()> {
        sqlx::query!(
            "UPDATE post_templates SET name = ?, body_md = ?, updated_at = ? WHERE id = ?",
            t.name,
            t.body_md,
            t.updated_at,
            t.id
        )
        .execute(&mut *self.tx)
        .await
        .map_err(db_error)?;
        Ok(())
    }

    async fn delete_template(&mut self, id: &str) -> Result<()> {
        sqlx::query!("DELETE FROM post_templates WHERE id = ?", id)
            .execute(&mut *self.tx)
            .await
            .map_err(db_error)?;
        Ok(())
    }

    async fn font_by_slug(&mut self, slug: &str) -> Result<Option<Font>> {
        let fonts = font_rows(&mut *self.tx, Some(slug)).await?;
        let files = font_file_rows(&mut *self.tx, Some(slug)).await?;
        Ok(assemble_fonts(fonts, files).pop())
    }

    async fn insert_font(&mut self, f: &Font) -> Result<()> {
        sqlx::query!(
            "INSERT INTO fonts (id, slug, name, fallback, license, license_path, created_at, updated_at)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
            f.id,
            f.slug,
            f.name,
            f.fallback,
            f.license,
            f.license_path,
            f.created_at,
            f.updated_at
        )
        .execute(&mut *self.tx)
        .await
        .map_err(db_error)?;
        Ok(())
    }

    async fn update_font(&mut self, f: &Font) -> Result<()> {
        sqlx::query!(
            "UPDATE fonts SET name = ?, fallback = ?, license = ?, license_path = ?, updated_at = ?
             WHERE id = ?",
            f.name,
            f.fallback,
            f.license,
            f.license_path,
            f.updated_at,
            f.id
        )
        .execute(&mut *self.tx)
        .await
        .map_err(db_error)?;
        Ok(())
    }

    async fn delete_font(&mut self, id: &str) -> Result<()> {
        // 외래키 CASCADE 가 있지만 연결 설정에 기대지 않고 파일 행을 먼저 지운다.
        sqlx::query!("DELETE FROM font_files WHERE font_id = ?", id)
            .execute(&mut *self.tx)
            .await
            .map_err(db_error)?;
        sqlx::query!("DELETE FROM fonts WHERE id = ?", id)
            .execute(&mut *self.tx)
            .await
            .map_err(db_error)?;
        Ok(())
    }

    async fn insert_font_file(&mut self, font_id: &str, f: &FontFile) -> Result<()> {
        let italic = i64::from(f.italic);
        sqlx::query!(
            "INSERT INTO font_files (id, font_id, weight, italic, format, path, size, sha256, created_at)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)",
            f.id,
            font_id,
            f.weight,
            italic,
            f.format,
            f.path,
            f.size,
            f.sha256,
            f.created_at
        )
        .execute(&mut *self.tx)
        .await
        .map_err(db_error)?;
        Ok(())
    }

    async fn delete_font_file(&mut self, id: &str) -> Result<()> {
        sqlx::query!("DELETE FROM font_files WHERE id = ?", id)
            .execute(&mut *self.tx)
            .await
            .map_err(db_error)?;
        Ok(())
    }

    async fn tag_by_slug(&mut self, slug: &str) -> Result<Option<Tag>> {
        tag_by_slug(&mut *self.tx, slug).await
    }

    async fn categories_of_board(&mut self, board_id: &str) -> Result<Vec<Category>> {
        categories_of_board(&mut *self.tx, board_id).await
    }

    async fn category_by_id(&mut self, id: &str) -> Result<Option<Category>> {
        category_by_id(&mut *self.tx, id).await
    }

    async fn category_by_slug(&mut self, board_id: &str, slug: &str) -> Result<Option<Category>> {
        Ok(sqlx::query_as!(
            CategoryRow,
            "SELECT id, board_id, name, slug, sort_order, created_at, updated_at
             FROM categories WHERE board_id = ? AND slug = ?",
            board_id,
            slug
        )
        .fetch_optional(&mut *self.tx)
        .await
        .map_err(db_error)?
        .map(Category::from))
    }

    async fn insert_category(&mut self, c: &Category) -> Result<()> {
        sqlx::query!(
            "INSERT INTO categories (id, board_id, name, slug, sort_order, created_at, updated_at)
             VALUES (?, ?, ?, ?, ?, ?, ?)",
            c.id,
            c.board_id,
            c.name,
            c.slug,
            c.sort_order,
            c.created_at,
            c.updated_at
        )
        .execute(&mut *self.tx)
        .await
        .map_err(db_error)?;
        Ok(())
    }

    async fn update_category(&mut self, c: &Category) -> Result<()> {
        let res = sqlx::query!(
            "UPDATE categories SET name = ?, slug = ?, sort_order = ?, updated_at = ? WHERE id = ?",
            c.name,
            c.slug,
            c.sort_order,
            c.updated_at,
            c.id
        )
        .execute(&mut *self.tx)
        .await
        .map_err(db_error)?;
        if res.rows_affected() != 1 {
            return Err(Error::new(Code::NotFound).arg("id", c.id.clone()));
        }
        Ok(())
    }

    async fn delete_category(&mut self, id: &str) -> Result<u64> {
        let cleared = sqlx::query!(
            "UPDATE posts SET category_id = NULL WHERE category_id = ?",
            id
        )
        .execute(&mut *self.tx)
        .await
        .map_err(db_error)?
        .rows_affected();
        sqlx::query!("DELETE FROM categories WHERE id = ?", id)
            .execute(&mut *self.tx)
            .await
            .map_err(db_error)?;
        Ok(cleared)
    }
}

// ---- 글 틀 ----

struct TemplateRow {
    id: String,
    name: String,
    body_md: String,
    created_at: String,
    updated_at: String,
}

impl From<TemplateRow> for PostTemplate {
    fn from(r: TemplateRow) -> Self {
        PostTemplate {
            id: r.id,
            name: r.name,
            body_md: r.body_md,
            builtin: false,
            created_at: r.created_at,
            updated_at: r.updated_at,
        }
    }
}

// ---- 서체 ----

struct FontRow {
    id: String,
    slug: String,
    name: String,
    fallback: String,
    license: String,
    license_path: Option<String>,
    created_at: String,
    updated_at: String,
}

struct FontFileRow {
    id: String,
    font_id: String,
    slug: String,
    weight: i64,
    italic: i64,
    format: String,
    path: String,
    size: i64,
    sha256: String,
    created_at: String,
}

/// `slug` 가 없으면 전부.
async fn font_rows<'e>(e: impl SqliteExecutor<'e>, slug: Option<&str>) -> Result<Vec<FontRow>> {
    sqlx::query_as!(
        FontRow,
        "SELECT id, slug, name, fallback, license, license_path, created_at, updated_at
         FROM fonts WHERE (?1 IS NULL OR slug = ?1) ORDER BY name, slug",
        slug
    )
    .fetch_all(e)
    .await
    .map_err(db_error)
}

async fn font_file_rows<'e>(
    e: impl SqliteExecutor<'e>,
    slug: Option<&str>,
) -> Result<Vec<FontFileRow>> {
    sqlx::query_as!(
        FontFileRow,
        r#"SELECT ff.id AS "id!", ff.font_id AS "font_id!", f.slug AS "slug!",
                  ff.weight AS "weight!", ff.italic AS "italic!", ff.format AS "format!",
                  ff.path AS "path!", ff.size AS "size!", ff.sha256 AS "sha256!",
                  ff.created_at AS "created_at!"
           FROM font_files ff JOIN fonts f ON f.id = ff.font_id
           WHERE (?1 IS NULL OR f.slug = ?1)
           ORDER BY ff.weight, ff.italic"#,
        slug
    )
    .fetch_all(e)
    .await
    .map_err(db_error)
}

fn assemble_fonts(fonts: Vec<FontRow>, files: Vec<FontFileRow>) -> Vec<Font> {
    fonts
        .into_iter()
        .map(|r| Font {
            files: files
                .iter()
                .filter(|f| f.font_id == r.id)
                .map(|f| FontFile {
                    id: f.id.clone(),
                    weight: f.weight,
                    italic: f.italic != 0,
                    format: f.format.clone(),
                    url: urls::font_file(&f.slug, &f.path),
                    path: f.path.clone(),
                    size: f.size,
                    sha256: f.sha256.clone(),
                    created_at: f.created_at.clone(),
                })
                .collect(),
            license_url: r.license_path.as_ref().map(|_| urls::font_license(&r.slug)),
            id: r.id,
            slug: r.slug,
            name: r.name,
            fallback: r.fallback,
            license: r.license,
            license_path: r.license_path,
            created_at: r.created_at,
            updated_at: r.updated_at,
        })
        .collect()
}

// ---- 첨부 파일 ----

struct AttachmentRow {
    id: String,
    path: String,
    filename: String,
    mime: String,
    size: i64,
    created_at: String,
}

impl From<AttachmentRow> for Attachment {
    fn from(r: AttachmentRow) -> Self {
        Attachment {
            url: urls::file(&r.id),
            id: r.id,
            path: r.path,
            filename: r.filename,
            mime: r.mime,
            size: r.size,
            created_at: r.created_at,
        }
    }
}

async fn attachment_by_id<'e>(e: impl SqliteExecutor<'e>, id: &str) -> Result<Option<Attachment>> {
    let row = sqlx::query_as!(
        AttachmentRow,
        "SELECT id, path, filename, mime, size, created_at FROM files WHERE id = ?",
        id
    )
    .fetch_optional(e)
    .await
    .map_err(db_error)?;
    Ok(row.map(Attachment::from))
}

// ---- 미디어 ----

struct MediaRow {
    id: String,
    path: String,
    mime: String,
    size: i64,
    width: Option<i64>,
    height: Option<i64>,
    alt_text: Option<String>,
    variants: String,
    created_at: String,
    updated_at: String,
}

impl TryFrom<MediaRow> for Media {
    type Error = Error;

    fn try_from(r: MediaRow) -> Result<Self> {
        let mut variants: Vec<MediaVariant> =
            serde_json::from_str(&r.variants).map_err(Error::internal)?;
        for v in &mut variants {
            v.url = urls::media(&v.path);
        }
        let side = |v: Option<i64>| u32::try_from(v.unwrap_or(0)).unwrap_or(0);
        Ok(Media {
            url: urls::media(&r.path),
            id: r.id,
            path: r.path,
            mime: r.mime,
            size: r.size,
            width: side(r.width),
            height: side(r.height),
            alt_text: r.alt_text,
            variants,
            created_at: r.created_at,
            updated_at: r.updated_at,
        })
    }
}

/// 주소는 경로로 다시 만들 수 있으니 저장하지 않는다.
fn variants_json(m: &Media) -> Result<String> {
    let stored: Vec<MediaVariant> = m
        .variants
        .iter()
        .map(|v| MediaVariant {
            url: String::new(),
            ..v.clone()
        })
        .collect();
    json(&stored)
}

async fn media_by_id<'e>(e: impl SqliteExecutor<'e>, id: &str) -> Result<Option<Media>> {
    sqlx::query_as!(
        MediaRow,
        "SELECT id, path, mime, size, width, height, alt_text, variants, created_at, updated_at
         FROM media WHERE id = ?",
        id
    )
    .fetch_optional(e)
    .await
    .map_err(db_error)?
    .map(Media::try_from)
    .transpose()
}

// ---- 회원·세션 (U12) ----

struct MemberRow {
    id: String,
    email: String,
    display_name: String,
    password_hash: Option<String>,
    status: String,
    email_verified_at: Option<String>,
    withdrawn_at: Option<String>,
    created_at: String,
    updated_at: String,
}

impl TryFrom<MemberRow> for Member {
    type Error = Error;

    fn try_from(r: MemberRow) -> Result<Self> {
        Ok(Member {
            status: MemberStatus::parse(&r.status)
                .ok_or_else(|| Error::internal(format!("알 수 없는 회원 상태: {}", r.status)))?,
            id: r.id,
            email: r.email,
            display_name: r.display_name,
            password_hash: r.password_hash,
            email_verified_at: r.email_verified_at,
            withdrawn_at: r.withdrawn_at,
            created_at: r.created_at,
            updated_at: r.updated_at,
        })
    }
}

async fn member_totp<'e>(
    e: impl SqliteExecutor<'e>,
    member_id: &str,
) -> Result<Option<MemberTotp>> {
    let row = sqlx::query!(
        "SELECT member_id, secret, enabled_at, last_step, created_at
         FROM member_totp WHERE member_id = ?",
        member_id
    )
    .fetch_optional(e)
    .await
    .map_err(db_error)?;
    Ok(row.map(|r| MemberTotp {
        member_id: r.member_id,
        secret: r.secret,
        enabled_at: r.enabled_at,
        last_step: r.last_step,
        created_at: r.created_at,
    }))
}

fn purpose(s: &str) -> Result<TicketPurpose> {
    TicketPurpose::parse(s).ok_or_else(|| Error::internal(format!("알 수 없는 링크 쓰임새: {s}")))
}

async fn member_by_id<'e>(e: impl SqliteExecutor<'e>, id: &str) -> Result<Option<Member>> {
    sqlx::query_as!(
        MemberRow,
        "SELECT id, email, display_name, password_hash, status, email_verified_at, withdrawn_at,
                    created_at, updated_at
         FROM members WHERE id = ?",
        id
    )
    .fetch_optional(e)
    .await
    .map_err(db_error)?
    .map(Member::try_from)
    .transpose()
}

async fn member_by_name<'e>(e: impl SqliteExecutor<'e>, name: &str) -> Result<Option<Member>> {
    sqlx::query_as!(
        MemberRow,
        "SELECT id, email, display_name, password_hash, status, email_verified_at, withdrawn_at,
                created_at, updated_at
         FROM members WHERE display_name = ? COLLATE NOCASE",
        name
    )
    .fetch_optional(e)
    .await
    .map_err(db_error)?
    .map(Member::try_from)
    .transpose()
}

async fn member_by_email<'e>(e: impl SqliteExecutor<'e>, email: &str) -> Result<Option<Member>> {
    sqlx::query_as!(
        MemberRow,
        r#"SELECT m.id AS "id!", COALESCE(d.email, m.email) AS "email!: String",
                  m.display_name AS "display_name!", m.password_hash, m.status AS "status!",
                  m.email_verified_at, m.withdrawn_at, m.created_at AS "created_at!",
                  m.updated_at AS "updated_at!"
           FROM members m LEFT JOIN member_dormant d ON d.member_id = m.id
           WHERE m.email = ? OR d.email = ?"#,
        email,
        email
    )
    .fetch_optional(e)
    .await
    .map_err(db_error)?
    .map(Member::try_from)
    .transpose()
}
