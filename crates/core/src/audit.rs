//! 감사 기록 (PHASE1_PLAN §4.2). 쓰기 트랜잭션은 감사 기록 없이 커밋되지 않는다 (`store::Tx`).

use serde::Serialize;
use serde_json::{Value, json};

use crate::actor::Actor;
use crate::clock::{self, Id};

/// diff 에서 지우는 키. 값 대신 `"[redacted]"` 를 남긴다.
const SECRET_KEYS: [&str; 6] = [
    "password",
    "password_hash",
    "token",
    "token_hash",
    "secret",
    "ticket_hash",
];

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct AuditRecord {
    pub id: Id,
    pub actor_type: &'static str,
    pub actor_id: Option<Id>,
    pub action: &'static str,
    pub target_type: &'static str,
    pub target_id: Option<Id>,
    pub diff: Value,
    pub at: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Target {
    pub kind: &'static str,
    pub id: Option<Id>,
}

impl Target {
    pub fn new(kind: &'static str, id: impl Into<Id>) -> Self {
        Self {
            kind,
            id: Some(id.into()),
        }
    }

    pub fn post(id: &str) -> Self {
        Self::new("post", id)
    }

    pub fn board(id: &str) -> Self {
        Self::new("board", id)
    }

    pub fn token(id: &str) -> Self {
        Self::new("token", id)
    }

    pub fn site() -> Self {
        Self {
            kind: "site",
            id: None,
        }
    }
}

impl AuditRecord {
    pub fn new(actor: &Actor, action: &'static str, target: Target, mut diff: Value) -> Self {
        redact(&mut diff);
        let (actor_type, actor_id) = actor.audit_identity();
        Self {
            id: clock::new_id(),
            actor_type,
            actor_id,
            action,
            target_type: target.kind,
            target_id: target.id,
            diff,
            at: clock::now(),
        }
    }
}

/// 변경 전후를 하나의 JSON 으로. Phase 3 changeset 역연산이 `before` 를 쓴다.
pub fn diff<T: Serialize>(before: Option<&T>, after: Option<&T>) -> Value {
    json!({
        "before": before.map(|v| serde_json::to_value(v).unwrap_or(Value::Null)),
        "after": after.map(|v| serde_json::to_value(v).unwrap_or(Value::Null)),
    })
}

pub fn redact(value: &mut Value) {
    match value {
        Value::Object(map) => {
            for (key, v) in map.iter_mut() {
                if SECRET_KEYS.contains(&key.as_str()) {
                    *v = Value::String("[redacted]".into());
                } else {
                    redact(v);
                }
            }
        }
        Value::Array(items) => items.iter_mut().for_each(redact),
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn secrets_are_redacted_recursively() {
        let mut v = json!({
            "after": {"name": "ci", "token_hash": "abc", "nested": [{"password": "pw"}]}
        });
        redact(&mut v);
        let s = v.to_string();
        assert!(!s.contains("abc") && !s.contains("\"pw\""), "{s}");
        assert!(s.contains("ci"));
    }
}
