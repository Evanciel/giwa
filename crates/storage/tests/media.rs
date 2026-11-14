//! 미디어 서비스. 실제 SQLite 파일과 데이터 디렉터리의 `media/` 로 검증한다.
#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::io::Cursor;
use std::sync::Arc;

use giwa_core::actor::{Actor, Scope};
use giwa_core::model::PageReq;
use giwa_core::services::anonymous_actor;
use giwa_core::services::media::{self, Crop, MediaPatch, Transform, Upload};
use giwa_core::{Code, Ctx};
use giwa_storage::{DataDir, Db, FsBlobs, SqliteStore};
use image::{DynamicImage, ImageFormat, Rgb, RgbImage};

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
        extensions: giwa_core::store::no_extensions(),
    };
    Env {
        _tmp: tmp,
        dir,
        db,
        ctx,
    }
}

fn jpeg(w: u32, h: u32) -> Vec<u8> {
    let img = DynamicImage::ImageRgb8(RgbImage::from_pixel(w, h, Rgb([30, 120, 200])));
    let mut buf = Vec::new();
    img.write_to(&mut Cursor::new(&mut buf), ImageFormat::Jpeg)
        .unwrap();
    buf
}

fn upload(name: &str, bytes: Vec<u8>) -> Upload {
    Upload {
        filename: name.into(),
        bytes,
        alt_text: Some("파란 사각형".into()),
    }
}

fn files_under(dir: &std::path::Path) -> Vec<String> {
    let mut out = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        for e in std::fs::read_dir(&d).unwrap() {
            let p = e.unwrap().path();
            if p.is_dir() {
                stack.push(p);
            } else {
                out.push(p.strip_prefix(dir).unwrap().display().to_string());
            }
        }
    }
    out.sort();
    out
}

async fn audit_actions(db: &Db) -> Vec<String> {
    sqlx::query_scalar("SELECT action FROM audit_log WHERE target_type = 'media' ORDER BY at")
        .fetch_all(db.read_pool())
        .await
        .unwrap()
}

#[tokio::test]
async fn upload_list_update_delete_with_files_and_audit() {
    let e = env().await;
    let local = Actor::local();
    let m = media::upload(&e.ctx, &local, upload("여름 사진.jpeg", jpeg(1500, 900)))
        .await
        .unwrap();
    assert_eq!(m.mime, "image/jpeg");
    assert_eq!((m.width, m.height), (1500, 900));
    assert!(m.path.ends_with(&format!("{}.jpg", m.id)), "{}", m.path);
    assert_eq!(m.url, format!("/media/{}", m.path));
    assert_eq!(m.variants.len(), 2);
    assert!(
        m.variants
            .iter()
            .all(|v| v.url == format!("/media/{}", v.path))
    );
    // 사용자 파일 이름은 경로에 들어가지 않는다.
    assert!(!m.path.contains("여름"));
    assert_eq!(files_under(&e.dir.media_dir()).len(), 3);

    let got = media::get(&e.ctx, &local, &m.id).await.unwrap();
    assert_eq!(got, m);
    let list = media::list(&e.ctx, &local, PageReq::default())
        .await
        .unwrap();
    assert_eq!((list.total, list.items.len()), (1, 1));

    let patch = MediaPatch {
        alt_text: Some("하늘색 사각형".into()),
    };
    let after = media::update(&e.ctx, &local, &m.id, patch.clone())
        .await
        .unwrap();
    assert_eq!(after.alt_text.as_deref(), Some("하늘색 사각형"));
    // 같은 값이면 감사 기록이 늘지 않는다.
    media::update(&e.ctx, &local, &m.id, patch).await.unwrap();

    media::delete(&e.ctx, &local, &m.id, false).await.unwrap();
    assert!(files_under(&e.dir.media_dir()).is_empty());
    let err = media::get(&e.ctx, &local, &m.id).await.unwrap_err();
    assert_eq!(err.code(), Code::NotFound);
    assert_eq!(
        audit_actions(&e.db).await,
        vec!["media.upload", "media.update", "media.delete"]
    );
}

#[tokio::test]
async fn rejected_upload_leaves_no_file_and_no_row() {
    let e = env().await;
    let err = media::upload(
        &e.ctx,
        &Actor::local(),
        upload("x.png", b"<svg onload=alert(1)>".to_vec()),
    )
    .await
    .unwrap_err();
    assert_eq!(err.code(), Code::Validation);
    assert!(files_under(&e.dir.media_dir()).is_empty());
    assert!(audit_actions(&e.db).await.is_empty());
}

