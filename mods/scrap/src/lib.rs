//! 스크랩 모듈 (GAP M8, ADR-0068). 회원이 글을 모아 두고 내 정보와 모음 화면에서 다시 본다.

use std::sync::Arc;

use async_trait::async_trait;
use axum::extract::{Form, Path, Query, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Redirect, Response};
use axum::routing::{get, post};
use axum::{Extension, Router};
use giwa_core::actor::{Actor, Permission};
use giwa_core::audit::{AuditRecord, Target};
use giwa_core::model::AuditEvent;
use giwa_core::services::{boards, posts, settings};
use giwa_core::store::ModuleMigration;
use giwa_core::urls::{self, Target as UrlTarget};
use giwa_core::{Code, Ctx, Error, Result, clock};
use giwa_modules::{
    ACCOUNT_BAR, ME_SECTIONS, Manifest, Module, ModuleState, POST_FOOTER, SlotContent, SlotView,
    ToolDef, module_error, schema_of,
};
use giwa_storage::Db;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

static MANIFEST: Manifest = Manifest {
    id: "scrap",
    name: "스크랩",
    version: env!("CARGO_PKG_VERSION"),
    description: "회원이 글을 모아 두고 내 정보에서 다시 봅니다. 게시판 즐겨찾기도 맡습니다.",
    events: &["member.withdraw"],
};

const PER_PAGE: i64 = 20;
const ME_LIMIT: i64 = 5;
/// 즐겨찾는 게시판 수 한도 (GAP Q26, ADR-0108).
const MAX_BOARDS: i64 = 30;

#[derive(Clone)]
pub struct ScrapModule {
    db: Db,
}

