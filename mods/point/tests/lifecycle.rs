//! 모듈 생명주기와 이벤트 배달 (ADR-0066, 0067). 켜고 끄고 지워도 코어 데이터가 그대로인지,
//! 배달이 멱등하고 실패를 보관하는지 실제 SQLite 파일로 본다.
#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use async_trait::async_trait;
use maru_core::actor::{Actor, Permission, rank};
use maru_core::model::AuditEvent;
use maru_core::services::auth;
use maru_core::services::boards::{self, NewBoard};
use maru_core::services::members::{self, SignupInput};
use maru_core::services::posts::{self, NewPost};
use maru_core::services::settings::{self, SettingsPatch};
use maru_core::{Code, Ctx, Error, Result};
use maru_mod_point::PointModule;
use maru_modules::delivery::{MAX_ATTEMPTS, deliver_once};
use maru_modules::{Manifest, Module, Registry, service};
use maru_storage::{DataDir, Db, FsBlobs, SqliteStore};
use serde_json::{Map, Value, json};

const PASSWORD: &str = "test-pass-1234";

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
    settings::update(
        &ctx,
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
    boards::create(
        &ctx,
        &Actor::local(),
        NewBoard {
            slug: "blog".into(),
            name: "블로그".into(),
            kind: "blog".into(),
            policy: None,
            sort_order: 0,
            layout: None,
            field_schema: None,
            list_order: None,
        },
    )
    .await
    .unwrap();
    Env { _tmp: tmp, db, ctx }
}

fn point(db: &Db) -> Registry {
    Registry::new(vec![Arc::new(PointModule::new(db.clone()))]).unwrap()
}

/// 코어 표마다 줄 수. 모듈이 코어를 건드리지 않았는지 비교한다.
async fn core_counts(db: &Db) -> Vec<(String, i64)> {
    let tables: Vec<String> = sqlx::query_scalar(
        "SELECT name FROM sqlite_master WHERE type = 'table'
         AND name NOT LIKE 'sqlite_%' AND name NOT LIKE '_sqlx%' AND name NOT LIKE 'mod%' AND name <> 'audit_log'
         AND name NOT LIKE '%_fts%' ORDER BY name",
    )
    .fetch_all(db.read_pool())
    .await
    .unwrap();
    let mut out = Vec::new();
    for t in tables {
        let n: i64 =
            sqlx::query_scalar(sqlx::AssertSqlSafe(format!("SELECT COUNT(*) FROM \"{t}\"")))
                .fetch_one(db.read_pool())
                .await
                .unwrap();
        out.push((t, n));
    }
    out
}

async fn publish(ctx: &Ctx, actor: &Actor, title: &str) -> String {
    let p = posts::create_draft(
        ctx,
        actor,
        serde_json::from_value::<NewPost>(json!({
            "board": "blog",
            "title": title,
            "body_md": "본문",
        }))
        .unwrap(),
    )
    .await
    .unwrap();
    posts::publish(ctx, actor, &p.id).await.unwrap();
    p.id
}

async fn seed_member(ctx: &Ctx, email: &str) -> String {
    let name = email.split('@').next().unwrap();
    members::signup(
        ctx,
        SignupInput {
            email: email.into(),
            display_name: format!("이름{name}"),
            password: PASSWORD.into(),
            captcha: None,
        },
    )
    .await
    .unwrap();
    members::get(ctx, &Actor::local(), email)
        .await
        .unwrap()
        .member
        .id
}

fn writer(id: &str) -> Actor {
    Actor::human(
        id.to_owned(),
        [
            Permission::PostWrite,
            Permission::PostPublish,
            Permission::PostModerate,
        ],
        rank::EDITOR,
    )
}

async fn balance(ctx: &Ctx, reg: &Registry, member: &str) -> i64 {
    service::call_tool(
        ctx,
        &Actor::local(),
        reg,
        "point",
        "balance",
        json!({ "member": member }),
    )
    .await
    .unwrap()["balance"]
        .as_i64()
        .unwrap()
}

