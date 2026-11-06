#![allow(clippy::expect_used, clippy::unwrap_used)]

use maru_core::Code;
use maru_core::config::Config;
use maru_storage::{DataDir, Db};

/// 한글과 공백이 섞인 경로 (Windows 사용자 폴더 대비).
fn data_dir() -> (tempfile::TempDir, DataDir) {
    let tmp = tempfile::tempdir().unwrap();
    let dir = DataDir::new(tmp.path().join("내 블로그 데이터"));
    dir.ensure_layout().unwrap();
    (tmp, dir)
}

async fn open(dir: &DataDir) -> Db {
    let db = Db::open(&dir.db_path(), true).await.unwrap();
    db.migrate().await.unwrap();
    db
}

async fn count(db: &Db, sql: &'static str) -> i64 {
    sqlx::query_scalar(sql)
        .fetch_one(db.read_pool())
        .await
        .unwrap()
}

#[tokio::test]
async fn migrate_creates_schema_with_wal_and_fk() {
    let (_tmp, dir) = data_dir();
    let db = open(&dir).await;

    let mode: String = sqlx::query_scalar("PRAGMA journal_mode")
        .fetch_one(db.write_pool())
        .await
        .unwrap();
    assert_eq!(mode, "wal");
    assert_eq!(count(&db, "PRAGMA foreign_keys").await, 1);
    assert_eq!(count(&db, "SELECT count(*) FROM roles").await, 5);
    assert_eq!(
        count(
            &db,
            "SELECT count(*) FROM sqlite_master WHERE type='table' AND name IN \
             ('members','tokens','audit_log','boards','posts','revisions','media','site_settings')"
        )
        .await,
        8
    );
    db.close().await;
    assert!(dir.is_initialized());
}

#[tokio::test]
async fn migrate_is_idempotent() {
    let (_tmp, dir) = data_dir();
    let db = open(&dir).await;
    db.migrate().await.unwrap();
    db.close().await;
    let db = open(&dir).await;
    assert_eq!(count(&db, "SELECT count(*) FROM roles").await, 5);
    db.close().await;
}

#[tokio::test]
async fn up_down_up() {
    let (_tmp, dir) = data_dir();
    let db = open(&dir).await;
    db.undo(0).await.unwrap();
    assert_eq!(
        count(&db, "SELECT count(*) FROM sqlite_master WHERE name='posts'").await,
        0
    );
    db.migrate().await.unwrap();
    assert_eq!(count(&db, "SELECT count(*) FROM roles").await, 5);
    db.close().await;
}

#[tokio::test]
async fn read_pool_cannot_write() {
    let (_tmp, dir) = data_dir();
    let db = open(&dir).await;
    let res = sqlx::query("INSERT INTO site_settings(key, value, updated_at) VALUES ('x','1','t')")
        .execute(db.read_pool())
        .await;
    assert!(res.is_err());
    db.close().await;
}

#[tokio::test]
async fn foreign_keys_are_enforced() {
    let (_tmp, dir) = data_dir();
    let db = open(&dir).await;
    let res =
        sqlx::query("INSERT INTO member_roles(member_id, role_id) VALUES ('nobody','role_admin')")
            .execute(db.write_pool())
            .await;
    assert!(res.is_err());
    db.close().await;
}

#[tokio::test]
async fn newer_schema_is_rejected() {
    let (_tmp, dir) = data_dir();
    let db = open(&dir).await;
    sqlx::query(
        "INSERT INTO _sqlx_migrations(version, description, success, checksum, execution_time) \
         VALUES (99991231000000, 'from the future', 1, x'00', 0)",
    )
    .execute(db.write_pool())
    .await
    .unwrap();
    let err = db.migrate().await.unwrap_err();
    assert_eq!(err.code(), Code::SchemaTooNew);
    db.close().await;
}

#[tokio::test]
async fn open_without_create_fails_when_missing() {
    let (_tmp, dir) = data_dir();
    assert!(Db::open(&dir.db_path(), false).await.is_err());
}

#[test]
fn config_roundtrip_and_not_overwritten() {
    let (_tmp, dir) = data_dir();
    assert_eq!(dir.read_config().unwrap_err().code(), Code::NotFound);

    let cfg = Config {
        bind: "127.0.0.1:3139".into(),
        site_url: "https://blog.example.com".into(),
        mail: None,
        trust_tunnel: false,
    };
    assert!(dir.write_config_if_absent(&cfg).unwrap());
    assert!(!dir.write_config_if_absent(&Config::default()).unwrap());
    assert_eq!(dir.read_config().unwrap(), cfg);
}

#[test]
fn bad_config_is_validation_error() {
    let (_tmp, dir) = data_dir();
    std::fs::write(dir.config_path(), "bind = 1\n").unwrap();
    assert_eq!(dir.read_config().unwrap_err().code(), Code::Validation);
}
