//! 공개 글쓰기 파일 붙이기 (GAP M4, ADR-0065). 게시판 역할·손님 거부·하루 한도·종류 검사를 검증한다.
#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::sync::Arc;

use maru_core::actor::{Actor, Permission, rank};
use maru_core::model::BoardPolicy;
use maru_core::services::board_posts::{self, MEMBER_UPLOADS_PER_DAY};
use maru_core::services::boards::{self, BoardPatch, NewBoard};
use maru_core::{Code, Ctx};
use maru_storage::{DataDir, Db, FsBlobs, SqliteStore};

struct Env {
    _tmp: tempfile::TempDir,
    _db: Db,
    ctx: Ctx,
}

async fn env(upload: &str) -> Env {
    let tmp = tempfile::tempdir().unwrap();
    let dir = DataDir::new(tmp.path().join("데이터"));
    dir.ensure_layout().unwrap();
    let db = Db::open(&dir.db_path(), true).await.unwrap();
    db.migrate().await.unwrap();
    let ctx = Ctx {
        store: Arc::new(SqliteStore::new(db.clone())),
        blobs: Arc::new(FsBlobs::new(dir.media_dir())),
        site_url: "http://127.0.0.1:8080".into(),
    };
    boards::create(
        &ctx,
        &Actor::local(),
        NewBoard {
            slug: "free".into(),
            name: "자유".into(),
            kind: "free".into(),
            policy: Some(BoardPolicy {
                upload: upload.into(),
                ..BoardPolicy::default()
            }),
            sort_order: 0,
            layout: None,
            field_schema: None,
            list_order: None,
        },
    )
    .await
    .unwrap();
    Env {
        _tmp: tmp,
        _db: db,
        ctx,
    }
}

fn member(id: &str) -> Actor {
    Actor::human(id.to_owned(), [Permission::PostWrite], rank::MEMBER)
}

fn text() -> Vec<u8> {
    "함께 볼 메모".as_bytes().to_vec()
}

#[tokio::test]
async fn upload_follows_the_board_role_and_refuses_guests() {
    let e = env("none").await;
    let m = member("m1");
    let attach = |a: Actor, name: &'static str, bytes: Vec<u8>| {
        let ctx = e.ctx.clone();
        async move { board_posts::attach(&ctx, &a, "free", name.into(), bytes).await }
    };

    // 기본은 받지 않는다.
    let err = attach(m.clone(), "memo.txt", text()).await.unwrap_err();
    assert_eq!(err.code(), Code::Forbidden);

    let open = |role: &str| BoardPatch {
        policy: Some(BoardPolicy {
            upload: role.into(),
            ..BoardPolicy::default()
        }),
        ..BoardPatch::default()
    };
    boards::update(&e.ctx, &Actor::local(), "free", open("member"))
        .await
        .unwrap();

    let ok = attach(m.clone(), "메모[1].txt", text()).await.unwrap();
    assert!(!ok.image);
    assert!(ok.url.starts_with("/files/"), "{}", ok.url);
    assert_eq!(ok.markdown, format!("[메모1.txt (1KB)]({})", ok.url));

    // 손님은 역할과 상관없이 안 되고, 손님 역할은 고를 수 없다.
    let guest = Actor::anonymous([Permission::SiteView]);
    let err = attach(guest, "memo.txt", text()).await.unwrap_err();
    assert_eq!(err.code(), Code::Unauthorized);
    let err = boards::update(&e.ctx, &Actor::local(), "free", open("guest"))
        .await
        .unwrap_err();
    assert_eq!(err.code(), Code::Validation);
    let err = boards::update(&e.ctx, &Actor::local(), "free", open("robot"))
        .await
        .unwrap_err();
    assert_eq!(err.code(), Code::Validation);

    // 에이전트는 이 경로를 쓰지 않는다.
    let agent = Actor::agent(
        "t".into(),
        &[
            maru_core::actor::Scope::Read,
            maru_core::actor::Scope::Draft,
        ],
        None,
    );
    let err = attach(agent, "memo.txt", text()).await.unwrap_err();
    assert_eq!(err.code(), Code::Forbidden);

    // 역할이 높아지면 회원은 막힌다.
    boards::update(&e.ctx, &Actor::local(), "free", open("editor"))
        .await
        .unwrap();
    let err = attach(m.clone(), "memo.txt", text()).await.unwrap_err();
    assert_eq!(err.code(), Code::Forbidden);

    // 없는 게시판
    let err = board_posts::attach(&e.ctx, &m, "없음", "a.txt".into(), text())
        .await
        .unwrap_err();
    assert_eq!(err.code(), Code::NotFound);
}

#[tokio::test]
async fn kinds_are_checked_like_the_admin_path() {
    let e = env("member").await;
    let m = member("m1");
    for (name, bytes) in [
        ("x.html", b"<script>alert(1)</script>".to_vec()),
        ("x.svg", b"<svg onload=alert(1)></svg>".to_vec()),
        ("x.pdf", b"not a pdf".to_vec()),
        ("x.png", b"not a png".to_vec()),
        ("noext", b"abc".to_vec()),
    ] {
        let err = board_posts::attach(&e.ctx, &m, "free", name.into(), bytes)
            .await
            .unwrap_err();
        assert_eq!(err.code(), Code::Validation, "{name}");
    }
    let png: Vec<u8> = {
        let mut out = std::io::Cursor::new(Vec::new());
        image::RgbImage::new(2, 2)
            .write_to(&mut out, image::ImageFormat::Png)
            .unwrap();
        out.into_inner()
    };
    let ok = board_posts::attach(&e.ctx, &m, "free", "그림 하나.png".into(), png)
        .await
        .unwrap();
    assert!(ok.image);
    assert_eq!(ok.markdown, format!("![그림 하나]({})", ok.url));
}

#[tokio::test]
async fn members_have_a_daily_quota_but_editors_do_not() {
    let e = env("member").await;
    let m = member("m1");
    for _ in 0..MEMBER_UPLOADS_PER_DAY {
        board_posts::attach(&e.ctx, &m, "free", "a.txt".into(), text())
            .await
            .unwrap();
    }
    let err = board_posts::attach(&e.ctx, &m, "free", "a.txt".into(), text())
        .await
        .unwrap_err();
    assert_eq!(err.code(), Code::RateLimited);

    // 다른 회원은 따로 센다.
    board_posts::attach(&e.ctx, &member("m2"), "free", "a.txt".into(), text())
        .await
        .unwrap();
    // `post.upload` 권한이 있는 사람은 세지 않는다.
    let editor = Actor::human("m1".to_owned(), [Permission::PostUpload], rank::EDITOR);
    board_posts::attach(&e.ctx, &editor, "free", "a.txt".into(), text())
        .await
        .unwrap();
}
