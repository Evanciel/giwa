//! 예시 콘텐츠 (ADR-0142). 사이트 종류(프리셋)를 고르면 빈 게시판에 예시 글과 그림을 넣어
//! 첫 화면부터 다 갖춘 사이트로 보이게 하고, 운영자는 그 글을 고쳐 쓰거나 한 번에 지운다.
//!
//! - 넣은 것은 `site_settings` 의 `samples.state` 에 적는다(글 ID·넣은 시각·그림 ID).
//! - 「고치지 않은 예시」 = 넣은 뒤 수정·댓글·답변글이 없는 글. 지우기는 이것만 아주 지우고,
//!   고친 글은 운영자의 글로 남긴다(목록에서 뺀다).
//! - 한 번 넣은 게시판은 다시 넣지 않는다. 지운 뒤 프리셋을 다시 적용해도 예시가 돌아오지 않는다.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};

use crate::actor::{Actor, Permission};
use crate::audit::Target;
use crate::clock;
use crate::error::{Code, Error, Result};
use crate::sample_art;
use crate::services::{design, media, posts};
use crate::store::{Ctx, Tx};

const KEY: &str = "samples.state";
/// 예시 글 사이 시간 간격. 목록이 「방금」 한 줄로 몰리지 않게 지난 날짜로 나눠 둔다.
const SPACING_HOURS: i64 = 31;

/// 프리셋 `posts.ensure` 의 글 하나.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SamplePost {
    pub title: String,
    #[serde(default)]
    pub body_md: String,
    #[serde(default)]
    pub excerpt: Option<String>,
    /// 분류 슬러그
    #[serde(default)]
    pub category: Option<String>,
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default)]
    pub notice: bool,
    /// 여분 칸 값 (게시판 `field_schema`)
    #[serde(default)]
    pub meta: Map<String, Value>,
    /// 본문 맨 위 그림의 장면: `landscape` `still` `room` `city` `food` `desk` `people` `abstract`.
    /// 사이트 주 색으로 서버가 그린다. 비우면 그림 없음
    #[serde(default)]
    pub image: Option<String>,
    /// 그림 대체 텍스트. 비우면 「예시 그림」
    #[serde(default)]
    pub alt: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct State {
    #[serde(default)]
    boards: Vec<String>,
    #[serde(default)]
    posts: Vec<Seeded>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Seeded {
    id: String,
    board: String,
    at: String,
    #[serde(default)]
    media: Vec<String>,
}

#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct SampleStatus {
    /// 아직 예시로 남아 있는 글 수
    pub posts: usize,
    /// 그중 고치지 않아 지우기로 지워질 글 수
    pub untouched: usize,
    /// 예시를 넣은 게시판 슬러그
    pub boards: Vec<String>,
}

#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct ClearResult {
    /// 지운 예시 글 수
    pub removed: usize,
    /// 고쳐 써서 남긴 글 수 (이제 예시가 아니다)
    pub kept: usize,
    /// 지운 예시 그림 수
    pub media_removed: usize,
}

async fn load(ctx: &Ctx) -> Result<State> {
    Ok(ctx
        .store
        .settings()
        .await?
        .into_iter()
        .find(|(k, _)| k == KEY)
        .and_then(|(_, v)| serde_json::from_value(v).ok())
        .unwrap_or_default())
}

/// 게시판이 비어 있고 예시를 넣은 적이 없으면 넣는다. 넣은 글 ID 를 돌려준다(안 넣었으면 빈 목록).
pub async fn seed(
    ctx: &Ctx,
    actor: &Actor,
    board: &str,
    items: &[SamplePost],
) -> Result<Vec<String>> {
    actor.require(Permission::StructureEdit)?;
    for item in items {
        if let Some(m) = item.image.as_deref()
            && !sample_art::is_motif(m)
        {
            return Err(Error::new(Code::Validation)
                .with_message("error.preset_invalid")
                .arg("id", "posts.ensure")
                .arg(
                    "detail",
                    format!("image `{m}`: {}", sample_art::MOTIFS.join("·")),
                ));
        }
    }
    let mut state = load(ctx).await?;
    if items.is_empty() || state.boards.iter().any(|b| b == board) {
        return Ok(Vec::new());
    }
    {
        let mut tx = Tx::begin(ctx).await?;
        let b = tx
            .board_by_slug(board)
            .await?
            .ok_or_else(|| crate::services::boards::not_found(board))?;
        if tx.count_board_posts(&b.id).await? > 0 {
            return Ok(Vec::new());
        }
    }
    let primary = design::tokens(ctx).await?.color.primary;
    let mut made = Vec::with_capacity(items.len());
    for (i, item) in items.iter().enumerate() {
        let mut body = item.body_md.trim().to_owned();
        let mut media_ids = Vec::new();
        if let Some(motif) = item.image.as_deref() {
            let seed = board.bytes().fold(i as u32 + 1, |a, b| {
                a.wrapping_mul(31).wrapping_add(b as u32)
            });
            let bytes = sample_art::render(motif, seed, &primary)?;
            let alt = item.alt.clone().unwrap_or_else(|| "예시 그림".into());
            let m = media::store(
                ctx,
                actor,
                media::Upload {
                    filename: "sample.jpg".into(),
                    bytes,
                    alt_text: Some(alt.clone()),
                },
            )
            .await?;
            body = format!("![{alt}]({})\n\n{body}", m.url);
            media_ids.push(m.id);
        }
        let post = posts::create_draft(
            ctx,
            actor,
            posts::NewPost {
                board: board.to_owned(),
                title: item.title.clone(),
                slug: None,
                body_md: body,
                excerpt: item.excerpt.clone(),
                cover: None,
                category: item.category.clone(),
                tags: item.tags.clone(),
                notice: item.notice,
                secret: false,
                meta: item.meta.clone(),
            },
        )
        .await?;
        posts::publish(ctx, actor, &post.id).await?;
        made.push(Seeded {
            id: post.id.clone(),
            board: board.to_owned(),
            // 첫 글이 가장 새 글이 되게 지난 시각으로 나눈다.
            at: clock::hours_ago(SPACING_HOURS * i as i64 + 2),
            media: media_ids,
        });
    }
    let ids: Vec<String> = made.iter().map(|s| s.id.clone()).collect();
    let mut tx = Tx::begin(ctx).await?;
    for s in &made {
        if let Some(mut p) = tx.post_by_id(&s.id).await? {
            p.published_at = Some(s.at.clone());
            p.last_activity_at = Some(s.at.clone());
            p.updated_at = s.at.clone();
            tx.update_post(&p).await?;
        }
    }
    state.boards.push(board.to_owned());
    state.posts.extend(made);
    let v = serde_json::to_value(&state).map_err(Error::internal)?;
    tx.put_setting(KEY, &v, &clock::now()).await?;
    tx.audit(
        actor,
        "sample.seed",
        Target::site(),
        json!({ "board": board, "posts": ids }),
    )
    .await?;
    tx.commit().await?;
    Ok(ids)
}

