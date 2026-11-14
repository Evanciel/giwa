//! 백업 (GAP S6, ADR-0063). 파일 작업은 [`Archiver`] 구현(storage)이 하고, 여기서는 권한·이름·감사를 맡는다.
//! 복원은 서버가 꺼진 상태에서 CLI 만 한다(`giwa restore`, storage 의 `backup::restore`).

use std::path::PathBuf;

use async_trait::async_trait;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::actor::{Actor, Permission};
use crate::audit::Target;
use crate::error::{Code, Error, Result};
use crate::store::{Ctx, Tx};

/// 백업 파일 하나.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct BackupInfo {
    /// 파일 이름 (`maru-20261104-091500.tar.gz`, 비밀을 뺐으면 `-nosecrets`)
    pub name: String,
    pub size: u64,
    /// 만든 시각 (RFC3339, UTC)
    pub created_at: String,
    /// 세션·토큰·비밀번호 해시·로그인 링크·보낼 편지가 들어 있나
    pub secrets: bool,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct NewBackup {
    /// 참이면 세션·토큰·비밀번호 해시·로그인 링크·보낼 편지를 뺀다(남에게 줄 사본용).
    /// 뺀 백업으로 되살리면 모두 로그인 링크로 다시 들어와야 한다
    #[serde(default)]
    pub no_secrets: bool,
}

/// 백업 파일을 만들고 읽는 쪽 (데이터 디렉터리의 `backups/`).
#[async_trait]
pub trait Archiver: Send + Sync {
    async fn create(&self, secrets: bool) -> Result<BackupInfo>;
    /// 최신 순.
    async fn list(&self) -> Result<Vec<BackupInfo>>;
    /// 이름이 가리키는 파일 경로 (없으면 `None`). 이름은 [`check_name`] 을 지난 값이다.
    async fn path_of(&self, name: &str) -> Result<Option<PathBuf>>;
    /// 지웠으면 참.
    async fn delete(&self, name: &str) -> Result<bool>;
}

/// `maru-YYYYMMDD-HHMMSS[-N][-nosecrets].tar.gz` 만 받는다(경로 조각·숨은 파일 없음).
pub fn check_name(name: &str) -> Result<&str> {
    let ok = name.len() <= 64
        && (name.starts_with("giwa-") || name.starts_with("maru-"))
        && name.ends_with(".tar.gz")
        && name
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-' || b == b'.')
        && !name.contains("..");
    if ok { Ok(name) } else { Err(not_found(name)) }
}

fn not_found(name: &str) -> Error {
    Error::new(Code::NotFound)
        .with_message("error.backup_not_found")
        .arg("name", name.chars().take(64).collect::<String>())
        .next("giwa backup list")
}

/// 지금 백업한다. 운영자만. 서버가 켜져 있어도 DB 는 한 시점의 사본이다(`VACUUM INTO`).
pub async fn create(
    ctx: &Ctx,
    actor: &Actor,
    archiver: &dyn Archiver,
    input: NewBackup,
) -> Result<BackupInfo> {
    actor.require(Permission::SiteAdmin)?;
    let info = archiver.create(!input.no_secrets).await?;
    let mut tx = Tx::begin(ctx).await?;
    tx.audit(
        actor,
        "backup.create",
        Target::new("backup", info.name.clone()),
        json!({ "after": { "name": info.name, "size": info.size, "secrets": info.secrets } }),
    )
    .await?;
    tx.commit().await?;
    Ok(info)
}

pub async fn list(actor: &Actor, archiver: &dyn Archiver) -> Result<Vec<BackupInfo>> {
    actor.require(Permission::SiteAdmin)?;
    archiver.list().await
}

/// 내려받을 파일 경로.
pub async fn file(actor: &Actor, archiver: &dyn Archiver, name: &str) -> Result<PathBuf> {
    actor.require(Permission::SiteAdmin)?;
    let name = check_name(name)?;
    archiver.path_of(name).await?.ok_or_else(|| not_found(name))
}

pub async fn delete(ctx: &Ctx, actor: &Actor, archiver: &dyn Archiver, name: &str) -> Result<()> {
    actor.require(Permission::SiteAdmin)?;
    let name = check_name(name)?;
    if !archiver.delete(name).await? {
        return Err(not_found(name));
    }
    let mut tx = Tx::begin(ctx).await?;
    tx.audit(
        actor,
        "backup.delete",
        Target::new("backup", name.to_owned()),
        json!({ "before": { "name": name } }),
    )
    .await?;
    tx.commit().await?;
    Ok(())
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn only_backup_names_pass() {
        for ok in [
            "giwa-20261114-091500.tar.gz",
            "maru-20261104-091500.tar.gz",
            "maru-20261104-091500-2-nosecrets.tar.gz",
        ] {
            assert_eq!(check_name(ok).unwrap(), ok);
        }
        for bad in [
            "../giwa.db",
            "maru-../../x.tar.gz",
            "maru-1.tar",
            "x.tar.gz",
            "maru-A.tar.gz",
            "maru-1/../../x.tar.gz",
            "",
        ] {
            assert_eq!(check_name(bad).unwrap_err().code(), Code::NotFound, "{bad}");
        }
    }
}
