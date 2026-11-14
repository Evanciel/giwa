//! 환경 변수 (ADR-0173). `GIWA_<이름>` 이 먼저, 없으면 이름을 바꾸기 전의 `MARU_<이름>` 을 읽는다.

/// `name` 은 접두 없이 (`TOKEN`, `LANG` …).
pub fn var(name: &str) -> Option<String> {
    ["GIWA_", "MARU_"]
        .iter()
        .find_map(|prefix| std::env::var(format!("{prefix}{name}")).ok())
}
