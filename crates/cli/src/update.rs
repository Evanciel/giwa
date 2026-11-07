//! `maru update` (SPEC §10, ADR-0132). 새 판을 확인하고, `--yes` 를 주면 바이너리를 바꾼다.
//!
//! 릴리스의 `VERSION`, `SHA256SUMS`, `maru-<대상>` 을 받는다. 체크섬이 맞고 받은 파일이 실행되어야 바꾼다.
//! 서명은 아직 없다: 서명 키가 생기기 전까지 체크섬은 내려받는 중의 손상만 잡고, 릴리스 저장소가
//! 뚫린 경우는 막지 못한다. 그 사실을 결과의 `signature_checked: false` 로 그대로 알린다.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use maru_core::{Code, Error, Result};
use serde_json::json;
use sha2::{Digest, Sha256};

use crate::UpdateArgs;
use crate::commands::Out;

const DEFAULT_BASE: &str = "https://github.com/OWNER/maru/releases/latest/download";
const MAX_BINARY: usize = 200 * 1024 * 1024;
const TIMEOUT: Duration = Duration::from_secs(600);

fn fail(detail: impl Into<String>, next: &str) -> Error {
    Error::new(Code::Validation)
        .with_message("error.usage_detail")
        .arg("detail", detail.into())
        .next(next)
}

/// 이 바이너리의 릴리스 이름. 없는 플랫폼은 직접 빌드한다.
fn target() -> Option<&'static str> {
    match (std::env::consts::OS, std::env::consts::ARCH) {
        ("linux", "x86_64") => Some("x86_64-unknown-linux-musl"),
        ("linux", "aarch64") => Some("aarch64-unknown-linux-musl"),
        ("macos", "x86_64") => Some("x86_64-apple-darwin"),
        ("macos", "aarch64") => Some("aarch64-apple-darwin"),
        ("windows", "x86_64") => Some("x86_64-pc-windows-msvc"),
        ("windows", "aarch64") => Some("aarch64-pc-windows-msvc"),
        _ => None,
    }
}

fn asset_name(target: &str) -> String {
    let ext = if cfg!(windows) { ".exe" } else { "" };
    format!("maru-{target}{ext}")
}

/// 받을 곳. 공개 저장소가 정해지기 전에는 기본 주소가 비어 있는 것과 같다.
fn base_url(flag: Option<String>) -> Result<String> {
    let base = flag
        .or_else(|| std::env::var("MARU_BASE_URL").ok())
        .unwrap_or_else(|| DEFAULT_BASE.to_owned());
    let base = base.trim().trim_end_matches('/').to_owned();
    if base.contains("/OWNER/") {
        return Err(fail(
            "이 판에는 공개 릴리스 주소가 정해져 있지 않습니다.",
            "maru update --base-url https://<릴리스 주소>",
        ));
    }
    let loopback = ["http://127.0.0.1:", "http://localhost:", "http://[::1]:"]
        .iter()
        .any(|p| base.starts_with(p));
    if !base.starts_with("https://") && !loopback {
        return Err(fail(
            "릴리스 주소는 https 여야 합니다.",
            "maru update --base-url https://<릴리스 주소>",
        ));
    }
    Ok(base)
}

fn parse_version(v: &str) -> Option<(u64, u64, u64)> {
    let v = v.trim().trim_start_matches('v');
    let core = v.split(['-', '+']).next()?;
    let mut parts = core.split('.').map(|p| p.parse::<u64>().ok());
    Some((parts.next()??, parts.next()??, parts.next()??))
}

/// `sha256sum` 형식에서 이름에 맞는 값.
fn checksum_of(sums: &str, name: &str) -> Option<String> {
    sums.lines().find_map(|line| {
        let (sum, file) = line.split_once(char::is_whitespace)?;
        let file = file.trim().trim_start_matches('*');
        (file == name && sum.len() == 64 && sum.chars().all(|c| c.is_ascii_hexdigit()))
            .then(|| sum.to_ascii_lowercase())
    })
}

async fn fetch(client: &reqwest::Client, url: &str, limit: usize) -> Result<Vec<u8>> {
    let failed = |e: String| fail(format!("{url} 를 받지 못했습니다: {e}"), "maru update");
    let mut res = client
        .get(url)
        .send()
        .await
        .map_err(|e| failed(e.to_string()))?;
    if !res.status().is_success() {
        return Err(failed(res.status().to_string()));
    }
    let mut body = Vec::new();
    while let Some(chunk) = res.chunk().await.map_err(|e| failed(e.to_string()))? {
        if body.len() + chunk.len() > limit {
            return Err(failed("크기가 너무 큽니다".to_owned()));
        }
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}

/// 받은 파일이 이 플랫폼에서 돌고 `maru` 인지 본다.
fn runs_as_maru(path: &Path) -> bool {
    Command::new(path)
        .arg("--version")
        .output()
        .is_ok_and(|o| o.status.success() && String::from_utf8_lossy(&o.stdout).contains("maru"))
}

fn sibling(exe: &Path, suffix: &str) -> PathBuf {
    let mut name = exe.file_name().unwrap_or_default().to_os_string();
    name.push(suffix);
    exe.with_file_name(name)
}

/// 돌고 있는 파일은 지울 수 없어도 이름은 바꿀 수 있다. 실패하면 되돌린다.
fn swap(exe: &Path, new: &Path) -> std::io::Result<()> {
    let old = sibling(exe, ".old");
    let _ = std::fs::remove_file(&old);
    std::fs::rename(exe, &old)?;
    if let Err(e) = std::fs::rename(new, exe) {
        let _ = std::fs::rename(&old, exe);
        return Err(e);
    }
    Ok(())
}

/// 받은 바이너리를 옆에 쓰고, 돌려 보고, 바꾼다.
fn install(exe: &Path, new: &Path, binary: &[u8]) -> std::io::Result<bool> {
    std::fs::write(new, binary)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(new, std::fs::Permissions::from_mode(0o755))?;
    }
    if !runs_as_maru(new) {
        return Ok(false);
    }
    swap(exe, new)?;
    Ok(true)
}

