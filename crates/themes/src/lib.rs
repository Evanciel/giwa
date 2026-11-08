//! Maru 기본 테마와 프리셋
//!
//! 템플릿은 소스 목록을 순서대로 찾는 겹 구조다: 사용자 테마 디렉터리 → 내장 테마 (PHASE1_PLAN §2).
//! Phase 3 changeset 미리보기는 이 목록 앞에 오버레이 소스 하나를 더하면 된다.

use std::borrow::Cow;
use std::collections::HashMap;
use std::path::{Component, Path, PathBuf};
use std::sync::OnceLock;

use maru_core::services::presets::Preset;
use maru_core::{Code, Error, Result};
use minijinja::{AutoEscape, Environment, UndefinedBehavior};
use rust_embed::RustEmbed;
use serde::Serialize;

/// 내장 테마가 지키는 템플릿 계약 버전 (`theme.toml` 의 `template_api`).
pub const TEMPLATE_API: u32 = 1;

#[derive(RustEmbed)]
#[folder = "builtin/"]
struct Builtin;

pub struct Theme {
    env: Environment<'static>,
    /// 내장 스킨 (ADR-0081). 화면의 `site.skin` 으로 고른다. 없거나 모르는 값이면 `env`.
    skins: Vec<(&'static str, Environment<'static>)>,
    user_dir: Option<PathBuf>,
}

impl Theme {
    /// `user_dir` 는 `<data>/themes/<이름>`. 없거나 비어 있으면 내장 테마만 쓴다.
    pub fn new(user_dir: Option<PathBuf>) -> Self {
        Self::with_templates(user_dir, Vec::new())
    }

    /// 모듈 템플릿 `(이름, 내용)` 을 더한다 (ADR-0066). 사용자 테마의 같은 이름이 먼저다.
    pub fn with_templates(
        user_dir: Option<PathBuf>,
        extra: Vec<(&'static str, &'static str)>,
    ) -> Self {
        let user_dir = user_dir.filter(|d| compatible(d));
        let env = public_env(user_dir.clone(), extra.clone(), None);
        let skins = maru_core::model::SKINS
            .iter()
            .skip(1)
            .map(|(id, _)| (*id, public_env(user_dir.clone(), extra.clone(), Some(id))))
            .collect();
        Self {
            env,
            skins,
            user_dir,
        }
    }

    pub fn render<S: Serialize>(&self, name: &str, ctx: &S) -> Result<String> {
        let value = minijinja::Value::from(minijinja::value::Serde(ctx));
        let skin = value.get_attr("site").and_then(|s| s.get_attr("skin")).ok();
        let env = skin
            .as_ref()
            .and_then(|s| s.as_str())
            .and_then(|s| self.skins.iter().find(|(id, _)| *id == s))
            .map_or(&self.env, |(_, e)| e);
        let tmpl = env.get_template(name).map_err(template_error)?;
        tmpl.render(value).map_err(template_error)
    }

    /// 정적 파일(`/assets/...`). 사용자 테마가 우선이다.
    pub fn asset(&self, path: &str) -> Option<(Cow<'static, [u8]>, &'static str)> {
        let mime = mime_for(path)?;
        if let Some(dir) = self.user_dir.as_deref()
            && let Some(p) = safe_join(&dir.join("static"), path)
            && let Ok(bytes) = std::fs::read(p)
        {
            return Some((Cow::Owned(bytes), mime));
        }
        Builtin::get(&format!("static/{path}")).map(|f| (f.data, mime))
    }
}

/// 공개 화면 템플릿 환경. 찾는 순서: 사용자 테마 → 스킨(`skins/<id>/templates`) → 모듈 템플릿 → 내장.
/// 스킨은 몇 장(base·home)만 갈아 끼우고 나머지는 내장 템플릿을 같이 쓴다 (ADR-0081).
fn public_env(
    dir: Option<PathBuf>,
    extra: Vec<(&'static str, &'static str)>,
    skin: Option<&'static str>,
) -> Environment<'static> {
    let mut env = Environment::new();
    env.set_undefined_behavior(UndefinedBehavior::Chainable);
    env.set_formatter(html_formatter);
    env.add_function("url_for", url_for);
    env.add_filter("ago", ago);
    env.add_filter("short_count", short_count);
    env.add_filter("stickers", stickers_filter);
    env.add_filter("t", t);
    env.set_loader(move |name| {
        if let Some(text) = dir.as_deref().and_then(|d| read_user(d, "templates", name)) {
            return Ok(Some(text));
        }
        if let Some(text) = skin
            .and_then(|s| Builtin::get(&format!("skins/{s}/templates/{name}")))
            .and_then(|f| String::from_utf8(f.data.into_owned()).ok())
        {
            return Ok(Some(text));
        }
        if let Some((_, text)) = extra.iter().find(|(n, _)| *n == name) {
            return Ok(Some((*text).to_owned()));
        }
        Ok(Builtin::get(&format!("templates/{name}"))
            .and_then(|f| String::from_utf8(f.data.into_owned()).ok()))
    });
    env
}

