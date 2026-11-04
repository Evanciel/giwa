//! 위키 게시판 (GAP C13, ADR-0078). 게시판 종류 `wiki` 의 공개된 맨 위 글이 문서다.
//!
//! - 본문의 `[[문서]]`·`[[문서|보일 글]]` 은 같은 게시판에서 제목이 같은(앞뒤 공백·밑줄·대소문자 무시) 문서로 잇는다.
//!   없으면 「아직 없는 문서」 링크 — 누르면 제목을 채운 글쓰기 화면이다.
//! - 역링크(이 문서를 가리키는 문서), 외톨이 문서(아무도 가리키지 않음), 필요한 문서(가리키지만 없음)는
//!   게시판의 공개 문서를 [`MAX_PAGES`] 개까지 읽어 그 자리에서 센다. 작은 위키를 위한 방식이다.

use std::collections::{HashMap, HashSet};

use schemars::JsonSchema;
use serde::Serialize;

use crate::actor::Actor;
use crate::error::{Code, Error, Result};
use crate::markdown::{WikiHref, wiki_key, wiki_targets};
use crate::model::{Board, Post};
use crate::services::boards;
use crate::store::{Ctx, WikiPage};
use crate::urls::{self, Target};

pub const MAX_PAGES: i64 = 2000;
/// 색인 목록 하나에 보일 최대 수.
pub const MAX_LIST: usize = 50;

#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct PageRef {
    pub title: String,
    pub url: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct Wanted {
    pub title: String,
    /// 이 제목을 가리키는 문서 수
    pub count: usize,
    pub create_url: String,
}

#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct WikiIndex {
    pub board: String,
    pub pages: usize,
    /// 다른 문서가 가리키지 않는 문서, 제목 순
    pub orphans: Vec<PageRef>,
    /// 가리키지만 아직 없는 문서, 많이 가리킨 순
    pub wanted: Vec<Wanted>,
}

/// 제목을 채운 글쓰기 화면.
pub fn create_url(board: &Board, title: &str) -> String {
    urls::with_query(&format!("/write/{}", board.slug), &[("title", title)])
}

fn page_ref(board: &Board, p: &WikiPage) -> PageRef {
    PageRef {
        title: p.title.clone(),
        url: urls::path(&Target::Post {
            board: &board.slug,
            post: &p.slug,
        }),
        updated_at: p.updated_at.clone(),
    }
}

fn require_wiki(board: &Board) -> Result<()> {
    if board.kind == "wiki" {
        Ok(())
    } else {
        Err(Error::new(Code::Validation)
            .with_message("error.not_wiki")
            .arg("board", board.slug.clone()))
    }
}

/// 본문의 위키 링크마다 갈 곳. 열쇠는 [`wiki_key`].
/// `board` 는 부르는 쪽이 읽기 권한을 확인한 위키 게시판이다. 공개 문서의 제목·주소만 돌려준다.
pub async fn links(ctx: &Ctx, board: &Board, md: &str) -> Result<HashMap<String, WikiHref>> {
    let targets = wiki_targets(md);
    if targets.is_empty() {
        return Ok(HashMap::new());
    }
    let pages = ctx.store.wiki_pages(&board.id, MAX_PAGES).await?;
    let mut by_key: HashMap<String, &WikiPage> = HashMap::new();
    for p in &pages {
        by_key.entry(wiki_key(&p.title)).or_insert(p);
    }
    Ok(targets
        .into_iter()
        .map(|t| {
            let key = wiki_key(&t);
            let href = match by_key.get(&key) {
                Some(p) => WikiHref {
                    href: page_ref(board, p).url,
                    missing: false,
                },
                None => WikiHref {
                    href: create_url(board, &t),
                    missing: true,
                },
            };
            (key, href)
        })
        .collect())
}

/// 이 문서를 가리키는 다른 공개 문서, 제목 순. `board` 는 [`links`] 와 같다.
pub async fn backlinks(ctx: &Ctx, board: &Board, post: &Post) -> Result<Vec<PageRef>> {
    let key = wiki_key(&post.title);
    let pages = ctx.store.wiki_pages(&board.id, MAX_PAGES).await?;
    let mut out: Vec<PageRef> = pages
        .iter()
        .filter(|p| p.id != post.id && p.body_md.contains("[["))
        .filter(|p| wiki_targets(&p.body_md).iter().any(|t| wiki_key(t) == key))
        .map(|p| page_ref(board, p))
        .collect();
    out.sort_by(|a, b| a.title.cmp(&b.title));
    out.truncate(MAX_LIST);
    Ok(out)
}

/// 위키 색인: 외톨이 문서와 필요한 문서 (REST·CLI).
pub async fn index(ctx: &Ctx, actor: &Actor, board_slug: &str) -> Result<WikiIndex> {
    let board = boards::readable(ctx, actor, board_slug).await?;
    require_wiki(&board)?;
    index_of(ctx, &board).await
}

/// [`index`] 의 계산. `board` 는 [`links`] 와 같다.
pub async fn index_of(ctx: &Ctx, board: &Board) -> Result<WikiIndex> {
    let pages = ctx.store.wiki_pages(&board.id, MAX_PAGES).await?;
    let keys: HashSet<String> = pages.iter().map(|p| wiki_key(&p.title)).collect();
    let mut linked: HashSet<String> = HashSet::new();
    let mut wanted: Vec<Wanted> = Vec::new();
    let mut wanted_at: HashMap<String, usize> = HashMap::new();
    for p in pages.iter().filter(|p| p.body_md.contains("[[")) {
        let own = wiki_key(&p.title);
        for t in wiki_targets(&p.body_md) {
            let k = wiki_key(&t);
            if k == own {
                continue;
            }
            if keys.contains(&k) {
                linked.insert(k);
            } else if let Some(&i) = wanted_at.get(&k) {
                wanted[i].count += 1;
            } else {
                wanted_at.insert(k, wanted.len());
                wanted.push(Wanted {
                    create_url: create_url(board, &t),
                    title: t,
                    count: 1,
                });
            }
        }
    }
    let mut orphans: Vec<PageRef> = pages
        .iter()
        .filter(|p| !linked.contains(&wiki_key(&p.title)))
        .map(|p| page_ref(board, p))
        .collect();
    orphans.sort_by(|a, b| a.title.cmp(&b.title));
    orphans.truncate(MAX_LIST);
    wanted.sort_by(|a, b| b.count.cmp(&a.count).then_with(|| a.title.cmp(&b.title)));
    wanted.truncate(MAX_LIST);
    Ok(WikiIndex {
        board: board.slug.clone(),
        pages: pages.len(),
        orphans,
        wanted,
    })
}
