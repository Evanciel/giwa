//! Changeset (SPEC §6, 원칙 4, ADR-0121). 구조·디자인·모듈 변경을 연산 목록으로 묶어
//! 미리보기 → 승인 → 적용하고, 한 번에 되돌린다.
//!
//! 서비스마다 자기 트랜잭션을 여니 연산 전체를 한 트랜잭션으로 묶을 수 없다. 그래서 사본 DB 에서
//! 먼저 돌려 보고, 실제 DB 에 차례로 적용하며 실패하면 이미 한 것을 역연산으로 되돌린다.

use async_trait::async_trait;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};

use crate::actor::{Actor, ActorKind, Permission};
use crate::audit::Target;
use crate::clock;
use crate::design::Tokens;
use crate::error::{Code, Error, Result};
use crate::model::{Board, Changeset, ChangesetStatus, MenuItem, PageStatus};
use crate::services::presets::{self, Preset};
use crate::services::{auth, boards, design, menus, pages, redirects, settings, widgets};
use crate::store::{Ctx, Tx};

/// 한 changeset 에 담을 수 있는 연산 수.
pub const MAX_OPS: usize = 500;
/// 1회용 미리보기 주소가 살아 있는 시간.
const PREVIEW_LINK_MINUTES: i64 = 30;

/// 바꾸는 일 하나. 적용할 때 역연산을 만든다.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "op")]
pub enum ChangeOp {
    #[serde(rename = "board.create")]
    BoardCreate(Box<boards::NewBoard>),
    #[serde(rename = "board.update")]
    BoardUpdate {
        slug: String,
        patch: Box<boards::BoardPatch>,
    },
    #[serde(rename = "board.delete")]
    BoardDelete { slug: String },
    #[serde(rename = "page.create")]
    PageCreate(Box<pages::NewPage>),
    #[serde(rename = "page.update")]
    PageUpdate {
        slug: String,
        patch: pages::PagePatch,
    },
    #[serde(rename = "page.publish")]
    PagePublish { slug: String },
    #[serde(rename = "page.hide")]
    PageHide { slug: String },
    #[serde(rename = "page.delete")]
    PageDelete { slug: String },
    #[serde(rename = "page.restore")]
    PageRestore { slug: String },
    #[serde(rename = "redirect.add")]
    RedirectAdd(Box<redirects::NewRedirect>),
    #[serde(rename = "redirect.remove")]
    RedirectRemove { from: String },
    #[serde(rename = "menu.set")]
    MenuSet {
        location: String,
        items: Vec<MenuItem>,
    },
    /// 디자인 토큰 일부를 겹쳐 쓴다.
    #[serde(rename = "tokens.set")]
    TokensSet { tokens: Value },
    #[serde(rename = "tokens.reset")]
    TokensReset,
    /// 사이트 설정. 값은 `SettingsPatch` 의 칸.
    #[serde(rename = "settings.set")]
    SettingsSet { values: Map<String, Value> },
    #[serde(rename = "widget.place")]
    WidgetPlace(Box<widgets::NewWidget>),
    #[serde(rename = "widget.configure")]
    WidgetConfigure {
        id: String,
        patch: widgets::WidgetPatch,
    },
    #[serde(rename = "widget.remove")]
    WidgetRemove { id: String },
    #[serde(rename = "module.enable")]
    ModuleEnable { id: String },
    #[serde(rename = "module.disable")]
    ModuleDisable { id: String },
    /// 프리셋의 `ensure` 연산 하나. 이미 사용자가 고친 것은 건드리지 않는다.
    #[serde(rename = "preset.step")]
    PresetStep { step: presets::Op },
}

