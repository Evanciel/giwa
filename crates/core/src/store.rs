//! 저장소 경계 (PHASE1_PLAN §4.1·§4.2). 구현은 `maru-storage`, 조립은 `cli`.
//!
//! 쓰기는 반드시 [`Tx`] 를 거친다. `Tx::commit` 은 감사 기록이 한 건도 없으면 거부한다.

use std::ops::{Deref, DerefMut};
use std::sync::Arc;

use async_trait::async_trait;
use serde_json::Value;

use crate::model::{AuditEvent, Changeset, ChangesetStatus, ModuleFailure, ModuleRow};
use crate::services::widgets::WidgetType;

use crate::actor::Actor;
use crate::audit::{AuditRecord, Target};
use crate::blobs::Blobs;
use crate::error::{Error, Result};
use crate::model::{
    Attachment, AuditEntry, Board, Category, Comment, CommentStatus, Font, FontFile, Listing,
    LoginTicket, MailOut, MailStatus, Media, Member, MenuLink, Page, PageReq, PageStatus, Post,
    PostStatus, PostTemplate, Revision, Role, Session, StatBatch, StatDay, StatPage, StatReferrer,
    Tag, TicketPurpose, Token, TotpChallenge, Widget,
};
use crate::model::{LinkPreview, LinkStatus, MemberActivity, MemberTotp, Signal};

/// 댓글 관리 목록 조건. 비어 있는 조건은 거르지 않는다.
#[derive(Debug, Clone, Copy, Default)]
pub struct CommentQuery<'a> {
    pub status: Option<CommentStatus>,
    pub post_id: Option<&'a str>,
    /// 이 게시판들의 글에 달린 댓글만. `None` 이면 전부
    pub board_ids: Option<&'a [String]>,
}

/// 위키 게시판의 공개 문서 한 장 (GAP C13).
#[derive(Debug, Clone)]
pub struct WikiPage {
    pub id: String,
    pub title: String,
    pub slug: String,
    pub body_md: String,
    pub updated_at: String,
}

/// 서비스 함수의 첫 인자.
#[derive(Clone)]
pub struct Ctx {
    pub store: Arc<dyn Store>,
    /// 미디어 파일 보관소.
    pub blobs: Arc<dyn Blobs>,
    /// 절대 URL 의 기준 (`config.toml`). 끝에 `/` 가 없다.
    pub site_url: String,
    /// 코어 밖(모듈)이 더하는 것 (ADR-0066). 모듈이 없으면 [`no_extensions`].
    pub extensions: Arc<dyn Extensions>,
}

/// 코어가 모듈에게 묻는 일. 코어는 모듈을 모르고 이 트레이트만 안다 (ADR-0066).
#[async_trait]
pub trait Extensions: Send + Sync {
    /// 켜진 모듈의 위젯 종류.
    async fn widget_types(&self, ctx: &Ctx) -> Vec<WidgetType>;
    /// 모듈 위젯의 설정을 검사한다. 그런 종류가 없으면 `None`.
    async fn check_widget(&self, ctx: &Ctx, kind: &str, config: Value) -> Option<Result<Value>>;
    /// 켜진 모듈의 보존 정리 (ADR-0073). 지운 수의 합. 한 모듈이 실패해도 나머지는 한다.
    async fn purge(&self, ctx: &Ctx) -> u64;
    /// 켜진 모듈의 운영 신호 (ADR-0074). 한 모듈이 실패해도 나머지는 한다.
    async fn signals(&self, ctx: &Ctx, actor: &Actor) -> Vec<Signal>;
    /// 모듈을 켜거나 끈다. 바꾸기 전에 켜져 있었는지 돌려준다 (ADR-0121).
    async fn set_module(&self, ctx: &Ctx, actor: &Actor, id: &str, on: bool) -> Result<bool>;
}

struct NoExtensions;

