//! 글 서비스. 공개 조회는 `public_*` 함수만 쓴다 (PHASE1_PLAN §4.6).

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};

use crate::actor::{Actor, Permission};
use crate::audit::{self, Target};
use crate::clock;
use crate::error::{Code, Error, Result};
use crate::fields;
use crate::model::{
    Board, Category, CommentStatus, Listing, PageReq, Post, PostKind, PostStatus, Revision, Tag,
};
use crate::services::{boards, categories, comments, redirects, required_text, tags};
use crate::slug;
use crate::store::{Ctx, PostFilter, PublishedQuery, Tx, WriteTx};

pub(crate) const MAX_TITLE: usize = 200;
const MAX_BODY_BYTES: usize = 1024 * 1024;
/// 한 번에 발행하는 예약 글 수. 남은 것은 다음 주기에.
const DUE_BATCH: i64 = 50;
const MAX_COVER: usize = 300;
/// 예약은 1년 안쪽만.
const MAX_SCHEDULE_DAYS: i64 = 366;

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct NewPost {
    /// 게시판 슬러그
    pub board: String,
    pub title: String,
    /// 비우면 제목에서 만든다
    #[serde(default)]
    pub slug: Option<String>,
    #[serde(default)]
    pub body_md: String,
    #[serde(default)]
    pub excerpt: Option<String>,
    /// 대표 이미지 (`/media/...`). 비우면 본문 첫 사진
    #[serde(default)]
    pub cover: Option<String>,
    /// 분류 슬러그 (`maru category list --board <게시판>`). 비우면 분류 없음
    #[serde(default)]
    pub category: Option<String>,
    /// 태그 이름. 앞의 `#` 은 떼고, 글마다 20개·태그마다 30자까지
    #[serde(default)]
    pub tags: Vec<String>,
    /// 게시판 목록 맨 위에 고정한다. 발행 권한이 필요하다
    #[serde(default)]
    pub notice: bool,
    /// 비밀글: 목록에는 제목만 보이고 본문은 작성자와 관리자만 읽는다
    #[serde(default)]
    pub secret: bool,
    /// 여분 칸 값 (`maru board show <게시판>` 의 `field_schema`, ADR-0057)
    #[serde(default)]
    pub meta: Map<String, Value>,
}

pub async fn create_draft(ctx: &Ctx, actor: &Actor, input: NewPost) -> Result<Post> {
    create_inner(ctx, actor, input, None, true).await
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ReplyInput {
    pub title: String,
    #[serde(default)]
    pub body_md: String,
    /// 비우면 제목에서 만든다
    #[serde(default)]
    pub slug: Option<String>,
    #[serde(default)]
    pub tags: Vec<String>,
    /// 위 글이 비밀글이면 답변도 늘 비밀글이다
    #[serde(default)]
    pub secret: bool,
    /// 여분 칸 값 (ADR-0057)
    #[serde(default)]
    pub meta: Map<String, Value>,
}

/// 답변글 초안을 만든다 (ADR-0056). 게시판·분류는 위 글을 따르고, 발행은 `publish` 로.
pub async fn reply(ctx: &Ctx, actor: &Actor, parent_id: &str, input: ReplyInput) -> Result<Post> {
    let parent = get(ctx, actor, parent_id).await?;
    let board = ctx
        .store
        .board_by_id(&parent.board_id)
        .await?
        .ok_or_else(|| not_found(parent_id))?;
    let new = NewPost {
        board: board.slug,
        title: input.title,
        slug: input.slug,
        body_md: input.body_md,
        excerpt: None,
        cover: None,
        category: None,
        tags: input.tags,
        notice: false,
        secret: input.secret,
        meta: input.meta,
    };
    // 답변은 필수 칸을 묻지 않는다(질문의 항목을 다시 채울 일이 없다). 모르는 칸은 버린다.
    create_inner(ctx, actor, new, Some(&parent.id), false).await
}

/// 답변 깊이 한도.
pub const MAX_REPLY_DEPTH: usize = 5;

/// 답변글이 들어갈 자리. 위 글을 트랜잭션 안에서 읽어 형제 순서가 겹치지 않는다.
pub(crate) struct ReplySpot {
    parent_id: String,
    thread_id: String,
    thread_path: String,
    thread_owner: Option<String>,
    pub(crate) category_id: Option<String>,
    pub(crate) secret: bool,
}

impl ReplySpot {
    pub(crate) fn apply(self, post: &mut Post) {
        post.parent_id = Some(self.parent_id);
        post.thread_id = Some(self.thread_id);
        post.thread_path = self.thread_path;
        post.thread_owner = self.thread_owner;
        post.category_id = self.category_id;
        post.is_secret |= self.secret;
    }
}

pub(crate) async fn reply_spot(
    tx: &mut dyn WriteTx,
    actor: &Actor,
    board: &Board,
    parent_id: &str,
) -> Result<ReplySpot> {
    let parent = tx
        .post_by_id(parent_id)
        .await?
        .filter(|p| p.board_id == board.id && p.status == PostStatus::Published)
        .ok_or_else(|| not_found(parent_id))?;
    if board.policy.reply == "none" {
        return Err(Error::new(Code::Forbidden)
            .with_message("error.reply_closed")
            .next(format!("maru board update {} --reply editor", board.slug)));
    }
    actor.require_rank(Permission::PostWrite, &board.policy.reply)?;
    if parent.is_secret && !can_read_secret(actor, board, &parent) {
        return Err(not_found(parent_id));
    }
    check_unlocked(actor, board, &parent)?;
    if let Some(root) = &parent.thread_id
        && let Some(root) = tx.post_by_id(root).await?
    {
        check_unlocked(actor, board, &root)?;
    }
    if parent.depth() >= MAX_REPLY_DEPTH {
        return Err(Error::new(Code::Validation)
            .with_message("error.reply_depth")
            .arg("max", MAX_REPLY_DEPTH.to_string()));
    }
    let n = tx.reply_count(&parent.id).await?;
    let root = parent.thread_id.is_none();
    Ok(ReplySpot {
        thread_path: format!("{}{:04}.", parent.thread_path, n + 1),
        thread_id: parent
            .thread_id
            .clone()
            .unwrap_or_else(|| parent.id.clone()),
        thread_owner: if root {
            parent.author_id.clone()
        } else {
            parent.thread_owner.clone()
        },
        category_id: parent.category_id.clone(),
        secret: parent.is_secret,
        parent_id: parent.id,
    })
}

/// `strict_fields` 가 거짓이면(다른 게시판으로 복사) 새 게시판에 없는 여분 칸은 버린다.
async fn create_inner(
    ctx: &Ctx,
    actor: &Actor,
    input: NewPost,
    reply_to: Option<&str>,
    strict_fields: bool,
) -> Result<Post> {
    let author_type = actor
        .author_type()
        .ok_or_else(|| actor.denied(Permission::PostWrite))?;
    actor.require(Permission::PostWrite)?;
    let title = required_text("title", &input.title, MAX_TITLE)?;
    let body_md = check_body(input.body_md)?;
    let excerpt = input
        .excerpt
        .map(|e| e.trim().to_owned())
        .filter(|e| !e.is_empty());
    let cover = check_cover(input.cover.as_deref().unwrap_or(""))?;

    let mut tx = Tx::begin(ctx).await?;
    let board = tx
        .board_by_slug(&input.board)
        .await?
        .ok_or_else(|| boards::not_found(&input.board))?;
    actor.require_board(Permission::PostWrite, &board.id)?;
    actor.require_rank(Permission::PostWrite, &board.policy.write)?;

    let slug = match input
        .slug
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        Some(s) => {
            let s = slug::normalize(s)?;
            if tx.slug_taken(&board.id, &s, None).await? {
                return Err(slug_conflict(&board, &s));
            }
            s
        }
        None => unique_slug(&mut *tx, &board.id, &slug::from_title(&title, "post"), None).await?,
    };
    let category_id = match input.category.as_deref() {
        Some(c) => categories::resolve(&mut *tx, &board, c)
            .await?
            .map(|c| c.id),
        None => None,
    };
    let tags = tags::normalize(&mut *tx, &input.tags).await?;
    let spot = match reply_to {
        Some(id) => Some(reply_spot(&mut *tx, actor, &board, id).await?),
        None => None,
    };
    let meta = fields::validate(&board.field_schema, &input.meta, strict_fields)?;
    if input.notice {
        actor.require(Permission::PostPublish)?;
        actor.require_board(Permission::PostPublish, &board.id)?;
        actor.require_rank(Permission::PostPublish, &board.policy.write)?;
    }

    let now = clock::now();
    let mut post = Post {
        id: clock::new_id(),
        board_id: board.id.clone(),
        kind: PostKind::Post,
        status: PostStatus::Draft,
        title,
        slug,
        body_md,
        excerpt,
        author_id: actor.member_id().cloned(),
        actor_type: author_type,
        published_at: None,
        created_at: now.clone(),
        updated_at: now,
        scheduled_at: None,
        cover,
        category_id,
        is_notice: input.notice,
        is_secret: input.secret,
        tags,
        view_count: 0,
        guest_name: None,
        guest_pw_hash: None,
        parent_id: None,
        thread_id: None,
        thread_path: String::new(),
        thread_owner: None,
        meta,
        last_activity_at: None,
        accepted_comment_id: None,
        is_locked: false,
    };
    if let Some(spot) = spot {
        spot.apply(&mut post);
    }
    tx.insert_post(&post).await?;
    tx.insert_revision(&revision_of(&post, actor)).await?;
    tx.audit(
        actor,
        "post.create",
        Target::post(&post.id),
        audit::diff(None, Some(&post)),
    )
    .await?;
    tx.commit().await?;
    Ok(post)
}

