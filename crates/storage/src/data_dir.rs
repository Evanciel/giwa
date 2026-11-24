//! 데이터 디렉터리 (SPEC §3.3).
//!
//! ```text
//! <data>/
//! ├─ giwa.db  config.toml
//! ├─ media/  themes/  backups/
//! ├─ mail/     파일로 남긴 편지
//! └─ secrets/  smtp-password (백업에 넣지 않는다)
//! ```

use std::fs;
use std::path::{Path, PathBuf};

use giwa_core::config::Config;
use giwa_core::{Code, Error, Result};

#[derive(Debug, Clone)]
pub struct DataDir {
    root: PathBuf,
}

impl DataDir {
    /// 이름을 바꾸기 전(ADR-0173)의 `maru.db` 가 있으면 `giwa.db` 로 옮겨 둔다(실패하면 그대로).
    pub fn new(root: impl Into<PathBuf>) -> Self {
        let dir = Self { root: root.into() };
        let _ = dir.adopt_legacy_db();
        dir
    }

    /// `giwa.db` 가 없고 예전 이름 `maru.db` 가 있으면 옮긴다(WAL·SHM 함께). 옮겼으면 true.
    pub fn adopt_legacy_db(&self) -> Result<bool> {
        let new = self.db_path();
        let old = self.root.join("maru.db");
        if new.exists() || !old.is_file() {
            return Ok(false);
        }
        for suffix in ["-wal", "-shm"] {
            let from = self.root.join(format!("maru.db{suffix}"));
            if from.exists() {
                let to = self.root.join(format!("giwa.db{suffix}"));
                fs::rename(&from, &to).map_err(|e| io_error(&from, e))?;
            }
        }
        fs::rename(&old, &new).map_err(|e| io_error(&old, e))?;
        Ok(true)
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn db_path(&self) -> PathBuf {
        self.root.join("giwa.db")
    }

    pub fn config_path(&self) -> PathBuf {
        self.root.join("config.toml")
    }

    pub fn media_dir(&self) -> PathBuf {
        self.root.join("media")
    }

    pub fn themes_dir(&self) -> PathBuf {
        self.root.join("themes")
    }

    pub fn backups_dir(&self) -> PathBuf {
        self.root.join("backups")
    }

    /// 외부 모듈(WASM) 폴더. 모듈마다 `<id>/module.toml`·`module.wasm`·`templates/` (ADR-0177).
    pub fn modules_dir(&self) -> PathBuf {
        self.root.join("modules")
    }

    /// 파일로 남긴 편지 (`FileMailer`). 1회용 링크가 들어 있다.
    pub fn mail_dir(&self) -> PathBuf {
        self.root.join("mail")
    }

    /// 백업에 넣지 않는 비밀 값 (ADR-0083)
    pub fn secrets_dir(&self) -> PathBuf {
        self.root.join("secrets")
    }

    pub fn smtp_password_path(&self) -> PathBuf {
        self.secrets_dir().join("smtp-password")
    }

    /// SMTP 비밀번호: 환경 변수 `GIWA_SMTP_PASSWORD` 가 먼저, 없으면 `secrets/smtp-password`.
    /// 돌려주는 두 번째 값은 어디서 읽었는지(`env`·`file`).
    pub fn smtp_password(&self) -> Option<(String, &'static str)> {
        if let Some(v) = giwa_core::env::var("SMTP_PASSWORD")
            && !v.is_empty()
        {
            return Some((v, "env"));
        }
        let text = fs::read_to_string(self.smtp_password_path()).ok()?;
        let v = text.trim_end_matches(['\r', '\n']).to_owned();
        (!v.is_empty()).then_some((v, "file"))
    }

    pub fn write_smtp_password(&self, password: &str) -> Result<()> {
        let dir = self.secrets_dir();
        fs::create_dir_all(&dir).map_err(|e| io_error(&dir, e))?;
        let path = self.smtp_password_path();
        fs::write(&path, password).map_err(|e| io_error(&path, e))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&path, fs::Permissions::from_mode(0o600))
                .map_err(|e| io_error(&path, e))?;
        }
        Ok(())
    }

    /// DB 파일이 있으면 초기화된 디렉터리로 본다.
    pub fn is_initialized(&self) -> bool {
        self.db_path().is_file()
    }

    /// 하위 디렉터리를 만든다. 이미 있으면 그대로 둔다.
    pub fn ensure_layout(&self) -> Result<()> {
        for dir in [
            self.root.clone(),
            self.media_dir(),
            self.themes_dir(),
            self.backups_dir(),
        ] {
            fs::create_dir_all(&dir).map_err(|e| io_error(&dir, e))?;
        }
        Ok(())
    }

    pub fn read_config(&self) -> Result<Config> {
        let path = self.config_path();
        let text = match fs::read_to_string(&path) {
            Ok(text) => text,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return Err(not_initialized(&self.root));
            }
            Err(e) => return Err(io_error(&path, e)),
        };
        let text = text.trim_start_matches('\u{feff}');
        let cfg: Config = toml::from_str(text).map_err(|e| {
            Error::new(Code::Validation)
                .with_message("error.config_parse")
                .arg("path", path.display().to_string())
                .arg("detail", e.message().to_owned())
        })?;
        cfg.validated()
    }

    /// 설정 파일이 없을 때만 쓴다. 썼으면 `true`.
    pub fn write_config_if_absent(&self, cfg: &Config) -> Result<bool> {
        if self.config_path().exists() {
            return Ok(false);
        }
        self.write_config(cfg)?;
        Ok(true)
    }

    /// 설정 파일을 통째로 다시 쓴다(검사를 통과한 값만). 주석은 매번 같은 안내로 새로 붙는다.
    pub fn write_config(&self, cfg: &Config) -> Result<()> {
        let cfg = cfg.clone().validated()?;
        let path = self.config_path();
        let body = toml::to_string_pretty(&cfg).map_err(Error::internal)?;
        let text = format!(
            "# giwa 설정. 바꾼 뒤에는 serve 를 다시 시작한다.\n\
             # bind: 서버 주소 (기본은 이 컴퓨터에서만 접속 가능)\n\
             # site_url: 사이트맵·RSS 등 절대 URL 의 기준\n\
             # trust_tunnel: 터널이 건네는 방문자 주소를 믿는다 (`giwa expose`)\n\
             # [mail]: 편지 보내는 방식 (`giwa mail setup`). SMTP 비밀번호는 여기 두지 않는다\n{body}"
        );
        fs::write(&path, text).map_err(|e| io_error(&path, e))
    }
}

pub fn not_initialized(root: &Path) -> Error {
    Error::new(Code::NotFound)
        .with_message("error.data_dir_not_initialized")
        .arg("path", root.display().to_string())
        .next(format!(
            "giwa init --data \"{}\" --preset blog --yes",
            root.display()
        ))
}

pub(crate) fn io_error(path: &Path, e: std::io::Error) -> Error {
    Error::new(Code::Internal)
        .with_message("error.io")
        .arg("path", path.display().to_string())
        .with_source(e)
}
