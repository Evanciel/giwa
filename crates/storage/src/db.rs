//! SQLite 연결과 마이그레이션 (PHASE1_PLAN §4.4).
//!
//! - WAL, `foreign_keys=ON`, `busy_timeout`
//! - 쓰기 풀 1개 + 읽기 풀 여러 개. 쓰기는 `BEGIN IMMEDIATE`로 시작한다 (`WriteTx`).
//! - 마이그레이션은 바이너리에 내장한다.

use std::path::Path;
use std::str::FromStr;
use std::time::Duration;

use maru_core::{Code, Error, Result};
use sha2::{Digest, Sha384};
use sqlx::ConnectOptions;
use sqlx::migrate::{MigrateError, Migrator};
use sqlx::sqlite::{
    SqliteConnectOptions, SqliteJournalMode, SqlitePool, SqlitePoolOptions, SqliteSynchronous,
};

pub static MIGRATOR: Migrator = sqlx::migrate!();

/// 줄 끝만 CRLF 인 사본으로 빌드한 바이너리가 적용한 마이그레이션은 체크섬이 달라 `VersionMismatch` 가 난다
/// (ADR-0150). 저장된 체크섬이 내장 SQL 의 줄 끝을 CRLF 로 바꾼 체크섬과 같은 행만 내장 체크섬으로 고친다.
/// 내용은 줄 끝 말고 같으므로 다시 실행하지 않는다. 마이그레이션 표가 없으면(새 DB) 할 일이 없다.
async fn repair_line_endings(pool: &SqlitePool) -> Result<()> {
    let table: Option<String> = sqlx::query_scalar(
        "SELECT name FROM sqlite_master WHERE type = 'table' AND name = '_sqlx_migrations'",
    )
    .fetch_optional(pool)
    .await
    .map_err(db_error)?;
    if table.is_none() {
        return Ok(());
    }
    for m in MIGRATOR
        .iter()
        .filter(|m| !m.migration_type.is_down_migration())
    {
        let sql = m.sql.as_str();
        if sql.contains("\r\n") || !sql.contains('\n') {
            continue;
        }
        let crlf = Sha384::digest(sql.replace('\n', "\r\n").as_bytes());
        sqlx::query("UPDATE _sqlx_migrations SET checksum = ? WHERE version = ? AND checksum = ?")
            .bind(m.checksum.as_ref())
            .bind(m.version)
            .bind(crlf.as_slice())
            .execute(pool)
            .await
            .map_err(db_error)?;
    }
    Ok(())
}

/// 다른 프로세스(서버 실행 중 CLI)가 쓰기 잠금을 쥐고 있을 때 기다리는 시간.
const BUSY_TIMEOUT: Duration = Duration::from_secs(10);
const READ_POOL_SIZE: u32 = 4;

#[derive(Debug, Clone)]
pub struct Db {
    pub(crate) write: SqlitePool,
    pub(crate) read: SqlitePool,
}

impl Db {
    /// DB를 연다. `create`가 `false`이면 파일이 없을 때 실패한다.
    pub async fn open(path: &Path, create: bool) -> Result<Self> {
        let opts = SqliteConnectOptions::from_str("sqlite:")
            .map_err(db_error)?
            .filename(path)
            .create_if_missing(create)
            .journal_mode(SqliteJournalMode::Wal)
            .synchronous(SqliteSynchronous::Normal)
            .foreign_keys(true)
            .busy_timeout(BUSY_TIMEOUT)
            // sqlx 는 1초 넘는 문장을 warn 으로 남긴다. 바쁜 PC 에서는 그 줄이 stderr 로 새어
            // `--json` 계약(성공하면 stderr 는 비어 있다)을 깨므로 문장 로그를 끈다.
            .disable_statement_logging();
        let write = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(opts.clone())
            .await
            .map_err(db_error)?;
        let read = SqlitePoolOptions::new()
            .max_connections(READ_POOL_SIZE)
            .connect_with(opts.read_only(true))
            .await
            .map_err(db_error)?;
        Ok(Self { write, read })
    }

    /// 내장 마이그레이션을 적용한다. DB가 바이너리보다 새로우면 `SCHEMA_TOO_NEW`.
    /// 이어서 검색 색인 버전을 확인해 자르는 규칙이 바뀌었으면 다시 만든다 (ADR-0050).
    pub async fn migrate(&self) -> Result<()> {
        repair_line_endings(&self.write).await?;
        MIGRATOR.run(&self.write).await.map_err(migrate_error)?;
        crate::search::ensure(&self.write).await
    }

    /// 지정 버전까지 되돌린다. 테스트와 복구용.
    pub async fn undo(&self, target: i64) -> Result<()> {
        MIGRATOR
            .undo(&self.write, target)
            .await
            .map_err(migrate_error)
    }

    /// 열린 연결을 모두 닫는다. Windows에서 파일을 지우기 전에 부른다.
    pub async fn close(&self) {
        self.write.close().await;
        self.read.close().await;
    }

    /// `PRAGMA quick_check` 결과. 정상이면 `ok`.
    pub async fn quick_check(&self) -> Result<String> {
        sqlx::query_scalar::<_, String>("PRAGMA quick_check")
            .fetch_one(&self.read)
            .await
            .map_err(db_error)
    }

    /// (DB 에 적용된 최신 마이그레이션, 바이너리가 아는 최신 마이그레이션)
    pub async fn schema_versions(&self) -> Result<(i64, i64)> {
        let applied: Option<i64> =
            sqlx::query_scalar("SELECT max(version) FROM _sqlx_migrations WHERE success = 1")
                .fetch_one(&self.read)
                .await
                .map_err(db_error)?;
        let known = MIGRATOR.iter().map(|m| m.version).max().unwrap_or(0);
        Ok((applied.unwrap_or(0), known))
    }

    pub fn write_pool(&self) -> &SqlitePool {
        &self.write
    }

    pub fn read_pool(&self) -> &SqlitePool {
        &self.read
    }
}

pub(crate) fn db_error(e: sqlx::Error) -> Error {
    Error::internal(e)
}

fn migrate_error(e: MigrateError) -> Error {
    match e {
        MigrateError::VersionMissing(version) => Error::new(Code::SchemaTooNew)
            .arg("version", version.to_string())
            .with_hint("hint.schema_too_new")
            .next("maru --version"),
        other => Error::internal(other),
    }
}