/// 관리 화면 템플릿. 사용자 테마로 덮어쓸 수 없게 내장 파일만 읽는다 (PHASE1_PLAN §7).
#[derive(RustEmbed)]
#[folder = "admin/"]
struct AdminFiles;

pub struct Admin {
    env: Environment<'static>,
}

impl Default for Admin {
    fn default() -> Self {
        Self::new()
    }
}

impl Admin {
    pub fn new() -> Self {
        let mut env = Environment::new();
        env.set_undefined_behavior(UndefinedBehavior::Chainable);
        env.set_formatter(html_formatter);
        env.add_function("url_for", url_for);
        env.set_loader(|name| {
            Ok(AdminFiles::get(&format!("templates/{name}"))
                .and_then(|f| String::from_utf8(f.data.into_owned()).ok()))
        });
        Self { env }
    }

    pub fn render<S: Serialize>(&self, name: &str, ctx: &S) -> Result<String> {
        let tmpl = self.env.get_template(name).map_err(template_error)?;
        tmpl.render(minijinja::Value::from(minijinja::value::Serde(ctx)))
            .map_err(template_error)
    }

    /// `/admin/assets/...`
    pub fn asset(&self, path: &str) -> Option<(Cow<'static, [u8]>, &'static str)> {
        let mime = mime_for(path)?;
        safe_join(Path::new("static"), path)?;
        AdminFiles::get(&format!("static/{path}")).map(|f| (f.data, mime))
    }
}

/// 사용자 테마 파일. 경로 탈출이면 없는 것으로 본다.
fn read_user(dir: &Path, sub: &str, name: &str) -> Option<String> {
    let path = safe_join(&dir.join(sub), name)?;
    std::fs::read_to_string(path).ok()
}

/// `..`, 절대 경로, 드라이브 접두어, 역슬래시를 거부한다.
/// 템플릿용 URL 빌더 (ADR-0017). `url_for("post", board, slug)`, `url_for("page", slug)` 등.
fn url_for(
    kind: &str,
    a: Option<String>,
    b: Option<String>,
) -> std::result::Result<String, minijinja::Error> {
    use maru_core::urls::{Target, path};
    let a = a.unwrap_or_default();
    let b = b.unwrap_or_default();
    let target = match kind {
        "home" => Target::Home,
        "board" => Target::Board { board: &a },
        "post" => Target::Post {
            board: &a,
            post: &b,
        },
        "page" => Target::Page { page: &a },
        "sitemap" => Target::Sitemap,
        "rss" => Target::Rss,
        _ => {
            return Err(minijinja::Error::new(
                minijinja::ErrorKind::InvalidOperation,
                format!("url_for: 알 수 없는 대상 `{kind}`"),
            ));
        }
    };
    Ok(path(&target))
}

/// 사용자 테마의 `theme.toml` 이 있으면 `template_api` 가 같아야 쓴다.
/// 없으면 템플릿 몇 개만 덮어쓴 테마로 보고 받아들인다.
pub fn compatible(dir: &Path) -> bool {
    #[derive(serde::Deserialize)]
    struct Manifest {
        template_api: u32,
    }
    let Ok(text) = std::fs::read_to_string(dir.join("theme.toml")) else {
        return true;
    };
    match toml::from_str::<Manifest>(&text) {
        Ok(m) if m.template_api == TEMPLATE_API => true,
        Ok(m) => {
            tracing::warn!(
                dir = %dir.display(),
                found = m.template_api,
                expected = TEMPLATE_API,
                "사용자 테마의 template_api 가 맞지 않아 내장 테마를 씁니다"
            );
            false
        }
        Err(e) => {
            tracing::warn!(dir = %dir.display(), error = %e, "사용자 테마의 theme.toml 을 읽지 못해 내장 테마를 씁니다");
            false
        }
    }
}

