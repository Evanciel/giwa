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
    /// 사람 승인 없이 changeset 을 적용하게 한다. 원칙 4 의 예외이니 명시적으로만 켠다.
    #[serde(default)]
    pub auto_apply: bool,
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
    require_operator(actor)?;
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
        auto_apply: input.auto_apply,
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
    Ok(Actor::agent(token.id, &token.scopes, token.board_ids).with_auto_apply(token.auto_apply))
}

/// 토큰 목록. 원문·해시는 내보내지 않는다. 운영자만.
pub async fn list(ctx: &Ctx, actor: &Actor) -> Result<Vec<Token>> {
    require_operator(actor)?;
    ctx.store.list_tokens().await
}

/// 토큰을 폐기한다. 이미 폐기됐으면 그대로 돌려준다 (멱등).
pub async fn revoke(ctx: &Ctx, actor: &Actor, id: &str) -> Result<Token> {
    require_operator(actor)?;
    let mut tx = Tx::begin(ctx).await?;
    let before = tx.token_by_id(id).await?.ok_or_else(|| {
        Error::new(Code::NotFound)
            .with_message("error.token_not_found")
            .arg("id", id.to_owned())
            .next("maru token list")
    })?;
    if before.revoked_at.is_some() {
        return Ok(before);
    }
    let now = clock::now();
    tx.revoke_token(id, &now).await?;
    let after = Token {
        revoked_at: Some(now.clone()),
        updated_at: now,
        ..before.clone()
    };
    tx.audit(
        actor,
        "token.revoke",
        Target::token(id),
        audit::diff(Some(&before), Some(&after)),
    )
    .await?;
    tx.commit().await?;
    Ok(after)
}

/// 토큰 관리는 사람 운영자만. 에이전트가 다른 토큰을 보거나 끄지 못하게.
fn require_operator(actor: &Actor) -> Result<()> {
    actor.require(Permission::SiteAdmin)?;
    if matches!(actor.kind(), ActorKind::Agent { .. }) {
        return Err(actor.denied(Permission::SiteAdmin));
    }
    Ok(())
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
