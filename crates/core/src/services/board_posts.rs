//! 게시판 글쓰기: 회원·비회원이 공개 화면에서 게시판 정책대로 자기 글을 올리고 고치고 지운다 (ADR-0053).
//!
//! 관리 경로(`posts::create_draft` → `publish`)와 달리 발행 권한 없이 바로 공개(또는 승인 대기)된다.
//! 게시판 정책 `write` 가 쓸 수 있는 사람을, `write_approval` 이 바로 보일지를 정한다.
//! 에이전트는 이 경로를 쓰지 않는다 — 초안으로 쓰고 발행 권한으로 올린다(원칙 4).

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::actor::AuthorType;
use crate::actor::{Actor, ActorKind, Permission};
use crate::audit::{self, Target};
use crate::clock;
use crate::error::{Code, Error, Result};
use crate::model::{Approval, Board, CaptchaMode, PageReq, Post, PostKind, PostStatus};
use crate::services::comments::{self, GuestAuthor};
use crate::services::{
    auth, captcha, categories, files, media, members, moderation, posts, required_text, settings,
    tags,
};
use crate::store::{AuditFilter, Ctx, Tx};

/// 글쓰기 폼에 내는 확인 질문은 오래 쓰는 동안 풀리지 않게 길게 둔다.
pub const CAPTCHA_MINUTES: i64 = 60;
/// 회원이 하루(24시간)에 붙일 수 있는 파일 수 (ADR-0065). `post.upload` 권한이 있으면 세지 않는다.
pub const MEMBER_UPLOADS_PER_DAY: i64 = 50;

#[derive(Debug, Clone, Default, Deserialize, JsonSchema)]
pub struct BoardWrite {
    /// 게시판 슬러그
    pub board: String,
    pub title: String,
    #[serde(default)]
    pub body_md: String,
    /// 분류 슬러그. 비우면 분류 없음
    #[serde(default)]
    pub category: Option<String>,
    #[serde(default)]
    pub tags: Vec<String>,
    /// 비밀글 (회원만)
    #[serde(default)]
    pub secret: bool,
    /// 로그인하지 않았을 때만
    #[serde(default)]
    pub guest: Option<GuestAuthor>,
    #[serde(default)]
    pub captcha: Option<captcha::Answer>,
    /// 답변글이면 위 글 id (ADR-0056)
    #[serde(default)]
    pub reply_to: Option<String>,
    /// 여분 칸 값 (ADR-0057)
    #[serde(default)]
    pub meta: serde_json::Map<String, serde_json::Value>,
}

#[derive(Debug, Clone, Default, Deserialize, JsonSchema)]
pub struct BoardEdit {
    /// 여분 칸 값. 폼은 모든 칸을 보내므로 통째로 바꾼다 (ADR-0057)
    #[serde(default)]
    pub meta: serde_json::Map<String, serde_json::Value>,
    pub title: String,
    #[serde(default)]
    pub body_md: String,
    #[serde(default)]
    pub category: Option<String>,
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default)]
    pub secret: bool,
}

/// 이 게시판에서 누가 어떻게 쓰는지 (화면이 폼을 그릴 때).
#[derive(Debug, Clone, serde::Serialize, JsonSchema)]
pub struct WriteRules {
    /// 손님도 쓸 수 있다
    pub guest: bool,
    /// 로그인한 회원이 쓸 수 있는 최소 역할
    pub role: String,
    /// 비회원 글은 승인 뒤 보인다
    pub guest_review: bool,
    /// 회원 글도 승인 뒤 보인다
    pub member_review: bool,
    /// 비회원은 확인 질문을 푼다
    pub captcha: bool,
}

pub fn rules(board: &Board, mode: CaptchaMode) -> WriteRules {
    WriteRules {
        guest: board.policy.write == "guest",
        role: board.policy.write.clone(),
        guest_review: board.policy.write_approval != Approval::Off,
        member_review: board.policy.write_approval == Approval::All,
        captcha: mode == CaptchaMode::Guest,
    }
}

/// 고치기·지우기 화면을 열 수 있는지.
#[derive(Debug, Clone)]
pub struct Editable {
    pub post: Post,
    pub board: Board,
    /// 손님 글이라 비밀번호를 받아야 한다
    pub by_password: bool,
}