/// 관리 화면 썸네일(`admin/static/shots/<name>.webp`)이 있는가 (ADR-0141).
/// `scripts/theme_shots.py` 가 실제 화면을 찍어 만든다. 없으면 카드는 견본 그림을 쓴다.
pub fn has_shot(name: &str) -> bool {
    AdminFiles::get(&format!("static/shots/{name}.webp")).is_some()
}

/// 내장 프리셋 id 목록.
pub fn preset_ids() -> Vec<String> {
    Builtin::iter()
        .filter_map(|p| {
            p.strip_prefix("presets/")
                .and_then(|n| n.strip_suffix(".toml"))
                .map(str::to_owned)
        })
        .collect()
}

/// 내장 프리셋을 읽는다.
pub fn preset(id: &str) -> Result<Preset> {
    let unknown = || {
        Error::new(Code::Validation)
            .with_message("error.preset_unknown")
            .arg("value", id.to_owned())
            .arg("allowed", preset_ids().join(", "))
    };
    if !id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-') {
        return Err(unknown());
    }
    let file = Builtin::get(&format!("presets/{id}.toml")).ok_or_else(unknown)?;
    let text = std::str::from_utf8(&file.data).map_err(Error::internal)?;
    toml::from_str(text).map_err(|e| {
        Error::new(Code::Internal)
            .with_message("error.preset_invalid")
            .arg("id", id.to_owned())
            .arg("detail", e.to_string())
    })
}

/// HTML 이스케이프는 `& < > " '` 만 한다. 기본 포매터는 `/` 까지 `&#x2f;` 로 바꿔
/// 모든 링크가 읽기 어려워진다. 속성값은 항상 큰따옴표로 감싸므로 `/` 는 안전하다.
/// 「방금 전」「5분 전」「3시간 전」, 하루가 넘으면 날짜 `2026. 11. 4.` (GAP Q11, ADR-0096).
/// `tz` 는 사이트 시간대(분, 기본 한국 +540). 시각이 아니면 빈 문자열.
/// 영어 사이트는 「just now」「5 min ago」「3 hr ago」. 날짜는 숫자뿐이라 그대로 둔다.
fn ago(state: &minijinja::State, at: minijinja::Value, tz: Option<i32>) -> String {
    ago_in(is_en(state), at, tz)
}

fn ago_in(en: bool, at: minijinja::Value, tz: Option<i32>) -> String {
    let Some(at) = at.as_str() else {
        return String::new();
    };
    match (maru_core::clock::seconds_since(at), en) {
        // 시계가 조금 어긋나 앞으로 온 시각도 「방금」으로 본다.
        (Some(s), false) if (-300..60).contains(&s) => "방금 전".to_owned(),
        (Some(s), false) if (60..3600).contains(&s) => format!("{}분 전", s / 60),
        (Some(s), false) if (3600..86_400).contains(&s) => format!("{}시간 전", s / 3600),
        (Some(s), true) if (-300..60).contains(&s) => "just now".to_owned(),
        (Some(s), true) if (60..3600).contains(&s) => format!("{} min ago", s / 60),
        (Some(s), true) if (3600..86_400).contains(&s) => format!("{} hr ago", s / 3600),
        _ => maru_core::clock::display_date(at, tz.unwrap_or(540)),
    }
}

/// 수 줄여 쓰기 (ADR-0096): 9,999 까지는 쉼표, 1만부터 「1.2만」, 1억부터 「1.2억」.
/// 소수 첫째 자리에서 버린다(실제보다 많아 보이지 않게). 수가 아니면 그대로.
/// 영어 사이트는 1,000 부터 「1.2K」, 100만부터 「1.2M」, 10억부터 「1.2B」.
fn short_count(state: &minijinja::State, n: minijinja::Value) -> String {
    short_count_in(is_en(state), n)
}

fn short_count_in(en: bool, n: minijinja::Value) -> String {
    let Some(n) = n.as_i64() else {
        return n.to_string();
    };
    let (unit, div) = match (n.unsigned_abs(), en) {
        (a, false) if a >= 100_000_000 => ("억", 100_000_000),
        (a, false) if a >= 10_000 => ("만", 10_000),
        (a, true) if a >= 1_000_000_000 => ("B", 1_000_000_000),
        (a, true) if a >= 1_000_000 => ("M", 1_000_000),
        (a, true) if a >= 1_000 => ("K", 1_000),
        _ => return grouped(n),
    };
    let tenths = n / (div / 10);
    let (whole, frac) = (tenths / 10, (tenths % 10).abs());
    if frac == 0 {
        format!("{whole}{unit}")
    } else {
        format!("{whole}.{frac}{unit}")
    }
}

