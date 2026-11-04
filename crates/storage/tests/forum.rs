//! 포럼형 게시판 (GAP C14, ADR-0058). 실제 SQLite 파일로 활동순 목록·답변 채택·글 잠금을 검증한다.
#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::sync::Arc;
use std::time::Duration;

use maru_core::actor::{Actor, Permission, rank};
use maru_core::model::PageReq;
use maru_core::services::board_posts::{self, BoardEdit, BoardWrite};
use maru_core::services::boards::{self, BoardPatch, NewBoard};
use maru_core::services::comments::{self, NewComment};
use maru_core::services::members::{self, SignupInput};
use maru_core::services::posts;
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
        extensions: maru_core::store::no_extensions(),
    };
    let local = Actor::local();
    for (slug, kind) in [("forum", "forum"), ("free", "free")] {
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
    settings::update(
        &ctx,
        &local,
        SettingsPatch {
            signup: Some("open".into()),
            signup_verify: Some(false),
            captcha: Some("off".into()),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    Env { _tmp: tmp, db, ctx }
}

async fn member(ctx: &Ctx, email: &str) -> Actor {
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
            Permission::PostComment,
        ],
        rank::MEMBER,
    )
}

fn editor() -> Actor {
    Actor::human(
        "editor-x".into(),
        [
            Permission::PostRead,
            Permission::PostWrite,
            Permission::PostModerate,
        ],
        rank::EDITOR,
    )
}

/// 시각이 밀리초 단위라 같은 값이 되지 않게 조금 쉰다.
async fn tick() {
    tokio::time::sleep(Duration::from_millis(15)).await;
}

async fn write(ctx: &Ctx, who: &Actor, board: &str, title: &str) -> String {
    tick().await;
    board_posts::write(
        ctx,
        who,
        BoardWrite {
            board: board.into(),
            title: title.into(),
            body_md: "본문".into(),
            ..Default::default()
        },
    )
    .await
    .unwrap()
    .id
}

async fn comment(ctx: &Ctx, who: &Actor, post: &str, body: &str) -> String {
    tick().await;
    comments::create(
        ctx,
        who,
        NewComment {
            post_id: post.into(),
            reply_to: None,
            body: body.into(),
            guest: None,
            captcha: None,
        },
    )
    .await
    .unwrap()
    .id
}

async fn titles(ctx: &Ctx, board: &str) -> Vec<String> {
    let local = Actor::local();
    let b = boards::show(ctx, &local, board).await.unwrap();
    posts::public_board(ctx, &local, &b, None, PageReq::new(1, 20))
        .await
        .unwrap()
        .posts
        .items
        .into_iter()
        .map(|p| p.title)
        .collect()
}

async fn audits(db: &Db, action: &str) -> i64 {
    sqlx::query_scalar("SELECT count(*) FROM audit_log WHERE action = ?")
        .bind(action)
        .fetch_one(db.read_pool())
        .await
        .unwrap()
}

