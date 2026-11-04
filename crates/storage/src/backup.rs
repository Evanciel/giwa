//! 백업·복원 파일 작업 (GAP S6, ADR-0063).
//!
//! 백업 파일은 `.tar.gz` 하나다: `manifest.json`, `maru.db`(`VACUUM INTO` 사본), `config.toml`,
//! `media/`, `themes/`. 복원은 서버가 꺼져 있을 때만 한다 — `serve` 가 [`lock_serve`] 로 잡는
//! `serve.lock` 을 복원도 잡아 본다.

use std::fs::{self, File, TryLockError};
use std::io::Read;
use std::path::{Component, Path, PathBuf};

use async_trait::async_trait;
use flate2::Compression;
use flate2::read::GzDecoder;
use flate2::write::GzEncoder;
use maru_core::services::backups::{Archiver, BackupInfo, check_name};
use maru_core::{Code, Error, Result, clock};
use serde::{Deserialize, Serialize};
use sqlx::Connection;
use sqlx::sqlite::{SqliteConnectOptions, SqliteConnection};

use crate::data_dir::{DataDir, io_error};
use crate::db::{Db, MIGRATOR, db_error};

/// 백업 파일 형식 판.
pub const FORMAT: u32 = 1;
/// 서버가 켜져 있는 동안 잡고 있는 잠금 파일.
pub const SERVE_LOCK: &str = "serve.lock";
/// 백업에 넣는 맨 위 이름. 복원은 이것 말고는 풀지 않는다.
const ENTRIES: [&str; 5] = ["manifest.json", "maru.db", "config.toml", "media", "themes"];
/// 비밀 빼기: 남에게 줄 사본에서 지우는 것.
const STRIP: [&str; 8] = [
    "DELETE FROM sessions",
    "DELETE FROM login_tickets",
    "DELETE FROM tokens",
    "DELETE FROM mail_outbox",
    "UPDATE members SET password_hash = NULL",
    "DELETE FROM member_totp",
    "DELETE FROM member_recovery_codes",
    "DELETE FROM totp_challenges",
];

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Manifest {
    pub format: u32,
    pub maru_version: String,
    /// 백업한 DB 의 최신 마이그레이션 번호
    pub schema: i64,
    pub created_at: String,
    pub secrets: bool,
}

pub struct FsArchiver {
    dir: DataDir,
    db: Db,
}

impl FsArchiver {
    pub fn new(dir: DataDir, db: Db) -> Self {
        Self { dir, db }
    }

    async fn build(&self, work: &Path, secrets: bool) -> Result<BackupInfo> {
        let created_at = clock::now();
        let copy = work.join("maru.db");
        // 읽기 연결 하나에서 한 시점의 사본을 만든다. 서버가 쓰는 중이어도 일관된다.
        sqlx::query("VACUUM INTO ?")
            .bind(copy.to_string_lossy().into_owned())
            .execute(&self.db.read)
            .await
            .map_err(db_error)?;
        let schema: Option<i64> =
            sqlx::query_scalar("SELECT max(version) FROM _sqlx_migrations WHERE success = 1")
                .fetch_one(&self.db.read)
                .await
                .map_err(db_error)?;
        if !secrets {
            strip(&copy).await?;
        }
        let manifest = Manifest {
            format: FORMAT,
            maru_version: env!("CARGO_PKG_VERSION").to_owned(),
            schema: schema.unwrap_or(0),
            created_at: created_at.clone(),
            secrets,
        };
        let backups = self.dir.backups_dir();
        let stamp = stamp_of(&created_at);
        let suffix = if secrets { "" } else { "-nosecrets" };
        let mut name = format!("maru-{stamp}{suffix}.tar.gz");
        let mut n = 2;
        while backups.join(&name).exists() {
            name = format!("maru-{stamp}-{n}{suffix}.tar.gz");
            n += 1;
        }
        let out = backups.join(&name);
        let dir = self.dir.clone();
        let work = work.to_owned();
        let target = out.clone();
        tokio::task::spawn_blocking(move || pack(&dir, &work, &manifest, &target))
            .await
            .map_err(Error::internal)??;
        let size = fs::metadata(&out).map_err(|e| io_error(&out, e))?.len();
        Ok(BackupInfo {
            name,
            size,
            created_at,
            secrets,
        })
    }
}

#[async_trait]
impl Archiver for FsArchiver {
    async fn create(&self, secrets: bool) -> Result<BackupInfo> {
        let backups = self.dir.backups_dir();
        fs::create_dir_all(&backups).map_err(|e| io_error(&backups, e))?;
        let work = backups.join(format!(".work-{}", clock::new_id()));
        fs::create_dir_all(&work).map_err(|e| io_error(&work, e))?;
        let res = self.build(&work, secrets).await;
        let _ = fs::remove_dir_all(&work);
        res
    }

