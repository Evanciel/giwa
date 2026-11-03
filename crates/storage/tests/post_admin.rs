//! 리비전 비교·되돌리기, 글 이동·복사, 일괄 동작 (GAP C11·C12, ADR-0055).
//! 실제 SQLite 파일로 권한·상태·슬러그 겹침·일부 성공·감사를 검증한다.
#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::sync::Arc;

use maru_core::actor::{Actor, Permission, rank};
use maru_core::diff::Op;
use maru_core::model::{BoardPolicy, PostStatus};
use maru_core::services::boards::{self, NewBoard};
use maru_core::services::categories::{self, NewCategory};
use maru_core::services::members::{self, SignupInput};
use maru_core::services::posts::{
    self, BulkAction, BulkInput, CopyInput, MoveInput, NewPost, PostPatch,
};
use maru_core::services::settings::{self, SettingsPatch};
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
    let local = Actor::local();
    for (slug, kind, write) in [
        ("blog", "blog", "editor"),
        ("free", "free", "member"),
        ("notice", "notice", "editor"),
    ] {
        boards::create(
            &ctx,
            &local,
            NewBoard {
                slug: slug.into(),
                name: slug.into(),
                kind: kind.into(),
                policy: Some(BoardPolicy {
                    write: write.into(),
                    ..Default::default()
                }),
                sort_order: 0,
                layout: None,
                field_schema: None,
                list_order: None,
            },
        )
        .await
        .unwrap();
    }
    Env { _tmp: tmp, db, ctx }
}

