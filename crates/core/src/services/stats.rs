//! 방문자 통계 (GAP S3, ADR-0061).
//!
//! - 공개 화면(`render`) 한 번 = 조회 1. 쿠키도 외부 스크립트도 쓰지 않는다.
//! - 방문자 구분은 조회수(ADR-0049)와 같다: 그날(한국 시간)의 메모리 소금 + IP + User-Agent 의
//!   SHA-256. 해시도 IP 도 DB 에 남기지 않고, 날이 바뀌면 소금을 새로 만든다.
//! - 봇, `DNT: 1`·`Sec-GPC: 1`, 미리 읽기, 발행 권한이 있는 사람은 세지 않는다(웹 계층이 거른다).
//! - 경로는 쿼리 문자열을 지우고, 들어온 곳은 Referer 의 호스트만 남긴다.
//! - 모은 수는 [`flush`] 가 조회수와 같은 주기에 날짜별 표에 더하고, 보존 기간이 지난 날을 지운다.
//!   통계는 쓰기 작업이 아니라 감사하지 않는다.

use std::collections::{BTreeMap, HashSet, VecDeque};
use std::sync::{Mutex, MutexGuard};

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use time::{Date, Duration, OffsetDateTime};

use crate::actor::{Actor, Permission};
use crate::error::{Code, Error, Result};
use crate::model::{StatBatch, StatDay, StatPage, StatReferrer};
use crate::services::settings;
use crate::store::Ctx;

/// 하루에 기억하는 방문 표시 수. 넘으면 오래된 것부터 잊는다(그 사람을 한 번 더 센다).
pub const MAX_SEEN: usize = 200_000;
/// 저장하는 경로 길이 상한 (글자).
pub const MAX_PATH_CHARS: usize = 200;
/// 요약 기간 상한 (일).
pub const MAX_DAYS: u32 = 366;
/// 요약의 많이 본 화면·들어온 곳 개수.
pub const TOP: u32 = 20;
const DAY_OFFSET_SECS: i64 = 9 * 3600;

/// 공개 화면 한 번의 방문. 웹 계층이 요청에서 뽑아 넘긴다.
#[derive(Debug, Clone, Default)]
pub struct Hit<'a> {
    /// 요청 경로 (쿼리 포함이어도 된다)
    pub path: &'a str,
    /// `Referer` 머리 (없으면 빈 값)
    pub referrer: &'a str,
    /// 이 사이트로 보는 호스트들 (요청의 `Host`, `site_url` 의 호스트). Referer 가 이 중 하나면 내부 이동
    pub own_hosts: &'a [&'a str],
    pub ip: &'a str,
    pub user_agent: &'a str,
}

/// 들어온 곳.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Source {
    /// Referer 없음·읽을 수 없음
    Direct,
    /// 이 사이트 안에서 옮겨 옴 (세지 않는다)
    Internal,
    External(String),
}

/// 쿼리·조각을 지우고 끝 `/` 를 정리한 경로. `/` 로 시작하지 않으면 `None`.
pub fn normalize_path(raw: &str) -> Option<String> {
    let p = raw.split(['?', '#']).next().unwrap_or_default();
    if !p.starts_with('/') {
        return None;
    }
    let p = if p.len() > 1 {
        p.trim_end_matches('/')
    } else {
        p
    };
    let p = if p.is_empty() { "/" } else { p };
    Some(p.chars().take(MAX_PATH_CHARS).collect())
}

/// Referer 에서 호스트만. http(s) 가 아니거나 호스트 모양이 아니면 직접 들어온 것으로 본다.
pub fn source_of(referrer: &str, own_hosts: &[&str]) -> Source {
    let r = referrer.trim();
    let Some(rest) = r
        .strip_prefix("https://")
        .or_else(|| r.strip_prefix("http://"))
    else {
        return Source::Direct;
    };
    let authority = rest.split(['/', '?', '#']).next().unwrap_or_default();
    let authority = authority.rsplit('@').next().unwrap_or_default();
    let host = host_only(authority);
    let ok = !host.is_empty()
        && host.len() <= 253
        && host.chars().all(|c| {
            c.is_ascii_alphanumeric() || c == '.' || c == '-' || c == ':' || c == '[' || c == ']'
        });
    if !ok {
        return Source::Direct;
    }
    if own_hosts.iter().any(|h| host_only(h) == host) {
        return Source::Internal;
    }
    Source::External(host)
}