/// 모음의 한 줄. 지워졌거나 볼 수 없게 된 글은 제목을 비운다.
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct ScrapRow {
    pub post_id: String,
    pub title: Option<String>,
    pub url: Option<String>,
    pub board: Option<String>,
    pub date: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct ListInput {
    /// 회원 ID. 비우면 나. 다른 회원은 회원 관리 권한이 있어야 한다
    #[serde(default)]
    member: Option<String>,
    #[serde(default)]
    page: Option<i64>,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct ToggleInput {
    post: String,
}

#[derive(Debug, Default, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct Empty {}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct BoardToggleInput {
    /// 게시판 주소 이름(slug)
    board: String,
}

fn parse_input<T: for<'de> Deserialize<'de>>(input: Value) -> Result<T> {
    serde_json::from_value(input).map_err(|e| {
        Error::new(Code::Validation)
            .with_message("error.json_body")
            .arg("detail", e.to_string())
    })
}

fn gone() -> Error {
    module_error(Code::NotFound, "스크랩할 글을 찾을 수 없습니다.")
}

impl ScrapModule {
    pub fn new(db: Db) -> Self {
        Self { db }
    }

    /// 이 사람이 지금 볼 수 있는 글의 주소와 제목·게시판 이름.
    async fn visible(
        &self,
        ctx: &Ctx,
        actor: &Actor,
        post_id: &str,
    ) -> Result<(String, String, String)> {
        let post = ctx.store.post_by_id(post_id).await?.ok_or_else(gone)?;
        let board = ctx
            .store
            .board_by_id(&post.board_id)
            .await?
            .ok_or_else(gone)?;
        let (board, post) = posts::public_post(ctx, actor, &board.slug, &post.slug)
            .await
            .map_err(|e| {
                if e.code() == Code::NotFound {
                    gone()
                } else {
                    e
                }
            })?;
        let url = urls::path(&UrlTarget::Post {
            board: &board.slug,
            post: &post.slug,
        });
        Ok((url, post.title, board.name))
    }

    /// 넣거나 뺀다. 지금 들어 있는지와 글 주소를 돌려준다.
    async fn toggle(
        &self,
        ctx: &Ctx,
        actor: &Actor,
        member_id: &str,
        post_id: &str,
    ) -> Result<(bool, Option<String>)> {
        let mut tx = self
            .db
            .write_pool()
            .begin_with("BEGIN IMMEDIATE")
            .await
            .map_err(Error::internal)?;
        let removed = sqlx::query!(
            "DELETE FROM mod_scrap_item WHERE member_id = ? AND post_id = ?",
            member_id,
            post_id
        )
        .execute(&mut *tx)
        .await
        .map_err(Error::internal)?
        .rows_affected()
            > 0;
        // 빼는 일은 글이 사라진 뒤에도 된다. 넣는 일은 볼 수 있는 글만.
        let url = self.visible(ctx, actor, post_id).await.map(|(url, ..)| url);
        let url = match url {
            Ok(u) => Some(u),
            Err(_) if removed => None,
            Err(e) => return Err(e),
        };
        if !removed {
            let at = clock::now();
            sqlx::query!(
                "INSERT INTO mod_scrap_item (member_id, post_id, at) VALUES (?, ?, ?)",
                member_id,
                post_id,
                at
            )
            .execute(&mut *tx)
            .await
            .map_err(Error::internal)?;
        }
        let audit = AuditRecord::new(
            actor,
            if removed { "scrap.remove" } else { "scrap.add" },
            Target::new("post", post_id),
            json!({}),
        );
        giwa_storage::sqlite::insert_audit(&mut tx, &audit).await?;
        tx.commit().await.map_err(Error::internal)?;
        Ok((!removed, url))
    }

    /// 새로 넣은 것부터. (줄들, 전체 수)
    async fn rows(
        &self,
        ctx: &Ctx,
        actor: &Actor,
        member_id: &str,
        limit: i64,
        offset: i64,
    ) -> Result<(Vec<ScrapRow>, i64)> {
        let items = sqlx::query!(
            "SELECT post_id, at FROM mod_scrap_item WHERE member_id = ?
             ORDER BY at DESC, post_id DESC LIMIT ? OFFSET ?",
            member_id,
            limit,
            offset
        )
        .fetch_all(self.db.read_pool())
        .await
        .map_err(Error::internal)?;
        let total = sqlx::query_scalar!(
            r#"SELECT COUNT(*) AS "n!: i64" FROM mod_scrap_item WHERE member_id = ?"#,
            member_id
        )
        .fetch_one(self.db.read_pool())
        .await
        .map_err(Error::internal)?;
        let tz = settings::site(ctx).await?.tz_offset_min;
        let mut out = Vec::with_capacity(items.len());
        for item in items {
            let (url, title, board) = match self.visible(ctx, actor, &item.post_id).await {
                Ok((u, t, b)) => (Some(u), Some(t), Some(b)),
                Err(_) => (None, None, None),
            };
            out.push(ScrapRow {
                post_id: item.post_id,
                title,
                url,
                board,
                date: clock::display_date(&item.at, tz),
            });
        }
        Ok((out, total))
    }

    /// 게시판 즐겨찾기를 넣거나 뺀다 (GAP Q26, ADR-0108). 빼는 일은 언제나, 넣는 일은 읽을 수 있는 게시판만.
    async fn toggle_board(
        &self,
        ctx: &Ctx,
        actor: &Actor,
        member_id: &str,
        slug: &str,
    ) -> Result<bool> {
        let board = ctx
            .store
            .board_by_slug(slug)
            .await?
            .ok_or_else(|| module_error(Code::NotFound, "게시판을 찾을 수 없습니다."))?;
        let mut tx = self
            .db
            .write_pool()
            .begin_with("BEGIN IMMEDIATE")
            .await
            .map_err(Error::internal)?;
        let removed = sqlx::query!(
            "DELETE FROM mod_scrap_board WHERE member_id = ? AND board_id = ?",
            member_id,
            board.id
        )
        .execute(&mut *tx)
        .await
        .map_err(Error::internal)?
        .rows_affected()
            > 0;
        if !removed {
            boards::readable(ctx, actor, slug).await?;
            let n = sqlx::query_scalar!(
                r#"SELECT COUNT(*) AS "n!: i64" FROM mod_scrap_board WHERE member_id = ?"#,
                member_id
            )
            .fetch_one(&mut *tx)
            .await
            .map_err(Error::internal)?;
            if n >= MAX_BOARDS {
                return Err(module_error(
                    Code::Validation,
                    "즐겨찾는 게시판은 30개까지입니다. 하나를 빼고 다시 넣으세요.",
                ));
            }
            let at = clock::now();
            sqlx::query!(
                "INSERT INTO mod_scrap_board (member_id, board_id, at) VALUES (?, ?, ?)",
                member_id,
                board.id,
                at
            )
            .execute(&mut *tx)
            .await
            .map_err(Error::internal)?;
        }
        let audit = AuditRecord::new(
            actor,
            if removed {
                "scrap.board_remove"
            } else {
                "scrap.board_add"
            },
            Target::new("board", &board.id),
            json!({}),
        );
        giwa_storage::sqlite::insert_audit(&mut tx, &audit).await?;
        tx.commit().await.map_err(Error::internal)?;
        Ok(!removed)
    }

    /// 즐겨찾는 게시판, 넣은 차례대로. 지웠거나 이제 읽을 수 없는 게시판은 건너뛴다.
    async fn favorite_boards(
        &self,
        ctx: &Ctx,
        actor: &Actor,
        member_id: &str,
    ) -> Result<Vec<Value>> {
        let ids = sqlx::query_scalar!(
            "SELECT board_id FROM mod_scrap_board WHERE member_id = ? ORDER BY at, board_id LIMIT ?",
            member_id,
            MAX_BOARDS
        )
        .fetch_all(self.db.read_pool())
        .await
        .map_err(Error::internal)?;
        let mut out = Vec::with_capacity(ids.len());
        for id in ids {
            let Some(board) = ctx.store.board_by_id(&id).await? else {
                continue;
            };
            if boards::readable(ctx, actor, &board.slug).await.is_err() {
                continue;
            }
            out.push(json!({
                "slug": board.slug,
                "name": board.name,
                "url": urls::path(&UrlTarget::Board { board: &board.slug }),
            }));
        }
        Ok(out)
    }

    async fn has(&self, member_id: &str, post_id: &str) -> Result<bool> {
        let n = sqlx::query_scalar!(
            r#"SELECT COUNT(*) AS "n!: i64" FROM mod_scrap_item WHERE member_id = ? AND post_id = ?"#,
            member_id,
            post_id
        )
        .fetch_one(self.db.read_pool())
        .await
        .map_err(Error::internal)?;
        Ok(n > 0)
    }

    async fn tool_list(&self, ctx: &Ctx, actor: &Actor, input: ListInput) -> Result<Value> {
        let member = match input.member {
            Some(m) if actor.member_id() != Some(&m) => {
                actor.require(Permission::MemberManage)?;
                m
            }
            Some(m) => m,
            None => actor.member_id().cloned().ok_or_else(|| {
                module_error(Code::Validation, "회원을 적으세요: --member <회원 ID>")
            })?,
        };
        let page = input.page.unwrap_or(1).max(1);
        let (items, total) = self
            .rows(ctx, actor, &member, PER_PAGE, (page - 1) * PER_PAGE)
            .await?;
        Ok(json!({ "items": items, "total": total, "page": page }))
    }

    async fn tool_toggle(&self, ctx: &Ctx, actor: &Actor, input: ToggleInput) -> Result<Value> {
        let Some(member) = actor.member_id() else {
            return Err(module_error(
                Code::Forbidden,
                "스크랩은 회원 본인만 할 수 있습니다.",
            ));
        };
        let (on, _) = self.toggle(ctx, actor, member, &input.post).await?;
        Ok(json!({ "scrapped": on }))
    }

    async fn tool_boards(&self, ctx: &Ctx, actor: &Actor) -> Result<Value> {
        let Some(member) = actor.member_id() else {
            return Err(module_error(
                Code::Forbidden,
                "즐겨찾는 게시판은 회원 본인만 봅니다.",
            ));
        };
        Ok(json!({ "boards": self.favorite_boards(ctx, actor, member).await? }))
    }

    async fn tool_board_toggle(
        &self,
        ctx: &Ctx,
        actor: &Actor,
        input: BoardToggleInput,
    ) -> Result<Value> {
        let Some(member) = actor.member_id() else {
            return Err(module_error(
                Code::Forbidden,
                "게시판 즐겨찾기는 회원 본인만 할 수 있습니다.",
            ));
        };
        let on = self.toggle_board(ctx, actor, member, &input.board).await?;
        Ok(json!({ "favorite": on }))
    }
}

#[async_trait]
impl Module for ScrapModule {
    fn manifest(&self) -> &'static Manifest {
        &MANIFEST
    }

    fn migrations(&self) -> Vec<ModuleMigration> {
        vec![
            ModuleMigration {
                version: 20261105300001,
                name: "scrap",
                up: include_str!("../migrations/20261105300001_scrap.up.sql"),
                down: include_str!("../migrations/20261105300001_scrap.down.sql"),
            },
            ModuleMigration {
                version: 20261106300001,
                name: "scrap_board",
                up: include_str!("../migrations/20261106300001_scrap_board.up.sql"),
                down: include_str!("../migrations/20261106300001_scrap_board.down.sql"),
            },
        ]
    }

    fn routes(&self) -> Option<Router<ModuleState>> {
        Some(
            Router::new()
                .route("/", get(list_page))
                .route("/post/{id}", post(toggle_form))
                .route("/board/{slug}", post(board_form))
                .layer(Extension(Arc::new(self.clone()))),
        )
    }

    fn templates(&self) -> Vec<(&'static str, &'static str)> {
        vec![
            (
                "mod/scrap/post.html",
                include_str!("../templates/post.html"),
            ),
            ("mod/scrap/me.html", include_str!("../templates/me.html")),
            (
                "mod/scrap/list.html",
                include_str!("../templates/list.html"),
            ),
        ]
    }

    fn tools(&self) -> Vec<ToolDef> {
        vec![
            ToolDef {
                name: "list",
                description: "회원의 스크랩 목록",
                input_schema: schema_of::<ListInput>(),
                permission: None,
            },
            ToolDef {
                name: "toggle",
                description: "회원 본인의 스크랩을 넣거나 뺀다",
                input_schema: schema_of::<ToggleInput>(),
                permission: None,
            },
            ToolDef {
                name: "boards",
                description: "회원 본인이 즐겨찾는 게시판",
                input_schema: schema_of::<Empty>(),
                permission: None,
            },
            ToolDef {
                name: "board_toggle",
                description: "회원 본인의 게시판 즐겨찾기를 넣거나 뺀다",
                input_schema: schema_of::<BoardToggleInput>(),
                permission: None,
            },
        ]
    }

    async fn call_tool(&self, ctx: &Ctx, actor: &Actor, tool: &str, input: Value) -> Result<Value> {
        match tool {
            "list" => self.tool_list(ctx, actor, parse_input(input)?).await,
            "toggle" => self.tool_toggle(ctx, actor, parse_input(input)?).await,
            "boards" => {
                let _: Empty = parse_input(input)?;
                self.tool_boards(ctx, actor).await
            }
            "board_toggle" => {
                self.tool_board_toggle(ctx, actor, parse_input(input)?)
                    .await
            }
            other => Err(giwa_modules::tool_not_found(MANIFEST.id, other)),
        }
    }

    /// 탈퇴한 회원의 모음은 남기지 않는다.
    async fn on_event(&self, _ctx: &Ctx, ev: &AuditEvent) -> Result<()> {
        if ev.action == "member.withdraw"
            && let Some(member) = &ev.target_id
        {
            sqlx::query!("DELETE FROM mod_scrap_item WHERE member_id = ?", member)
                .execute(self.db.write_pool())
                .await
                .map_err(Error::internal)?;
            sqlx::query!("DELETE FROM mod_scrap_board WHERE member_id = ?", member)
                .execute(self.db.write_pool())
                .await
                .map_err(Error::internal)?;
        }
        Ok(())
    }

    async fn slot(
        &self,
        ctx: &Ctx,
        slot: &str,
        view: &SlotView<'_>,
    ) -> Result<Option<SlotContent>> {
        match slot {
            // 이름 메뉴의 「스크랩」 (GAP Q14, ADR-0099)
            // 즐겨찾는 게시판과 별 단추 주소도 함께 준다 (GAP Q26, ADR-0108).
            ACCOUNT_BAR => {
                let Some(member) = view.viewer.member_id() else {
                    return Ok(None);
                };
                let favorites = self.favorite_boards(ctx, view.viewer, member).await?;
                Ok(Some(SlotContent {
                    template: "",
                    data: json!({
                        "label": "스크랩",
                        "url": "/m/scrap",
                        "menu": true,
                        "favorite_boards": favorites,
                        "board_star": "/m/scrap/board/",
                    }),
                }))
            }
            POST_FOOTER => {
                let Some(post_id) = view.post_id else {
                    return Ok(None);
                };
                let pressed = match view.viewer.member_id() {
                    Some(m) => self.has(m, post_id).await?,
                    None => false,
                };
                Ok(Some(SlotContent {
                    template: "mod/scrap/post.html",
                    data: json!({
                        "action": format!("/m/scrap/post/{post_id}"),
                        "pressed": pressed,
                    }),
                }))
            }
            ME_SECTIONS => {
                let Some(member) = view.viewer.member_id() else {
                    return Ok(None);
                };
                let (items, total) = self.rows(ctx, view.viewer, member, ME_LIMIT, 0).await?;
                Ok(Some(SlotContent {
                    template: "mod/scrap/me.html",
                    data: json!({ "items": items, "total": total, "more_url": "/m/scrap" }),
                }))
            }
            _ => Ok(None),
        }
    }
}

