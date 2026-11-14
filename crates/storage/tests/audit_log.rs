//! 감사 로그 보기 (GAP S14, ADR-0062). 실제 SQLite 파일로 조건·순서·쪽·권한을 검증한다.
#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::sync::Arc;

use giwa_core::actor::{Actor, Permission, Scope, rank};
use giwa_core::services::audit_log::{self, AuditQuery};
use giwa_core::services::boards::{self, NewBoard};
use giwa_core::services::posts::{self, NewPost};
use giwa_core::{Code, Ctx};
use giwa_storage::{DataDir, Db, FsBlobs, SqliteStore};

struct Env {
    _tmp: tempfile::TempDir,
    _db: Db,
    ctx: Ctx,
    post_id: String,
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
        extensions: giwa_core::store::no_extensions(),
    };
    let local = Actor::local();
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
    let p = posts::create_draft(
        &ctx,
        &local,
        NewPost {
            board: "free".into(),
            title: "감사 글".into(),
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
    posts::publish(&ctx, &local, &p.id).await.unwrap();
    Env {
        _tmp: tmp,
        _db: db,
        ctx,
        post_id: p.id,
    }
}

fn q() -> AuditQuery {
    AuditQuery::default()
}

#[tokio::test]
async fn filters_narrow_newest_first() {
    let e = env().await;
    let local = Actor::local();
    let all = audit_log::list(&e.ctx, &local, &q()).await.unwrap();
    assert!(all.total >= 3, "{}", all.total);
    let ats: Vec<&str> = all.items.iter().map(|x| x.at.as_str()).collect();
    let mut sorted = ats.clone();
    sorted.sort_by(|a, b| b.cmp(a));
    assert_eq!(ats, sorted, "최신 순");
    assert_eq!(all.items[0].action, "post.publish");
    assert_eq!(all.items[0].actor_type, "local");

    let posts_only = audit_log::list(
        &e.ctx,
        &local,
        &AuditQuery {
            action: Some("post.".into()),
            ..q()
        },
    )
    .await
    .unwrap();
    assert!(
        posts_only
            .items
            .iter()
            .all(|x| x.action.starts_with("post."))
    );
    assert!(posts_only.items.iter().any(|x| x.action == "post.create"));
    let exact = audit_log::list(
        &e.ctx,
        &local,
        &AuditQuery {
            action: Some("post.publish".into()),
            ..q()
        },
    )
    .await
    .unwrap();
    assert_eq!(exact.total, 1);
    // `_` 는 LIKE 와일드카드가 아니라 글자다.
    let none = audit_log::list(
        &e.ctx,
        &local,
        &AuditQuery {
            action: Some("pos_".into()),
            ..q()
        },
    )
    .await
    .unwrap();
    assert_eq!(none.total, 0);

    let target = audit_log::list(
        &e.ctx,
        &local,
        &AuditQuery {
            target_type: Some("post".into()),
            target_id: Some(e.post_id.clone()),
            ..q()
        },
    )
    .await
    .unwrap();
    assert!(target.total >= 2);
    assert!(
        target
            .items
            .iter()
            .all(|x| x.target_id.as_deref() == Some(e.post_id.as_str()))
    );

    let future = audit_log::list(
        &e.ctx,
        &local,
        &AuditQuery {
            since: Some("2999-01-01T00:00:00+09:00".into()),
            ..q()
        },
    )
    .await
    .unwrap();
    assert_eq!(future.total, 0);
    let past = audit_log::list(
        &e.ctx,
        &local,
        &AuditQuery {
            until: Some("2999-01-01T00:00:00Z".into()),
            actor_type: Some("local".into()),
            ..q()
        },
    )
    .await
    .unwrap();
    assert_eq!(past.total, all.total);

    let paged = audit_log::list(
        &e.ctx,
        &local,
        &AuditQuery {
            page: Some(2),
            per_page: Some(1),
            ..q()
        },
    )
    .await
    .unwrap();
    assert_eq!(paged.items.len(), 1);
    assert_eq!(paged.items[0].id, all.items[1].id);
    assert_eq!(paged.total, all.total);
}

#[tokio::test]
async fn bad_conditions_are_rejected() {
    let e = env().await;
    let local = Actor::local();
    for bad in [
        AuditQuery {
            actor_type: Some("robot".into()),
            ..q()
        },
        AuditQuery {
            action: Some("post' OR 1=1".into()),
            ..q()
        },
        AuditQuery {
            target_type: Some("Post".into()),
            ..q()
        },
        AuditQuery {
            since: Some("어제".into()),
            ..q()
        },
    ] {
        let err = audit_log::list(&e.ctx, &local, &bad).await.unwrap_err();
        assert_eq!(err.code(), Code::Validation, "{bad:?}");
    }
}

#[tokio::test]
async fn only_site_admins_read_the_log() {
    let e = env().await;
    let guest = giwa_core::services::anonymous_actor(&e.ctx).await.unwrap();
    let err = audit_log::list(&e.ctx, &guest, &q()).await.unwrap_err();
    assert_eq!(err.code(), Code::Unauthorized);
    let editor = Actor::human(
        "e1".into(),
        [Permission::PostPublish, Permission::PostModerate],
        rank::EDITOR,
    );
    let err = audit_log::list(&e.ctx, &editor, &q()).await.unwrap_err();
    assert_eq!(err.code(), Code::Forbidden);
    let agent = Actor::agent("t1".into(), &Scope::ALL, None);
    let err = audit_log::list(&e.ctx, &agent, &q()).await.unwrap_err();
    assert_eq!(
        err.code(),
        Code::Forbidden,
        "에이전트 토큰은 감사 로그를 못 본다"
    );
    let admin = Actor::human("a1".into(), [Permission::SiteAdmin], rank::ADMIN);
    assert!(audit_log::list(&e.ctx, &admin, &q()).await.unwrap().total > 0);
}