/// 초안·검토·숨김 글을 발행한다. 이미 발행된 글이면 그대로 돌려준다(멱등).
pub async fn publish(ctx: &Ctx, actor: &Actor, id: &str) -> Result<Post> {
    publish_as(ctx, actor, id, "post.publish").await
}

/// 예약 시각이 지난 글을 발행한다. 서버가 주기적으로 부른다.
/// 예약은 발행 권한이 있는 사람만 걸 수 있으므로 여기서는 로컬 권한으로 발행한다.
/// 한 글이 실패해도 나머지는 계속하고, 글마다 결과를 돌려준다 (실패한 글은 다음 주기에 다시).
pub async fn publish_due(ctx: &Ctx) -> Result<Vec<(String, Result<Post>)>> {
    let ids = ctx.store.due_posts(&clock::now(), DUE_BATCH).await?;
    let actor = Actor::local();
    let mut out = Vec::with_capacity(ids.len());
    for id in ids {
        let res = publish_as(ctx, &actor, &id, "post.publish_scheduled").await;
        out.push((id, res));
    }
    Ok(out)
}

async fn publish_as(ctx: &Ctx, actor: &Actor, id: &str, action: &'static str) -> Result<Post> {
    let mut tx = Tx::begin(ctx).await?;
    let (board, before) = load_for_write(&mut *tx, actor, id).await?;
    authorize_post(actor, &board, &before, Permission::PostPublish)?;
    if before.status == PostStatus::Published {
        return Ok(before);
    }
    if !matches!(
        before.status,
        PostStatus::Draft | PostStatus::Review | PostStatus::Hidden | PostStatus::Pending
    ) {
        return Err(bad_transition(&before, PostStatus::Published));
    }
    let now = clock::now();
    let after = Post {
        status: PostStatus::Published,
        published_at: before.published_at.clone().or_else(|| Some(now.clone())),
        updated_at: now,
        scheduled_at: None,
        ..before.clone()
    };
    tx.update_post(&after).await?;
    if let Some(root) = &after.thread_id {
        tx.touch_post(root, &after.updated_at).await?;
    }
    tx.audit(
        actor,
        action,
        Target::post(&after.id),
        audit::diff(Some(&before), Some(&after)),
    )
    .await?;
    tx.commit().await?;
    Ok(after)
}

/// 관리용 단건 조회. 공개 글이 아니면 쓸 권한이 있는 사람만 본다.
pub async fn get(ctx: &Ctx, actor: &Actor, id: &str) -> Result<Post> {
    let post = ctx
        .store
        .post_by_id(id)
        .await?
        .ok_or_else(|| not_found(id))?;
    let board = ctx
        .store
        .board_by_id(&post.board_id)
        .await?
        .ok_or_else(|| not_found(id))?;
    if post.is_public()
        && actor.has(Permission::PostRead)
        && actor
            .require_rank(Permission::PostRead, &board.policy.read)
            .is_ok()
    {
        if post.is_secret && !can_read_secret(actor, &board, &post) {
            return Err(secret(&post));
        }
        return Ok(post);
    }
    // 초안은 존재를 드러내지 않는다.
    authorize_post(actor, &board, &post, Permission::PostWrite).map_err(|_| not_found(id))?;
    Ok(post)
}

/// 공개 글 하나. 홈·목록·RSS·사이트맵과 같은 공개 기준을 쓴다.
/// 비밀글은 작성자·관리자가 아니면 `FORBIDDEN`(`error.post_secret`)이다. 글이 있다는 것은 숨기지 않는다.
pub async fn public_post(
    ctx: &Ctx,
    actor: &Actor,
    board_slug: &str,
    post_slug: &str,
) -> Result<(Board, Post)> {
    let board = boards::readable(ctx, actor, board_slug).await?;
    let slug = slug::normalize(post_slug).map_err(|_| not_found(post_slug))?;
    let post = ctx
        .store
        .post_by_slug(&board.id, &slug)
        .await?
        .filter(Post::is_public)
        .ok_or_else(|| not_found(post_slug))?;
    if post.is_secret && !can_read_secret(actor, &board, &post) {
        return Err(secret(&post));
    }
    Ok((board, post))
}

/// 게시판 목록 순서 (GAP Q7, ADR-0092). 추천 순은 반응 모듈의 인기글 화면이 맡는다.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ListSort {
    /// 최신(또는 게시판이 정한 최근 활동) 순
    #[default]
    Latest,
    /// 조회 많은 순
    Views,
    /// 공개 댓글 많은 순
    Comments,
}

impl ListSort {
    /// 모르는 값은 최신 순으로 (오래된 링크도 열린다).
    pub fn parse(s: &str) -> Self {
        match s.trim() {
            "views" => Self::Views,
            "comments" => Self::Comments,
            _ => Self::Latest,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Latest => "latest",
            Self::Views => "views",
            Self::Comments => "comments",
        }
    }
}

