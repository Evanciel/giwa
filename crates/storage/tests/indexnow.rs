//! IndexNow (ADR-0143). 가짜 보내는 곳으로 주소 모으기·중복 빼기·비공개 빼기·실패 뒤 기다리기를 본다.
#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::sync::{Arc, Mutex};

use maru_core::Ctx;
use maru_core::actor::Actor;
use maru_core::services::boards::{self, NewBoard};
use maru_core::services::indexnow::{self, IndexNowSender, SendReport};
use maru_core::services::pages::{self, NewPage};
use maru_core::services::posts::{self, NewPost};
use maru_core::services::settings::{self, SettingsPatch};
use maru_storage::{DataDir, Db, FsBlobs, SqliteStore};
use serde_json::{Value, json};

const SITE: &str = "https://maru.site.co.kr";

struct Env {
    _tmp: tempfile::TempDir,
    _db: Db,
    ctx: Ctx,
}

async fn env(site_url: &str) -> Env {
    let tmp = tempfile::tempdir().unwrap();
    let dir = DataDir::new(tmp.path().join("데이터"));
    dir.ensure_layout().unwrap();
    let db = Db::open(&dir.db_path(), true).await.unwrap();
    db.migrate().await.unwrap();
    let ctx = Ctx {
        store: Arc::new(SqliteStore::new(db.clone())),
        blobs: Arc::new(FsBlobs::new(dir.media_dir())),
        site_url: site_url.into(),
        extensions: maru_core::store::no_extensions(),
    };
    Env {
        _tmp: tmp,
        _db: db,
        ctx,
    }
}

/// 받은 본문을 모으고 정해 둔 응답 코드를 돌려준다.
struct Fake {
    sent: Mutex<Vec<(String, Value)>>,
    reply: Mutex<Result<u16, String>>,
}

impl Fake {
    fn new() -> Self {
        Self {
            sent: Mutex::new(Vec::new()),
            reply: Mutex::new(Ok(200)),
        }
    }
    fn bodies(&self) -> Vec<Value> {
        self.sent
            .lock()
            .unwrap()
            .iter()
            .map(|(_, b)| b.clone())
            .collect()
    }
}

#[async_trait::async_trait]
impl IndexNowSender for Fake {
    async fn submit(&self, endpoint: &str, body: Vec<u8>) -> Result<u16, String> {
        let v: Value = serde_json::from_slice(&body).unwrap();
        self.sent.lock().unwrap().push((endpoint.to_owned(), v));
        self.reply.lock().unwrap().clone()
    }
}

async fn board(ctx: &Ctx, slug: &str, policy: Option<Value>) {
    boards::create(
        ctx,
        &Actor::local(),
        NewBoard {
            slug: slug.into(),
            name: slug.into(),
            kind: "free".into(),
            policy: policy.map(|p| serde_json::from_value(p).unwrap()),
            sort_order: 0,
            layout: None,
            field_schema: None,
            list_order: None,
        },
    )
    .await
    .unwrap();
}

