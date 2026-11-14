//! 올린 서체 서비스 (ADR-0048). 실제 SQLite 파일과 데이터 디렉터리의 보관소로 검증한다.
#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::sync::Arc;

use giwa_core::actor::{Actor, Scope};
use giwa_core::markdown;
use giwa_core::services::fonts::{self, FontPatch, FontUpload, NewFont};
use giwa_core::services::{anonymous_actor, design};
use giwa_core::{Code, Ctx};
use giwa_storage::{DataDir, Db, FsBlobs, SqliteStore};
use serde_json::json;

const PEN: &[u8] = include_bytes!("../../themes/builtin/static/fonts/maru-pen-1.woff2");

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

fn designer() -> Actor {
    Actor::agent("t-design".into(), &[Scope::Read, Scope::Design], None)
}

fn new_font(name: &str, slug: Option<&str>) -> NewFont {
    NewFont {
        name: name.into(),
        slug: slug.map(Into::into),
        license: "OFL-1.1".into(),
        rights_confirmed: true,
        ..NewFont::default()
    }
}

/// 표 하나(`len` 바이트)짜리 sfnt.
fn ttf(len: usize) -> Vec<u8> {
    let mut b = vec![0, 1, 0, 0, 0, 1, 0, 16, 0, 0, 0, 0];
    b.extend_from_slice(b"glyf");
    b.extend_from_slice(&[0, 0, 0, 0]);
    b.extend_from_slice(&28u32.to_be_bytes());
    b.extend_from_slice(&u32::try_from(len).unwrap().to_be_bytes());
    b.resize(28 + len, 7);
    b
}

fn stored(e: &Env) -> Vec<String> {
    let root = e.dir.media_dir().join("fonts");
    let mut out = Vec::new();
    let mut stack = vec![root.clone()];
    while let Some(d) = stack.pop() {
        let Ok(rd) = std::fs::read_dir(&d) else {
            continue;
        };
        for entry in rd {
            let p = entry.unwrap().path();
            if p.is_dir() {
                stack.push(p);
            } else {
                out.push(
                    p.strip_prefix(&root)
                        .unwrap()
                        .display()
                        .to_string()
                        .replace('\\', "/"),
                );
            }
        }
    }
    out.sort();
    out
}

async fn audit_actions(db: &Db) -> Vec<String> {
    sqlx::query_scalar("SELECT action FROM audit_log WHERE target_type = 'font' ORDER BY at, id")
        .fetch_all(db.read_pool())
        .await
        .unwrap()
}

#[tokio::test]
async fn add_upload_replace_remove_with_blobs_and_audit() {
    let e = env().await;
    let a = designer();
    let font = fonts::add(&e.ctx, &a, new_font("Company Gothic", None))
        .await
        .unwrap();
    assert_eq!(font.slug, "company-gothic");
    // 같은 slug 로 다시 만들면 새로 만들지 않는다.
    let again = fonts::add(&e.ctx, &a, new_font("Company Gothic", None))
        .await
        .unwrap();
    assert_eq!(again.id, font.id);
    // 한글 이름은 font-1
    let ko = fonts::add(&e.ctx, &a, new_font("회사 명조", None))
        .await
        .unwrap();
    assert_eq!(ko.slug, "font-1");

    let up = fonts::add_file(
        &e.ctx,
        &a,
        "company-gothic",
        FontUpload {
            weight: 400,
            italic: false,
            bytes: PEN.to_vec(),
        },
    )
    .await
    .unwrap();
    assert!(up.warnings.is_empty());
    let f = &up.font.files[0];
    assert_eq!(f.format, "woff2");
    assert!(f.url.starts_with("/fonts/company-gothic/400-"));
    assert_eq!(stored(&e).len(), 1);
    // 같은 내용은 아무것도 쓰지 않는다.
    let same = fonts::add_file(
        &e.ctx,
        &a,
        "company-gothic",
        FontUpload {
            weight: 400,
            italic: false,
            bytes: PEN.to_vec(),
        },
    )
    .await
    .unwrap();
    assert_eq!(same.font.files[0].id, f.id);

    // 굵게 파일 추가, 그리고 보통 파일 바꾸기(옛 파일은 지워진다). 큰 파일은 경고.
    let bold = fonts::add_file(
        &e.ctx,
        &a,
        "company-gothic",
        FontUpload {
            weight: 700,
            italic: false,
            bytes: ttf(16),
        },
    )
    .await
    .unwrap();
    assert_eq!(bold.font.files.len(), 2);
    let replaced = fonts::add_file(
        &e.ctx,
        &a,
        "company-gothic",
        FontUpload {
            weight: 400,
            italic: false,
            bytes: ttf(fonts::WARN_BYTES + 1),
        },
    )
    .await
    .unwrap();
    assert_eq!(replaced.warnings.len(), 1);
    assert_eq!(replaced.font.files[0].format, "ttf");
    let files = stored(&e);
    assert_eq!(files.len(), 2, "{files:?}");
    assert!(files.iter().all(|p| !p.ends_with(".woff2")));

    // 공개 파일 내려받기: DB 에 있는 이름만
    let name = replaced.font.files[1]
        .url
        .rsplit('/')
        .next()
        .unwrap()
        .to_owned();
    let (format, bytes) = fonts::file(&e.ctx, "company-gothic", &name).await.unwrap();
    assert_eq!((format.as_str(), bytes.len()), ("ttf", 44));
    for (slug, name) in [
        ("company-gothic", "../../giwa.db"),
        ("company-gothic", "LICENSE.txt"),
        ("Company", "x.ttf"),
    ] {
        assert_eq!(
            fonts::file(&e.ctx, slug, name).await.unwrap_err().code(),
            Code::NotFound
        );
    }

    // CSS 와 글 안 꾸밈
    let css = fonts::css(&fonts::list(&e.ctx).await.unwrap());
    assert!(css.contains("font-weight: 700"));
    assert!(css.contains(".ff-company-gothic {"));
    let html = markdown::render(":font[회사]{company-gothic}");
    assert!(
        html.as_str()
            .contains("<span class=\"ff-company-gothic\">회사</span>"),
        "{}",
        html.as_str()
    );

    // 굵기 하나 떼기, 서체 지우기 → 파일이 모두 사라진다.
    let after = fonts::remove_file(&e.ctx, &a, "company-gothic", 700, false)
        .await
        .unwrap();
    assert_eq!(after.files.len(), 1);
    let removed = fonts::remove(&e.ctx, &a, "company-gothic").await.unwrap();
    assert_eq!(removed.used_in_posts, 0);
    assert!(stored(&e).is_empty());
    assert_eq!(
        audit_actions(&e.db).await,
        [
            "font.add",
            "font.add",
            "font.file.add",
            "font.file.add",
            "font.file.add",
            "font.file.remove",
            "font.remove"
        ]
    );
}

