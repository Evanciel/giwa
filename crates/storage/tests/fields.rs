//! 여분 필드 (GAP C10, ADR-0057). 실제 SQLite 파일로 칸 정의 저장·글 값 검증·이동·공개 쓰기·비밀글 가림을 검증한다.
#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::sync::Arc;

use giwa_core::actor::{Actor, Permission, rank};
use giwa_core::fields::{FieldDef, FieldOption};
use giwa_core::model::PageReq;
use giwa_core::services::board_posts::{self, BoardWrite};
use giwa_core::services::boards::{self, BoardPatch, NewBoard};
use giwa_core::services::members::{self, SignupInput};
use giwa_core::services::posts::{self, CopyInput, MoveInput, NewPost, PostPatch};
use giwa_core::services::settings::{self, SettingsPatch};
use giwa_core::{Code, Ctx};
use giwa_storage::{DataDir, Db, FsBlobs, SqliteStore};
use serde_json::{Map, Value, json};

struct Env {
    _tmp: tempfile::TempDir,
    db: Db,
    ctx: Ctx,
}

fn def(key: &str, label: &str, kind: &str, required: bool, options: &[&str]) -> FieldDef {
    FieldDef {
        key: key.into(),
        label: label.into(),
        kind: kind.into(),
        required,
        options: options
            .iter()
            .map(|o| FieldOption {
                value: (*o).into(),
                label: String::new(),
            })
            .collect(),
        help: None,
        list: false,
        filter: false,
        role: None,
    }
}

fn market() -> Vec<FieldDef> {
    vec![
        def("price", "가격", "number", true, &[]),
        def("area", "지역", "select", false, &["seoul", "busan"]),
    ]
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
    for (slug, schema) in [("market", Some(market())), ("free", None)] {
        boards::create(
            &ctx,
            &local,
            NewBoard {
                slug: slug.into(),
                name: slug.into(),
                kind: "free".into(),
                policy: None,
                sort_order: 0,
                layout: None,
                field_schema: schema,
                list_order: None,
            },
        )
        .await
        .unwrap();
    }
    Env { _tmp: tmp, db, ctx }
}

fn post(board: &str, meta: Value) -> NewPost {
    NewPost {
        board: board.into(),
        title: "자전거 팝니다".into(),
        slug: None,
        body_md: "거의 새것".into(),
        excerpt: None,
        cover: None,
        category: None,
        tags: Vec::new(),
        notice: false,
        secret: false,
        meta: meta.as_object().cloned().unwrap_or_default(),
    }
}

async fn stored_meta(db: &Db, id: &str) -> Value {
    let raw: String = sqlx::query_scalar("SELECT meta FROM posts WHERE id = ?")
        .bind(id)
        .fetch_one(db.read_pool())
        .await
        .unwrap();
    serde_json::from_str(&raw).unwrap()
}

#[tokio::test]
async fn board_schema_is_checked_and_saved() {
    let e = env().await;
    let local = Actor::local();
    let b = boards::show(&e.ctx, &local, "market").await.unwrap();
    assert_eq!(b.field_schema, market());
    // 잘못된 정의는 고치지 않는다.
    for bad in [
        vec![def("Price", "가격", "number", false, &[])],
        vec![def("photo", "사진", "media", false, &[])],
        vec![def("area", "지역", "select", false, &[])],
        vec![
            def("a", "가", "text", false, &[]),
            def("a", "나", "text", false, &[]),
        ],
    ] {
        let err = boards::update(
            &e.ctx,
            &local,
            "market",
            BoardPatch {
                field_schema: Some(bad),
                list_order: None,
                ..Default::default()
            },
        )
        .await
        .unwrap_err();
        assert_eq!(err.code(), Code::Validation);
    }
    assert_eq!(
        boards::show(&e.ctx, &local, "market")
            .await
            .unwrap()
            .field_schema,
        market()
    );
}