async fn publish(ctx: &Ctx, board: &str, slug: &str, secret: bool) -> String {
    let local = Actor::local();
    let p = posts::create_draft(
        ctx,
        &local,
        NewPost {
            board: board.into(),
            title: format!("글 {slug}"),
            slug: Some(slug.into()),
            body_md: "본문".into(),
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
    posts::publish(ctx, &local, &p.id).await.unwrap();
    p.id
}

#[tokio::test]
async fn changed_public_addresses_are_sent_once_with_the_key() {
    let e = env(SITE).await;
    let fake = Fake::new();
    let mut scanned = 0;
    board(&e.ctx, "free", None).await;
    board(
        &e.ctx,
        "staff",
        Some(json!({ "list": "member", "read": "member" })),
    )
    .await;
    let a = publish(&e.ctx, "free", "a", false).await;
    publish(&e.ctx, "free", "b", false).await;
    publish(&e.ctx, "free", "hidden", true).await;
    publish(&e.ctx, "staff", "inside", false).await;
    pages::create(
        &e.ctx,
        &Actor::local(),
        NewPage {
            slug: "about".into(),
            title: "소개".into(),
            body_md: "소개".into(),
            layout: "default".into(),
            sort_order: 0,
            parent: None,
        },
    )
    .await
    .unwrap();
    pages::publish(&e.ctx, &Actor::local(), "about")
        .await
        .unwrap();

    let r = indexnow::send_pending(&e.ctx, &fake, indexnow::ENDPOINT, &mut scanned)
        .await
        .unwrap();
    assert_eq!(
        r,
        SendReport::Sent {
            urls: 3,
            status: 200
        }
    );
    let sent = fake.sent.lock().unwrap().clone();
    assert_eq!(sent[0].0, "https://searchadvisor.naver.com/indexnow");
    let body = &sent[0].1;
    assert_eq!(body["host"], "maru.site.co.kr");
    let key = body["key"].as_str().unwrap().to_owned();
    assert_eq!(key.len(), 32);
    assert!(key.chars().all(|c| c.is_ascii_hexdigit()));
    assert_eq!(body["keyLocation"], format!("{SITE}/{key}.txt"));
    let urls: Vec<&str> = body["urlList"]
        .as_array()
        .unwrap()
        .iter()
        .map(|u| u.as_str().unwrap())
        .collect();
    assert_eq!(
        urls,
        [
            format!("{SITE}/b/free/a"),
            format!("{SITE}/b/free/b"),
            format!("{SITE}/p/about"),
        ],
        "비밀글·회원 게시판은 빠지고 같은 주소는 한 번"
    );
    assert_eq!(
        indexnow::key(&e.ctx).await.unwrap().as_deref(),
        Some(key.as_str())
    );

    // 새 동작이 없으면 다시 보내지 않는다(보낸 기록 자체는 알릴 동작이 아니다).
    let again = indexnow::send_pending(&e.ctx, &fake, indexnow::ENDPOINT, &mut scanned)
        .await
        .unwrap();
    assert_eq!(again, SendReport::Idle);
    assert_eq!(fake.bodies().len(), 1);

    // 고친 글만 다음에 보낸다. 같은 키를 쓴다.
    posts::update(
        &e.ctx,
        &Actor::local(),
        &a,
        serde_json::from_value(json!({ "title": "고친 글" })).unwrap(),
    )
    .await
    .unwrap();
    let r = indexnow::send_pending(&e.ctx, &fake, indexnow::ENDPOINT, &mut scanned)
        .await
        .unwrap();
    assert_eq!(
        r,
        SendReport::Sent {
            urls: 1,
            status: 200
        }
    );
    let last = fake.bodies().pop().unwrap();
    assert_eq!(last["urlList"], json!([format!("{SITE}/b/free/a")]));
    assert_eq!(last["key"], key.as_str());

    let s = indexnow::status(&e.ctx, &Actor::local()).await.unwrap();
    assert!(s.enabled && s.public_site);
    assert_eq!(s.sent_total, 4);
    assert_eq!(s.last_status, Some(200));
    assert_eq!(s.key_url, Some(format!("{SITE}/{key}.txt")));
}

#[tokio::test]
async fn failures_wait_and_keep_the_addresses() {
    let e = env(SITE).await;
    let fake = Fake::new();
    *fake.reply.lock().unwrap() = Ok(422);
    let mut scanned = 0;
    board(&e.ctx, "free", None).await;
    publish(&e.ctx, "free", "a", false).await;
    let r = indexnow::send_pending(&e.ctx, &fake, indexnow::ENDPOINT, &mut scanned)
        .await
        .unwrap();
    assert!(matches!(r, SendReport::Failed { urls: 1, .. }), "{r:?}");
    let s = indexnow::status(&e.ctx, &Actor::local()).await.unwrap();
    assert_eq!(s.last_status, Some(422));
    assert!(s.retry_at.is_some() && s.last_error.is_some());
    // 다시 보낼 때까지 기다리고, 읽은 자리를 넘기지 않았으니 그때 같은 주소를 다시 보낸다.
    let r = indexnow::send_pending(&e.ctx, &fake, indexnow::ENDPOINT, &mut scanned)
        .await
        .unwrap();
    assert_eq!(r, SendReport::Waiting);
    assert_eq!(fake.bodies().len(), 1);
}

#[tokio::test]
async fn local_sites_and_switched_off_sites_send_nothing() {
    for url in [
        "http://127.0.0.1:8080",
        "https://localhost",
        "https://shop.test",
    ] {
        let e = env(url).await;
        let fake = Fake::new();
        board(&e.ctx, "free", None).await;
        publish(&e.ctx, "free", "a", false).await;
        let r = indexnow::send_pending(&e.ctx, &fake, indexnow::ENDPOINT, &mut 0)
            .await
            .unwrap();
        assert_eq!(r, SendReport::Off, "{url}");
        assert!(fake.bodies().is_empty());
    }
    let e = env(SITE).await;
    let fake = Fake::new();
    settings::update(
        &e.ctx,
        &Actor::local(),
        SettingsPatch {
            indexnow: Some(false),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    board(&e.ctx, "free", None).await;
    publish(&e.ctx, "free", "a", false).await;
    let r = indexnow::send_pending(&e.ctx, &fake, indexnow::ENDPOINT, &mut 0)
        .await
        .unwrap();
    assert_eq!(r, SendReport::Off);
    assert!(fake.bodies().is_empty());
    assert_eq!(indexnow::key(&e.ctx).await.unwrap(), None);
}
