//! U4 쓰기 경계·감사, U5 글 서비스. 실제 SQLite 파일로 core 서비스를 검증한다.
#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::sync::Arc;

use maru_core::actor::{Actor, Scope};
use maru_core::model::{PageReq, PostStatus};
use maru_core::services::boards::{self, NewBoard};
use maru_core::services::posts::{self, NewPost};
use maru_core::services::{anonymous_actor, tokens};
use maru_core::store::Tx;
use maru_core::{Code, Ctx};
use maru_storage::{DataDir, Db, SqliteStore};

struct Env {
    _tmp: tempfile::TempDir,
    dir: DataDir,
    db: Db,
    ctx: Ctx,
}

async fn env() -> Env {
    let tmp = tempfile::tempdir().unwrap();
    let dir = DataDir::new(tmp.path().join("데이터 폴더"));
    dir.ensure_layout().unwrap();
    let db = Db::open(&dir.db_path(), true).await.unwrap();
    db.migrate().await.unwrap();
    let ctx = ctx_for(&db);
    Env {
        _tmp: tmp,
        dir,
        db,
        ctx,
    }
}

fn ctx_for(db: &Db) -> Ctx {
    Ctx {
        store: Arc::new(SqliteStore::new(db.clone())),
        site_url: "http://127.0.0.1:8080".into(),
    }
}

async fn blog(ctx: &Ctx) {
    boards::create(
        ctx,
        &Actor::local(),
        NewBoard {
            slug: "blog".into(),
            name: "블로그".into(),
            kind: "blog".into(),
            policy: None,
            sort_order: 0,
        },
    )
    .await
    .unwrap();
}

fn draft(title: &str) -> NewPost {
    NewPost {
        board: "blog".into(),
        title: title.into(),
        slug: None,
        body_md: "본문입니다.".into(),
        excerpt: None,
    }
}

async fn scalar(db: &Db, sql: &'static str) -> i64 {
    sqlx::query_scalar(sql)
        .fetch_one(db.read_pool())
        .await
        .unwrap()
}

// ---- U4 ----

#[tokio::test]
async fn audit_failure_rolls_back_write() {
    let e = env().await;
    blog(&e.ctx).await;
    sqlx::query(
        "CREATE TRIGGER fail_audit BEFORE INSERT ON audit_log \
         BEGIN SELECT RAISE(ABORT, 'audit down'); END",
    )
    .execute(e.db.write_pool())
    .await
    .unwrap();

    let err = posts::create_draft(&e.ctx, &Actor::local(), draft("남지 않아야 할 글"))
        .await
        .unwrap_err();
    assert_eq!(err.code(), Code::Internal);
    assert_eq!(scalar(&e.db, "SELECT count(*) FROM posts").await, 0);
    assert_eq!(scalar(&e.db, "SELECT count(*) FROM revisions").await, 0);
    e.db.close().await;
}

#[tokio::test]
async fn commit_without_audit_is_rejected() {
    let e = env().await;
    blog(&e.ctx).await;
    let before = scalar(&e.db, "SELECT count(*) FROM boards").await;

    let mut tx = Tx::begin(&e.ctx).await.unwrap();
    let mut board = e.ctx.store.board_by_slug("blog").await.unwrap().unwrap();
    board.id = "sneaky".into();
    board.slug = "sneaky".into();
    tx.insert_board(&board).await.unwrap();
    let err = tx.commit().await.unwrap_err();
    assert_eq!(err.code(), Code::Internal);
    assert_eq!(scalar(&e.db, "SELECT count(*) FROM boards").await, before);
    e.db.close().await;
}

/// 서버와 CLI 가 따로 연 것처럼 연결 풀을 둘 만들어 동시에 쓴다.
#[tokio::test]
async fn two_writers_do_not_hit_busy() {
    let e = env().await;
    blog(&e.ctx).await;
    let other = Db::open(&e.dir.db_path(), false).await.unwrap();
    let ctx2 = ctx_for(&other);

    let run = |ctx: Ctx, tag: &'static str| async move {
        for i in 0..40 {
            posts::create_draft(&ctx, &Actor::local(), draft(&format!("{tag} {i}")))
                .await
                .unwrap();
        }
    };
    tokio::join!(run(e.ctx.clone(), "서버"), run(ctx2, "CLI"));

    assert_eq!(scalar(&e.db, "SELECT count(*) FROM posts").await, 80);
    assert_eq!(
        scalar(
            &e.db,
            "SELECT count(*) FROM audit_log WHERE action = 'post.create'"
        )
        .await,
        80
    );
    other.close().await;
    e.db.close().await;
}

