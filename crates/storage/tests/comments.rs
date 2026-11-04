//! 댓글·대댓글·승인 대기·비회원 댓글·자체 캡차 (GAP M5·M6·M10, ADR-0052).
//! 실제 SQLite 파일로 게시판 정책·승인·비밀번호·지운 자리·개수·캡차 1회성을 검증한다.
#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::sync::Arc;

use maru_core::actor::{Actor, Permission, rank};
use maru_core::model::{Approval, BoardPolicy, CommentStatus, PageReq};
use maru_core::services::boards::{self, BoardPatch, NewBoard};
use maru_core::services::comments::{self, GuestAuthor, Moderation, NewComment};
use maru_core::services::members::{self, SignupInput};
use maru_core::services::posts::{self, NewPost};
use maru_core::services::settings::{self, SettingsPatch};
use maru_core::services::{anonymous_actor, captcha};
use maru_core::{Code, Ctx};
use maru_storage::{DataDir, Db, FsBlobs, SqliteStore};

struct Env {
    _tmp: tempfile::TempDir,
    db: Db,
    ctx: Ctx,
    post: String,
    member: Actor,
    guest: Actor,
}

async fn env(policy: BoardPolicy) -> Env {
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
    boards::create(
        &ctx,
        &local,
        NewBoard {
            slug: "free".into(),
            name: "자유".into(),
            kind: "free".into(),
            policy: Some(policy),
            sort_order: 0,
            layout: None,
            field_schema: None,
            list_order: None,
        },
    )
    .await
    .unwrap();
    let post = posts::create_draft(
        &ctx,
        &local,
        NewPost {
            board: "free".into(),
            title: "댓글 받는 글".into(),
            slug: None,
            body_md: "본문".into(),
            excerpt: None,
            cover: None,
            category: None,
            tags: vec![],
            notice: false,
            secret: false,
            meta: Default::default(),
        },
    )
    .await
    .unwrap();
    posts::publish(&ctx, &local, &post.id).await.unwrap();
    members::signup(
        &ctx,
        SignupInput {
            email: "dal@example.test".into(),
            display_name: "달빛".into(),
            password: "test-password-1".into(),
            captcha: None,
        },
    )
    .await
    .unwrap();
    let m = ctx
        .store
        .member_by_email("dal@example.test")
        .await
        .unwrap()
        .unwrap();
    let member = Actor::human(
        m.id,
        [
            Permission::SiteView,
            Permission::BoardList,
            Permission::PostRead,
            Permission::PostWrite,
            Permission::PostComment,
            Permission::PostUpload,
        ],
        rank::MEMBER,
    );
    let guest = anonymous_actor(&ctx).await.unwrap();
    Env {
        _tmp: tmp,
        db,
        ctx,
        post: post.id,
        member,
        guest,
    }
}

fn open_policy() -> BoardPolicy {
    BoardPolicy {
        comment: "guest".into(),
        ..Default::default()
    }
}

fn editor() -> Actor {
    Actor::human(
        "editor-x".into(),
        [Permission::PostRead, Permission::PostModerate],
        rank::EDITOR,
    )
}

fn text(post: &str, body: &str) -> NewComment {
    NewComment {
        post_id: post.into(),
        reply_to: None,
        body: body.into(),
        guest: None,
        captcha: None,
    }
}

/// 질문을 받고 2초 기다려 답한다. 테스트는 저장된 답을 DB 에서 읽는다.
async fn solved(e: &Env) -> captcha::Answer {
    let ch = captcha::issue(&e.ctx).await.unwrap();
    let hash = maru_core::services::tokens::hash(&ch.token);
    let answer: String = sqlx::query_scalar("SELECT answer FROM challenges WHERE id_hash = ?")
        .bind(hash)
        .fetch_one(e.db.read_pool())
        .await
        .unwrap();
    tokio::time::sleep(std::time::Duration::from_millis(2100)).await;
    captcha::Answer {
        token: ch.token,
        answer,
    }
}

fn as_guest(post: &str, body: &str, answer: Option<captcha::Answer>) -> NewComment {
    NewComment {
        guest: Some(GuestAuthor {
            name: "지나가던손님".into(),
            password: "1234".into(),
        }),
        captcha: answer,
        ..text(post, body)
    }
}