pub(crate) async fn run(args: UpdateArgs) -> Out {
    let current = env!("CARGO_PKG_VERSION");
    let base = base_url(args.base_url)?;
    maru_storage::tls::install_provider();
    let client = reqwest::Client::builder()
        .timeout(TIMEOUT)
        .user_agent(concat!("maru/", env!("CARGO_PKG_VERSION")))
        .build()
        .map_err(Error::internal)?;

    let latest_raw = fetch(&client, &format!("{base}/VERSION"), 64).await?;
    let latest = String::from_utf8_lossy(&latest_raw).trim().to_owned();
    let (Some(have), Some(want)) = (parse_version(current), parse_version(&latest)) else {
        return Err(fail(
            format!("릴리스의 VERSION 을 읽지 못했습니다: {latest}"),
            "maru update",
        ));
    };
    if want <= have {
        let human = format!("최신 판입니다 ({current}).");
        return Ok((
            json!({ "current": current, "latest": latest, "update_available": false, "updated": false }),
            human,
        ));
    }
    if !args.yes {
        let human = format!(
            "새 판이 있습니다: {current} -> {latest}\n바꾸려면: maru update --yes\n먼저 백업을 권합니다: maru backup"
        );
        return Ok((
            json!({
                "current": current, "latest": latest, "update_available": true, "updated": false,
                "next": ["maru backup", "maru update --yes"],
            }),
            human,
        ));
    }

    let target = target().ok_or_else(|| {
        fail(
            "이 플랫폼용 릴리스가 없습니다. 소스에서 빌드해 주세요.",
            "cargo build --release -p maru-cli",
        )
    })?;
    let name = asset_name(target);
    let sums_raw = fetch(&client, &format!("{base}/SHA256SUMS"), 64 * 1024).await?;
    let sums = String::from_utf8_lossy(&sums_raw);
    let want_sum = checksum_of(&sums, &name)
        .ok_or_else(|| fail(format!("SHA256SUMS 에 {name} 이 없습니다."), "maru update"))?;
    let binary = fetch(&client, &format!("{base}/{name}"), MAX_BINARY).await?;
    let got_sum = hex::encode(Sha256::digest(&binary));
    if got_sum != want_sum {
        return Err(fail(
            format!("{name} 의 체크섬이 맞지 않아 바꾸지 않았습니다."),
            "maru update --yes",
        ));
    }

    let exe = std::env::current_exe().map_err(Error::internal)?;
    let new = sibling(&exe, &format!(".new-{}.exe", std::process::id()));
    if !install(&exe, &new, &binary).unwrap_or(false) {
        let _ = std::fs::remove_file(&new);
        return Err(fail(
            "새 바이너리를 놓거나 실행하지 못해 지금 판을 그대로 두었습니다.",
            "maru doctor",
        ));
    }
    let human = format!(
        "{current} -> {latest} 로 바꿨습니다. 돌고 있는 서버는 다시 시작해야 새 판이 됩니다.\n체크섬은 확인했지만 서명은 아직 확인하지 않습니다."
    );
    Ok((
        json!({
            "current": current,
            "latest": latest,
            "update_available": true,
            "updated": true,
            "sha256": got_sum,
            "signature_checked": false,
            "previous": sibling(&exe, ".old"),
            "next": ["maru service stop", "maru service start"],
        }),
        human,
    ))
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn 판은_숫자로_비교한다() {
        assert!(parse_version("v0.10.0").unwrap() > parse_version("0.9.9").unwrap());
        assert_eq!(parse_version("1.2.3-beta"), Some((1, 2, 3)));
        for bad in ["", "1.2", "a.b.c", "<html>"] {
            assert_eq!(parse_version(bad), None, "{bad}");
        }
    }

    #[test]
    fn 체크섬은_이름이_같은_줄만_쓴다() {
        let a = "a".repeat(64);
        let b = "B".repeat(64);
        let sums = format!("{a}  maru-x.zip\n{b} *maru-x\nshort maru-y\n");
        assert_eq!(checksum_of(&sums, "maru-x.zip").unwrap(), a);
        assert_eq!(checksum_of(&sums, "maru-x").unwrap(), "b".repeat(64));
        assert_eq!(checksum_of(&sums, "maru-y"), None);
        assert_eq!(checksum_of(&sums, "maru"), None);
    }

    #[test]
    fn 릴리스_주소는_https_이거나_이_컴퓨터다() {
        assert_eq!(
            base_url(Some("https://example.test/dl/".into())).unwrap(),
            "https://example.test/dl"
        );
        assert!(base_url(Some("http://127.0.0.1:3950".into())).is_ok());
        for bad in [
            "http://example.test",
            "ftp://x",
            "http://127.0.0.1.evil.test",
        ] {
            assert!(base_url(Some(bad.into())).is_err(), "{bad}");
        }
        assert!(base_url(Some(DEFAULT_BASE.into())).is_err());
    }
}