#[tokio::test]
async fn denials_carry_the_right_code() {
    let e = env().await;
    blog(&e.ctx).await;
    let anon = anonymous_actor(&e.ctx).await.unwrap();
    assert_eq!(
        posts::create_draft(&e.ctx, &anon, draft("x"))
            .await
            .unwrap_err()
            .code(),
        Code::Unauthorized
    );

    // 기본 토큰(read, draft)은 초안은 쓰지만 발행은 못 한다.
    let agent = Actor::agent("t1".into(), &Scope::DEFAULT, None);
    let own = posts::create_draft(&e.ctx, &agent, draft("x"))
        .await
        .unwrap();
    let err = posts::publish(&e.ctx, &agent, &own.id).await.unwrap_err();
    assert_eq!(err.code(), Code::Forbidden);

    // 읽기 전용 토큰은 초안도 못 쓴다.
    let reader = Actor::agent("t0".into(), &[Scope::Read], None);
    let err = posts::create_draft(&e.ctx, &reader, draft("x"))
        .await
        .unwrap_err();
    assert_eq!(err.code(), Code::Forbidden);

    // publish 스코프가 있으면 편집자 서열이라 쓸 수 있고 발행도 된다.
    let editor = Actor::agent(
        "t2".into(),
        &[Scope::Read, Scope::Draft, Scope::Publish],
        None,
    );
    let post = posts::create_draft(&e.ctx, &editor, draft("에이전트 글"))
        .await
        .unwrap();
    posts::publish(&e.ctx, &editor, &post.id).await.unwrap();

    // 구조 변경은 structure.edit 가 필요하다.
    let err = boards::create(
        &e.ctx,
        &editor,
        NewBoard {
            slug: "free".into(),
            name: "자유".into(),
            kind: "free".into(),
            policy: None,
            sort_order: 0,
        },
    )
    .await
    .unwrap_err();
    assert_eq!(err.code(), Code::Forbidden);
    e.db.close().await;
}

#[tokio::test]
async fn audit_rows_name_the_actor() {
    let e = env().await;
    blog(&e.ctx).await;
    let editor = Actor::agent("tok_1".into(), &[Scope::Draft, Scope::Publish], None);
    let post = posts::create_draft(&e.ctx, &editor, draft("감사 대조"))
        .await
        .unwrap();
    posts::publish(&e.ctx, &editor, &post.id).await.unwrap();

    let rows: Vec<(String, Option<String>, String, Option<String>)> = sqlx::query_as(
        "SELECT actor_type, actor_id, action, target_id FROM audit_log \
         WHERE target_type = 'post' ORDER BY at, id",
    )
    .fetch_all(e.db.read_pool())
    .await
    .unwrap();
    assert_eq!(
        rows,
        vec![
            (
                "agent".into(),
                Some("tok_1".into()),
                "post.create".into(),
                Some(post.id.clone())
            ),
            (
                "agent".into(),
                Some("tok_1".into()),
                "post.publish".into(),
                Some(post.id.clone())
            ),
        ]
    );
    e.db.close().await;
}

// ---- U5 ----