/// 공개 목록 조건 (`public_posts_where`).
#[derive(Debug, Clone, Default)]
pub struct PublicFilter {
    /// 공지만(`Some(true)`) 또는 공지 빼고(`Some(false)`).
    pub notices: Option<bool>,
    pub category_id: Option<String>,
    pub tag_id: Option<String>,
    /// 비밀글을 아예 뺀다 (피드·사이트맵·태그 목록).
    pub exclude_secret: bool,
    /// 마지막 활동이 늦은 스레드부터 (ADR-0058)
    pub by_activity: bool,
    /// 여분 필드 거르기 (ADR-0089)
    pub meta: Vec<crate::fields::MetaFilter>,
    /// 목록 순서 (ADR-0092)
    pub sort: ListSort,
}

/// 공개 글 목록. `board` 가 없으면 읽을 수 있는 모든 게시판.
/// 볼 수 없는 비밀글은 제목만 남기고 본문·요약·대표 이미지·태그를 비운다.
pub async fn public_posts(
    ctx: &Ctx,
    actor: &Actor,
    board: Option<&Board>,
    page: PageReq,
) -> Result<Listing<Post>> {
    public_posts_where(ctx, actor, board, &PublicFilter::default(), page).await
}

/// 인기 글 (ADR-0059): 최근 `days` 일(0 이면 전체) 안에 발행된 공개 글을 조회수 → 댓글 수 → 최신 순.
/// 비밀글은 뺀다.
pub async fn public_popular(
    ctx: &Ctx,
    actor: &Actor,
    board: Option<&Board>,
    days: u32,
    limit: u32,
) -> Result<Vec<Post>> {
    if !actor.has(Permission::PostRead) {
        return Ok(vec![]);
    }
    let q = crate::store::PopularQuery {
        board_id: board.map(|b| b.id.clone()),
        max_read_rank: actor.rank(),
        since: (days > 0).then(|| clock::hours_ago(i64::from(days) * 24)),
        limit: i64::from(limit.clamp(1, 50)),
    };
    ctx.store.list_popular(&q).await
}

pub async fn public_posts_where(
    ctx: &Ctx,
    actor: &Actor,
    board: Option<&Board>,
    filter: &PublicFilter,
    page: PageReq,
) -> Result<Listing<Post>> {
    if !actor.has(Permission::PostRead) {
        return Ok(Listing {
            items: vec![],
            total: 0,
            page: page.page,
            per_page: page.per_page,
        });
    }
    let q = PublishedQuery {
        board_id: board.map(|b| b.id.clone()),
        category_id: filter.category_id.clone(),
        notices: filter.notices,
        tag_id: filter.tag_id.clone(),
        exclude_secret: filter.exclude_secret,
        by_activity: filter.by_activity,
        meta_filters: if filter.meta.is_empty() {
            None
        } else {
            Some(serde_json::to_string(&filter.meta).map_err(Error::internal)?)
        },
        sort: (filter.sort != ListSort::Latest).then(|| filter.sort.as_str().to_owned()),
        ..PublishedQuery::new(actor.rank(), page.limit(), page.offset())
    };
    let total = ctx.store.count_published(&q).await?;
    let items = ctx.store.list_published(&q).await?;
    Ok(Listing {
        items: mask_secrets(ctx, actor, board, items).await?,
        total,
        page: page.page,
        per_page: page.per_page,
    })
}

/// 게시판 첫 화면에 필요한 것.
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct BoardPage {
    /// 공지 (모든 쪽·모든 분류에서 맨 위)
    pub notices: Vec<Post>,
    /// 공지를 뺀 글
    pub posts: Listing<Post>,
    pub categories: Vec<Category>,
    /// 고른 분류
    pub category: Option<Category>,
    /// 실제로 건 여분 필드 거르기 (칸 키, 값). 버려진 값은 없다 (ADR-0089)
    pub filters: Vec<(String, String)>,
    /// 목록 순서 (ADR-0092)
    pub sort: ListSort,
}

const MAX_NOTICES: u32 = 20;

/// 게시판 목록 화면. `category` 는 분류 슬러그.
pub async fn public_board(
    ctx: &Ctx,
    actor: &Actor,
    board: &Board,
    category: Option<&str>,
    page: PageReq,
) -> Result<BoardPage> {
    public_board_filtered(ctx, actor, board, category, &[], ListSort::Latest, page).await
}

/// 여분 필드로 거른 게시판 목록 (GAP Q4, ADR-0089). `fields` 는 주소의 `f.<키>=<값>` 쌍(앞머리 뗀 것).
/// 거를 수 없는 칸·값은 버리고, 실제로 쓴 조건을 `BoardPage::filters` 로 돌려준다. 공지는 거르지 않는다.
pub async fn public_board_filtered(
    ctx: &Ctx,
    actor: &Actor,
    board: &Board,
    category: Option<&str>,
    fields: &[(String, String)],
    sort: ListSort,
    page: PageReq,
) -> Result<BoardPage> {
    let tz = crate::services::settings::site(ctx).await?.tz_offset_min;
    let applied = crate::fields::filters(&board.field_schema, fields, &crate::clock::today(tz));
    let categories = ctx.store.categories_of_board(&board.id).await?;
    let category = match category.map(str::trim).filter(|c| !c.is_empty()) {
        Some(c) => {
            let key = slug::normalize(c).unwrap_or_default();
            Some(
                categories
                    .iter()
                    .find(|x| x.slug == key)
                    .cloned()
                    .ok_or_else(|| {
                        Error::new(Code::NotFound)
                            .with_message("error.category_not_found")
                            .arg("board", board.slug.clone())
                            .arg("slug", c)
                    })?,
            )
        }
        None => None,
    };
    let notices = public_posts_where(
        ctx,
        actor,
        Some(board),
        &PublicFilter {
            notices: Some(true),
            ..PublicFilter::default()
        },
        PageReq::new(1, MAX_NOTICES),
    )
    .await?
    .items;
    let posts = public_posts_where(
        ctx,
        actor,
        Some(board),
        &PublicFilter {
            notices: Some(false),
            category_id: category.as_ref().map(|c| c.id.clone()),
            by_activity: board.effective_order() == "activity",
            meta: applied.iter().map(|(_, f)| f.clone()).collect(),
            sort,
            ..PublicFilter::default()
        },
        page,
    )
    .await?;
    Ok(BoardPage {
        notices,
        posts,
        categories,
        category,
        filters: applied.into_iter().map(|(d, f)| (d.key, f.token)).collect(),
        sort,
    })
}

/// 태그가 달린 공개 글 (비밀글 제외).
pub async fn public_tag(
    ctx: &Ctx,
    actor: &Actor,
    tag_slug: &str,
    page: PageReq,
) -> Result<(Tag, Listing<Post>)> {
    let tag = tags::find(ctx, tag_slug).await?;
    let list = public_posts_where(
        ctx,
        actor,
        None,
        &PublicFilter {
            tag_id: Some(tag.id.clone()),
            exclude_secret: true,
            ..PublicFilter::default()
        },
        page,
    )
    .await?;
    Ok((tag, list))
}

/// 글 아래의 이웃: 같은 게시판의 이전(더 오래된)·다음(더 새) 글과 관련 글.
#[derive(Debug, Clone, Default, Serialize, JsonSchema)]
pub struct Nearby {
    pub prev: Option<Post>,
    pub next: Option<Post>,
    /// 겹치는 태그 수 → 같은 분류 → 같은 게시판 → 최신 순. 비밀글·공지는 없다
    pub related: Vec<Post>,
}