impl ChangeOp {
    /// 이 연산을 담으려면 있어야 하는 권한.
    fn permission(&self) -> Permission {
        match self {
            Self::BoardCreate(_)
            | Self::BoardUpdate { .. }
            | Self::BoardDelete { .. }
            | Self::PageCreate(_)
            | Self::PageUpdate { .. }
            | Self::PagePublish { .. }
            | Self::PageHide { .. }
            | Self::PageDelete { .. }
            | Self::PageRestore { .. }
            | Self::RedirectAdd(_)
            | Self::RedirectRemove { .. }
            | Self::MenuSet { .. }
            | Self::SettingsSet { .. }
            | Self::PresetStep { .. } => Permission::StructureEdit,
            Self::TokensSet { .. }
            | Self::TokensReset
            | Self::WidgetPlace(_)
            | Self::WidgetConfigure { .. }
            | Self::WidgetRemove { .. } => Permission::DesignEdit,
            Self::ModuleEnable { .. } | Self::ModuleDisable { .. } => Permission::ModuleManage,
        }
    }
}

/// 사본 DB. 적용 전 시험과 미리보기에 쓴다. 코어는 파일을 모른다.
#[async_trait]
pub trait Sandbox: Send + Sync {
    /// 지금 DB 의 사본을 새로 만들어 같은 서비스를 돌릴 컨텍스트를 준다.
    async fn open(&self, ctx: &Ctx, key: &str) -> Result<Ctx>;
    /// 사본을 닫고 지운다. 없으면 아무것도 하지 않는다.
    async fn close(&self, key: &str);
}

fn not_found(id: &str) -> Error {
    Error::new(Code::NotFound)
        .with_message("error.changeset_not_found")
        .arg("id", id)
        .next("maru changeset list")
}

fn wrong_status(cs: &Changeset) -> Error {
    Error::new(Code::Conflict)
        .with_message("error.changeset_status")
        .arg("id", cs.id.clone())
        .arg("status", cs.status.as_str())
        .next(format!("maru changeset show {}", cs.id))
}

fn bad_op(detail: impl std::fmt::Display) -> Error {
    Error::new(Code::Validation)
        .with_message("error.changeset_op")
        .arg("detail", detail.to_string())
}

fn parse_ops(values: &[Value]) -> Result<Vec<ChangeOp>> {
    values
        .iter()
        .map(|v| serde_json::from_value(v.clone()).map_err(bad_op))
        .collect()
}

fn to_values(ops: &[ChangeOp]) -> Result<Vec<Value>> {
    ops.iter()
        .map(|op| serde_json::to_value(op).map_err(Error::internal))
        .collect()
}

/// 볼 수 있는 사람: 이 컴퓨터, 토큰, 구조·디자인·모듈을 다룰 수 있는 회원.
fn require_view(actor: &Actor) -> Result<()> {
    match actor.kind() {
        ActorKind::Anonymous | ActorKind::Human { .. } => actor.require(Permission::StructureEdit),
        _ => Ok(()),
    }
}

/// 적용·되돌리기는 사람(소유자·관리자), 이 컴퓨터, 자동 적용을 허락받은 토큰만 (SPEC §5.3).
fn require_apply(actor: &Actor) -> Result<()> {
    match actor.kind() {
        ActorKind::Agent { .. } if !actor.can_auto_apply() => Err(Error::new(Code::Forbidden)
            .with_message("error.changeset_apply_agent")
            .next("maru changeset apply <id>")),
        ActorKind::Anonymous | ActorKind::Human { .. } => actor.require(Permission::SiteAdmin),
        _ => Ok(()),
    }
}

fn checked(actor: &Actor, ops: &[Value]) -> Result<Vec<ChangeOp>> {
    let parsed = parse_ops(ops)?;
    for op in &parsed {
        actor.require(op.permission())?;
    }
    Ok(parsed)
}

/// 새 changeset 을 만든다. 연산은 비어 있어도 된다(`add` 로 더한다).
pub async fn create(ctx: &Ctx, actor: &Actor, title: &str, ops: Vec<Value>) -> Result<Changeset> {
    require_view(actor)?;
    let title = title.trim();
    if title.is_empty() || title.chars().count() > 200 {
        return Err(Error::new(Code::Validation).with_message("error.changeset_title"));
    }
    if ops.len() > MAX_OPS {
        return Err(bad_op(format!("연산은 {MAX_OPS}개까지입니다")));
    }
    checked(actor, &ops)?;
    let (actor_type, actor_id) = actor.audit_identity();
    let cs = Changeset {
        id: clock::new_id(),
        title: title.to_owned(),
        actor_type: actor_type.to_owned(),
        actor_id,
        status: ChangesetStatus::Preview,
        ops,
        inverse: Vec::new(),
        error: None,
        created_at: clock::now(),
        applied_at: None,
        reverted_at: None,
    };
    let mut tx = Tx::begin(ctx).await?;
    tx.save_changeset(&cs).await?;
    tx.audit(
        actor,
        "changeset.create",
        Target::new("changeset", cs.id.clone()),
        json!({ "title": cs.title, "ops": cs.ops }),
    )
    .await?;
    tx.commit().await?;
    Ok(cs)
}

