//! 공식 모듈 목록과 `giwa module …`, `giwa <모듈> <툴> --칸 값` (ADR-0066).

use std::path::Path;
use std::sync::Arc;

use giwa_core::{Code, Error, Result};
use giwa_modules::delivery::deliver_once;
use giwa_modules::{Module, Registry, service};
use giwa_storage::{DataDir, Db};
use serde_json::{Map, Number, Value, json};

use crate::commands::{Out, actor, open};
use crate::{Global, ModuleCmd};

/// 이 바이너리에 든 모듈과 데이터 폴더의 외부 모듈(ADR-0177). 새 공식 모듈은 여기와 [`IDS`] 에 더한다.
pub(crate) fn registry(db: &Db, data: &Path) -> Result<Arc<Registry>> {
    // 예약·쇼핑은 결제 모듈 크레이트를 모르고 `Checkout` 으로만 주문을 맡긴다 (ADR-0166).
    let pay = Arc::new(giwa_mod_pay::PayModule::new(db.clone()));
    let mut modules: Vec<Arc<dyn Module>> = vec![
        Arc::new(giwa_mod_point::PointModule::new(db.clone())),
        Arc::new(giwa_mod_reaction::ReactionModule::new(db.clone())),
        Arc::new(giwa_mod_scrap::ScrapModule::new(db.clone())),
        Arc::new(giwa_mod_notify::NotifyModule::new(db.clone())),
        Arc::new(giwa_mod_message::MessageModule::new(db.clone())),
        Arc::new(giwa_mod_poll::PollModule::new(db.clone())),
        Arc::new(giwa_mod_report::ReportModule::new(db.clone())),
        Arc::new(giwa_mod_ads::AdsModule::new()),
        Arc::new(giwa_mod_consult::ConsultModule::new()),
        Arc::new(giwa_mod_map::MapModule::new()),
        Arc::new(giwa_mod_kakao::KakaoModule::new()),
        Arc::new(giwa_mod_webhook::WebhookModule::new(db.clone())?),
        Arc::new(giwa_mod_social::SocialModule::new(db.clone())),
        Arc::new(giwa_mod_forms::FormsModule::new(db.clone())),
        pay.clone(),
        Arc::new(giwa_mod_booking::BookingModule::new(
            db.clone(),
            pay.clone(),
        )),
        Arc::new(giwa_mod_shop::ShopModule::new(db.clone(), pay)),
    ];
    let external = giwa_wasm::load(&DataDir::new(data).modules_dir(), db, &IDS);
    modules.extend(external.into_iter().map(|m| m as Arc<dyn Module>));
    Ok(Arc::new(Registry::new(modules)?))
}

/// `giwa <모듈> <툴>` 의 모듈이 이 바이너리나 데이터 폴더에 있는가.
fn known(data: &Path, id: &str) -> bool {
    IDS.contains(&id)
        || (!id.is_empty()
            && id
                .chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
            && DataDir::new(data)
                .modules_dir()
                .join(id)
                .join("module.toml")
                .is_file())
}

/// 데이터 디렉터리를 열기 전에 모르는 명령을 가려내는 데 쓴다.
const IDS: [&str; 17] = [
    "point", "reaction", "scrap", "notify", "message", "poll", "report", "ads", "consult",
    "webhook", "social", "forms", "map", "kakao", "pay", "booking", "shop",
];

fn usage(detail: impl Into<String>) -> Error {
    Error::new(Code::Usage)
        .with_message("error.usage_detail")
        .arg("detail", detail.into())
        .with_hint("hint.usage")
}

pub(crate) async fn module_cmd(g: &Global, cmd: ModuleCmd) -> Out {
    let (ctx, _config, db) = open(g).await?;
    let res = async {
        let registry = registry(&db, &g.data)?;
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
                        .next(format!("giwa module remove {id} --purge --yes")));
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

/// `giwa <모듈> <툴> --칸 값`. 입력 모양은 툴의 JSON Schema 를 따른다.
pub(crate) async fn external(g: &Global, args: Vec<String>) -> Out {
    let mut args = args
        .into_iter()
        .filter(|a| !matches!(a.as_str(), "--json" | "--yes" | "--quiet"));
    let id = args.next().unwrap_or_default();
    if !known(&g.data, &id) {
        return Err(usage(format!("모르는 명령입니다: {id}")).next("giwa --help"));
    }
    let (ctx, _config, db) = open(g).await?;
    let res = async {
        let registry = registry(&db, &g.data)?;
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
        if is_array(prop) {
            out.entry(key)
                .or_insert_with(|| Value::Array(Vec::new()))
                .as_array_mut()
                .ok_or_else(|| usage(format!("--{name} 값이 맞지 않습니다: {raw}")))?
                .push(Value::String(raw));
            continue;
        }
        out.insert(
            key,
            typed(prop, &raw)
                .ok_or_else(|| usage(format!("--{name} 값이 맞지 않습니다: {raw}")))?,
        );
    }
    Ok(Value::Object(out))
}

/// 배열 칸인가. 문자열 배열만 받는다(`--칸 값` 을 되풀이).
fn is_array(prop: &Value) -> bool {
    match &prop["type"] {
        Value::String(s) => s == "array",
        Value::Array(a) => a.iter().any(|v| v == "array"),
        _ => false,
    }
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
