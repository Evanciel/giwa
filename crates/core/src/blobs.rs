//! 미디어 파일 보관소 경계. 구현은 `maru-storage`(데이터 디렉터리의 `media/`).
//!
//! 경로는 서비스가 만든 상대 경로(`2026/11/<id>.jpg`)만 쓴다. 사용자가 준 파일 이름은 경로에 들어가지 않는다.

use async_trait::async_trait;

use crate::error::Result;

#[async_trait]
pub trait Blobs: Send + Sync {
    /// 같은 경로가 있으면 덮어쓴다. 쓰다 실패해도 반쪽 파일을 남기지 않는다.
    async fn put(&self, path: &str, bytes: &[u8]) -> Result<()>;
    async fn get(&self, path: &str) -> Result<Option<Vec<u8>>>;
    /// 없으면 아무 일도 하지 않는다.
    async fn delete(&self, path: &str) -> Result<()>;
}

/// 보관소 경로 규칙: 소문자·숫자·`-`·`.` 조각을 `/` 로 잇고, `.`·`..` 조각과 빈 조각은 없다.
pub fn valid_path(path: &str) -> bool {
    !path.is_empty()
        && path.len() <= 200
        && path.split('/').all(|seg| {
            !seg.is_empty()
                && seg != "."
                && seg != ".."
                && !seg.starts_with('.')
                && seg
                    .bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-' || b == b'.')
        })
}

#[cfg(test)]
mod tests {
    use super::valid_path;

    #[test]
    fn path_rules() {
        assert!(valid_path("2026/11/01abc.jpg"));
        assert!(valid_path("2026/11/01abc-640.webp"));
        for bad in [
            "",
            "/etc/passwd",
            "../x.jpg",
            "2026/../x",
            "2026//x",
            "a/.hidden",
            "A.jpg",
            "a\\b.jpg",
            "a b.jpg",
            "한글.jpg",
            "x.jpg/",
        ] {
            assert!(!valid_path(bad), "{bad:?}");
        }
    }
}