/// `Host` 모양(`이름:포트`, `[::1]:포트`)에서 포트를 떼고 소문자로.
fn host_only(authority: &str) -> String {
    let a = authority.trim().to_ascii_lowercase();
    if let Some(v6) = a.strip_prefix('[') {
        return format!("[{}]", v6.split(']').next().unwrap_or_default());
    }
    a.split(':').next().unwrap_or_default().to_owned()
}

#[derive(Default)]
struct Pending {
    /// 날 → (조회, 방문자)
    days: BTreeMap<String, (i64, i64)>,
    /// (날, 경로) → (조회, 방문자)
    pages: BTreeMap<(String, String), (i64, i64)>,
    /// (날, 호스트) → 들어온 수
    referrers: BTreeMap<(String, String), i64>,
}

struct Inner {
    day: i64,
    salt: [u8; 32],
    seen: HashSet<[u8; 32]>,
    order: VecDeque<[u8; 32]>,
    pending: Pending,
    /// 마지막으로 보존 정리를 한 날
    purged: Option<i64>,
}

/// 서버 하나에 하나. 웹 계층이 공개 화면마다 [`StatCounter::record`] 를 부른다.
pub struct StatCounter {
    inner: Mutex<Inner>,
}

impl Default for StatCounter {
    fn default() -> Self {
        Self::new()
    }
}

fn day_of(unix: i64) -> i64 {
    (unix + DAY_OFFSET_SECS).div_euclid(86_400)
}

/// 한국 시간 날짜 `YYYY-MM-DD`.
fn day_string(day: i64) -> String {
    let date = OffsetDateTime::from_unix_timestamp(day * 86_400)
        .map(|t| t.date())
        .unwrap_or(Date::MIN);
    date_string(date)
}

fn date_string(d: Date) -> String {
    format!("{:04}-{:02}-{:02}", d.year(), u8::from(d.month()), d.day())
}

fn fresh_salt() -> [u8; 32] {
    let mut salt = [0u8; 32];
    if getrandom::fill(&mut salt).is_err() {
        salt = Sha256::digest(crate::clock::now().as_bytes()).into();
    }
    salt
}

impl StatCounter {
    pub fn new() -> Self {
        Self {
            inner: Mutex::new(Inner {
                day: day_of(OffsetDateTime::now_utc().unix_timestamp()),
                salt: fresh_salt(),
                seen: HashSet::new(),
                order: VecDeque::new(),
                pending: Pending::default(),
                purged: None,
            }),
        }
    }

    fn lock(&self) -> MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// 방문 한 번. 경로를 읽을 수 없으면 세지 않고 거짓.
    pub fn record(&self, hit: &Hit<'_>) -> bool {
        self.record_at(OffsetDateTime::now_utc().unix_timestamp(), hit)
    }

    fn record_at(&self, unix: i64, hit: &Hit<'_>) -> bool {
        let Some(path) = normalize_path(hit.path) else {
            return false;
        };
        let source = source_of(hit.referrer, hit.own_hosts);
        let mut g = self.lock();
        let day = day_of(unix);
        if day != g.day {
            g.day = day;
            g.salt = fresh_salt();
            g.seen.clear();
            g.order.clear();
        }
        let salt = g.salt;
        let key = |extra: &str| -> [u8; 32] {
            let mut h = Sha256::new();
            h.update(salt);
            for part in [hit.ip, hit.user_agent, extra] {
                h.update((part.len() as u64).to_le_bytes());
                h.update(part.as_bytes());
            }
            h.finalize().into()
        };
        // 사이트 방문자 표시와 화면별 표시. 경로는 `/` 로 시작하므로 빈 값과 겹치지 않는다.
        let new_site = g.remember(key(""));
        let new_page = g.remember(key(&path));
        let date = day_string(day);
        let d = g.pending.days.entry(date.clone()).or_default();
        d.0 += 1;
        d.1 += i64::from(new_site);
        let p = g.pending.pages.entry((date.clone(), path)).or_default();
        p.0 += 1;
        p.1 += i64::from(new_page);
        if let Source::External(host) = source {
            *g.pending.referrers.entry((date, host)).or_default() += 1;
        }
        true
    }

