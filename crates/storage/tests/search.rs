//! 사이트 검색 (GAP C5, ADR-0050). 실제 SQLite 파일로 색인·공개 범위·순위·다시 만들기를 검증한다.
#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::sync::Arc;

use maru_core::actor::{Actor, Scope};
use maru_core::model::{BoardPolicy, PageReq, Post};
use maru_core::search::Range;
use maru_core::services::anonymous_actor;
use maru_core::services::boards::{self, NewBoard};
use maru_core::services::pages::{self, NewPage};
use maru_core::services::posts::{self, NewPost, PostPatch};
use maru_core::services::search::{self, HitKind, SearchInput, Sort};
use maru_core::{Code, Ctx};
use maru_storage::{DataDir, Db, FsBlobs, SqliteStore};

struct Env {
    _tmp: tempfile::TempDir,
    db: Db,
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
    };
    for (slug, read) in [("blog", None), ("notes", None), ("members", Some("member"))] {
        boards::create(
            &ctx,
            &Actor::local(),
            NewBoard {
                slug: slug.into(),
                name: format!("{slug} 게시판"),
                kind: "blog".into(),
                policy: read.map(|r: &str| BoardPolicy {
                    read: r.into(),
                    ..Default::default()
                }),
                sort_order: 0,
            },
        )
        .await
        .unwrap();
    }
    Env { _tmp: tmp, db, ctx }
}

fn new_post(board: &str, title: &str, body: &str, tags: &[&str]) -> NewPost {
    NewPost {
        board: board.into(),
        title: title.into(),
        slug: None,
        body_md: body.into(),
        excerpt: None,
        cover: None,
        category: None,
        tags: tags.iter().map(|s| (*s).to_owned()).collect(),
        notice: false,
        secret: false,
    }
}

async fn published(ctx: &Ctx, input: NewPost) -> Post {
    let local = Actor::local();
    let p = posts::create_draft(ctx, &local, input).await.unwrap();
    posts::publish(ctx, &local, &p.id).await.unwrap()
}

async fn titles(ctx: &Ctx, input: SearchInput) -> Vec<String> {
    let anon = anonymous_actor(ctx).await.unwrap();
    search::search(ctx, &anon, input)
        .await
        .unwrap()
        .items
        .into_iter()
        .map(|h| h.title)
        .collect()
}

async fn find(ctx: &Ctx, q: &str) -> Vec<String> {
    titles(ctx, SearchInput::new(q)).await
}

#[tokio::test]
async fn hangul_words_are_found_inside_longer_words() {
    let e = env().await;
    published(
        &e.ctx,
        new_post("blog", "여행 기록", "**제주도에서** 올레길을 걸었다.", &[]),
    )
    .await;
    published(
        &e.ctx,
        new_post("blog", "부산 국밥", "돼지국밥 세 곳", &["맛집"]),
    )
    .await;

    assert_eq!(find(&e.ctx, "제주").await, ["여행 기록"]);
    assert_eq!(find(&e.ctx, "제주도").await, ["여행 기록"]);
    assert_eq!(find(&e.ctx, "올레길").await, ["여행 기록"]);
    assert_eq!(find(&e.ctx, "길").await, ["여행 기록"]);
    assert_eq!(find(&e.ctx, "국밥").await, ["부산 국밥"]);
    assert_eq!(find(&e.ctx, "맛집").await, ["부산 국밥"], "태그");
    assert!(find(&e.ctx, "서울").await.is_empty());
    // 문법: AND · OR · 구 · 빼기
    assert!(find(&e.ctx, "제주 국밥").await.is_empty());
    let mut both = find(&e.ctx, "제주 OR 국밥").await;
    both.sort();
    assert_eq!(both, ["부산 국밥", "여행 기록"]);
    assert_eq!(find(&e.ctx, "\"올레길을 걸었다\"").await, ["여행 기록"]);
    assert!(find(&e.ctx, "\"걸었다 올레길\"").await.is_empty());
    assert_eq!(find(&e.ctx, "국밥 -부산").await, Vec::<String>::new());
    assert_eq!(find(&e.ctx, "OR 제주 -부산").await, ["여행 기록"]);

    // 본문 토막: 마크다운을 걷어낸 글자에서 찾은 말을 표시한다.
    let anon = anonymous_actor(&e.ctx).await.unwrap();
    let r = search::search(&e.ctx, &anon, SearchInput::new("제주"))
        .await
        .unwrap();
    let hit = &r.items[0];
    assert_eq!(hit.kind, HitKind::Post);
    assert_eq!(hit.url, "/b/blog/%EC%97%AC%ED%96%89-%EA%B8%B0%EB%A1%9D");
    assert_eq!(hit.board.as_ref().unwrap().slug, "blog");
    assert!(hit.snippet.iter().any(|s| s.hit && s.text == "제주"));
    assert!(hit.snippet.iter().all(|s| !s.text.contains("**")));
    e.db.close().await;
}