    async fn list(&self) -> Result<Vec<BackupInfo>> {
        let backups = self.dir.backups_dir();
        let entries = match fs::read_dir(&backups) {
            Ok(e) => e,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(io_error(&backups, e)),
        };
        let mut out = Vec::new();
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            let Ok(meta) = entry.metadata() else { continue };
            if !meta.is_file() || check_name(&name).is_err() {
                continue;
            }
            out.push(BackupInfo {
                created_at: created_of(&name),
                secrets: !name.contains("-nosecrets"),
                size: meta.len(),
                name,
            });
        }
        out.sort_by(|a, b| b.name.cmp(&a.name));
        Ok(out)
    }

    async fn path_of(&self, name: &str) -> Result<Option<PathBuf>> {
        let path = self.dir.backups_dir().join(check_name(name)?);
        Ok(path.is_file().then_some(path))
    }

    async fn delete(&self, name: &str) -> Result<bool> {
        let path = self.dir.backups_dir().join(check_name(name)?);
        match fs::remove_file(&path) {
            Ok(()) => Ok(true),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
            Err(e) => Err(io_error(&path, e)),
        }
    }
}

/// `2026-11-04T09:15:00.123Z` → `20261104-091500`.
fn stamp_of(at: &str) -> String {
    let digits: String = at.chars().take(19).filter(char::is_ascii_digit).collect();
    format!(
        "{}-{}",
        &digits[..8.min(digits.len())],
        &digits[8.min(digits.len())..]
    )
}

/// 파일 이름의 시각 → RFC3339 (UTC). 모르면 빈 값.
fn created_of(name: &str) -> String {
    let s = name.trim_start_matches("maru-");
    let b = s.as_bytes();
    if b.len() < 15 || b[8] != b'-' || !b[..8].iter().chain(&b[9..15]).all(u8::is_ascii_digit) {
        return String::new();
    }
    format!(
        "{}-{}-{}T{}:{}:{}Z",
        &s[0..4],
        &s[4..6],
        &s[6..8],
        &s[9..11],
        &s[11..13],
        &s[13..15]
    )
}

async fn strip(copy: &Path) -> Result<()> {
    let opts = SqliteConnectOptions::new().filename(copy);
    let mut c = SqliteConnection::connect_with(&opts)
        .await
        .map_err(db_error)?;
    for sql in STRIP {
        sqlx::query(sql).execute(&mut c).await.map_err(db_error)?;
    }
    // 지운 값이 빈 쪽에 남지 않게 다시 쓴다.
    sqlx::query("VACUUM")
        .execute(&mut c)
        .await
        .map_err(db_error)?;
    c.close().await.map_err(db_error)
}

fn pack(dir: &DataDir, work: &Path, manifest: &Manifest, out: &Path) -> Result<()> {
    let part = out.with_extension("part");
    let io = |p: &Path| {
        let p = p.to_owned();
        move |e: std::io::Error| io_error(&p, e)
    };
    let file = File::create(&part).map_err(io(&part))?;
    let mut tar = tar::Builder::new(GzEncoder::new(file, Compression::default()));
    tar.follow_symlinks(false);
    let body = serde_json::to_vec_pretty(manifest).map_err(Error::internal)?;
    let mut header = tar::Header::new_gnu();
    header.set_size(body.len() as u64);
    header.set_mode(0o644);
    header.set_cksum();
    tar.append_data(&mut header, "manifest.json", body.as_slice())
        .map_err(io(&part))?;
    tar.append_path_with_name(work.join("maru.db"), "maru.db")
        .map_err(io(&part))?;
    let config = dir.config_path();
    if config.is_file() {
        tar.append_path_with_name(&config, "config.toml")
            .map_err(io(&config))?;
    }
    for (name, path) in [("media", dir.media_dir()), ("themes", dir.themes_dir())] {
        if path.is_dir() {
            tar.append_dir_all(name, &path).map_err(io(&path))?;
        }
    }
    let file = tar
        .into_inner()
        .map_err(io(&part))?
        .finish()
        .map_err(io(&part))?;
    file.sync_all().map_err(io(&part))?;
    drop(file);
    fs::rename(&part, out).map_err(io(out))
}

