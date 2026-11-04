//! 모듈 생명주기와 툴 호출 (SPEC §7.3, ADR-0066). 웹·REST·CLI 가 같은 함수를 부른다.

use maru_core::actor::{Actor, Permission};
use maru_core::audit::Target;
use maru_core::clock;
use maru_core::fields::{self, FieldDef};
use maru_core::model::{ModuleFailure, ModuleRow};
use maru_core::store::{Ctx, Tx};
use maru_core::{Code, Error, Result};
use schemars::JsonSchema;
use serde::Serialize;
use serde_json::{Map, Value, json};

use crate::{Module, Registry, tool_not_found};

/// 툴 설명 (목록·에이전트용).
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct ToolInfo {
    pub name: String,
    pub description: String,
    pub input_schema: Value,
    pub permission: Option<String>,
}

/// 모듈 하나의 상태.
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct ModuleInfo {
    pub id: String,
    pub name: String,
    pub version: String,
    pub description: String,
    /// 마이그레이션이 적용돼 있다
    pub installed: bool,
    pub enabled: bool,
    /// 기본값 위에 저장한 값을 얹은 설정
    pub settings: Map<String, Value>,
    pub settings_fields: Vec<FieldDef>,
    pub tools: Vec<ToolInfo>,
    pub widgets: Vec<String>,
    pub events: Vec<String>,
    /// 실패 보관함·재시도 대기에 있는 이벤트 수
    pub failures: i64,
}

/// 제거 결과.
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct Removed {
    pub id: String,
    /// 되돌린 마이그레이션 버전
    pub reverted: Vec<i64>,
}

fn not_found(id: &str) -> Error {
    Error::new(Code::NotFound)
        .with_message("error.module_not_found")
        .arg("id", id)
        .next("maru module list")
}

fn disabled(id: &str) -> Error {
    Error::new(Code::Conflict)
        .with_message("error.module_disabled")
        .arg("id", id)
        .next(format!("maru module enable {id}"))
}

fn merged(module: &dyn Module, row: Option<&ModuleRow>) -> Map<String, Value> {
    let mut out = module.default_settings();
    if let Some(Value::Object(saved)) = row.map(|r| &r.settings) {
        out.extend(saved.clone());
    }
    out
}

async fn info(ctx: &Ctx, module: &dyn Module, row: Option<&ModuleRow>) -> Result<ModuleInfo> {
    let m = module.manifest();
    let failures = if row.is_some() {
        ctx.store.module_failures(Some(m.id)).await?.len() as i64
    } else {
        0
    };
    Ok(ModuleInfo {
        id: m.id.to_owned(),
        name: m.name.to_owned(),
        version: m.version.to_owned(),
        description: m.description.to_owned(),
        installed: row.is_some(),
        enabled: row.is_some_and(|r| r.enabled),
        settings: merged(module, row),
        settings_fields: module.settings_fields(),
        tools: module
            .tools()
            .into_iter()
            .map(|t| ToolInfo {
                name: t.name.to_owned(),
                description: t.description.to_owned(),
                input_schema: t.input_schema,
                permission: t.permission.map(|p| p.as_str().to_owned()),
            })
            .collect(),
        widgets: module
            .widget_types()
            .into_iter()
            .map(|t| t.id.to_owned())
            .collect(),
        events: m.events.iter().map(|e| (*e).to_owned()).collect(),
        failures,
    })
}

pub async fn list(ctx: &Ctx, actor: &Actor, registry: &Registry) -> Result<Vec<ModuleInfo>> {
    actor.require(Permission::ModuleManage)?;
    let rows = ctx.store.module_rows().await?;
    let mut out = Vec::new();
    for m in registry.iter() {
        let row = rows.iter().find(|r| r.id == m.manifest().id);
        out.push(info(ctx, m.as_ref(), row).await?);
    }
    Ok(out)
}

pub async fn show(ctx: &Ctx, actor: &Actor, registry: &Registry, id: &str) -> Result<ModuleInfo> {
    actor.require(Permission::ModuleManage)?;
    let m = registry.get(id).ok_or_else(|| not_found(id))?;
    let row = ctx.store.module_row(id).await?;
    info(ctx, m.as_ref(), row.as_ref()).await
}

/// 마이그레이션을 적용하고 켠다. 이미 켜져 있으면 그대로(새 마이그레이션만 적용).
/// 꺼져 있던 동안의 이벤트는 소급하지 않는다.
pub async fn enable(ctx: &Ctx, actor: &Actor, registry: &Registry, id: &str) -> Result<ModuleInfo> {
    actor.require(Permission::ModuleManage)?;
    let m = registry.get(id).ok_or_else(|| not_found(id))?;
    let manifest = m.manifest();
    let applied = ctx.store.module_apply(id, &m.migrations()).await?;
    let before = ctx.store.module_row(id).await?;
    if before.as_ref().is_some_and(|r| r.enabled) && applied.is_empty() {
        return info(ctx, m.as_ref(), before.as_ref()).await;
    }
    let now = clock::now();
    let row = ModuleRow {
        id: id.to_owned(),
        enabled: true,
        version: manifest.version.to_owned(),
        settings: before
            .as_ref()
            .map_or_else(|| json!({}), |r| r.settings.clone()),
        cursor: match &before {
            Some(r) if r.enabled => r.cursor,
            _ => ctx.store.audit_last_seq().await?,
        },
        installed_at: before
            .as_ref()
            .map_or_else(|| now.clone(), |r| r.installed_at.clone()),
        updated_at: now,
    };
    let mut tx = Tx::begin(ctx).await?;
    tx.upsert_module(&row).await?;
    tx.audit(
        actor,
        "module.enable",
        Target::new("module", id),
        json!({ "version": manifest.version, "migrations": applied }),
    )
    .await?;
    tx.commit().await?;
    info(ctx, m.as_ref(), Some(&row)).await
}

