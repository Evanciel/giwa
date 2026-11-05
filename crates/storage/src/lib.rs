//! Maru SQLite 저장소 구현

pub mod backup;
pub mod blobs;
mod comments;
pub mod data_dir;
pub mod db;
pub mod fetch;
mod links;
pub mod mail;
pub mod mailer;
mod search;
pub mod sqlite;
pub mod tls;

use std::sync::Arc;

use maru_core::config::Config;
use maru_core::{Ctx, Result};

pub use blobs::FsBlobs;
pub use data_dir::DataDir;
pub use db::Db;
pub use sqlite::SqliteStore;

/// 초기화된 데이터 디렉터리를 열어 서비스 컨텍스트를 만든다.
pub async fn open(dir: &DataDir) -> Result<(Ctx, Config, Db)> {
    if !dir.is_initialized() {
        return Err(data_dir::not_initialized(dir.root()));
    }
    let config = dir.read_config()?;
    let db = Db::open(&dir.db_path(), false).await?;
    db.migrate().await?;
    let ctx = Ctx {
        store: Arc::new(SqliteStore::new(db.clone())),
        blobs: Arc::new(FsBlobs::new(dir.media_dir())),
        site_url: config.site_url.clone(),
        extensions: maru_core::store::no_extensions(),
    };
    Ok((ctx, config, db))
}