    fn take(&self) -> StatBatch {
        let p = std::mem::take(&mut self.lock().pending);
        StatBatch {
            days: p
                .days
                .into_iter()
                .map(|(day, (views, visitors))| StatDay {
                    day,
                    views,
                    visitors,
                })
                .collect(),
            pages: p
                .pages
                .into_iter()
                .map(|((day, path), (views, visitors))| {
                    (
                        day,
                        StatPage {
                            path,
                            views,
                            visitors,
                            title: String::new(),
                        },
                    )
                })
                .collect(),
            referrers: p
                .referrers
                .into_iter()
                .map(|((day, host), visits)| (day, StatReferrer { host, visits }))
                .collect(),
        }
    }

    fn put_back(&self, batch: StatBatch) {
        let mut g = self.lock();
        for d in batch.days {
            let e = g.pending.days.entry(d.day).or_default();
            e.0 += d.views;
            e.1 += d.visitors;
        }
        for (day, p) in batch.pages {
            let e = g.pending.pages.entry((day, p.path)).or_default();
            e.0 += p.views;
            e.1 += p.visitors;
        }
        for (day, r) in batch.referrers {
            *g.pending.referrers.entry((day, r.host)).or_default() += r.visits;
        }
    }
}

impl Inner {
    /// 오늘 처음 보는 표시면 참.
    fn remember(&mut self, key: [u8; 32]) -> bool {
        if !self.seen.insert(key) {
            return false;
        }
        self.order.push_back(key);
        while self.order.len() > MAX_SEEN {
            if let Some(old) = self.order.pop_front() {
                self.seen.remove(&old);
            }
        }
        true
    }
}

/// 모은 수를 DB 에 더하고 보존 기간이 지난 날을 지운다. 더하기에 실패하면 수를 되돌려 둔다.
pub async fn flush(ctx: &Ctx, counter: &StatCounter) -> Result<usize> {
    let batch = counter.take();
    let n = batch.days.len();
    if n > 0
        && let Err(e) = ctx.store.add_stats(&batch).await
    {
        counter.put_back(batch);
        return Err(e);
    }
    // 보존 정리는 하루에 한 번(서버를 켠 뒤 첫 반영 포함). 실패하면 다음 반영에 다시 한다.
    let day = day_of(OffsetDateTime::now_utc().unix_timestamp());
    if counter.lock().purged != Some(day) {
        let keep = settings::site(ctx).await?.stats_retention_days;
        ctx.store.purge_stats(&cutoff(today(), keep)).await?;
        counter.lock().purged = Some(day);
    }
    Ok(n)
}

fn today() -> Date {
    OffsetDateTime::now_utc()
        .saturating_add(Duration::seconds(DAY_OFFSET_SECS))
        .date()
}

/// 이 날짜보다 앞선 날은 지운다.
fn cutoff(today: Date, keep_days: u32) -> String {
    date_string(today.saturating_sub(Duration::days(i64::from(keep_days))))
}

/// 요약 기간.
#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StatsQuery {
    /// 오늘까지 며칠 (1~366, 기본 30)
    #[serde(default)]
    pub days: Option<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct StatsSummary {
    pub from: String,
    pub to: String,
    pub views: i64,
    /// 날마다 센 방문자의 합 (여러 날 온 사람은 날마다 한 번)
    pub visitors: i64,
    /// 기간의 모든 날 (없는 날은 0)
    pub daily: Vec<StatDay>,
    pub pages: Vec<StatPage>,
    pub referrers: Vec<StatReferrer>,
}

/// 기간 요약. 발행 권한(SPEC §9 `stats_summary`: read/publish)이 필요하다.
pub async fn summary(ctx: &Ctx, actor: &Actor, q: &StatsQuery) -> Result<StatsSummary> {
    actor.require(Permission::PostPublish)?;
    let days = q.days.unwrap_or(30);
    if !(1..=MAX_DAYS).contains(&days) {
        return Err(Error::new(Code::Validation)
            .with_message("error.stats_days")
            .arg("max", MAX_DAYS.to_string()));
    }
    let to_date = today();
    let from_date = to_date.saturating_sub(Duration::days(i64::from(days - 1)));
    let (from, to) = (date_string(from_date), date_string(to_date));
    let stored = ctx.store.stat_days(&from, &to).await?;
    let mut daily = Vec::with_capacity(days as usize);
    let mut d = from_date;
    loop {
        let s = date_string(d);
        daily.push(
            stored
                .iter()
                .find(|x| x.day == s)
                .cloned()
                .unwrap_or(StatDay {
                    day: s,
                    views: 0,
                    visitors: 0,
                }),
        );
        if d >= to_date {
            break;
        }
        d = d.saturating_add(Duration::days(1));
    }
    let mut pages = ctx.store.stat_pages(&from, &to, TOP).await?;
    for p in &mut pages {
        p.title = title_of(ctx, &p.path).await;
    }
    Ok(StatsSummary {
        views: daily.iter().map(|d| d.views).sum(),
        visitors: daily.iter().map(|d| d.visitors).sum(),
        referrers: ctx.store.stat_referrers(&from, &to, TOP).await?,
        from,
        to,
        daily,
        pages,
    })
}

