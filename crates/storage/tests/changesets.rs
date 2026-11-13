//! changeset 의 보상과 권한 (ADR-0121). 사본에서는 통과하고 실제에서 실패하는 경우는 CLI 로 만들 수 없어
//! 여기서 낡은 사본을 주어 본다.
#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::sync::Arc;

use async_trait::async_trait;
use maru_core::actor::{Actor, Permission, Scope, rank};
use maru_core::model::ChangesetStatus;
use maru_core::services::boards::{self, NewBoard};
use maru_core::services::changesets::{self, Sandbox};
use maru_core::{Code, Ctx, Result};
use maru_storage::sandbox::FsSandbox;
use maru_storage::{DataDir, Db, FsBlobs, SqliteStore};
use serde_json::json;

struct Env {
    _tmp: tempfile::TempDir,
    dir: DataDir,
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
    Env {
        _tmp: tmp,
        dir,
        db,
        ctx,
    }
}

fn new_board(slug: &str) -> NewBoard {
    NewBoard {
        slug: slug.into(),
        name: slug.into(),
        kind: "forum".into(),
        policy: None,
        sort_order: 0,
        layout: None,
        field_schema: None,
        list_order: None,
    }
}

/// 언제나 다른(빈) DB 를 주는 사본. 시험은 통과하고 실제 적용에서 부딪힌다.
struct Stale(Env);

#[async_trait]
impl Sandbox for Stale {
    async fn open(&self, _ctx: &Ctx, _key: &str) -> Result<Ctx> {
        Ok(self.0.ctx.clone())
    }

    async fn close(&self, _key: &str) {}
}

#[tokio::test]
async fn 실제_적용에서_실패하면_앞_연산을_되돌려_처음으로_돌아간다() {
    let e = env().await;
    let local = Actor::local();
    boards::create(&e.ctx, &local, new_board("taken"))
        .await
        .unwrap();
    let cs = changesets::create(
        &e.ctx,
        &local,
        None,
        "부딪히는 묶음",
        vec![
            json!({ "op": "board.create", "slug": "fresh", "name": "새 판", "kind": "forum" }),
            json!({ "op": "board.create", "slug": "taken", "name": "겹침", "kind": "forum" }),
        ],
    )
    .await
    .unwrap();

    let stale = Stale(env().await);
    let err = changesets::apply(&e.ctx, &local, &stale, &cs.id)
        .await
        .unwrap_err();
    assert!(err.message().contains("2번째"), "{}", err.message());

    let list = boards::list(&e.ctx, &local).await.unwrap();
    assert!(
        !list.iter().any(|b| b.slug == "fresh"),
        "앞 연산이 되돌려졌다"
    );
    let after = changesets::get(&e.ctx, &local, &cs.id).await.unwrap();
    assert_eq!(after.status, ChangesetStatus::Preview);
    assert!(after.error.unwrap().contains("2번째"));
    assert!(after.inverse.is_empty());

    stale.0.db.close().await;
    e.db.close().await;
}

#[tokio::test]
async fn 사본은_실제_사이트에_닿지_않는다() {
    let e = env().await;
    let local = Actor::local();
    let sandbox = FsSandbox::new(&e.dir, e.db.clone());
    let cs = changesets::create(
        &e.ctx,
        &local,
        None,
        "미리보기",
        vec![json!({ "op": "board.create", "slug": "only-copy", "name": "사본", "kind": "forum" })],
    )
    .await
    .unwrap();

    let copy = changesets::sandboxed(&e.ctx, &sandbox, "cs-1", &cs)
        .await
        .unwrap();
    assert!(boards::show(&copy, &local, "only-copy").await.is_ok());
    assert_eq!(
        boards::show(&e.ctx, &local, "only-copy")
            .await
            .unwrap_err()
            .code(),
        Code::NotFound
    );

    sandbox.close("cs-1").await;
    assert!(!e.dir.root().join("previews").join("cs-1.db").exists());
    e.db.close().await;
}

#[tokio::test]
async fn 토큰은_담을_수_있어도_승인_없이_적용하지_못한다() {
    let e = env().await;
    let sandbox = FsSandbox::new(&e.dir, e.db.clone());
    let op =
        json!({ "op": "board.create", "slug": "by-agent", "name": "에이전트", "kind": "forum" });

    // 스코프가 모자라면 담지도 못한다.
    let reader = Actor::agent("t1".into(), &[Scope::Read], None);
    assert_eq!(
        changesets::create(&e.ctx, &reader, None, "읽기만", vec![op.clone()])
            .await
            .unwrap_err()
            .code(),
        Code::Forbidden
    );

    let agent = Actor::agent("t2".into(), &[Scope::Read, Scope::Structure], None);
    let cs = changesets::create(&e.ctx, &agent, None, "구조", vec![op.clone()])
        .await
        .unwrap();
    assert_eq!(cs.actor_type, "agent");
    assert_eq!(
        changesets::apply(&e.ctx, &agent, &sandbox, &cs.id)
            .await
            .unwrap_err()
            .code(),
        Code::Forbidden
    );
    assert!(
        boards::show(&e.ctx, &Actor::local(), "by-agent")
            .await
            .is_err()
    );

    // 자동 적용을 허락받은 토큰은 된다.
    let auto = agent.clone().with_auto_apply(true);
    let applied = changesets::apply(&e.ctx, &auto, &sandbox, &cs.id)
        .await
        .unwrap();
    assert_eq!(applied.status, ChangesetStatus::Applied);

    // 회원은 관리자만 적용한다.
    let editor = Actor::human("m1".into(), [Permission::StructureEdit], rank::EDITOR);
    let second = changesets::create(
        &e.ctx,
        &editor,
        None,
        "편집자",
        vec![json!({ "op": "board.create", "slug": "by-editor", "name": "편집", "kind": "forum" })],
    )
    .await
    .unwrap();
    assert_eq!(
        changesets::apply(&e.ctx, &editor, &sandbox, &second.id)
            .await
            .unwrap_err()
            .code(),
        Code::Forbidden
    );
    e.db.close().await;
}

#[tokio::test]
async fn 미리보기_토큰은_한_번만_쓴다() {
    let e = env().await;
    let local = Actor::local();
    let cs = changesets::create(&e.ctx, &local, None, "미리보기", Vec::new())
        .await
        .unwrap();
    let link = changesets::preview_link(&e.ctx, &local, &cs.id)
        .await
        .unwrap();
    let secret = link.url.rsplit_once("token=").unwrap().1.to_owned();

    let wrong = changesets::redeem_preview_token(&e.ctx, &cs.id, "틀린값")
        .await
        .unwrap_err();
    assert_eq!(wrong.code(), Code::Forbidden);
    changesets::redeem_preview_token(&e.ctx, &cs.id, &secret)
        .await
        .unwrap();
    let again = changesets::redeem_preview_token(&e.ctx, &cs.id, &secret)
        .await
        .unwrap_err();
    assert_eq!(again.code(), Code::Forbidden);

    // 편집자 세션으로는 열 수 없다.
    let editor = Actor::human("m2".into(), [Permission::StructureEdit], rank::EDITOR);
    assert_eq!(
        changesets::for_preview(&e.ctx, &editor, &cs.id)
            .await
            .unwrap_err()
            .code(),
        Code::Forbidden
    );
    e.db.close().await;
}