#[tokio::test]
async fn enable_disable_remove_leaves_core_data_alone() {
    let e = env().await;
    let reg = point(&e.db);
    let local = Actor::local();
    let member = seed_member(&e.ctx, "moon@example.test").await;
    let author = writer(&member);

    // 켜기 전의 글은 소급하지 않는다.
    publish(&e.ctx, &author, "켜기 전").await;
    let before = core_counts(&e.db).await;

    let info = service::enable(&e.ctx, &local, &reg, "point")
        .await
        .unwrap();
    assert!(info.enabled && info.installed);
    assert_eq!(info.settings["post"], 10);
    // 멱등
    assert!(
        service::enable(&e.ctx, &local, &reg, "point")
            .await
            .unwrap()
            .enabled
    );

    deliver_once(&e.ctx, &reg).await.unwrap();
    assert_eq!(balance(&e.ctx, &reg, &member).await, 0, "소급하지 않는다");

    // 켠 뒤의 글은 적립, 다시 배달해도 두 번 주지 않는다.
    let post = publish(&e.ctx, &author, "켠 뒤").await;
    assert!(deliver_once(&e.ctx, &reg).await.unwrap() >= 1);
    assert_eq!(balance(&e.ctx, &reg, &member).await, 10);
    assert_eq!(deliver_once(&e.ctx, &reg).await.unwrap(), 0);
    posts::hide(&e.ctx, &author, &post).await.unwrap();
    posts::publish(&e.ctx, &author, &post).await.unwrap();
    deliver_once(&e.ctx, &reg).await.unwrap();
    assert_eq!(
        balance(&e.ctx, &reg, &member).await,
        10,
        "같은 글로 두 번 안 준다"
    );

    // 로그인은 하루 한 번만 준다.
    auth::login_password(&e.ctx, "moon@example.test", PASSWORD)
        .await
        .unwrap();
    auth::login_password(&e.ctx, "moon@example.test", PASSWORD)
        .await
        .unwrap();
    deliver_once(&e.ctx, &reg).await.unwrap();
    assert_eq!(balance(&e.ctx, &reg, &member).await, 11);

    // 끄면 툴은 충돌, 그동안의 글은 적립 안 됨, 데이터는 남는다.
    service::disable(&e.ctx, &local, &reg, "point")
        .await
        .unwrap();
    let err = service::call_tool(&e.ctx, &local, &reg, "point", "ranking", json!({}))
        .await
        .unwrap_err();
    assert_eq!(err.code(), Code::Conflict);
    publish(&e.ctx, &author, "끈 동안").await;
    deliver_once(&e.ctx, &reg).await.unwrap();
    service::enable(&e.ctx, &local, &reg, "point")
        .await
        .unwrap();
    deliver_once(&e.ctx, &reg).await.unwrap();
    assert_eq!(
        balance(&e.ctx, &reg, &member).await,
        11,
        "꺼진 동안은 소급 안 함, 잔액 유지"
    );

    // 지워지면 준 만큼 되돌린다. 다시 배달해도 한 번만.
    posts::delete(&e.ctx, &author, &post).await.unwrap();
    deliver_once(&e.ctx, &reg).await.unwrap();
    assert_eq!(
        balance(&e.ctx, &reg, &member).await,
        1,
        "로그인 1점만 남는다"
    );

    // 제거: 표가 사라지고 코어 표는 그대로. 다시 지워도 안전, 다시 켜면 새로 시작.
    let after_posts = core_counts(&e.db).await;
    let removed = service::remove(&e.ctx, &local, &reg, "point")
        .await
        .unwrap();
    assert_eq!(removed.reverted.len(), 1);
    let left: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM sqlite_master WHERE name LIKE 'mod_point%'")
            .fetch_one(e.db.read_pool())
            .await
            .unwrap();
    assert_eq!(left, 0);
    let mut expect = after_posts.clone();
    for (t, n) in &mut expect {
        if t == "modules" || t == "module_migrations" {
            *n = 0;
        }
    }
    let mut now = core_counts(&e.db).await;
    now.sort();
    expect.sort();
    assert_eq!(now, expect, "코어 표의 줄 수가 그대로다");
    assert!(before.iter().any(|(t, _)| t == "posts"), "{before:?}");
    assert!(
        service::remove(&e.ctx, &local, &reg, "point")
            .await
            .unwrap()
            .reverted
            .is_empty()
    );
    service::enable(&e.ctx, &local, &reg, "point")
        .await
        .unwrap();
    assert_eq!(balance(&e.ctx, &reg, &member).await, 0);
}

