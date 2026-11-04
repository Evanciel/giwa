//! 공식 모듈 목록과 `maru module …`, `maru <모듈> <툴> --칸 값` (ADR-0066).

use std::sync::Arc;

use maru_core::{Code, Error, Result};
use maru_modules::delivery::deliver_once;
use maru_modules::{Module, Registry, service};
use maru_storage::Db;
use serde_json::{Map, Number, Value, json};

use crate::commands::{Out, actor, open};
use crate::{Global, ModuleCmd};

/// 이 바이너리에 든 모듈. 새 공식 모듈은 여기와 [`IDS`] 에 더한다.
pub(crate) fn registry(db: &Db) -> Result<Arc<Registry>> {
    let modules: Vec<Arc<dyn Module>> = vec![
        Arc::new(maru_mod_point::PointModule::new(db.clone())),
        Arc::new(maru_mod_reaction::ReactionModule::new(db.clone())),
        Arc::new(maru_mod_scrap::ScrapModule::new(db.clone())),
    ];
    Ok(Arc::new(Registry::new(modules)?))
}

/// 데이터 디렉터리를 열기 전에 모르는 명령을 가려내는 데 쓴다.
const IDS: [&str; 3] = ["point", "reaction", "scrap"];

fn usage(detail: impl Into<String>) -> Error {
    Error::new(Code::Usage)
        .with_message("error.usage_detail")
        .arg("detail", detail.into())
        .with_hint("hint.usage")
}

pub(crate) async fn module_cmd(g: &Global, cmd: ModuleCmd) -> Out {
    let (ctx, _config, db) = open(g).await?;
    let res = async {
        let registry = registry(&db)?;
        let reg = registry.as_ref();
        let who = actor(&ctx).await?;
        Ok::<_, Error>(match cmd {
            ModuleCmd::List => {
                let list = service::list(&ctx, &who, reg).await?;
                let lines: Vec<String> = list
                    .iter()
                    .map(|m| {
                        let state = match (m.enabled, m.installed) {
                            (true, _) => "켜짐",
                            (false, true) => "꺼짐",
                            _ => "설치 안 됨",
                        };
                        format!("{}\t{}\t{state}", m.id, m.name)
                    })
                    .collect();
                (json!(list), lines.join("\n"))
            }
            ModuleCmd::Show { id } => {
                let m = service::show(&ctx, &who, reg, &id).await?;
                let human = serde_json::to_string_pretty(&m).map_err(Error::internal)?;
                (json!(m), human)
            }
            ModuleCmd::Enable { id } => {
                let m = service::enable(&ctx, &who, reg, &id).await?;
                let human = format!("켰습니다: {} ({})", m.name, m.id);
                (json!(m), human)
            }
            ModuleCmd::Disable { id } => {
                let m = service::disable(&ctx, &who, reg, &id).await?;
                let human = format!("껐습니다: {} ({}). 데이터는 그대로입니다.", m.name, m.id);
                (json!(m), human)
            }
            ModuleCmd::Remove { id, purge } => {
                if !(purge && g.yes) {
                    return Err(Error::new(Code::Validation)
                        .with_message("error.module_remove_confirm")
                        .arg("id", id.clone())
                        .next(format!("maru module remove {id} --purge --yes")));
                }
                let r = service::remove(&ctx, &who, reg, &id).await?;
                let human = format!(
                    "제거했습니다: {} (되돌린 마이그레이션 {}개)",
                    r.id,
                    r.reverted.len()
                );
                (json!(r), human)
            }
            ModuleCmd::Set { id, field } => {
                let mut patch = Map::new();
                for f in field {
                    let (k, v) = f
                        .split_once('=')
                        .ok_or_else(|| usage(format!("--field 는 이름=값 모양입니다: {f}")))?;
                    patch.insert(k.trim().to_owned(), Value::String(v.to_owned()));
                }
                let m = service::set_settings(&ctx, &who, reg, &id, patch).await?;
                let human = format!("설정을 저장했습니다: {}", m.id);
                (json!(m), human)
            }
            ModuleCmd::Failures { id } => {
                let list = service::failures(&ctx, &who, reg, id.as_deref()).await?;
                let human = if list.is_empty() {
                    "처리하지 못한 이벤트가 없습니다.".to_owned()
                } else {
                    list.iter()
                        .map(|f| {
                            format!(
                                "{}\t{}\t{}회\t{}",
                                f.module, f.action, f.attempts, f.last_error
                            )
                        })
                        .collect::<Vec<_>>()
                        .join("\n")
                };
                (json!(list), human)
            }
            ModuleCmd::Deliver => {
                let n = deliver_once(&ctx, reg).await?;
                (
                    json!({ "delivered": n }),
                    format!("이벤트 {n}개를 넘겼습니다."),
                )
            }
        })
    }
    .await;
    db.close().await;
    res
}