/// 끈다. 데이터는 그대로다.
pub async fn disable(
    ctx: &Ctx,
    actor: &Actor,
    registry: &Registry,
    id: &str,
) -> Result<ModuleInfo> {
    actor.require(Permission::ModuleManage)?;
    let m = registry.get(id).ok_or_else(|| not_found(id))?;
    let Some(mut row) = ctx.store.module_row(id).await? else {
        return info(ctx, m.as_ref(), None).await;
    };
    if !row.enabled {
        return info(ctx, m.as_ref(), Some(&row)).await;
    }
    row.enabled = false;
    row.updated_at = clock::now();
    let mut tx = Tx::begin(ctx).await?;
    tx.upsert_module(&row).await?;
    tx.audit(
        actor,
        "module.disable",
        Target::new("module", id),
        json!({}),
    )
    .await?;
    tx.commit().await?;
    info(ctx, m.as_ref(), Some(&row)).await
}

/// down 마이그레이션을 실행하고 상태까지 지운다. 모듈의 데이터가 사라진다. 없으면 그대로(멱등).
pub async fn remove(ctx: &Ctx, actor: &Actor, registry: &Registry, id: &str) -> Result<Removed> {
    actor.require(Permission::ModuleManage)?;
    let m = registry.get(id).ok_or_else(|| not_found(id))?;
    let reverted = ctx.store.module_revert(id, &m.migrations()).await?;
    if ctx.store.module_row(id).await?.is_some() {
        let mut tx = Tx::begin(ctx).await?;
        tx.delete_module(id).await?;
        tx.audit(
            actor,
            "module.remove",
            Target::new("module", id),
            json!({ "reverted": reverted }),
        )
        .await?;
        tx.commit().await?;
    }
    Ok(Removed {
        id: id.to_owned(),
        reverted,
    })
}

/// 모듈 설정 일부를 바꾼다. 설치돼 있으면 꺼져 있어도 된다.
pub async fn set_settings(
    ctx: &Ctx,
    actor: &Actor,
    registry: &Registry,
    id: &str,
    patch: Map<String, Value>,
) -> Result<ModuleInfo> {
    actor.require(Permission::ModuleManage)?;
    let m = registry.get(id).ok_or_else(|| not_found(id))?;
    let Some(mut row) = ctx.store.module_row(id).await? else {
        return Err(disabled(id));
    };
    let mut next = merged(m.as_ref(), Some(&row));
    next.extend(patch);
    let checked = fields::validate(&m.settings_fields(), &next, true)?;
    m.check_settings(&checked)?;
    let diff = json!({ "before": row.settings, "after": checked });
    row.settings = Value::Object(checked);
    row.updated_at = clock::now();
    let mut tx = Tx::begin(ctx).await?;
    tx.upsert_module(&row).await?;
    tx.audit(actor, "module.settings", Target::new("module", id), diff)
        .await?;
    tx.commit().await?;
    info(ctx, m.as_ref(), Some(&row)).await
}

/// 실패 보관함과 재시도 대기.
pub async fn failures(
    ctx: &Ctx,
    actor: &Actor,
    registry: &Registry,
    id: Option<&str>,
) -> Result<Vec<ModuleFailure>> {
    actor.require(Permission::ModuleManage)?;
    if let Some(id) = id
        && registry.get(id).is_none()
    {
        return Err(not_found(id));
    }
    ctx.store.module_failures(id).await
}

/// 모듈 툴을 부른다. 꺼져 있으면 충돌.
pub async fn call_tool(
    ctx: &Ctx,
    actor: &Actor,
    registry: &Registry,
    id: &str,
    tool: &str,
    input: Value,
) -> Result<Value> {
    let m = registry.get(id).ok_or_else(|| not_found(id))?;
    if !ctx.store.module_row(id).await?.is_some_and(|r| r.enabled) {
        return Err(disabled(id));
    }
    let def = m
        .tools()
        .into_iter()
        .find(|t| t.name == tool)
        .ok_or_else(|| tool_not_found(id, tool))?;
    if let Some(p) = def.permission {
        actor.require(p)?;
    }
    m.call_tool(ctx, actor, tool, input).await
}

/// 저장된 모듈 설정을 기본값에 얹어 읽는다. 모듈 자신이 쓴다.
pub async fn settings_of(ctx: &Ctx, module: &dyn Module) -> Result<Map<String, Value>> {
    let row = ctx.store.module_row(module.manifest().id).await?;
    Ok(merged(module, row.as_ref()))
}

/// 켜진 모듈의 새 마이그레이션을 적용한다. 바이너리를 올린 뒤 서버 시작 때 부른다.
pub async fn upgrade_enabled(ctx: &Ctx, registry: &Registry) -> Result<()> {
    for row in ctx.store.module_rows().await? {
        if let Some(m) = registry.get(&row.id) {
            let applied = ctx.store.module_apply(&row.id, &m.migrations()).await?;
            if !applied.is_empty() {
                tracing::info!(module = %row.id, ?applied, "모듈 마이그레이션 적용");
            }
        }
    }
    Ok(())
}
