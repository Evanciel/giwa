//! 모듈 시스템 (SPEC §7, ADR-0066).
//!
//! 코어 도메인 밖의 기능은 이 트레이트만 구현해 붙는다. 공식 모듈은 `mods/<id>` 에 있고 바이너리만
//! 안다. 이 크레이트와 `maru-web` 은 `dyn Module` 만 보므로 sqlx 를 모른다.
//!
//! - 이벤트: 감사 기록이 곧 이벤트다. [`delivery`] 가 `audit_log` 를 따라가며 넘긴다.
//! - 라우트: `/m/<id>` 아래에만 놓인다. 꺼져 있으면 404.
//! - 툴: 같은 정의로 REST·CLI(나중에 MCP)가 만들어진다.

pub mod delivery;
pub mod service;

use std::sync::Arc;

use async_trait::async_trait;
use axum::Router;
use axum::http::HeaderMap;
use axum::response::Response;
use maru_core::actor::{Actor, Permission};
use maru_core::fields::FieldDef;
use maru_core::model::AuditEvent;
use maru_core::services::widgets::WidgetType;
use maru_core::store::{Extensions, ModuleMigration};
use maru_core::{Code, Ctx, Error, Result};
use serde::Serialize;
use serde_json::{Map, Value};

/// 모듈의 신분과 구독. 바이너리에 내장되므로 정적 값이다 (SPEC §7.1 의 `module.toml` 은 Phase 5 외부 모듈 때).
#[derive(Debug, Clone, Copy)]
pub struct Manifest {
    /// 영문 소문자·숫자·`_`. 표 이름(`mod_<id>_`)과 주소(`/m/<id>`)에 쓴다
    pub id: &'static str,
    pub name: &'static str,
    pub version: &'static str,
    pub description: &'static str,
    /// 받을 감사 동작 이름 (`post.publish`, `comment.create` …)
    pub events: &'static [&'static str],
}

/// 모듈이 내놓는 동작 하나. REST·CLI 가 이 정의로 만들어진다 (원칙 1).
#[derive(Debug, Clone)]
pub struct ToolDef {
    pub name: &'static str,
    pub description: &'static str,
    /// 입력의 JSON Schema. CLI 는 `properties` 로 `--칸 값` 을 해석한다
    pub input_schema: Value,
    /// 있으면 이 권한이 있어야 부를 수 있다. 더 세밀한 검사는 `call_tool` 안에서
    pub permission: Option<Permission>,
}

/// 입력 구조체의 JSON Schema. [`ToolDef::input_schema`] 를 채울 때 쓴다.
pub fn schema_of<T: schemars::JsonSchema>() -> Value {
    serde_json::to_value(schemars::schema_for!(T)).unwrap_or(Value::Null)
}

/// 모듈이 라우트에서 받는 상태.
#[derive(Clone)]
pub struct ModuleState {
    pub ctx: Ctx,
    pub host: Arc<dyn Host>,
}

/// 글 본문 아래 (반응·스크랩 단추).
pub const POST_FOOTER: &str = "post_footer";
/// 댓글마다. 템플릿에서 지금 댓글은 `c` 다.
pub const COMMENT_ACTIONS: &str = "comment_actions";
/// 내 정보 화면의 칸.
pub const ME_SECTIONS: &str = "me_sections";

/// 화면 자리를 채울 때 모듈이 보는 것.
pub struct SlotView<'a> {
    pub viewer: &'a Actor,
    pub post_id: Option<&'a str>,
    pub comment_ids: &'a [String],
}

/// 모듈이 자리에 넣는 것. 템플릿에서 `s.data` 로 읽는다.
pub struct SlotContent {
    pub template: &'static str,
    pub data: Value,
}

/// 템플릿에 넘기는 자리 하나.
#[derive(Debug, Clone, Serialize)]
pub struct SlotItem {
    pub module: &'static str,
    pub template: &'static str,
    pub data: Value,
}