const RELATED_LIMIT: i64 = 4;

/// `public_post` 로 읽은 글의 이웃. 이전·다음 글이 볼 수 없는 비밀글이면 제목만 남긴다.
pub async fn nearby(ctx: &Ctx, actor: &Actor, board: &Board, post: &Post) -> Result<Nearby> {
    if !post.is_public()
        || !actor.has(Permission::PostRead)
        || actor
            .require_rank(Permission::PostRead, &board.policy.read)
            .is_err()
    {
        return Ok(Nearby::default());
    }
    let (prev, next) = ctx.store.neighbor_posts(post).await?;
    let related = ctx
        .store
        .related_posts(post, actor.rank(), RELATED_LIMIT)
        .await?;
    let shown = |p: Post| {
        if p.is_secret && !can_read_secret(actor, board, &p) {
            masked(p)
        } else {
            p
        }
    };
    Ok(Nearby {
        prev: prev.map(shown),
        next: next.map(shown),
        related,
    })
}

/// 비밀글 본문을 볼 수 있는가: 작성자 본인 또는 그 게시판을 관리할 수 있는 사람.
pub fn can_read_secret(actor: &Actor, board: &Board, post: &Post) -> bool {
    is_own(actor, post)
        // 비밀 답변은 맨 위 글을 쓴 회원도 읽는다 (ADR-0056).
        || (post.thread_owner.is_some() && actor.member_id() == post.thread_owner.as_ref())
        || (actor.has(Permission::PostModerate)
            && actor
                .require_rank(Permission::PostModerate, &board.policy.moderate)
                .is_ok())
}

/// 볼 수 없는 비밀글의 본문·요약·대표 이미지·태그를 비운다.
async fn mask_secrets(
    ctx: &Ctx,
    actor: &Actor,
    board: Option<&Board>,
    items: Vec<Post>,
) -> Result<Vec<Post>> {
    if !items.iter().any(|p| p.is_secret) {
        return Ok(items);
    }
    let all = match board {
        Some(b) => vec![b.clone()],
        None => ctx.store.list_boards().await?,
    };
    Ok(items
        .into_iter()
        .map(|p| {
            let readable = !p.is_secret
                || all
                    .iter()
                    .find(|b| b.id == p.board_id)
                    .is_some_and(|b| can_read_secret(actor, b, &p));
            if readable { p } else { masked(p) }
        })
        .collect())
}

/// 비밀글의 제목만 남긴다.
fn masked(p: Post) -> Post {
    Post {
        body_md: String::new(),
        excerpt: None,
        cover: None,
        tags: vec![],
        meta: Map::new(),
        ..p
    }
}