/// 프리셋 하나를 changeset 으로 (SPEC §11 `create_site_from_preset`, ADR-0023).
pub async fn create_from_preset(ctx: &Ctx, actor: &Actor, preset: &Preset) -> Result<Changeset> {
    let ops: Vec<ChangeOp> = preset
        .ops
        .iter()
        .map(|step| ChangeOp::PresetStep { step: step.clone() })
        .collect();
    create(
        ctx,
        actor,
        &format!("프리셋 적용: {}", preset.name),
        to_values(&ops)?,
    )
    .await
}

/// 아직 적용 전인 changeset 에 연산을 더한다.
pub async fn add(ctx: &Ctx, actor: &Actor, id: &str, ops: Vec<Value>) -> Result<Changeset> {
    require_view(actor)?;
    let mut cs = get(ctx, actor, id).await?;
    if cs.status != ChangesetStatus::Preview {
        return Err(wrong_status(&cs));
    }
    if cs.ops.len() + ops.len() > MAX_OPS {
        return Err(bad_op(format!("연산은 {MAX_OPS}개까지입니다")));
    }
    checked(actor, &ops)?;
    cs.ops.extend(ops.iter().cloned());
    cs.error = None;
    let mut tx = Tx::begin(ctx).await?;
    tx.save_changeset(&cs).await?;
    tx.audit(
        actor,
        "changeset.add",
        Target::new("changeset", cs.id.clone()),
        json!({ "ops": ops }),
    )
    .await?;
    tx.commit().await?;
    Ok(cs)
}

pub async fn get(ctx: &Ctx, actor: &Actor, id: &str) -> Result<Changeset> {
    require_view(actor)?;
    ctx.store.changeset(id).await?.ok_or_else(|| not_found(id))
}

pub async fn list(
    ctx: &Ctx,
    actor: &Actor,
    status: Option<ChangesetStatus>,
    limit: i64,
) -> Result<Vec<Changeset>> {
    require_view(actor)?;
    ctx.store.changesets(status, limit.clamp(1, 200)).await
}

/// 버린다. 이미 버렸으면 그대로 돌려준다.
pub async fn discard(ctx: &Ctx, actor: &Actor, id: &str) -> Result<Changeset> {
    let mut cs = get(ctx, actor, id).await?;
    checked(actor, &cs.ops)?;
    match cs.status {
        ChangesetStatus::Discarded => return Ok(cs),
        ChangesetStatus::Preview => {}
        _ => return Err(wrong_status(&cs)),
    }
    cs.status = ChangesetStatus::Discarded;
    let mut tx = Tx::begin(ctx).await?;
    tx.save_changeset(&cs).await?;
    tx.audit(
        actor,
        "changeset.discard",
        Target::new("changeset", cs.id.clone()),
        json!({ "title": cs.title }),
    )
    .await?;
    tx.commit().await?;
    Ok(cs)
}

/// 사본에서 연산들을 차례로 돌린다. 실패하면 몇 번째인지 알려 준다.
async fn run_all(ctx: &Ctx, actor: &Actor, ops: &[ChangeOp]) -> Result<()> {
    for (i, op) in ops.iter().enumerate() {
        run(ctx, actor, op).await.map_err(|e| failed_at(i, &e))?;
    }
    Ok(())
}

/// 1회용 미리보기 주소. 관리 화면에 로그인하지 않는 사람에게 건넨다 (SPEC §6).
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct PreviewLink {
    pub changeset_id: String,
    pub url: String,
    pub expires_at: String,
}

fn hash_preview_token(secret: &str) -> String {
    hex::encode(Sha256::digest(secret.as_bytes()))
}