/// 공개 화면 영어 문구 (ADR-0134). 키는 템플릿의 한국어 원문이다.
fn english() -> &'static HashMap<String, String> {
    static EN: OnceLock<HashMap<String, String>> = OnceLock::new();
    EN.get_or_init(|| {
        Builtin::get("i18n/en.toml")
            .and_then(|f| toml::from_str(std::str::from_utf8(&f.data).ok()?).ok())
            .unwrap_or_default()
    })
}

/// Rust 가 만드는 공개 화면 문구 (ADR-0134). 프로세스 언어가 영어면 영어표에서, 아니면 그대로.
pub fn tr(ko: &'static str) -> &'static str {
    if maru_core::messages::lang() == "en" {
        english().get(ko).map_or(ko, String::as_str)
    } else {
        ko
    }
}

/// 화면이 영어인지. 템플릿 문맥의 `site.lang`, 없으면 프로세스 언어.
fn is_en(state: &minijinja::State) -> bool {
    let site = state
        .lookup("site")
        .and_then(|s| s.get_attr("lang").ok())
        .and_then(|l| l.as_str().map(str::to_owned));
    site.as_deref()
        .unwrap_or(maru_core::messages::lang())
        .starts_with("en")
}

/// `{{ "한국어"|t }}`. 영어 사이트면 표의 영어로 바꾼다. 표에 있는 글자는 템플릿에 쓴 원문이라
/// 그대로(HTML 로) 내보내고, 표에 없는 값(변수 등)은 다른 값처럼 이스케이프된다.
fn t(state: &minijinja::State, text: minijinja::Value) -> minijinja::Value {
    let Some(s) = text.as_str() else {
        return text;
    };
    match english().get(s) {
        Some(en) if is_en(state) => minijinja::Value::from_safe_string(en.clone()),
        Some(_) => minijinja::Value::from_safe_string(s.to_owned()),
        None => text,
    }
}

/// 세 자리마다 쉼표.
fn grouped(n: i64) -> String {
    let digits = n.unsigned_abs().to_string();
    let mut out = String::new();
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    if n < 0 { format!("-{out}") } else { out }
}

fn html_formatter(
    out: &mut minijinja::Output,
    state: &mut minijinja::State,
    value: &minijinja::Value,
) -> std::result::Result<(), minijinja::Error> {
    // 값이 없는 칸(null)은 빈칸으로 그린다. 글자로 찍으면 폼에 "none" 이 들어가 저장 때 그대로 남는다.
    if value.is_none() || value.is_undefined() {
        return Ok(());
    }
    if !matches!(state.auto_escape(), AutoEscape::Html) || value.is_safe() {
        return minijinja::escape_formatter(out, state, value);
    }
    let mut escaped = String::new();
    push_escaped(&mut escaped, &value.to_string());
    out.write_str(&escaped).map_err(minijinja::Error::from)
}

fn push_escaped(out: &mut String, s: &str) {
    out.reserve(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#x27;"),
            c => out.push(c),
        }
    }
}

/// 스티커 하나 (GAP E15, ADR-0115). 목록은 `static/stickers/index.json` 한 곳에 있고 고르기 창도 그 파일을 읽는다.
#[derive(Debug, Clone, Serialize, serde::Deserialize)]
pub struct Sticker {
    pub id: String,
    pub label: String,
    pub src: String,
}

/// 내장 스티커 전부.
pub fn stickers() -> &'static [Sticker] {
    #[derive(serde::Deserialize)]
    struct Index {
        sets: Vec<Set>,
    }
    #[derive(serde::Deserialize)]
    struct Set {
        items: Vec<Sticker>,
    }
    static ALL: std::sync::LazyLock<Vec<Sticker>> = std::sync::LazyLock::new(|| {
        serde_json::from_str::<Index>(include_str!("../builtin/static/stickers/index.json"))
            .map(|i| i.sets.into_iter().flat_map(|s| s.items).collect())
            .unwrap_or_default()
    });
    &ALL
}

/// 댓글 하나에 그림으로 그리는 스티커 수. 넘치면 글자로 둔다(도배 방지).
const MAX_STICKERS_PER_BODY: usize = 10;

