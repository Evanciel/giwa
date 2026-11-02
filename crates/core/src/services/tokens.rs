//! API 토큰 (ADR-0009). 원문은 발급 때 한 번만 보여 주고 SHA-256 해시만 저장한다.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::actor::{Actor, ActorKind, Permission, Scope};
use crate::audit::{self, Target};
use crate::clock;
use crate::error::{Code, Error, Result};
use crate::model::Token;
use crate::services::required_text;
use crate::store::{Ctx, Tx};

const PREFIX: &str = "maru_";

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct NewToken {
    pub name: String,
    /// 비우면 `read`, `draft`
    #[serde(default)]
    pub scopes: Vec<Scope>,
    #[serde(default)]
    pub board_ids: Option<Vec<String>>,
    /// RFC3339
    #[serde(default)]
    pub expires_at: Option<String>,
}

#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct IssuedToken {
    #[serde(flatten)]
    pub token: Token,
    /// 원문. 다시 볼 수 없다.
    pub secret: String,
}

/// 사람(로컬 포함)만 발급한다. 에이전트가 자기보다 넓은 토큰을 만들지 못하게.
pub async fn create(ctx: &Ctx, actor: &Actor, input: NewToken) -> Result<IssuedToken> {
    actor.require(Permission::SiteAdmin)?;
    if matches!(actor.kind(), ActorKind::Agent { .. }) {
        return Err(actor.denied(Permission::SiteAdmin));
    }
    let name = required_text("name", &input.name, 100)?;
    let mut scopes = if input.scopes.is_empty() {
        Scope::DEFAULT.to_vec()
    } else {
        input.scopes
    };
    scopes.sort();
    scopes.dedup();
    let expires_at = match input.expires_at {
        Some(s) => Some(clock::parse(&s).map(clock::format).ok_or_else(|| {
            Error::new(Code::Validation)
                .with_message("error.timestamp")
                .arg("value", s)
        })?),
        None => None,
    };

    let secret = generate()?;
    let now = clock::now();
    let token = Token {
        id: clock::new_id(),
        name,
        token_hash: hash(&secret),
        scopes,
        board_ids: input.board_ids,
        expires_at,
        last_used_at: None,
        revoked_at: None,
        created_at: now.clone(),
        updated_at: now,
    };
    let mut tx = Tx::begin(ctx).await?;
    tx.insert_token(&token).await?;
    tx.audit(
        actor,
        "token.create",
        Target::token(&token.id),
        audit::diff(None, Some(&token)),
    )
    .await?;
    tx.commit().await?;
    Ok(IssuedToken { token, secret })
}

/// bearer 토큰으로 Actor 를 만든다. 실패 사유는 구분하지 않는다.
pub async fn authenticate(ctx: &Ctx, secret: &str) -> Result<Actor> {
    let invalid = || {
        Error::new(Code::Unauthorized)
            .with_message("error.token_invalid")
            .next("maru token create --name <이름> --yes")
    };
    if !secret.starts_with(PREFIX) {
        return Err(invalid());
    }
    let token = ctx
        .store
        .token_by_hash(&hash(secret))
        .await?
        .ok_or_else(invalid)?;
    if token.revoked_at.is_some() {
        return Err(invalid());
    }
    let now = clock::now();
    if let Some(exp) = &token.expires_at
        && exp.as_str() <= now.as_str()
    {
        return Err(invalid());
    }
    ctx.store.touch_token(&token.id, &now).await?;
    Ok(Actor::agent(token.id, &token.scopes, token.board_ids))
}

fn generate() -> Result<String> {
    let mut buf = [0u8; 32];
    getrandom::fill(&mut buf).map_err(|e| Error::internal(e.to_string()))?;
    Ok(format!("{PREFIX}{}", hex::encode(buf)))
}

pub fn hash(secret: &str) -> String {
    hex::encode(Sha256::digest(secret.as_bytes()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generated_tokens_differ_and_hash_is_hex() {
        let a = generate().unwrap();
        let b = generate().unwrap();
        assert_ne!(a, b);
        assert!(a.starts_with(PREFIX));
        assert_eq!(a.len(), PREFIX.len() + 64);
        assert_eq!(hash(&a).len(), 64);
        assert_eq!(hash(&a), hash(&a));
    }
}