/// 넣은 뒤 고치거나 댓글·답변글이 달리지 않은 글인가.
async fn untouched(tx: &mut Tx, s: &Seeded) -> Result<Option<bool>> {
    let Some(p) = tx.post_by_id(&s.id).await? else {
        return Ok(None);
    };
    let same_activity = p.last_activity_at.as_deref().is_none_or(|a| a == s.at);
    Ok(Some(
        p.updated_at == s.at && same_activity && tx.reply_count(&s.id).await? == 0,
    ))
}

pub async fn status(ctx: &Ctx, actor: &Actor) -> Result<SampleStatus> {
    actor.require(Permission::StructureEdit)?;
    let state = load(ctx).await?;
    let mut tx = Tx::begin(ctx).await?;
    let (mut posts, mut fresh) = (0, 0);
    for s in &state.posts {
        match untouched(&mut tx, s).await? {
            Some(true) => {
                posts += 1;
                fresh += 1;
            }
            Some(false) => posts += 1,
            None => {}
        }
    }
    Ok(SampleStatus {
        posts,
        untouched: fresh,
        boards: state.boards,
    })
}

/// 고치지 않은 예시 글과 그 그림을 아주 지운다. 고친 글은 남기고 예시 목록에서만 뺀다.
/// 지운 게시판에는 프리셋을 다시 적용해도 예시가 다시 들어가지 않는다.
pub async fn clear(ctx: &Ctx, actor: &Actor) -> Result<ClearResult> {
    actor.require(Permission::StructureEdit)?;
    let state = load(ctx).await?;
    let ids: Vec<String> = state.posts.iter().map(|s| s.id.clone()).collect();
    remove(ctx, actor, &ids, false).await
}

/// `ids` 중 예시 글을 지운다. `forget_boards` 면 그 게시판을 「넣은 적 없음」으로 되돌린다
/// (변경 묶음 되돌리기: 다시 적용하면 예시가 다시 들어간다).
pub(crate) async fn remove(
    ctx: &Ctx,
    actor: &Actor,
    ids: &[String],
    forget_boards: bool,
) -> Result<ClearResult> {
    actor.require(Permission::StructureEdit)?;
    let mut state = load(ctx).await?;
    let mut out = ClearResult {
        removed: 0,
        kept: 0,
        media_removed: 0,
    };
    let mut media_ids = Vec::new();
    let mut boards_touched = Vec::new();
    let mut tx = Tx::begin(ctx).await?;
    for s in state.posts.iter().filter(|s| ids.contains(&s.id)) {
        match untouched(&mut tx, s).await? {
            Some(true) => {
                tx.purge_post(&s.id).await?;
                tx.audit(
                    actor,
                    "sample.remove",
                    Target::post(&s.id),
                    json!({ "board": s.board }),
                )
                .await?;
                media_ids.extend(s.media.iter().cloned());
                out.removed += 1;
            }
            Some(false) => out.kept += 1,
            None => {}
        }
        boards_touched.push(s.board.clone());
    }
    tx.audit(
        actor,
        "sample.clear",
        Target::site(),
        json!({ "removed": out.removed, "kept": out.kept }),
    )
    .await?;
    state.posts.retain(|s| !ids.contains(&s.id));
    if forget_boards {
        state
            .boards
            .retain(|b| !boards_touched.contains(b) || state.posts.iter().any(|s| &s.board == b));
    }
    let v = serde_json::to_value(&state).map_err(Error::internal)?;
    tx.put_setting(KEY, &v, &clock::now()).await?;
    tx.commit().await?;
    for id in media_ids {
        // 예시 글은 이미 지웠다. 사람이 이 그림을 다른 곳에 썼으면 쓰는 곳이 남아 지우지 않는다 (ADR-0172).
        if media::delete(ctx, actor, &id, false).await.is_ok() {
            out.media_removed += 1;
        }
    }
    Ok(out)
}
