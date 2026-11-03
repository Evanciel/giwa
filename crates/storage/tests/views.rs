//! 조회수·이전/다음 글·관련 글 (GAP C6·C7, ADR-0049). 실제 SQLite 파일로 core 서비스를 검증한다.
#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::sync::Arc;

use maru_core::actor::{Actor, Scope};
use maru_core::model::Post;
use maru_core::services::anonymous_actor;
use maru_core::services::boards::{self, NewBoard};
use maru_core::services::posts::{self, NewPost};
use maru_core::services::views::{self, ViewCounter};
use maru_core::{Code, Ctx};
use maru_storage::{DataDir, Db, FsBlobs, SqliteStore};

const UA: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) Chrome/130.0";

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
    for slug in ["blog", "notes"] {
        boards::create(
            &ctx,
            &Actor::local(),
            NewBoard {
                slug: slug.into(),
                name: slug.into(),
                kind: "blog".into(),
                policy: None,
                sort_order: 0,
                layout: None,
                field_schema: None,
            },
        )
        .await
        .unwrap();
    }
    Env { _tmp: tmp, db, ctx }
}

fn new_post(board: &str, title: &str, tags: &[&str]) -> NewPost {
    NewPost {
        board: board.into(),
        title: title.into(),
        slug: None,
        body_md: format!("{title} 본문"),
        excerpt: None,
        cover: None,
        category: None,
        tags: tags.iter().map(|s| (*s).to_owned()).collect(),
        notice: false,
        secret: false,
        meta: Default::default(),
    }
}

async fn published(ctx: &Ctx, input: NewPost) -> Post {
    let local = Actor::local();
    let p = posts::create_draft(ctx, &local, input).await.unwrap();
    posts::publish(ctx, &local, &p.id).await.unwrap()
}

async fn views_audits(db: &Db) -> i64 {
    sqlx::query_scalar("SELECT count(*) FROM audit_log WHERE action = 'post.views.set'")
        .fetch_one(db.read_pool())
        .await
        .unwrap()
}

