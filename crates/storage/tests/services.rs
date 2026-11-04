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
use maru_storage::{DataDir, Db, FsBlobs, SqliteStore};

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
    let ctx = ctx_for(&db, &dir);
    Env {
        _tmp: tmp,
        dir,
        db,
        ctx,
    }
}

fn ctx_for(db: &Db, dir: &DataDir) -> Ctx {
    Ctx {
        store: Arc::new(SqliteStore::new(db.clone())),
        blobs: Arc::new(FsBlobs::new(dir.media_dir())),
        site_url: "http://127.0.0.1:8080".into(),
        extensions: maru_core::store::no_extensions(),
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
            layout: None,
            field_schema: None,
            list_order: None,
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
        cover: None,
        category: None,
        tags: vec![],
        notice: false,
        secret: false,
        meta: Default::default(),
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
    let ctx2 = ctx_for(&other, &e.dir);

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
            layout: None,
            field_schema: None,
            list_order: None,
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
async fn check_slug_reports_taken_and_derived_addresses() {
    let e = env().await;
    blog(&e.ctx).await;
    let local = Actor::local();
    let a = posts::create_draft(&e.ctx, &local, draft("같은 제목"))
        .await
        .unwrap();
    let audits = scalar(&e.db, "SELECT count(*) FROM audit_log").await;
    let check =
        |slug: Option<&str>, title: Option<&str>, post: Option<&str>| posts::SlugCheckInput {
            board: "blog".into(),
            slug: slug.map(Into::into),
            title: title.map(Into::into),
            post_id: post.map(Into::into),
        };

    // 다른 글이 쓰는 주소: 겹침 + 다음 후보 (저장하면 실제로 막히는 경우와 같다)
    let c = posts::check_slug(&e.ctx, &local, check(Some(" 같은-제목 "), None, None))
        .await
        .unwrap();
    assert_eq!((c.slug.as_str(), c.available), ("같은-제목", false));
    assert_eq!(c.suggestion.as_deref(), Some("같은-제목-2"));
    assert_eq!(c.path, "/b/blog/%EA%B0%99%EC%9D%80-%EC%A0%9C%EB%AA%A9");
    // 고치는 글 자신의 주소는 겹침이 아니다
    let c = posts::check_slug(&e.ctx, &local, check(Some("같은-제목"), None, Some(&a.id)))
        .await
        .unwrap();
    assert!(c.available && c.suggestion.is_none());
    // 주소를 비우면 저장할 때와 같은 규칙으로 제목에서 만든다
    let c = posts::check_slug(&e.ctx, &local, check(None, Some("같은 제목"), None))
        .await
        .unwrap();
    assert_eq!(
        (c.slug.as_str(), c.available, c.derived),
        ("같은-제목-2", true, true)
    );
    let b = posts::create_draft(&e.ctx, &local, draft("같은 제목"))
        .await
        .unwrap();
    assert_eq!(b.slug, c.slug);
    // 정규화: 대문자·공백
    let c = posts::check_slug(&e.ctx, &local, check(Some("New Post"), None, None))
        .await
        .unwrap();
    assert_eq!((c.slug.as_str(), c.available), ("new-post", true));
    // 없는 게시판, 쓰기 권한 없는 손님
    let mut missing = check(Some("x"), None, None);
    missing.board = "nope".into();
    let err = posts::check_slug(&e.ctx, &local, missing)
        .await
        .unwrap_err();
    assert_eq!(err.code(), Code::NotFound);
    let guest = anonymous_actor(&e.ctx).await.unwrap();
    assert!(
        posts::check_slug(&e.ctx, &guest, check(Some("x"), None, None))
            .await
            .is_err()
    );
    // 확인만 하므로 감사 기록이 늘지 않는다 (create_draft 한 번만 늘었다)
    assert_eq!(
        scalar(&e.db, "SELECT count(*) FROM audit_log").await,
        audits + 1
    );
    e.db.close().await;
}

#[tokio::test]
async fn preview_returns_summary_and_site_photos() {
    let e = env().await;
    let local = Actor::local();
    let out = posts::preview(
        &e.ctx,
        &local,
        posts::PreviewInput {
            body_md: "## 제목\n\n첫 **문단** ![밖](https://example.test/a.jpg)\n\n![가](/media/2026/11/a.jpg) ![나](/media/2026/11/b.png)".into(),
        },
    )
    .unwrap();
    assert!(out.html.contains("<h2"), "{}", out.html);
    assert!(out.summary.starts_with("제목 첫 문단"), "{}", out.summary);
    assert_eq!(out.images, ["/media/2026/11/a.jpg", "/media/2026/11/b.png"]);
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
            layout: None,
            field_schema: None,
            list_order: None,
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
    let own = posts::list(&e.ctx, &writer, None, None, None, PageReq::default())
        .await
        .unwrap();
    assert_eq!(own.total, 1);
    let all = posts::list(&e.ctx, &editor, None, None, None, PageReq::default())
        .await
        .unwrap();
    assert_eq!(all.total, 2);
    e.db.close().await;
}

#[tokio::test]
async fn board_layout_defaults_and_validation() {
    let e = env().await;
    let local = Actor::local();
    blog(&e.ctx).await;
    let b = boards::show(&e.ctx, &local, "blog").await.unwrap();
    assert_eq!(b.layout, "");
    assert_eq!(b.effective_layout(), "webzine");
    let set = |l: &str| boards::BoardPatch {
        layout: Some(l.into()),
        ..Default::default()
    };
    let card = boards::update(&e.ctx, &local, "blog", set("card"))
        .await
        .unwrap();
    assert_eq!(card.effective_layout(), "card");
    // 다시 읽어도 남아 있다 (DB 열).
    assert_eq!(
        boards::show(&e.ctx, &local, "blog").await.unwrap().layout,
        "card"
    );
    let err = boards::update(&e.ctx, &local, "blog", set("masonry"))
        .await
        .unwrap_err();
    assert_eq!(err.code(), Code::Validation);
    // 빈 값은 종류 기본값으로 되돌린다.
    let reset = boards::update(&e.ctx, &local, "blog", set(""))
        .await
        .unwrap();
    assert_eq!(reset.effective_layout(), "webzine");
    let free = boards::create(
        &e.ctx,
        &local,
        NewBoard {
            slug: "free".into(),
            name: "자유".into(),
            kind: "free".into(),
            policy: None,
            sort_order: 0,
            layout: None,
            field_schema: None,
            list_order: None,
        },
    )
    .await
    .unwrap();
    assert_eq!(free.effective_layout(), "list");
    let changes = scalar(
        &e.db,
        "SELECT count(*) FROM audit_log WHERE action = 'board.update'",
    )
    .await;
    assert_eq!(changes, 2, "바꾼 두 번만 기록 (거부는 없음)");
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
        layout: None,
        field_schema: None,
        list_order: None,
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
        kind: Some("chatroom".into()),
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
    // 목차 기준: 기본 3, 0 은 끔, 20 을 넘으면 20 으로
    assert_eq!(settings::site(&e.ctx).await.unwrap().toc_min_headings, 3);
    for (input, stored) in [(0, 0), (99, 20)] {
        let patch = settings::SettingsPatch {
            toc_min_headings: Some(input),
            ..Default::default()
        };
        let s = settings::update(&e.ctx, &local, patch).await.unwrap();
        assert_eq!(s.toc_min_headings, stored);
    }
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

// ---- U10 ----

use maru_core::design::Tokens;
use maru_core::services::{design, presets};

#[tokio::test]
async fn tokens_update_validate_and_audit() {
    let e = env().await;
    let local = Actor::local();
    assert_eq!(design::tokens(&e.ctx).await.unwrap(), Tokens::default());
    let t = design::update(
        &e.ctx,
        &local,
        serde_json::json!({ "color": { "primary": "#1f4f8f" }, "type_scale": { "base": 18 } }),
    )
    .await
    .unwrap();
    assert_eq!(t.color.primary, "#1f4f8f");
    assert_eq!(design::tokens(&e.ctx).await.unwrap().type_scale.base, 18);

    // 대비 부족·주입은 저장되지 않고 이전 값이 남는다.
    let low = serde_json::json!({ "color": { "muted": "#dddddd" } });
    assert_eq!(
        design::update(&e.ctx, &local, low)
            .await
            .unwrap_err()
            .code(),
        Code::Validation
    );
    let inject = serde_json::json!({ "font": { "body": "x}</style><script>" } });
    assert_eq!(
        design::update(&e.ctx, &local, inject)
            .await
            .unwrap_err()
            .code(),
        Code::Validation
    );
    assert_eq!(
        design::tokens(&e.ctx).await.unwrap().color.primary,
        "#1f4f8f"
    );
    assert_eq!(
        scalar(
            &e.db,
            "SELECT count(*) FROM audit_log WHERE action = 'design.tokens'"
        )
        .await,
        1
    );

    // 기본 토큰(read, draft)에는 design 스코프가 없다.
    let agent = Actor::agent("t".into(), &Scope::DEFAULT, None);
    let err = design::update(&e.ctx, &agent, serde_json::json!({}))
        .await
        .unwrap_err();
    assert_eq!(err.code(), Code::Forbidden);
    design::reset(&e.ctx, &local).await.unwrap();
    assert_eq!(design::tokens(&e.ctx).await.unwrap(), Tokens::default());
    e.db.close().await;
}

#[tokio::test]
async fn preset_apply_is_idempotent_and_keeps_user_edits() {
    let e = env().await;
    let local = Actor::local();
    let preset: presets::Preset = serde_json::from_value(serde_json::json!({
        "id": "t",
        "name": "테스트",
        "ops": [
            { "op": "board.ensure", "slug": "notes", "name": "노트" },
            { "op": "page.ensure", "slug": "about", "title": "소개", "publish": true },
            { "op": "menu.ensure", "location": "header",
              "items": [{ "label": "노트", "link": { "kind": "board", "slug": "notes" } }] },
        ]
    }))
    .unwrap();
    let first = presets::apply(&e.ctx, &local, &preset).await.unwrap();
    assert!(first.iter().all(|r| r.changed), "{first:?}");
    let audits = scalar(&e.db, "SELECT count(*) FROM audit_log").await;
    let second = presets::apply(&e.ctx, &local, &preset).await.unwrap();
    assert!(second.iter().all(|r| !r.changed), "{second:?}");
    assert_eq!(
        scalar(&e.db, "SELECT count(*) FROM audit_log").await,
        audits
    );

    // 사용자가 바꾼 메뉴·숨긴 페이지는 다시 적용해도 그대로다.
    menus::set(&e.ctx, &local, "header", vec![]).await.unwrap();
    let mine = vec![MenuItem {
        label: "내 메뉴".into(),
        link: MenuLink::Home,
        children: vec![],
    }];
    menus::set(&e.ctx, &local, "header", mine.clone())
        .await
        .unwrap();
    pages::hide(&e.ctx, &local, "about").await.unwrap();
    presets::apply(&e.ctx, &local, &preset).await.unwrap();
    assert_eq!(menus::get(&e.ctx, "header").await.unwrap(), mine);
    assert_eq!(
        pages::get(&e.ctx, &local, "about").await.unwrap().status,
        PageStatus::Hidden
    );
    e.db.close().await;
}

// ---- 예약 발행·대표 이미지 ----

fn later(days: i64) -> String {
    maru_core::clock::format(time::OffsetDateTime::now_utc() + time::Duration::days(days))
}

#[tokio::test]
async fn scheduled_post_is_published_when_due() {
    let e = env().await;
    blog(&e.ctx).await;
    let local = Actor::local();
    let p = posts::create_draft(&e.ctx, &local, draft("예약 글"))
        .await
        .unwrap();
    let at = later(1);
    let patch = posts::PostPatch {
        scheduled_at: Some(at.clone()),
        ..Default::default()
    };
    let p = posts::update(&e.ctx, &local, &p.id, patch.clone())
        .await
        .unwrap();
    assert_eq!(p.scheduled_at.as_deref(), Some(at.as_str()));
    assert_eq!(p.status, PostStatus::Draft);
    // 같은 값을 다시 걸어도 그대로 (멱등)
    let again = posts::update(&e.ctx, &local, &p.id, patch).await.unwrap();
    assert_eq!(again.updated_at, p.updated_at);
    // 아직 때가 아니면 아무것도 하지 않는다
    assert!(posts::publish_due(&e.ctx).await.unwrap().is_empty());

    // 시각이 지난 것처럼 만든다
    sqlx::query("UPDATE posts SET scheduled_at = '2000-01-01T00:00:00.000000Z'")
        .execute(e.db.write_pool())
        .await
        .unwrap();
    let done = posts::publish_due(&e.ctx).await.unwrap();
    assert_eq!(done.len(), 1);
    let published = done[0].1.as_ref().unwrap();
    assert_eq!(published.status, PostStatus::Published);
    assert!(published.scheduled_at.is_none());
    assert!(published.published_at.is_some());
    assert_eq!(
        scalar(
            &e.db,
            "SELECT count(*) FROM audit_log WHERE action = 'post.publish_scheduled'"
        )
        .await,
        1
    );
    // 두 번째 주기에는 할 일이 없다
    assert!(posts::publish_due(&e.ctx).await.unwrap().is_empty());
    e.db.close().await;
}

#[tokio::test]
async fn schedule_rules() {
    let e = env().await;
    blog(&e.ctx).await;
    let local = Actor::local();
    let p = posts::create_draft(&e.ctx, &local, draft("규칙"))
        .await
        .unwrap();
    let set = |at: &str| posts::PostPatch {
        scheduled_at: Some(at.into()),
        ..Default::default()
    };
    for bad in ["내일", "2000-01-01T00:00:00Z", &later(400)] {
        let err = posts::update(&e.ctx, &local, &p.id, set(bad))
            .await
            .unwrap_err();
        assert_eq!(err.code(), Code::Validation, "{bad}");
    }
    // 발행 권한 없는 에이전트는 예약할 수 없다
    let writer = Actor::agent("w".into(), &Scope::DEFAULT, None);
    let mine = posts::create_draft(&e.ctx, &writer, draft("에이전트 글"))
        .await
        .unwrap();
    let err = posts::update(&e.ctx, &writer, &mine.id, set(&later(1)))
        .await
        .unwrap_err();
    assert_eq!(err.code(), Code::Forbidden);
    // 공개된 글은 예약할 수 없다
    posts::publish(&e.ctx, &local, &p.id).await.unwrap();
    let err = posts::update(&e.ctx, &local, &p.id, set(&later(1)))
        .await
        .unwrap_err();
    assert_eq!(err.code(), Code::Conflict);
    // 숨기면 다시 예약할 수 있고, 삭제하면 예약도 지워진다
    posts::hide(&e.ctx, &local, &p.id).await.unwrap();
    let p = posts::update(&e.ctx, &local, &p.id, set(&later(2)))
        .await
        .unwrap();
    assert!(p.scheduled_at.is_some());
    let p = posts::delete(&e.ctx, &local, &p.id).await.unwrap();
    assert!(p.scheduled_at.is_none());
    // 빈 문자열은 취소
    let q = posts::create_draft(&e.ctx, &local, draft("취소"))
        .await
        .unwrap();
    posts::update(&e.ctx, &local, &q.id, set(&later(1)))
        .await
        .unwrap();
    let q = posts::update(&e.ctx, &local, &q.id, set("")).await.unwrap();
    assert!(q.scheduled_at.is_none());
    e.db.close().await;
}

#[tokio::test]
async fn cover_is_checked_and_falls_back_to_first_photo() {
    let e = env().await;
    blog(&e.ctx).await;
    let local = Actor::local();
    let mut input = draft("사진 글");
    input.body_md = "글 ![밖](https://example.test/a.jpg) ![첫](/media/2026/11/abc.jpg)".into();
    let p = posts::create_draft(&e.ctx, &local, input).await.unwrap();
    assert_eq!(p.cover, None);
    assert_eq!(
        posts::cover_of(&p).as_deref(),
        Some("/media/2026/11/abc.jpg")
    );
    let set = |c: &str| posts::PostPatch {
        cover: Some(c.into()),
        ..Default::default()
    };
    let p = posts::update(&e.ctx, &local, &p.id, set(" /media/2026/11/def.png "))
        .await
        .unwrap();
    assert_eq!(p.cover.as_deref(), Some("/media/2026/11/def.png"));
    assert_eq!(
        posts::cover_of(&p).as_deref(),
        Some("/media/2026/11/def.png")
    );
    for bad in [
        "https://example.test/x.jpg",
        "/media/../maru.db",
        "/media/a b.jpg",
        "/media/a\".jpg",
        "javascript:alert(1)",
    ] {
        let err = posts::update(&e.ctx, &local, &p.id, set(bad))
            .await
            .unwrap_err();
        assert_eq!(err.code(), Code::Validation, "{bad}");
    }
    let p = posts::update(&e.ctx, &local, &p.id, set("")).await.unwrap();
    assert_eq!(p.cover, None);
    e.db.close().await;
}

#[tokio::test]
async fn templates_upsert_by_name_and_protect_builtins() {
    use maru_core::services::templates::{self, SaveTemplate};
    let e = env().await;
    let local = Actor::local();
    let save = |name: &str, body: &str| SaveTemplate {
        name: name.into(),
        body_md: body.into(),
    };
    let builtin = templates::list(&e.ctx, &local).await.unwrap();
    assert!(builtin.iter().any(|t| t.name == "여행기" && t.builtin));

    let a = templates::save(&e.ctx, &local, save("주간 회고", "## 한 일\r\n\r\n- "))
        .await
        .unwrap();
    assert_eq!(a.body_md, "## 한 일\n\n- ");
    // 같은 내용이면 그대로, 다른 내용이면 같은 ID 로 덮어쓴다
    let same = templates::save(&e.ctx, &local, save("주간 회고", "## 한 일\n\n- "))
        .await
        .unwrap();
    assert_eq!(same.updated_at, a.updated_at);
    let b = templates::save(&e.ctx, &local, save(" 주간 회고 ", "## 배운 것"))
        .await
        .unwrap();
    assert_eq!(b.id, a.id);
    assert_eq!(b.body_md, "## 배운 것");
    assert_eq!(
        scalar(&e.db, "SELECT count(*) FROM post_templates").await,
        1
    );
    let list = templates::list(&e.ctx, &local).await.unwrap();
    assert_eq!(list.len(), builtin.len() + 1);

    // 기본 틀 이름·ID 는 저장·삭제할 수 없다
    let err = templates::save(&e.ctx, &local, save("여행기", "x"))
        .await
        .unwrap_err();
    assert_eq!(err.code(), Code::Conflict);
    let err = templates::delete(&e.ctx, &local, "builtin-travel")
        .await
        .unwrap_err();
    assert_eq!(err.code(), Code::Conflict);
    // 기본 토큰(초안 권한)은 읽기만
    let writer = Actor::agent("w".into(), &Scope::DEFAULT, None);
    assert!(templates::list(&e.ctx, &writer).await.is_ok());
    let err = templates::save(&e.ctx, &writer, save("남의 틀", "x"))
        .await
        .unwrap_err();
    assert_eq!(err.code(), Code::Forbidden);

    // 이름으로 지운다. 감사 기록이 남는다
    templates::delete(&e.ctx, &local, "주간 회고")
        .await
        .unwrap();
    let err = templates::delete(&e.ctx, &local, "주간 회고")
        .await
        .unwrap_err();
    assert_eq!(err.code(), Code::NotFound);
    assert_eq!(
        scalar(
            &e.db,
            "SELECT count(*) FROM audit_log WHERE action LIKE 'template.%'"
        )
        .await,
        3
    );
    e.db.close().await;
}

// ---- 분류·태그·공지·비밀글 (GAP C1~C4) ----

use maru_core::services::{categories, tags};

fn in_category(title: &str, category: &str) -> NewPost {
    NewPost {
        category: Some(category.into()),
        ..draft(title)
    }
}

fn new_category(name: &str) -> categories::NewCategory {
    categories::NewCategory {
        board: "blog".into(),
        name: name.into(),
        slug: None,
    }
}

#[tokio::test]
async fn categories_are_idempotent_and_delete_clears_posts() {
    let e = env().await;
    blog(&e.ctx).await;
    let local = Actor::local();
    let travel = categories::create(&e.ctx, &local, new_category("제주 여행"))
        .await
        .unwrap();
    assert_eq!(travel.slug, "제주-여행");
    let again = categories::create(&e.ctx, &local, new_category("제주 여행"))
        .await
        .unwrap();
    assert_eq!(again.id, travel.id, "같은 주소면 기존 분류");
    let food = categories::create(&e.ctx, &local, new_category("맛집"))
        .await
        .unwrap();
    assert!(food.sort_order > travel.sort_order, "새 분류는 뒤에");

    // 구조 편집 권한이 없으면 만들 수 없다.
    let writer = Actor::agent("w".into(), &Scope::DEFAULT, None);
    assert_eq!(
        categories::create(&e.ctx, &writer, new_category("몰래"))
            .await
            .unwrap_err()
            .code(),
        Code::Forbidden
    );

    // 없는 분류로 글을 쓰면 다음 명령을 알려 준다.
    let err = posts::create_draft(&e.ctx, &local, in_category("글", "없는-분류"))
        .await
        .unwrap_err();
    assert_eq!(err.code(), Code::NotFound);
    assert!(
        err.next_commands()
            .iter()
            .any(|c| c.contains("maru category list"))
    );

    let post = posts::create_draft(&e.ctx, &local, in_category("한라산", "제주-여행"))
        .await
        .unwrap();
    assert_eq!(post.category_id.as_deref(), Some(travel.id.as_str()));

    // 이름 바꾸기·순서
    let patch = categories::CategoryPatch {
        name: Some("제주".into()),
        sort_order: Some(9),
        ..Default::default()
    };
    let renamed = categories::update(&e.ctx, &local, "blog", "제주-여행", patch)
        .await
        .unwrap();
    assert_eq!(
        (renamed.name.as_str(), renamed.slug.as_str()),
        ("제주", "제주-여행")
    );
    let list = categories::list(&e.ctx, &local, "blog").await.unwrap();
    assert_eq!(list.first().map(|c| c.slug.as_str()), Some("맛집"));

    // 지우면 글은 남고 분류만 빈다. 감사 기록에 비운 글 수가 남는다.
    categories::delete(&e.ctx, &local, "blog", "제주-여행")
        .await
        .unwrap();
    let after = posts::get(&e.ctx, &local, &post.id).await.unwrap();
    assert_eq!(after.category_id, None);
    assert_eq!(
        scalar(
            &e.db,
            "SELECT count(*) FROM audit_log WHERE action = 'category.delete'
             AND json_extract(diff, '$.posts_cleared') = 1"
        )
        .await,
        1
    );

    // 분류 비우기(빈 문자열)
    let p2 = posts::create_draft(&e.ctx, &local, in_category("국밥", "맛집"))
        .await
        .unwrap();
    let cleared = posts::update(
        &e.ctx,
        &local,
        &p2.id,
        posts::PostPatch {
            category: Some(String::new()),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    assert_eq!(cleared.category_id, None);
    e.db.close().await;
}

fn with_tags(title: &str, t: &[&str]) -> NewPost {
    NewPost {
        tags: t.iter().map(|s| (*s).to_owned()).collect(),
        ..draft(title)
    }
}

#[tokio::test]
async fn tags_are_normalized_ordered_and_counted() {
    let e = env().await;
    blog(&e.ctx).await;
    let local = Actor::local();
    let anon = anonymous_actor(&e.ctx).await.unwrap();

    let a = posts::create_draft(
        &e.ctx,
        &local,
        with_tags("첫 글", &["#Rust", " 제주   여행 ", "rust", "  "]),
    )
    .await
    .unwrap();
    assert_eq!(
        a.tags,
        vec!["Rust", "제주 여행"],
        "앞 #·공백 정리, 중복 제거, 순서 유지"
    );
    // 이미 있는 태그는 처음 이름으로 맞춘다.
    let b = posts::create_draft(&e.ctx, &local, with_tags("둘째 글", &["RUST"]))
        .await
        .unwrap();
    assert_eq!(b.tags, vec!["Rust"]);
    let reread = posts::get(&e.ctx, &local, &a.id).await.unwrap();
    assert_eq!(reread.tags, a.tags, "저장한 순서대로 다시 읽힌다");

    // 공개 전에는 태그 목록에 없다.
    assert!(tags::list(&e.ctx, &anon).await.unwrap().is_empty());
    posts::publish(&e.ctx, &local, &a.id).await.unwrap();
    posts::publish(&e.ctx, &local, &b.id).await.unwrap();
    let list = tags::list(&e.ctx, &anon).await.unwrap();
    let rust = list.iter().find(|t| t.slug == "rust").unwrap();
    assert_eq!(rust.posts, 2);
    let (tag, by_tag) = posts::public_tag(&e.ctx, &anon, "Rust", PageReq::default())
        .await
        .unwrap();
    assert_eq!((tag.name.as_str(), by_tag.total), ("Rust", 2));

    // 태그 바꾸기·모두 빼기
    let changed = posts::update(
        &e.ctx,
        &local,
        &a.id,
        posts::PostPatch {
            tags: Some(vec!["여행".into(), "Rust".into()]),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    assert_eq!(changed.tags, vec!["여행", "Rust"]);
    let none = posts::update(
        &e.ctx,
        &local,
        &a.id,
        posts::PostPatch {
            tags: Some(vec![]),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    assert!(none.tags.is_empty());

    // 한도
    let many: Vec<String> = (0..21).map(|i| format!("t{i}")).collect();
    let err = posts::create_draft(
        &e.ctx,
        &local,
        NewPost {
            tags: many,
            ..draft("많음")
        },
    )
    .await
    .unwrap_err();
    assert_eq!(err.code(), Code::Validation);
    let long = "가".repeat(31);
    let err = posts::create_draft(&e.ctx, &local, with_tags("긺", &[long.as_str()]))
        .await
        .unwrap_err();
    assert_eq!(err.code(), Code::Validation);
    let err = posts::create_draft(&e.ctx, &local, with_tags("기호", &["!!!"]))
        .await
        .unwrap_err();
    assert_eq!(err.code(), Code::Validation);
    e.db.close().await;
}

#[tokio::test]
async fn notices_need_publish_and_stay_on_top() {
    let e = env().await;
    blog(&e.ctx).await;
    let local = Actor::local();
    let anon = anonymous_actor(&e.ctx).await.unwrap();
    let writer = Actor::agent("w".into(), &Scope::DEFAULT, None);
    let notice = |title: &str| NewPost {
        notice: true,
        ..draft(title)
    };

    assert_eq!(
        posts::create_draft(&e.ctx, &writer, notice("몰래 공지"))
            .await
            .unwrap_err()
            .code(),
        Code::Forbidden
    );
    let mine = posts::create_draft(&e.ctx, &writer, draft("내 글"))
        .await
        .unwrap();
    assert_eq!(
        posts::update(
            &e.ctx,
            &writer,
            &mine.id,
            posts::PostPatch {
                notice: Some(true),
                ..Default::default()
            }
        )
        .await
        .unwrap_err()
        .code(),
        Code::Forbidden
    );

    categories::create(&e.ctx, &local, new_category("일상"))
        .await
        .unwrap();
    let n = posts::create_draft(&e.ctx, &local, notice("공지입니다"))
        .await
        .unwrap();
    posts::publish(&e.ctx, &local, &n.id).await.unwrap();
    for (i, cat) in [None, Some("일상"), Some("일상")].into_iter().enumerate() {
        let input = NewPost {
            category: cat.map(str::to_owned),
            ..draft(&format!("일반 {i}"))
        };
        let p = posts::create_draft(&e.ctx, &local, input).await.unwrap();
        posts::publish(&e.ctx, &local, &p.id).await.unwrap();
    }

    let board = boards::readable(&e.ctx, &anon, "blog").await.unwrap();
    let page = posts::public_board(&e.ctx, &anon, &board, None, PageReq::default())
        .await
        .unwrap();
    assert_eq!(page.notices.len(), 1);
    assert_eq!(page.notices[0].title, "공지입니다");
    assert_eq!(page.posts.total, 3, "공지는 일반 목록에서 빠진다");
    assert_eq!(page.categories.len(), 1);

    // 분류를 골라도 공지는 그대로 위에 있다.
    let page = posts::public_board(&e.ctx, &anon, &board, Some("일상"), PageReq::default())
        .await
        .unwrap();
    assert_eq!((page.notices.len(), page.posts.total), (1, 2));
    assert_eq!(page.category.map(|c| c.slug), Some("일상".to_owned()));
    assert_eq!(
        posts::public_board(&e.ctx, &anon, &board, Some("없음"), PageReq::default())
            .await
            .unwrap_err()
            .code(),
        Code::NotFound
    );
    e.db.close().await;
}

#[tokio::test]
async fn secret_post_body_is_only_for_author_and_moderators() {
    let e = env().await;
    blog(&e.ctx).await;
    let local = Actor::local();
    let anon = anonymous_actor(&e.ctx).await.unwrap();
    let moderator = Actor::agent(
        "ed".into(),
        &[Scope::Read, Scope::Draft, Scope::Publish],
        None,
    );
    let reader = Actor::agent("r".into(), &[Scope::Read], None);

    let input = NewPost {
        secret: true,
        meta: Default::default(),
        tags: vec!["비밀태그".into()],
        excerpt: Some("요약도 비밀".into()),
        body_md: "비밀 본문입니다.".into(),
        ..draft("비밀 상담")
    };
    let s = posts::create_draft(&e.ctx, &local, input).await.unwrap();
    posts::publish(&e.ctx, &local, &s.id).await.unwrap();

    // 손님·읽기 토큰: 본문은 FORBIDDEN(글이 있다는 것은 숨기지 않는다), 목록에는 제목만
    for who in [&anon, &reader] {
        let err = posts::public_post(&e.ctx, who, "blog", &s.slug)
            .await
            .unwrap_err();
        assert_eq!(err.code(), Code::Forbidden);
        assert_eq!(err.message_key(), "error.post_secret");
        assert_eq!(
            posts::get(&e.ctx, who, &s.id).await.unwrap_err().code(),
            Code::Forbidden
        );
        let list = posts::public_posts(&e.ctx, who, None, PageReq::default())
            .await
            .unwrap();
        let item = list.items.iter().find(|p| p.id == s.id).unwrap();
        assert_eq!(item.title, "비밀 상담");
        assert!(item.is_secret);
        assert!(item.body_md.is_empty() && item.excerpt.is_none() && item.tags.is_empty());
        assert!(item.cover.is_none());
    }
    // 태그 목록·태그 페이지·피드용 목록에는 아예 없다.
    assert!(tags::list(&e.ctx, &anon).await.unwrap().is_empty());
    let (_, by_tag) = posts::public_tag(&e.ctx, &anon, "비밀태그", PageReq::default())
        .await
        .unwrap();
    assert_eq!(by_tag.total, 0);
    let feed = posts::public_posts_where(
        &e.ctx,
        &anon,
        None,
        &posts::PublicFilter {
            exclude_secret: true,
            ..Default::default()
        },
        PageReq::default(),
    )
    .await
    .unwrap();
    assert_eq!(feed.total, 0);

    // 작성자(로컬)와 관리자는 본문을 읽는다.
    for who in [&local, &moderator] {
        let (_, p) = posts::public_post(&e.ctx, who, "blog", &s.slug)
            .await
            .unwrap();
        assert!(p.body_md.contains("비밀 본문"));
        let list = posts::public_posts(&e.ctx, who, None, PageReq::default())
            .await
            .unwrap();
        assert!(
            list.items
                .iter()
                .any(|p| p.id == s.id && !p.body_md.is_empty())
        );
    }

    // 비밀을 풀면 누구나 읽는다.
    posts::update(
        &e.ctx,
        &local,
        &s.id,
        posts::PostPatch {
            secret: Some(false),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    posts::public_post(&e.ctx, &anon, "blog", &s.slug)
        .await
        .unwrap();
    e.db.close().await;
}
