//! 저장소 경계 (PHASE1_PLAN §4.1·§4.2). 구현은 `maru-storage`, 조립은 `cli`.
//!
//! 쓰기는 반드시 [`Tx`] 를 거친다. `Tx::commit` 은 감사 기록이 한 건도 없으면 거부한다.

use std::ops::{Deref, DerefMut};
use std::sync::Arc;

use async_trait::async_trait;
use serde_json::Value;

use crate::actor::Actor;
use crate::audit::{AuditRecord, Target};
use crate::blobs::Blobs;
use crate::error::{Error, Result};
use crate::model::{
    Attachment, Board, Category, Font, FontFile, LoginTicket, Media, Member, MenuLink, Page,
    PageStatus, Post, PostStatus, PostTemplate, Revision, Role, Session, Tag, Token,
};

/// 서비스 함수의 첫 인자.
#[derive(Clone)]
pub struct Ctx {
    pub store: Arc<dyn Store>,
    /// 미디어 파일 보관소.
    pub blobs: Arc<dyn Blobs>,
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
    pub category_id: Option<String>,
    /// `None` 전체, `Some(true)` 공지만, `Some(false)` 공지 빼고.
    pub notices: Option<bool>,
    pub tag_id: Option<String>,
    /// 비밀글을 뺀다 (피드·사이트맵·태그 목록).
    pub exclude_secret: bool,
}

impl PublishedQuery {
    /// 조건 없이 읽을 수 있는 모든 공개 글.
    pub fn new(max_read_rank: i64, limit: i64, offset: i64) -> Self {
        Self {
            board_id: None,
            max_read_rank,
            limit,
            offset,
            category_id: None,
            notices: None,
            tag_id: None,
            exclude_secret: false,
        }
    }
}

/// 공개 검색 조건 (GAP C5). 공개 범위 SQL 은 저장소 구현 한 곳에만 있다.
#[derive(Debug, Clone)]
pub struct SearchQuery {
    /// FTS5 질의 (`search::Query::to_fts`). 사용자 입력을 그대로 넣지 않는다.
    pub fts: String,
    /// 이 게시판의 글만. 있으면 페이지는 빠진다.
    pub board_id: Option<String>,
    /// 이 서열 이하의 `read` 정책을 가진 게시판만 포함한다.
    pub max_read_rank: i64,
    /// 공개 페이지도 넣는다.
    pub pages: bool,
    /// 참이면 최신순, 거짓이면 정확도순(같으면 최신).
    pub newest: bool,
    pub limit: i64,
    pub offset: i64,
}

/// 검색 결과 한 건 (어느 글·페이지인지만). 내용은 서비스가 다시 읽는다.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SearchRef {
    Post(String),
    Page(String),
}

/// 관리용 글 목록 조건.
#[derive(Debug, Clone)]
pub struct PostFilter {
    pub board_id: Option<String>,
    pub status: Option<PostStatus>,
    /// FTS5 질의 (`search::Query::to_fts`). 사용자 입력을 그대로 넣지 않는다.
    pub search: Option<String>,
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
    async fn page_by_id(&self, id: &str) -> Result<Option<Page>>;
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

    async fn media_by_id(&self, id: &str) -> Result<Option<Media>>;
    /// 최근 것부터.
    async fn list_media(&self, limit: i64, offset: i64) -> Result<Vec<Media>>;
    async fn count_media(&self) -> Result<i64>;
    /// 예약 시각이 지났고 아직 공개되지 않은 글 ID (오래된 예약부터).
    async fn due_posts(&self, now: &str, limit: i64) -> Result<Vec<String>>;

    async fn attachment_by_id(&self, id: &str) -> Result<Option<Attachment>>;
    /// 최근 것부터.
    async fn list_attachments(&self, limit: i64, offset: i64) -> Result<Vec<Attachment>>;
    async fn count_attachments(&self) -> Result<i64>;

    /// 사용자가 저장한 글 틀 (이름순).
    async fn list_templates(&self) -> Result<Vec<PostTemplate>>;

    /// 게시판의 분류 (정렬 순서, 이름순).
    async fn categories_of_board(&self, board_id: &str) -> Result<Vec<Category>>;
    async fn category_by_id(&self, id: &str) -> Result<Option<Category>>;
    async fn tag_by_slug(&self, slug: &str) -> Result<Option<Tag>>;
    /// 공개 글이 하나 이상 달린 태그와 그 글 수 (`PublishedQuery` 와 같은 공개 기준). 이름순.
    async fn list_tags(&self, max_read_rank: i64) -> Result<Vec<Tag>>;

    /// 올린 서체 (이름순, 파일은 굵기·기울임 순).
    async fn list_fonts(&self) -> Result<Vec<Font>>;
    async fn font_by_slug(&self, slug: &str) -> Result<Option<Font>>;
    /// 본문에 `]{slug}` 꾸밈이 남은 글(지운 글 제외)·페이지 수.
    async fn count_font_uses(&self, slug: &str) -> Result<i64>;