#[async_trait]
impl Extensions for NoExtensions {
    async fn widget_types(&self, _ctx: &Ctx) -> Vec<WidgetType> {
        Vec::new()
    }

    async fn check_widget(&self, _ctx: &Ctx, _kind: &str, _config: Value) -> Option<Result<Value>> {
        None
    }

    async fn purge(&self, _ctx: &Ctx) -> u64 {
        0
    }

    async fn signals(&self, _ctx: &Ctx, _actor: &Actor) -> Vec<Signal> {
        Vec::new()
    }

    async fn set_module(&self, _ctx: &Ctx, _actor: &Actor, id: &str, _on: bool) -> Result<bool> {
        Err(crate::error::Error::new(crate::error::Code::NotFound).arg("id", id))
    }
}

/// 모듈 없이 쓰는 컨텍스트의 `extensions`.
pub fn no_extensions() -> Arc<dyn Extensions> {
    Arc::new(NoExtensions)
}

/// 모듈 마이그레이션 하나 (ADR-0066). 내용은 바이너리에 든다.
#[derive(Debug, Clone, Copy)]
pub struct ModuleMigration {
    pub version: i64,
    pub name: &'static str,
    pub up: &'static str,
    pub down: &'static str,
}

/// 감사 로그 조건 (ADR-0062). 빈 값은 따지지 않는다. `action` 은 그 이름 자신과 `action.…`.
/// 시각은 저장 형식(UTC)이다.
#[derive(Debug, Clone, Default)]
pub struct AuditFilter {
    pub actor_type: String,
    pub actor_id: String,
    pub action: String,
    pub target_type: String,
    pub target_id: String,
    pub since: String,
    pub until: String,
}

/// 인기글 조건 (ADR-0059). 비밀글은 언제나 뺀다.
#[derive(Debug, Clone)]
pub struct PopularQuery {
    pub board_id: Option<String>,
    /// 이 서열 이하의 `read` 정책을 가진 게시판만 포함한다.
    pub max_read_rank: i64,
    /// 이 시각 뒤에 발행된 글만. `None` 이면 전체 기간
    pub since: Option<String>,
    pub limit: i64,
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
    /// 마지막 활동(댓글·답변)이 늦은 스레드부터 (ADR-0058).
    pub by_activity: bool,
    /// 여분 필드 거르기 (ADR-0089): `fields::MetaFilter` 배열의 JSON. 없으면 거르지 않는다.
    pub meta_filters: Option<String>,
    /// 목록 순서 (ADR-0092): `views`·`comments`. 없으면 최신(스레드) 순.
    pub sort: Option<String>,
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
            by_activity: false,
            meta_filters: None,
            sort: None,
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

/// 글자 그대로 찾는 곳 (GAP Q7, ADR-0092).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LiteralTarget {
    /// 공개 댓글 내용
    Comment,
    /// 글쓴이: 활동 중인 회원의 닉네임 또는 손님 이름
    Author,
}

/// 댓글·글쓴이 검색 조건. 공개 기준은 [`SearchQuery`] 와 같다(글만, 페이지 없음, 최신순).
#[derive(Debug, Clone)]
pub struct LiteralSearchQuery {
    pub target: LiteralTarget,
    /// `LIKE` 패턴. `%`·`_`·`\` 는 `\` 로 이미 막았다 (`ESCAPE '\'`).
    pub pattern: String,
    pub board_id: Option<String>,
    pub max_read_rank: i64,
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