#[tokio::test]
async fn title_matches_rank_first_and_sort_can_switch_to_newest() {
    let e = env().await;
    published(&e.ctx, new_post("blog", "제주 여행", "바다", &[])).await;
    published(
        &e.ctx,
        new_post("blog", "가을 산책", "제주에 다녀왔다", &[]),
    )
    .await;
    published(&e.ctx, new_post("blog", "겨울 이야기", "눈", &["제주"])).await;

    assert_eq!(
        find(&e.ctx, "제주").await,
        ["제주 여행", "겨울 이야기", "가을 산책"],
        "제목 ×5 → 태그 ×3 → 본문"
    );
    let mut newest = SearchInput::new("제주");
    newest.sort = Sort::New;
    assert_eq!(
        titles(&e.ctx, newest).await,
        ["겨울 이야기", "가을 산책", "제주 여행"]
    );
    let mut title_only = SearchInput::new("제주");
    title_only.range = Range::Title;
    assert_eq!(titles(&e.ctx, title_only).await, ["제주 여행"]);
    let mut body_only = SearchInput::new("제주");
    body_only.range = Range::Body;
    assert_eq!(titles(&e.ctx, body_only).await, ["가을 산책"]);
    let mut tag_only = SearchInput::new("제주");
    tag_only.range = Range::Tag;
    assert_eq!(titles(&e.ctx, tag_only).await, ["겨울 이야기"]);
    e.db.close().await;
}

#[tokio::test]
async fn private_things_never_show_up_for_guests() {
    let e = env().await;
    let local = Actor::local();
    published(&e.ctx, new_post("blog", "공개 제주", "공개 본문", &[])).await;
    let mut secret = new_post("blog", "비밀 제주", "비밀 본문 제주", &[]);
    secret.secret = true;
    published(&e.ctx, secret).await;
    posts::create_draft(&e.ctx, &local, new_post("blog", "초안 제주", "", &[]))
        .await
        .unwrap();
    let hidden = published(&e.ctx, new_post("blog", "숨긴 제주", "", &[])).await;
    posts::hide(&e.ctx, &local, &hidden.id).await.unwrap();
    let gone = published(&e.ctx, new_post("blog", "지운 제주", "", &[])).await;
    posts::delete(&e.ctx, &local, &gone.id).await.unwrap();
    published(&e.ctx, new_post("members", "회원 제주", "", &[])).await;

    assert_eq!(find(&e.ctx, "제주").await, ["공개 제주"]);

    // 회원 전용 게시판은 읽을 수 있는 사람에게만.
    let reader = Actor::agent("t".into(), &[Scope::Read], None);
    let r = search::search(&e.ctx, &reader, SearchInput::new("제주"))
        .await
        .unwrap();
    let mut seen: Vec<_> = r.items.into_iter().map(|h| h.title).collect();
    seen.sort();
    assert_eq!(seen, ["공개 제주", "회원 제주"]);
    // 못 읽는 게시판을 고르면 없는 게시판과 같다.
    let anon = anonymous_actor(&e.ctx).await.unwrap();
    let mut input = SearchInput::new("제주");
    input.board = Some("members".into());
    assert_eq!(
        search::search(&e.ctx, &anon, input)
            .await
            .unwrap_err()
            .code(),
        Code::NotFound
    );

    // 관리 목록 검색은 권한 안에서 초안·비밀글·숨긴 글까지.
    let admin = posts::list(&e.ctx, &local, None, None, Some("제주"), PageReq::default())
        .await
        .unwrap();
    assert_eq!(admin.total, 6, "지운 글도 관리 목록엔 남는다");
    let none = posts::list(&e.ctx, &local, None, None, Some("서울"), PageReq::default())
        .await
        .unwrap();
    assert_eq!(none.total, 0);
    e.db.close().await;
}

