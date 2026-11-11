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
    /// 끝에 이어 쓴다. 없으면 만든다. 이어 쓴 뒤의 크기를 돌려준다 (나눠 올리기, ADR-0161).
    async fn append(&self, path: &str, bytes: &[u8]) -> Result<u64> {
        let mut all = self.get(path).await?.unwrap_or_default();
        all.extend_from_slice(bytes);
        self.put(path, &all).await?;
        Ok(all.len() as u64)
    }
    /// 바이트 수. 없으면 `None`.
    async fn size(&self, path: &str) -> Result<Option<u64>> {
        Ok(self.get(path).await?.map(|b| b.len() as u64))
    }
    /// `start` 부터 최대 `len` 바이트 (동영상 부분 응답).
    async fn get_range(&self, path: &str, start: u64, len: u64) -> Result<Option<Vec<u8>>> {
        Ok(self.get(path).await?.map(|b| {
            let s = usize::try_from(start).unwrap_or(usize::MAX).min(b.len());
            let e = s
                .saturating_add(usize::try_from(len).unwrap_or(usize::MAX))
                .min(b.len());
            b[s..e].to_vec()
        }))
    }
    /// 이름을 바꾼다. 받는 쪽이 있으면 덮어쓴다.
    async fn rename(&self, from: &str, to: &str) -> Result<()> {
        if let Some(b) = self.get(from).await? {
            self.put(to, &b).await?;
            self.delete(from).await?;
        }
        Ok(())
    }
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