#[tokio::test]
async fn settings_and_tools_check_input_and_permission() {
    let e = env().await;
    let reg = point(&e.db);
    let local = Actor::local();
    let member = seed_member(&e.ctx, "moon@example.test").await;
    service::enable(&e.ctx, &local, &reg, "point")
        .await
        .unwrap();

    let set = |pairs: &[(&str, &str)]| {
        let patch: Map<String, Value> = pairs
            .iter()
            .map(|(k, v)| ((*k).to_owned(), json!(v)))
            .collect();
        service::set_settings(&e.ctx, &local, &reg, "point", patch)
    };
    let ok = set(&[("post", "25")]).await.unwrap();
    assert_eq!(ok.settings["post"], 25, "폼의 글자도 숫자로 저장");
    assert_eq!(ok.settings["comment"], 2, "다른 칸은 그대로");
    for bad in [
        vec![("post", "-1")],
        vec![("post", "많이")],
        vec![("levels", "500,100")],
        vec![("nope", "1")],
    ] {
        let err = set(&bad).await.unwrap_err();
        assert_eq!(err.code(), Code::Validation, "{bad:?}");
    }

    publish(&e.ctx, &writer(&member), "글").await;
    deliver_once(&e.ctx, &reg).await.unwrap();
    assert_eq!(balance(&e.ctx, &reg, &member).await, 25);

    // 손으로 고치기: 회원 관리 권한, 사유 필수, 감사 기록.
    let plain = Actor::human(member.clone(), [], rank::MEMBER);
    let input = json!({ "member": "moon@example.test", "amount": 100, "reason": "행사 상품" });
    let err = service::call_tool(&e.ctx, &plain, &reg, "point", "adjust", input.clone())
        .await
        .unwrap_err();
    assert_eq!(err.code(), Code::Forbidden);
    let err = service::call_tool(
        &e.ctx,
        &local,
        &reg,
        "point",
        "adjust",
        json!({ "member": "moon@example.test", "amount": 5, "reason": " " }),
    )
    .await
    .unwrap_err();
    assert_eq!(err.code(), Code::Validation);
    let out = service::call_tool(&e.ctx, &local, &reg, "point", "adjust", input)
        .await
        .unwrap();
    assert_eq!(out["balance"], 125);
    let audited: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM audit_log WHERE action = 'point.adjust'")
            .fetch_one(e.db.read_pool())
            .await
            .unwrap();
    assert_eq!(audited, 1);

    // 레벨: 기본 기준 100 을 넘었다. 내 내역은 보이고 남의 것은 안 보인다.
    let me = service::call_tool(&e.ctx, &plain, &reg, "point", "balance", json!({}))
        .await
        .unwrap();
    assert_eq!(me["level"], 2);
    let mine = service::call_tool(&e.ctx, &plain, &reg, "point", "history", json!({}))
        .await
        .unwrap();
    assert_eq!(mine["items"].as_array().unwrap().len(), 2);
    let other = seed_member(&e.ctx, "sun@example.test").await;
    let err = service::call_tool(
        &e.ctx,
        &plain,
        &reg,
        "point",
        "history",
        json!({ "member": other }),
    )
    .await
    .unwrap_err();
    assert_eq!(err.code(), Code::Forbidden);

    // 모르는 칸·모르는 툴·모르는 모듈
    let err = service::call_tool(&e.ctx, &local, &reg, "point", "ranking", json!({ "x": 1 }))
        .await
        .unwrap_err();
    assert_eq!(err.code(), Code::Validation);
    let err = service::call_tool(&e.ctx, &local, &reg, "point", "nope", json!({}))
        .await
        .unwrap_err();
    assert_eq!(err.code(), Code::NotFound);
    let err = service::enable(&e.ctx, &local, &reg, "nope")
        .await
        .unwrap_err();
    assert_eq!(err.code(), Code::NotFound);

    // 관리 권한
    let err = service::enable(&e.ctx, &plain, &reg, "point")
        .await
        .unwrap_err();
    assert_eq!(err.code(), Code::Forbidden);
}