#[tokio::test]
async fn draft_publish_and_public_visibility() {
    let e = env().await;
    blog(&e.ctx).await;
    let local = Actor::local();
    let anon = anonymous_actor(&e.ctx).await.unwrap();

    let post = posts::create_draft(&e.ctx, &local, draft("첫 글: 안녕하세요!"))
        .await
        .unwrap();
    assert_eq!(post.status, PostStatus::Draft);
    assert_eq!(post.slug, "첫-글-안녕하세요");
    assert_eq!(
        posts::revisions(&e.ctx, &local, &post.id)
            .await
            .unwrap()
            .len(),
        1
    );

    // 초안은 공개 경로에서 보이지 않고, 존재도 드러내지 않는다.
    let err = posts::public_post(&e.ctx, &anon, "blog", &post.slug)
        .await
        .unwrap_err();
    assert_eq!(err.code(), Code::NotFound);
    assert_eq!(
        posts::get(&e.ctx, &anon, &post.id)
            .await
            .unwrap_err()
            .code(),
        Code::NotFound
    );
    let list = posts::public_posts(&e.ctx, &anon, None, PageReq::default())
        .await
        .unwrap();
    assert_eq!(list.total, 0);

    let published = posts::publish(&e.ctx, &local, &post.id).await.unwrap();
    assert_eq!(published.status, PostStatus::Published);
    assert!(published.published_at.is_some());

    let (_, seen) = posts::public_post(&e.ctx, &anon, "blog", "첫-글-안녕하세요")
        .await
        .unwrap();
    assert_eq!(seen.id, post.id);
    let list = posts::public_posts(&e.ctx, &anon, None, PageReq::default())
        .await
        .unwrap();
    assert_eq!(list.total, 1);
    assert_eq!(list.items[0].title, "첫 글: 안녕하세요!");

    // 다시 발행해도 상태·감사 로그가 그대로다 (멱등).
    let audits = scalar(&e.db, "SELECT count(*) FROM audit_log").await;
    let again = posts::publish(&e.ctx, &local, &post.id).await.unwrap();
    assert_eq!(again.published_at, published.published_at);
    assert_eq!(
        scalar(&e.db, "SELECT count(*) FROM audit_log").await,
        audits
    );
    e.db.close().await;
}

#[tokio::test]
async fn slugs_are_unique_per_board() {
    let e = env().await;
    blog(&e.ctx).await;
    let local = Actor::local();
    let a = posts::create_draft(&e.ctx, &local, draft("같은 제목"))
        .await
        .unwrap();
    let b = posts::create_draft(&e.ctx, &local, draft("같은 제목"))
        .await
        .unwrap();
    assert_eq!(a.slug, "같은-제목");
    assert_eq!(b.slug, "같은-제목-2");

    let mut explicit = draft("다른 제목");
    explicit.slug = Some("같은-제목".into());
    let err = posts::create_draft(&e.ctx, &local, explicit)
        .await
        .unwrap_err();
    assert_eq!(err.code(), Code::Conflict);
    e.db.close().await;
}

#[tokio::test]
async fn private_board_hides_posts_from_guests() {
    let e = env().await;
    let local = Actor::local();
    boards::create(
        &e.ctx,
        &local,
        NewBoard {
            slug: "members".into(),
            name: "회원 전용".into(),
            kind: "free".into(),
            policy: Some(maru_core::model::BoardPolicy {
                read: "member".into(),
                ..Default::default()
            }),
            sort_order: 0,
        },
    )
    .await
    .unwrap();
    let mut input = draft("회원 글");
    input.board = "members".into();
    let post = posts::create_draft(&e.ctx, &local, input).await.unwrap();
    posts::publish(&e.ctx, &local, &post.id).await.unwrap();

    let anon = anonymous_actor(&e.ctx).await.unwrap();
    let list = posts::public_posts(&e.ctx, &anon, None, PageReq::default())
        .await
        .unwrap();
    assert_eq!(list.total, 0);
    assert_eq!(
        posts::public_post(&e.ctx, &anon, "members", &post.slug)
            .await
            .unwrap_err()
            .code(),
        Code::NotFound
    );
    let reader = Actor::agent("t".into(), &[Scope::Read, Scope::Draft], None);
    let list = posts::public_posts(&e.ctx, &reader, None, PageReq::default())
        .await
        .unwrap();
    assert_eq!(list.total, 1);
    e.db.close().await;
}