pub async fn preview_link(ctx: &Ctx, actor: &Actor, id: &str) -> Result<PreviewLink> {
    let cs = get(ctx, actor, id).await?;
    if cs.status != ChangesetStatus::Preview {
        return Err(wrong_status(&cs));
    }
    let secret = auth::new_secret()?;
    let expires_at = clock::format(
        time::OffsetDateTime::now_utc() + time::Duration::minutes(PREVIEW_LINK_MINUTES),
    );
    let mut tx = Tx::begin(ctx).await?;
    tx.set_changeset_preview_token(id, &hash_preview_token(&secret), &expires_at)
        .await?;
    tx.audit(
        actor,
        "changeset.preview_link",
        Target::new("changeset", id),
        json!({ "expires_at": expires_at }),
    )
    .await?;
    tx.commit().await?;
    Ok(PreviewLink {
        changeset_id: id.to_owned(),
        url: format!(
            "{}/__preview/{id}?token={secret}",
            ctx.site_url.trim_end_matches('/')
        ),
        expires_at,
    })
}

/// 미리보기 주소의 토큰을 쓴다. 한 번 쓰면 사라진다.
pub async fn redeem_preview_token(ctx: &Ctx, id: &str, secret: &str) -> Result<Changeset> {
    let used = ctx
        .store
        .take_changeset_preview_token(id, &hash_preview_token(secret), &clock::now())
        .await?;
    if !used {
        return Err(Error::new(Code::Forbidden).with_message("error.changeset_preview_token"));
    }
    ctx.store.changeset(id).await?.ok_or_else(|| not_found(id))
}

/// 세션으로 미리보기를 열 수 있는 사람: 소유자·관리자 (SPEC §6).
pub async fn for_preview(ctx: &Ctx, actor: &Actor, id: &str) -> Result<Changeset> {
    actor.require(Permission::SiteAdmin)?;
    get(ctx, actor, id).await
}

/// 사본에 연산을 적용한 컨텍스트 (미리보기). 실패하면 사본을 지운다.
/// 들어온 사람은 이미 확인했고, 사본에서만 도는 일이라 운영자 권한으로 돌린다.
pub async fn sandboxed(ctx: &Ctx, sandbox: &dyn Sandbox, key: &str, cs: &Changeset) -> Result<Ctx> {
    if cs.status != ChangesetStatus::Preview {
        return Err(wrong_status(cs));
    }
    let ops = parse_ops(&cs.ops)?;
    let copy = sandbox.open(ctx, key).await?;
    if let Err(e) = run_all(&copy, &Actor::local(), &ops).await {
        sandbox.close(key).await;
        return Err(e);
    }
    Ok(copy)
}

fn failed_at(i: usize, e: &Error) -> Error {
    let mut out = Error::new(e.code())
        .with_message("error.changeset_failed")
        .arg("index", (i + 1).to_string())
        .arg("detail", e.message());
    for next in e.next_commands() {
        out = out.next(next.clone());
    }
    out
}

