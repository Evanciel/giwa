//! U12 사람 로그인. 실제 SQLite 파일로 세션·로그인 링크 역경로를 검증한다.
#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::sync::Arc;

use maru_core::actor::{Actor, Permission, Scope};
use maru_core::services::auth;
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
    Env { _tmp: tmp, db, ctx }
}

async fn scalar(db: &Db, sql: &'static str) -> i64 {
    sqlx::query_scalar(sql)
        .fetch_one(db.read_pool())
        .await
        .unwrap()
}

async fn owner(ctx: &Ctx) {
    auth::ensure_owner(ctx, &Actor::local(), "owner@example.test", "운영자")
        .await
        .unwrap();
}

fn raw_of(path: &str) -> &str {
    path.trim_start_matches("/admin/link/")
}

#[tokio::test]
async fn owner_is_created_once_and_second_member_is_refused() {
    let e = env().await;
    let a = auth::ensure_owner(&e.ctx, &Actor::local(), "Owner@Example.test", "운영자")
        .await
        .unwrap();
    assert!(a.created);
    assert_eq!(a.item.email, "owner@example.test");
    let b = auth::ensure_owner(&e.ctx, &Actor::local(), "owner@example.test", "다른 이름")
        .await
        .unwrap();
    assert!(!b.created);
    let err = auth::ensure_owner(&e.ctx, &Actor::local(), "other@example.test", "둘째")
        .await
        .unwrap_err();
    assert_eq!(err.code(), Code::Conflict);
    assert_eq!(scalar(&e.db, "SELECT count(*) FROM members").await, 1);
    assert_eq!(
        scalar(
            &e.db,
            "SELECT count(*) FROM audit_log WHERE action = 'member.create'"
        )
        .await,
        1
    );
    e.db.close().await;
}

#[tokio::test]
async fn agent_cannot_issue_login_link() {
    let e = env().await;
    owner(&e.ctx).await;
    let agent = Actor::agent("tok".into(), &[Scope::Admin], None);
    let err = auth::issue_login_link(&e.ctx, &agent, None)
        .await
        .unwrap_err();
    assert_eq!(err.code(), Code::Forbidden);
    let nobody = Actor::human("x".into(), [Permission::SiteView], 0);
    let err = auth::issue_login_link(&e.ctx, &nobody, None)
        .await
        .unwrap_err();
    assert_eq!(err.code(), Code::Forbidden);
    e.db.close().await;
}

#[tokio::test]
async fn login_link_works_once_and_gives_owner_session() {
    let e = env().await;
    owner(&e.ctx).await;
    let link = auth::issue_login_link(&e.ctx, &Actor::local(), None)
        .await
        .unwrap();
    assert!(link.url.starts_with("http://127.0.0.1:8080/admin/link/"));
    let raw = raw_of(&link.path).to_owned();
    // 원문은 저장되지 않는다.
    let stored: String = sqlx::query_scalar("SELECT token_hash FROM login_tickets")
        .fetch_one(e.db.read_pool())
        .await
        .unwrap();
    assert_ne!(stored, raw);

    assert_eq!(
        auth::peek_link(&e.ctx, &raw).await.unwrap().email,
        "owner@example.test"
    );
    let s = auth::consume_link(&e.ctx, &raw).await.unwrap();
    let who = auth::signed_in(&e.ctx, &s.secret).await.unwrap().unwrap();
    assert!(who.actor.has(Permission::SiteAdmin));
    assert!(who.actor.has(Permission::PostPublish));
    assert_eq!(who.session.csrf_token.len(), 64);

    // 두 번째 사용, 확인 화면 모두 거부.
    assert_eq!(
        auth::consume_link(&e.ctx, &raw).await.unwrap_err().code(),
        Code::Unauthorized
    );
    assert!(auth::peek_link(&e.ctx, &raw).await.is_err());
    // 엉뚱한 값.
    assert!(auth::consume_link(&e.ctx, "nope").await.is_err());
    assert!(auth::signed_in(&e.ctx, "nope").await.unwrap().is_none());
    assert!(auth::signed_in(&e.ctx, "").await.unwrap().is_none());
    e.db.close().await;
}

#[tokio::test]
async fn new_link_revokes_old_one() {
    let e = env().await;
    owner(&e.ctx).await;
    let old = auth::issue_login_link(&e.ctx, &Actor::local(), None)
        .await
        .unwrap();
    let new = auth::issue_login_link(&e.ctx, &Actor::local(), Some("owner@example.test"))
        .await
        .unwrap();
    assert!(auth::consume_link(&e.ctx, raw_of(&old.path)).await.is_err());
    assert!(auth::consume_link(&e.ctx, raw_of(&new.path)).await.is_ok());
    e.db.close().await;
}

#[tokio::test]
async fn expired_link_and_session_are_refused_and_purged() {
    let e = env().await;
    owner(&e.ctx).await;
    let link = auth::issue_login_link(&e.ctx, &Actor::local(), None)
        .await
        .unwrap();
    let s = auth::issue_login_link(&e.ctx, &Actor::local(), None)
        .await
        .unwrap();
    let session = auth::consume_link(&e.ctx, raw_of(&s.path)).await.unwrap();
    let _ = link;
    sqlx::query("UPDATE login_tickets SET expires_at = '2000-01-01T00:00:00Z'")
        .execute(e.db.write_pool())
        .await
        .unwrap();
    sqlx::query("UPDATE sessions SET expires_at = '2000-01-01T00:00:00Z'")
        .execute(e.db.write_pool())
        .await
        .unwrap();
    assert!(
        auth::signed_in(&e.ctx, &session.secret)
            .await
            .unwrap()
            .is_none()
    );
    let purged = auth::purge_expired(&e.ctx).await.unwrap();
    assert!(purged >= 2, "{purged}");
    assert_eq!(scalar(&e.db, "SELECT count(*) FROM sessions").await, 0);
    assert_eq!(scalar(&e.db, "SELECT count(*) FROM login_tickets").await, 0);
    e.db.close().await;
}