async fn member(ctx: &Ctx, email: &str) -> Actor {
    settings::update(
        ctx,
        &Actor::local(),
        SettingsPatch {
            signup: Some("open".into()),
            signup_verify: Some(false),
            captcha: Some("off".into()),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    members::signup(
        ctx,
        SignupInput {
            email: email.into(),
            display_name: email.split('@').next().unwrap_or(email).into(),
            password: "test-password-1".into(),
            captcha: None,
        },
    )
    .await
    .unwrap();
    let m = ctx.store.member_by_email(email).await.unwrap().unwrap();
    Actor::human(
        m.id,
        [
            Permission::SiteView,
            Permission::BoardList,
            Permission::PostRead,
            Permission::PostWrite,
        ],
        rank::MEMBER,
    )
}

fn new(board: &str, title: &str, slug: &str) -> NewPost {
    NewPost {
        board: board.into(),
        title: title.into(),
        slug: Some(slug.into()),
        body_md: "하나\n둘\n셋".into(),
        excerpt: None,
        cover: None,
        category: None,
        tags: vec!["태그".into()],
        notice: false,
        secret: false,
        meta: Default::default(),
    }
}

async fn count(db: &Db, sql: &'static str) -> i64 {
    sqlx::query_scalar(sql)
        .fetch_one(db.read_pool())
        .await
        .unwrap()
}

#[tokio::test]
async fn compare_and_revert_to_an_older_revision() {
    let e = env().await;
    let local = Actor::local();
    let p = posts::create_draft(&e.ctx, &local, new("blog", "처음 제목", "p"))
        .await
        .unwrap();
    posts::update(
        &e.ctx,
        &local,
        &p.id,
        PostPatch {
            title: Some("고친 제목".into()),
            body_md: Some("하나\n둘둘\n셋\n넷".into()),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let revs = posts::revisions(&e.ctx, &local, &p.id).await.unwrap();
    assert_eq!(revs.len(), 2);
    let first = &revs[0];

    let cmp = posts::compare(&e.ctx, &local, &p.id, &first.id)
        .await
        .unwrap();
    assert!(cmp.title_changed);
    assert_eq!((cmp.body.added, cmp.body.removed), (2, 1));
    assert_eq!(cmp.body.lines[1].op, Op::Del);
    assert_eq!(cmp.body.lines[1].text, "둘");

    let back = posts::revert(&e.ctx, &local, &p.id, &first.id)
        .await
        .unwrap();
    assert_eq!(back.title, "처음 제목");
    assert_eq!(back.body_md, "하나\n둘\n셋");
    assert_eq!(
        posts::revisions(&e.ctx, &local, &p.id).await.unwrap().len(),
        3,
        "되돌리기도 리비전"
    );
    // 이미 같으면 그대로 (멱등).
    posts::revert(&e.ctx, &local, &p.id, &first.id)
        .await
        .unwrap();
    assert_eq!(
        posts::revisions(&e.ctx, &local, &p.id).await.unwrap().len(),
        3
    );
    assert_eq!(
        count(
            &e.db,
            "SELECT count(*) FROM audit_log WHERE action = 'post.revert'"
        )
        .await,
        1
    );

    // 다른 글의 리비전은 없는 것으로.
    let other = posts::create_draft(&e.ctx, &local, new("blog", "다른 글", "q"))
        .await
        .unwrap();
    let err = posts::revert(&e.ctx, &local, &other.id, &first.id)
        .await
        .unwrap_err();
    assert_eq!(err.code(), Code::NotFound);
    // 지운 글은 되돌리지 않는다.
    posts::delete(&e.ctx, &local, &p.id).await.unwrap();
    let err = posts::revert(&e.ctx, &local, &p.id, &first.id)
        .await
        .unwrap_err();
    assert_eq!(err.code(), Code::Conflict);
    e.db.close().await;
}

#[tokio::test]
async fn members_revert_only_their_own_posts() {
    let e = env().await;
    let alice = member(&e.ctx, "alice@example.test").await;
    let bob = member(&e.ctx, "bob@example.test").await;
    let p = posts::create_draft(&e.ctx, &alice, new("free", "앨리스 글", "a"))
        .await
        .unwrap();
    let rev = posts::revisions(&e.ctx, &alice, &p.id).await.unwrap()[0]
        .id
        .clone();
    let err = posts::revert(&e.ctx, &bob, &p.id, &rev).await.unwrap_err();
    assert_eq!(err.code(), Code::Forbidden);
    e.db.close().await;
}

#[tokio::test]
async fn move_keeps_the_post_and_resolves_slug_and_category() {
    let e = env().await;
    let local = Actor::local();
    categories::create(
        &e.ctx,
        &local,
        NewCategory {
            board: "blog".into(),
            name: "잡담".into(),
            slug: Some("chat".into()),
        },
    )
    .await
    .unwrap();
    let mut input = new("blog", "옮길 글", "same");
    input.category = Some("chat".into());
    let p = posts::create_draft(&e.ctx, &local, input).await.unwrap();
    posts::publish(&e.ctx, &local, &p.id).await.unwrap();
    posts::create_draft(&e.ctx, &local, new("free", "이미 있는 글", "same"))
        .await
        .unwrap();

    let moved = posts::move_to(
        &e.ctx,
        &local,
        &p.id,
        MoveInput {
            board: "free".into(),
            category: None,
        },
    )
    .await
    .unwrap();
    assert_eq!(moved.id, p.id);
    assert_eq!(moved.status, PostStatus::Published);
    assert_eq!(moved.slug, "same-2", "겹치면 번호를 붙인다");
    assert_eq!(moved.category_id, None, "분류는 새 게시판 것만");
    let free = boards::show(&e.ctx, &local, "free").await.unwrap();
    assert_eq!(moved.board_id, free.id);
    let (_, public) = posts::public_post(
        &e.ctx,
        &maru_core::services::anonymous_actor(&e.ctx).await.unwrap(),
        "free",
        "same-2",
    )
    .await
    .unwrap();
    assert_eq!(public.id, p.id);
    assert_eq!(
        count(
            &e.db,
            "SELECT count(*) FROM audit_log WHERE action = 'post.move'"
        )
        .await,
        1
    );
    let err = posts::move_to(
        &e.ctx,
        &local,
        &p.id,
        MoveInput {
            board: "nowhere".into(),
            category: None,
        },
    )
    .await
    .unwrap_err();
    assert_eq!(err.code(), Code::NotFound);
    e.db.close().await;
}

#[tokio::test]
async fn members_cannot_move_into_staff_boards_and_copy_is_a_draft() {
    let e = env().await;
    let alice = member(&e.ctx, "alice@example.test").await;
    let p = posts::create_draft(&e.ctx, &alice, new("free", "앨리스 글", "a"))
        .await
        .unwrap();
    let err = posts::move_to(
        &e.ctx,
        &alice,
        &p.id,
        MoveInput {
            board: "notice".into(),
            category: None,
        },
    )
    .await
    .unwrap_err();
    assert_eq!(err.code(), Code::Forbidden, "새 게시판 쓰기 권한");

    let local = Actor::local();
    posts::publish(&e.ctx, &local, &p.id).await.unwrap();
    let copy = posts::copy_to(
        &e.ctx,
        &local,
        &p.id,
        CopyInput {
            board: "blog".into(),
        },
    )
    .await
    .unwrap();
    assert_ne!(copy.id, p.id);
    assert_eq!(copy.status, PostStatus::Draft);
    assert_eq!(copy.title, "앨리스 글");
    assert_eq!(copy.tags, vec!["태그".to_owned()]);
    assert_eq!(copy.author_id, None, "글쓴이는 복사한 사람");
    assert_eq!(copy.view_count, 0);
    e.db.close().await;
}

#[tokio::test]
async fn bulk_reports_each_post() {
    let e = env().await;
    let local = Actor::local();
    let a = posts::create_draft(&e.ctx, &local, new("blog", "가", "a"))
        .await
        .unwrap();
    let b = posts::create_draft(&e.ctx, &local, new("blog", "나", "b"))
        .await
        .unwrap();
    posts::publish(&e.ctx, &local, &a.id).await.unwrap();
    // 초안은 숨길 수 없다: 하나는 되고 하나는 안 된다.
    let items = posts::bulk(
        &e.ctx,
        &local,
        BulkInput {
            ids: vec![a.id.clone(), b.id.clone(), a.id.clone(), "없는-글".into()],
            action: BulkAction::Hide,
        },
    )
    .await
    .unwrap();
    assert_eq!(items.len(), 3, "같은 id 는 한 번");
    assert!(items[0].ok);
    assert!(!items[1].ok);
    assert_eq!(items[1].error.as_ref().unwrap().code, "CONFLICT");
    assert_eq!(items[2].error.as_ref().unwrap().code, "NOT_FOUND");
    assert_eq!(
        posts::get(&e.ctx, &local, &a.id).await.unwrap().status,
        PostStatus::Hidden
    );

    let moved = posts::bulk(
        &e.ctx,
        &local,
        BulkInput {
            ids: vec![a.id.clone(), b.id.clone()],
            action: BulkAction::Move {
                board: "free".into(),
                category: None,
            },
        },
    )
    .await
    .unwrap();
    assert!(moved.iter().all(|i| i.ok));
    assert_eq!(
        count(
            &e.db,
            "SELECT count(*) FROM posts p JOIN boards b ON b.id = p.board_id WHERE b.slug = 'free'"
        )
        .await,
        2
    );
    let err = posts::bulk(
        &e.ctx,
        &local,
        BulkInput {
            ids: vec![],
            action: BulkAction::Publish,
        },
    )
    .await
    .unwrap_err();
    assert_eq!(err.code(), Code::Validation);
    e.db.close().await;
}
