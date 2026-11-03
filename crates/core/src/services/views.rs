//! 조회수 (GAP C6, ADR-0049).
//!
//! - 글 화면을 그릴 때 센다. 같은 방문자가 같은 날(한국 시간) 같은 글을 다시 열면 세지 않는다.
//! - 방문자 표시는 SHA-256(그날의 무작위 소금 + IP + User-Agent + 글 ID) 이다. 소금은 메모리에만 있고
//!   날이 바뀌면 새로 만든다. IP 원문도 해시도 DB 에 남기지 않는다.
//! - 봇(User-Agent), 발행 권한이 있는 사람(운영자·편집자)은 세지 않는다.
//! - 모은 수는 주기마다 [`flush`] 로 한 트랜잭션에 더한다. 조회수는 통계라 감사 대상이 아니다.
//!   사람이 조회수를 고치는 [`set`] 은 감사에 남긴다.

use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::{Mutex, MutexGuard};

use serde_json::json;
use sha2::{Digest, Sha256};
use time::OffsetDateTime;

use crate::actor::{Actor, Permission};
use crate::audit::{self, Target};
use crate::error::{Code, Error, Result};
use crate::model::Post;
use crate::store::{Ctx, Tx};

/// 하루에 기억하는 (방문자, 글) 수. 넘으면 오래된 것부터 잊는다(그 방문자가 다시 보면 한 번 더 센다).
pub const MAX_SEEN: usize = 100_000;
/// 운영자가 정할 수 있는 조회수 상한.
pub const MAX_VIEWS: i64 = 1_000_000_000_000;
/// 하루 경계는 한국 시간 자정.
const DAY_OFFSET_SECS: i64 = 9 * 3600;

/// 이 낱말이 User-Agent 에 들어 있으면 사람이 아닌 것으로 본다 (소문자 비교).
const BOT_MARKERS: [&str; 16] = [
    "bot",
    "crawl",
    "spider",
    "slurp",
    "preview",
    "headless",
    "lighthouse",
    "facebookexternalhit",
    "curl/",
    "wget/",
    "python-",
    "go-http-client",
    "okhttp",
    "httpclient",
    "monitor",
    "feed",
];

/// User-Agent 가 비었거나 봇 표시가 있으면 참.
pub fn is_bot(user_agent: &str) -> bool {
    let ua = user_agent.trim().to_ascii_lowercase();
    ua.is_empty() || BOT_MARKERS.iter().any(|m| ua.contains(m))
}

struct Inner {
    day: i64,
    salt: [u8; 32],
    seen: HashSet<[u8; 32]>,
    order: VecDeque<[u8; 32]>,
    pending: HashMap<String, i64>,
}

/// 서버 하나에 하나. 웹 계층이 들고 있다가 글 화면마다 [`ViewCounter::record`] 를 부른다.
pub struct ViewCounter {
    inner: Mutex<Inner>,
}

impl Default for ViewCounter {
    fn default() -> Self {
        Self::new()
    }
}

fn day_of(unix: i64) -> i64 {
    (unix + DAY_OFFSET_SECS).div_euclid(86_400)
}

fn fresh_salt() -> [u8; 32] {
    let mut salt = [0u8; 32];
    // 무작위를 못 얻으면 날짜만으로 섞인 소금이 된다. 방문자 구분이 약해질 뿐 저장되지는 않는다.
    if getrandom::fill(&mut salt).is_err() {
        salt = Sha256::digest(crate::clock::now().as_bytes()).into();
    }
    salt
}

impl ViewCounter {
    pub fn new() -> Self {
        Self {
            inner: Mutex::new(Inner {
                day: day_of(OffsetDateTime::now_utc().unix_timestamp()),
                salt: fresh_salt(),
                seen: HashSet::new(),
                order: VecDeque::new(),
                pending: HashMap::new(),
            }),
        }
    }

    fn lock(&self) -> MutexGuard<'_, Inner> {
        // 다른 스레드가 패닉해도 계수는 계속한다 (값이 조금 틀릴 뿐이다).
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// 조회 한 번. 센 경우에만 참이다.
    /// `viewer` 는 로그인한 사람(없으면 손님). 발행 권한이 있으면 세지 않는다.
    pub fn record(
        &self,
        viewer: Option<&Actor>,
        post_id: &str,
        ip: &str,
        user_agent: &str,
    ) -> bool {
        if viewer.is_some_and(|a| a.has(Permission::PostPublish)) || is_bot(user_agent) {
            return false;
        }
        self.record_at(
            OffsetDateTime::now_utc().unix_timestamp(),
            post_id,
            ip,
            user_agent,
        )
    }

    fn record_at(&self, unix: i64, post_id: &str, ip: &str, user_agent: &str) -> bool {
        let mut g = self.lock();
        let day = day_of(unix);
        if day != g.day {
            g.day = day;
            g.salt = fresh_salt();
            g.seen.clear();
            g.order.clear();
        }
        let mut h = Sha256::new();
        h.update(g.salt);
        for part in [ip, user_agent, post_id] {
            h.update((part.len() as u64).to_le_bytes());
            h.update(part.as_bytes());
        }
        let key: [u8; 32] = h.finalize().into();
        if !g.seen.insert(key) {
            return false;
        }
        g.order.push_back(key);
        while g.order.len() > MAX_SEEN {
            if let Some(old) = g.order.pop_front() {
                g.seen.remove(&old);
            }
        }
        *g.pending.entry(post_id.to_owned()).or_insert(0) += 1;
        true
    }

