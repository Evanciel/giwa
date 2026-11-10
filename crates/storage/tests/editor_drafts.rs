//! 화면 편집기 초안 (ADR-0152). 실제 SQLite 파일로 덮어쓰기, 본인 것만, 크기 한도, 에이전트는 읽기만을 확인한다.
#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::sync::Arc;

use maru_core::actor::{Actor, Permission, Scope, rank};
use maru_core::services::{auth, editor_drafts};
use maru_core::{Code, Ctx};
use maru_storage::{DataDir, Db, FsBlobs, SqliteStore};
use serde_json::json;

async fn env() -> (tempfile::TempDir, Ctx, String) {
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
    let owner = auth::ensure_owner(&ctx, &Actor::local(), "owner@example.test", "운영자")
        .await
        .unwrap();
    (tmp, ctx, owner.item.id)
}

#[tokio::test]
async fn drafts_are_per_person_and_overwrite() {
    let (_tmp, ctx, owner) = env().await;
    let me = Actor::human(owner.clone(), [Permission::DesignEdit], rank::OWNER);

    assert!(
        editor_drafts::get(&ctx, &me, "home")
            .await
            .unwrap()
            .is_none()
    );
    editor_drafts::save(&ctx, &me, "home", json!({ "widgets": [1] }), "a1")
        .await
        .unwrap();
    let d = editor_drafts::save(&ctx, &me, "home", json!({ "widgets": [1, 2] }), "b2")
        .await
        .unwrap();
    let got = editor_drafts::get(&ctx, &me, "home")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(got, d, "덮어쓴다");
    assert_eq!(got.body, json!({ "widgets": [1, 2] }));
    assert_eq!(got.base, "b2");
    assert!(
        editor_drafts::get(&ctx, &me, "page:about")
            .await
            .unwrap()
            .is_none(),
        "화면마다 따로"
    );

    // 다른 사람(화면 편집 권한이 있어도)은 내 초안을 자기 것으로 보지 않는다.
    let other = Actor::human("someone-else".into(), [Permission::DesignEdit], rank::ADMIN);
    assert!(
        editor_drafts::get(&ctx, &other, "home")
            .await
            .unwrap()
            .is_none()
    );
    assert!(!editor_drafts::discard(&ctx, &other, "home").await.unwrap());

    // 에이전트는 화면의 초안을 읽기만 한다.
    let agent = Actor::agent("t1".into(), &[Scope::Read, Scope::Design], None);
    let all = editor_drafts::for_screen(&ctx, &agent, "home")
        .await
        .unwrap();
    assert_eq!(all.len(), 1);
    assert_eq!(all[0].member_id, owner);
    let err = editor_drafts::save(&ctx, &agent, "home", json!({}), "x")
        .await
        .unwrap_err();
    assert_eq!(err.code(), Code::Forbidden);
    let reader = Actor::agent("t2".into(), &[Scope::Read], None);
    assert_eq!(
        editor_drafts::for_screen(&ctx, &reader, "home")
            .await
            .unwrap_err()
            .code(),
        Code::Forbidden
    );

    assert!(editor_drafts::discard(&ctx, &me, "home").await.unwrap());
    assert!(
        !editor_drafts::discard(&ctx, &me, "home").await.unwrap(),
        "멱등"
    );
    assert!(
        editor_drafts::get(&ctx, &me, "home")
            .await
            .unwrap()
            .is_none()
    );
}

#[tokio::test]
async fn drafts_check_size_and_shape() {
    let (_tmp, ctx, owner) = env().await;
    let me = Actor::human(owner, [Permission::DesignEdit], rank::OWNER);
    let big = "가".repeat(editor_drafts::MAX_BYTES / 3 + 1);
    let err = editor_drafts::save(&ctx, &me, "home", json!({ "x": big }), "a")
        .await
        .unwrap_err();
    assert_eq!(err.code(), Code::Validation);
    assert_eq!(err.message_key(), "error.draft_too_large");
    for bad in ["", "../x", "home page", &"a".repeat(200)] {
        assert_eq!(
            editor_drafts::save(&ctx, &me, bad, json!({}), "a")
                .await
                .unwrap_err()
                .code(),
            Code::Validation,
            "{bad}"
        );
    }
    assert_eq!(
        editor_drafts::save(&ctx, &me, "home", json!([1]), "a")
            .await
            .unwrap_err()
            .message_key(),
        "error.draft_body"
    );
    let viewer = Actor::human("v".into(), [Permission::SiteView], rank::MEMBER);
    assert_eq!(
        editor_drafts::get(&ctx, &viewer, "home")
            .await
            .unwrap_err()
            .code(),
        Code::Forbidden
    );
}
