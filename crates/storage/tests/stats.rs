//! 방문자 통계 (GAP S3, ADR-0061). 실제 SQLite 파일로 더하기·누적·보존 정리·요약·권한을 검증한다.
#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::sync::Arc;

use giwa_core::actor::{Actor, Permission, rank};
use giwa_core::model::{StatBatch, StatDay, StatPage, StatReferrer};
use giwa_core::services::boards::{self, NewBoard};
use giwa_core::services::posts::{self, NewPost};
use giwa_core::services::settings::{self, SettingsPatch};
use giwa_core::services::stats::{self, Hit, StatCounter, StatsQuery};
use giwa_core::{Code, Ctx};
use giwa_storage::{DataDir, Db, FsBlobs, SqliteStore};

const UA: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) Chrome/130.0";
const OWN: [&str; 1] = ["127.0.0.1:8080"];

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
        extensions: giwa_core::store::no_extensions(),
    };
    Env { _tmp: tmp, db, ctx }
}

fn hit<'a>(path: &'a str, referrer: &'a str, ip: &'a str) -> Hit<'a> {
    Hit {
        path,
        referrer,
        own_hosts: &OWN,
        ip,
        user_agent: UA,
    }
}

async fn published(ctx: &Ctx, title: &str) -> String {
    let local = Actor::local();
    boards::create(
        ctx,
        &local,
        NewBoard {
            slug: "free".into(),
            name: "자유게시판".into(),
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
            title: title.into(),
            slug: Some("hello".into()),
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
    posts::publish(ctx, &local, &p.id).await.unwrap().slug
}

#[tokio::test]
async fn visits_reach_the_db_accumulate_and_summarize_with_titles() {
    let e = env().await;
    let slug = published(&e.ctx, "안녕하세요").await;
    let post_path = format!("/b/free/{slug}?from=feed");
    let c = StatCounter::new();
    c.record(&hit("/", "", "10.0.0.1"));
    c.record(&hit(
        &post_path,
        "https://search.naver.com/x?q=1",
        "10.0.0.1",
    ));
    c.record(&hit(&post_path, "https://search.naver.com/", "10.0.0.2"));
    c.record(&hit("/b/free", "http://127.0.0.1:8080/", "10.0.0.2"));
    assert_eq!(stats::flush(&e.ctx, &c).await.unwrap(), 1, "오늘 하루");
    // 다음 반영은 누적된다.
    c.record(&hit("/", "https://www.google.com/", "10.0.0.3"));
    stats::flush(&e.ctx, &c).await.unwrap();
    assert_eq!(
        stats::flush(&e.ctx, &c).await.unwrap(),
        0,
        "모은 것이 없으면 0"
    );

    let s = stats::summary(&e.ctx, &Actor::local(), &StatsQuery { days: Some(7) })
        .await
        .unwrap();
    assert_eq!(s.daily.len(), 7, "없는 날은 0 으로 채운다");
    assert_eq!(s.daily.last().unwrap().day, s.to);
    assert_eq!((s.views, s.visitors), (5, 3));
    // 같은 수면 경로 순 (조회 → 방문자 → 경로).
    assert_eq!(s.pages[0].path, "/");
    let top = &s.pages[1];
    assert_eq!(top.path, "/b/free/hello", "쿼리 문자열은 남기지 않는다");
    assert_eq!((top.views, top.visitors), (2, 2));
    assert_eq!(top.title, "안녕하세요");
    let home = s.pages.iter().find(|p| p.path == "/").unwrap();
    assert_eq!((home.title.as_str(), home.views), ("첫 화면", 2));
    let board = s.pages.iter().find(|p| p.path == "/b/free").unwrap();
    assert_eq!(board.title, "자유게시판");
    assert_eq!(
        s.referrers,
        vec![
            StatReferrer {
                host: "search.naver.com".into(),
                visits: 2
            },
            StatReferrer {
                host: "www.google.com".into(),
                visits: 1
            },
        ],
        "내부 이동은 들어온 곳이 아니다"
    );

    // IP·해시·전체 Referer 는 어느 표에도 없다.
    for (table, cols) in [
        ("stat_days", "day || views || visitors"),
        ("stat_pages", "day || path || views || visitors"),
        ("stat_referrers", "day || host || visits"),
    ] {
        let rows: Vec<String> =
            sqlx::query_scalar(sqlx::AssertSqlSafe(format!("SELECT {cols} FROM {table}")))
                .fetch_all(e.db.read_pool())
                .await
                .unwrap();
        assert!(
            rows.iter()
                .all(|r| !r.contains("10.0.0") && !r.contains("q=1")),
            "{table}: {rows:?}"
        );
    }
}

#[tokio::test]
async fn old_days_are_purged_by_the_retention_setting() {
    let e = env().await;
    let local = Actor::local();
    let old = StatBatch {
        days: vec![StatDay {
            day: "2000-01-01".into(),
            views: 9,
            visitors: 9,
        }],
        pages: vec![(
            "2000-01-01".into(),
            StatPage {
                path: "/".into(),
                title: String::new(),
                views: 9,
                visitors: 9,
            },
        )],
        referrers: vec![(
            "2000-01-01".into(),
            StatReferrer {
                host: "a.kr".into(),
                visits: 9,
            },
        )],
    };
    e.ctx.store.add_stats(&old).await.unwrap();
    assert_eq!(
        e.ctx
            .store
            .stat_days("1999-01-01", "2000-12-31")
            .await
            .unwrap()
            .len(),
        1
    );

    for bad in [29, 3651] {
        let err = settings::update(
            &e.ctx,
            &local,
            SettingsPatch {
                stats_retention_days: Some(bad),
                ..Default::default()
            },
        )
        .await
        .unwrap_err();
        assert_eq!(err.code(), Code::Validation, "{bad}");
    }
    let s = settings::update(
        &e.ctx,
        &local,
        SettingsPatch {
            stats_retention_days: Some(3650),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    assert_eq!(s.stats_retention_days, 3650);

    // 3650일이면 2000년은 지나 지워진다. 정리는 하루 한 번(새 계수기의 첫 반영).
    stats::flush(&e.ctx, &StatCounter::new()).await.unwrap();
    let store = &e.ctx.store;
    assert!(
        store
            .stat_days("1999-01-01", "2000-12-31")
            .await
            .unwrap()
            .is_empty()
    );
    assert!(
        store
            .stat_pages("1999-01-01", "2000-12-31", 10)
            .await
            .unwrap()
            .is_empty()
    );
    assert!(
        store
            .stat_referrers("1999-01-01", "2000-12-31", 10)
            .await
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn only_publishers_read_stats_and_ranges_are_checked() {
    let e = env().await;
    let q = StatsQuery { days: Some(30) };
    let guest = giwa_core::services::anonymous_actor(&e.ctx).await.unwrap();
    let err = stats::summary(&e.ctx, &guest, &q).await.unwrap_err();
    assert_eq!(err.code(), Code::Unauthorized);
    let member = Actor::human(
        "m1".into(),
        [Permission::PostRead, Permission::PostWrite],
        rank::MEMBER,
    );
    let err = stats::summary(&e.ctx, &member, &q).await.unwrap_err();
    assert_eq!(err.code(), Code::Forbidden);
    let editor = Actor::human("e1".into(), [Permission::PostPublish], rank::EDITOR);
    let s = stats::summary(&e.ctx, &editor, &StatsQuery { days: None })
        .await
        .unwrap();
    assert_eq!(s.daily.len(), 30, "기본 30일");
    assert_eq!(s.views, 0);
    for bad in [0, 367] {
        let err = stats::summary(&e.ctx, &editor, &StatsQuery { days: Some(bad) })
            .await
            .unwrap_err();
        assert_eq!(err.code(), Code::Validation, "{bad}");
    }
}