/// 경로가 가리키는 글·페이지·게시판 이름. 모르면 빈 값(경로만 보인다).
async fn title_of(ctx: &Ctx, path: &str) -> String {
    let parts: Vec<&str> = path.trim_start_matches('/').split('/').collect();
    let found = match parts.as_slice() {
        [""] => Some("첫 화면".to_owned()),
        ["b", board] => ctx
            .store
            .board_by_slug(board)
            .await
            .ok()
            .flatten()
            .map(|b| b.name),
        ["b", board, post] => match ctx.store.board_by_slug(board).await.ok().flatten() {
            Some(b) => ctx
                .store
                .post_by_slug(&b.id, post)
                .await
                .ok()
                .flatten()
                .map(|p| p.title),
            None => None,
        },
        ["p", page] => ctx
            .store
            .page_by_slug(page)
            .await
            .ok()
            .flatten()
            .map(|p| p.title),
        _ => None,
    };
    found.unwrap_or_default()
}

/// CSV (UTF-8 BOM, 구역 셋). 셀이 수식 문자로 시작하면 `'` 를 붙인다.
pub fn csv(s: &StatsSummary) -> String {
    let mut out = String::from("\u{feff}");
    let mut row = |cells: &[&str]| {
        let line: Vec<String> = cells.iter().map(|c| cell(c)).collect();
        out.push_str(&line.join(","));
        out.push_str("\r\n");
    };
    row(&["날짜", "조회", "방문자"]);
    for d in &s.daily {
        row(&[&d.day, &d.views.to_string(), &d.visitors.to_string()]);
    }
    row(&[]);
    row(&["화면", "제목", "조회", "방문자"]);
    for p in &s.pages {
        row(&[
            &p.path,
            &p.title,
            &p.views.to_string(),
            &p.visitors.to_string(),
        ]);
    }
    row(&[]);
    row(&["들어온 곳", "횟수"]);
    for r in &s.referrers {
        row(&[&r.host, &r.visits.to_string()]);
    }
    out
}

