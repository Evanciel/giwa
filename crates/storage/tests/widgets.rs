//! 위젯 배치 (GAP S1, ADR-0059). 실제 SQLite 파일로 검사·정규화·권한·감사·화면 범위·인기글을 검증한다.
#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::sync::Arc;

use maru_core::actor::{Actor, Permission, rank};
use maru_core::model::Post;
use maru_core::services::boards::{self, NewBoard};
use maru_core::services::posts::{self, NewPost};
use maru_core::services::views;
use maru_core::services::widgets::{self, NewWidget, Screen, WidgetPatch};
use maru_core::{Code, Ctx};
use maru_storage::{DataDir, Db, FsBlobs, SqliteStore};
use serde_json::{Value, json};

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
    for slug in ["notice", "free"] {
        boards::create(
            &ctx,
            &Actor::local(),
            NewBoard {
                slug: slug.into(),
                name: slug.into(),
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
    }
    Env { _tmp: tmp, db, ctx }
}

fn new(kind: &str, region: &str, scope: &str, config: Value) -> NewWidget {
    NewWidget {
        kind: kind.into(),
        region: region.into(),
        scope: scope.into(),
        title: String::new(),
        config,
        position: None,
        enabled: true,
    }
}

async fn audits(db: &Db, action: &str) -> i64 {
    sqlx::query_scalar("SELECT count(*) FROM audit_log WHERE action = ?")
        .bind(action)
        .fetch_one(db.read_pool())
        .await
        .unwrap()
}

async fn published(ctx: &Ctx, board: &str, title: &str, secret: bool) -> Post {
    let local = Actor::local();
    let p = posts::create_draft(
        ctx,
        &local,
        NewPost {
            board: board.into(),
            title: title.into(),
            slug: None,
            body_md: format!("{title} 본문"),
            excerpt: None,
            cover: None,
            category: None,
            tags: vec![],
            notice: false,
            secret,
            meta: Default::default(),
        },
    )
    .await
    .unwrap();
    posts::publish(ctx, &local, &p.id).await.unwrap()
}

#[tokio::test]
async fn place_normalizes_config_and_is_idempotent() {
    let e = env().await;
    let local = Actor::local();
    let w = widgets::place(
        &e.ctx,
        &local,
        new(
            "latest_posts",
            "sidebar",
            "all",
            json!({ "board": "Notice" }),
        ),
    )
    .await
    .unwrap();
    assert_eq!(
        w.config,
        json!({ "board": "notice", "limit": 5, "view": "list", "show_date": true, "pages": 1, "auto": false })
    );
    assert_eq!(w.sort_order, 0);
    // 같은 설정을 다시 놓으면 같은 위젯 (원칙 3)
    let again = widgets::place(
        &e.ctx,
        &local,
        new(
            "latest_posts",
            "sidebar",
            "all",
            json!({ "board": "notice" }),
        ),
    )
    .await
    .unwrap();
    assert_eq!(again.id, w.id);
    // 다른 설정이면 뒤에 붙는다
    let second = widgets::place(
        &e.ctx,
        &local,
        new("popular_posts", "sidebar", "all", Value::Null),
    )
    .await
    .unwrap();
    assert_eq!(second.sort_order, 1);
    assert_eq!(
        second.config,
        json!({ "board": "", "limit": 5, "days": 7, "style": "lines", "pages": 1, "auto": false })
    );
    assert_eq!(widgets::list(&e.ctx, &local).await.unwrap().len(), 2);
    assert_eq!(audits(&e.db, "widget.place").await, 2);
}

#[tokio::test]
async fn bad_input_is_rejected() {
    let e = env().await;
    let local = Actor::local();
    let cases = [
        new("clock", "sidebar", "all", Value::Null),
        new("search", "aside", "all", Value::Null),
        new("search", "sidebar", "post:x", Value::Null),
        new("latest_posts", "main", "all", json!({ "limit": 0 })),
        new(
            "latest_posts",
            "main",
            "all",
            json!({ "limit": 5, "color": "red" }),
        ),
        new("latest_posts", "main", "all", json!({ "view": "grid" })),
        new("popular_posts", "main", "all", json!({ "days": 400 })),
        new("markdown_block", "main", "all", json!({ "body": "  " })),
        new("menu", "main", "all", json!({ "location": "side" })),
        new(
            "notice_bar",
            "header",
            "all",
            json!({ "text": "안내", "link": "javascript:alert(1)" }),
        ),
        new("image_banner", "hero", "all", json!({ "slides": [] })),
        new(
            "image_banner",
            "hero",
            "all",
            json!({ "slides": [{ "image": "https://evil.example/a.png" }] }),
        ),
        new("gallery_grid", "main", "all", json!({})),
        new(
            "gallery_grid",
            "main",
            "all",
            json!({ "board": "free", "columns": 9 }),
        ),
    ];
    for c in cases {
        let kind = c.kind.clone();
        let err = widgets::place(&e.ctx, &local, c).await.unwrap_err();
        assert_eq!(err.code(), Code::Validation, "{kind}: {err:?}");
    }
    let missing = widgets::place(
        &e.ctx,
        &local,
        new("latest_posts", "main", "all", json!({ "board": "nope" })),
    )
    .await
    .unwrap_err();
    assert_eq!(missing.code(), Code::NotFound);
    assert!(widgets::list(&e.ctx, &local).await.unwrap().is_empty());
}

#[tokio::test]
async fn only_design_editors_write() {
    let e = env().await;
    let member = Actor::human(
        "m1".into(),
        [Permission::PostRead, Permission::PostWrite],
        rank::MEMBER,
    );
    let err = widgets::place(
        &e.ctx,
        &member,
        new("search", "sidebar", "all", Value::Null),
    )
    .await
    .unwrap_err();
    assert_eq!(err.code(), Code::Forbidden);
    assert_eq!(
        widgets::list(&e.ctx, &member).await.unwrap_err().code(),
        Code::Forbidden
    );
    let designer = Actor::human("d1".into(), [Permission::DesignEdit], rank::EDITOR);
    let w = widgets::place(
        &e.ctx,
        &designer,
        new("search", "sidebar", "all", Value::Null),
    )
    .await
    .unwrap();
    let err = widgets::remove(&e.ctx, &member, &w.id).await.unwrap_err();
    assert_eq!(err.code(), Code::Forbidden);
    widgets::remove(&e.ctx, &designer, &w.id).await.unwrap();
    assert_eq!(audits(&e.db, "widget.remove").await, 1);
    let gone = widgets::remove(&e.ctx, &designer, &w.id).await.unwrap_err();
    assert_eq!(gone.code(), Code::NotFound);
}

#[tokio::test]
async fn configure_changes_and_audits_only_real_changes() {
    let e = env().await;
    let local = Actor::local();
    let w = widgets::place(
        &e.ctx,
        &local,
        new(
            "notice_bar",
            "header",
            "all",
            json!({ "text": "점검 안내", "link": "/p/notice" }),
        ),
    )
    .await
    .unwrap();
    let same = widgets::configure(&e.ctx, &local, &w.id, WidgetPatch::default())
        .await
        .unwrap();
    assert_eq!(same, w);
    assert_eq!(audits(&e.db, "widget.configure").await, 0);
    let after = widgets::configure(
        &e.ctx,
        &local,
        &w.id,
        WidgetPatch {
            scope: Some("board:Free".into()),
            enabled: Some(false),
            config: Some(json!({ "text": "새 안내" })),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    assert_eq!(after.scope, "board:free");
    assert!(!after.enabled);
    assert_eq!(after.config, json!({ "text": "새 안내", "link": "" }));
    assert_eq!(audits(&e.db, "widget.configure").await, 1);
    let bad = widgets::configure(
        &e.ctx,
        &local,
        &w.id,
        WidgetPatch {
            config: Some(json!({ "link": "/x" })),
            ..Default::default()
        },
    )
    .await
    .unwrap_err();
    assert_eq!(bad.code(), Code::Validation);
}

#[tokio::test]
async fn screens_get_matching_enabled_widgets_in_order() {
    let e = env().await;
    let local = Actor::local();
    let all = widgets::place(&e.ctx, &local, new("search", "sidebar", "all", Value::Null))
        .await
        .unwrap();
    let home = widgets::place(
        &e.ctx,
        &local,
        NewWidget {
            position: Some(-1),
            ..new("popular_posts", "sidebar", "home", Value::Null)
        },
    )
    .await
    .unwrap();
    let boards_w = widgets::place(
        &e.ctx,
        &local,
        new("latest_posts", "main", "boards", Value::Null),
    )
    .await
    .unwrap();
    let free = widgets::place(
        &e.ctx,
        &local,
        new(
            "markdown_block",
            "main",
            "board:free",
            json!({ "body": "자유게시판 안내" }),
        ),
    )
    .await
    .unwrap();
    let about = widgets::place(
        &e.ctx,
        &local,
        new(
            "markdown_block",
            "main",
            "page:about",
            json!({ "body": "소개" }),
        ),
    )
    .await
    .unwrap();
    let off = widgets::place(
        &e.ctx,
        &local,
        NewWidget {
            enabled: false,
            ..new("search", "footer", "all", Value::Null)
        },
    )
    .await
    .unwrap();
    let ids =
        |list: Vec<maru_core::model::Widget>| list.into_iter().map(|w| w.id).collect::<Vec<_>>();
    // 영역 → 순서: main 이 sidebar 보다 앞, sidebar 안에서는 position -1 이 먼저
    assert_eq!(
        ids(widgets::for_screen(&e.ctx, Screen::Home).await.unwrap()),
        vec![home.id.clone(), all.id.clone()]
    );
    assert_eq!(
        ids(widgets::for_screen(&e.ctx, Screen::Board("free"))
            .await
            .unwrap()),
        vec![boards_w.id.clone(), free.id.clone(), all.id.clone()]
    );
    assert_eq!(
        ids(widgets::for_screen(&e.ctx, Screen::Board("notice"))
            .await
            .unwrap()),
        vec![boards_w.id.clone(), all.id.clone()]
    );
    assert_eq!(
        ids(widgets::for_screen(&e.ctx, Screen::Page("about"))
            .await
            .unwrap()),
        vec![about.id.clone(), all.id.clone()]
    );
    assert_eq!(
        ids(widgets::for_screen(&e.ctx, Screen::Other).await.unwrap()),
        vec![all.id.clone()]
    );
    assert!(!ids(widgets::for_screen(&e.ctx, Screen::Other).await.unwrap()).contains(&off.id));
}

#[tokio::test]
async fn popular_posts_order_by_views_then_recency_and_skip_secret() {
    let e = env().await;
    let local = Actor::local();
    let a = published(&e.ctx, "free", "가", false).await;
    let b = published(&e.ctx, "free", "나", false).await;
    let c = published(&e.ctx, "notice", "다", false).await;
    let s = published(&e.ctx, "free", "비밀", true).await;
    views::set(&e.ctx, &local, &a.id, 5).await.unwrap();
    views::set(&e.ctx, &local, &b.id, 50).await.unwrap();
    views::set(&e.ctx, &local, &s.id, 500).await.unwrap();
    let guest = maru_core::services::anonymous_actor(&e.ctx).await.unwrap();
    let titles = |list: Vec<Post>| list.into_iter().map(|p| p.title).collect::<Vec<_>>();
    assert_eq!(
        titles(
            posts::public_popular(&e.ctx, &guest, None, 7, 10)
                .await
                .unwrap()
        ),
        vec!["나", "가", "다"]
    );
    let free = boards::readable(&e.ctx, &guest, "free").await.unwrap();
    assert_eq!(
        titles(
            posts::public_popular(&e.ctx, &guest, Some(&free), 0, 1)
                .await
                .unwrap()
        ),
        vec!["나"]
    );
    // 기간 밖(발행 시각을 8일 전으로)
    sqlx::query("UPDATE posts SET published_at = '2020-01-01T00:00:00.000Z' WHERE id = ?")
        .bind(&b.id)
        .execute(e.db.write_pool())
        .await
        .unwrap();
    assert_eq!(
        titles(
            posts::public_popular(&e.ctx, &guest, None, 7, 10)
                .await
                .unwrap()
        ),
        vec!["가", "다"]
    );
    assert_eq!(
        titles(
            posts::public_popular(&e.ctx, &guest, None, 0, 10)
                .await
                .unwrap()
        ),
        vec!["나", "가", "다"]
    );
    let _ = c;
}

#[tokio::test]
async fn popups_pair_with_their_region_and_follow_their_period() {
    let e = env().await;
    let local = Actor::local();
    for (kind, region) in [("popup", "sidebar"), ("search", "popup")] {
        let err = widgets::place(
            &e.ctx,
            &local,
            new(kind, region, "all", json!({ "body": "x" })),
        )
        .await
        .unwrap_err();
        assert_eq!(err.code(), Code::Validation, "{kind}·{region}");
    }
    for bad in [
        json!({ "body": "" }),
        json!({ "body": "x", "starts_at": "내일" }),
        json!({ "body": "x", "starts_at": "2030-01-02T00:00:00Z", "ends_at": "2030-01-01T00:00:00Z" }),
        json!({ "body": "x", "dismiss_days": 31 }),
        json!({ "body": "x", "position": "top" }),
        json!({ "body": "x", "image": "https://evil.example/a.png" }),
    ] {
        let err = widgets::place(&e.ctx, &local, new("popup", "popup", "all", bad.clone()))
            .await
            .unwrap_err();
        assert_eq!(err.code(), Code::Validation, "{bad}");
    }
    let now = widgets::place(
        &e.ctx,
        &local,
        new(
            "popup",
            "popup",
            "home",
            json!({ "body": "지금", "starts_at": "2020-01-01T09:00:00+09:00", "ends_at": "2099-01-01T00:00:00Z" }),
        ),
    )
    .await
    .unwrap();
    // 시각은 저장 형식(UTC)으로, 나머지는 기본값
    assert_eq!(now.config["starts_at"], "2020-01-01T00:00:00.000000Z");
    assert_eq!(now.config["dismiss_days"], 1);
    assert_eq!(now.config["position"], "center");
    let later = widgets::place(
        &e.ctx,
        &local,
        new(
            "popup",
            "popup",
            "home",
            json!({ "body": "나중", "starts_at": "2099-01-01T00:00:00Z" }),
        ),
    )
    .await
    .unwrap();
    let over = widgets::place(
        &e.ctx,
        &local,
        new(
            "popup",
            "popup",
            "home",
            json!({ "body": "끝남", "ends_at": "2020-01-01T00:00:00Z" }),
        ),
    )
    .await
    .unwrap();
    let shown: Vec<_> = widgets::for_screen(&e.ctx, Screen::Home)
        .await
        .unwrap()
        .into_iter()
        .map(|w| w.id)
        .collect();
    assert_eq!(shown, vec![now.id.clone()]);
    assert!(!shown.contains(&later.id) && !shown.contains(&over.id));
    // 팝업을 다른 영역으로 옮길 수 없다
    let err = widgets::configure(
        &e.ctx,
        &local,
        &now.id,
        WidgetPatch {
            region: Some("main".into()),
            ..Default::default()
        },
    )
    .await
    .unwrap_err();
    assert_eq!(err.code(), Code::Validation);
}