#[tokio::test]
async fn token_lifecycle() {
    let e = env().await;
    let issued = tokens::create(
        &e.ctx,
        &Actor::local(),
        tokens::NewToken {
            name: "테스트".into(),
            scopes: vec![],
            board_ids: None,
            expires_at: None,
        },
    )
    .await
    .unwrap();
    assert_eq!(issued.token.scopes, Scope::DEFAULT.to_vec());

    let actor = tokens::authenticate(&e.ctx, &issued.secret).await.unwrap();
    assert!(actor.has(maru_core::Permission::PostWrite));
    assert!(!actor.has(maru_core::Permission::PostPublish));

    for bad in ["", "maru_00", "Bearer x", &issued.secret[..20]] {
        assert_eq!(
            tokens::authenticate(&e.ctx, bad).await.unwrap_err().code(),
            Code::Unauthorized
        );
    }

    // 에이전트는 토큰을 만들 수 없다.
    let err = tokens::create(
        &e.ctx,
        &actor,
        tokens::NewToken {
            name: "x".into(),
            scopes: vec![Scope::Admin],
            board_ids: None,
            expires_at: None,
        },
    )
    .await
    .unwrap_err();
    assert_eq!(err.code(), Code::Forbidden);

    // 감사 로그와 DB 에 원문 토큰이 없다.
    let leaked: i64 =
        sqlx::query_scalar("SELECT count(*) FROM audit_log WHERE diff LIKE '%' || ? || '%'")
            .bind(&issued.secret[5..])
            .fetch_one(e.db.read_pool())
            .await
            .unwrap();
    assert_eq!(leaked, 0);
    let hash_in_audit: i64 =
        sqlx::query_scalar("SELECT count(*) FROM audit_log WHERE diff LIKE '%' || ? || '%'")
            .bind(&issued.token.token_hash)
            .fetch_one(e.db.read_pool())
            .await
            .unwrap();
    assert_eq!(hash_in_audit, 0);
    e.db.close().await;
}

// ---- U8 ----

#[tokio::test]
async fn full_lifecycle_and_public_sync() {
    let e = env().await;
    blog(&e.ctx).await;
    let local = Actor::local();
    let anon = anonymous_actor(&e.ctx).await.unwrap();
    let p = posts::create_draft(&e.ctx, &local, draft("생애 주기"))
        .await
        .unwrap();
    let visible = |n: i64| {
        let ctx = e.ctx.clone();
        let anon = anon.clone();
        async move {
            let list = posts::public_posts(&ctx, &anon, None, PageReq::default())
                .await
                .unwrap();
            assert_eq!(list.total, n);
        }
    };

    assert_eq!(
        posts::submit(&e.ctx, &local, &p.id).await.unwrap().status,
        PostStatus::Review
    );
    posts::publish(&e.ctx, &local, &p.id).await.unwrap();
    visible(1).await;
    assert_eq!(
        posts::hide(&e.ctx, &local, &p.id).await.unwrap().status,
        PostStatus::Hidden
    );
    visible(0).await;
    assert_eq!(
        posts::restore(&e.ctx, &local, &p.id).await.unwrap().status,
        PostStatus::Published
    );
    visible(1).await;
    assert_eq!(
        posts::delete(&e.ctx, &local, &p.id).await.unwrap().status,
        PostStatus::Deleted
    );
    visible(0).await;
    assert_eq!(
        posts::public_post(&e.ctx, &anon, "blog", &p.slug)
            .await
            .unwrap_err()
            .code(),
        Code::NotFound
    );
    // 삭제된 글은 고칠 수 없고 발행도 안 된다. 복원하면 공개로 돌아온다.
    let patch = posts::PostPatch {
        title: Some("x".into()),
        ..Default::default()
    };
    assert_eq!(
        posts::update(&e.ctx, &local, &p.id, patch)
            .await
            .unwrap_err()
            .code(),
        Code::Conflict
    );
    assert_eq!(
        posts::publish(&e.ctx, &local, &p.id)
            .await
            .unwrap_err()
            .code(),
        Code::Conflict
    );
    posts::restore(&e.ctx, &local, &p.id).await.unwrap();
    visible(1).await;

    // 발행된 적 없는 초안은 복원하면 초안이다.
    let d = posts::create_draft(&e.ctx, &local, draft("초안"))
        .await
        .unwrap();
    posts::delete(&e.ctx, &local, &d.id).await.unwrap();
    assert_eq!(
        posts::restore(&e.ctx, &local, &d.id).await.unwrap().status,
        PostStatus::Draft
    );
    e.db.close().await;
}

