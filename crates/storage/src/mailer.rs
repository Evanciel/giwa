//! 편지를 데이터 디렉터리 `mail/` 에 .eml 파일로 남기는 전송 방식 (ADR-0051).
//!
//! SMTP 를 붙이기 전까지의 기본값. 메일 프로그램으로 열 수 있는 RFC 5322 모양으로 쓴다.
//! 파일에는 1회용 링크가 들어 있으므로 데이터 디렉터리 밖으로 내보내지 않는다.

use std::fs;
use std::io::Write;
use std::path::PathBuf;

use maru_core::model::MailOut;
use maru_core::services::mail::Mailer;

pub struct FileMailer {
    dir: PathBuf,
    from: String,
}

impl FileMailer {
    /// `site_url` 의 호스트로 `noreply@<호스트>` 보낸 사람을 만든다.
    pub fn new(dir: PathBuf, site_url: &str) -> Self {
        let host = site_url
            .split_once("://")
            .map_or(site_url, |(_, rest)| rest)
            .split(['/', ':'])
            .next()
            .unwrap_or("localhost");
        let host = if host.is_empty() { "localhost" } else { host };
        Self {
            dir,
            from: format!("noreply@{host}"),
        }
    }

    pub fn dir(&self) -> &std::path::Path {
        &self.dir
    }
}

impl Mailer for FileMailer {
    fn kind(&self) -> &'static str {
        "file"
    }

    fn send(&self, m: &MailOut) -> Result<(), String> {
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