/// 모듈 라우트가 웹 계층에 맡기는 일.
#[async_trait]
pub trait Host: Send + Sync {
    /// 공개 화면을 테마로 그린다. `data` 에 `site` 등 공통 값이 더해진다.
    async fn page(&self, template: &str, title: &str, data: Value) -> Response;
    /// 공개 오류 화면.
    async fn error(&self, err: &Error) -> Response;
    /// 지금 요청의 사람. 로그인하지 않았으면 손님.
    async fn viewer(&self, headers: &HeaderMap) -> Result<Actor>;
    /// 공개 화면의 폼은 쿠키 토큰이 없어(화면을 캐시할 수 있게) 출처로 위조 요청을 막는다.
    fn same_origin(&self, headers: &HeaderMap) -> bool;
}

/// 모듈. 빠진 것은 기본 구현(없음)으로 둔다.
#[async_trait]
pub trait Module: Send + Sync {
    fn manifest(&self) -> &'static Manifest;

    fn migrations(&self) -> Vec<ModuleMigration> {
        Vec::new()
    }

    /// `/m/<id>` 아래에 놓일 라우트.
    fn routes(&self) -> Option<Router<ModuleState>> {
        None
    }

    /// (템플릿 이름, 내용). 이름은 `mod/<id>/…`. 사용자 테마가 같은 이름으로 덮을 수 있다.
    fn templates(&self) -> Vec<(&'static str, &'static str)> {
        Vec::new()
    }

    fn tools(&self) -> Vec<ToolDef> {
        Vec::new()
    }

    async fn call_tool(
        &self,
        _ctx: &Ctx,
        _actor: &Actor,
        tool: &str,
        _input: Value,
    ) -> Result<Value> {
        Err(tool_not_found(self.manifest().id, tool))
    }

    /// 구독한 이벤트 처리. 같은 이벤트가 다시 올 수 있으므로 `ev.id` 등으로 멱등해야 한다.
    async fn on_event(&self, _ctx: &Ctx, _ev: &AuditEvent) -> Result<()> {
        Ok(())
    }

    /// 보존 정리 (ADR-0073). 서버가 하루 한 번, `maru privacy run` 이 바로 부른다. 지운 수를 돌려준다.
    async fn purge(&self, _ctx: &Ctx) -> Result<u64> {
        Ok(0)
    }

    fn settings_fields(&self) -> Vec<FieldDef> {
        Vec::new()
    }

    /// 설정의 기본값. 저장한 값이 이 위에 얹힌다.
    fn default_settings(&self) -> Map<String, Value> {
        Map::new()
    }

    /// 칸 정의로 못 잡는 설정 검사.
    fn check_settings(&self, _settings: &Map<String, Value>) -> Result<()> {
        Ok(())
    }

    /// 모듈이 가진 관리 화면 주소. 모듈 관리 화면에서 잇는다.
    fn admin_url(&self) -> Option<&'static str> {
        None
    }

    fn widget_types(&self) -> Vec<WidgetType> {
        Vec::new()
    }

    /// 위젯 설정을 검사하고 기본값을 채운 모양을 돌려준다.
    async fn check_widget(&self, _ctx: &Ctx, _kind: &str, config: Value) -> Result<Value> {
        Ok(config)
    }

    /// 위젯 템플릿(`mod/<id>/<kind>.html`)에 넘길 값. `more_url` 이 있으면 「더 보기」 링크가 된다.
    async fn widget_view(&self, _ctx: &Ctx, _kind: &str, _config: &Value) -> Result<Value> {
        Ok(Value::Null)
    }

    /// 화면 자리([`POST_FOOTER`] 등)에 넣을 것. 없으면 `None`.
    async fn slot(
        &self,
        _ctx: &Ctx,
        _slot: &str,
        _view: &SlotView<'_>,
    ) -> Result<Option<SlotContent>> {
        Ok(None)
    }
}

/// 모듈이 자기 문장으로 내는 오류. 문장 카탈로그는 코어 몫이라 키 대신 글자를 싣는다.
pub fn module_error(code: Code, text: impl Into<String>) -> Error {
    Error::new(code)
        .with_message("error.module_message")
        .arg("text", text.into())
}

pub fn tool_not_found(id: &str, tool: &str) -> Error {
    Error::new(Code::NotFound)
        .with_message("error.module_tool_not_found")
        .arg("module", id)
        .arg("tool", tool)
        .next(format!("maru module show {id}"))
}

