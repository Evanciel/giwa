//! 백업·복원 (GAP S6, ADR-0063). 실제 SQLite 파일로 사본·비밀 빼기·복원·잠금·나쁜 파일 거부를 검증한다.
#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::fs::File;
use std::path::Path;
use std::sync::Arc;

use flate2::read::GzDecoder;
use maru_core::actor::Actor;
use maru_core::services::backups::{self, NewBackup};
use maru_core::services::boards::{self, NewBoard};
use maru_core::services::posts::{self, NewPost};
use maru_core::services::tokens::{self, NewToken};
use maru_core::{Code, Ctx};
use maru_storage::backup::{self, FsArchiver};
use maru_storage::{DataDir, Db, FsBlobs, SqliteStore};

async fn open(dir: &DataDir) -> (Db, Ctx) {
    let db = Db::open(&dir.db_path(), true).await.unwrap();
    db.migrate().await.unwrap();
    let ctx = Ctx {
        store: Arc::new(SqliteStore::new(db.clone())),
        blobs: Arc::new(FsBlobs::new(dir.media_dir())),
        site_url: "http://127.0.0.1:8080".into(),
        extensions: maru_core::store::no_extensions(),
    };
    (db, ctx)
}

async fn seed(ctx: &Ctx) -> String {
    let local = Actor::local();
    boards::create(
        ctx,
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
        ctx,
        &local,
        NewPost {
            board: "free".into(),
            title: "되살릴 글".into(),
            slug: Some("keep".into()),
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
    tokens::create(
        ctx,
        &local,
        NewToken {
            name: "에이전트".into(),
            scopes: vec![],
            board_ids: None,
            auto_apply: false,
            expires_at: None,
        },
    )
    .await
    .unwrap();
    p.id
}

/// 백업 파일의 항목 이름들.
fn entries(path: &Path) -> Vec<String> {
    let mut tar = tar::Archive::new(GzDecoder::new(File::open(path).unwrap()));
    tar.entries()
        .unwrap()
        .map(|e| {
            e.unwrap()
                .path()
                .unwrap()
                .to_string_lossy()
                .replace('\\', "/")
        })
        .collect()
}

/// 백업 안의 DB 를 꺼내 표 하나의 줄 수를 센다.
async fn count_in_backup(path: &Path, table: &str) -> i64 {
    let out = tempfile::tempdir().unwrap();
    let mut tar = tar::Archive::new(GzDecoder::new(File::open(path).unwrap()));
    for e in tar.entries().unwrap() {
        let mut e = e.unwrap();
        if e.path().unwrap().to_string_lossy() == "maru.db" {
            e.unpack(out.path().join("maru.db")).unwrap();
        }
    }
    let db = Db::open(&out.path().join("maru.db"), false).await.unwrap();
    let n: i64 = sqlx::query_scalar(sqlx::AssertSqlSafe(format!("SELECT count(*) FROM {table}")))
        .fetch_one(db.read_pool())
        .await
        .unwrap();
    db.close().await;
    n
}

#[tokio::test]
async fn backup_copies_db_and_strips_secrets_on_request() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = DataDir::new(tmp.path().join("데이터"));
    dir.ensure_layout().unwrap();
    std::fs::write(dir.media_dir().join("a.txt"), "그림 대신").unwrap();
    let (db, ctx) = open(&dir).await;
    seed(&ctx).await;
    let archiver = FsArchiver::new(dir.clone(), db.clone());
    let local = Actor::local();

    let full = backups::create(&ctx, &local, &archiver, NewBackup::default())
        .await
        .unwrap();
    assert!(full.secrets);
    assert!(full.name.starts_with("maru-") && full.name.ends_with(".tar.gz"));
    let bare = backups::create(&ctx, &local, &archiver, NewBackup { no_secrets: true })
        .await
        .unwrap();
    assert!(!bare.secrets);
    assert!(bare.name.ends_with("-nosecrets.tar.gz"), "{}", bare.name);
    assert_ne!(full.name, bare.name);

    let list = backups::list(&local, &archiver).await.unwrap();
    assert_eq!(list.len(), 2);
    let full_path = backups::file(&local, &archiver, &full.name).await.unwrap();
    let names = entries(&full_path);
    assert_eq!(names[0], "manifest.json", "{names:?}");
    for want in ["maru.db", "media/a.txt"] {
        assert!(names.iter().any(|n| n == want), "{want} 없음: {names:?}");
    }
    assert!(
        !names.iter().any(|n| n.starts_with("backups")),
        "백업 안에 백업을 넣지 않는다: {names:?}"
    );
    assert_eq!(count_in_backup(&full_path, "tokens").await, 1);
    assert_eq!(count_in_backup(&full_path, "posts").await, 1);
    let bare_path = backups::file(&local, &archiver, &bare.name).await.unwrap();
    assert_eq!(count_in_backup(&bare_path, "tokens").await, 0);
    assert_eq!(count_in_backup(&bare_path, "posts").await, 1);

    // 이름 검사·없는 파일·권한
    let err = backups::file(&local, &archiver, "../maru.db")
        .await
        .unwrap_err();
    assert_eq!(err.code(), Code::NotFound);
    let err = backups::delete(&ctx, &local, &archiver, "maru-20000101-000000.tar.gz")
        .await
        .unwrap_err();
    assert_eq!(err.code(), Code::NotFound);
    let agent = Actor::agent("t".into(), &[maru_core::actor::Scope::Admin], None);
    let err = backups::list(&agent, &archiver).await.unwrap_err();
    assert_eq!(err.code(), Code::Forbidden, "에이전트는 백업을 못 다룬다");

    backups::delete(&ctx, &local, &archiver, &bare.name)
        .await
        .unwrap();
    assert_eq!(backups::list(&local, &archiver).await.unwrap().len(), 1);
    db.close().await;
}

#[tokio::test]
async fn restore_brings_back_data_and_keeps_previous() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = DataDir::new(tmp.path().join("데이터"));
    dir.ensure_layout().unwrap();
    let (db, ctx) = open(&dir).await;
    let id = seed(&ctx).await;
    let local = Actor::local();
    let archiver = FsArchiver::new(dir.clone(), db.clone());
    let b = backups::create(&ctx, &local, &archiver, NewBackup::default())
        .await
        .unwrap();
    let path = dir.backups_dir().join(&b.name);
    posts::delete(&ctx, &local, &id).await.unwrap();

    // 서버가 잠금을 쥐고 있으면 복원하지 않는다.
    let lock = backup::lock_serve(&dir).unwrap();
    let err = backup::restore(&dir, &path).await.unwrap_err();
    assert_eq!(err.code(), Code::Conflict);
    assert!(backup::lock_serve(&dir).is_err(), "두 번째 서버도 못 뜬다");
    drop(lock);

    db.close().await;
    let report = backup::restore(&dir, &path).await.unwrap();
    assert!(report.manifest.secrets);
    assert!(report.previous.starts_with(dir.backups_dir()));
    assert!(
        report.previous.join("maru.db").exists(),
        "이전 DB 를 옮겨 둔다"
    );
    assert!(path.exists(), "백업 파일은 그대로");

    let (db, ctx) = open(&dir).await;
    let back = posts::get(&ctx, &local, &id).await.unwrap();
    assert_eq!(back.title, "되살릴 글");
    db.close().await;
}