/// 적용한다. 이미 적용했으면 그대로 돌려준다.
pub async fn apply(ctx: &Ctx, actor: &Actor, sandbox: &dyn Sandbox, id: &str) -> Result<Changeset> {
    require_apply(actor)?;
    let mut cs = get(ctx, actor, id).await?;
    match cs.status {
        ChangesetStatus::Applied => return Ok(cs),
        ChangesetStatus::Preview => {}
        _ => return Err(wrong_status(&cs)),
    }
    let ops = parse_ops(&cs.ops)?;
    if ops.is_empty() {
        return Err(bad_op("연산이 없습니다"));
    }

    // 사본에서 먼저 돌려 본다. 여기서 실패하면 실제 사이트는 건드리지 않았다.
    let key = format!("rehearse-{}", cs.id);
    let copy = sandbox.open(ctx, &key).await?;
    let rehearsal = run_all(&copy, actor, &ops).await;
    sandbox.close(&key).await;
    if let Err(failed) = rehearsal {
        cs.error = Some(failed.message());
        ctx.store.save_changeset(&cs).await?;
        return Err(failed);
    }

    cs.status = ChangesetStatus::Applying;
    cs.error = None;
    ctx.store.save_changeset(&cs).await?;
    let started = clock::now();
    for (i, op) in ops.iter().enumerate() {
        match run(ctx, actor, op).await {
            Ok(inv) => {
                cs.inverse.extend(to_values(&inv)?);
                // 도중에 꺼져도 `revert` 로 되돌릴 수 있게 연산마다 남긴다.
                ctx.store.save_changeset(&cs).await?;
            }
            Err(e) => {
                let failed = failed_at(i, &e);
                return Err(compensate(ctx, actor, cs, failed, &started).await);
            }
        }
    }

    cs.status = ChangesetStatus::Applied;
    cs.applied_at = Some(clock::now());
    let mut tx = Tx::begin(ctx).await?;
    tx.save_changeset(&cs).await?;
    tx.audit(
        actor,
        "changeset.apply",
        Target::new("changeset", cs.id.clone()),
        json!({ "title": cs.title, "ops": cs.ops, "inverse": cs.inverse }),
    )
    .await?;
    tx.commit().await?;
    stamp(ctx, actor, &cs.id, &started).await?;
    Ok(cs)
}

/// 이 changeset 이 남긴 감사 기록에 번호를 붙인다. 서비스는 changeset 을 모른다.
async fn stamp(ctx: &Ctx, actor: &Actor, id: &str, since: &str) -> Result<()> {
    let (actor_type, actor_id) = actor.audit_identity();
    ctx.store
        .stamp_audit(id, since, actor_type, actor_id.as_deref())
        .await
}

/// 적용 도중 실패: 한 것을 되돌려 처음 상태로. 되돌리기도 실패하면 `failed` 로 남긴다.
async fn compensate(
    ctx: &Ctx,
    actor: &Actor,
    mut cs: Changeset,
    failed: Error,
    since: &str,
) -> Error {
    let message = failed.message();
    match undo(ctx, actor, &mut cs).await {
        Ok(()) => {
            cs.status = ChangesetStatus::Preview;
            cs.error = Some(message);
        }
        Err(e) => {
            cs.status = ChangesetStatus::Failed;
            cs.error = Some(format!("{message} / 되돌리기 실패: {}", e.message()));
        }
    }
    if let Err(e) = ctx.store.save_changeset(&cs).await {
        return e;
    }
    if let Err(e) = stamp(ctx, actor, &cs.id, since).await {
        return e;
    }
    failed
}

/// 역연산을 뒤에서부터 실행한다. 한 것은 목록에서 빼 다시 돌려도 안전하다.
async fn undo(ctx: &Ctx, actor: &Actor, cs: &mut Changeset) -> Result<()> {
    let ops = parse_ops(&cs.inverse)?;
    for op in ops.iter().rev() {
        run(ctx, actor, op).await?;
        cs.inverse.pop();
        ctx.store.save_changeset(cs).await?;
    }
    Ok(())
}

/// 한 번에 되돌린다. 이미 되돌렸으면 그대로 돌려준다.
pub async fn revert(ctx: &Ctx, actor: &Actor, id: &str) -> Result<Changeset> {
    require_apply(actor)?;
    let mut cs = get(ctx, actor, id).await?;
    match cs.status {
        ChangesetStatus::Reverted => return Ok(cs),
        ChangesetStatus::Applied | ChangesetStatus::Applying | ChangesetStatus::Failed => {}
        _ => return Err(wrong_status(&cs)),
    }
    let started = clock::now();
    if let Err(e) = undo(ctx, actor, &mut cs).await {
        cs.status = ChangesetStatus::Failed;
        cs.error = Some(e.message());
        ctx.store.save_changeset(&cs).await?;
        stamp(ctx, actor, &cs.id, &started).await?;
        return Err(e.next(format!("maru changeset revert {id}")));
    }
    cs.status = ChangesetStatus::Reverted;
    cs.error = None;
    cs.reverted_at = Some(clock::now());
    let mut tx = Tx::begin(ctx).await?;
    tx.save_changeset(&cs).await?;
    tx.audit(
        actor,
        "changeset.revert",
        Target::new("changeset", cs.id.clone()),
        json!({ "title": cs.title }),
    )
    .await?;
    tx.commit().await?;
    stamp(ctx, actor, &cs.id, &started).await?;
    Ok(cs)
}