    // ---- 모듈 (ADR-0066) ----
    async fn module_rows(&self) -> Result<Vec<ModuleRow>>;
    async fn module_row(&self, id: &str) -> Result<Option<ModuleRow>>;
    /// 아직 안 적용한 마이그레이션을 차례로 적용하고 그 버전을 돌려준다.
    async fn module_apply(&self, id: &str, migrations: &[ModuleMigration]) -> Result<Vec<i64>>;
    /// 적용한 마이그레이션을 거꾸로 되돌리고 그 버전을 돌려준다.
    async fn module_revert(&self, id: &str, migrations: &[ModuleMigration]) -> Result<Vec<i64>>;
    async fn module_cursor_set(&self, id: &str, seq: i64) -> Result<()>;
    /// 감사 기록의 마지막 순번. 없으면 0.
    async fn audit_last_seq(&self) -> Result<i64>;
    /// `after` 초과 `upto` 이하에서 이 동작들만 적힌 순서대로.
    async fn audit_after(
        &self,
        after: i64,
        upto: i64,
        actions: &[&str],
        limit: i64,
    ) -> Result<Vec<AuditEvent>>;
    async fn audit_event(&self, id: &str) -> Result<Option<AuditEvent>>;
    async fn module_failure_put(&self, failure: &ModuleFailure) -> Result<()>;
    async fn module_failure_delete(&self, module: &str, event_id: &str) -> Result<()>;
    /// 빈 모듈이면 전체. 최근 것부터.
    async fn module_failures(&self, module: Option<&str>) -> Result<Vec<ModuleFailure>>;
    /// 지금 다시 시도할 것.
    async fn module_failures_due(&self, module: &str, now: &str) -> Result<Vec<ModuleFailure>>;

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
    /// 모든 위젯. 영역 → 순서 → 만든 순.
    async fn list_widgets(&self) -> Result<Vec<Widget>>;
    async fn widget_by_id(&self, id: &str) -> Result<Option<Widget>>;
    /// 조회수 → 공개 댓글 수 → 최신 순.
    async fn list_popular(&self, q: &PopularQuery) -> Result<Vec<Post>>;
    async fn settings(&self) -> Result<Vec<(String, Value)>>;