#[tokio::test]
async fn update_makes_revision_and_checks_slug() {
    let e = env().await;
    blog(&e.ctx).await;
    let local = Actor::local();
    let a = posts::create_draft(&e.ctx, &local, draft("첫째"))
        .await
        .unwrap();
    let b = posts::create_draft(&e.ctx, &local, draft("둘째"))
        .await
        .unwrap();

    let patch = posts::PostPatch {
        body_md: Some("고친 본문".into()),
        ..Default::default()
    };
    let a2 = posts::update(&e.ctx, &local, &a.id, patch.clone())
        .await
        .unwrap();
    assert_eq!(a2.body_md, "고친 본문");
    assert_eq!(
        posts::revisions(&e.ctx, &local, &a.id).await.unwrap().len(),
        2
    );
    // 같은 내용으로 다시 고치면 아무 일도 없다.
    let audits = scalar(&e.db, "SELECT count(*) FROM audit_log").await;
    posts::update(&e.ctx, &local, &a.id, patch).await.unwrap();
    assert_eq!(
        scalar(&e.db, "SELECT count(*) FROM audit_log").await,
        audits
    );

    let clash = posts::PostPatch {
        slug: Some(b.slug.clone()),
        ..Default::default()
    };
    assert_eq!(
        posts::update(&e.ctx, &local, &a.id, clash)
            .await
            .unwrap_err()
            .code(),
        Code::Conflict
    );
    e.db.close().await;
}

#[tokio::test]
async fn own_and_other_permissions() {
    let e = env().await;
    blog(&e.ctx).await;
    let writer = Actor::agent("w".into(), &Scope::DEFAULT, None);
    let editor = Actor::agent(
        "ed".into(),
        &[Scope::Read, Scope::Draft, Scope::Publish],
        None,
    );
    let local = Actor::local();

    // 로컬(운영자)이 쓴 글: 기본 토큰에게는 남의 글이다.
    let others = posts::create_draft(&e.ctx, &local, draft("운영자 글"))
        .await
        .unwrap();
    let patch = posts::PostPatch {
        title: Some("바꿈".into()),
        ..Default::default()
    };
    assert_eq!(
        posts::update(&e.ctx, &writer, &others.id, patch.clone())
            .await
            .unwrap_err()
            .code(),
        Code::Forbidden
    );
    assert_eq!(
        posts::delete(&e.ctx, &writer, &others.id)
            .await
            .unwrap_err()
            .code(),
        Code::Forbidden
    );
    assert_eq!(
        posts::get(&e.ctx, &writer, &others.id)
            .await
            .unwrap_err()
            .code(),
        Code::NotFound
    );
    // 관리 권한(publish 스코프)이 있으면 남의 글도 고친다.
    posts::update(&e.ctx, &editor, &others.id, patch.clone())
        .await
        .unwrap();

    // 본인 글은 기본 토큰으로 고치고 지우고 되살린다. 단, 발행된 적 있으면 되살리기는 발행 권한이 필요하다.
    let mine = posts::create_draft(&e.ctx, &writer, draft("내 글"))
        .await
        .unwrap();
    posts::update(&e.ctx, &writer, &mine.id, patch)
        .await
        .unwrap();
    posts::delete(&e.ctx, &writer, &mine.id).await.unwrap();
    posts::restore(&e.ctx, &writer, &mine.id).await.unwrap();
    posts::publish(&e.ctx, &editor, &mine.id).await.unwrap();
    assert_eq!(
        posts::hide(&e.ctx, &writer, &mine.id)
            .await
            .unwrap_err()
            .code(),
        Code::Forbidden
    );

    // 목록: 기본 토큰은 본인 글만, 편집자는 전부
    let own = posts::list(&e.ctx, &writer, None, None, PageReq::default())
        .await
        .unwrap();
    assert_eq!(own.total, 1);
    let all = posts::list(&e.ctx, &editor, None, None, PageReq::default())
        .await
        .unwrap();
    assert_eq!(all.total, 2);
    e.db.close().await;
}