/// 공개 화면 답변 폼을 열기 전에: 위 글을 찾고 이 사람이 답변을 달 수 있는지 본다 (ADR-0056).
/// 글쓰기 역할 검사는 `write` 와 같이 화면 쪽 `may_write` 가 먼저 한다.
pub async fn reply_target(ctx: &Ctx, actor: &Actor, board: &Board, id: &str) -> Result<Post> {
    let parent = ctx
        .store
        .post_by_id(id)
        .await?
        .filter(|p| p.board_id == board.id && p.status == PostStatus::Published)
        .filter(|p| !p.is_secret || posts::can_read_secret(actor, board, p))
        .ok_or_else(|| posts::not_found(id))?;
    if board.policy.reply == "none" {
        return Err(Error::new(Code::Forbidden).with_message("error.reply_closed"));
    }
    actor.require_rank(Permission::PostWrite, &board.policy.reply)?;
    if parent.depth() >= posts::MAX_REPLY_DEPTH {
        return Err(Error::new(Code::Validation)
            .with_message("error.reply_depth")
            .arg("max", posts::MAX_REPLY_DEPTH.to_string()));
    }
    Ok(parent)
}

pub async fn write(ctx: &Ctx, actor: &Actor, input: BoardWrite) -> Result<Post> {
    let board = ctx
        .store
        .board_by_slug(&input.board)
        .await?
        .ok_or_else(|| crate::services::boards::not_found(&input.board))?;
    let site = settings::site(ctx).await?;
    let rules = rules(&board, site.captcha);
    let title = required_text("title", &input.title, posts::MAX_TITLE)?;
    let body_md = posts::check_body(input.body_md)?;
    moderation::check_text(&site, actor, &[&title, &body_md])?;
    moderation::check_flood(ctx, &site, actor).await?;

    let (author_id, guest_name, guest_pw_hash) = match actor.kind() {
        ActorKind::Anonymous => {
            if !rules.guest {
                return Err(actor.denied(Permission::PostWrite));
            }
            if input.secret {
                return Err(Error::new(Code::Validation).with_message("error.guest_secret"));
            }
            let guest = input
                .guest
                .as_ref()
                .ok_or_else(|| Error::new(Code::Validation).with_message("error.guest_required"))?;
            let name = members::check_name(&guest.name)?;
            let pw = comments::check_guest_password(&guest.password)?;
            if rules.captcha {
                captcha::check(ctx, &input.captcha.clone().unwrap_or_default()).await?;
            }
            (None, Some(name), Some(auth::hash_raw(pw)?))
        }
        ActorKind::Human { member_id } => {
            actor.require(Permission::PostWrite)?;
            actor.require_board(Permission::PostWrite, &board.id)?;
            actor.require_rank(Permission::PostWrite, &board.policy.write)?;
            (Some(member_id.clone()), None, None)
        }
        ActorKind::Local => (None, None, None),
        ActorKind::Agent { .. } => {
            return Err(Error::new(Code::Forbidden)
                .with_message("error.post_agent_board")
                .next(format!(
                    "maru post new --board {} --title <제목>",
                    board.slug
                )));
        }
    };
    let review = match board.policy.write_approval {
        Approval::Off => false,
        Approval::Guest => guest_name.is_some(),
        Approval::All => !comments::can_moderate(actor, &board),
    };

    let mut tx = Tx::begin(ctx).await?;
    let slug = posts::unique_slug(
        &mut *tx,
        &board.id,
        &crate::slug::from_title(&title, "post"),
        None,
    )
    .await?;
    let category_id = match input.category.as_deref().filter(|c| !c.is_empty()) {
        Some(c) => categories::resolve(&mut *tx, &board, c)
            .await?
            .map(|c| c.id),
        None => None,
    };
    let tags = tags::normalize(&mut *tx, &input.tags).await?;
    let spot = match input.reply_to.as_deref() {
        Some(id) => Some(posts::reply_spot(&mut *tx, actor, &board, id).await?),
        None => None,
    };
    // 답변은 필수 칸을 묻지 않는다 (posts::reply 와 같다).
    let meta = crate::fields::validate(&board.field_schema, &input.meta, spot.is_none())?;
    let now = clock::now();
    let mut post = Post {
        id: clock::new_id(),
        board_id: board.id.clone(),
        kind: PostKind::Post,
        status: if review {
            PostStatus::Pending
        } else {
            PostStatus::Published
        },
        title,
        slug,
        body_md,
        excerpt: None,
        author_id,
        actor_type: actor.author_type().unwrap_or(AuthorType::Human),
        published_at: (!review).then(|| now.clone()),
        created_at: now.clone(),
        updated_at: now,
        scheduled_at: None,
        cover: None,
        category_id,
        is_notice: false,
        is_secret: input.secret,
        tags,
        view_count: 0,
        guest_name,
        guest_pw_hash,
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
    if let (Some(root), PostStatus::Published) = (&post.thread_id, post.status) {
        tx.touch_post(root, &post.created_at).await?;
    }
    tx.insert_revision(&posts::revision_of(&post, actor))
        .await?;
    tx.audit(
        actor,
        "post.write",
        Target::post(&post.id),
        audit::diff(None, Some(&post)),
    )
    .await?;
    tx.commit().await?;
    Ok(post)
}

/// 게시판·글 주소로 찾는다 (공개 화면의 `/b/{board}/{post}/edit`).
pub async fn editable_at(ctx: &Ctx, actor: &Actor, board: &str, post: &str) -> Result<Editable> {
    let b = ctx
        .store
        .board_by_slug(board)
        .await?
        .ok_or_else(|| crate::services::boards::not_found(board))?;
    let p = ctx
        .store
        .post_by_slug(&b.id, post)
        .await?
        .ok_or_else(|| posts::not_found(post))?;
    editable(ctx, actor, &p.id).await
}

/// 고치기·지우기 화면을 열기 전에. 볼 수 없는 글은 없는 글처럼.
pub async fn editable(ctx: &Ctx, actor: &Actor, id: &str) -> Result<Editable> {
    let post = ctx
        .store
        .post_by_id(id)
        .await?
        .filter(|p| p.status != PostStatus::Deleted)
        .ok_or_else(|| posts::not_found(id))?;
    let board = ctx
        .store
        .board_by_id(&post.board_id)
        .await?
        .ok_or_else(|| posts::not_found(id))?;
    posts::check_unlocked(actor, &board, &post)?;
    if post.guest_name.is_some() && !comments::can_moderate(actor, &board) {
        return Ok(Editable {
            post,
            board,
            by_password: true,
        });
    }
    posts::authorize_post(actor, &board, &post, Permission::PostWrite)?;
    Ok(Editable {
        post,
        board,
        by_password: false,
    })
}

pub async fn edit(
    ctx: &Ctx,
    actor: &Actor,
    id: &str,
    input: BoardEdit,
    guest_password: Option<&str>,
) -> Result<Post> {
    let e = editable(ctx, actor, id).await?;
    if e.by_password {
        check_password(&e.post, guest_password)?;
    }
    let before = e.post;
    if input.secret && before.guest_name.is_some() {
        return Err(Error::new(Code::Validation).with_message("error.guest_secret"));
    }
    let mut tx = Tx::begin(ctx).await?;
    let mut after = before.clone();
    after.title = required_text("title", &input.title, posts::MAX_TITLE)?;
    after.body_md = posts::check_body(input.body_md)?;
    moderation::check_text(
        &settings::site(ctx).await?,
        actor,
        &[&after.title, &after.body_md],
    )?;
    after.category_id = match input.category.as_deref().filter(|c| !c.is_empty()) {
        Some(c) => categories::resolve(&mut *tx, &e.board, c)
            .await?
            .map(|c| c.id),
        None => None,
    };
    after.tags = tags::normalize(&mut *tx, &input.tags).await?;
    after.is_secret = input.secret;
    after.meta = crate::fields::validate(&e.board.field_schema, &input.meta, true)?;
    if after == before {
        return Ok(before);
    }
    after.updated_at = clock::now();
    tx.update_post(&after).await?;
    if after.title != before.title || after.body_md != before.body_md {
        tx.insert_revision(&posts::revision_of(&after, actor))
            .await?;
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

/// 되돌릴 수 있는 삭제 (`posts::delete` 와 같은 상태). 손님은 비밀번호로.
pub async fn remove(
    ctx: &Ctx,
    actor: &Actor,
    id: &str,
    guest_password: Option<&str>,
) -> Result<Post> {
    let e = editable(ctx, actor, id).await?;
    if !e.by_password {
        return posts::delete(ctx, actor, id).await;
    }
    check_password(&e.post, guest_password)?;
    let before = e.post;
    let after = Post {
        status: PostStatus::Deleted,
        updated_at: clock::now(),
        scheduled_at: None,
        ..before.clone()
    };
    let mut tx = Tx::begin(ctx).await?;
    tx.update_post(&after).await?;
    tx.audit(
        actor,
        "post.delete",
        Target::post(&after.id),
        audit::diff(Some(&before), Some(&after)),
    )
    .await?;
    tx.commit().await?;
    Ok(after)
}

fn check_password(post: &Post, given: Option<&str>) -> Result<()> {
    match (given, &post.guest_pw_hash) {
        (Some(pw), Some(hash)) if auth::verify(pw, hash) => Ok(()),
        _ => Err(Error::new(Code::Forbidden).with_message("error.post_password")),
    }
}

/// 공개 글쓰기 화면에서 붙인 결과.
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct Attached {
    pub url: String,
    pub filename: String,
    pub size: i64,
    pub image: bool,
    /// 본문에 넣을 마크다운
    pub markdown: String,
}

/// 이 사람이 이 게시판의 글쓰기 화면에서 파일을 붙일 수 있나. 손님·에이전트는 안 된다.
pub fn may_upload(actor: &Actor, board: &Board) -> bool {
    matches!(actor.kind(), ActorKind::Human { .. })
        && actor
            .require_board(Permission::PostUpload, &board.id)
            .is_ok()
        && actor
            .require_rank(Permission::PostUpload, &board.policy.upload)
            .is_ok()
}

/// 공개 글쓰기 화면에서 파일 하나를 붙인다 (GAP M4, ADR-0065). 그림은 다시 인코딩하고,
/// 문서는 확장자·서명 검사를 지난다(관리 경로와 같은 보관). 글과 묶지 않고 마크다운만 돌려준다.
pub async fn attach(
    ctx: &Ctx,
    actor: &Actor,
    board: &str,
    filename: String,
    bytes: Vec<u8>,
) -> Result<Attached> {
    let board = ctx
        .store
        .board_by_slug(board)
        .await?
        .ok_or_else(|| crate::services::boards::not_found(board))?;
    if !may_upload(actor, &board) {
        return Err(actor.denied(Permission::PostUpload));
    }
    if !actor.has(Permission::PostUpload) {
        check_upload_quota(ctx, actor).await?;
    }
    if media::is_image(&filename) {
        let label = link_label(filename.rsplit_once('.').map_or("", |(stem, _)| stem));
        let m = media::store(
            ctx,
            actor,
            media::Upload {
                filename: filename.clone(),
                bytes,
                alt_text: None,
            },
        )
        .await?;
        Ok(Attached {
            markdown: format!("![{label}]({})", m.url),
            url: m.url,
            filename,
            size: m.size,
            image: true,
        })
    } else {
        let f = files::store(ctx, actor, files::Upload { filename, bytes }).await?;
        let label = link_label(&f.filename);
        Ok(Attached {
            markdown: format!("[{label} ({})]({})", size_label(f.size), f.url),
            url: f.url,
            filename: f.filename,
            size: f.size,
            image: false,
        })
    }
}

/// 지난 24시간에 이 회원이 올린 수를 감사 기록으로 센다.
async fn check_upload_quota(ctx: &Ctx, actor: &Actor) -> Result<()> {
    let Some(id) = actor.member_id() else {
        return Ok(());
    };
    let since = clock::hours_ago(24);
    let mut used = 0;
    for action in ["file.upload", "media.upload"] {
        let filter = AuditFilter {
            actor_type: String::new(),
            actor_id: id.clone(),
            action: action.to_owned(),
            target_type: String::new(),
            target_id: String::new(),
            since: since.clone(),
            until: String::new(),
        };
        used += ctx
            .store
            .list_audit(&filter, PageReq::new(1, 1))
            .await?
            .total;
    }
    if used >= MEMBER_UPLOADS_PER_DAY {
        return Err(Error::new(Code::RateLimited)
            .with_message("error.upload_quota")
            .arg("max", MEMBER_UPLOADS_PER_DAY.to_string()));
    }
    Ok(())
}

/// 마크다운 링크 글자에서 문법을 깨는 글자를 뺀다.
fn link_label(name: &str) -> String {
    name.chars()
        .filter(|c| {
            !matches!(
                c,
                '[' | ']' | '(' | ')' | '\\' | '`' | '*' | '_' | '!' | '<' | '>'
            )
        })
        .filter(|c| !c.is_control())
        .collect::<String>()
        .trim()
        .to_owned()
}

fn size_label(n: i64) -> String {
    let f = n as f64;
    if f < 1024.0 * 1024.0 {
        format!("{:.0}KB", (f / 1024.0).max(1.0))
    } else {
        format!("{:.1}MB", f / 1024.0 / 1024.0)
    }
}