fn secret(post: &Post) -> Error {
    Error::new(Code::Forbidden)
        .with_message("error.post_secret")
        .arg("title", post.title.clone())
        .with_hint("hint.post_secret")
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PostPatch {
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub slug: Option<String>,
    #[serde(default)]
    pub body_md: Option<String>,
    /// 빈 문자열이면 요약을 지운다
    #[serde(default)]
    pub excerpt: Option<String>,
    /// 대표 이미지 (`/media/...`). 빈 문자열이면 지운다
    #[serde(default)]
    pub cover: Option<String>,
    /// 예약 발행 시각 (RFC3339, 예: `2026-11-05T09:00:00+09:00`). 빈 문자열이면 예약을 취소한다.
    /// 발행 권한이 필요하고, 아직 공개되지 않은 글에만 걸린다
    #[serde(default)]
    pub scheduled_at: Option<String>,
    /// 분류 슬러그. 빈 문자열이면 분류를 뺀다
    #[serde(default)]
    pub category: Option<String>,
    /// 태그를 통째로 바꾼다. 빈 배열이면 모두 뺀다
    #[serde(default)]
    pub tags: Option<Vec<String>>,
    /// 게시판 맨 위 고정. 바꾸려면 발행 권한이 필요하다
    #[serde(default)]
    pub notice: Option<bool>,
    /// 비밀글 여부
    #[serde(default)]
    pub secret: Option<bool>,
    /// 여분 칸 값을 통째로 바꾼다. 빈 객체면 모두 지운다 (ADR-0057)
    #[serde(default)]
    pub meta: Option<Map<String, Value>>,
}

/// 글을 고친다. 제목이나 본문이 바뀌면 리비전을 남긴다.
pub async fn update(ctx: &Ctx, actor: &Actor, id: &str, patch: PostPatch) -> Result<Post> {
    let mut tx = Tx::begin(ctx).await?;
    let (board, before) = load_for_write(&mut *tx, actor, id).await?;
    authorize_post(actor, &board, &before, Permission::PostWrite)?;
    check_unlocked(actor, &board, &before)?;
    if before.status == PostStatus::Deleted {
        return Err(bad_transition(&before, before.status));
    }
    let mut after = before.clone();
    if let Some(t) = &patch.title {
        after.title = required_text("title", t, MAX_TITLE)?;
    }
    if let Some(b) = patch.body_md {
        after.body_md = check_body(b)?;
    }
    if let Some(e) = patch.excerpt {
        let e = e.trim().to_owned();
        after.excerpt = (!e.is_empty()).then_some(e);
    }
    if let Some(c) = &patch.cover {
        after.cover = check_cover(c)?;
    }
    if let Some(m) = &patch.meta {
        after.meta = fields::validate(&board.field_schema, m, true)?;
    }
    if let Some(at) = &patch.scheduled_at {
        after.scheduled_at = check_schedule(at)?;
        if after.scheduled_at.is_some() && after.scheduled_at != before.scheduled_at {
            authorize_post(actor, &board, &before, Permission::PostPublish)?;
            if !matches!(
                before.status,
                PostStatus::Draft | PostStatus::Review | PostStatus::Hidden
            ) {
                return Err(Error::new(Code::Conflict)
                    .with_message("error.post_schedule_status")
                    .arg("status", before.status.as_str())
                    .with_hint("hint.post_schedule_status")
                    .next(format!("maru post hide {}", before.id)));
            }
        }
    }
    if let Some(s) = &patch.slug {
        let s = slug::normalize(s)?;
        if s != before.slug && tx.slug_taken(&board.id, &s, Some(&before.id)).await? {
            return Err(slug_conflict(&board, &s));
        }
        after.slug = s;
    }
    if let Some(c) = &patch.category {
        after.category_id = categories::resolve(&mut *tx, &board, c)
            .await?
            .map(|c| c.id);
    }
    if let Some(t) = &patch.tags {
        after.tags = tags::normalize(&mut *tx, t).await?;
    }
    if let Some(n) = patch.notice {
        if n != before.is_notice {
            authorize_post(actor, &board, &before, Permission::PostPublish)?;
        }
        after.is_notice = n;
    }
    if let Some(s) = patch.secret {
        after.is_secret = s;
    }
    if after == before {
        return Ok(before);
    }
    after.updated_at = clock::now();
    tx.update_post(&after).await?;
    if after.slug != before.slug {
        let old = crate::urls::path(&crate::urls::Target::Post {
            board: &board.slug,
            post: &before.slug,
        });
        let new = crate::urls::path(&crate::urls::Target::Post {
            board: &board.slug,
            post: &after.slug,
        });
        redirects::moved(&mut tx, &old, &new).await?;
    }
    if after.title != before.title || after.body_md != before.body_md {
        tx.insert_revision(&revision_of(&after, actor)).await?;
    }
    tx.audit(
        actor,
        "post.update",
        Target::post(&after.id),
        audit::diff(Some(&before), Some(&after)),
    )
    .await?;
    tx.commit().await?;
    Ok(after)
}

/// 초안을 검토 요청 상태로.
pub async fn submit(ctx: &Ctx, actor: &Actor, id: &str) -> Result<Post> {
    transition(
        ctx,
        actor,
        id,
        Permission::PostWrite,
        &[PostStatus::Draft],
        |_| PostStatus::Review,
        "post.submit",
    )
    .await
}

/// 공개 글을 숨긴다. 목록·글·RSS·사이트맵에서 함께 빠진다.
pub async fn hide(ctx: &Ctx, actor: &Actor, id: &str) -> Result<Post> {
    transition(
        ctx,
        actor,
        id,
        Permission::PostPublish,
        &[PostStatus::Published],
        |_| PostStatus::Hidden,
        "post.hide",
    )
    .await
}

/// 되돌릴 수 있는 삭제 (ADR-0029). 영구 삭제는 없다.
pub async fn delete(ctx: &Ctx, actor: &Actor, id: &str) -> Result<Post> {
    transition(
        ctx,
        actor,
        id,
        Permission::PostWrite,
        &[
            PostStatus::Draft,
            PostStatus::Review,
            PostStatus::Published,
            PostStatus::Hidden,
        ],
        |_| PostStatus::Deleted,
        "post.delete",
    )
    .await
}

/// 삭제·숨김을 되돌린다. 한 번이라도 발행된 글이면 공개로, 아니면 초안으로.
pub async fn restore(ctx: &Ctx, actor: &Actor, id: &str) -> Result<Post> {
    transition(
        ctx,
        actor,
        id,
        Permission::PostWrite,
        &[PostStatus::Deleted, PostStatus::Hidden],
        |p| {
            if p.published_at.is_some() {
                PostStatus::Published
            } else {
                PostStatus::Draft
            }
        },
        "post.restore",
    )
    .await
}

async fn transition(
    ctx: &Ctx,
    actor: &Actor,
    id: &str,
    perm: Permission,
    from: &[PostStatus],
    to: impl Fn(&Post) -> PostStatus,
    action: &'static str,
) -> Result<Post> {
    let mut tx = Tx::begin(ctx).await?;
    let (board, before) = load_for_write(&mut *tx, actor, id).await?;
    authorize_post(actor, &board, &before, perm)?;
    if perm == Permission::PostWrite {
        check_unlocked(actor, &board, &before)?;
    }
    let target = to(&before);
    if before.status == target {
        return Ok(before);
    }
    if !from.contains(&before.status) {
        return Err(bad_transition(&before, target));
    }
    // 다시 공개되는 경로는 발행 권한이 필요하다.
    if target == PostStatus::Published {
        authorize_post(actor, &board, &before, Permission::PostPublish)?;
    }
    // 예약은 아직 공개 전인 글에만 남는다. 삭제·공개되면 함께 지운다.
    let keep_schedule = matches!(
        target,
        PostStatus::Draft | PostStatus::Review | PostStatus::Hidden
    );
    let after = Post {
        status: target,
        updated_at: clock::now(),
        scheduled_at: before.scheduled_at.clone().filter(|_| keep_schedule),
        ..before.clone()
    };
    tx.update_post(&after).await?;
    tx.audit(
        actor,
        action,
        Target::post(&after.id),
        audit::diff(Some(&before), Some(&after)),
    )
    .await?;
    tx.commit().await?;
    Ok(after)
}

/// 같은 게시판에 같은 슬러그가 있으면 그것을 돌려준다 (`--if-not-exists`).
pub async fn ensure_draft(
    ctx: &Ctx,
    actor: &Actor,
    input: NewPost,
) -> Result<boards::Created<Post>> {
    if let Some(s) = input.slug.as_deref().filter(|s| !s.trim().is_empty()) {
        let board = ctx
            .store
            .board_by_slug(&input.board)
            .await?
            .ok_or_else(|| boards::not_found(&input.board))?;
        let s = slug::normalize(s)?;
        if let Some(existing) = ctx.store.post_by_slug(&board.id, &s).await? {
            authorize_post(actor, &board, &existing, Permission::PostWrite)?;
            return Ok(boards::Created {
                created: false,
                item: existing,
            });
        }
    }
    Ok(boards::Created {
        created: true,
        item: create_draft(ctx, actor, input).await?,
    })
}

/// 관리용 목록. 남의 글 관리 권한이 없으면 본인 글만 남긴다.
/// `search` 가 있으면 제목·내용·태그에 그 말이 든 글만 (초안·비밀글 포함, GAP C5).
pub async fn list(
    ctx: &Ctx,
    actor: &Actor,
    board: Option<&str>,
    status: Option<PostStatus>,
    search: Option<&str>,
    page: PageReq,
) -> Result<Listing<Post>> {
    actor.require(Permission::PostWrite)?;
    let search = match search.map(str::trim).filter(|s| !s.is_empty()) {
        Some(q) => Some(crate::search::Query::parse(q)?.to_fts(crate::search::Range::All)),
        None => None,
    };
    let board_id = match board {
        Some(slug) => Some(
            ctx.store
                .board_by_slug(slug)
                .await?
                .ok_or_else(|| boards::not_found(slug))?
                .id,
        ),
        None => None,
    };
    let filter = PostFilter {
        board_id,
        status,
        search,
        limit: page.limit(),
        offset: page.offset(),
    };
    if actor.has(Permission::PostModerate) {
        let total = ctx.store.count_posts(&filter).await?;
        let items = ctx.store.list_posts(&filter).await?;
        return Ok(Listing {
            items,
            total,
            page: page.page,
            per_page: page.per_page,
        });
    }
    // 본인 글만: 전체를 읽어 거른 뒤 쪽을 나눈다. 글 수가 많아지면 SQL 조건으로 옮긴다.
    let all = PostFilter {
        limit: i64::MAX,
        offset: 0,
        ..filter
    };
    let own: Vec<Post> = ctx
        .store
        .list_posts(&all)
        .await?
        .into_iter()
        .filter(|p| is_own(actor, p))
        .collect();
    let total = i64::try_from(own.len()).unwrap_or(i64::MAX);
    let items = own
        .into_iter()
        .skip(usize::try_from(page.offset()).unwrap_or(usize::MAX))
        .take(page.per_page as usize)
        .collect();
    Ok(Listing {
        items,
        total,
        page: page.page,
        per_page: page.per_page,
    })
}

pub async fn revisions(ctx: &Ctx, actor: &Actor, id: &str) -> Result<Vec<Revision>> {
    let post = get(ctx, actor, id).await?;
    ctx.store.revisions_of_post(&post.id).await
}

// ---- 리비전 비교·되돌리기, 이동·복사, 일괄 (ADR-0055) ----

/// 리비전 하나와 지금 글의 비교.
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct RevisionCompare {
    pub revision: Revision,
    /// 그 리비전 → 지금 글로 무엇이 바뀌었나
    pub title_changed: bool,
    pub current_title: String,
    pub body: crate::diff::Diff,
}

/// 글의 리비전 하나. 다른 글의 리비전이면 없는 것으로 본다.
pub async fn revision(ctx: &Ctx, actor: &Actor, id: &str, rev: &str) -> Result<Revision> {
    let post = get(ctx, actor, id).await?;
    ctx.store
        .revisions_of_post(&post.id)
        .await?
        .into_iter()
        .find(|r| r.id == rev)
        .ok_or_else(|| revision_not_found(&post.id, rev))
}

pub async fn compare(ctx: &Ctx, actor: &Actor, id: &str, rev: &str) -> Result<RevisionCompare> {
    let post = get(ctx, actor, id).await?;
    let revision = revision(ctx, actor, id, rev).await?;
    Ok(RevisionCompare {
        title_changed: revision.title != post.title,
        current_title: post.title.clone(),
        body: crate::diff::lines(&revision.body_md, &post.body_md),
        revision,
    })
}

/// 그 리비전의 제목·본문으로 되돌린다. 권한·상태 규칙은 `update` 와 같고 리비전이 하나 더 쌓인다.
pub async fn revert(ctx: &Ctx, actor: &Actor, id: &str, rev: &str) -> Result<Post> {
    let mut tx = Tx::begin(ctx).await?;
    let (board, before) = load_for_write(&mut *tx, actor, id).await?;
    authorize_post(actor, &board, &before, Permission::PostWrite)?;
    check_unlocked(actor, &board, &before)?;
    if before.status == PostStatus::Deleted {
        return Err(bad_transition(&before, before.status));
    }
    // 리비전은 쌓기만 하므로 읽기 풀에서 찾아도 된다.
    let revision = ctx
        .store
        .revisions_of_post(&before.id)
        .await?
        .into_iter()
        .find(|r| r.id == rev)
        .ok_or_else(|| revision_not_found(&before.id, rev))?;
    if revision.title == before.title && revision.body_md == before.body_md {
        return Ok(before);
    }
    let mut after = before.clone();
    after.title = revision.title.clone();
    after.body_md = revision.body_md.clone();
    after.updated_at = clock::now();
    tx.update_post(&after).await?;
    tx.insert_revision(&revision_of(&after, actor)).await?;
    let mut diff = audit::diff(Some(&before), Some(&after));
    if let Some(o) = diff.as_object_mut() {
        o.insert("revision".into(), serde_json::json!(revision.id));
    }
    tx.audit(actor, "post.revert", Target::post(&after.id), diff)
        .await?;
    tx.commit().await?;
    Ok(after)
}

fn revision_not_found(post: &str, rev: &str) -> Error {
    Error::new(Code::NotFound)
        .with_message("error.revision_not_found")
        .arg("id", rev.to_owned())
        .next(format!("maru post revisions {post}"))
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct MoveInput {
    /// 옮겨 갈 게시판 슬러그
    pub board: String,
    /// 새 게시판의 분류 슬러그. 생략하면 분류를 비운다
    #[serde(default)]
    pub category: Option<String>,
}

/// 글을 다른 게시판으로 옮긴다. 댓글·조회수·리비전은 글을 따라간다.
pub async fn move_to(ctx: &Ctx, actor: &Actor, id: &str, input: MoveInput) -> Result<Post> {
    let mut tx = Tx::begin(ctx).await?;
    let (from, before) = load_for_write(&mut *tx, actor, id).await?;
    authorize_post(actor, &from, &before, Permission::PostWrite)?;
    if before.status == PostStatus::Deleted {
        return Err(bad_transition(&before, before.status));
    }
    let to = tx
        .board_by_slug(&input.board)
        .await?
        .ok_or_else(|| boards::not_found(&input.board))?;
    actor.require_board(Permission::PostWrite, &to.id)?;
    actor.require_rank(Permission::PostWrite, &to.policy.write)?;
    let mut after = before.clone();
    after.board_id = to.id.clone();
    // 새 게시판에 없는 칸·맞지 않는 값은 버린다.
    after.meta = fields::validate(&to.field_schema, &before.meta, false)?;
    after.category_id = match input.category.as_deref().filter(|c| !c.is_empty()) {
        Some(c) => categories::resolve(&mut *tx, &to, c).await?.map(|c| c.id),
        None => None,
    };
    if to.id != from.id {
        after.slug = unique_slug(&mut *tx, &to.id, &before.slug, Some(&before.id)).await?;
    }
    if after == before {
        return Ok(before);
    }
    after.updated_at = clock::now();
    tx.update_post(&after).await?;
    if to.id != from.id || after.slug != before.slug {
        let old = crate::urls::path(&crate::urls::Target::Post {
            board: &from.slug,
            post: &before.slug,
        });
        let new = crate::urls::path(&crate::urls::Target::Post {
            board: &to.slug,
            post: &after.slug,
        });
        redirects::moved(&mut tx, &old, &new).await?;
    }
    tx.audit(
        actor,
        "post.move",
        Target::post(&after.id),
        audit::diff(Some(&before), Some(&after)),
    )
    .await?;
    tx.commit().await?;
    Ok(after)
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CopyInput {
    /// 복사해 둘 게시판 슬러그 (같은 게시판도 된다)
    pub board: String,
}

/// 다른 게시판에 **초안**으로 복사한다. 글쓴이는 복사한 사람, 댓글·조회수는 가져가지 않는다.
pub async fn copy_to(ctx: &Ctx, actor: &Actor, id: &str, input: CopyInput) -> Result<Post> {
    let source = get(ctx, actor, id).await?;
    if source.status == PostStatus::Deleted {
        return Err(bad_transition(&source, source.status));
    }
    create_inner(
        ctx,
        actor,
        NewPost {
            board: input.board,
            title: source.title,
            slug: None,
            body_md: source.body_md,
            excerpt: source.excerpt,
            cover: source.cover,
            category: None,
            tags: source.tags,
            notice: false,
            secret: source.is_secret,
            meta: source.meta,
        },
        None,
        false,
    )
    .await
}

/// 한 번에 고를 수 있는 글 수.
pub const BULK_MAX: usize = 100;

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
pub enum BulkAction {
    Publish,
    Hide,
    Delete,
    Restore,
    Move {
        board: String,
        #[serde(default)]
        category: Option<String>,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct BulkInput {
    pub ids: Vec<String>,
    #[serde(flatten)]
    pub action: BulkAction,
}

#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct BulkItem {
    pub id: String,
    pub ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub post: Option<Post>,
    /// 실패한 까닭 (오류 코드와 메시지)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<BulkError>,
}

#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct BulkError {
    pub code: String,
    pub message: String,
}

/// 글마다 기존 서비스를 따로 부른다(각자 트랜잭션). 일부만 되는 것이 정상이다.
pub async fn bulk(ctx: &Ctx, actor: &Actor, input: BulkInput) -> Result<Vec<BulkItem>> {
    if input.ids.is_empty() || input.ids.len() > BULK_MAX {
        return Err(Error::new(Code::Validation)
            .with_message("error.bulk_size")
            .arg("max", BULK_MAX.to_string()));
    }
    let mut seen = std::collections::HashSet::new();
    let mut out = Vec::with_capacity(input.ids.len());
    for id in input.ids.iter().filter(|id| seen.insert(id.as_str())) {
        let res = match &input.action {
            BulkAction::Publish => publish(ctx, actor, id).await,
            BulkAction::Hide => hide(ctx, actor, id).await,
            BulkAction::Delete => delete(ctx, actor, id).await,
            BulkAction::Restore => restore(ctx, actor, id).await,
            BulkAction::Move { board, category } => {
                let input = MoveInput {
                    board: board.clone(),
                    category: category.clone(),
                };
                move_to(ctx, actor, id, input).await
            }
        };
        out.push(match res {
            Ok(post) => BulkItem {
                id: id.clone(),
                ok: true,
                post: Some(post),
                error: None,
            },
            Err(e) => BulkItem {
                id: id.clone(),
                ok: false,
                post: None,
                error: Some(BulkError {
                    code: e.code().as_str().to_owned(),
                    message: e.message(),
                }),
            },
        });
    }
    Ok(out)
}

/// 잠긴 글은 그 게시판 관리자만 건드린다 (ADR-0058).
pub(crate) fn check_unlocked(actor: &Actor, board: &Board, post: &Post) -> Result<()> {
    if post.is_locked && !comments::can_moderate(actor, board) {
        return Err(Error::new(Code::Forbidden)
            .with_message("error.post_locked")
            .next(format!("maru post lock {} --off", post.id)));
    }
    Ok(())
}

/// REST `POST /posts/{id}/lock`
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct LockInput {
    pub locked: bool,
}

/// REST `POST /posts/{id}/accept`. `comment_id` 가 비면 채택을 뺀다
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct AcceptInput {
    #[serde(default)]
    pub comment_id: Option<String>,
}

/// 글을 잠그거나 푼다. 그 게시판 관리자만 (ADR-0058).
pub async fn lock(ctx: &Ctx, actor: &Actor, id: &str, locked: bool) -> Result<Post> {
    let mut tx = Tx::begin(ctx).await?;
    let (board, before) = load_for_write(&mut *tx, actor, id).await?;
    actor.require(Permission::PostModerate)?;
    actor.require_board(Permission::PostModerate, &board.id)?;
    actor.require_rank(Permission::PostModerate, &board.policy.moderate)?;
    if before.status == PostStatus::Deleted {
        return Err(bad_transition(&before, before.status));
    }
    if before.is_locked == locked {
        return Ok(before);
    }
    let after = Post {
        is_locked: locked,
        updated_at: clock::now(),
        ..before.clone()
    };
    tx.update_post(&after).await?;
    tx.audit(
        actor,
        if locked { "post.lock" } else { "post.unlock" },
        Target::post(&after.id),
        audit::diff(Some(&before), Some(&after)),
    )
    .await?;
    tx.commit().await?;
    Ok(after)
}

/// 댓글을 답변으로 채택한다. `None` 이면 채택을 뺀다 (ADR-0058).
/// 글쓴 회원 본인이나 그 게시판 관리자만, `qna`·`forum` 게시판의 공개 글에서만.
pub async fn accept(ctx: &Ctx, actor: &Actor, id: &str, comment_id: Option<&str>) -> Result<Post> {
    let mut tx = Tx::begin(ctx).await?;
    let (board, before) = load_for_write(&mut *tx, actor, id).await?;
    may_accept(actor, &board, &before)?;
    let accepted = match comment_id {
        None => None,
        Some(cid) => {
            let c = tx
                .comment_by_id(cid)
                .await?
                .filter(|c| c.post_id == before.id && c.status == CommentStatus::Published)
                .ok_or_else(|| {
                    Error::new(Code::Validation)
                        .with_message("error.accept_comment")
                        .arg("id", cid)
                })?;
            Some(c.id)
        }
    };
    if accepted == before.accepted_comment_id {
        return Ok(before);
    }
    let after = Post {
        accepted_comment_id: accepted,
        updated_at: clock::now(),
        ..before.clone()
    };
    tx.update_post(&after).await?;
    tx.audit(
        actor,
        if after.accepted_comment_id.is_some() {
            "post.accept"
        } else {
            "post.unaccept"
        },
        Target::post(&after.id),
        json!({ "before": before.accepted_comment_id, "after": after.accepted_comment_id }),
    )
    .await?;
    tx.commit().await?;
    Ok(after)
}

/// 이 사람이 이 글의 답변을 고를 수 있는가 (ADR-0058).
pub(crate) fn may_accept(actor: &Actor, board: &Board, post: &Post) -> Result<()> {
    if !board.accepts_answers() {
        return Err(Error::new(Code::Validation)
            .with_message("error.accept_kind")
            .next(format!("maru board update {} --kind forum", board.slug)));
    }
    if post.status != PostStatus::Published {
        return Err(not_found(&post.id));
    }
    let moderator = comments::can_moderate(actor, board);
    if !moderator && !(is_own(actor, post) && actor.has(Permission::PostWrite)) {
        return Err(Error::new(Code::Forbidden).with_message("error.accept_who"));
    }
    check_unlocked(actor, board, post)
}

/// 본인 글이면 기본 권한, 남의 글이면 추가로 `post.moderate` (ADR-0029).
pub(crate) fn authorize_post(
    actor: &Actor,
    board: &Board,
    post: &Post,
    perm: Permission,
) -> Result<()> {
    actor.require(perm)?;
    actor.require_board(perm, &board.id)?;
    actor.require_rank(perm, &board.policy.write)?;
    if !is_own(actor, post) {
        actor.require(Permission::PostModerate)?;
        actor.require_rank(Permission::PostModerate, &board.policy.moderate)?;
    }
    Ok(())
}

/// 회원은 `author_id` 로, 에이전트는 에이전트가 쓴 글을 본인 글로 본다. 로컬은 운영자다.
/// 비회원 글은 누구의 글도 아니다 — 비밀번호(`board_posts`)나 관리 권한으로만 고친다.
fn is_own(actor: &Actor, post: &Post) -> bool {
    if post.guest_name.is_some() {
        return false;
    }
    match (actor.member_id(), actor.author_type()) {
        (Some(id), _) => post.author_id.as_deref() == Some(id.as_str()),
        (None, Some(t)) => post.author_id.is_none() && post.actor_type == t,
        (None, None) => false,
    }
}

async fn load_for_write(tx: &mut dyn WriteTx, actor: &Actor, id: &str) -> Result<(Board, Post)> {
    let post = tx.post_by_id(id).await?.ok_or_else(|| not_found(id))?;
    let board = tx
        .board_by_id(&post.board_id)
        .await?
        .ok_or_else(|| not_found(id))?;
    // 읽지도 못하는 사용자에게는 존재를 숨긴다.
    if actor.author_type().is_none() {
        return Err(actor.denied(Permission::PostWrite));
    }
    Ok((board, post))
}

pub(crate) async fn unique_slug(
    tx: &mut dyn WriteTx,
    board_id: &str,
    base: &str,
    except: Option<&str>,
) -> Result<String> {
    if !tx.slug_taken(board_id, base, except).await? {
        return Ok(base.to_owned());
    }
    for n in 2..1000 {
        let candidate = format!("{base}-{n}");
        if !tx.slug_taken(board_id, &candidate, except).await? {
            return Ok(candidate);
        }
    }
    Ok(format!("{base}-{}", clock::new_id()))
}

/// 읽기 전용 확인용 (check_slug). 저장은 트랜잭션 안의 slug_taken 으로 다시 확인한다.
async fn slug_used(ctx: &Ctx, board_id: &str, slug: &str, except: Option<&str>) -> Result<bool> {
    Ok(ctx
        .store
        .post_by_slug(board_id, slug)
        .await?
        .is_some_and(|p| Some(p.id.as_str()) != except))
}

async fn next_free_slug(
    ctx: &Ctx,
    board_id: &str,
    base: &str,
    except: Option<&str>,
) -> Result<String> {
    for n in 2..1000 {
        let candidate = format!("{base}-{n}");
        if !slug_used(ctx, board_id, &candidate, except).await? {
            return Ok(candidate);
        }
    }
    Ok(format!("{base}-{}", clock::new_id()))
}

pub(crate) fn revision_of(post: &Post, actor: &Actor) -> Revision {
    let (_, actor_id) = actor.audit_identity();
    Revision {
        id: clock::new_id(),
        post_id: Some(post.id.clone()),
        page_id: None,
        title: post.title.clone(),
        body_md: post.body_md.clone(),
        actor_type: post.actor_type,
        actor_id,
        created_at: post.updated_at.clone(),
    }
}

#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PreviewInput {
    pub body_md: String,
}

#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct Preview {
    /// 공개 화면과 같은 렌더러·정화를 거친 HTML
    pub html: String,
    /// 요약을 비워 두면 목록·검색·공유 미리보기에 쓰이는 본문 앞부분
    pub summary: String,
    /// 본문에 나오는 이 사이트의 사진 (`/media/...`, 나오는 차례). 대표 이미지 후보
    pub images: Vec<String>,
}

/// 저장하지 않고 본문을 공개 화면과 같은 방식으로 그린다. 글을 쓸 수 있는 사람만.
pub fn preview(_ctx: &Ctx, actor: &Actor, input: PreviewInput) -> Result<Preview> {
    actor.require(Permission::PostWrite)?;
    let body = check_body(input.body_md)?;
    let mut images = crate::markdown::image_urls(&body);
    images.retain(|u| u.starts_with("/media/"));
    images.dedup();
    Ok(Preview {
        html: crate::markdown::render(&body).into_string(),
        summary: crate::markdown::plain_text(&body, crate::markdown::SUMMARY_CHARS),
        images,
    })
}

#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SlugCheckInput {
    /// 게시판 슬러그
    pub board: String,
    /// 확인할 주소. 비우면 제목에서 만들 주소를 알려 준다
    #[serde(default)]
    pub slug: Option<String>,
    /// 주소를 비웠을 때 주소를 만들 제목
    #[serde(default)]
    pub title: Option<String>,
    /// 고치는 글의 ID. 그 글 자신의 주소는 겹친 것으로 보지 않는다
    #[serde(default)]
    pub post_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct SlugCheck {
    /// 저장될 주소 (정규화한 값)
    pub slug: String,
    /// 이 게시판에서 쓸 수 있으면 true
    pub available: bool,
    /// 겹치면 비어 있는 다음 후보
    pub suggestion: Option<String>,
    /// 주소를 비워 제목에서 만든 값이면 true
    pub derived: bool,
    /// 공개 주소의 경로 (`/b/게시판/주소`)
    pub path: String,
}

/// 글 주소가 이 게시판에서 비어 있는지 저장하기 전에 확인한다. 아무것도 바꾸지 않는다.
/// 주소를 비우면 저장할 때와 같은 규칙으로 제목에서 만든 주소를 돌려준다.
pub async fn check_slug(ctx: &Ctx, actor: &Actor, input: SlugCheckInput) -> Result<SlugCheck> {
    actor.require(Permission::PostWrite)?;
    let board = ctx
        .store
        .board_by_slug(&input.board)
        .await?
        .ok_or_else(|| boards::not_found(&input.board))?;
    actor.require_board(Permission::PostWrite, &board.id)?;
    let except = input.post_id.as_deref();
    let given = input
        .slug
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty());
    let (slug, available, suggestion, derived) = match given {
        Some(s) => {
            let s = slug::normalize(s)?;
            if slug_used(ctx, &board.id, &s, except).await? {
                let next = next_free_slug(ctx, &board.id, &s, except).await?;
                (s, false, Some(next), false)
            } else {
                (s, true, None, false)
            }
        }
        None => {
            // 저장할 때(create_draft·unique_slug)와 같은 규칙
            let base = slug::from_title(input.title.as_deref().unwrap_or("").trim(), "post");
            let s = if slug_used(ctx, &board.id, &base, except).await? {
                next_free_slug(ctx, &board.id, &base, except).await?
            } else {
                base
            };
            (s, true, None, true)
        }
    };
    let path = crate::urls::path(&crate::urls::Target::Post {
        board: &board.slug,
        post: &slug,
    });
    Ok(SlugCheck {
        slug,
        available,
        suggestion,
        derived,
        path,
    })
}