#[tokio::test]
async fn member_comment_is_public_and_counted() {
    let e = env(BoardPolicy::default()).await;
    let c = comments::create(&e.ctx, &e.member, text(&e.post, "  첫 댓글\r\n둘째 줄 "))
        .await
        .unwrap();
    assert_eq!(c.status, CommentStatus::Published);
    assert_eq!(c.body, "첫 댓글\n둘째 줄");
    assert_eq!(c.author_name, "달빛");
    assert!(c.untrusted);
    let list = comments::list_for_post(&e.ctx, &e.guest, &e.post)
        .await
        .unwrap();
    assert_eq!(list.len(), 1);
    let counts = comments::counts(&e.ctx, std::slice::from_ref(&e.post))
        .await
        .unwrap();
    assert_eq!(counts.get(&e.post), Some(&1));

    // 기본 정책(member)에서 손님은 못 쓴다. 에이전트도 못 쓴다.
    let err = comments::create(&e.ctx, &e.guest, as_guest(&e.post, "손님", None))
        .await
        .unwrap_err();
    assert_eq!(err.code(), Code::Unauthorized);
    let agent = Actor::agent("t".into(), &maru_core::actor::Scope::ALL, None);
    let err = comments::create(&e.ctx, &agent, text(&e.post, "에이전트"))
        .await
        .unwrap_err();
    assert_eq!(err.code(), Code::Forbidden);
    // 길이
    let err = comments::create(&e.ctx, &e.member, text(&e.post, " \n "))
        .await
        .unwrap_err();
    assert_eq!(err.code(), Code::Validation);
    let long = "가".repeat(comments::MAX_BODY_CHARS + 1);
    assert!(
        comments::create(&e.ctx, &e.member, text(&e.post, &long))
            .await
            .is_err()
    );
}

#[tokio::test]
async fn comments_can_be_closed() {
    let e = env(BoardPolicy {
        comment: "none".into(),
        ..Default::default()
    })
    .await;
    let err = comments::create(&e.ctx, &e.member, text(&e.post, "닫힘"))
        .await
        .unwrap_err();
    assert_eq!(err.code(), Code::Forbidden);
    // 모르는 역할 이름은 거부, none 은 된다.
    let bad = boards::update(
        &e.ctx,
        &Actor::local(),
        "free",
        BoardPatch {
            name: None,
            kind: None,
            policy: Some(BoardPolicy {
                comment: "visitor".into(),
                ..Default::default()
            }),
            sort_order: None,
            layout: None,
            field_schema: None,
            list_order: None,
        },
    )
    .await
    .unwrap_err();
    assert_eq!(bad.code(), Code::Validation);
}