#[tokio::test]
async fn boards_crud_and_non_empty_delete() {
    let e = env().await;
    let local = Actor::local();
    blog(&e.ctx).await;
    let input = NewBoard {
        slug: "notice".into(),
        name: "공지".into(),
        kind: "notice".into(),
        policy: None,
        sort_order: 1,
    };
    let first = boards::ensure(&e.ctx, &local, input.clone()).await.unwrap();
    assert!(first.created);
    let second = boards::ensure(&e.ctx, &local, input.clone()).await.unwrap();
    assert!(!second.created);
    assert_eq!(
        boards::create(&e.ctx, &local, input)
            .await
            .unwrap_err()
            .code(),
        Code::Conflict
    );

    let patch = boards::BoardPatch {
        name: Some("알림".into()),
        ..Default::default()
    };
    assert_eq!(
        boards::update(&e.ctx, &local, "notice", patch)
            .await
            .unwrap()
            .name,
        "알림"
    );
    let bad = boards::BoardPatch {
        kind: Some("wiki".into()),
        ..Default::default()
    };
    assert_eq!(
        boards::update(&e.ctx, &local, "notice", bad)
            .await
            .unwrap_err()
            .code(),
        Code::Validation
    );

    // 글이 있는 게시판은 지울 수 없다 (삭제 상태 글 포함).
    let p = posts::create_draft(&e.ctx, &local, draft("글"))
        .await
        .unwrap();
    posts::delete(&e.ctx, &local, &p.id).await.unwrap();
    assert_eq!(
        boards::delete(&e.ctx, &local, "blog")
            .await
            .unwrap_err()
            .code(),
        Code::Conflict
    );
    boards::delete(&e.ctx, &local, "notice").await.unwrap();
    assert_eq!(
        boards::show(&e.ctx, &local, "notice")
            .await
            .unwrap_err()
            .code(),
        Code::NotFound
    );
    e.db.close().await;
}

#[tokio::test]
async fn ensure_draft_is_idempotent() {
    let e = env().await;
    blog(&e.ctx).await;
    let local = Actor::local();
    let mut input = draft("소개");
    input.slug = Some("about-me".into());
    assert!(
        posts::ensure_draft(&e.ctx, &local, input.clone())
            .await
            .unwrap()
            .created
    );
    assert!(
        !posts::ensure_draft(&e.ctx, &local, input)
            .await
            .unwrap()
            .created
    );
    assert_eq!(scalar(&e.db, "SELECT count(*) FROM posts").await, 1);
    e.db.close().await;
}

// ---- U9 ----

use maru_core::model::{MenuItem, MenuLink, PageStatus};
use maru_core::services::{menus, pages, settings};

#[tokio::test]
async fn pages_lifecycle_and_privacy() {
    let e = env().await;
    let local = Actor::local();
    let anon = anonymous_actor(&e.ctx).await.unwrap();
    let input = pages::NewPage {
        slug: "about".into(),
        title: "소개".into(),
        body_md: "저는 개발자입니다.".into(),
        layout: "default".into(),
        sort_order: 0,
    };
    let p = pages::create(&e.ctx, &local, input.clone()).await.unwrap();
    assert_eq!(p.status, PageStatus::Draft);
    assert!(!pages::ensure(&e.ctx, &local, input).await.unwrap().created);

    // 초안은 공개 조회·목록에서 보이지 않는다.
    assert_eq!(
        pages::public_page(&e.ctx, &anon, "about")
            .await
            .unwrap_err()
            .code(),
        Code::NotFound
    );
    assert_eq!(
        pages::get(&e.ctx, &anon, "about").await.unwrap_err().code(),
        Code::NotFound
    );
    assert!(pages::public_pages(&e.ctx, &anon).await.unwrap().is_empty());

    pages::publish(&e.ctx, &local, "about").await.unwrap();
    assert_eq!(
        pages::public_page(&e.ctx, &anon, "about")
            .await
            .unwrap()
            .title,
        "소개"
    );

    let patch = pages::PagePatch {
        body_md: Some("고침".into()),
        ..Default::default()
    };
    pages::update(&e.ctx, &local, "about", patch).await.unwrap();
    assert_eq!(
        scalar(
            &e.db,
            "SELECT count(*) FROM revisions WHERE page_id IS NOT NULL"
        )
        .await,
        2
    );
    pages::delete(&e.ctx, &local, "about").await.unwrap();
    assert!(pages::public_pages(&e.ctx, &anon).await.unwrap().is_empty());
    assert_eq!(
        pages::restore(&e.ctx, &local, "about")
            .await
            .unwrap()
            .status,
        PageStatus::Published
    );

    // 에이전트 기본 토큰은 구조 변경 권한이 없다.
    let agent = Actor::agent("t".into(), &Scope::DEFAULT, None);
    let err = pages::publish(&e.ctx, &agent, "about").await.unwrap_err();
    assert_eq!(err.code(), Code::Forbidden);
    e.db.close().await;
}