#[tokio::test]
async fn concurrent_link_use_succeeds_once() {
    let e = env().await;
    owner(&e.ctx).await;
    let link = auth::issue_login_link(&e.ctx, &Actor::local(), None)
        .await
        .unwrap();
    let raw = raw_of(&link.path).to_owned();
    let mut handles = Vec::new();
    for _ in 0..8 {
        let ctx = e.ctx.clone();
        let raw = raw.clone();
        handles.push(tokio::spawn(
            async move { auth::consume_link(&ctx, &raw).await },
        ));
    }
    let mut ok = 0;
    for h in handles {
        if h.await.unwrap().is_ok() {
            ok += 1;
        }
    }
    assert_eq!(ok, 1);
    assert_eq!(scalar(&e.db, "SELECT count(*) FROM sessions").await, 1);
    e.db.close().await;
}

#[tokio::test]
async fn password_login_and_change_drop_other_sessions() {
    let e = env().await;
    owner(&e.ctx).await;
    // 비밀번호가 없으면 어떤 값도 실패한다.
    assert_eq!(
        auth::login_password(&e.ctx, "owner@example.test", "")
            .await
            .unwrap_err()
            .code(),
        Code::Unauthorized
    );
    let l1 = auth::issue_login_link(&e.ctx, &Actor::local(), None)
        .await
        .unwrap();
    let first = auth::consume_link(&e.ctx, raw_of(&l1.path)).await.unwrap();
    let l2 = auth::issue_login_link(&e.ctx, &Actor::local(), None)
        .await
        .unwrap();
    let second = auth::consume_link(&e.ctx, raw_of(&l2.path)).await.unwrap();
    let who = auth::signed_in(&e.ctx, &first.secret)
        .await
        .unwrap()
        .unwrap();

    assert_eq!(
        auth::set_password(&e.ctx, &who, "짧음")
            .await
            .unwrap_err()
            .code(),
        Code::Validation
    );
    let fresh = auth::set_password(&e.ctx, &who, "시험용-비밀번호-1234")
        .await
        .unwrap();
    assert!(
        auth::signed_in(&e.ctx, &first.secret)
            .await
            .unwrap()
            .is_none()
    );
    assert!(
        auth::signed_in(&e.ctx, &second.secret)
            .await
            .unwrap()
            .is_none()
    );
    assert!(
        auth::signed_in(&e.ctx, &fresh.secret)
            .await
            .unwrap()
            .is_some()
    );

    // 해시는 argon2id, 원문 아님.
    let hash: String = sqlx::query_scalar("SELECT password_hash FROM members")
        .fetch_one(e.db.read_pool())
        .await
        .unwrap();
    assert!(hash.starts_with("$argon2id$"));

    let wrong = auth::login_password(&e.ctx, "owner@example.test", "틀린-비밀번호-0000")
        .await
        .unwrap_err();
    let unknown = auth::login_password(&e.ctx, "ghost@example.test", "시험용-비밀번호-1234")
        .await
        .unwrap_err();
    // 실패 사유를 구분하지 않는다.
    assert_eq!(wrong.code(), unknown.code());
    assert_eq!(wrong.message(), unknown.message());
    let ok = auth::login_password(&e.ctx, "OWNER@example.test", "시험용-비밀번호-1234")
        .await
        .unwrap();
    let who = auth::signed_in(&e.ctx, &ok.secret).await.unwrap().unwrap();
    auth::logout(&e.ctx, &who).await.unwrap();
    assert!(auth::signed_in(&e.ctx, &ok.secret).await.unwrap().is_none());

    // 감사 기록에 비밀 값이 없다.
    let rows: Vec<String> = sqlx::query_scalar("SELECT diff FROM audit_log")
        .fetch_all(e.db.read_pool())
        .await
        .unwrap();
    for d in rows {
        assert!(!d.contains("시험용-비밀번호"), "{d}");
        assert!(!d.contains("argon2"), "{d}");
        assert!(!d.contains(&first.secret), "{d}");
    }
    e.db.close().await;
}

#[tokio::test]
async fn disabled_member_loses_session() {
    let e = env().await;
    owner(&e.ctx).await;
    let l = auth::issue_login_link(&e.ctx, &Actor::local(), None)
        .await
        .unwrap();
    let s = auth::consume_link(&e.ctx, raw_of(&l.path)).await.unwrap();
    sqlx::query("UPDATE members SET status = 'disabled'")
        .execute(e.db.write_pool())
        .await
        .unwrap();
    assert!(auth::signed_in(&e.ctx, &s.secret).await.unwrap().is_none());
    assert_eq!(
        auth::issue_login_link(&e.ctx, &Actor::local(), None)
            .await
            .unwrap_err()
            .code(),
        Code::Forbidden
    );
    e.db.close().await;
}

#[test]
fn same_secret_compares_whole_value() {
    assert!(auth::same_secret("abc", "abc"));
    assert!(!auth::same_secret("abc", "abd"));
    assert!(!auth::same_secret("abc", "ab"));
    assert!(!auth::same_secret("", "a"));
}