#[tokio::test]
async fn restore_rejects_foreign_archives() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = DataDir::new(tmp.path().join("데이터"));
    dir.ensure_layout().unwrap();

    let make = |name: &str, entries: &[(&str, &[u8])]| {
        let path = tmp.path().join(name);
        let gz = flate2::write::GzEncoder::new(
            File::create(&path).unwrap(),
            flate2::Compression::fast(),
        );
        let mut b = tar::Builder::new(gz);
        for (p, data) in entries {
            let mut h = tar::Header::new_gnu();
            h.set_size(data.len() as u64);
            h.set_mode(0o644);
            h.set_cksum();
            b.append_data(&mut h, p, *data).unwrap();
        }
        b.into_inner().unwrap().finish().unwrap();
        path
    };
    let manifest = |schema: i64| {
        serde_json::json!({
            "format": 1, "maru_version": "9.9.9", "schema": schema,
            "created_at": "2026-11-04T00:00:00Z", "secrets": true
        })
        .to_string()
        .into_bytes()
    };

    let stray = make(
        "stray.tar.gz",
        &[("manifest.json", &manifest(1)), ("evil.txt", b"x")],
    );
    let err = backup::restore(&dir, &stray).await.unwrap_err();
    assert_eq!(err.code(), Code::Validation, "{err}");

    let no_manifest = make("nomani.tar.gz", &[("maru.db", b"x")]);
    assert_eq!(
        backup::restore(&dir, &no_manifest)
            .await
            .unwrap_err()
            .code(),
        Code::Validation
    );

    let newer = make(
        "newer.tar.gz",
        &[
            ("manifest.json", &manifest(99_999_999_999_999)),
            ("maru.db", b"x"),
        ],
    );
    let err = backup::restore(&dir, &newer).await.unwrap_err();
    assert_eq!(err.code(), Code::SchemaTooNew, "{err}");

    let not_gz = tmp.path().join("plain.tar.gz");
    std::fs::write(&not_gz, b"not a gzip").unwrap();
    assert_eq!(
        backup::restore(&dir, &not_gz).await.unwrap_err().code(),
        Code::Validation
    );
    assert!(
        std::fs::read_dir(dir.backups_dir())
            .unwrap()
            .next()
            .is_none(),
        "거부하면 지금 데이터를 옮기지 않는다"
    );
}
