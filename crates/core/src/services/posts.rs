//! 글 서비스. 공개 조회는 `public_*` 함수만 쓴다 (PHASE1_PLAN §4.6).

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::actor::{Actor, Permission};
use crate::audit::{self, Target};
use crate::clock;
use crate::error::{Code, Error, Result};
use crate::model::{Board, Listing, PageReq, Post, PostKind, PostStatus, Revision};
use crate::services::{boards, required_text};
use crate::slug;
use crate::store::{Ctx, PublishedQuery, Tx, WriteTx};

const MAX_TITLE: usize = 200;
const MAX_BODY_BYTES: usize = 1024 * 1024;

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
}

pub async fn create_draft(ctx: &Ctx, actor: &Actor, input: NewPost) -> Result<Post> {
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

    let now = clock::now();
    let post = Post {
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
    };
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
    let mut tx = Tx::begin(ctx).await?;
    let (board, before) = load_for_write(&mut *tx, actor, id).await?;
    authorize_post(actor, &board, &before, Permission::PostPublish)?;
    if before.status == PostStatus::Published {
        return Ok(before);
    }
    if !matches!(
        before.status,
        PostStatus::Draft | PostStatus::Review | PostStatus::Hidden
    ) {
        return Err(bad_transition(&before, PostStatus::Published));
    }
    let now = clock::now();
    let after = Post {
        status: PostStatus::Published,
        published_at: before.published_at.clone().or_else(|| Some(now.clone())),
        updated_at: now,
        ..before.clone()
    };
    tx.update_post(&after).await?;
    tx.audit(
        actor,
        "post.publish",
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
        return Ok(post);
    }
    // 초안은 존재를 드러내지 않는다.
    authorize_post(actor, &board, &post, Permission::PostWrite).map_err(|_| not_found(id))?;
    Ok(post)
}

/// 공개 글 하나. 홈·목록·RSS·사이트맵과 같은 공개 기준을 쓴다.
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
    Ok((board, post))
}

/// 공개 글 목록. `board` 가 없으면 읽을 수 있는 모든 게시판.
pub async fn public_posts(
    ctx: &Ctx,
    actor: &Actor,
    board: Option<&Board>,
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
        max_read_rank: actor.rank(),
        limit: page.limit(),
        offset: page.offset(),
    };
    let total = ctx.store.count_published(&q).await?;
    let items = ctx.store.list_published(&q).await?;
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
fn is_own(actor: &Actor, post: &Post) -> bool {
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

async fn unique_slug(
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

fn revision_of(post: &Post, actor: &Actor) -> Revision {
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

fn check_body(body: String) -> Result<String> {
    if body.len() > MAX_BODY_BYTES {
        return Err(Error::new(Code::Validation)
            .with_message("error.body_too_large")
            .arg("max", MAX_BODY_BYTES.to_string()));
    }
    Ok(body.trim_start_matches('\u{feff}').to_owned())
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