#[derive(Debug, Default, Deserialize)]
struct ToggleForm {
    /// `list` 면 모음 화면으로 돌아간다.
    #[serde(default)]
    from: String,
    #[serde(default)]
    page: Option<i64>,
}

async fn toggle_form(
    State(state): State<ModuleState>,
    Extension(module): Extension<Arc<ScrapModule>>,
    Path(id): Path<String>,
    headers: HeaderMap,
    Form(f): Form<ToggleForm>,
) -> Response {
    if !state.host.same_origin(&headers) {
        return StatusCode::FORBIDDEN.into_response();
    }
    let actor = match state.host.viewer(&headers).await {
        Ok(a) => a,
        Err(e) => return state.host.error(&e).await,
    };
    let Some(member) = actor.member_id() else {
        let next = match module.visible(&state.ctx, &actor, &id).await {
            Ok((url, ..)) => url,
            Err(e) => return state.host.error(&e).await,
        };
        return Redirect::to(&urls::with_query("/login", &[("next", next.as_str())]))
            .into_response();
    };
    match module.toggle(&state.ctx, &actor, member, &id).await {
        Ok((_, url)) => {
            if f.from == "list" {
                let page = f.page.unwrap_or(1).max(1);
                return Redirect::to(&format!("/m/scrap?page={page}")).into_response();
            }
            Redirect::to(&format!(
                "{}#reactions",
                url.unwrap_or_else(|| "/m/scrap".into())
            ))
            .into_response()
        }
        Err(e) => state.host.error(&e).await,
    }
}