    async fn member_by_id(&self, id: &str) -> Result<Option<Member>>;
    async fn member_by_email(&self, email: &str) -> Result<Option<Member>>;
    /// 닉네임으로 (대소문자 무시)
    async fn member_by_name(&self, name: &str) -> Result<Option<Member>>;
    /// 가입 순 (오래된 것부터)
    async fn list_members(&self, limit: i64, offset: i64) -> Result<Vec<Member>>;
    async fn count_role_members(&self, role: &str) -> Result<i64>;
    async fn count_members(&self) -> Result<i64>;
    async fn first_member(&self) -> Result<Option<Member>>;
    async fn member_roles(&self, member_id: &str) -> Result<Vec<Role>>;
    async fn session_by_hash(&self, hash: &str) -> Result<Option<Session>>;
    async fn ticket_by_hash(&self, hash: &str) -> Result<Option<LoginTicket>>;
    /// 회원의 TOTP (ADR-0072)
    async fn member_totp(&self, member_id: &str) -> Result<Option<MemberTotp>>;
    /// 쓰지 않은 복구 코드 수
    async fn recovery_codes_left(&self, member_id: &str) -> Result<i64>;
    async fn totp_challenge_by_hash(&self, hash: &str) -> Result<Option<TotpChallenge>>;
    /// 틀린 횟수를 하나 올리고 새 횟수를 돌려준다. 감사 대상이 아닌 시스템 기록.
    async fn bump_totp_challenge(&self, id: &str) -> Result<i64>;
    /// 횟수를 다 쓴 확인 단계를 버린다. 시스템 기록.
    async fn delete_totp_challenge(&self, id: &str) -> Result<()>;
    /// 마지막 로그인과 휴면 (ADR-0073)
    async fn member_activity(&self, member_id: &str) -> Result<MemberActivity>;
    /// 휴면 미리 알림을 보낼 회원: 마지막 접속(없으면 가입)이 `seen_before` 보다 앞서고
    /// 아직 알리지 않은, 쓰는 중인 일반 회원(직원 역할 없음). 휴면 회원은 빠진다.
    async fn members_to_notify(&self, seen_before: &str, limit: i64) -> Result<Vec<Member>>;
    /// 휴면으로 바꿀 회원: 위 조건에 더해 `noticed_before` 보다 먼저 알린 회원.
    async fn members_to_sleep(
        &self,
        seen_before: &str,
        noticed_before: &str,
        limit: i64,
    ) -> Result<Vec<Member>>;
    /// 새것부터. `status` 가 없으면 전부
    async fn mails(&self, status: Option<MailStatus>, limit: i64) -> Result<Vec<MailOut>>;
    /// 보냈거나 실패한 편지 중 `before` 보다 먼저 쌓인 것을 지운다. 시스템 정리라 감사 대상이 아니다.
    async fn purge_mails(&self, before: &str) -> Result<u64>;
    /// `since` 뒤로 댓글이 `min` 개 이상 달린 글과 그 수, 많은 순 (ADR-0074)
    async fn comment_bursts(&self, since: &str, min: i64, limit: i64)
    -> Result<Vec<(String, i64)>>;
    /// 이 순위 이상의 역할을 가진, 쓰는 중인 회원
    async fn staff_members(&self, min_rank: i64) -> Result<Vec<Member>>;
    /// `before` 보다 먼저 알린 신호를 잊는다. 시스템 정리.
    async fn purge_signal_sent(&self, before: &str) -> Result<u64>;
    async fn mail_by_id(&self, id: &str) -> Result<Option<MailOut>>;
    /// 배달 결과 (상태·시도 수·오류·보낸 때). 감사 대상이 아닌 시스템 기록.
    async fn update_mail(&self, m: &MailOut) -> Result<()>;
    /// 처음 보는 링크 카드 주소를 가져올 차례로 적는다 (ADR-0084). 감사 대상이 아닌 시스템 기록.
    async fn note_links(&self, urls: &[String], now: &str) -> Result<()>;
    async fn changeset(&self, id: &str) -> Result<Option<Changeset>>;
    /// 새것부터. `status` 가 없으면 전부
    async fn changesets(
        &self,
        status: Option<ChangesetStatus>,
        limit: i64,
    ) -> Result<Vec<Changeset>>;
    /// 진행 상황. 시작과 끝은 감사 기록과 함께 `WriteTx::save_changeset` 으로 남긴다.
    async fn save_changeset(&self, cs: &Changeset) -> Result<()>;
    /// `since` 뒤로 이 주체가 남긴 감사 기록에 changeset 번호를 붙인다.
    async fn stamp_audit(
        &self,
        id: &str,
        since: &str,
        actor_type: &str,
        actor_id: Option<&str>,
    ) -> Result<()>;
    async fn link_previews(&self, urls: &[String]) -> Result<Vec<LinkPreview>>;
    /// 가져올 차례인 주소, 오래 기다린 것부터
    async fn pending_links(&self, limit: i64) -> Result<Vec<LinkPreview>>;
    /// 가져온 결과. 시스템 기록.
    async fn update_link(&self, p: &LinkPreview) -> Result<()>;
    /// 새것부터. `status` 가 없으면 전부
    async fn links(&self, status: Option<LinkStatus>, limit: i64) -> Result<Vec<LinkPreview>>;