    /// 모아 둔 조회수를 한 트랜잭션으로 더한다. 통계라 감사 대상이 아니고 `updated_at` 도 그대로다 (ADR-0049).
    /// 그 사이 지워진 글은 건너뛴다.
    async fn add_views(&self, counts: &[(String, i64)]) -> Result<()>;
    /// 같은 게시판의 바로 앞(더 오래된)·뒤(더 새) 공개 글. 순서는 `published_at`, 같으면 `id`.
    async fn neighbor_posts(&self, post: &Post) -> Result<(Option<Post>, Option<Post>)>;
    /// 관련 글: 겹치는 태그 수 → 같은 분류 → 같은 게시판 → 최신 순. 비밀글·공지·자기 자신은 뺀다.
    /// 태그가 겹치지 않는 다른 게시판의 글은 넣지 않는다.
    async fn related_posts(&self, post: &Post, max_read_rank: i64, limit: i64)
    -> Result<Vec<Post>>;

    /// 공개 검색: 공개된 글(비밀글 제외, 읽을 수 있는 게시판)·공개 페이지. 맞은 수도 돌려준다.
    async fn search(&self, q: &SearchQuery) -> Result<(Vec<SearchRef>, i64)>;
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
    /// 글과 함께 `post.tags` 도 쓴다. 태그 이름은 core 가 정규화해 둔 것이어야 한다.
    async fn insert_post(&mut self, post: &Post) -> Result<()>;
    /// 글과 함께 `post.tags` 를 통째로 바꾼다.
    async fn update_post(&mut self, post: &Post) -> Result<()>;
    /// 조회수를 정한 값으로 바꾼다 (운영자 고치기). `updated_at` 은 그대로다.
    async fn set_view_count(&mut self, id: &str, n: i64) -> Result<()>;
    async fn tag_by_slug(&mut self, slug: &str) -> Result<Option<Tag>>;

    async fn categories_of_board(&mut self, board_id: &str) -> Result<Vec<Category>>;
    async fn category_by_id(&mut self, id: &str) -> Result<Option<Category>>;
    async fn category_by_slug(&mut self, board_id: &str, slug: &str) -> Result<Option<Category>>;
    async fn insert_category(&mut self, c: &Category) -> Result<()>;
    async fn update_category(&mut self, c: &Category) -> Result<()>;
    /// 분류를 지우고 그 분류의 글은 분류를 비운다. 비운 글 수를 돌려준다.
    async fn delete_category(&mut self, id: &str) -> Result<u64>;
    async fn insert_revision(&mut self, rev: &Revision) -> Result<()>;

    async fn insert_token(&mut self, token: &Token) -> Result<()>;
    async fn token_by_id(&mut self, id: &str) -> Result<Option<Token>>;
    async fn revoke_token(&mut self, id: &str, at: &str) -> Result<()>;

    async fn page_by_id(&mut self, id: &str) -> Result<Option<Page>>;
    async fn page_by_slug(&mut self, slug: &str) -> Result<Option<Page>>;
    async fn insert_page(&mut self, page: &Page) -> Result<()>;
    async fn update_page(&mut self, page: &Page) -> Result<()>;
    /// 검색 색인을 처음부터 다시 만든다 (글 수, 페이지 수).
    /// `insert_post`·`update_post`·페이지 쓰기는 같은 트랜잭션에서 색인을 스스로 맞춘다.
    async fn rebuild_search(&mut self) -> Result<(i64, i64)>;

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

    async fn media_by_id(&mut self, id: &str) -> Result<Option<Media>>;
    async fn insert_media(&mut self, m: &Media) -> Result<()>;
    async fn update_media(&mut self, m: &Media) -> Result<()>;
    async fn delete_media(&mut self, id: &str) -> Result<()>;

    async fn attachment_by_id(&mut self, id: &str) -> Result<Option<Attachment>>;
    async fn insert_attachment(&mut self, a: &Attachment) -> Result<()>;
    async fn delete_attachment(&mut self, id: &str) -> Result<()>;

    async fn template_by_id(&mut self, id: &str) -> Result<Option<PostTemplate>>;
    async fn template_by_name(&mut self, name: &str) -> Result<Option<PostTemplate>>;
    async fn insert_template(&mut self, t: &PostTemplate) -> Result<()>;
    async fn update_template(&mut self, t: &PostTemplate) -> Result<()>;
    async fn delete_template(&mut self, id: &str) -> Result<()>;

    async fn font_by_slug(&mut self, slug: &str) -> Result<Option<Font>>;
    /// 서체만 쓴다 (`files` 는 무시).
    async fn insert_font(&mut self, f: &Font) -> Result<()>;
    /// 이름·대체 계열·라이선스·전문 경로·`updated_at`.
    async fn update_font(&mut self, f: &Font) -> Result<()>;
    /// 서체와 그 파일 행을 지운다.
    async fn delete_font(&mut self, id: &str) -> Result<()>;
    async fn insert_font_file(&mut self, font_id: &str, f: &FontFile) -> Result<()>;
    async fn delete_font_file(&mut self, id: &str) -> Result<()>;
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
