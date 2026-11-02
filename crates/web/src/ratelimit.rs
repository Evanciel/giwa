//! 연결 주소별 분 단위 요청 제한 (PHASE1_PLAN §7).
//!
//! 키는 연결 주소만 쓴다. `X-Forwarded-For` 같은 전달 헤더는 믿지 않는다.
//! 키 수에 상한을 두고, 가득 차면 지난 분의 키를 먼저 지운다. 그래도 차 있으면 거절한다.

use std::collections::HashMap;
use std::net::IpAddr;
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

const MAX_KEYS: usize = 10_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Bucket {
    /// 모든 요청
    All,
    /// 로그인 시도 (비밀번호, 로그인 링크 사용)
    Login,
}

impl Bucket {
    fn limit(self) -> u32 {
        match self {
            Self::All => 600,
            Self::Login => 10,
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct Window {
    minute: u64,
    count: u32,
}

#[derive(Debug, Default)]
pub struct RateLimiter {
    keys: Mutex<HashMap<(IpAddr, Bucket), Window>>,
}

fn now_minute() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() / 60)
        .unwrap_or(0)
}

impl RateLimiter {
    /// 이번 요청을 세고, 한도 안이면 `true`.
    pub fn check(&self, ip: IpAddr, bucket: Bucket) -> bool {
        self.check_at(ip, bucket, now_minute())
    }

    fn check_at(&self, ip: IpAddr, bucket: Bucket, minute: u64) -> bool {
        let mut keys = self.keys.lock().unwrap_or_else(|e| e.into_inner());
        let key = (ip, bucket);
        if keys.len() >= MAX_KEYS && !keys.contains_key(&key) {
            keys.retain(|_, w| w.minute == minute);
            if keys.len() >= MAX_KEYS {
                return false;
            }
        }
        let w = keys.entry(key).or_insert(Window { minute, count: 0 });
        if w.minute != minute {
            *w = Window { minute, count: 0 };
        }
        w.count = w.count.saturating_add(1);
        w.count <= bucket.limit()
    }

    /// 지난 분의 키를 지운다. 서버가 주기적으로 부른다.
    pub fn purge(&self) -> usize {
        self.purge_at(now_minute())
    }

    fn purge_at(&self, minute: u64) -> usize {
        let mut keys = self.keys.lock().unwrap_or_else(|e| e.into_inner());
        let before = keys.len();
        keys.retain(|_, w| w.minute == minute);
        before - keys.len()
    }

    pub fn len(&self) -> usize {
        self.keys.lock().unwrap_or_else(|e| e.into_inner()).len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::Ipv4Addr;

    fn ip(n: u32) -> IpAddr {
        IpAddr::V4(Ipv4Addr::from(n))
    }

    #[test]
    fn login_limit_and_next_minute_reset() {
        let r = RateLimiter::default();
        for _ in 0..10 {
            assert!(r.check_at(ip(1), Bucket::Login, 5));
        }
        assert!(!r.check_at(ip(1), Bucket::Login, 5));
        // 다른 주소, 다른 버킷은 따로 센다.
        assert!(r.check_at(ip(2), Bucket::Login, 5));
        assert!(r.check_at(ip(1), Bucket::All, 5));
        assert!(r.check_at(ip(1), Bucket::Login, 6));
    }

    #[test]
    fn keys_are_bounded_and_purged() {
        let r = RateLimiter::default();
        for n in 0..MAX_KEYS as u32 {
            assert!(r.check_at(ip(n), Bucket::All, 1));
        }
        assert_eq!(r.len(), MAX_KEYS);
        // 같은 분에 새 키는 거절한다.
        assert!(!r.check_at(ip(u32::MAX), Bucket::All, 1));
        assert_eq!(r.len(), MAX_KEYS);
        // 다음 분에는 지난 키를 비우고 받는다.
        assert!(r.check_at(ip(u32::MAX), Bucket::All, 2));
        assert_eq!(r.len(), 1);
        assert_eq!(r.purge_at(3), 1);
        assert!(r.is_empty());
    }
}
