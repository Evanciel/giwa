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
    /// 편지 보내는 방식. 없으면 파일로만 남긴다 (ADR-0051·0083).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mail: Option<MailConfig>,
    /// 루프백 연결의 `CF-Connecting-IP` 를 방문자 주소로 믿는다. `maru expose` 가 켠다 (ADR-0118).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub trust_tunnel: bool,
}

/// `[mail]` — 편지 전송 설정. 비밀번호는 여기 두지 않는다(백업 파일에 설정이 들어간다).
/// 비밀번호는 환경 변수 `MARU_SMTP_PASSWORD` 또는 데이터 디렉터리 `secrets/smtp-password`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct MailConfig {
    pub transport: MailTransport,
    /// 보낸 사람 주소. 없으면 `noreply@<site_url 호스트>`
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub smtp: Option<SmtpConfig>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum MailTransport {
    File,
    Smtp,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SmtpConfig {
    pub host: String,
    pub port: u16,
    pub security: SmtpSecurity,
    /// 로그인 이름. 없으면 로그인하지 않는다(같은 컴퓨터의 중계 서버 등)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub username: Option<String>,
}

/// `starttls`(보통 587) · `tls`(보통 465) · `none`(암호화 없음 — 이 컴퓨터 안의 중계 서버만)
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum SmtpSecurity {
    Starttls,
    Tls,
    None,
}

impl SmtpSecurity {
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "starttls" => Some(Self::Starttls),
            "tls" => Some(Self::Tls),
            "none" => Some(Self::None),
            _ => None,
        }
    }
}

/// 암호화 없이 보내도 되는 곳: 이 컴퓨터 안
pub fn is_loopback_host(host: &str) -> bool {
    let h = host.trim_start_matches('[').trim_end_matches(']');
    h.eq_ignore_ascii_case("localhost")
        || h.parse::<std::net::IpAddr>()
            .is_ok_and(|ip| ip.is_loopback())
}

/// 아주 느슨한 주소 모양 검사: `이름@호스트.점` , 공백·꺾쇠·줄바꿈 없음
pub fn looks_like_email(s: &str) -> bool {
    let Some((local, host)) = s.split_once('@') else {
        return false;
    };
    !local.is_empty()
        && host.contains('.')
        && !host.starts_with('.')
        && !host.ends_with('.')
        && !s
            .chars()
            .any(|c| c.is_whitespace() || c.is_control() || "<>\"\\,;()".contains(c))
        && !host.contains('@')
}

impl Default for Config {
    fn default() -> Self {
        Self {
            bind: DEFAULT_BIND.to_owned(),
            site_url: format!("http://{DEFAULT_BIND}"),
            mail: None,
            trust_tunnel: false,
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
        if let Some(mail) = &self.mail {
            mail.check()?;
        }
        Ok(self)
    }
}

impl MailConfig {
    pub fn check(&self) -> Result<()> {
        let bad = |key: &'static str| Error::new(Code::Validation).with_message(key);
        if let Some(from) = &self.from
            && !looks_like_email(from)
        {
            return Err(bad("error.mail_from").arg("value", from.clone()));
        }
        match (self.transport, &self.smtp) {
            (MailTransport::File, _) => Ok(()),
            (MailTransport::Smtp, None) => Err(bad("error.mail_smtp_missing")),
            (MailTransport::Smtp, Some(s)) => {
                if s.host.trim().is_empty()
                    || s.host.contains(|c: char| c.is_whitespace() || c == '/')
                {
                    return Err(bad("error.mail_smtp_host").arg("value", s.host.clone()));
                }
                if s.port == 0 {
                    return Err(bad("error.mail_smtp_port"));
                }
                // 암호화 없는 연결로 비밀번호·편지를 밖으로 보내지 않는다
                if s.security == SmtpSecurity::None && !is_loopback_host(&s.host) {
                    return Err(bad("error.mail_smtp_plain").arg("host", s.host.clone()));
                }
                Ok(())
            }
        }
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

    fn smtp(host: &str, security: SmtpSecurity) -> MailConfig {
        MailConfig {
            transport: MailTransport::Smtp,
            from: Some("noreply@example.test".into()),
            smtp: Some(SmtpConfig {
                host: host.into(),
                port: 587,
                security,
                username: None,
            }),
        }
    }

    #[test]
    fn mail_config_rules() {
        assert!(
            smtp("smtp.example.test", SmtpSecurity::Starttls)
                .check()
                .is_ok()
        );
        // 암호화 없음은 이 컴퓨터 안의 중계 서버만
        assert!(smtp("127.0.0.1", SmtpSecurity::None).check().is_ok());
        assert!(smtp("localhost", SmtpSecurity::None).check().is_ok());
        assert!(
            smtp("smtp.example.test", SmtpSecurity::None)
                .check()
                .is_err()
        );
        assert!(smtp("a b", SmtpSecurity::Tls).check().is_err());
        let mut m = smtp("smtp.example.test", SmtpSecurity::Tls);
        m.from = Some("이름 <a@b.test>".into());
        assert!(m.check().is_err());
        m.smtp = None;
        m.from = None;
        assert!(m.check().is_err());
        m.transport = MailTransport::File;
        assert!(m.check().is_ok());
    }
}