/// 고친 칸만 바뀌기 전 값으로 되돌리는 패치. 값이 없던 칸은 고칠 수 없으니 잡아낸다.
fn restore_values(changed: &Map<String, Value>, before: &Value) -> Result<Map<String, Value>> {
    let mut out = Map::new();
    for (key, value) in changed {
        if value.is_null() {
            continue;
        }
        let old = before
            .get(key)
            .ok_or_else(|| Error::internal(format!("되돌릴 칸이 없다: {key}")))?;
        // 금칙어처럼 입력은 글자, 저장은 목록인 칸
        let old = match (old, value) {
            (Value::Array(list), Value::String(_)) => Value::String(
                list.iter()
                    .filter_map(Value::as_str)
                    .collect::<Vec<_>>()
                    .join("\n"),
            ),
            _ => old.clone(),
        };
        out.insert(key.clone(), old);
    }
    Ok(out)
}

fn restore_patch<P: Serialize + serde::de::DeserializeOwned, B: Serialize>(
    patch: &P,
    before: &B,
) -> Result<P> {
    let changed = serde_json::to_value(patch).map_err(Error::internal)?;
    let before = serde_json::to_value(before).map_err(Error::internal)?;
    let Value::Object(changed) = changed else {
        return Err(Error::internal("패치가 객체가 아니다"));
    };
    serde_json::from_value(Value::Object(restore_values(&changed, &before)?))
        .map_err(Error::internal)
}

/// 지운 게시판을 되살리는 역연산. 안내 문구는 만들 때 받지 않아 고치기를 함께 둔다.
/// 역연산은 뒤에서부터 돌므로 고치기를 앞에 둔다.
fn recreate_board(before: &Board) -> Vec<ChangeOp> {
    vec![
        ChangeOp::BoardUpdate {
            slug: before.slug.clone(),
            patch: Box::new(boards::BoardPatch {
                description: Some(before.description.clone()),
                ..Default::default()
            }),
        },
        ChangeOp::BoardCreate(Box::new(boards::NewBoard {
            slug: before.slug.clone(),
            name: before.name.clone(),
            kind: before.kind.clone(),
            policy: Some(before.policy.clone()),
            sort_order: before.sort_order,
            layout: Some(before.layout.clone()),
            field_schema: Some(before.field_schema.clone()),
            list_order: Some(before.list_order.clone()),
        })),
    ]
}