#[tokio::test]
async fn menu_set_get_and_rejects_bad_trees() {
    let e = env().await;
    let local = Actor::local();
    let tree = vec![
        MenuItem {
            label: "홈".into(),
            link: MenuLink::Home,
            children: vec![],
        },
        MenuItem {
            label: "글".into(),
            link: MenuLink::Board {
                slug: "blog".into(),
            },
            children: vec![MenuItem {
                label: "소개".into(),
                link: MenuLink::Page {
                    slug: "about".into(),
                },
                children: vec![],
            }],
        },
    ];
    menus::set(&e.ctx, &local, "header", tree.clone())
        .await
        .unwrap();
    assert_eq!(menus::get(&e.ctx, "header").await.unwrap(), tree);
    assert!(menus::get(&e.ctx, "footer").await.unwrap().is_empty());

    // 같은 트리를 다시 넣으면 감사 기록이 늘지 않는다.
    let audits = scalar(&e.db, "SELECT count(*) FROM audit_log").await;
    menus::set(&e.ctx, &local, "header", tree).await.unwrap();
    assert_eq!(
        scalar(&e.db, "SELECT count(*) FROM audit_log").await,
        audits
    );

    let bad_url = vec![MenuItem {
        label: "x".into(),
        link: MenuLink::Url {
            href: "javascript:alert(1)".into(),
        },
        children: vec![],
    }];
    assert_eq!(
        menus::set(&e.ctx, &local, "header", bad_url)
            .await
            .unwrap_err()
            .code(),
        Code::Validation
    );
    assert_eq!(
        menus::set(&e.ctx, &local, "sidebar", vec![])
            .await
            .unwrap_err()
            .code(),
        Code::Validation
    );
    // 실패한 시도는 기존 메뉴를 건드리지 않는다.
    assert_eq!(menus::get(&e.ctx, "header").await.unwrap().len(), 2);
    e.db.close().await;
}

#[tokio::test]
async fn settings_update_and_audit() {
    let e = env().await;
    let local = Actor::local();
    assert_eq!(settings::site(&e.ctx).await.unwrap().name, "Maru");
    let patch = settings::SettingsPatch {
        name: Some("홍길동의 블로그".into()),
        description: Some("개발과 일상".into()),
        ..Default::default()
    };
    let s = settings::update(&e.ctx, &local, patch).await.unwrap();
    assert_eq!(s.name, "홍길동의 블로그");
    assert_eq!(
        settings::site(&e.ctx).await.unwrap().description,
        "개발과 일상"
    );
    assert_eq!(
        scalar(
            &e.db,
            "SELECT count(*) FROM audit_log WHERE action = 'settings.update'"
        )
        .await,
        1
    );
    let bad = settings::SettingsPatch {
        tz_offset_min: Some(10_000),
        ..Default::default()
    };
    assert_eq!(
        settings::update(&e.ctx, &local, bad)
            .await
            .unwrap_err()
            .code(),
        Code::Validation
    );
    let agent = Actor::agent("t".into(), &Scope::DEFAULT, None);
    let patch = settings::SettingsPatch {
        name: Some("x".into()),
        ..Default::default()
    };
    assert_eq!(
        settings::update(&e.ctx, &agent, patch)
            .await
            .unwrap_err()
            .code(),
        Code::Forbidden
    );
    e.db.close().await;
}