#[tokio::test]
async fn post_values_are_validated_and_stored() {
    let e = env().await;
    let local = Actor::local();
    let p = posts::create_draft(
        &e.ctx,
        &local,
        post("market", json!({"price": "120,000", "area": "busan"})),
    )
    .await
    .unwrap();
    assert_eq!(
        stored_meta(&e.db, &p.id).await,
        json!({"price": 120000, "area": "busan"})
    );
    // 필수 칸, 모르는 칸, 선택지 밖 값은 거부
    for (meta, msg) in [
        (json!({"area": "seoul"}), "가격 칸을 채워 주세요."),
        (
            json!({"price": 1, "color": "red"}),
            "이 게시판에 없는 칸입니다: color",
        ),
        (
            json!({"price": 1, "area": "jeju"}),
            "지역 칸 값이 올바르지 않습니다 (선택지에 없는 값).",
        ),
    ] {
        let err = posts::create_draft(&e.ctx, &local, post("market", meta))
            .await
            .unwrap_err();
        assert_eq!(err.code(), Code::Validation);
        assert_eq!(err.message(), msg);
    }
    // 칸이 없는 게시판에 값을 보내면 거부
    let err = posts::create_draft(&e.ctx, &local, post("free", json!({"price": 1})))
        .await
        .unwrap_err();
    assert_eq!(err.code(), Code::Validation);
    // 고치기: 통째로 바꾸고, 안 보내면 그대로
    let p = posts::update(
        &e.ctx,
        &local,
        &p.id,
        PostPatch {
            meta: Some(json!({"price": 99000}).as_object().cloned().unwrap()),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    assert_eq!(stored_meta(&e.db, &p.id).await, json!({"price": 99000}));
    posts::update(
        &e.ctx,
        &local,
        &p.id,
        PostPatch {
            title: Some("자전거 팝니다 (가격 내림)".into()),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    assert_eq!(stored_meta(&e.db, &p.id).await, json!({"price": 99000}));
    // 감사 기록 diff 에 값이 남는다.
    let n: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM audit_log WHERE action = 'post.update' AND diff LIKE '%99000%'",
    )
    .fetch_one(e.db.read_pool())
    .await
    .unwrap();
    assert!(n >= 1);
}

#[tokio::test]
async fn moving_or_copying_keeps_only_fields_of_the_new_board() {
    let e = env().await;
    let local = Actor::local();
    let p = posts::create_draft(&e.ctx, &local, post("market", json!({"price": 5000})))
        .await
        .unwrap();
    let copy = posts::copy_to(
        &e.ctx,
        &local,
        &p.id,
        CopyInput {
            board: "free".into(),
        },
    )
    .await
    .unwrap();
    assert_eq!(stored_meta(&e.db, &copy.id).await, json!({}));
    let moved = posts::move_to(
        &e.ctx,
        &local,
        &p.id,
        MoveInput {
            board: "free".into(),
            category: None,
        },
    )
    .await
    .unwrap();
    assert!(moved.meta.is_empty());
    // 정의를 바꿔도 이미 쓴 값은 그대로 두고, 표시는 지금 정의에 있는 키만
    let q = posts::create_draft(
        &e.ctx,
        &local,
        post("market", json!({"price": 7, "area": "seoul"})),
    )
    .await
    .unwrap();
    boards::update(
        &e.ctx,
        &local,
        "market",
        BoardPatch {
            field_schema: Some(vec![def("price", "가격", "number", true, &[])]),
            list_order: None,
            ..Default::default()
        },
    )
    .await
    .unwrap();
    assert_eq!(
        stored_meta(&e.db, &q.id).await,
        json!({"price": 7, "area": "seoul"})
    );
    let b = boards::show(&e.ctx, &local, "market").await.unwrap();
    let shown = giwa_core::fields::display(
        &b.field_schema,
        &e.ctx.store.post_by_id(&q.id).await.unwrap().unwrap().meta,
    );
    assert_eq!(shown.len(), 1);
    assert_eq!(shown[0].text, "7");
}

async fn member(ctx: &Ctx, email: &str) -> Actor {
    settings::update(
        ctx,
        &Actor::local(),
        SettingsPatch {
            signup: Some("open".into()),
            signup_verify: Some(false),
            captcha: Some("off".into()),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    members::signup(
        ctx,
        SignupInput {
            email: email.into(),
            display_name: email.split('@').next().unwrap_or(email).into(),
            password: "test-password-1".into(),
            captcha: None,
        },
    )
    .await
    .unwrap();
    let m = ctx.store.member_by_email(email).await.unwrap().unwrap();
    Actor::human(
        m.id,
        [
            Permission::SiteView,
            Permission::BoardList,
            Permission::PostRead,
            Permission::PostWrite,
        ],
        rank::MEMBER,
    )
}

#[tokio::test]
async fn public_writes_use_the_same_rules_and_secret_hides_values() {
    let e = env().await;
    let m = member(&e.ctx, "seller@example.test").await;
    let write = |meta: Value, secret: bool| BoardWrite {
        board: "market".into(),
        title: "책상 팝니다".into(),
        body_md: "튼튼합니다".into(),
        secret,
        meta: meta.as_object().cloned().unwrap_or_default(),
        ..Default::default()
    };
    let err = board_posts::write(&e.ctx, &m, write(json!({}), false))
        .await
        .unwrap_err();
    assert_eq!(err.message(), "가격 칸을 채워 주세요.");
    let p = board_posts::write(&e.ctx, &m, write(json!({"price": "30000"}), true))
        .await
        .unwrap();
    assert_eq!(stored_meta(&e.db, &p.id).await, json!({"price": 30000}));
    // 비밀글은 목록에서 값도 비운다.
    let guest = giwa_core::services::anonymous_actor(&e.ctx).await.unwrap();
    let b = boards::show(&e.ctx, &Actor::local(), "market")
        .await
        .unwrap();
    let page = posts::public_board(&e.ctx, &guest, &b, None, PageReq::new(1, 20))
        .await
        .unwrap();
    let listed = page.posts.items.iter().find(|x| x.id == p.id).unwrap();
    assert_eq!(listed.meta, Map::new());
}