/// 연산 하나를 실행하고 역연산을 돌려준다. 바뀐 것이 없으면 역연산도 없다.
async fn run(ctx: &Ctx, actor: &Actor, op: &ChangeOp) -> Result<Vec<ChangeOp>> {
    let actor = &actor.for_changeset();
    Ok(match op {
        ChangeOp::BoardCreate(input) => {
            let b = boards::create(ctx, actor, (**input).clone()).await?;
            vec![ChangeOp::BoardDelete { slug: b.slug }]
        }
        ChangeOp::BoardUpdate { slug, patch } => {
            let before = boards::show(ctx, actor, slug).await?;
            boards::update(ctx, actor, slug, (**patch).clone()).await?;
            vec![ChangeOp::BoardUpdate {
                slug: slug.clone(),
                patch: Box::new(restore_patch(&**patch, &before)?),
            }]
        }
        ChangeOp::BoardDelete { slug } => {
            let before = boards::show(ctx, actor, slug).await?;
            boards::delete(ctx, actor, slug).await?;
            recreate_board(&before)
        }
        ChangeOp::PageCreate(input) => {
            let p = pages::create(ctx, actor, (**input).clone()).await?;
            vec![ChangeOp::PageDelete { slug: p.slug }]
        }
        ChangeOp::PageUpdate { slug, patch } => {
            let before = pages::get(ctx, actor, slug).await?;
            let parent = pages::parent_slug(ctx, &before).await?;
            let after = pages::update(ctx, actor, slug, patch.clone()).await?;
            let mut before = serde_json::to_value(&before).map_err(Error::internal)?;
            before["parent"] = Value::String(parent);
            vec![ChangeOp::PageUpdate {
                slug: after.slug,
                patch: restore_patch(patch, &before)?,
            }]
        }
        ChangeOp::PagePublish { slug } => {
            let before = pages::get(ctx, actor, slug).await?;
            pages::publish(ctx, actor, slug).await?;
            if before.status == PageStatus::Published {
                Vec::new()
            } else {
                vec![ChangeOp::PageHide { slug: slug.clone() }]
            }
        }
        ChangeOp::PageHide { slug } => {
            let before = pages::get(ctx, actor, slug).await?;
            pages::hide(ctx, actor, slug).await?;
            if before.status == PageStatus::Published {
                vec![ChangeOp::PagePublish { slug: slug.clone() }]
            } else {
                Vec::new()
            }
        }
        ChangeOp::PageDelete { slug } => {
            pages::delete(ctx, actor, slug).await?;
            vec![ChangeOp::PageRestore { slug: slug.clone() }]
        }
        ChangeOp::PageRestore { slug } => {
            pages::restore(ctx, actor, slug).await?;
            vec![ChangeOp::PageDelete { slug: slug.clone() }]
        }
        ChangeOp::RedirectAdd(input) => {
            let before = redirects::get(ctx, actor, &input.from).await?;
            let row = redirects::add(ctx, actor, (**input).clone()).await?;
            vec![match before {
                Some(b) => ChangeOp::RedirectAdd(Box::new(redirects::NewRedirect {
                    from: b.from_path,
                    to: b.to_path,
                })),
                None => ChangeOp::RedirectRemove {
                    from: row.from_path,
                },
            }]
        }
        ChangeOp::RedirectRemove { from } => {
            let row = redirects::remove(ctx, actor, from).await?;
            vec![ChangeOp::RedirectAdd(Box::new(redirects::NewRedirect {
                from: row.from_path,
                to: row.to_path,
            }))]
        }
        ChangeOp::MenuSet { location, items } => {
            let before = menus::get(ctx, location).await?;
            menus::set(ctx, actor, location, items.clone()).await?;
            vec![ChangeOp::MenuSet {
                location: location.clone(),
                items: before,
            }]
        }
        ChangeOp::TokensSet { tokens } => {
            let before = design::tokens(ctx).await?;
            design::update(ctx, actor, tokens.clone()).await?;
            vec![restore_tokens(&before)?]
        }
        ChangeOp::TokensReset => {
            let before = design::tokens(ctx).await?;
            design::reset(ctx, actor).await?;
            vec![restore_tokens(&before)?]
        }
        ChangeOp::SettingsSet { values } => {
            let before = settings::site(ctx).await?;
            let patch: settings::SettingsPatch =
                serde_json::from_value(Value::Object(values.clone())).map_err(bad_op)?;
            settings::update(ctx, actor, patch).await?;
            let before = serde_json::to_value(before).map_err(Error::internal)?;
            vec![ChangeOp::SettingsSet {
                values: restore_values(values, &before)?,
            }]
        }
        ChangeOp::WidgetPlace(input) => {
            let w = widgets::place(ctx, actor, (**input).clone()).await?;
            vec![ChangeOp::WidgetRemove { id: w.id }]
        }
        ChangeOp::WidgetConfigure { id, patch } => {
            let before = widgets::get(ctx, actor, id).await?;
            widgets::configure(ctx, actor, id, patch.clone()).await?;
            let undo = widgets::WidgetPatch {
                region: patch.region.as_ref().map(|_| before.region.clone()),
                scope: patch.scope.as_ref().map(|_| before.scope.clone()),
                title: patch.title.as_ref().map(|_| before.title.clone()),
                config: patch.config.as_ref().map(|_| before.config.clone()),
                position: patch.position.map(|_| before.sort_order),
                enabled: patch.enabled.map(|_| before.enabled),
            };
            vec![ChangeOp::WidgetConfigure {
                id: id.clone(),
                patch: undo,
            }]
        }
        ChangeOp::WidgetRemove { id } => {
            let w = widgets::remove(ctx, actor, id).await?;
            vec![ChangeOp::WidgetPlace(Box::new(widgets::NewWidget {
                kind: w.kind,
                region: w.region,
                scope: w.scope,
                title: w.title,
                config: w.config,
                position: Some(w.sort_order),
                enabled: w.enabled,
            }))]
        }
        ChangeOp::ModuleEnable { id } => {
            let was_on = ctx.extensions.set_module(ctx, actor, id, true).await?;
            if was_on {
                Vec::new()
            } else {
                vec![ChangeOp::ModuleDisable { id: id.clone() }]
            }
        }
        ChangeOp::ModuleDisable { id } => {
            let was_on = ctx.extensions.set_module(ctx, actor, id, false).await?;
            if was_on {
                vec![ChangeOp::ModuleEnable { id: id.clone() }]
            } else {
                Vec::new()
            }
        }
        ChangeOp::PresetStep { step } => run_preset_step(ctx, actor, step).await?,
    })
}

