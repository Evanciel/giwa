//! SPEC §10.1 `--json` 출력 계약. CLI와 REST가 같은 모양을 쓴다.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::error::ErrorBody;

/// 성공 응답: `{ "ok": true, "data": ..., "warnings": [] }`
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Success<T> {
    pub ok: bool,
    pub data: T,
    pub warnings: Vec<Warning>,
}

impl<T> Success<T> {
    pub fn new(data: T) -> Self {
        Self {
            ok: true,
            data,
            warnings: Vec::new(),
        }
    }

    pub fn warn(mut self, warning: Warning) -> Self {
        self.warnings.push(warning);
        self
    }
}

/// 실패 응답: `{ "ok": false, "error": { ... } }`
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Failure {
    pub ok: bool,
    pub error: ErrorBody,
}

impl Failure {
    pub fn new(error: ErrorBody) -> Self {
        Self { ok: false, error }
    }
}

/// 실패는 아니지만 알려야 할 것.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Warning {
    pub code: String,
    pub message: String,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::{Code, Error};

    #[test]
    fn success_shape() {
        let out = Success::new(serde_json::json!({ "id": "01J" })).warn(Warning {
            code: "SITE_URL_DEFAULT".into(),
            message: "site_url 이 기본값입니다.".into(),
        });
        insta::assert_json_snapshot!(out);
    }

    #[test]
    fn failure_shape() {
        let err = Error::new(Code::Forbidden);
        insta::assert_json_snapshot!(Failure::new(err.body(None)));
    }
}