/// `maru <모듈> <툴> --칸 값`. 입력 모양은 툴의 JSON Schema 를 따른다.
pub(crate) async fn external(g: &Global, args: Vec<String>) -> Out {
    let mut args = args
        .into_iter()
        .filter(|a| !matches!(a.as_str(), "--json" | "--yes" | "--quiet"));
    let id = args.next().unwrap_or_default();
    if !IDS.contains(&id.as_str()) {
        return Err(usage(format!("모르는 명령입니다: {id}")).next("maru --help"));
    }
    let (ctx, _config, db) = open(g).await?;
    let res = async {
        let registry = registry(&db)?;
        let module = registry
            .get(&id)
            .ok_or_else(|| usage(format!("모르는 모듈입니다: {id}")))?;
        let tools = module.tools();
        let names = || tools.iter().map(|t| t.name).collect::<Vec<_>>().join(", ");
        let tool = args
            .next()
            .ok_or_else(|| usage(format!("툴을 적으세요. 있는 툴: {}", names())))?;
        let def = tools.iter().find(|t| t.name == tool).ok_or_else(|| {
            usage(format!(
                "{id} 모듈에 없는 툴입니다: {tool}. 있는 툴: {}",
                names()
            ))
        })?;
        let input = parse_args(&def.input_schema, args)?;
        let who = actor(&ctx).await?;
        let out = service::call_tool(&ctx, &who, &registry, &id, &tool, input).await?;
        let human = serde_json::to_string_pretty(&out).map_err(Error::internal)?;
        Ok::<_, Error>((out, human))
    }
    .await;
    db.close().await;
    res
}

/// `--칸 값` 을 스키마의 타입대로 JSON 으로 바꾼다. `-` 는 `_` 로 읽는다.
fn parse_args(schema: &Value, args: impl Iterator<Item = String>) -> Result<Value> {
    let mut args = args;
    let mut out = Map::new();
    while let Some(flag) = args.next() {
        let Some(name) = flag.strip_prefix("--") else {
            return Err(usage(format!("--칸 값 모양이어야 합니다: {flag}")));
        };
        let key = name.replace('-', "_");
        let prop = &schema["properties"][&key];
        if prop.is_null() {
            return Err(usage(format!("모르는 칸입니다: --{name}")));
        }
        let raw = args
            .next()
            .ok_or_else(|| usage(format!("--{name} 의 값이 없습니다")))?;
        out.insert(
            key,
            typed(prop, &raw)
                .ok_or_else(|| usage(format!("--{name} 값이 맞지 않습니다: {raw}")))?,
        );
    }
    Ok(Value::Object(out))
}

fn typed(prop: &Value, raw: &str) -> Option<Value> {
    let is = |t: &str| match &prop["type"] {
        Value::String(s) => s == t,
        Value::Array(a) => a.iter().any(|v| v == t),
        _ => false,
    };
    if is("integer") {
        raw.parse::<i64>().ok().map(Value::from)
    } else if is("number") {
        raw.parse::<f64>()
            .ok()
            .and_then(Number::from_f64)
            .map(Value::Number)
    } else if is("boolean") {
        raw.parse::<bool>().ok().map(Value::Bool)
    } else {
        Some(Value::String(raw.to_owned()))
    }
}