pub(crate) fn check_body(body: String) -> Result<String> {
    super::body_text(body, MAX_BODY_BYTES)
}

/// 글의 대표 이미지: 지정한 것, 없으면 본문에 처음 나오는 보관소 사진.
pub fn cover_of(post: &Post) -> Option<String> {
    post.cover.clone().or_else(|| {
        crate::markdown::image_urls(&post.body_md)
            .into_iter()
            .find(|u| u.starts_with("/media/"))
    })
}

/// 대표 이미지는 이 사이트에 올린 파일만. 빈 값은 없음(본문 첫 사진을 쓴다).
pub(crate) fn check_cover(value: &str) -> Result<Option<String>> {
    let v = value.trim();
    if v.is_empty() {
        return Ok(None);
    }
    let ok = v.len() <= MAX_COVER
        && v.starts_with("/media/")
        && !v.contains("..")
        && v.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"/._-".contains(&b));
    if !ok {
        return Err(Error::new(Code::Validation)
            .with_message("error.cover_url")
            .arg("value", v));
    }
    Ok(Some(v.to_owned()))
}

/// 예약 시각을 저장 형태(UTC)로. 빈 값은 예약 취소. 지금 이후 1년 안쪽만 받는다.
fn check_schedule(value: &str) -> Result<Option<String>> {
    let v = value.trim();
    if v.is_empty() {
        return Ok(None);
    }
    let at = clock::parse(v).ok_or_else(|| {
        Error::new(Code::Validation)
            .with_message("error.timestamp")
            .arg("value", v)
    })?;
    let now = time::OffsetDateTime::now_utc();
    if at <= now || at > now + time::Duration::days(MAX_SCHEDULE_DAYS) {
        return Err(Error::new(Code::Validation)
            .with_message("error.schedule_range")
            .arg("days", MAX_SCHEDULE_DAYS.to_string())
            .arg("value", v));
    }
    Ok(Some(clock::format(at)))
}

pub(crate) fn not_found(id: &str) -> Error {
    Error::new(Code::NotFound)
        .with_message("error.post_not_found")
        .arg("id", id)
        .next("maru post list")
}

fn slug_conflict(board: &Board, slug: &str) -> Error {
    Error::new(Code::Conflict)
        .with_message("error.slug_taken")
        .arg("slug", slug)
        .arg("board", board.slug.clone())
}

fn bad_transition(post: &Post, to: PostStatus) -> Error {
    Error::new(Code::Conflict)
        .with_message("error.post_transition")
        .arg("from", post.status.as_str())
        .arg("to", to.as_str())
}