#[tokio::test]
async fn forum_lists_by_last_activity() {
    let e = env().await;
    let m = member(&e.ctx, "asker@example.test").await;
    let old = write(&e.ctx, &m, "forum", "오래된 질문").await;
    write(&e.ctx, &m, "forum", "새 질문").await;
    let old_free = write(&e.ctx, &m, "free", "오래된 잡담").await;
    write(&e.ctx, &m, "free", "새 잡담").await;
    assert_eq!(titles(&e.ctx, "forum").await, ["새 질문", "오래된 질문"]);

    // 오래된 글에 댓글이 달리면 포럼은 맨 위로, 자유게시판(새 글순)은 그대로.
    comment(&e.ctx, &m, &old, "저도 궁금합니다").await;
    comment(&e.ctx, &m, &old_free, "ㅋㅋ").await;
    assert_eq!(titles(&e.ctx, "forum").await, ["오래된 질문", "새 질문"]);
    assert_eq!(titles(&e.ctx, "free").await, ["새 잡담", "오래된 잡담"]);
    let p = e.ctx.store.post_by_id(&old).await.unwrap().unwrap();
    assert!(p.last_activity_at.is_some(), "DB 에 활동 시각이 남는다");

    // 게시판마다 순서를 바꿀 수 있다. 모르는 값은 거부.
    let local = Actor::local();
    let b = boards::update(
        &e.ctx,
        &local,
        "free",
        BoardPatch {
            list_order: Some("activity".into()),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    assert_eq!(b.effective_order(), "activity");
    assert_eq!(titles(&e.ctx, "free").await, ["오래된 잡담", "새 잡담"]);
    let err = boards::update(
        &e.ctx,
        &local,
        "free",
        BoardPatch {
            list_order: Some("hot".into()),
            ..Default::default()
        },
    )
    .await
    .unwrap_err();
    assert_eq!(err.code(), Code::Validation);
}

#[tokio::test]
async fn approved_comment_and_reply_count_as_activity() {
    let e = env().await;
    let m = member(&e.ctx, "asker@example.test").await;
    let local = Actor::local();
    let current = boards::show(&e.ctx, &local, "forum").await.unwrap().policy;
    boards::update(
        &e.ctx,
        &local,
        "forum",
        BoardPatch {
            policy: Some(maru_core::model::BoardPolicy {
                comment_approval: maru_core::model::Approval::All,
                reply: "member".into(),
                ..current
            }),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let first = write(&e.ctx, &m, "forum", "첫 글").await;
    let second = write(&e.ctx, &m, "forum", "둘째 글").await;

    // 승인 대기 댓글은 아직 활동이 아니다. 승인하면 활동이 된다.
    let c = comment(&e.ctx, &m, &first, "확인 부탁").await;
    assert_eq!(titles(&e.ctx, "forum").await, ["둘째 글", "첫 글"]);
    tick().await;
    comments::moderate(&e.ctx, &editor(), &c, comments::Moderation::Approve)
        .await
        .unwrap();
    assert_eq!(titles(&e.ctx, "forum").await, ["첫 글", "둘째 글"]);

    // 둘째 글에 답변글이 달리면 그 스레드가 위로 (답변은 위 글 바로 아래).
    tick().await;
    board_posts::write(
        &e.ctx,
        &m,
        BoardWrite {
            board: "forum".into(),
            title: "RE: 둘째 글".into(),
            body_md: "답".into(),
            reply_to: Some(second),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    assert_eq!(
        titles(&e.ctx, "forum").await,
        ["둘째 글", "RE: 둘째 글", "첫 글"]
    );
}

#[tokio::test]
async fn author_or_moderator_accepts_an_answer() {
    let e = env().await;
    let asker = member(&e.ctx, "asker@example.test").await;
    let other = member(&e.ctx, "other@example.test").await;
    let q = write(&e.ctx, &asker, "forum", "질문").await;
    let a1 = comment(&e.ctx, &other, &q, "이렇게 해 보세요").await;
    let a2 = comment(&e.ctx, &other, &q, "아니면 이렇게").await;

    // 남이 채택할 수는 없다.
    let err = posts::accept(&e.ctx, &other, &q, Some(&a1))
        .await
        .unwrap_err();
    assert_eq!(err.code(), Code::Forbidden);

    let p = posts::accept(&e.ctx, &asker, &q, Some(&a1)).await.unwrap();
    assert_eq!(p.accepted_comment_id.as_deref(), Some(a1.as_str()));
    let t = comments::accept_target(&e.ctx, &asker, &a1).await.unwrap();
    assert!(t.accepted);
    assert_eq!(t.post_id, q);

    // 관리자는 바꾸고 뺄 수 있다. 감사 기록이 남는다.
    let p = posts::accept(&e.ctx, &editor(), &q, Some(&a2))
        .await
        .unwrap();
    assert_eq!(p.accepted_comment_id.as_deref(), Some(a2.as_str()));
    let p = posts::accept(&e.ctx, &editor(), &q, None).await.unwrap();
    assert!(p.accepted_comment_id.is_none());
    let stored = e.ctx.store.post_by_id(&q).await.unwrap().unwrap();
    assert!(stored.accepted_comment_id.is_none());
    assert_eq!(audits(&e.db, "post.accept").await, 2);
    assert_eq!(audits(&e.db, "post.unaccept").await, 1);

    // 다른 글의 댓글, 지운 댓글은 채택할 수 없다.
    let q2 = write(&e.ctx, &asker, "forum", "다른 질문").await;
    let err = posts::accept(&e.ctx, &asker, &q2, Some(&a1))
        .await
        .unwrap_err();
    assert_eq!(err.code(), Code::Validation);
    comments::delete(&e.ctx, &other, &a2, None).await.unwrap();
    let err = posts::accept(&e.ctx, &asker, &q, Some(&a2))
        .await
        .unwrap_err();
    assert_eq!(err.code(), Code::Validation);

    // 채택은 qna·forum 게시판에서만.
    let chat = write(&e.ctx, &asker, "free", "잡담").await;
    let c = comment(&e.ctx, &other, &chat, "댓글").await;
    let err = posts::accept(&e.ctx, &asker, &chat, Some(&c))
        .await
        .unwrap_err();
    assert_eq!(err.code(), Code::Validation);
}

#[tokio::test]
async fn locked_post_refuses_everyone_but_moderators() {
    let e = env().await;
    let asker = member(&e.ctx, "asker@example.test").await;
    let other = member(&e.ctx, "other@example.test").await;
    let q = write(&e.ctx, &asker, "forum", "분쟁 글").await;
    let c = comment(&e.ctx, &other, &q, "반박").await;

    // 회원은 잠글 수 없다.
    let err = posts::lock(&e.ctx, &asker, &q, true).await.unwrap_err();
    assert_eq!(err.code(), Code::Forbidden);
    let p = posts::lock(&e.ctx, &editor(), &q, true).await.unwrap();
    assert!(p.is_locked);
    assert!(e.ctx.store.post_by_id(&q).await.unwrap().unwrap().is_locked);
    assert_eq!(audits(&e.db, "post.lock").await, 1);

    let locked = |err: maru_core::Error| {
        assert_eq!(err.code(), Code::Forbidden);
        assert!(err.message().contains("잠긴 글"), "{}", err.message());
    };
    let new_comment = NewComment {
        post_id: q.clone(),
        reply_to: None,
        body: "한마디 더".into(),
        guest: None,
        captcha: None,
    };
    locked(
        comments::create(&e.ctx, &other, new_comment.clone())
            .await
            .unwrap_err(),
    );
    locked(
        comments::update(&e.ctx, &other, &c, "고친 반박", None)
            .await
            .unwrap_err(),
    );
    locked(
        comments::delete(&e.ctx, &other, &c, None)
            .await
            .unwrap_err(),
    );
    locked(
        board_posts::edit(
            &e.ctx,
            &asker,
            &q,
            BoardEdit {
                title: "고친 제목".into(),
                body_md: "고친 본문".into(),
                ..Default::default()
            },
            None,
        )
        .await
        .unwrap_err(),
    );
    locked(
        board_posts::remove(&e.ctx, &asker, &q, None)
            .await
            .unwrap_err(),
    );
    locked(
        posts::accept(&e.ctx, &asker, &q, Some(&c))
            .await
            .unwrap_err(),
    );

    // 운영자는 잠긴 글에도 쓴다. 풀면 회원도 다시 된다.
    comments::create(&e.ctx, &Actor::local(), new_comment.clone())
        .await
        .unwrap();
    posts::lock(&e.ctx, &editor(), &q, false).await.unwrap();
    comments::create(&e.ctx, &other, new_comment).await.unwrap();
    assert_eq!(audits(&e.db, "post.unlock").await, 1);
}
