//! 사본 DB (ADR-0121). `VACUUM INTO` 로 한 시점을 복사해 같은 서비스를 돌린다.
//! 적용 전 시험과 미리보기에 쓴다. 파일은 `<데이터>/previews/` 에 두고 백업에 넣지 않는다.
//!
//! 미디어는 원본을 읽고, 사본에서 새로 쓰는 파일(예시 글 그림, ADR-0142)은 `previews/<key>-media/` 에
//! 따로 둔다. 원본 파일은 사본에서 지우지 않고, 사본을 닫으면 따로 둔 파일도 지운다.

use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use giwa_core::blobs::Blobs;
use giwa_core::services::changesets::Sandbox;
use giwa_core::{Ctx, Error, Result};

use crate::blobs::FsBlobs;
use crate::data_dir::{DataDir, io_error};
use crate::db::{Db, db_error};
use crate::sqlite::SqliteStore;

pub struct FsSandbox {
    dir: PathBuf,
    db: Db,
    open: Mutex<HashMap<String, Db>>,
}

impl FsSandbox {
    pub fn new(data: &DataDir, db: Db) -> Self {
        Self {
            dir: data.root().join("previews"),
            db,
            open: Mutex::new(HashMap::new()),
        }
    }

    /// 꺼질 때 못 지운 사본을 치운다. 서버를 켤 때 부른다.
    pub fn clear(&self) {
        let _ = fs::remove_dir_all(&self.dir);
    }

    fn path(&self, key: &str) -> PathBuf {
        self.dir.join(format!("{key}.db"))
    }

    fn media_dir(&self, key: &str) -> PathBuf {
        self.dir.join(format!("{key}-media"))
    }

    fn remove_files(&self, key: &str) {
        let _ = fs::remove_dir_all(self.media_dir(key));
        let path = self.path(key);
        for suffix in ["", "-wal", "-shm"] {
            let mut name = path.clone().into_os_string();
            name.push(suffix);
            let _ = fs::remove_file(name);
        }
    }
}

#[async_trait]
impl Sandbox for FsSandbox {
    async fn open(&self, ctx: &Ctx, key: &str) -> Result<Ctx> {
        self.close(key).await;
        fs::create_dir_all(&self.dir).map_err(|e| io_error(&self.dir, e))?;
        let path = self.path(key);
        sqlx::query("VACUUM INTO ?")
            .bind(path.to_string_lossy().into_owned())
            .execute(&self.db.read)
            .await
            .map_err(db_error)?;
        let copy = Db::open(&path, false).await?;
        self.open
            .lock()
            .map_err(|_| Error::internal("사본 목록 잠금 실패"))?
            .insert(key.to_owned(), copy.clone());
        let mut out = ctx.clone();
        out.store = Arc::new(SqliteStore::new(copy));
        out.blobs = Arc::new(Overlay {
            top: FsBlobs::new(self.media_dir(key)),
            base: ctx.blobs.clone(),
        });
        Ok(out)
    }

    async fn close(&self, key: &str) {
        let db = self.open.lock().ok().and_then(|mut m| m.remove(key));
        if let Some(db) = db {
            db.close().await;
        }
        self.remove_files(key);
    }
}

/// 사본의 미디어: 쓰기·지우기는 위층에만, 읽기는 위층 다음 원본.
struct Overlay {
    top: FsBlobs,
    base: Arc<dyn Blobs>,
}

#[async_trait]
impl Blobs for Overlay {
    async fn put(&self, path: &str, bytes: &[u8]) -> Result<()> {
        self.top.put(path, bytes).await
    }

    async fn get(&self, path: &str) -> Result<Option<Vec<u8>>> {
        match self.top.get(path).await? {
            Some(b) => Ok(Some(b)),
            None => self.base.get(path).await,
        }
    }

    async fn delete(&self, path: &str) -> Result<()> {
        self.top.delete(path).await
    }
}
