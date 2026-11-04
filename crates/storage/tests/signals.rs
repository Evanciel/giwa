//! 운영 신호 (GAP M19, ADR-0074). 실제 SQLite 파일로 댓글 급증 신호, 권한, 설정 범위,
//! 메일 알림(관리자·소유자에게, 같은 신호는 한 번만, 끄면 보내지 않음)을 검증한다.
#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::sync::Arc;

use maru_core::actor::{Actor, Permission, Scope, rank};
use maru_core::model::MailStatus;
use maru_core::services::boards::{self, NewBoard};
use maru_core::services::comments::{self, NewComment};
use maru_core::services::posts::{self, NewPost};
use maru_core::services::settings::{self, SettingsPatch};
use maru_core::services::{auth, mail, signals};
use maru_core::{Code, Ctx};
use maru_storage::{DataDir, Db, FsBlobs, SqliteStore};

struct Env {
    _tmp: tempfile::TempDir,
    db: Db,
    ctx: Ctx,
    post: String,
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
    auth::ensure_owner(&ctx, &local, "owner@example.test", "운영자")
        .await
        .unwrap();
    boards::create(
        &ctx,
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
    let post = posts::create_draft(
        &ctx,
        &local,
        NewPost {
            board: "free".into(),
            title: "뜨거운 글".into(),
            slug: Some("hot".into()),
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
    Env {
        _tmp: tmp,
        db,
        ctx,
        post: post.id,
    }
}

async fn comment(e: &Env, n: usize) {
    for i in 0..n {
        comments::create(
            &e.ctx,
            &Actor::local(),
            NewComment {
                post_id: e.post.clone(),
                reply_to: None,
                body: format!("댓글 {i}"),
                guest: None,
                captcha: None,
            },
        )
        .await
        .unwrap();
    }
}

async fn set(e: &Env, patch: SettingsPatch) {
    settings::update(&e.ctx, &Actor::local(), patch)
        .await
        .unwrap();
}

async fn signal_mails(e: &Env) -> Vec<maru_core::model::MailOut> {
    mail::list(&e.ctx, &Actor::local(), Some(MailStatus::Queued), 50)
        .await
        .unwrap()
        .into_iter()
        .filter(|m| m.subject.contains("살펴볼 일"))
        .collect()
}

#[tokio::test]
async fn comment_burst_shows_and_alerts_once() {
    let e = env().await;
    set(
        &e,
        SettingsPatch {
            signal_comment_burst: Some(3),
            ..Default::default()
        },
    )
    .await;
    comment(&e, 2).await;
    assert!(
        signals::list(&e.ctx, &Actor::local())
            .await
            .unwrap()
            .is_empty()
    );
    comment(&e, 1).await;
    let list = signals::list(&e.ctx, &Actor::local()).await.unwrap();
    assert_eq!(list.len(), 1);
    let s = &list[0];
    assert_eq!((s.kind.as_str(), s.count), ("comment_burst", 3));
    assert!(s.message.contains("뜨거운 글") && s.message.contains("3개"));
    assert_eq!(s.url.as_deref(), Some("/b/free/hot"));

    assert_eq!(signals::alert(&e.ctx).await.unwrap(), 1);
    let mails = signal_mails(&e).await;
    assert_eq!(mails.len(), 1, "소유자 한 명에게");
    assert_eq!(mails[0].to_addr, "owner@example.test");
    assert!(
        mails[0].body.contains("뜨거운 글")
            && mails[0].body.contains("http://127.0.0.1:8080/b/free/hot")
    );
    // 댓글이 더 달려도 같은 신호(같은 날)는 다시 알리지 않는다
    comment(&e, 2).await;
    assert_eq!(signals::alert(&e.ctx).await.unwrap(), 0);
    assert_eq!(signal_mails(&e).await.len(), 1);
    let audit: i64 =
        sqlx::query_scalar("SELECT count(*) FROM audit_log WHERE action = 'signal.alert'")
            .fetch_one(e.db.read_pool())
            .await
            .unwrap();
    assert_eq!(audit, 1);

    // 1시간이 지난 댓글은 세지 않는다
    sqlx::query("UPDATE comments SET created_at = '2000-01-01T00:00:00Z'")
        .execute(e.db.write_pool())
        .await
        .unwrap();
    assert!(
        signals::list(&e.ctx, &Actor::local())
            .await
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn off_switches_and_permissions() {
    let e = env().await;
    set(
        &e,
        SettingsPatch {
            signal_comment_burst: Some(3),
            signal_mail: Some(false),
            ..Default::default()
        },
    )
    .await;
    comment(&e, 3).await;
    assert_eq!(
        signals::list(&e.ctx, &Actor::local()).await.unwrap().len(),
        1
    );
    assert_eq!(
        signals::alert(&e.ctx).await.unwrap(),
        0,
        "메일을 끄면 보내지 않는다"
    );
    assert!(signal_mails(&e).await.is_empty());

    set(
        &e,
        SettingsPatch {
            signal_comment_burst: Some(0),
            ..Default::default()
        },
    )
    .await;
    assert!(
        signals::list(&e.ctx, &Actor::local())
            .await
            .unwrap()
            .is_empty(),
        "0 이면 끔"
    );

    for bad in [1, 2, 1001] {
        let err = settings::update(
            &e.ctx,
            &Actor::local(),
            SettingsPatch {
                signal_comment_burst: Some(bad),
                ..Default::default()
            },
        )
        .await
        .unwrap_err();
        assert_eq!(err.code(), Code::Validation, "{bad}");
    }

    // 게시판 관리 권한이 없으면 못 본다. 읽기 토큰도.
    let member = Actor::human("m1".into(), [Permission::PostRead], rank::MEMBER);
    assert_eq!(
        signals::list(&e.ctx, &member).await.unwrap_err().code(),
        Code::Forbidden
    );
    let reader = Actor::agent("t1".into(), &[Scope::Read], None);
    assert_eq!(
        signals::list(&e.ctx, &reader).await.unwrap_err().code(),
        Code::Forbidden
    );
}
