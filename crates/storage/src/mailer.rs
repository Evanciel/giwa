//! 편지 전송 방식 (ADR-0051·0083).
//!
//! - `FileMailer`: 데이터 디렉터리 `mail/` 에 .eml 파일로 남긴다(기본값). 메일 프로그램으로 열 수
//!   있는 RFC 5322 모양. 파일에는 1회용 링크가 들어 있으므로 데이터 디렉터리 밖으로 내보내지 않는다.
//! - `SmtpMailer`: 설정 `[mail.smtp]` 의 서버로 보낸다. 비밀번호는 설정 파일이 아니라
//!   `MARU_SMTP_PASSWORD` 또는 `secrets/smtp-password` 에서 읽는다.

use std::fs;
use std::io::Write;
use std::path::PathBuf;

use std::sync::Arc;
use std::time::Duration;

use lettre::message::Mailbox;
use lettre::message::header::ContentType;
use lettre::transport::smtp::authentication::Credentials;
use lettre::transport::smtp::extension::ClientId;
use lettre::{AsyncSmtpTransport, AsyncTransport, Message, Tokio1Executor};
use maru_core::config::{Config, MailTransport, SmtpConfig, SmtpSecurity};
use maru_core::model::MailOut;
use maru_core::services::mail::Mailer;
use maru_core::{Code, Error, Result};

use crate::data_dir::DataDir;

/// 연결·명령 하나를 기다리는 최대 시간
const SMTP_TIMEOUT: Duration = Duration::from_secs(20);

pub struct FileMailer {
    dir: PathBuf,
    from: String,
}

/// `site_url` 의 호스트
fn site_host(site_url: &str) -> &str {
    let host = site_url
        .split_once("://")
        .map_or(site_url, |(_, rest)| rest)
        .split(['/', ':'])
        .next()
        .unwrap_or("localhost");
    if host.is_empty() { "localhost" } else { host }
}

/// 설정의 보낸 사람, 없으면 `noreply@<site_url 호스트>`
pub fn from_address(config: &Config) -> String {
    config
        .mail
        .as_ref()
        .and_then(|m| m.from.clone())
        .unwrap_or_else(|| format!("noreply@{}", site_host(&config.site_url)))
}

/// 설정대로 전송 방식을 만든다. 돌려주는 두 번째 값은 사람에게 보여 줄 설명.
pub fn from_config(dir: &DataDir, config: &Config) -> Result<(Arc<dyn Mailer>, String)> {
    let from = from_address(config);
    match config.mail.as_ref().map(|m| (m.transport, m.smtp.as_ref())) {
        Some((MailTransport::Smtp, Some(smtp))) => {
            let password = dir.smtp_password().map(|(p, _)| p);
            let mailer = SmtpMailer::new(smtp, &from, password)?;
            let about = format!(
                "{}:{} ({}) 로 보냅니다, 보낸 사람 {from}",
                smtp.host,
                smtp.port,
                security_name(smtp.security)
            );
            Ok((Arc::new(mailer), about))
        }
        _ => {
            let mailer = FileMailer {
                dir: dir.mail_dir(),
                from,
            };
            let about = format!("파일로만 남깁니다: {}", mailer.dir.display());
            Ok((Arc::new(mailer), about))
        }
    }
}

pub fn security_name(s: SmtpSecurity) -> &'static str {
    match s {
        SmtpSecurity::Starttls => "starttls",
        SmtpSecurity::Tls => "tls",
        SmtpSecurity::None => "none",
    }
}

impl FileMailer {
    /// `site_url` 의 호스트로 `noreply@<호스트>` 보낸 사람을 만든다.
    pub fn new(dir: PathBuf, site_url: &str) -> Self {
        Self {
            dir,
            from: format!("noreply@{}", site_host(site_url)),
        }
    }

    pub fn dir(&self) -> &std::path::Path {
        &self.dir
    }
}

#[async_trait::async_trait]
impl Mailer for FileMailer {
    fn kind(&self) -> &'static str {
        "file"
    }

    async fn send(&self, m: &MailOut) -> std::result::Result<(), String> {
        fs::create_dir_all(&self.dir).map_err(|e| e.to_string())?;
        let stamp: String = m
            .created_at
            .chars()
            .filter(char::is_ascii_digit)
            .take(14)
            .collect();
        let path = self.dir.join(format!("{stamp}-{}.eml", m.id));
        let text = eml(&self.from, m);
        // 이미 있으면 (재시도) 덮어쓴다.
        let mut f = fs::File::create(&path).map_err(|e| e.to_string())?;
        f.write_all(text.as_bytes()).map_err(|e| e.to_string())
    }
}

pub struct SmtpMailer {
    transport: AsyncSmtpTransport<Tokio1Executor>,
    from: Mailbox,
}

