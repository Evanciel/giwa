//! `maru_core::blobs::Blobs` 의 파일 시스템 구현. 뿌리는 데이터 디렉터리의 `media/`.

use std::path::{Path, PathBuf};

use async_trait::async_trait;
use maru_core::blobs::{Blobs, valid_path};
use maru_core::{Code, Error, Result, clock};

#[derive(Debug, Clone)]
pub struct FsBlobs {
    root: PathBuf,
}

impl FsBlobs {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    /// 규칙에 맞는 경로만 뿌리 아래로 이어 붙인다. 뿌리 밖으로 나갈 수 없다.
    fn resolve(&self, path: &str) -> Result<PathBuf> {
        if !valid_path(path) {
            return Err(Error::internal(format!("보관소 경로 규칙 위반: {path:?}")));
        }
        Ok(path
            .split('/')
            .fold(self.root.clone(), |p, seg| p.join(seg)))
    }
}

fn io_error(path: &Path, e: std::io::Error) -> Error {
    Error::new(Code::Internal)
        .with_message("error.io")
        .arg("path", path.display().to_string())
        .with_source(e)
}

#[async_trait]
impl Blobs for FsBlobs {
    async fn put(&self, path: &str, bytes: &[u8]) -> Result<()> {
        let dest = self.resolve(path)?;
        if let Some(parent) = dest.parent() {
            tokio::fs::create_dir_all(parent)
                .await
                .map_err(|e| io_error(parent, e))?;
        }
        // 임시 이름으로 다 쓴 뒤 바꾼다. 중간에 멈춰도 반쪽 파일이 원래 이름으로 보이지 않는다.
        let tmp = dest.with_extension(format!("tmp-{}", clock::new_id()));
        if let Err(e) = tokio::fs::write(&tmp, bytes).await {
            let _ = tokio::fs::remove_file(&tmp).await;
            return Err(io_error(&tmp, e));
        }
        if let Err(e) = tokio::fs::rename(&tmp, &dest).await {
            let _ = tokio::fs::remove_file(&tmp).await;
            return Err(io_error(&dest, e));
        }
        Ok(())
    }

    async fn get(&self, path: &str) -> Result<Option<Vec<u8>>> {
        let src = self.resolve(path)?;
        match tokio::fs::read(&src).await {
            Ok(bytes) => Ok(Some(bytes)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(io_error(&src, e)),
        }
    }

    async fn delete(&self, path: &str) -> Result<()> {
        let src = self.resolve(path)?;
        match tokio::fs::remove_file(&src).await {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(io_error(&src, e)),
        }
    }

    async fn append(&self, path: &str, bytes: &[u8]) -> Result<u64> {
        use tokio::io::AsyncWriteExt;
        let dest = self.resolve(path)?;
        if let Some(parent) = dest.parent() {
            tokio::fs::create_dir_all(parent)
                .await
                .map_err(|e| io_error(parent, e))?;
        }
        let mut f = tokio::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&dest)
            .await
            .map_err(|e| io_error(&dest, e))?;
        f.write_all(bytes).await.map_err(|e| io_error(&dest, e))?;
        f.flush().await.map_err(|e| io_error(&dest, e))?;
        let meta = f.metadata().await.map_err(|e| io_error(&dest, e))?;
        Ok(meta.len())
    }

    async fn size(&self, path: &str) -> Result<Option<u64>> {
        let src = self.resolve(path)?;
        match tokio::fs::metadata(&src).await {
            Ok(m) => Ok(Some(m.len())),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(io_error(&src, e)),
        }
    }

    async fn get_range(&self, path: &str, start: u64, len: u64) -> Result<Option<Vec<u8>>> {
        use tokio::io::{AsyncReadExt, AsyncSeekExt};
        let src = self.resolve(path)?;
        let mut f = match tokio::fs::File::open(&src).await {
            Ok(f) => f,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(io_error(&src, e)),
        };
        f.seek(std::io::SeekFrom::Start(start))
            .await
            .map_err(|e| io_error(&src, e))?;
        let mut out = Vec::new();
        f.take(len)
            .read_to_end(&mut out)
            .await
            .map_err(|e| io_error(&src, e))?;
        Ok(Some(out))
    }

    async fn rename(&self, from: &str, to: &str) -> Result<()> {
        let src = self.resolve(from)?;
        let dest = self.resolve(to)?;
        if let Some(parent) = dest.parent() {
            tokio::fs::create_dir_all(parent)
                .await
                .map_err(|e| io_error(parent, e))?;
        }
        match tokio::fs::rename(&src, &dest).await {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(io_error(&dest, e)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn put_get_delete_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let blobs = FsBlobs::new(dir.path());
        blobs.put("2026/11/a.jpg", b"one").await.unwrap();
        blobs.put("2026/11/a.jpg", b"two").await.unwrap();
        assert_eq!(blobs.get("2026/11/a.jpg").await.unwrap().unwrap(), b"two");
        blobs.delete("2026/11/a.jpg").await.unwrap();
        blobs.delete("2026/11/a.jpg").await.unwrap();
        assert!(blobs.get("2026/11/a.jpg").await.unwrap().is_none());
        // 임시 파일이 남지 않는다.
        let left: Vec<_> = std::fs::read_dir(dir.path().join("2026").join("11"))
            .unwrap()
            .collect();
        assert!(left.is_empty());
    }

    #[tokio::test]
    async fn paths_outside_rules_are_refused() {
        let dir = tempfile::tempdir().unwrap();
        let blobs = FsBlobs::new(dir.path().join("media"));
        for bad in ["../x.jpg", "a/../../x", "/abs.jpg", "C:/x.jpg"] {
            assert!(blobs.put(bad, b"x").await.is_err(), "{bad}");
            assert!(blobs.get(bad).await.is_err(), "{bad}");
        }
    }
}