fn restore_tokens(before: &Tokens) -> Result<ChangeOp> {
    Ok(ChangeOp::TokensSet {
        tokens: serde_json::to_value(before).map_err(Error::internal)?,
    })
}

/// 프리셋 연산은 있으면 건너뛰므로, 실제로 바뀐 것만 되돌린다.
async fn run_preset_step(ctx: &Ctx, actor: &Actor, step: &presets::Op) -> Result<Vec<ChangeOp>> {
    let before_tokens = design::tokens(ctx).await?;
    let before_settings =
        serde_json::to_value(settings::site(ctx).await?).map_err(Error::internal)?;
    let before_widgets: Vec<String> = widgets::list(ctx, actor)
        .await?
        .into_iter()
        .map(|w| w.id)
        .collect();
    let res = presets::apply_op(ctx, actor, step).await?;
    if !res.changed {
        return Ok(Vec::new());
    }
    Ok(match step {
        presets::Op::BoardEnsure(_) => vec![ChangeOp::BoardDelete { slug: res.target }],
        presets::Op::PageEnsure { .. } => vec![ChangeOp::PageDelete { slug: res.target }],
        presets::Op::ModuleEnsure { id } => vec![ChangeOp::ModuleDisable { id: id.clone() }],
        presets::Op::MenuEnsure { location, .. } => vec![ChangeOp::MenuSet {
            location: location.clone(),
            items: Vec::new(),
        }],
        presets::Op::TokensEnsure { .. } => vec![restore_tokens(&before_tokens)?],
        presets::Op::SettingsEnsure { values } => vec![ChangeOp::SettingsSet {
            values: restore_values(values, &before_settings)?,
        }],
        presets::Op::WidgetsEnsure { .. } => widgets::list(ctx, actor)
            .await?
            .into_iter()
            .filter(|w| !before_widgets.contains(&w.id))
            .map(|w| ChangeOp::WidgetRemove { id: w.id })
            .collect(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 연산은_이름표로_읽고_쓴다() {
        let op: ChangeOp = serde_json::from_value(json!({
            "op": "menu.set", "location": "header", "items": []
        }))
        .unwrap();
        assert_eq!(serde_json::to_value(&op).unwrap()["op"], "menu.set");
        assert!(serde_json::from_value::<ChangeOp>(json!({ "op": "site.burn" })).is_err());
    }

    #[test]
    fn 설정_칸은_모두_되돌릴_수_있다() {
        // SettingsPatch 의 칸이 SiteSettings 에 없으면 settings.set 역연산을 못 만든다.
        let patch = serde_json::to_value(settings::SettingsPatch::default()).unwrap();
        let before = serde_json::to_value(crate::model::SiteSettings::default()).unwrap();
        for key in patch.as_object().unwrap().keys() {
            assert!(before.get(key).is_some(), "{key}");
        }
    }

    #[test]
    fn 금칙어는_글자로_되돌린다() {
        let changed = json!({ "banned_words": "새말" });
        let before = json!({ "banned_words": ["옛말", "헌말"] });
        let out = restore_values(changed.as_object().unwrap(), &before).unwrap();
        assert_eq!(out["banned_words"], "옛말\n헌말");
    }
}
