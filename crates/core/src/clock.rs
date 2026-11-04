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

/// 지금부터 `hours` 시간 전. 저장 형식이라 문자열 비교로 「그 뒤인지」 알 수 있다.
pub fn hours_ago(hours: i64) -> String {
    format(OffsetDateTime::now_utc() - time::Duration::hours(hours))
}

/// 저장된 시각이나 사용자가 준 RFC3339 시각을 읽는다.
/// 지금부터 `minutes` 분 뒤.
pub fn minutes_later(minutes: i64) -> String {
    format(OffsetDateTime::now_utc() + time::Duration::minutes(minutes))
}

pub fn parse(s: &str) -> Option<OffsetDateTime> {
    OffsetDateTime::parse(s, &Rfc3339).ok()
}

/// 그 시각에서 지금까지 흐른 초.
pub fn seconds_since(s: &str) -> Option<i64> {
    parse(s).map(|t| (OffsetDateTime::now_utc() - t).whole_seconds())
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

const LOCAL_INPUT: &[BorrowedFormatItem<'static>] =
    format_description!("[year]-[month]-[day]T[hour]:[minute]");

fn site_offset(offset_minutes: i32) -> UtcOffset {
    UtcOffset::from_whole_seconds(offset_minutes * 60).unwrap_or(UtcOffset::UTC)
}

/// 브라우저 `datetime-local` 값(`2026-11-05T09:00`, 초는 버린다)을 사이트 시간대 시각으로 읽어 RFC3339 로.
/// 이미 RFC3339 면 그대로 받는다.
pub fn from_local_input(s: &str, offset_minutes: i32) -> Option<String> {
    let s = s.trim();
    if let Some(t) = parse(s) {
        return Some(format(t));
    }
    let head = s.get(..16)?;
    let t = time::PrimitiveDateTime::parse(head, LOCAL_INPUT).ok()?;
    Some(format(t.assume_offset(site_offset(offset_minutes))))
}

/// 저장된 시각을 `datetime-local` 입력 값으로 (사이트 시간대).
pub fn to_local_input(s: &str, offset_minutes: i32) -> String {
    parse(s)
        .and_then(|t| {
            t.to_offset(site_offset(offset_minutes))
                .format(LOCAL_INPUT)
                .ok()
        })
        .unwrap_or_default()
}

/// 화면용 날짜·시각 `2026. 11. 5. 09:00` (사이트 시간대).
pub fn display_datetime(s: &str, offset_minutes: i32) -> String {
    let Some(t) = parse(s) else {
        return String::new();
    };
    let t = t.to_offset(site_offset(offset_minutes));
    format!(
        "{}. {}. {}. {:02}:{:02}",
        t.year(),
        u8::from(t.month()),
        t.day(),
        t.hour(),
        t.minute()
    )
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

    #[test]
    fn local_input_round_trip() {
        let utc = from_local_input("2026-11-05T09:00", 540).unwrap();
        assert_eq!(utc, "2026-11-05T00:00:00.000000Z");
        assert_eq!(to_local_input(&utc, 540), "2026-11-05T09:00");
        assert_eq!(display_datetime(&utc, 540), "2026. 11. 5. 09:00");
        assert_eq!(
            from_local_input("2026-11-05T09:00:30", 0).unwrap(),
            "2026-11-05T09:00:00.000000Z"
        );
        assert_eq!(
            from_local_input("2026-11-05T09:00:00+09:00", 0).unwrap(),
            utc
        );
        assert!(from_local_input("내일 아침", 540).is_none());
        assert!(from_local_input("", 540).is_none());
    }
}
