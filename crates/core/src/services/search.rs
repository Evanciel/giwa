//! 사이트 검색 (GAP C5, ADR-0050).
//!
//! - 손님 기준 공개 검색: 공개된 글(비밀글 제외, 읽을 수 있는 게시판)과 공개 페이지.
//!   관리 화면의 검색은 `posts::list` 의 `search` 로 한다(초안·비밀글 포함, 권한 안에서).
//! - 읽기라 감사 기록이 없다. 검색어도 남기지 않는다.
//! - 색인 다시 만들기([`reindex`])는 사이트 관리 권한이고 감사에 남는다.

use std::collections::HashMap;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::actor::{Actor, Permission};
use crate::audit::{self, Target as AuditTarget};
use crate::error::{Code, Error, Result};
use crate::markdown;
use crate::model::{Board, PageReq};
use crate::search::{Query, Range, SNIPPET_CHARS, Segment, snippet};
use crate::store::{Ctx, SearchQuery, SearchRef, Tx};
use crate::urls::{self, Target};

use super::boards;

/// 한 쪽에 보이는 결과 수.
pub const PER_PAGE: u32 = 20;

/// 결과 순서.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Sort {
    /// 정확도 (제목 ×5, 태그 ×3, 본문 ×1). 같으면 최신.
    #[default]
    Rank,
    /// 최신 발행 순.
    New,
}

impl Sort {
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "" | "rank" => Some(Self::Rank),
            "new" | "newest" => Some(Self::New),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Rank => "rank",
            Self::New => "new",
        }
    }
}

fn first_page() -> u32 {
    1
}

/// 검색 입력 (REST 쿼리·CLI 인자·화면 폼 공통).
#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub struct SearchInput {
    /// 검색어. 띄어 쓰면 모두, `OR` 는 어느 하나, `"구"` 는 붙은 그대로, `-낱말` 은 빼기.
    pub q: String,
    /// 이 게시판 안에서만 (게시판 주소).
    #[serde(default)]
    pub board: Option<String>,
    /// 찾을 곳: all(제목·내용·태그) · title · body · tag
    #[serde(default, rename = "in")]
    pub range: Range,
    #[serde(default)]
    pub sort: Sort,
    #[serde(default = "first_page")]
    pub page: u32,
}

/// 문자열로 받은 찾을 곳·순서 (REST 쿼리·CLI 인자). 빈 값은 기본값, 모르는 값은 검증 오류.
pub fn parse_options(range: &str, sort: &str) -> Result<(Range, Sort)> {
    let bad = |name: &str, value: &str, allowed: &str| {
        Error::new(Code::Validation)
            .with_message("error.search_option")
            .arg("name", name)
            .arg("value", value)
            .arg("allowed", allowed)
    };
    let r = Range::parse(range.trim()).ok_or_else(|| bad("in", range, "all, title, body, tag"))?;
    let s = Sort::parse(sort.trim()).ok_or_else(|| bad("sort", sort, "rank, new"))?;
    Ok((r, s))
}