#[tokio::test]
async fn flush_adds_counts_without_touching_updated_at_or_audit() {
    let e = env().await;
    let a = published(&e.ctx, new_post("blog", "첫 글", &[])).await;
    let audits_before: i64 = sqlx::query_scalar("SELECT count(*) FROM audit_log")
        .fetch_one(e.db.read_pool())
        .await
        .unwrap();

    let counter = ViewCounter::new();
    assert!(counter.record(None, &a.id, "10.0.0.1", UA));
    assert!(
        !counter.record(None, &a.id, "10.0.0.1", UA),
        "새로고침은 세지 않는다"
    );
    assert!(counter.record(None, &a.id, "10.0.0.2", UA));
    assert_eq!(counter.shown(&a), 2, "반영 전에도 화면에는 보인다");

    assert_eq!(views::flush(&e.ctx, &counter).await.unwrap(), 1);
    assert_eq!(
        views::flush(&e.ctx, &counter).await.unwrap(),
        0,
        "두 번째는 할 일 없음"
    );
    let after = posts::get(&e.ctx, &Actor::local(), &a.id).await.unwrap();
    assert_eq!(after.view_count, 2);
    assert_eq!(counter.shown(&after), 2);
    assert_eq!(
        after.updated_at, a.updated_at,
        "조회는 글 고친 시각이 아니다"
    );
    let audits_after: i64 = sqlx::query_scalar("SELECT count(*) FROM audit_log")
        .fetch_one(e.db.read_pool())
        .await
        .unwrap();
    assert_eq!(audits_after, audits_before, "조회수는 감사 대상이 아니다");

    // 글을 고쳐도 조회수는 그대로다 (update_post 가 덮어쓰지 않는다).
    let edited = posts::update(
        &e.ctx,
        &Actor::local(),
        &a.id,
        posts::PostPatch {
            title: Some("고친 제목".into()),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    assert_eq!(edited.view_count, 2);
    let reread = posts::get(&e.ctx, &Actor::local(), &a.id).await.unwrap();
    assert_eq!(reread.view_count, 2);
    e.db.close().await;
}

#[tokio::test]
async fn set_views_needs_moderate_and_is_audited() {
    let e = env().await;
    let a = published(&e.ctx, new_post("blog", "글", &[])).await;
    let writer = Actor::agent("t-w".into(), &[Scope::Read, Scope::Draft], None);
    let err = views::set(&e.ctx, &writer, &a.id, 0).await.unwrap_err();
    assert_eq!(err.code(), Code::Forbidden);
    let err = views::set(&e.ctx, &Actor::local(), &a.id, -1)
        .await
        .unwrap_err();
    assert_eq!(err.code(), Code::Validation);
    let err = views::set(&e.ctx, &Actor::local(), "없는-글", 0)
        .await
        .unwrap_err();
    assert_eq!(err.code(), Code::NotFound);

    let p = views::set(&e.ctx, &Actor::local(), &a.id, 42)
        .await
        .unwrap();
    assert_eq!(p.view_count, 42);
    let again = views::set(&e.ctx, &Actor::local(), &a.id, 42)
        .await
        .unwrap();
    assert_eq!(again.view_count, 42, "같은 값을 다시 정해도 된다");
    let reread = posts::get(&e.ctx, &Actor::local(), &a.id).await.unwrap();
    assert_eq!(reread.view_count, 42);
    assert_eq!(reread.updated_at, a.updated_at);
    assert_eq!(views_audits(&e.db).await, 2);
    e.db.close().await;
}

#[tokio::test]
async fn neighbors_follow_publish_order_within_board_and_mask_secrets() {
    let e = env().await;
    let local = Actor::local();
    let anon = anonymous_actor(&e.ctx).await.unwrap();
    let one = published(&e.ctx, new_post("blog", "하나", &[])).await;
    let _other = published(&e.ctx, new_post("notes", "다른 게시판", &[])).await;
    let two = published(&e.ctx, new_post("blog", "둘", &[])).await;
    let secret = published(
        &e.ctx,
        NewPost {
            secret: true,
            ..new_post("blog", "비밀", &[])
        },
    )
    .await;
    let draft = posts::create_draft(&e.ctx, &local, new_post("blog", "초안", &[]))
        .await
        .unwrap();
    let four = published(&e.ctx, new_post("blog", "넷", &[])).await;
    let blog = boards::readable(&e.ctx, &anon, "blog").await.unwrap();

    let n = posts::nearby(&e.ctx, &anon, &blog, &two).await.unwrap();
    assert_eq!(
        n.prev.as_ref().map(|p| p.title.as_str()),
        Some("하나"),
        "다른 게시판 글은 건너뛴다"
    );
    let next = n.next.unwrap();
    assert_eq!(next.title, "비밀", "비밀글도 순서에 든다");
    assert!(next.body_md.is_empty(), "손님에게는 비밀글 본문이 없다");

    let n = posts::nearby(&e.ctx, &anon, &blog, &secret).await.unwrap();
    assert_eq!(
        n.next.map(|p| p.title),
        Some("넷".into()),
        "초안은 건너뛴다"
    );
    let _ = draft;

    let n = posts::nearby(&e.ctx, &anon, &blog, &one).await.unwrap();
    assert!(n.prev.is_none(), "첫 글은 이전 글이 없다");
    let n = posts::nearby(&e.ctx, &anon, &blog, &four).await.unwrap();
    assert!(n.next.is_none(), "마지막 글은 다음 글이 없다");

    // 관리자는 비밀글 본문까지 받는다.
    let n = posts::nearby(&e.ctx, &local, &blog, &two).await.unwrap();
    assert!(!n.next.unwrap().body_md.is_empty());
    e.db.close().await;
}

#[tokio::test]
async fn related_prefers_shared_tags_then_board_and_skips_secret_and_notice() {
    let e = env().await;
    let anon = anonymous_actor(&e.ctx).await.unwrap();
    let base = published(
        &e.ctx,
        new_post("blog", "기준", &["러스트", "웹", "SQLite"]),
    )
    .await;
    let two_tags = published(&e.ctx, new_post("notes", "태그 둘", &["러스트", "웹"])).await;
    let one_tag = published(&e.ctx, new_post("blog", "태그 하나", &["웹"])).await;
    let same_board = published(&e.ctx, new_post("blog", "같은 게시판", &[])).await;
    let _far = published(&e.ctx, new_post("notes", "상관없는 글", &["요리"])).await;
    let _secret = published(
        &e.ctx,
        NewPost {
            secret: true,
            ..new_post("blog", "비밀", &["러스트", "웹", "SQLite"])
        },
    )
    .await;
    let _notice = published(
        &e.ctx,
        NewPost {
            notice: true,
            ..new_post("blog", "공지", &["러스트"])
        },
    )
    .await;
    let blog = boards::readable(&e.ctx, &anon, "blog").await.unwrap();

    let n = posts::nearby(&e.ctx, &anon, &blog, &base).await.unwrap();
    let titles: Vec<&str> = n.related.iter().map(|p| p.title.as_str()).collect();
    assert_eq!(titles, vec!["태그 둘", "태그 하나", "같은 게시판"]);
    assert_eq!(n.related[0].id, two_tags.id);
    assert_eq!(n.related[1].id, one_tag.id);
    assert_eq!(n.related[2].id, same_board.id);
    e.db.close().await;
}
