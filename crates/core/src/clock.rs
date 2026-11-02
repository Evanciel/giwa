//! ID 와 시각. 시각은 UTC 고정 자릿수 문자열이라 문자열 정렬이 시간 정렬과 같다.

use time::format_description::BorrowedFormatItem;
use time::format_description::well_known::{Rfc2822, Rfc3339};
use time::macros::format_description;
use time::{OffsetDateTime, UtcOffset};

pub type Id = String;

const FORMAT: &[BorrowedFormatItem<'static>] =
    format_description!("[year]-[month]-[day]T[hour]:[minute]:[second].[subsecond digits:6]Z");

pub fn new_id() -> Id {
    ulid::Ulid::generate().to_string().to_ascii_lowercase()
}

pub fn now() -> String {
    format(OffsetDateTime::now_utc())
}

pub fn format(t: OffsetDateTime) -> String {
    t.to_offset(UtcOffset::UTC)
        .format(FORMAT)
        .unwrap_or_default()
}

/// 저장된 시각이나 사용자가 준 RFC3339 시각을 읽는다.
pub fn parse(s: &str) -> Option<OffsetDateTime> {
    OffsetDateTime::parse(s, &Rfc3339).ok()
}

/// 화면용 날짜 `2026. 11. 2.` (한국 표기). `offset_minutes` 는 사이트 시간대(기본 +540).
pub fn display_date(s: &str, offset_minutes: i32) -> String {
    let Some(t) = parse(s) else {
        return String::new();
    };
    let offset = UtcOffset::from_whole_seconds(offset_minutes * 60).unwrap_or(UtcOffset::UTC);
    let t = t.to_offset(offset);
    format!("{}. {}. {}.", t.year(), u8::from(t.month()), t.day())
}

/// RSS `pubDate` 용 RFC 2822 (UTC).
pub fn rfc2822(s: &str) -> String {
    parse(s)
        .and_then(|t| t.to_offset(UtcOffset::UTC).format(&Rfc2822).ok())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fixed_width_and_sortable() {
        let a = format(OffsetDateTime::UNIX_EPOCH);
        assert_eq!(a, "1970-01-01T00:00:00.000000Z");
        let b = now();
        assert_eq!(a.len(), b.len());
        assert!(a < b);
        assert!(parse(&b).is_some());
    }

    #[test]
    fn ids_are_unique_lowercase() {
        let a = new_id();
        assert_eq!(a.len(), 26);
        assert_eq!(a, a.to_ascii_lowercase());
        assert_ne!(a, new_id());
    }
}