impl SmtpMailer {
    pub fn new(cfg: &SmtpConfig, from: &str, password: Option<String>) -> Result<Self> {
        let from: Mailbox = from.parse().map_err(|_| {
            Error::new(Code::Validation)
                .with_message("error.mail_from")
                .arg("value", from)
                .next("maru mail setup --transport smtp --from noreply@your-domain.example …")
        })?;
        let tls_err = |e: lettre::transport::smtp::Error| {
            Error::new(Code::Validation)
                .with_message("error.mail_smtp_host")
                .arg("value", format!("{} ({e})", cfg.host))
        };
        let builder = match cfg.security {
            SmtpSecurity::Starttls => {
                AsyncSmtpTransport::<Tokio1Executor>::starttls_relay(&cfg.host).map_err(tls_err)?
            }
            SmtpSecurity::Tls => {
                AsyncSmtpTransport::<Tokio1Executor>::relay(&cfg.host).map_err(tls_err)?
            }
            // 설정 검사가 이 컴퓨터 안(127.0.0.1·localhost)만 허락한다
            SmtpSecurity::None => {
                AsyncSmtpTransport::<Tokio1Executor>::builder_dangerous(cfg.host.clone())
            }
        };
        let hello = from.email.domain().to_owned();
        let mut builder = builder
            .port(cfg.port)
            .timeout(Some(SMTP_TIMEOUT))
            .hello_name(ClientId::Domain(hello));
        // 비밀번호가 없으면 로그인 없이 시도한다 — 서버가 거절하면 그 이유가 편지 기록에 남는다
        if let (Some(user), Some(pw)) = (&cfg.username, password) {
            builder = builder.credentials(Credentials::new(user.clone(), pw));
        }
        Ok(Self {
            transport: builder.build(),
            from,
        })
    }
}

#[async_trait::async_trait]
impl Mailer for SmtpMailer {
    fn kind(&self) -> &'static str {
        "smtp"
    }

    async fn send(&self, m: &MailOut) -> std::result::Result<(), String> {
        let to: Mailbox = m
            .to_addr
            .parse()
            .map_err(|e| format!("받는 주소를 읽지 못했습니다: {e}"))?;
        let msg = Message::builder()
            .from(self.from.clone())
            .to(to)
            .subject(m.subject.replace(['\r', '\n'], " "))
            .message_id(Some(format!("<{}@{}>", m.id, self.from.email.domain())))
            .header(ContentType::TEXT_PLAIN)
            .body(m.body.clone())
            .map_err(|e| format!("편지를 만들지 못했습니다: {e}"))?;
        self.transport
            .send(msg)
            .await
            .map(|_| ())
            .map_err(|e| format!("SMTP 전송 실패: {e}"))
    }
}

fn eml(from: &str, m: &MailOut) -> String {
    let date = maru_core::clock::rfc2822(&m.created_at);
    let body = m.body.replace("\r\n", "\n").replace('\n', "\r\n");
    format!(
        "From: {from}\r\nTo: {to}\r\nSubject: =?UTF-8?B?{subject}?=\r\nDate: {date}\r\n\
         Message-ID: <{id}@maru>\r\nMIME-Version: 1.0\r\n\
         Content-Type: text/plain; charset=utf-8\r\nContent-Transfer-Encoding: 8bit\r\n\r\n{body}",
        to = m.to_addr.replace(['\r', '\n'], ""),
        subject = base64(m.subject.replace(['\r', '\n'], " ").as_bytes()),
        id = m.id,
    )
}

/// 표준 base64 (RFC 4648, 패딩 있음). 제목 인코딩(RFC 2047)에만 쓴다.
fn base64(input: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(input.len().div_ceil(3) * 4);
    for chunk in input.chunks(3) {
        let b = [
            chunk[0],
            chunk.get(1).copied().unwrap_or(0),
            chunk.get(2).copied().unwrap_or(0),
        ];
        let n = (u32::from(b[0]) << 16) | (u32::from(b[1]) << 8) | u32::from(b[2]);
        for i in 0..4 {
            if i <= chunk.len() {
                out.push(T[((n >> (18 - 6 * i)) & 63) as usize] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64_matches_rfc4648() {
        assert_eq!(base64(b""), "");
        assert_eq!(base64(b"f"), "Zg==");
        assert_eq!(base64(b"fo"), "Zm8=");
        assert_eq!(base64(b"foo"), "Zm9v");
        assert_eq!(base64(b"foobar"), "Zm9vYmFy");
        assert_eq!(base64("가".as_bytes()), "6rCA");
    }

    #[test]
    fn from_uses_site_host() {
        let m = FileMailer::new(PathBuf::from("x"), "https://blog.example.test:8443/a");
        assert_eq!(m.from, "noreply@blog.example.test");
    }
}