static FLAKY: Manifest = Manifest {
    id: "flaky",
    name: "늘 실패",
    version: "0.1.0",
    description: "시험용",
    events: &["post.publish"],
};

struct Flaky {
    broken: AtomicBool,
    calls: AtomicUsize,
}

#[async_trait]
impl Module for Flaky {
    fn manifest(&self) -> &'static Manifest {
        &FLAKY
    }

    async fn on_event(&self, _ctx: &Ctx, _ev: &AuditEvent) -> Result<()> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        if self.broken.load(Ordering::SeqCst) {
            return Err(Error::new(Code::External));
        }
        Ok(())
    }
}

/// 재시도 시각을 지금으로 당긴다 (기다리지 않으려고).
async fn due_now(db: &Db) {
    sqlx::query("UPDATE module_event_failures SET next_at = '2000-01-01T00:00:00Z'")
        .execute(db.write_pool())
        .await
        .unwrap();
}

#[tokio::test]
async fn failures_retry_then_give_up_and_do_not_block_others() {
    let e = env().await;
    let flaky = Arc::new(Flaky {
        broken: AtomicBool::new(true),
        calls: AtomicUsize::new(0),
    });
    let reg = Registry::new(vec![
        flaky.clone(),
        Arc::new(PointModule::new(e.db.clone())),
    ])
    .unwrap();
    let local = Actor::local();
    let member = seed_member(&e.ctx, "moon@example.test").await;
    service::enable(&e.ctx, &local, &reg, "flaky")
        .await
        .unwrap();
    service::enable(&e.ctx, &local, &reg, "point")
        .await
        .unwrap();

    publish(&e.ctx, &writer(&member), "글").await;
    deliver_once(&e.ctx, &reg).await.unwrap();
    assert_eq!(flaky.calls.load(Ordering::SeqCst), 1);
    assert_eq!(
        balance(&e.ctx, &reg, &member).await,
        10,
        "다른 모듈은 그대로 받는다"
    );
    let list = service::failures(&e.ctx, &local, &reg, Some("flaky"))
        .await
        .unwrap();
    assert_eq!(list.len(), 1);
    assert_eq!(list[0].attempts, 1);
    assert!(!list[0].gave_up);

    // 때가 안 됐으면 다시 부르지 않고, 커서는 넘어갔다.
    deliver_once(&e.ctx, &reg).await.unwrap();
    assert_eq!(flaky.calls.load(Ordering::SeqCst), 1);

    // 복구되면 지워진다.
    flaky.broken.store(false, Ordering::SeqCst);
    due_now(&e.db).await;
    deliver_once(&e.ctx, &reg).await.unwrap();
    assert!(
        service::failures(&e.ctx, &local, &reg, None)
            .await
            .unwrap()
            .is_empty()
    );

    // 끝내 안 되면 보관함에 남고 더 부르지 않는다.
    flaky.broken.store(true, Ordering::SeqCst);
    publish(&e.ctx, &writer(&member), "둘째").await;
    deliver_once(&e.ctx, &reg).await.unwrap();
    for _ in 1..MAX_ATTEMPTS {
        due_now(&e.db).await;
        deliver_once(&e.ctx, &reg).await.unwrap();
    }
    let list = service::failures(&e.ctx, &local, &reg, Some("flaky"))
        .await
        .unwrap();
    assert_eq!(list[0].attempts, MAX_ATTEMPTS);
    assert!(list[0].gave_up);
    let calls = flaky.calls.load(Ordering::SeqCst);
    due_now(&e.db).await;
    deliver_once(&e.ctx, &reg).await.unwrap();
    assert_eq!(flaky.calls.load(Ordering::SeqCst), calls);

    // 모듈을 지우면 그 기록도 사라진다.
    service::remove(&e.ctx, &local, &reg, "flaky")
        .await
        .unwrap();
    assert!(
        service::failures(&e.ctx, &local, &reg, None)
            .await
            .unwrap()
            .is_empty()
    );
}