/// 이 바이너리에 든 모듈 목록. 켜짐은 DB 에 있다.
#[derive(Default)]
pub struct Registry {
    modules: Vec<Arc<dyn Module>>,
}

impl Registry {
    /// id 가 겹치거나 주소에 못 쓸 모양이면 내부 오류.
    pub fn new(modules: Vec<Arc<dyn Module>>) -> Result<Self> {
        for (i, m) in modules.iter().enumerate() {
            let id = m.manifest().id;
            let good = !id.is_empty()
                && id
                    .chars()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_');
            if !good || modules[..i].iter().any(|o| o.manifest().id == id) {
                return Err(Error::internal(format!("모듈 id 가 잘못됐다: {id}")));
            }
        }
        Ok(Self { modules })
    }

    pub fn iter(&self) -> impl Iterator<Item = &Arc<dyn Module>> {
        self.modules.iter()
    }

    pub fn get(&self, id: &str) -> Option<&Arc<dyn Module>> {
        self.modules.iter().find(|m| m.manifest().id == id)
    }

    /// 켜진 모듈. DB 를 못 읽으면 경고하고 없는 것으로 친다(공개 화면이 모듈 때문에 죽지 않게).
    pub async fn enabled(&self, ctx: &Ctx) -> Vec<&Arc<dyn Module>> {
        if self.modules.is_empty() {
            return Vec::new();
        }
        match ctx.store.module_rows().await {
            Ok(rows) => self
                .modules
                .iter()
                .filter(|m| rows.iter().any(|r| r.enabled && r.id == m.manifest().id))
                .collect(),
            Err(e) => {
                tracing::warn!(error = ?e, "모듈 상태를 못 읽어 모두 꺼진 것으로 칩니다");
                Vec::new()
            }
        }
    }

    /// 켜진 모듈들이 자리에 넣는 것. 한 모듈이 실패해도 화면은 그대로 나간다 (위젯과 같다).
    pub async fn slots(
        &self,
        ctx: &Ctx,
        names: &[&str],
        view: &SlotView<'_>,
    ) -> Map<String, Value> {
        let enabled = self.enabled(ctx).await;
        let mut out = Map::new();
        for name in names {
            let mut items = Vec::new();
            for m in &enabled {
                let id = m.manifest().id;
                match m.slot(ctx, name, view).await {
                    Ok(Some(c)) => items.push(SlotItem {
                        module: id,
                        template: c.template,
                        data: c.data,
                    }),
                    Ok(None) => {}
                    Err(e) => {
                        tracing::warn!(module = id, slot = name, error = ?e, "화면 자리를 못 채웠습니다")
                    }
                }
            }
            out.insert(
                (*name).to_owned(),
                serde_json::to_value(items).unwrap_or(Value::Null),
            );
        }
        out
    }

    /// 이 위젯 종류를 가진 켜진 모듈.
    pub async fn widget_owner(&self, ctx: &Ctx, kind: &str) -> Option<&Arc<dyn Module>> {
        self.enabled(ctx)
            .await
            .into_iter()
            .find(|m| m.widget_types().iter().any(|t| t.id == kind))
    }
}

#[async_trait]
impl Extensions for Registry {
    async fn widget_types(&self, ctx: &Ctx) -> Vec<WidgetType> {
        self.enabled(ctx)
            .await
            .into_iter()
            .flat_map(|m| m.widget_types())
            .collect()
    }

    async fn check_widget(&self, ctx: &Ctx, kind: &str, config: Value) -> Option<Result<Value>> {
        let owner = self.widget_owner(ctx, kind).await?;
        Some(owner.check_widget(ctx, kind, config).await)
    }

    async fn purge(&self, ctx: &Ctx) -> u64 {
        let mut removed = 0;
        for m in self.enabled(ctx).await {
            match m.purge(ctx).await {
                Ok(n) => removed += n,
                Err(e) => {
                    tracing::warn!(module = %m.manifest().id, error = ?e, "모듈 보존 정리 실패")
                }
            }
        }
        removed
    }
}