#[tokio::test]
async fn license_text_is_stored_and_served() {
    let e = env().await;
    let a = designer();
    let mut input = new_font("Open Font", Some("open-font"));
    input.license_text = Some("SIL OPEN FONT LICENSE Version 1.1".into());
    let font = fonts::add(&e.ctx, &a, input).await.unwrap();
    assert_eq!(
        font.license_url.as_deref(),
        Some("/fonts/open-font/LICENSE.txt")
    );
    let (format, bytes) = fonts::file(&e.ctx, "open-font", "LICENSE.txt")
        .await
        .unwrap();
    assert_eq!(format, "txt");
    assert!(bytes.starts_with(b"SIL OPEN"));
    // 전문 떼기 → 보관소에서도 지워진다.
    let patched = fonts::update(
        &e.ctx,
        &a,
        "open-font",
        FontPatch {
            license_text: Some(String::new()),
            ..FontPatch::default()
        },
    )
    .await
    .unwrap();
    assert!(patched.license_url.is_none());
    assert!(stored(&e).is_empty());
}

#[tokio::test]
async fn rejects_bad_input_without_leaving_files() {
    let e = env().await;
    let a = designer();
    let mut no_rights = new_font("A", Some("aa"));
    no_rights.rights_confirmed = false;
    let mut bad_license = new_font("A", Some("aa"));
    bad_license.license = "GPL".into();
    for (input, code) in [
        (no_rights, Code::Validation),
        (bad_license, Code::Validation),
        (new_font("A", Some("serif")), Code::Validation),
        (new_font("A", Some("Bad Slug")), Code::Validation),
        (new_font("", Some("aa")), Code::Validation),
    ] {
        assert_eq!(
            fonts::add(&e.ctx, &a, input).await.unwrap_err().code(),
            code
        );
    }
    fonts::add(&e.ctx, &a, new_font("A", Some("aa")))
        .await
        .unwrap();
    for (weight, bytes) in [
        (400, b"<svg onload=alert(1)>".to_vec()),
        (400, b"PK\x03\x04".to_vec()),
        (450, ttf(4)),
        (1000, ttf(4)),
    ] {
        let err = fonts::add_file(
            &e.ctx,
            &a,
            "aa",
            FontUpload {
                weight,
                italic: false,
                bytes,
            },
        )
        .await
        .unwrap_err();
        assert_eq!(err.code(), Code::Validation);
    }
    assert!(stored(&e).is_empty());
}

#[tokio::test]
async fn permissions_follow_scopes() {
    let e = env().await;
    let writer = Actor::agent("t1".into(), &Scope::DEFAULT, None);
    assert_eq!(
        fonts::add(&e.ctx, &writer, new_font("A", Some("aa")))
            .await
            .unwrap_err()
            .code(),
        Code::Forbidden
    );
    let guest = anonymous_actor(&e.ctx).await.unwrap();
    assert_eq!(
        fonts::add(&e.ctx, &guest, new_font("A", Some("aa")))
            .await
            .unwrap_err()
            .code(),
        Code::Unauthorized
    );
}

#[tokio::test]
async fn design_tokens_use_uploaded_fonts_and_block_removal() {
    let e = env().await;
    let a = designer();
    // 없는 서체는 토큰에 못 넣는다.
    let err = design::update(
        &e.ctx,
        &a.for_changeset(),
        json!({ "font": { "body": "company-gothic" } }),
    )
    .await
    .unwrap_err();
    assert_eq!(err.code(), Code::Validation);
    assert!(err.next_commands().iter().any(|c| c == "giwa font list"));

    fonts::add(&e.ctx, &a, new_font("Company Gothic", None))
        .await
        .unwrap();
    let tokens = design::update(
        &e.ctx,
        &a.for_changeset(),
        json!({ "font": { "body": "company-gothic" } }),
    )
    .await
    .unwrap();
    let css = tokens.to_css(&fonts::list(&e.ctx).await.unwrap());
    assert!(css.contains("--font-body: \"maru-u-company-gothic\", "));

    let err = fonts::remove(&e.ctx, &a, "company-gothic")
        .await
        .unwrap_err();
    assert_eq!(err.code(), Code::Conflict);
    assert!(
        err.next_commands()
            .iter()
            .any(|c| c == "giwa design set --set font.body=Pretendard")
    );
    design::update(
        &e.ctx,
        &a.for_changeset(),
        json!({ "font": { "body": "Pretendard" } }),
    )
    .await
    .unwrap();
    fonts::remove(&e.ctx, &a, "company-gothic").await.unwrap();
}