/// 게시판 머리의 별 단추 (GAP Q26, ADR-0108). 스크립트는 JSON 을, 폼은 게시판으로 돌아가기를 받는다.
async fn board_form(
    State(state): State<ModuleState>,
    Extension(module): Extension<Arc<ScrapModule>>,
    Path(slug): Path<String>,
    headers: HeaderMap,
) -> Response {
    if !state.host.same_origin(&headers) {
        return StatusCode::FORBIDDEN.into_response();
    }
    let actor = match state.host.viewer(&headers).await {
        Ok(a) => a,
        Err(e) => return state.host.error(&e).await,
    };
    let back = urls::path(&UrlTarget::Board { board: &slug });
    let Some(member) = actor.member_id() else {
        return Redirect::to(&urls::with_query("/login", &[("next", back.as_str())]))
            .into_response();
    };
    match module.toggle_board(&state.ctx, &actor, member, &slug).await {
        Ok(on) => {
            let wants_json = headers
                .get(header::ACCEPT)
                .and_then(|v| v.to_str().ok())
                .is_some_and(|a| a.contains("application/json"));
            if wants_json {
                axum::Json(json!({ "favorite": on })).into_response()
            } else {
                Redirect::to(&back).into_response()
            }
        }
        Err(e) => state.host.error(&e).await,
    }
}

#[derive(Debug, Default, Deserialize)]
struct ListQuery {
    #[serde(default)]
    page: Option<i64>,
}

async fn list_page(
    State(state): State<ModuleState>,
    Extension(module): Extension<Arc<ScrapModule>>,
    Query(q): Query<ListQuery>,
    headers: HeaderMap,
) -> Response {
    let actor = match state.host.viewer(&headers).await {
        Ok(a) => a,
        Err(e) => return state.host.error(&e).await,
    };
    let Some(member) = actor.member_id() else {
        return Redirect::to(&urls::with_query("/login", &[("next", "/m/scrap")])).into_response();
    };
    let page = q.page.unwrap_or(1).max(1);
    match module
        .rows(&state.ctx, &actor, member, PER_PAGE, (page - 1) * PER_PAGE)
        .await
    {
        Ok((items, total)) => {
            state
                .host
                .page(
                    "mod/scrap/list.html",
                    "내 스크랩",
                    json!({
                        "items": items,
                        "total": total,
                        "page": page,
                        "prev": (page > 1).then_some(page - 1),
                        "next": (page * PER_PAGE < total).then_some(page + 1),
                        "noindex": true,
                    }),
                )
                .await
        }
        Err(e) => state.host.error(&e).await,
    }
}