/// 수식 주입 방어 + 따옴표.
fn cell(v: &str) -> String {
    let guarded = if v.starts_with(['=', '+', '-', '@', '\t', '\r']) {
        format!("'{v}")
    } else {
        v.to_owned()
    };
    if guarded.contains([',', '"', '\n', '\r']) {
        format!("\"{}\"", guarded.replace('"', "\"\""))
    } else {
        guarded
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    const UA: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) Chrome/130.0";
    const NOON: i64 = 1_793_000_000;
    const OWN: [&str; 2] = ["127.0.0.1:3139", "example.kr"];

    fn hit<'a>(path: &'a str, referrer: &'a str, ip: &'a str) -> Hit<'a> {
        Hit {
            path,
            referrer,
            own_hosts: &OWN,
            ip,
            user_agent: UA,
        }
    }

    #[test]
    fn paths_lose_queries_and_trailing_slashes() {
        assert_eq!(normalize_path("/b/free?page=2#c1").unwrap(), "/b/free");
        assert_eq!(normalize_path("/b/free/").unwrap(), "/b/free");
        assert_eq!(normalize_path("/").unwrap(), "/");
        assert_eq!(normalize_path("/?utm_source=x").unwrap(), "/");
        assert!(normalize_path("http://x/").is_none());
        let long = format!("/{}", "가".repeat(300));
        assert_eq!(
            normalize_path(&long).unwrap().chars().count(),
            MAX_PATH_CHARS
        );
    }

    #[test]
    fn referrers_keep_only_the_host() {
        assert_eq!(
            source_of("https://search.naver.com/search.naver?query=비밀", &OWN),
            Source::External("search.naver.com".into())
        );
        assert_eq!(
            source_of("https://user:pw@Example.COM:8443/x", &["a.kr"]),
            Source::External("example.com".into())
        );
        assert_eq!(
            source_of("http://127.0.0.1:3139/b/free", &OWN),
            Source::Internal
        );
        assert_eq!(source_of("https://example.kr/", &OWN), Source::Internal);
        assert_eq!(source_of("", &OWN), Source::Direct);
        assert_eq!(source_of("android-app://com.google/", &OWN), Source::Direct);
        assert_eq!(source_of("https://<script>/", &OWN), Source::Direct);
    }

    #[test]
    fn visitors_count_once_a_day_and_pages_separately() {
        let c = StatCounter::new();
        assert!(c.record_at(NOON, &hit("/", "", "10.0.0.1")));
        assert!(c.record_at(NOON + 5, &hit("/?x=1", "", "10.0.0.1")));
        assert!(c.record_at(
            NOON + 9,
            &hit("/b/free", "https://www.google.com/", "10.0.0.1")
        ));
        assert!(c.record_at(NOON, &hit("/", "http://127.0.0.1:3139/", "10.0.0.2")));
        assert!(!c.record_at(NOON, &hit("bad", "", "10.0.0.3")));
        let b = c.take();
        assert_eq!(b.days.len(), 1);
        assert_eq!((b.days[0].views, b.days[0].visitors), (4, 2));
        let home = b.pages.iter().find(|(_, p)| p.path == "/").unwrap();
        assert_eq!((home.1.views, home.1.visitors), (3, 2));
        assert_eq!(b.referrers.len(), 1, "내부 이동·직접은 들어온 곳이 아니다");
        assert_eq!(b.referrers[0].1.host, "www.google.com");
        // 다음 날이면 같은 사람도 다시 방문자다.
        assert!(c.record_at(NOON + 86_400, &hit("/", "", "10.0.0.1")));
        let b = c.take();
        assert_eq!(b.days[0].visitors, 1);
        assert_ne!(b.days[0].day, day_string(day_of(NOON)));
    }

    #[test]
    fn taken_counts_can_be_put_back() {
        let c = StatCounter::new();
        c.record_at(NOON, &hit("/", "https://a.kr/", "1"));
        let b = c.take();
        c.record_at(NOON, &hit("/", "https://a.kr/", "2"));
        c.put_back(b);
        let b = c.take();
        assert_eq!((b.days[0].views, b.days[0].visitors), (2, 2));
        assert_eq!(b.referrers[0].1.visits, 2);
    }

    #[test]
    fn memory_is_bounded() {
        let c = StatCounter::new();
        for i in 0..(MAX_SEEN / 2 + 10) {
            c.record_at(NOON, &hit("/", "", &format!("ip{i}")));
        }
        let g = c.lock();
        assert_eq!(g.seen.len(), MAX_SEEN);
        assert_eq!(g.order.len(), MAX_SEEN);
    }

    #[test]
    fn csv_guards_formulas_and_quotes() {
        assert_eq!(cell("=HYPERLINK(\"x\")"), "\"'=HYPERLINK(\"\"x\"\")\"");
        assert_eq!(cell("+1"), "'+1");
        assert_eq!(cell("@a"), "'@a");
        assert_eq!(cell("-2"), "'-2");
        assert_eq!(cell("a,b"), "\"a,b\"");
        assert_eq!(cell("/b/free"), "/b/free");
        let s = StatsSummary {
            from: "2026-11-01".into(),
            to: "2026-11-01".into(),
            views: 1,
            visitors: 1,
            daily: vec![StatDay {
                day: "2026-11-01".into(),
                views: 1,
                visitors: 1,
            }],
            pages: vec![],
            referrers: vec![StatReferrer {
                host: "=cmd".into(),
                visits: 1,
            }],
        };
        let out = csv(&s);
        assert!(out.starts_with('\u{feff}'));
        assert!(out.contains("2026-11-01,1,1\r\n"));
        assert!(out.contains("'=cmd,1"));
    }

    #[test]
    fn cutoff_keeps_the_last_n_days() {
        let d = Date::from_calendar_date(2026, time::Month::November, 4).unwrap();
        assert_eq!(cutoff(d, 30), "2026-10-05");
        assert_eq!(day_string(day_of(NOON)).len(), 10);
    }
}
