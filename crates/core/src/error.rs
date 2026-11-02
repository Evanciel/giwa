//! 도메인 에러. REST, CLI, MCP가 같은 `code`를 쓴다.
//!
//! - `code`는 안정된 계약이다. 이름을 바꾸면 에이전트가 깨진다.
//! - 사용자에게 보이는 문장은 메시지 키로 만든다 (`crate::messages`).
//! - 원인(`source`)은 로그로만 보내고 응답 본문에는 넣지 않는다.

use std::collections::BTreeMap;
use std::fmt;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::messages;

/// 안정된 에러 코드. 직렬화 이름이 곧 계약이다.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum Code {
    /// 예상하지 못한 내부 오류. 상세 원인은 로그에만 남는다.
    Internal,
    /// 명령이나 인자를 잘못 썼다.
    Usage,
    /// 입력 값이 규칙에 맞지 않는다.
    Validation,
    /// 대상이 없다.
    NotFound,
    /// 인증이 필요하거나 인증 정보가 틀렸다.
    Unauthorized,
    /// 인증은 됐지만 권한이 없다.
    Forbidden,
    /// 이미 존재하거나 현재 상태와 충돌한다.
    Conflict,
    /// 포트를 다른 프로그램이 쓰고 있다.
    PortInUse,
    /// DB 스키마가 이 바이너리보다 새롭다.
    SchemaTooNew,
    /// 요청이 너무 잦다.
    RateLimited,
    /// 외부 서비스·네트워크 연결에 실패했다.
    External,
}

/// SPEC §10.1 종료 코드.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum ExitCode {
    Success = 0,
    General = 1,
    Usage = 2,
    Permission = 3,
    Conflict = 4,
    External = 5,
}

impl Code {
    pub const ALL: [Code; 11] = [
        Code::Internal,
        Code::Usage,
        Code::Validation,
        Code::NotFound,
        Code::Unauthorized,
        Code::Forbidden,
        Code::Conflict,
        Code::PortInUse,
        Code::SchemaTooNew,
        Code::RateLimited,
        Code::External,
    ];

    /// 직렬화 이름과 같은 문자열.
    pub fn as_str(self) -> &'static str {
        match self {
            Code::Internal => "INTERNAL",
            Code::Usage => "USAGE",
            Code::Validation => "VALIDATION",
            Code::NotFound => "NOT_FOUND",
            Code::Unauthorized => "UNAUTHORIZED",
            Code::Forbidden => "FORBIDDEN",
            Code::Conflict => "CONFLICT",
            Code::PortInUse => "PORT_IN_USE",
            Code::SchemaTooNew => "SCHEMA_TOO_NEW",
            Code::RateLimited => "RATE_LIMITED",
            Code::External => "EXTERNAL",
        }
    }

    pub fn exit_code(self) -> ExitCode {
        match self {
            Code::Internal | Code::NotFound | Code::SchemaTooNew | Code::RateLimited => {
                ExitCode::General
            }
            Code::Usage | Code::Validation => ExitCode::Usage,
            Code::Unauthorized | Code::Forbidden => ExitCode::Permission,
            Code::Conflict | Code::PortInUse => ExitCode::Conflict,
            Code::External => ExitCode::External,
        }
    }

    /// REST 응답 상태 코드. `web`이 axum 타입으로 바꾼다.
    pub fn http_status(self) -> u16 {
        match self {
            Code::Internal | Code::SchemaTooNew => 500,
            Code::Usage | Code::Validation => 400,
            Code::NotFound => 404,
            Code::Unauthorized => 401,
            Code::Forbidden => 403,
            Code::Conflict | Code::PortInUse => 409,
            Code::RateLimited => 429,
            Code::External => 502,
        }
    }

    /// 이 코드의 기본 메시지 키.
    pub fn message_key(self) -> &'static str {
        match self {
            Code::Internal => "error.internal",
            Code::Usage => "error.usage",
            Code::Validation => "error.validation",
            Code::NotFound => "error.not_found",
            Code::Unauthorized => "error.unauthorized",
            Code::Forbidden => "error.forbidden",
            Code::Conflict => "error.conflict",
            Code::PortInUse => "error.port_in_use",
            Code::SchemaTooNew => "error.schema_too_new",
            Code::RateLimited => "error.rate_limited",
            Code::External => "error.external",
        }
    }
}

type Source = Box<dyn std::error::Error + Send + Sync + 'static>;

/// 도메인 에러.
#[derive(Debug)]
pub struct Error {
    code: Code,
    message_key: &'static str,
    args: BTreeMap<&'static str, String>,
    hint_key: Option<&'static str>,
    next: Vec<String>,
    source: Option<Source>,
}

impl Error {
    pub fn new(code: Code) -> Self {
        Self {
            code,
            message_key: code.message_key(),
            args: BTreeMap::new(),
            hint_key: None,
            next: Vec::new(),
            source: None,
        }
    }