#[tokio::test]
async fn guest_comment_needs_captcha_and_review() {
    let e = env(open_policy()).await;
    settings::update(
        &e.ctx,
        &Actor::local(),
        SettingsPatch {
            captcha: Some("guest".into()),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    // 이름·비밀번호 없이, 질문 없이는 안 된다.
    let err = comments::create(&e.ctx, &e.guest, text(&e.post, "손님"))
        .await
        .unwrap_err();
    assert_eq!(err.code(), Code::Validation);
    let err = comments::create(&e.ctx, &e.guest, as_guest(&e.post, "손님", None))
        .await
        .unwrap_err();
    assert_eq!(err.code(), Code::Validation);

    // 너무 빨리 낸 답·틀린 답은 거부, 도전은 한 번만 쓴다.
    let ch = captcha::issue(&e.ctx).await.unwrap();
    let hash = maru_core::services::tokens::hash(&ch.token);
    let answer: String = sqlx::query_scalar("SELECT answer FROM challenges WHERE id_hash = ?")
        .bind(&hash)
        .fetch_one(e.db.read_pool())
        .await
        .unwrap();
    let quick = captcha::Answer {
        token: ch.token.clone(),
        answer: answer.clone(),
    };
    assert!(captcha::check(&e.ctx, &quick).await.is_err(), "2초 안 답");
    assert!(
        captcha::check(&e.ctx, &quick).await.is_err(),
        "같은 도전 재사용"
    );
    let mut wrong = solved(&e).await;
    wrong.answer.push('9');
    assert!(
        comments::create(&e.ctx, &e.guest, as_guest(&e.post, "손님", Some(wrong)))
            .await
            .is_err()
    );
    let reused = solved(&e).await;
    let c = comments::create(
        &e.ctx,
        &e.guest,
        as_guest(&e.post, "비회원 댓글", Some(reused.clone())),
    )
    .await
    .unwrap();
    assert!(
        comments::create(&e.ctx, &e.guest, as_guest(&e.post, "또", Some(reused)))
            .await
            .is_err(),
        "같은 답 재사용"
    );
    assert_eq!(c.status, CommentStatus::Pending);
    assert_eq!(c.author_name, "지나가던손님");
    assert!(c.is_guest);
    assert!(
        comments::list_for_post(&e.ctx, &e.guest, &e.post)
            .await
            .unwrap()
            .is_empty(),
        "승인 전에는 안 보인다"
    );

    // 승인 큐 → 승인 → 보인다.
    let queue = comments::admin_list(
        &e.ctx,
        &editor(),
        Some(CommentStatus::Pending),
        None,
        PageReq::new(1, 20),
    )
    .await
    .unwrap();
    assert_eq!(queue.total, 1);
    assert!(
        comments::moderate(&e.ctx, &e.member, &c.id, Moderation::Approve)
            .await
            .is_err()
    );
    let ok = comments::moderate(&e.ctx, &editor(), &c.id, Moderation::Approve)
        .await
        .unwrap();
    assert_eq!(ok.status, CommentStatus::Published);
    assert_eq!(
        comments::list_for_post(&e.ctx, &e.guest, &e.post)
            .await
            .unwrap()
            .len(),
        1
    );

    // 질문을 끄면 이름·비밀번호만으로 된다.
    settings::update(
        &e.ctx,
        &Actor::local(),
        SettingsPatch {
            captcha: Some("off".into()),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    comments::create(&e.ctx, &e.guest, as_guest(&e.post, "질문 없이", None))
        .await
        .unwrap();
    // 비밀번호 해시는 밖으로 나가지 않는다.
    let json = serde_json::to_string(&ok).unwrap();
    assert!(!json.contains("argon2"), "{json}");
}

#[tokio::test]
async fn replies_edit_delete_and_placeholders() {
    let e = env(BoardPolicy {
        comment: "guest".into(),
        comment_approval: Approval::Off,
        ..Default::default()
    })
    .await;
    settings::update(
        &e.ctx,
        &Actor::local(),
        SettingsPatch {
            captcha: Some("off".into()),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let root = comments::create(&e.ctx, &e.member, text(&e.post, "뿌리"))
        .await
        .unwrap();
    let guest = comments::create(
        &e.ctx,
        &e.guest,
        NewComment {
            reply_to: Some(root.id.clone()),
            ..as_guest(&e.post, "대댓글", None)
        },
    )
    .await
    .unwrap();
    assert_eq!(guest.parent_id.as_deref(), Some(root.id.as_str()));
    assert_eq!(guest.depth, 1);
    // 대댓글에 답해도 같은 뿌리, 누구에게인지 이름이 붙는다.
    let deep = comments::create(
        &e.ctx,
        &e.member,
        NewComment {
            reply_to: Some(guest.id.clone()),
            ..text(&e.post, "대댓글의 대댓글")
        },
    )
    .await
    .unwrap();
    assert_eq!(deep.parent_id.as_deref(), Some(root.id.as_str()));
    assert_eq!(deep.reply_to_name.as_deref(), Some("지나가던손님"));
    let order: Vec<String> = comments::list_for_post(&e.ctx, &e.guest, &e.post)
        .await
        .unwrap()
        .into_iter()
        .map(|c| c.body)
        .collect();
    assert_eq!(order, ["뿌리", "대댓글", "대댓글의 대댓글"]);

    // 비회원 댓글: 비밀번호가 맞아야 고친다. 남의 회원 댓글은 못 고친다.
    let err = comments::update(&e.ctx, &e.guest, &guest.id, "고침", Some("9999"))
        .await
        .unwrap_err();
    assert_eq!(err.code(), Code::Forbidden);
    let fixed = comments::update(&e.ctx, &e.guest, &guest.id, "고친 대댓글", Some("1234"))
        .await
        .unwrap();
    assert_eq!(fixed.body, "고친 대댓글");
    assert!(
        comments::update(&e.ctx, &e.guest, &root.id, "남의 것", Some("1234"))
            .await
            .is_err()
    );

    // 대댓글이 있는 뿌리를 지우면 자리만 남는다. 대댓글을 다 지우면 자리도 사라진다.
    comments::delete(&e.ctx, &e.member, &root.id, None)
        .await
        .unwrap();
    let list = comments::list_for_post(&e.ctx, &e.guest, &e.post)
        .await
        .unwrap();
    assert_eq!(list.len(), 3);
    assert_eq!(list[0].status, CommentStatus::Deleted);
    assert_eq!(list[0].body, "");
    assert_eq!(list[0].author_name, "");
    comments::delete(&e.ctx, &e.guest, &guest.id, Some("1234"))
        .await
        .unwrap();
    comments::delete(&e.ctx, &editor(), &deep.id, None)
        .await
        .unwrap();
    assert!(
        comments::list_for_post(&e.ctx, &e.guest, &e.post)
            .await
            .unwrap()
            .is_empty()
    );
    assert!(
        comments::moderate(&e.ctx, &editor(), &root.id, Moderation::Approve)
            .await
            .is_err()
    );
    let counts = comments::counts(&e.ctx, std::slice::from_ref(&e.post))
        .await
        .unwrap();
    assert_eq!(counts.get(&e.post), None);

    // 감사: 만들기 3, 고치기 1, 지우기 3. 본문은 남기지 않는다.
    let n: i64 = sqlx::query_scalar("SELECT count(*) FROM audit_log WHERE action LIKE 'comment.%'")
        .fetch_one(e.db.read_pool())
        .await
        .unwrap();
    assert_eq!(n, 7);
    let leaked: i64 =
        sqlx::query_scalar("SELECT count(*) FROM audit_log WHERE instr(diff, '대댓글') > 0")
            .fetch_one(e.db.read_pool())
            .await
            .unwrap();
    assert_eq!(leaked, 0);
}

#[tokio::test]
async fn hidden_and_secret_posts_take_no_comments() {
    let e = env(open_policy()).await;
    posts::hide(&e.ctx, &Actor::local(), &e.post).await.unwrap();
    let err = comments::create(&e.ctx, &e.member, text(&e.post, "숨긴 글"))
        .await
        .unwrap_err();
    assert_eq!(err.code(), Code::NotFound);
    assert!(
        comments::list_for_post(&e.ctx, &e.guest, &e.post)
            .await
            .is_err()
    );
}
