//! 데이터 디렉터리 (SPEC §3.3).
//!
//! ```text
//! <data>/
//! ├─ maru.db  config.toml
//! ├─ media/  themes/  backups/
//! ```

use std::fs;
use std::path::{Path, PathBuf};

use maru_core::config::Config;
use maru_core::{Code, Error, Result};

#[derive(Debug, Clone)]
pub struct DataDir {
    root: PathBuf,
}

impl DataDir {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn db_path(&self) -> PathBuf {
        self.root.join("maru.db")
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

    /// 파일로 남긴 편지 (`FileMailer`). 1회용 링크가 들어 있다.
    pub fn mail_dir(&self) -> PathBuf {
        self.root.join("mail")
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
        let path = self.config_path();
        if path.exists() {
            return Ok(false);
        }
        let body = toml::to_string_pretty(cfg).map_err(Error::internal)?;
        let text = format!(
            "# maru 설정. 바꾼 뒤에는 serve 를 다시 시작한다.\n\
             # bind: 서버 주소 (기본은 이 컴퓨터에서만 접속 가능)\n\
             # site_url: 사이트맵·RSS 등 절대 URL 의 기준\n{body}"
        );
        fs::write(&path, text).map_err(|e| io_error(&path, e))?;
        Ok(true)
    }
}

pub fn not_initialized(root: &Path) -> Error {
    Error::new(Code::NotFound)
        .with_message("error.data_dir_not_initialized")
        .arg("path", root.display().to_string())
        .next(format!(
            "maru init --data \"{}\" --preset blog --yes",
            root.display()
        ))
}

pub(crate) fn io_error(path: &Path, e: std::io::Error) -> Error {
    Error::new(Code::Internal)
        .with_message("error.io")
        .arg("path", path.display().to_string())
        .with_source(e)
}