    /// 예상하지 못한 오류를 감싼다. 원인은 로그로만 나간다.
    pub fn internal(source: impl Into<Source>) -> Self {
        Self::new(Code::Internal).with_source(source)
    }

    /// 기본 메시지 대신 다른 메시지 키를 쓴다.
    pub fn with_message(mut self, key: &'static str) -> Self {
        self.message_key = key;
        self
    }

    /// 메시지의 `{name}` 자리에 넣을 값.
    pub fn arg(mut self, name: &'static str, value: impl Into<String>) -> Self {
        self.args.insert(name, value.into());
        self
    }

    pub fn with_hint(mut self, key: &'static str) -> Self {
        self.hint_key = Some(key);
        self
    }

    /// 다음에 실행할 명령. 순서대로 시도한다.
    pub fn next(mut self, command: impl Into<String>) -> Self {
        self.next.push(command.into());
        self
    }

    pub fn with_source(mut self, source: impl Into<Source>) -> Self {
        self.source = Some(source.into());
        self
    }

    pub fn code(&self) -> Code {
        self.code
    }

    /// 문장 카탈로그 키. 같은 코드 안에서 원인을 구분할 때 쓴다.
    pub fn message_key(&self) -> &'static str {
        self.message_key
    }

    pub fn exit_code(&self) -> ExitCode {
        self.code.exit_code()
    }

    pub fn message(&self) -> String {
        messages::render(self.message_key, &self.args)
    }

    pub fn hint(&self) -> Option<String> {
        self.hint_key.map(|key| messages::render(key, &self.args))
    }

    pub fn next_commands(&self) -> &[String] {
        &self.next
    }

    /// 응답 본문. 내부 오류는 원인·인자·다음 명령을 모두 숨기고 요청 ID만 준다.
    pub fn body(&self, request_id: Option<&str>) -> ErrorBody {
        if self.code == Code::Internal {
            return ErrorBody {
                code: Code::Internal,
                message: messages::render(Code::Internal.message_key(), &BTreeMap::new()),
                hint: None,
                next: Vec::new(),
                request_id: request_id.map(str::to_owned),
            };
        }
        ErrorBody {
            code: self.code,
            message: self.message(),
            hint: self.hint(),
            next: self.next.clone(),
            request_id: request_id.map(str::to_owned),
        }
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.code.as_str(), self.message())
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        self.source
            .as_deref()
            .map(|s| s as &(dyn std::error::Error + 'static))
    }
}

/// SPEC §10.1 실패 응답의 `error` 필드.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ErrorBody {
    pub code: Code,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hint: Option<String>,
    #[serde(default)]
    pub next: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub request_id: Option<String>,
}

pub type Result<T, E = Error> = std::result::Result<T, E>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn code_strings_match_serde_names() {
        for code in Code::ALL {
            let json = serde_json::to_value(code).unwrap();
            assert_eq!(json, serde_json::Value::String(code.as_str().into()));
        }
    }

    #[test]
    fn every_code_has_a_korean_message() {
        for code in Code::ALL {
            assert!(
                messages::lookup(code.message_key()).is_some(),
                "{} 메시지 키 없음",
                code.as_str()
            );
        }
    }

    #[test]
    fn exit_code_mapping() {
        let table = [
            (Code::Internal, 1),
            (Code::NotFound, 1),
            (Code::SchemaTooNew, 1),
            (Code::RateLimited, 1),
            (Code::Usage, 2),
            (Code::Validation, 2),
            (Code::Unauthorized, 3),
            (Code::Forbidden, 3),
            (Code::Conflict, 4),
            (Code::PortInUse, 4),
            (Code::External, 5),
        ];
        assert_eq!(table.len(), Code::ALL.len());
        for (code, exit) in table {
            assert_eq!(code.exit_code() as u8, exit, "{}", code.as_str());
        }
    }

    #[test]
    fn port_in_use_body() {
        let err = Error::new(Code::PortInUse)
            .arg("port", "8080")
            .with_hint("hint.port_in_use")
            .next("maru serve --bind 127.0.0.1:8081");
        insta::assert_json_snapshot!(err.body(None));
    }

    #[test]
    fn internal_body_hides_source_and_details() {
        let source = std::io::Error::other("near \"SELEC\": syntax error at C:\\data\\maru.db");
        let err = Error::internal(source)
            .arg("path", "C:\\data\\maru.db")
            .next("secret command");
        let body = err.body(Some("req_01J"));
        let text = serde_json::to_string(&body).unwrap();
        assert!(!text.contains("SELEC"), "{text}");
        assert!(!text.contains("maru.db"), "{text}");
        assert!(!text.contains("secret"), "{text}");
        insta::assert_json_snapshot!(body);
        assert!(std::error::Error::source(&err).is_some());
    }
}
