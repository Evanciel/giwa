//! 사본 DB (ADR-0121). `VACUUM INTO` 로 한 시점을 복사해 같은 서비스를 돌린다.
//! 적용 전 시험과 미리보기에 쓴다. 파일은 `<데이터>/previews/` 에 두고 백업에 넣지 않는다.
//!
//! 미디어 보관소는 원본을 함께 쓴다. changeset 연산은 미디어 파일을 건드리지 않는다.

use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use maru_core::services::changesets::Sandbox;
use maru_core::{Ctx, Error, Result};

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

    fn remove_files(&self, key: &str) {
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
