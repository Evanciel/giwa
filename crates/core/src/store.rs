//! 저장소 경계 (PHASE1_PLAN §4.1·§4.2). 구현은 `maru-storage`, 조립은 `cli`.
//!
//! 쓰기는 반드시 [`Tx`] 를 거친다. `Tx::commit` 은 감사 기록이 한 건도 없으면 거부한다.

use std::ops::{Deref, DerefMut};
use std::sync::Arc;

use async_trait::async_trait;
use serde_json::Value;

use crate::actor::Actor;
use crate::audit::{AuditRecord, Target};
use crate::error::{Error, Result};
use crate::model::{
    Board, LoginTicket, Member, MenuLink, Page, PageStatus, Post, PostStatus, Revision, Role,
    Session, Token,
};

/// 서비스 함수의 첫 인자.
#[derive(Clone)]
pub struct Ctx {
    pub store: Arc<dyn Store>,
    /// 절대 URL 의 기준 (`config.toml`). 끝에 `/` 가 없다.
    pub site_url: String,
}

/// 공개 글 목록 조건. 공개 범위 SQL 은 저장소 구현 한 곳에만 있다.
#[derive(Debug, Clone)]
pub struct PublishedQuery {
    pub board_id: Option<String>,
    /// 이 서열 이하의 `read` 정책을 가진 게시판만 포함한다.
    pub max_read_rank: i64,
    pub limit: i64,
    pub offset: i64,
}

/// 관리용 글 목록 조건.
#[derive(Debug, Clone)]
pub struct PostFilter {
    pub board_id: Option<String>,
    pub status: Option<PostStatus>,
    pub limit: i64,
    pub offset: i64,
}

#[async_trait]
pub trait Store: Send + Sync {
    /// `BEGIN IMMEDIATE`. 다른 쓰기가 끝날 때까지 기다린다.
    async fn begin_write(&self) -> Result<Box<dyn WriteTx>>;

    async fn role_by_name(&self, name: &str) -> Result<Option<Role>>;
    async fn token_by_hash(&self, hash: &str) -> Result<Option<Token>>;
    /// 감사 대상이 아닌 시스템 기록.
    async fn touch_token(&self, id: &str, at: &str) -> Result<()>;
    async fn list_tokens(&self) -> Result<Vec<Token>>;

    async fn board_by_id(&self, id: &str) -> Result<Option<Board>>;
    async fn board_by_slug(&self, slug: &str) -> Result<Option<Board>>;
    async fn list_boards(&self) -> Result<Vec<Board>>;

    async fn post_by_id(&self, id: &str) -> Result<Option<Post>>;
    async fn post_by_slug(&self, board_id: &str, slug: &str) -> Result<Option<Post>>;
    async fn list_published(&self, q: &PublishedQuery) -> Result<Vec<Post>>;
    async fn count_published(&self, q: &PublishedQuery) -> Result<i64>;
    async fn list_posts(&self, q: &PostFilter) -> Result<Vec<Post>>;
    async fn count_posts(&self, q: &PostFilter) -> Result<i64>;
    async fn revisions_of_post(&self, post_id: &str) -> Result<Vec<Revision>>;

    async fn page_by_slug(&self, slug: &str) -> Result<Option<Page>>;
    async fn list_pages(&self, status: Option<PageStatus>) -> Result<Vec<Page>>;
    async fn menu_rows(&self, location: &str) -> Result<Vec<MenuRow>>;
    async fn settings(&self) -> Result<Vec<(String, Value)>>;

    async fn member_by_id(&self, id: &str) -> Result<Option<Member>>;
    async fn member_by_email(&self, email: &str) -> Result<Option<Member>>;
    async fn count_members(&self) -> Result<i64>;
    async fn first_member(&self) -> Result<Option<Member>>;
    async fn member_roles(&self, member_id: &str) -> Result<Vec<Role>>;
    async fn session_by_hash(&self, hash: &str) -> Result<Option<Session>>;
    async fn ticket_by_hash(&self, hash: &str) -> Result<Option<LoginTicket>>;
    /// 만료된 세션·로그인 링크를 지운다. 시스템 정리라 감사 대상이 아니다 (PHASE1_PLAN §5).
    async fn purge_expired(&self, now: &str) -> Result<u64>;
}

#[async_trait]
pub trait WriteTx: Send {
    async fn insert_audit(&mut self, rec: &AuditRecord) -> Result<()>;
    async fn commit(self: Box<Self>) -> Result<()>;

