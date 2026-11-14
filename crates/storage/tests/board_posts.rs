//! 공개 화면 글쓰기: 회원·비회원 글, 승인 대기, 비밀번호로 고치기·지우기 (GAP M6, ADR-0053).
//! 실제 SQLite 파일로 게시판 정책·확인 질문·승인·비밀번호·남의 글 거부·감사를 검증한다.
#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::sync::Arc;

use giwa_core::actor::{Actor, Permission, rank};
use giwa_core::model::{Approval, BoardPolicy, PostStatus};
use giwa_core::services::board_posts::{self, BoardEdit, BoardWrite};
use giwa_core::services::boards::{self, NewBoard};
use giwa_core::services::comments::GuestAuthor;
use giwa_core::services::members::{self, SignupInput};
use giwa_core::services::posts;
use giwa_core::services::settings::{self, SettingsPatch};
use giwa_core::services::{anonymous_actor, captcha};
use giwa_core::{Code, Ctx};
use giwa_storage::{DataDir, Db, FsBlobs, SqliteStore};

struct Env {
    _tmp: tempfile::TempDir,
    db: Db,
    ctx: Ctx,
    member: Actor,
    other: Actor,
    editor: Actor,
    guest: Actor,
}

async fn member(ctx: &Ctx, email: &str, name: &str) -> Actor {
    members::signup(
        ctx,
        SignupInput {
            email: email.into(),
            display_name: name.into(),
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
            Permission::PostUpload,
        ],
        rank::MEMBER,
    )
}