/// 서버 실행 잠금. 돌려받은 파일을 들고 있는 동안 잠겨 있다.
/// 이미 누가 잡고 있으면 `CONFLICT`(서버가 켜져 있음).
pub fn lock_serve(dir: &DataDir) -> Result<File> {
    let path = dir.root().join(SERVE_LOCK);
    let file = fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(&path)
        .map_err(|e| io_error(&path, e))?;
    match file.try_lock() {
        Ok(()) => Ok(file),
        Err(TryLockError::WouldBlock) => Err(Error::new(Code::Conflict)
            .with_message("error.server_running")
            .arg("path", dir.root().display().to_string())),
        Err(TryLockError::Error(e)) => Err(io_error(&path, e)),
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct RestoreReport {
    pub manifest: Manifest,
    /// 복원 전 데이터를 옮겨 둔 곳
    pub previous: PathBuf,
}

/// 백업 파일로 되살린다. 서버가 꺼져 있어야 한다. 지금 데이터는 `backups/before-restore-<시각>/` 로 옮긴다.
/// 끝나면 부르는 쪽이 DB 를 열어 마이그레이션한다(옛 백업이면 최신 스키마로 올라간다).
pub async fn restore(dir: &DataDir, archive: &Path) -> Result<RestoreReport> {
    let _lock = lock_serve(dir)?;
    let dir = dir.clone();
    let archive = archive.to_owned();
    tokio::task::spawn_blocking(move || restore_blocking(&dir, &archive))
        .await
        .map_err(Error::internal)?
}

fn invalid(archive: &Path, detail: impl Into<String>) -> Error {
    Error::new(Code::Validation)
        .with_message("error.backup_invalid")
        .arg("path", archive.display().to_string())
        .arg("detail", detail.into())
}

fn open_tar(archive: &Path) -> Result<tar::Archive<GzDecoder<File>>> {
    let file = File::open(archive).map_err(|e| {
        Error::new(Code::NotFound)
            .with_message("error.io")
            .arg("path", archive.display().to_string())
            .with_source(e)
    })?;
    Ok(tar::Archive::new(GzDecoder::new(file)))
}

/// 첫째 판: 이름·종류를 검사하고 manifest 를 읽는다. 아무것도 풀지 않는다.
fn inspect(archive: &Path) -> Result<Manifest> {
    let mut tar = open_tar(archive)?;
    let mut manifest = None;
    let mut has_db = false;
    for entry in tar.entries().map_err(|e| invalid(archive, e.to_string()))? {
        let mut entry = entry.map_err(|e| invalid(archive, e.to_string()))?;
        let path = entry
            .path()
            .map_err(|e| invalid(archive, e.to_string()))?
            .into_owned();
        let mut parts = path.components();
        let first = match parts.next() {
            Some(Component::Normal(s)) => s.to_string_lossy().into_owned(),
            _ => {
                return Err(invalid(
                    archive,
                    format!("허용하지 않는 경로 {}", path.display()),
                ));
            }
        };
        if !ENTRIES.contains(&first.as_str()) || !parts.all(|c| matches!(c, Component::Normal(_))) {
            return Err(invalid(
                archive,
                format!("허용하지 않는 경로 {}", path.display()),
            ));
        }
        let kind = entry.header().entry_type();
        if !(kind.is_file() || kind.is_dir()) {
            return Err(invalid(
                archive,
                format!("파일·폴더가 아닌 항목 {}", path.display()),
            ));
        }
        match first.as_str() {
            "manifest.json" => {
                let mut text = String::new();
                entry
                    .read_to_string(&mut text)
                    .map_err(|e| invalid(archive, e.to_string()))?;
                manifest = Some(
                    serde_json::from_str::<Manifest>(&text)
                        .map_err(|e| invalid(archive, e.to_string()))?,
                );
            }
            "maru.db" => has_db = true,
            _ => {}
        }
    }
    let manifest = manifest.ok_or_else(|| invalid(archive, "manifest.json 이 없습니다"))?;
    if !has_db {
        return Err(invalid(archive, "maru.db 가 없습니다"));
    }
    if manifest.format != FORMAT {
        return Err(invalid(archive, format!("모르는 형식 {}", manifest.format)));
    }
    let known = MIGRATOR.iter().map(|m| m.version).max().unwrap_or(0);
    if manifest.schema > known {
        return Err(Error::new(Code::SchemaTooNew)
            .with_message("error.backup_newer")
            .arg("schema", manifest.schema.to_string())
            .arg("known", known.to_string())
            .arg("version", manifest.maru_version.clone()));
    }
    Ok(manifest)
}

fn restore_blocking(dir: &DataDir, archive: &Path) -> Result<RestoreReport> {
    let manifest = inspect(archive)?;
    let root = dir.root();
    let previous = dir
        .backups_dir()
        .join(format!("before-restore-{}", stamp_of(&clock::now())));
    fs::create_dir_all(&previous).map_err(|e| io_error(&previous, e))?;
    for name in [
        "maru.db",
        "maru.db-wal",
        "maru.db-shm",
        "config.toml",
        "media",
        "themes",
    ] {
        let from = root.join(name);
        if from.exists() {
            fs::rename(&from, previous.join(name)).map_err(|e| io_error(&from, e))?;
        }
    }
    let mut tar = open_tar(archive)?;
    for entry in tar.entries().map_err(|e| invalid(archive, e.to_string()))? {
        let mut entry = entry.map_err(|e| invalid(archive, e.to_string()))?;
        let is_manifest = entry
            .path()
            .map(|p| p.as_os_str() == "manifest.json")
            .unwrap_or(false);
        if is_manifest {
            continue;
        }
        // unpack_in 은 데이터 디렉터리 밖으로 나가는 경로를 거부한다(첫째 판 검사와 이중).
        entry
            .unpack_in(root)
            .map_err(|e| invalid(archive, e.to_string()))?;
    }
    dir.ensure_layout()?;
    Ok(RestoreReport { manifest, previous })
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn stamps_and_names_round_trip() {
        assert_eq!(stamp_of("2026-11-04T09:15:07.123456Z"), "20261104-091507");
        assert_eq!(
            created_of("maru-20261104-091507-2-nosecrets.tar.gz"),
            "2026-11-04T09:15:07Z"
        );
        assert_eq!(created_of("maru-x.tar.gz"), "");
    }
}