    async fn board_by_id(&mut self, id: &str) -> Result<Option<Board>>;
    async fn board_by_slug(&mut self, slug: &str) -> Result<Option<Board>>;
    async fn insert_board(&mut self, board: &Board) -> Result<()>;
    async fn update_board(&mut self, board: &Board) -> Result<()>;
    async fn delete_board(&mut self, id: &str) -> Result<()>;
    /// 상태와 무관하게 게시판에 남은 글 수 (`deleted` 포함).
    async fn count_board_posts(&mut self, board_id: &str) -> Result<i64>;

    async fn post_by_id(&mut self, id: &str) -> Result<Option<Post>>;
    async fn slug_taken(
        &mut self,
        board_id: &str,
        slug: &str,
        except: Option<&str>,
    ) -> Result<bool>;
    async fn insert_post(&mut self, post: &Post) -> Result<()>;
    async fn update_post(&mut self, post: &Post) -> Result<()>;
    async fn insert_revision(&mut self, rev: &Revision) -> Result<()>;

    async fn insert_token(&mut self, token: &Token) -> Result<()>;
    async fn token_by_id(&mut self, id: &str) -> Result<Option<Token>>;
    async fn revoke_token(&mut self, id: &str, at: &str) -> Result<()>;

    async fn page_by_id(&mut self, id: &str) -> Result<Option<Page>>;
    async fn page_by_slug(&mut self, slug: &str) -> Result<Option<Page>>;
    async fn insert_page(&mut self, page: &Page) -> Result<()>;
    async fn update_page(&mut self, page: &Page) -> Result<()>;

    async fn menu_rows(&mut self, location: &str) -> Result<Vec<MenuRow>>;
    /// 위치의 메뉴를 통째로 바꾼다.
    async fn replace_menu(&mut self, location: &str, rows: &[MenuRow]) -> Result<()>;

    async fn settings(&mut self) -> Result<Vec<(String, Value)>>;
    async fn put_setting(&mut self, key: &str, value: &Value, at: &str) -> Result<()>;

    async fn member_by_id(&mut self, id: &str) -> Result<Option<Member>>;
    async fn member_by_email(&mut self, email: &str) -> Result<Option<Member>>;
    async fn insert_member(&mut self, m: &Member) -> Result<()>;
    async fn update_member(&mut self, m: &Member) -> Result<()>;
    async fn add_member_role(&mut self, member_id: &str, role: &str) -> Result<()>;
    async fn insert_ticket(&mut self, t: &LoginTicket) -> Result<()>;
    /// 회원의 아직 쓰지 않은 링크를 모두 지운다 (새 링크 발급 시).
    async fn delete_open_tickets(&mut self, member_id: &str) -> Result<u64>;
    /// 유효한 링크를 원자적으로 사용 처리한다. 동시에 두 번 불러도 한 번만 `Some`.
    async fn use_ticket(&mut self, hash: &str, now: &str) -> Result<Option<LoginTicket>>;
    async fn insert_session(&mut self, s: &Session) -> Result<()>;
    async fn delete_session(&mut self, id: &str) -> Result<u64>;
    async fn delete_member_sessions(&mut self, member_id: &str) -> Result<u64>;
}

/// 쓰기 트랜잭션. 드롭하면 롤백된다.
pub struct Tx {
    inner: Box<dyn WriteTx>,
    audited: usize,
}

impl Tx {
    pub async fn begin(ctx: &Ctx) -> Result<Self> {
        Ok(Self {
            inner: ctx.store.begin_write().await?,
            audited: 0,
        })
    }

    /// 비밀 값을 지운 뒤 같은 트랜잭션에 감사 기록을 남긴다.
    pub async fn audit(
        &mut self,
        actor: &Actor,
        action: &'static str,
        target: Target,
        diff: Value,
    ) -> Result<()> {
        let rec = AuditRecord::new(actor, action, target, diff);
        self.inner.insert_audit(&rec).await?;
        self.audited += 1;
        Ok(())
    }

    pub async fn commit(self) -> Result<()> {
        if self.audited == 0 {
            return Err(Error::internal(
                "감사 기록 없는 쓰기 트랜잭션은 커밋할 수 없다",
            ));
        }
        self.inner.commit().await
    }
}

impl Deref for Tx {
    type Target = dyn WriteTx;

    fn deref(&self) -> &Self::Target {
        &*self.inner
    }
}

impl DerefMut for Tx {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut *self.inner
    }
}

/// 메뉴 한 줄. 트리 조립은 core(`services::menus`)가 한다.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MenuRow {
    pub id: String,
    pub parent_id: Option<String>,
    pub label: String,
    pub link: MenuLink,
    pub sort_order: i64,
    pub created_at: String,
}
