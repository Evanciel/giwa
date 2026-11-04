//! 위키 게시판 (GAP C13, ADR-0078). 실제 SQLite 파일로 `[[문서]]` 잇기·아직 없는 문서·역링크·
//! 외톨이 문서·필요한 문서·비밀글 제외, 위키가 아닌 게시판에서는 문법이 글자로 남는지를 본다.
#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::sync::Arc;

use maru_core::actor::Actor;
use maru_core::markdown;
use maru_core::services::boards::{self, NewBoard};
use maru_core::services::posts::{self, NewPost};
use maru_core::services::wiki;
use maru_core::{Code, Ctx};
use maru_storage::{DataDir, Db, FsBlobs, SqliteStore};

struct Env {
    _tmp: tempfile::TempDir,
    ctx: Ctx,
}

async fn env() -> Env {
    let tmp = tempfile::tempdir().unwrap();
    let dir = DataDir::new(tmp.path().join("데이터"));
    dir.ensure_layout().unwrap();
    let db = Db::open(&dir.db_path(), true).await.unwrap();
    db.migrate().await.unwrap();
    let ctx = Ctx {
        store: Arc::new(SqliteStore::new(db.clone())),
        blobs: Arc::new(FsBlobs::new(dir.media_dir())),
        site_url: "http://127.0.0.1:8080".into(),
        extensions: maru_core::store::no_extensions(),
    };
    for (slug, kind) in [("wiki", "wiki"), ("free", "free")] {
        boards::create(
            &ctx,
            &Actor::local(),
            NewBoard {
                slug: slug.into(),
                name: slug.into(),
                kind: kind.into(),
                policy: None,
                sort_order: 0,
                layout: None,
                field_schema: None,
                list_order: None,
            },
        )
        .await
        .unwrap();
    }
    Env { _tmp: tmp, ctx }
}

async fn page(e: &Env, board: &str, title: &str, slug: &str, body: &str, secret: bool) -> String {
    let p = posts::create_draft(
        &e.ctx,
        &Actor::local(),
        NewPost {
            board: board.into(),
            title: title.into(),
            slug: Some(slug.into()),
            body_md: body.into(),
            excerpt: None,
            cover: None,
            category: None,
            tags: vec![],
            notice: false,
            secret,
            meta: Default::default(),
        },
    )
    .await
    .unwrap();
    posts::publish(&e.ctx, &Actor::local(), &p.id)
        .await
        .unwrap();
    p.id
}

#[tokio::test]
async fn wiki_links_backlinks_orphans_and_wanted() {
    let e = env().await;
    let front = "[[고양이]]와 [[ 강아지 |개]], 그리고 [[없는 문서]]. 또 [[고양이]].";
    page(&e, "wiki", "대문", "front", front, false).await;
    page(&e, "wiki", "고양이", "cat", "[[대문]]으로", false).await;
    page(&e, "wiki", "강아지", "dog", "그냥 글", false).await;
    page(&e, "wiki", "외로운 문서", "alone", "", false).await;
    page(
        &e,
        "wiki",
        "숨은 문서",
        "hidden",
        "[[외로운 문서]] [[비밀 대상]]",
        true,
    )
    .await;

    let board = boards::readable(&e.ctx, &Actor::local(), "wiki")
        .await
        .unwrap();
    let links = wiki::links(&e.ctx, &board, front).await.unwrap();
    assert_eq!(links.len(), 3, "같은 제목은 한 번");
    assert_eq!(links["고양이"].href, "/b/wiki/cat");
    assert!(!links["강아지"].missing, "앞뒤 공백은 무시");
    let new = &links["없는 문서"];
    assert!(new.missing);
    assert!(new.href.starts_with("/write/wiki?title="), "{}", new.href);

    let doc = markdown::render_wiki_document(front, &|_| None, &|t| {
        links[&markdown::wiki_key(t)].clone()
    });
    let html = doc.html.as_str();
    assert!(
        html.contains("<a class=\"wikilink\" href=\"/b/wiki/cat\""),
        "{html}"
    );
    assert!(html.contains(">개</a>"), "보일 글: {html}");
    assert!(html.contains("class=\"wikilink wikilink-new\""), "{html}");
    assert!(html.contains("title=\"아직 없는 문서\""), "{html}");
    // 위키가 아닌 본문에서는 글자 그대로
    let plain = markdown::render("[[고양이]]").into_string();
    assert!(
        plain.contains("[[고양이]]") && !plain.contains("wikilink"),
        "{plain}"
    );

    let cat = e
        .ctx
        .store
        .post_by_slug(&board.id, "cat")
        .await
        .unwrap()
        .unwrap();
    let back = wiki::backlinks(&e.ctx, &board, &cat).await.unwrap();
    let titles: Vec<&str> = back.iter().map(|b| b.title.as_str()).collect();
    assert_eq!(titles, ["대문"]);

    let idx = wiki::index(&e.ctx, &Actor::local(), "wiki").await.unwrap();
    assert_eq!(idx.pages, 4, "비밀글은 문서로 세지 않는다");
    let wanted: Vec<(&str, usize)> = idx
        .wanted
        .iter()
        .map(|w| (w.title.as_str(), w.count))
        .collect();
    assert_eq!(wanted, [("없는 문서", 1)], "비밀글이 가리킨 것은 빠진다");
    let orphans: Vec<&str> = idx.orphans.iter().map(|o| o.title.as_str()).collect();
    assert_eq!(orphans, ["외로운 문서"], "비밀글의 링크는 세지 않는다");

    page(&e, "free", "자유 글", "f1", "[[고양이]]", false).await;
    let err = wiki::index(&e.ctx, &Actor::local(), "free")
        .await
        .unwrap_err();
    assert_eq!(err.code(), Code::Validation);
}