    async fn comment_by_id(&self, id: &str) -> Result<Option<Comment>>;
    /// 회원이 마지막으로 글이나 댓글을 쓴 때 (도배 방지, ADR-0071)
    async fn last_member_write(&self, member_id: &str) -> Result<Option<String>>;
    /// 회원의 마지막 댓글 (본문, 쓴 때)
    async fn last_member_comment(&self, member_id: &str) -> Result<Option<(String, String)>>;
    /// 글의 댓글, 쓴 순서. `statuses` 에 든 상태만.
    async fn comments_of_post(
        &self,
        post_id: &str,
        statuses: &[CommentStatus],
    ) -> Result<Vec<Comment>>;
    /// 관리 목록, 새것부터.
    async fn list_comments(
        &self,
        q: &CommentQuery<'_>,
        limit: i64,
        offset: i64,
    ) -> Result<Vec<Comment>>;
    async fn count_comments(&self, q: &CommentQuery<'_>) -> Result<i64>;
    /// 게시판의 공개된 맨 위 글(비밀글 제외), 먼저 올린 것부터 `limit` 개 (위키, GAP C13).
    async fn wiki_pages(&self, board_id: &str, limit: i64) -> Result<Vec<WikiPage>>;
    /// 회원이 쓴 댓글, 새것부터. 지운 댓글은 빠진다 (GAP M3).
    async fn member_comments(
        &self,
        member_id: &str,
        limit: i64,
        offset: i64,
    ) -> Result<Vec<Comment>>;
    async fn count_member_comments(&self, member_id: &str) -> Result<i64>;
    /// 공개 프로필 (GAP Q2): 자기소개. 없으면 `None`
    async fn member_bio(&self, member_id: &str) -> Result<Option<String>>;
    /// 회원 서명 (GAP Q17). 없으면 `None`
    async fn member_signature(&self, member_id: &str) -> Result<Option<String>>;
    /// 공개 프로필: 회원의 공개 글 ID(새것부터)와 전체 수. 기준은 게시판 목록과 같다
    async fn public_posts_of(
        &self,
        member_id: &str,
        max_read_rank: i64,
        limit: i64,
    ) -> Result<(Vec<String>, i64)>;
    /// 공개 프로필: 공개 글의 공개 댓글(새것부터)과 전체 수
    async fn public_comments_of(
        &self,
        member_id: &str,
        max_read_rank: i64,
        limit: i64,
    ) -> Result<(Vec<Comment>, i64)>;
    /// 글마다 공개 댓글 수. 댓글이 없는 글은 빠진다.
    async fn comment_counts(&self, post_ids: &[String]) -> Result<Vec<(String, i64)>>;
    /// 캡차 도전을 둔다 (시스템 기록, 감사 대상 아님). 만료된 것은 함께 지운다.
    async fn insert_challenge(
        &self,
        id_hash: &str,
        answer: &str,
        now: &str,
        expires_at: &str,
    ) -> Result<()>;
    /// 유효한 도전을 꺼내며 지우고 `(답, 낸 때)` 를 돌려준다. 한 번만 `Some`.
    async fn take_challenge(&self, id_hash: &str, now: &str) -> Result<Option<(String, String)>>;
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
    /// 모아 둔 방문 통계를 날짜별 표에 한 트랜잭션으로 더한다 (ADR-0061).
    async fn add_stats(&self, batch: &StatBatch) -> Result<()>;
    /// 감사 기록, 최신 순.
    async fn list_audit(&self, filter: &AuditFilter, page: PageReq) -> Result<Listing<AuditEntry>>;
    /// `before_day` 보다 앞선 날의 통계를 지운다.
    async fn purge_stats(&self, before_day: &str) -> Result<()>;
    /// `from`~`to`(포함) 중 기록이 있는 날, 날짜 순.
    async fn stat_days(&self, from: &str, to: &str) -> Result<Vec<StatDay>>;
    /// 기간의 화면별 합, 조회 → 방문자 → 경로 순.
    async fn stat_pages(&self, from: &str, to: &str, limit: u32) -> Result<Vec<StatPage>>;
    /// 기간의 들어온 곳별 합, 많은 순.
    async fn stat_referrers(&self, from: &str, to: &str, limit: u32) -> Result<Vec<StatReferrer>>;
    /// 같은 게시판의 바로 앞(더 오래된)·뒤(더 새) 공개 글. 순서는 `published_at`, 같으면 `id`.
    async fn neighbor_posts(&self, post: &Post) -> Result<(Option<Post>, Option<Post>)>;
    /// 관련 글: 겹치는 태그 수 → 같은 분류 → 같은 게시판 → 최신 순. 비밀글·공지·자기 자신은 뺀다.
    /// 태그가 겹치지 않는 다른 게시판의 글은 넣지 않는다.
    async fn related_posts(&self, post: &Post, max_read_rank: i64, limit: i64)
    -> Result<Vec<Post>>;

    /// 공개 검색: 공개된 글(비밀글 제외, 읽을 수 있는 게시판)·공개 페이지. 맞은 수도 돌려준다.
    async fn search(&self, q: &SearchQuery) -> Result<(Vec<SearchRef>, i64)>;

    /// 댓글·글쓴이 검색 (ADR-0092). (글 ID, 맞은 댓글 내용) 과 맞은 글 수.
    async fn search_literal(
        &self,
        q: &LiteralSearchQuery,
    ) -> Result<(Vec<(String, Option<String>)>, i64)>;
}

#[async_trait]
pub trait WriteTx: Send {
    async fn save_changeset(&mut self, cs: &Changeset) -> Result<()>;
    async fn insert_audit(&mut self, rec: &AuditRecord) -> Result<()>;
    async fn commit(self: Box<Self>) -> Result<()>;