    /// 아직 DB 에 더하지 않은 수. 화면에 보일 조회수 = DB 값 + 이것.
    pub fn pending(&self, post_id: &str) -> i64 {
        self.lock().pending.get(post_id).copied().unwrap_or(0)
    }

    fn take(&self) -> Vec<(String, i64)> {
        let mut out: Vec<(String, i64)> = self.lock().pending.drain().collect();
        out.sort();
        out
    }

    fn put_back(&self, counts: Vec<(String, i64)>) {
        let mut g = self.lock();
        for (id, n) in counts {
            *g.pending.entry(id).or_insert(0) += n;
        }
    }

    /// 화면에 보일 조회수 (DB 값 + 아직 더하지 않은 수).
    pub fn shown(&self, post: &Post) -> i64 {
        post.view_count.saturating_add(self.pending(&post.id))
    }
}

/// 모은 조회수를 DB 에 더한다. 실패하면 수를 되돌려 두고 다음에 다시 시도한다. 더한 글 수를 돌려준다.
pub async fn flush(ctx: &Ctx, counter: &ViewCounter) -> Result<usize> {
    let counts = counter.take();
    if counts.is_empty() {
        return Ok(0);
    }
    match ctx.store.add_views(&counts).await {
        Ok(()) => Ok(counts.len()),
        Err(e) => {
            counter.put_back(counts);
            Err(e)
        }
    }
}

/// 조회수를 정한 값으로 바꾼다 (예: 시험 조회를 0 으로). 게시판 관리 권한이 필요하고 감사에 남는다.
pub async fn set(ctx: &Ctx, actor: &Actor, id: &str, views: i64) -> Result<Post> {
    if !(0..=MAX_VIEWS).contains(&views) {
        return Err(Error::new(Code::Validation)
            .with_message("error.post_views")
            .arg("max", MAX_VIEWS.to_string()));
    }
    let mut tx = Tx::begin(ctx).await?;
    let post = tx
        .post_by_id(id)
        .await?
        .ok_or_else(|| super::posts::not_found(id))?;
    let board = tx
        .board_by_id(&post.board_id)
        .await?
        .ok_or_else(|| Error::internal("글의 게시판이 없습니다"))?;
    actor.require(Permission::PostModerate)?;
    actor.require_board(Permission::PostModerate, &board.id)?;
    actor.require_rank(Permission::PostModerate, &board.policy.moderate)?;
    // 같은 값이어도 다시 쓴다(멱등). 감사에는 그대로 남는다.
    tx.set_view_count(&post.id, views).await?;
    tx.audit(
        actor,
        "post.views.set",
        Target::post(&post.id),
        audit::diff(
            Some(&json!({ "view_count": post.view_count })),
            Some(&json!({ "view_count": views })),
        ),
    )
    .await?;
    tx.commit().await?;
    Ok(Post {
        view_count: views,
        ..post
    })
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    const UA: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) Chrome/130.0";
    const NOON: i64 = 1_793_000_000;

    #[test]
    fn bots_and_empty_agents_are_not_people() {
        assert!(is_bot(""));
        assert!(is_bot("Mozilla/5.0 (compatible; Googlebot/2.1)"));
        assert!(is_bot("Yeti/1.1 (NHN Corp.; https://naver.me/spd) spider"));
        assert!(is_bot("curl/8.4.0"));
        assert!(is_bot("Mozilla/5.0 HeadlessChrome/120"));
        assert!(!is_bot(UA));
    }

    #[test]
    fn same_visitor_counts_once_a_day() {
        let c = ViewCounter::new();
        assert!(c.record_at(NOON, "p1", "10.0.0.1", UA));
        assert!(!c.record_at(NOON + 60, "p1", "10.0.0.1", UA), "새로고침");
        assert!(c.record_at(NOON, "p2", "10.0.0.1", UA), "다른 글");
        assert!(c.record_at(NOON, "p1", "10.0.0.2", UA), "다른 IP");
        assert!(
            c.record_at(NOON, "p1", "10.0.0.1", "Mozilla/5.0 Firefox/131"),
            "다른 브라우저"
        );
        assert_eq!(c.pending("p1"), 3);
        // 다음 날이면 다시 센다.
        assert!(c.record_at(NOON + 86_400, "p1", "10.0.0.1", UA));
        assert_eq!(c.pending("p1"), 4);
    }

    #[test]
    fn staff_and_bots_are_skipped() {
        let c = ViewCounter::new();
        assert!(!c.record(Some(&Actor::local()), "p1", "10.0.0.1", UA));
        assert!(!c.record(None, "p1", "10.0.0.1", "Googlebot"));
        assert!(c.record(None, "p1", "10.0.0.1", UA));
        assert_eq!(c.pending("p1"), 1);
    }

    #[test]
    fn memory_is_bounded() {
        let c = ViewCounter::new();
        for i in 0..(MAX_SEEN + 10) {
            assert!(c.record_at(NOON, "p1", &format!("ip{i}"), UA));
        }
        let g = c.lock();
        assert_eq!(g.seen.len(), MAX_SEEN);
        assert_eq!(g.order.len(), MAX_SEEN);
    }

    #[test]
    fn taken_counts_can_be_put_back() {
        let c = ViewCounter::new();
        c.record_at(NOON, "p1", "a", UA);
        c.record_at(NOON, "p2", "a", UA);
        let taken = c.take();
        assert_eq!(taken, vec![("p1".to_owned(), 1), ("p2".to_owned(), 1)]);
        assert_eq!(c.pending("p1"), 0);
        c.record_at(NOON, "p1", "b", UA);
        c.put_back(taken);
        assert_eq!(c.pending("p1"), 2);
    }
}