/// 글자 본문의 `:cat-1:` 을 스티커 그림으로 바꾸고 나머지는 이스케이프한다. 모르는 이름은 글자 그대로다.
fn stickers_html(body: &str) -> String {
    let mut out = String::with_capacity(body.len());
    let mut rest = body;
    let mut shown = 0;
    while let Some(at) = rest.find(':') {
        push_escaped(&mut out, &rest[..at]);
        let after = &rest[at + 1..];
        let found = after
            .find(':')
            .map(|end| &after[..end])
            .filter(|_| shown < MAX_STICKERS_PER_BODY)
            .and_then(|id| stickers().iter().find(|s| s.id == id));
        if let Some(s) = found {
            out.push_str("<img class=\"sticker\" src=\"");
            push_escaped(&mut out, &s.src);
            out.push_str("\" alt=\"");
            push_escaped(&mut out, &s.label);
            out.push_str("\" width=\"96\" height=\"96\" loading=\"lazy\" decoding=\"async\">");
            rest = &after[s.id.len() + 1..];
            shown += 1;
        } else {
            out.push(':');
            rest = after;
        }
    }
    push_escaped(&mut out, rest);
    out
}

/// `{{ c.body|stickers(site.stickers) }}`. 설정이 꺼져 있으면 값을 그대로 돌려줘 평소처럼 이스케이프된다.
fn stickers_filter(body: minijinja::Value, on: Option<bool>) -> minijinja::Value {
    match (on.unwrap_or(false), body.as_str()) {
        (true, Some(s)) => minijinja::Value::from_safe_string(stickers_html(s)),
        _ => body,
    }
}

fn safe_join(base: &Path, rel: &str) -> Option<PathBuf> {
    if rel.is_empty() || rel.contains('\\') || rel.contains(':') {
        return None;
    }
    let rel_path = Path::new(rel);
    if !rel_path
        .components()
        .all(|c| matches!(c, Component::Normal(_)))
    {
        return None;
    }
    Some(base.join(rel_path))
}

fn mime_for(path: &str) -> Option<&'static str> {
    let ext = path.rsplit('.').next()?;
    Some(match ext {
        "css" => "text/css; charset=utf-8",
        "js" => "text/javascript; charset=utf-8",
        "svg" => "image/svg+xml",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "webp" => "image/webp",
        "ico" => "image/x-icon",
        "woff2" => "font/woff2",
        "txt" => "text/plain; charset=utf-8",
        "json" => "application/json",
        _ => return None,
    })
}

