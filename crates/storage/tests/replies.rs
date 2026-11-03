//! 답변글 (GAP C9, ADR-0056). 실제 SQLite 파일로 스레드 순서·깊이 한도·정책·비밀 답변을 검증한다.
#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::sync::Arc;

use maru_core::actor::{Actor, Permission, rank};
use maru_core::model::{BoardPolicy, PageReq, PostStatus};
use maru_core::services::board_posts::{self, BoardWrite};
use maru_core::services::boards::{self, BoardPatch, NewBoard};
use maru_core::services::categories::{self, NewCategory};
use maru_core::services::members::{self, SignupInput};
use maru_core::services::posts::{self, NewPost, ReplyInput};
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
    for (slug, kind) in [("qna", "qna"), ("free", "free")] {
        boards::create(
            &ctx,
            &local,
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

fn root(board: &str, title: &str) -> NewPost {
    NewPost {
        board: board.into(),
        title: title.into(),
        slug: None,
        body_md: "질문 본문".into(),
        excerpt: None,
        cover: None,
        category: None,
        tags: Vec::new(),
        notice: false,
        secret: false,
        meta: Default::default(),
    }
}

fn answer(title: &str) -> ReplyInput {
    ReplyInput {
        title: title.into(),
        body_md: "답변 본문".into(),
        slug: None,
        tags: Vec::new(),
        secret: false,
        meta: Default::default(),
    }
}

async fn publish_root(ctx: &Ctx, board: &str, title: &str) -> String {
    let local = Actor::local();
    let p = posts::create_draft(ctx, &local, root(board, title))
        .await
        .unwrap();
    posts::publish(ctx, &local, &p.id).await.unwrap().id
}

async fn publish_reply(ctx: &Ctx, parent: &str, title: &str) -> String {
    let local = Actor::local();
    let p = posts::reply(ctx, &local, parent, answer(title))
        .await
        .unwrap();
    posts::publish(ctx, &local, &p.id).await.unwrap().id
}

async fn set_reply(ctx: &Ctx, board: &str, role: &str) {
    let local = Actor::local();
    let current = boards::show(ctx, &local, board).await.unwrap().policy;
    boards::update(
        ctx,
        &local,
        board,
        BoardPatch {
            policy: Some(BoardPolicy {
                reply: role.into(),
                ..current
            }),
            ..Default::default()
        },
    )
    .await
    .unwrap();
}

#[tokio::test]
async fn replies_follow_their_thread_in_the_board_list() {
    let e = env().await;
    let local = Actor::local();
    // 문의 게시판은 운영자 답변이 기본으로 열려 있다.
    let qna = boards::show(&e.ctx, &local, "qna").await.unwrap();
    assert_eq!(qna.policy.reply, "editor");
    assert_eq!(
        boards::show(&e.ctx, &local, "free")
            .await
            .unwrap()
            .policy
            .reply,
        "none"
    );
    categories::create(
        &e.ctx,
        &local,
        NewCategory {
            board: "qna".into(),
            name: "결제".into(),
            slug: Some("pay".into()),
        },
    )
    .await
    .unwrap();
    let mut first = root("qna", "첫 질문");
    first.category = Some("pay".into());
    let a = posts::create_draft(&e.ctx, &local, first).await.unwrap();
    let a = posts::publish(&e.ctx, &local, &a.id).await.unwrap().id;
    let b = publish_root(&e.ctx, "qna", "둘째 질문").await;
    let a1 = publish_reply(&e.ctx, &a, "RE: 첫 질문").await;
    let a1a = publish_reply(&e.ctx, &a1, "RE: RE: 첫 질문").await;
    let a2 = publish_reply(&e.ctx, &a, "RE: 첫 질문 (추가)").await;

    let reply = e.ctx.store.post_by_id(&a1a).await.unwrap().unwrap();
    assert_eq!(reply.parent_id.as_deref(), Some(a1.as_str()));
    assert_eq!(reply.thread_id.as_deref(), Some(a.as_str()));
    assert_eq!(reply.thread_path, "0001.0001.");
    assert_eq!(reply.depth(), 2);
    // 분류는 위 글을 따른다.
    let top = e.ctx.store.post_by_id(&a).await.unwrap().unwrap();
    assert!(top.category_id.is_some());
    assert_eq!(reply.category_id, top.category_id);

    let guest = maru_core::services::anonymous_actor(&e.ctx).await.unwrap();
    let page = posts::public_board(&e.ctx, &guest, &qna, None, PageReq::new(1, 20))
        .await
        .unwrap();
    let order: Vec<&str> = page.posts.items.iter().map(|p| p.id.as_str()).collect();
    // 스레드끼리는 맨 위 글 발행순, 스레드 안에서는 순서 경로대로.
    assert_eq!(order, [&b, &a, &a1, &a1a, &a2].map(String::as_str));
}

#[tokio::test]
async fn reply_rules_policy_depth_and_status() {
    let e = env().await;
    let local = Actor::local();
    // 답변을 받지 않는 게시판
    let free = publish_root(&e.ctx, "free", "자유 글").await;
    let err = posts::reply(&e.ctx, &local, &free, answer("RE"))
        .await
        .unwrap_err();
    assert_eq!(err.code(), Code::Forbidden);
    assert_eq!(err.message(), "이 게시판은 답변글을 받지 않습니다.");

    // 초안에는 답변을 달지 않는다.
    let draft = posts::create_draft(&e.ctx, &local, root("qna", "초안"))
        .await
        .unwrap();
    let err = posts::reply(&e.ctx, &local, &draft.id, answer("RE"))
        .await
        .unwrap_err();
    assert_eq!(err.code(), Code::NotFound);

    // 깊이 한도: 다섯 단계까지
    let mut parent = publish_root(&e.ctx, "qna", "깊은 질문").await;
    for i in 1..=posts::MAX_REPLY_DEPTH {
        parent = publish_reply(&e.ctx, &parent, &format!("답변 {i}")).await;
    }
    let err = posts::reply(&e.ctx, &local, &parent, answer("너무 깊음"))
        .await
        .unwrap_err();
    assert_eq!(err.code(), Code::Validation);

    // 회원은 운영자 답변 게시판에 답변을 못 단다.
    let m = member(&e.ctx, "asker@example.test").await;
    let q = publish_root(&e.ctx, "qna", "회원 질문").await;
    let err = posts::reply(&e.ctx, &m, &q, answer("RE"))
        .await
        .unwrap_err();
    assert_eq!(err.code(), Code::Forbidden);
    // 답변 만들기도 감사 기록에 위 글과 함께 남는다.
    let n: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM audit_log WHERE action = 'post.create' AND diff LIKE '%\"parent_id\":\"%'",
    )
    .fetch_one(e.db.read_pool())
    .await
    .unwrap();
    assert_eq!(n, posts::MAX_REPLY_DEPTH as i64);
}

#[tokio::test]
async fn secret_question_answer_is_readable_by_the_asker() {
    let e = env().await;
    let asker = member(&e.ctx, "asker@example.test").await;
    let other = member(&e.ctx, "other@example.test").await;
    // 회원이 공개 화면에서 비밀 질문을 쓴다.
    let q = board_posts::write(
        &e.ctx,
        &asker,
        BoardWrite {
            board: "qna".into(),
            title: "비밀 질문".into(),
            body_md: "주문 번호 문의".into(),
            secret: true,
            ..Default::default()
        },
    )
    .await
    .unwrap();
    assert_eq!(q.status, PostStatus::Published);
    let a = publish_reply(&e.ctx, &q.id, "RE: 비밀 질문").await;
    let a = e.ctx.store.post_by_id(&a).await.unwrap().unwrap();
    assert!(a.is_secret, "비밀 질문의 답변은 비밀글");
    assert_eq!(a.thread_owner, asker.member_id().cloned());

    let (_, seen) = posts::public_post(&e.ctx, &asker, "qna", &a.slug)
        .await
        .unwrap();
    assert_eq!(seen.body_md, "답변 본문");
    let err = posts::public_post(&e.ctx, &other, "qna", &a.slug)
        .await
        .unwrap_err();
    assert_eq!(err.code(), Code::Forbidden);
}

#[tokio::test]
async fn members_reply_from_the_public_screen_when_the_board_allows() {
    let e = env().await;
    set_reply(&e.ctx, "free", "member").await;
    let m = member(&e.ctx, "writer@example.test").await;
    let top = publish_root(&e.ctx, "free", "토론 글").await;
    let board = boards::show(&e.ctx, &Actor::local(), "free").await.unwrap();
    let parent = board_posts::reply_target(&e.ctx, &m, &board, &top)
        .await
        .unwrap();
    assert_eq!(parent.id, top);
    let p = board_posts::write(
        &e.ctx,
        &m,
        BoardWrite {
            board: "free".into(),
            title: "RE: 토론 글".into(),
            body_md: "의견".into(),
            reply_to: Some(top.clone()),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    assert_eq!(p.parent_id.as_deref(), Some(top.as_str()));
    assert_eq!(p.thread_path, "0001.");

    // 닫으면 폼도 열리지 않는다.
    set_reply(&e.ctx, "free", "none").await;
    let board = boards::show(&e.ctx, &Actor::local(), "free").await.unwrap();
    let err = board_posts::reply_target(&e.ctx, &m, &board, &top)
        .await
        .unwrap_err();
    assert_eq!(err.code(), Code::Forbidden);
    // 알 수 없는 역할은 거부
    let current = board.policy.clone();
    let err = boards::update(
        &e.ctx,
        &Actor::local(),
        "free",
        BoardPatch {
            policy: Some(BoardPolicy {
                reply: "anyone".into(),
                ..current
            }),
            ..Default::default()
        },
    )
    .await
    .unwrap_err();
    assert_eq!(err.code(), Code::Validation);
}