async fn env(policy: BoardPolicy, captcha_mode: &str) -> Env {
    let tmp = tempfile::tempdir().unwrap();
    let dir = DataDir::new(tmp.path().join("데이터"));
    dir.ensure_layout().unwrap();
    let db = Db::open(&dir.db_path(), true).await.unwrap();
    db.migrate().await.unwrap();
    let ctx = Ctx {
        store: Arc::new(SqliteStore::new(db.clone())),
        blobs: Arc::new(FsBlobs::new(dir.media_dir())),
        site_url: "http://127.0.0.1:8080".into(),
        extensions: giwa_core::store::no_extensions(),
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
    let member_a = member(&ctx, "dal@example.test", "달빛").await;
    let other = member(&ctx, "byeol@example.test", "별빛").await;
    let ed = member(&ctx, "pyeon@example.test", "편집자").await;
    let editor = Actor::human(
        ed.member_id().unwrap().clone(),
        [
            Permission::PostRead,
            Permission::PostWrite,
            Permission::PostModerate,
        ],
        rank::EDITOR,
    );
    settings::update(
        &ctx,
        &local,
        SettingsPatch {
            captcha: Some(captcha_mode.into()),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let guest = anonymous_actor(&ctx).await.unwrap();
    Env {
        _tmp: tmp,
        db,
        ctx,
        member: member_a,
        other,
        editor,
        guest,
    }
}

fn draft(title: &str, body: &str) -> BoardWrite {
    BoardWrite {
        board: "free".into(),
        title: title.into(),
        body_md: body.into(),
        ..Default::default()
    }
}

fn edit(title: &str, body: &str) -> BoardEdit {
    BoardEdit {
        title: title.into(),
        body_md: body.into(),
        ..Default::default()
    }
}

/// 질문을 받고 2초 기다려 답한다. 테스트는 저장된 답을 DB 에서 읽는다.
async fn solved(e: &Env) -> captcha::Answer {
    let ch = captcha::issue_for(&e.ctx, board_posts::CAPTCHA_MINUTES)
        .await
        .unwrap();
    let hash = giwa_core::services::tokens::hash(&ch.token);
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

fn guest_author() -> Option<GuestAuthor> {
    Some(GuestAuthor {
        name: "지나가던손님".into(),
        password: "1234".into(),
    })
}

#[tokio::test]
async fn member_post_is_public_and_only_the_author_edits_it() {
    let e = env(BoardPolicy::default(), "guest").await;
    let p = board_posts::write(&e.ctx, &e.member, draft("첫 글", "**안녕**하세요"))
        .await
        .unwrap();
    assert_eq!(p.status, PostStatus::Published);
    assert!(p.published_at.is_some());
    assert!(p.guest_name.is_none());
    // 회원은 확인 질문 없이, 자기 글은 바로.
    let p = board_posts::edit(&e.ctx, &e.member, &p.id, edit("첫 글", "고친 본문"), None)
        .await
        .unwrap();
    assert_eq!(p.body_md, "고친 본문");
    let err = board_posts::edit(&e.ctx, &e.other, &p.id, edit("남의 글", "x"), None)
        .await
        .unwrap_err();
    assert_eq!(err.code(), Code::Forbidden);
    assert!(
        board_posts::editable(&e.ctx, &e.other, &p.id)
            .await
            .is_err()
    );
    // 손님(정책 member)·에이전트는 이 경로로 쓰지 못한다.
    let as_guest = BoardWrite {
        guest: guest_author(),
        ..draft("손님", "x")
    };
    let err = board_posts::write(&e.ctx, &e.guest, as_guest)
        .await
        .unwrap_err();
    assert_eq!(err.code(), Code::Unauthorized);
    let agent = Actor::agent("t".into(), &giwa_core::actor::Scope::ALL, None);
    let err = board_posts::write(&e.ctx, &agent, draft("에이전트", "x"))
        .await
        .unwrap_err();
    assert_eq!(err.code(), Code::Forbidden);
    // 남이 지우지 못하고, 글쓴이는 지운다.
    assert!(
        board_posts::remove(&e.ctx, &e.other, &p.id, None)
            .await
            .is_err()
    );
    let gone = board_posts::remove(&e.ctx, &e.member, &p.id, None)
        .await
        .unwrap();
    assert_eq!(gone.status, PostStatus::Deleted);
    let n: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM audit_log
          WHERE target_id = ? AND action IN ('post.write', 'post.update', 'post.delete')",
    )
    .bind(&p.id)
    .fetch_one(e.db.read_pool())
    .await
    .unwrap();
    assert_eq!(n, 3);
}

#[tokio::test]
async fn editor_only_board_refuses_members() {
    let e = env(
        BoardPolicy {
            write: "editor".into(),
            ..Default::default()
        },
        "guest",
    )
    .await;
    let err = board_posts::write(&e.ctx, &e.member, draft("회원", "x"))
        .await
        .unwrap_err();
    assert_eq!(err.code(), Code::Forbidden);
    let p = board_posts::write(&e.ctx, &e.editor, draft("편집자", "x"))
        .await
        .unwrap();
    assert_eq!(p.status, PostStatus::Published);
}

#[tokio::test]
async fn guest_post_needs_captcha_waits_for_review_and_edits_by_password() {
    let e = env(
        BoardPolicy {
            write: "guest".into(),
            ..Default::default()
        },
        "guest",
    )
    .await;
    let guest_write = |title: &str, captcha: Option<captcha::Answer>| BoardWrite {
        guest: guest_author(),
        captcha,
        ..draft(title, "손님 본문")
    };
    let err = board_posts::write(&e.ctx, &e.guest, guest_write("질문 없이", None))
        .await
        .unwrap_err();
    assert_eq!(err.code(), Code::Validation);
    let secret = BoardWrite {
        secret: true,
        ..guest_write("비밀", Some(solved(&e).await))
    };
    let err = board_posts::write(&e.ctx, &e.guest, secret)
        .await
        .unwrap_err();
    assert_eq!(err.code(), Code::Validation);
    let p = board_posts::write(
        &e.ctx,
        &e.guest,
        guest_write("손님 글", Some(solved(&e).await)),
    )
    .await
    .unwrap();
    assert_eq!(p.status, PostStatus::Pending);
    assert_eq!(p.guest_name.as_deref(), Some("지나가던손님"));
    assert!(p.author_id.is_none());
    let stored: String = sqlx::query_scalar("SELECT guest_pw_hash FROM posts WHERE id = ?")
        .bind(&p.id)
        .fetch_one(e.db.read_pool())
        .await
        .unwrap();
    assert!(stored.starts_with("$argon2id$"));
    assert!(!serde_json::to_string(&p).unwrap().contains("argon2"));

    // 승인 대기 글은 운영자가 발행한다.
    let p = posts::publish(&e.ctx, &Actor::local(), &p.id)
        .await
        .unwrap();
    assert_eq!(p.status, PostStatus::Published);

    // 손님은 비밀번호로. 다른 회원은 손님 글을 고치지 못한다.
    let ed = board_posts::editable(&e.ctx, &e.guest, &p.id)
        .await
        .unwrap();
    assert!(ed.by_password);
    let wrong = board_posts::edit(
        &e.ctx,
        &e.guest,
        &p.id,
        edit("손님 글", "고침"),
        Some("0000"),
    )
    .await
    .unwrap_err();
    assert_eq!(wrong.code(), Code::Forbidden);
    let err = board_posts::edit(&e.ctx, &e.member, &p.id, edit("손님 글", "고침"), None)
        .await
        .unwrap_err();
    assert_eq!(err.code(), Code::Forbidden);
    let p = board_posts::edit(
        &e.ctx,
        &e.guest,
        &p.id,
        edit("손님 글", "고친 손님 본문"),
        Some("1234"),
    )
    .await
    .unwrap();
    assert_eq!(p.body_md, "고친 손님 본문");
    assert_eq!(p.status, PostStatus::Published);
    // 게시판 관리자는 비밀번호 없이.
    let ed = board_posts::editable(&e.ctx, &e.editor, &p.id)
        .await
        .unwrap();
    assert!(!ed.by_password);
    // 지우기도 비밀번호로.
    assert!(
        board_posts::remove(&e.ctx, &e.guest, &p.id, Some("9999"))
            .await
            .is_err()
    );
    let gone = board_posts::remove(&e.ctx, &e.guest, &p.id, Some("1234"))
        .await
        .unwrap();
    assert_eq!(gone.status, PostStatus::Deleted);
    assert!(
        board_posts::editable(&e.ctx, &e.guest, &p.id)
            .await
            .is_err()
    );
}

#[tokio::test]
async fn approval_all_holds_member_posts_but_not_moderators() {
    let e = env(
        BoardPolicy {
            write_approval: Approval::All,
            ..Default::default()
        },
        "off",
    )
    .await;
    let p = board_posts::write(&e.ctx, &e.member, draft("회원 글", "x"))
        .await
        .unwrap();
    assert_eq!(p.status, PostStatus::Pending);
    assert!(p.published_at.is_none());
    let p = board_posts::write(&e.ctx, &e.editor, draft("편집자 글", "x"))
        .await
        .unwrap();
    assert_eq!(p.status, PostStatus::Published);

    // 승인을 끈 비회원 게시판은 손님 글도 바로.
    let off = env(
        BoardPolicy {
            write_approval: Approval::Off,
            write: "guest".into(),
            ..Default::default()
        },
        "off",
    )
    .await;
    let g = BoardWrite {
        guest: guest_author(),
        ..draft("바로 보이는 손님 글", "x")
    };
    let g = board_posts::write(&off.ctx, &off.guest, g).await.unwrap();
    assert_eq!(g.status, PostStatus::Published);
}