#[tokio::test]
async fn permissions_follow_scopes() {
    let e = env().await;
    // 기본 토큰(read, draft)은 올릴 수 있지만 지울 수 없다.
    let agent = Actor::agent("t1".into(), &Scope::DEFAULT, None);
    let m = media::upload(&e.ctx, &agent, upload("a.jpg", jpeg(50, 50)))
        .await
        .unwrap();
    assert!(m.variants.is_empty());
    let err = media::delete(&e.ctx, &agent, &m.id, false)
        .await
        .unwrap_err();
    assert_eq!(err.code(), Code::Forbidden);

    let reader = Actor::agent("t2".into(), &[Scope::Read], None);
    let err = media::upload(&e.ctx, &reader, upload("a.jpg", jpeg(50, 50)))
        .await
        .unwrap_err();
    assert_eq!(err.code(), Code::Forbidden);

    // 비로그인은 로그인하면 될 수 있다는 뜻의 401.
    let guest = anonymous_actor(&e.ctx).await.unwrap();
    assert_eq!(
        media::list(&e.ctx, &guest, PageReq::default())
            .await
            .unwrap_err()
            .code(),
        Code::Unauthorized
    );
}

/// 돌리기·자르기는 새 사진을 만들고 원본은 그대로 둔다 (ADR-0077).
#[tokio::test]
async fn transform_makes_a_new_media_and_keeps_the_original() {
    let e = env().await;
    let local = Actor::local();
    let m = media::upload(&e.ctx, &local, upload("가로.jpg", jpeg(1200, 800)))
        .await
        .unwrap();
    let t = Transform {
        rotate: 90,
        crop: Some(Crop {
            x: 100,
            y: 200,
            width: 600,
            height: 400,
        }),
    };
    let edited = media::transform(&e.ctx, &local, &m.id, t).await.unwrap();
    assert_ne!(edited.id, m.id);
    assert_eq!((edited.width, edited.height), (600, 400));
    assert_eq!(edited.mime, "image/jpeg");
    assert_eq!(edited.alt_text.as_deref(), Some("파란 사각형"));
    // 원본 행·파일이 남는다.
    let original = media::get(&e.ctx, &local, &m.id).await.unwrap();
    assert_eq!((original.width, original.height), (1200, 800));
    assert!(e.dir.media_dir().join(&original.path).exists());
    assert!(e.dir.media_dir().join(&edited.path).exists());
    let list = media::list(&e.ctx, &local, PageReq::default())
        .await
        .unwrap();
    assert_eq!(list.total, 2);
    assert_eq!(
        audit_actions(&e.db).await,
        vec!["media.upload", "media.edit"]
    );

    // 돌린 뒤 크기(800×1200) 밖으로 자르면 거절하고 아무것도 남기지 않는다.
    let bad = Transform {
        rotate: 90,
        crop: Some(Crop {
            x: 0,
            y: 0,
            width: 1200,
            height: 800,
        }),
    };
    let err = media::transform(&e.ctx, &local, &m.id, bad)
        .await
        .unwrap_err();
    assert_eq!(err.code(), Code::Validation);
    for rotate in [45, 360] {
        let err = media::transform(&e.ctx, &local, &m.id, Transform { rotate, crop: None })
            .await
            .unwrap_err();
        assert_eq!(err.code(), Code::Validation, "{rotate}");
    }
    let empty = Transform {
        rotate: 0,
        crop: None,
    };
    let err = media::transform(&e.ctx, &local, &m.id, empty)
        .await
        .unwrap_err();
    assert_eq!(err.code(), Code::Validation);
    // 읽기만 하는 토큰은 못 한다.
    let reader = Actor::agent("t".into(), &[Scope::Read], None);
    let t = Transform {
        rotate: 180,
        crop: None,
    };
    let err = media::transform(&e.ctx, &reader, &m.id, t)
        .await
        .unwrap_err();
    assert_eq!(err.code(), Code::Forbidden);
    assert_eq!(
        media::list(&e.ctx, &local, PageReq::default())
            .await
            .unwrap()
            .total,
        2
    );
}