#[tokio::test]
async fn index_follows_edits_and_board_filter() {
    let e = env().await;
    let local = Actor::local();
    let p = published(
        &e.ctx,
        new_post("blog", "처음 제목", "처음 본문", &["처음"]),
    )
    .await;
    published(&e.ctx, new_post("notes", "메모 처음", "", &[])).await;
    posts::update(
        &e.ctx,
        &local,
        &p.id,
        PostPatch {
            title: Some("나중 제목".into()),
            body_md: Some("나중 본문".into()),
            tags: Some(vec!["나중".into()]),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    assert_eq!(find(&e.ctx, "처음").await, ["메모 처음"]);
    assert_eq!(find(&e.ctx, "나중").await, ["나중 제목"]);

    let mut in_notes = SearchInput::new("처음 OR 나중");
    in_notes.board = Some("notes".into());
    assert_eq!(titles(&e.ctx, in_notes).await, ["메모 처음"]);

    // 페이지도 찾고, 게시판을 고르면 빠진다.
    let page = pages::create(
        &e.ctx,
        &local,
        NewPage {
            slug: "about".into(),
            title: "소개".into(),
            body_md: "나중에 만든 소개 페이지".into(),
            layout: "default".into(),
            sort_order: 0,
        },
    )
    .await
    .unwrap();
    assert_eq!(
        find(&e.ctx, "소개").await,
        Vec::<String>::new(),
        "초안 페이지"
    );
    pages::publish(&e.ctx, &local, &page.slug).await.unwrap();
    let anon = anonymous_actor(&e.ctx).await.unwrap();
    let r = search::search(&e.ctx, &anon, SearchInput::new("소개"))
        .await
        .unwrap();
    assert_eq!(r.items.len(), 1);
    assert_eq!(r.items[0].kind, HitKind::Page);
    assert_eq!(r.items[0].url, "/p/about");
    let mut in_blog = SearchInput::new("소개");
    in_blog.board = Some("blog".into());
    assert!(titles(&e.ctx, in_blog).await.is_empty());
    e.db.close().await;
}

#[tokio::test]
async fn paging_counts_all_hits() {
    let e = env().await;
    for i in 0..25 {
        published(&e.ctx, new_post("blog", &format!("국밥 {i}"), "", &[])).await;
    }
    let anon = anonymous_actor(&e.ctx).await.unwrap();
    let first = search::search(&e.ctx, &anon, SearchInput::new("국밥"))
        .await
        .unwrap();
    assert_eq!((first.total, first.items.len()), (25, 20));
    assert_eq!(first.total_pages(), 2);
    let mut second = SearchInput::new("국밥");
    second.page = 2;
    let second = search::search(&e.ctx, &anon, second).await.unwrap();
    assert_eq!((second.total, second.items.len()), (25, 5));
    e.db.close().await;
}

#[tokio::test]
async fn reindex_is_idempotent_audited_and_admin_only() {
    let e = env().await;
    published(&e.ctx, new_post("blog", "제주", "", &[])).await;

    // 색인 버전이 다르면 DB 를 열 때 다시 만든다.
    sqlx::query("UPDATE search_state SET version = 0")
        .execute(e.db.write_pool())
        .await
        .unwrap();
    sqlx::query("DELETE FROM search_fts WHERE rowid IS NOT NULL")
        .execute(e.db.write_pool())
        .await
        .unwrap();
    assert!(find(&e.ctx, "제주").await.is_empty());
    e.db.migrate().await.unwrap();
    assert_eq!(find(&e.ctx, "제주").await, ["제주"]);

    let local = Actor::local();
    let a = search::reindex(&e.ctx, &local).await.unwrap();
    let b = search::reindex(&e.ctx, &local).await.unwrap();
    assert_eq!(a, b);
    assert_eq!(a.posts, 1);
    assert_eq!(find(&e.ctx, "제주").await, ["제주"]);
    let audits: i64 =
        sqlx::query_scalar("SELECT count(*) FROM audit_log WHERE action = 'search.reindex'")
            .fetch_one(e.db.read_pool())
            .await
            .unwrap();
    assert_eq!(audits, 2);

    let anon = anonymous_actor(&e.ctx).await.unwrap();
    assert_eq!(
        search::reindex(&e.ctx, &anon).await.unwrap_err().code(),
        Code::Unauthorized
    );
    e.db.close().await;
}

#[tokio::test]
async fn odd_input_is_a_validation_error_not_a_crash() {
    let e = env().await;
    published(&e.ctx, new_post("blog", "제주", "", &[])).await;
    let anon = anonymous_actor(&e.ctx).await.unwrap();
    for q in [
        "\"",
        "*",
        "제주\"",
        "NEAR(제주)",
        "title:제주",
        "{body} : 제주",
        "a^b",
        "(제주",
        "AND",
        "'; DROP TABLE posts; --",
    ] {
        match search::search(&e.ctx, &anon, SearchInput::new(q)).await {
            Ok(_) => {}
            Err(err) => assert_eq!(err.code(), Code::Validation, "{q}"),
        }
    }
    for q in ["", "   ", "-제주", &"가".repeat(101)] {
        assert_eq!(
            search::search(&e.ctx, &anon, SearchInput::new(q))
                .await
                .unwrap_err()
                .code(),
            Code::Validation,
            "{q:?}"
        );
    }
    e.db.close().await;
}

/// 글 1만 개 다시 만들기 시간·파일 크기 (ADR-0050 기록용). `cargo test -p maru-storage --test search -- --ignored --nocapture`
#[tokio::test]
#[ignore]
async fn reindex_ten_thousand_posts() {
    let e = env().await;
    let board_id: String = sqlx::query_scalar("SELECT id FROM boards WHERE slug = 'blog'")
        .fetch_one(e.db.read_pool())
        .await
        .unwrap();
    let body = "제주도 올레길을 따라 걸으며 바다와 오름을 보았다. 점심은 고기국수, 저녁은 흑돼지. "
        .repeat(20);
    let mut tx = e.db.write_pool().begin().await.unwrap();
    for i in 0..10_000 {
        sqlx::query(
            "INSERT INTO posts (id, board_id, kind, status, title, slug, body_md, actor_type,
                                published_at, created_at, updated_at)
             VALUES (?, ?, 'post', 'published', ?, ?, ?, 'local', '2026-11-03T00:00:00Z',
                     '2026-11-03T00:00:00Z', '2026-11-03T00:00:00Z')",
        )
        .bind(format!("p{i:05}"))
        .bind(&board_id)
        .bind(format!("여행 기록 {i}"))
        .bind(format!("trip-{i}"))
        .bind(&body)
        .execute(&mut *tx)
        .await
        .unwrap();
    }
    tx.commit().await.unwrap();
    let size = |db: &Db| {
        let db = db.clone();
        async move {
            let pages: i64 = sqlx::query_scalar("PRAGMA page_count")
                .fetch_one(db.read_pool())
                .await
                .unwrap();
            pages * 4096
        }
    };
    let before = size(&e.db).await;
    let t = std::time::Instant::now();
    let done = search::reindex(&e.ctx, &Actor::local()).await.unwrap();
    let took = t.elapsed();
    let after = size(&e.db).await;
    let t = std::time::Instant::now();
    let r = search::search(
        &e.ctx,
        &anonymous_actor(&e.ctx).await.unwrap(),
        SearchInput::new("제주 흑돼지"),
    )
    .await
    .unwrap();
    println!(
        "글 {} 개: 다시 만들기 {:?}, DB {} KB → {} KB, 검색 {:?} ({} 건)",
        done.posts,
        took,
        before / 1024,
        after / 1024,
        t.elapsed(),
        r.total
    );
    e.db.close().await;
}