impl SearchInput {
    pub fn new(q: impl Into<String>) -> Self {
        Self {
            q: q.into(),
            board: None,
            range: Range::All,
            sort: Sort::Rank,
            page: 1,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum HitKind {
    Post,
    Page,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
pub struct HitBoard {
    pub slug: String,
    pub name: String,
}

/// 검색 결과 한 건.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
pub struct Hit {
    pub kind: HitKind,
    pub id: String,
    pub title: String,
    /// 사이트 안 주소 (`/b/{board}/{slug}`, `/p/{slug}`)
    pub url: String,
    /// 글이면 게시판
    pub board: Option<HitBoard>,
    pub published_at: Option<String>,
    pub tags: Vec<String>,
    /// 대표 이미지 (`/media/...`)
    pub cover: Option<String>,
    /// 검색어 둘레 본문 한 토막. `hit` 조각이 찾은 말이다.
    pub snippet: Vec<Segment>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
pub struct SearchResult {
    pub q: String,
    #[serde(rename = "in")]
    pub range: Range,
    pub sort: Sort,
    pub board: Option<String>,
    pub items: Vec<Hit>,
    pub total: i64,
    pub page: u32,
    pub per_page: u32,
}

impl SearchResult {
    pub fn total_pages(&self) -> i64 {
        (self.total + i64::from(self.per_page) - 1) / i64::from(self.per_page)
    }
}

/// 손님(또는 `actor`) 이 볼 수 있는 글·페이지에서 찾는다.
pub async fn search(ctx: &Ctx, actor: &Actor, input: SearchInput) -> Result<SearchResult> {
    let query = Query::parse(&input.q)?;
    let board = match input
        .board
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        Some(slug) => Some(boards::readable(ctx, actor, slug).await?),
        None => None,
    };
    let page = PageReq::new(input.page, PER_PAGE);
    let q = SearchQuery {
        fts: query.to_fts(input.range),
        board_id: board.as_ref().map(|b| b.id.clone()),
        // 글 읽기 권한이 없으면 어떤 게시판도 걸리지 않게 한다.
        max_read_rank: if actor.has(Permission::PostRead) {
            actor.rank()
        } else {
            i64::MIN
        },
        pages: actor.has(Permission::SiteView) && input.range != Range::Tag,
        newest: input.sort == Sort::New,
        limit: page.limit(),
        offset: page.offset(),
    };
    let (refs, total) = ctx.store.search(&q).await?;
    let mut boards_by_id: HashMap<String, Board> = HashMap::new();
    if let Some(b) = &board {
        boards_by_id.insert(b.id.clone(), b.clone());
    }
    let mut items = Vec::with_capacity(refs.len());
    for r in refs {
        match r {
            SearchRef::Post(id) => {
                // 저장소가 이미 거른 것을 한 번 더 확인한다 (비밀글이 새지 않게).
                let Some(p) = ctx.store.post_by_id(&id).await? else {
                    continue;
                };
                if !p.is_public() || p.is_secret {
                    continue;
                }
                if !boards_by_id.contains_key(&p.board_id) {
                    let Some(b) = ctx.store.board_by_id(&p.board_id).await? else {
                        continue;
                    };
                    boards_by_id.insert(b.id.clone(), b);
                }
                let Some(b) = boards_by_id.get(&p.board_id) else {
                    continue;
                };
                let plain = markdown::plain_text(&p.body_md, usize::MAX);
                items.push(Hit {
                    kind: HitKind::Post,
                    url: urls::path(&Target::Post {
                        board: &b.slug,
                        post: &p.slug,
                    }),
                    board: Some(HitBoard {
                        slug: b.slug.clone(),
                        name: b.name.clone(),
                    }),
                    snippet: snippet(&plain, &query, SNIPPET_CHARS),
                    id: p.id,
                    title: p.title,
                    published_at: p.published_at,
                    tags: p.tags,
                    cover: p.cover,
                });
            }
            SearchRef::Page(id) => {
                let Some(p) = ctx.store.page_by_id(&id).await? else {
                    continue;
                };
                if p.status != crate::model::PageStatus::Published {
                    continue;
                }
                let plain = markdown::plain_text(&p.body_md, usize::MAX);
                items.push(Hit {
                    kind: HitKind::Page,
                    url: urls::path(&Target::Page { page: &p.slug }),
                    board: None,
                    snippet: snippet(&plain, &query, SNIPPET_CHARS),
                    id: p.id,
                    title: p.title,
                    published_at: p.published_at,
                    tags: vec![],
                    cover: None,
                });
            }
        }
    }
    Ok(SearchResult {
        q: input.q.trim().to_owned(),
        range: input.range,
        sort: input.sort,
        board: board.map(|b| b.slug),
        items,
        total,
        page: page.page,
        per_page: page.per_page,
    })
}

/// 다시 만든 색인 크기.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, JsonSchema)]
pub struct Reindexed {
    pub posts: i64,
    pub pages: i64,
}

/// 검색 색인을 처음부터 다시 만든다. 사이트 관리 권한, 감사 `search.reindex`. 여러 번 해도 결과가 같다.
pub async fn reindex(ctx: &Ctx, actor: &Actor) -> Result<Reindexed> {
    actor.require(Permission::SiteAdmin)?;
    let mut tx = Tx::begin(ctx).await?;
    let (posts, pages) = tx.rebuild_search().await?;
    let done = Reindexed { posts, pages };
    tx.audit(
        actor,
        "search.reindex",
        AuditTarget::site(),
        audit::diff(None, Some(&json!(done))),
    )
    .await?;
    tx.commit().await?;
    Ok(done)
}