    async fn upsert_module(&mut self, row: &ModuleRow) -> Result<()>;
    /// 상태 줄과 실패 기록을 지운다.
    async fn delete_module(&mut self, id: &str) -> Result<()>;

    async fn board_by_id(&mut self, id: &str) -> Result<Option<Board>>;
    async fn board_by_slug(&mut self, slug: &str) -> Result<Option<Board>>;
    async fn insert_board(&mut self, board: &Board) -> Result<()>;
    async fn update_board(&mut self, board: &Board) -> Result<()>;
    async fn delete_board(&mut self, id: &str) -> Result<()>;
    /// 상태와 무관하게 게시판에 남은 글 수 (`deleted` 포함).
    async fn count_board_posts(&mut self, board_id: &str) -> Result<i64>;

    async fn post_by_id(&mut self, id: &str) -> Result<Option<Post>>;
    /// 마지막 활동 시각을 올린다. 더 늦은 값이 이미 있으면 그대로 둔다 (ADR-0058).
    async fn touch_post(&mut self, id: &str, at: &str) -> Result<()>;
    /// 이 글에 바로 달린 답변글 수 (상태 무관, ADR-0056).
    async fn reply_count(&mut self, parent_id: &str) -> Result<i64>;
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

    async fn widget_by_id(&mut self, id: &str) -> Result<Option<Widget>>;
    async fn insert_widget(&mut self, w: &Widget) -> Result<()>;
    async fn update_widget(&mut self, w: &Widget) -> Result<()>;
    async fn delete_widget(&mut self, id: &str) -> Result<()>;

    async fn settings(&mut self) -> Result<Vec<(String, Value)>>;
    async fn put_setting(&mut self, key: &str, value: &Value, at: &str) -> Result<()>;