fn template_error(e: minijinja::Error) -> Error {
    if e.kind() == minijinja::ErrorKind::TemplateNotFound {
        return Error::new(Code::NotFound)
            .with_message("error.template_not_found")
            .arg("name", e.name().unwrap_or_default().to_owned());
    }
    Error::internal(e)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn html_escaping_keeps_slashes_and_blocks_markup() {
        let theme = Theme::new(None);
        let html = theme
            .render(
                "page.html",
                &json!({
                    "site": { "name": "<b>x</b>", "menus": {} },
                    "page": { "title": "\"a\" & 'b'", "url": "/p/a", "body_html": "<p>ok</p>" },
                }),
            )
            .unwrap();
        assert!(html.contains("&lt;b&gt;x&lt;/b&gt;"));
        assert!(html.contains("&quot;a&quot; &amp; &#x27;b&#x27;"));
        assert!(html.contains("<p>ok</p>"), "정화된 본문은 |safe 로 그대로");
        assert!(html.contains(r#"href="/assets/site.css""#));
    }

    /// 요약이 없는 글을 고치면 요약 칸이 비어 있어야 한다. 예전엔 "none" 이 찍혀 저장 때 요약이 됐다.
    #[test]
    fn null_values_render_as_empty() {
        let admin = Admin::new();
        let html = admin
            .render(
                "edit.html",
                &json!({"site": {"name": "s"}, "values": {"title": "t", "excerpt": null, "slug": null}}),
            )
            .unwrap();
        // 요약 칸(textarea) 여는 태그 바로 뒤가 닫는 태그여야 한다 (속성이 늘어도 내용만 본다).
        let excerpt = html
            .split(r#"name="excerpt""#)
            .nth(1)
            .and_then(|rest| rest.split_once('>'))
            .map(|(_, after)| after);
        assert!(
            excerpt.is_some_and(|a| a.starts_with("</textarea>")),
            "{html}"
        );
        assert!(!html.to_ascii_lowercase().contains(">none<"), "{html}");
        assert!(html.contains(r#"name="slug" maxlength="120" value="""#));
    }

    #[test]
    fn path_escape_is_blocked() {
        let base = Path::new("base");
        for bad in ["../x", "/etc/passwd", "a/../../b", "a\\b", "C:x", ""] {
            assert!(safe_join(base, bad).is_none(), "{bad}");
        }
        assert!(safe_join(base, "css/site.css").is_some());
    }

    #[test]
    fn builtin_renders_and_escapes() {
        let theme = Theme::new(None);
        let html = theme
            .render(
                "error.html",
                &json!({"site": {"name": "<b>사이트</b>"}, "status": 404, "message": "<script>"}),
            )
            .unwrap();
        assert!(html.contains("&lt;script&gt;"), "{html}");
        assert!(!html.contains("<script>"));
    }

    #[test]
    fn user_theme_overrides_then_falls_back() {
        let tmp = std::env::temp_dir().join(format!("maru-theme-{}", std::process::id()));
        let tdir = tmp.join("templates");
        std::fs::create_dir_all(&tdir).unwrap();
        std::fs::write(tdir.join("error.html"), "사용자 {{ status }}").unwrap();
        let theme = Theme::new(Some(tmp.clone()));
        assert_eq!(
            theme.render("error.html", &json!({"status": 404})).unwrap(),
            "사용자 404"
        );

        std::fs::remove_file(tdir.join("error.html")).unwrap();
        let theme = Theme::new(Some(tmp.clone()));
        assert!(
            theme
                .render("error.html", &json!({"status": 404}))
                .unwrap()
                .contains("404")
        );
        let _ = std::fs::remove_dir_all(tmp);
    }

    /// site.css 는 tokens.css 뒤에 실린다. 토큰 변수를 `:root` 로 다시 정의하면 토큰(다크 모드 포함)을
    /// 모두 덮어쓰므로, 대체값은 명시도 0 인 `:where(:root)` 에만 둔다.
    #[test]
    fn site_css_does_not_override_tokens() {
        let css = std::str::from_utf8(&Builtin::get("static/site.css").unwrap().data)
            .unwrap()
            .to_owned();
        for line in css.lines() {
            assert!(!line.trim_start().starts_with(":root"), "{line}");
        }
        assert!(css.contains(":where(:root)"));
    }

    /// `--measure` 는 em 이다. `.wrap` 을 감싸는 머리·본문·푸터 상자에 글자 크기를 주면
    /// 그 영역만 폭이 달라져 왼쪽 선이 어긋난다 (UI-1 에서 푸터가 좁아진 버그).
    #[test]
    fn wrap_containers_keep_body_font_size() {
        let css = std::str::from_utf8(&Builtin::get("static/site.css").unwrap().data)
            .unwrap()
            .to_owned();
        for rule in css.split('}') {
            let Some((selector, body)) = rule.split_once('{') else {
                continue;
            };
            let selector = selector.lines().last().unwrap_or_default().trim();
            if [
                ".site-header",
                ".site-footer",
                "main",
                ".wrap",
                ".wrap.wide",
            ]
            .contains(&selector)
            {
                assert!(!body.contains("font-size"), "{selector} {{{body}}}");
            }
        }
    }

    #[test]
    fn sticker_catalog_matches_embedded_files() {
        let all = stickers();
        assert_eq!(all.len(), 63);
        let mut ids: Vec<&str> = all.iter().map(|s| s.id.as_str()).collect();
        ids.sort_unstable();
        ids.dedup();
        assert_eq!(ids.len(), 63, "이름이 겹치지 않는다");
        let theme = Theme::new(None);
        for s in all {
            let path = s.src.strip_prefix("/assets/").unwrap();
            assert!(theme.asset(path).is_some(), "{} 파일 없음", s.src);
            assert!(!s.label.is_empty());
        }
    }

    #[test]
    fn stickers_html_escapes_text_and_keeps_unknown_codes() {
        let html = stickers_html("<b>안녕</b> :cat-1: 12:30 :cat-999: :cat-2:");
        assert!(
            html.starts_with("&lt;b&gt;안녕&lt;/b&gt; <img class=\"sticker\""),
            "{html}"
        );
        assert_eq!(html.matches("<img").count(), 2, "{html}");
        assert!(html.contains("12:30 :cat-999: <img"), "{html}");
        // 그림 수는 정해 둔 데까지만
        let many = ":cat-1:".repeat(MAX_STICKERS_PER_BODY + 2);
        let html = stickers_html(&many);
        assert_eq!(html.matches("<img").count(), MAX_STICKERS_PER_BODY);
        assert!(html.ends_with(":cat-1::cat-1:"), "{html}");
    }

    #[test]
    fn 영어_사이트는_t_와_시간과_수를_영어로() {
        let mut env = Environment::new();
        env.set_auto_escape_callback(|_| AutoEscape::Html);
        env.add_filter("ago", ago);
        env.add_filter("short_count", short_count);
        env.add_filter("t", t);
        let en = |src: &str, at: &str, n: i64| {
            env.render_str(
                src,
                minijinja::context! { site => minijinja::context! { lang => "en" }, at => at, n => n },
            )
            .unwrap()
        };
        assert_eq!(en(r#"{{ "회원가입"|t }}"#, "", 0), "Sign up");
        assert_eq!(
            en(r#"{{ "표에 없는 <b>"|t }}"#, "", 0),
            "표에 없는 &lt;b&gt;"
        );
        assert_eq!(en("{{ x|t }}", "", 0), "");
        assert_eq!(en("{{ at|ago }}", &maru_core::clock::now(), 0), "just now");
        let five = maru_core::clock::minutes_later(-5);
        assert_eq!(en("{{ at|ago }}", &five, 0), "5 min ago");
        for (n, want) in [
            (999, "999"),
            (1_234, "1.2K"),
            (12_345, "12.3K"),
            (1_250_000, "1.2M"),
            (3_000_000_000, "3B"),
        ] {
            assert_eq!(en("{{ n|short_count }}", "", n), want);
        }
        let ko = env
            .render_str(
                r#"{{ "회원가입"|t }}"#,
                minijinja::context! { site => minijinja::context! { lang => "ko" } },
            )
            .unwrap();
        assert_eq!(ko, "회원가입");
    }

    /// 템플릿의 `"…"|t` 가 모두 영어표에 있다.
    #[test]
    fn 감싼_문구는_모두_영어표에_있다() {
        fn walk(dir: &Path, into: &mut Vec<PathBuf>) {
            for e in std::fs::read_dir(dir).unwrap() {
                let p = e.unwrap().path();
                if p.is_dir() {
                    walk(&p, into);
                } else if p.extension().is_some_and(|x| x == "html") {
                    into.push(p);
                }
            }
        }
        let root = Path::new(env!("CARGO_MANIFEST_DIR"));
        let mut all = Vec::new();
        walk(&root.join("builtin"), &mut all);
        for m in std::fs::read_dir(root.join("../../mods")).unwrap() {
            let t = m.unwrap().path().join("templates");
            if t.is_dir() {
                walk(&t, &mut all);
            }
        }
        let table = english();
        let mut missing = Vec::new();
        let mut count = 0;
        for f in &all {
            let src = std::fs::read_to_string(f).unwrap();
            for (q, end) in [("\"", "\"|t"), ("'", "'|t")] {
                let parts: Vec<&str> = src.split(end).collect();
                // 마지막 조각은 `|t` 앞이 아니다.
                for part in &parts[..parts.len() - 1] {
                    let Some(at) = part.rfind(q) else { continue };
                    let key = part[at + 1..].replace("\\\"", "\"");
                    if part[..at].ends_with('\\') {
                        continue;
                    }
                    count += 1;
                    if !table.contains_key(&key) {
                        missing.push(format!("{}: {key}", f.display()));
                    }
                }
            }
        }
        assert!(count > 300, "{count}");
        assert!(missing.is_empty(), "{missing:#?}");
    }

    #[test]
    fn ago_and_short_count_filters() {
        let mut env = Environment::new();
        env.add_filter("ago", ago);
        env.add_filter("short_count", short_count);
        env.add_filter("t", t);
        let render = |src: &str, at: &str| {
            env.render_str(src, minijinja::context! { at => at })
                .unwrap()
        };
        let now = maru_core::clock::now();
        assert_eq!(render("{{ at|ago }}", &now), "방금 전");
        let minutes = maru_core::clock::minutes_later(-5);
        assert_eq!(render("{{ at|ago }}", &minutes), "5분 전");
        assert_eq!(
            render("{{ at|ago }}", &maru_core::clock::hours_ago(3)),
            "3시간 전"
        );
        let old = maru_core::clock::hours_ago(48);
        assert_eq!(
            render("{{ at|ago(540) }}", &old),
            maru_core::clock::display_date(&old, 540)
        );
        assert_eq!(render("{{ none|ago }}", ""), "");
        assert_eq!(render("{{ missing|ago }}", ""), "");
        for (n, want) in [
            (0, "0"),
            (999, "999"),
            (1234, "1,234"),
            (9999, "9,999"),
            (10_000, "1만"),
            (12_345, "1.2만"),
            (19_999, "1.9만"),
            (1_234_567, "123.4만"),
            (100_000_000, "1억"),
            (250_000_000, "2.5억"),
            (-12_345, "-1.2만"),
        ] {
            let out = env
                .render_str("{{ n|short_count }}", minijinja::context! { n => n })
                .unwrap();
            assert_eq!(out, want, "{n}");
        }
    }

    #[test]
    fn url_for_matches_core_builder() {
        let mut env = Environment::new();
        env.add_function("url_for", url_for);
        let out = env
            .render_str(
                "{{ url_for('post', '블로그', '첫 글') }}|{{ url_for('page', 'about') }}|{{ url_for('home') }}",
                (),
            )
            .unwrap();
        let expected = format!(
            "{}|/p/about|/",
            maru_core::urls::path(&maru_core::urls::Target::Post {
                board: "블로그",
                post: "첫 글"
            })
        );
        assert_eq!(out, expected);
        assert!(env.render_str("{{ url_for('nope') }}", ()).is_err());
    }

    #[test]
    fn incompatible_user_theme_is_ignored() {
        let tmp = std::env::temp_dir().join(format!("maru-theme-api-{}", std::process::id()));
        let tdir = tmp.join("templates");
        std::fs::create_dir_all(&tdir).unwrap();
        std::fs::write(tdir.join("error.html"), "사용자").unwrap();
        std::fs::write(tmp.join("theme.toml"), "template_api = 99\n").unwrap();
        let theme = Theme::new(Some(tmp.clone()));
        let html = theme.render("error.html", &json!({"status": 404})).unwrap();
        assert_ne!(html, "사용자");
        let _ = std::fs::remove_dir_all(tmp);
    }

    /// ADR-0017: 내장 템플릿은 URL 을 조합하지 않고 컨텍스트의 `url` 만 쓴다.
    #[test]
    fn builtin_templates_have_no_path_literals() {
        for path in
            Builtin::iter().filter(|p| p.starts_with("templates/") || p.starts_with("skins/"))
        {
            let file = Builtin::get(&path).unwrap();
            let text = std::str::from_utf8(&file.data).unwrap();
            for bad in ["\"/b/", "\"/p/", "'/b/", "'/p/"] {
                assert!(!text.contains(bad), "{path} 에 경로 리터럴 {bad}");
            }
        }
    }

    #[test]
    fn builtin_manifest_and_presets_parse() {
        let manifest = std::str::from_utf8(&Builtin::get("theme.toml").unwrap().data)
            .unwrap()
            .to_owned();
        assert!(manifest.contains(&format!("template_api = {TEMPLATE_API}")));
        assert!(preset_ids().contains(&"blog".to_owned()));
        let blog = preset("blog").unwrap();
        assert_eq!(blog.id, "blog");
        assert!(!blog.ops.is_empty());
        assert!(preset("../x").is_err());
        assert!(preset("nope").is_err());
    }

    /// 스킨은 `site.skin` 으로 고른다. 모르는 값이면 기본 (ADR-0081).
    #[test]
    fn skin_is_picked_by_site_setting() {
        let theme = Theme::new(None);
        let ctx = |skin: &str| json!({"site": {"name": "마루", "skin": skin}, "posts": []});
        let community = theme.render("home.html", &ctx("community")).unwrap();
        assert!(community.contains("/assets/community.css"), "{community}");
        assert!(community.contains("data-bar="));
        for other in ["default", "nope", ""] {
            let html = theme.render("home.html", &ctx(other)).unwrap();
            assert!(!html.contains("community.css"), "{other}");
        }
        let html = theme.render("home.html", &json!({"posts": []})).unwrap();
        assert!(!html.contains("community.css"));
        for id in maru_core::model::SKINS.iter().skip(1).map(|(id, _)| id) {
            assert!(
                Builtin::get(&format!("skins/{id}/templates/base.html")).is_some(),
                "{id}"
            );
        }
        let (_, mime) = theme.asset("community.js").unwrap();
        assert!(mime.contains("javascript"), "{mime}");
    }

    #[test]
    fn assets_are_served() {
        let theme = Theme::new(None);
        let (_, mime) = theme.asset("site.css").unwrap();
        assert!(mime.starts_with("text/css"));
        assert!(theme.asset("../Cargo.toml").is_none());
    }
}
