//! 데이터 디렉터리의 `config.toml` 내용. 파일 읽기·쓰기는 `storage`가 한다.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::error::{Code, Error, Result};

pub const DEFAULT_BIND: &str = "127.0.0.1:8080";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Config {
    /// 서버 바인딩 주소. 기본은 로컬 전용 (CLAUDE.md 원칙 6).
    pub bind: String,
    /// 절대 URL의 기준. `Host` 헤더는 쓰지 않는다 (ADR-0011).
    pub site_url: String,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            bind: DEFAULT_BIND.to_owned(),
            site_url: format!("http://{DEFAULT_BIND}"),
        }
    }
}

impl Config {
    /// 형식 검사와 정규화(끝의 `/` 제거).
    pub fn validated(mut self) -> Result<Self> {
        let url = self.site_url.trim().trim_end_matches('/').to_owned();
        let rest = url
            .strip_prefix("https://")
            .or_else(|| url.strip_prefix("http://"));
        match rest {
            Some(host) if !host.is_empty() && !host.contains(char::is_whitespace) => {}
            _ => {
                return Err(Error::new(Code::Validation)
                    .with_message("error.config_site_url")
                    .arg("value", self.site_url));
            }
        }
        self.site_url = url;
        if self.bind.parse::<std::net::SocketAddr>().is_err() {
            return Err(Error::new(Code::Validation)
                .with_message("error.config_bind")
                .arg("value", self.bind));
        }
        Ok(self)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_is_local_only() {
        let cfg = Config::default().validated().unwrap();
        assert_eq!(cfg.bind, "127.0.0.1:8080");
        assert_eq!(cfg.site_url, "http://127.0.0.1:8080");
    }

    #[test]
    fn trailing_slash_is_removed() {
        let cfg = Config {
            site_url: "https://blog.example.com/".into(),
            ..Config::default()
        };
        assert_eq!(
            cfg.validated().unwrap().site_url,
            "https://blog.example.com"
        );
    }

    #[test]
    fn rejects_bad_values() {
        for url in ["blog.example.com", "ftp://x", "https://", "http://a b"] {
            let cfg = Config {
                site_url: url.into(),
                ..Config::default()
            };
            assert_eq!(
                cfg.validated().unwrap_err().code(),
                Code::Validation,
                "{url}"
            );
        }
        let cfg = Config {
            bind: "localhost".into(),
            ..Config::default()
        };
        assert_eq!(cfg.validated().unwrap_err().code(), Code::Validation);
    }
}