    async fn member_by_id(&mut self, id: &str) -> Result<Option<Member>>;
    async fn member_by_email(&mut self, email: &str) -> Result<Option<Member>>;
    async fn insert_member(&mut self, m: &Member) -> Result<()>;
    async fn update_member(&mut self, m: &Member) -> Result<()>;
    /// 자기소개 저장. 빈 값이면 지운다 (GAP Q2)
    async fn set_member_bio(&mut self, member_id: &str, bio: &str, now: &str) -> Result<()>;
    /// 서명 저장. 빈 값이면 지운다 (GAP Q17)
    async fn set_member_signature(
        &mut self,
        member_id: &str,
        signature: &str,
        now: &str,
    ) -> Result<()>;
    async fn add_member_role(&mut self, member_id: &str, role: &str) -> Result<()>;
    async fn clear_member_roles(&mut self, member_id: &str) -> Result<()>;
    async fn member_by_name(&mut self, name: &str) -> Result<Option<Member>>;
    /// 쌓였으면 `true`. 같은 `idem_key` 가 이미 있으면 `false`
    async fn queue_mail(&mut self, m: &MailOut) -> Result<bool>;
    async fn comment_by_id(&mut self, id: &str) -> Result<Option<Comment>>;
    async fn insert_comment(&mut self, c: &Comment) -> Result<()>;
    /// 상태·본문·고친 때.
    async fn update_comment(&mut self, c: &Comment) -> Result<()>;
    /// 지우지 않은 대댓글 수.
    async fn count_live_replies(&mut self, parent_id: &str) -> Result<i64>;
    async fn insert_ticket(&mut self, t: &LoginTicket) -> Result<()>;
    /// 회원의 아직 쓰지 않은 링크를 모두 지운다 (새 링크 발급 시).
    async fn delete_open_tickets(&mut self, member_id: &str, purpose: TicketPurpose)
    -> Result<u64>;
    /// 유효한 링크를 원자적으로 사용 처리한다. 동시에 두 번 불러도 한 번만 `Some`.
    async fn use_ticket(
        &mut self,
        hash: &str,
        purpose: TicketPurpose,
        now: &str,
    ) -> Result<Option<LoginTicket>>;
    async fn insert_session(&mut self, s: &Session) -> Result<()>;
    async fn delete_session(&mut self, id: &str) -> Result<u64>;
    async fn delete_member_sessions(&mut self, member_id: &str) -> Result<u64>;
    /// 처음 알리는 신호면 적고 `true` (ADR-0074).
    async fn mark_signal_sent(&mut self, key: &str, at: &str) -> Result<bool>;
    /// 링크 카드를 다시 가져올 차례로 되돌린다. 없는 주소면 `None` (ADR-0084).
    async fn reset_link(&mut self, url: &str) -> Result<Option<LinkPreview>>;
    /// 마지막 로그인을 적고 휴면 미리 알림 표시를 지운다 (ADR-0073).
    async fn touch_login(&mut self, member_id: &str, now: &str) -> Result<()>;
    async fn mark_dormant_notice(&mut self, member_id: &str, now: &str) -> Result<()>;
    /// 메일 주소를 분리 표로 옮기고 `dormant+<id>@invalid` 로 바꾼다. 옮겼으면 `true`.
    async fn make_dormant(&mut self, member_id: &str, now: &str) -> Result<bool>;
    /// 휴면이면 메일 주소를 되돌리고 그 주소를 돌려준다.
    async fn wake_member(&mut self, member_id: &str) -> Result<Option<String>>;
    async fn member_totp(&mut self, member_id: &str) -> Result<Option<MemberTotp>>;
    /// 넣거나 통째로 바꾼다.
    async fn put_member_totp(&mut self, t: &MemberTotp) -> Result<()>;
    /// `last_step` 이 `step` 보다 작을 때만 올린다. 올렸으면 `true` (같은 코드를 동시에 두 번 써도 한 번만).
    async fn advance_totp_step(&mut self, member_id: &str, step: i64) -> Result<bool>;
    /// TOTP·복구 코드·확인 단계를 모두 지운다. 지운 TOTP 가 있었으면 `true`.
    async fn delete_member_totp(&mut self, member_id: &str) -> Result<bool>;
    /// 복구 코드를 통째로 바꾼다.
    async fn replace_recovery_codes(&mut self, member_id: &str, hashes: &[String]) -> Result<()>;
    /// 쓰지 않은 코드를 사용 처리한다. 했으면 `true`.
    async fn use_recovery_code(&mut self, member_id: &str, hash: &str, now: &str) -> Result<bool>;
    async fn insert_totp_challenge(&mut self, c: &TotpChallenge) -> Result<()>;
    /// 지웠으면 `true`. 확인 단계는 지우면서 써서 동시에 두 번 성공하지 않는다.
    async fn take_totp_challenge(&mut self, id: &str) -> Result<bool>;

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
