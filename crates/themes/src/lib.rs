//! Maru 기본 테마와 프리셋
//!
//! 템플릿은 소스 목록을 순서대로 찾는 겹 구조다: 사용자 테마 디렉터리 → 내장 테마 (PHASE1_PLAN §2).
//! Phase 3 changeset 미리보기는 이 목록 앞에 오버레이 소스 하나를 더하면 된다.

use std::borrow::Cow;
use std::path::{Component, Path, PathBuf};

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
    user_dir: Option<PathBuf>,
}

impl Theme {
    /// `user_dir` 는 `<data>/themes/<이름>`. 없거나 비어 있으면 내장 테마만 쓴다.
    pub fn new(user_dir: Option<PathBuf>) -> Self {
        let user_dir = user_dir.filter(|d| compatible(d));
        let mut env = Environment::new();
        env.set_undefined_behavior(UndefinedBehavior::Chainable);
        env.set_formatter(html_formatter);
        env.add_function("url_for", url_for);
        let dir = user_dir.clone();
        env.set_loader(move |name| {
            if let Some(text) = dir.as_deref().and_then(|d| read_user(d, "templates", name)) {
                return Ok(Some(text));
            }
            Ok(Builtin::get(&format!("templates/{name}"))
                .and_then(|f| String::from_utf8(f.data.into_owned()).ok()))
        });
        Self { env, user_dir }
    }

    pub fn render<S: Serialize>(&self, name: &str, ctx: &S) -> Result<String> {
        let tmpl = self.env.get_template(name).map_err(template_error)?;
        tmpl.render(minijinja::Value::from(minijinja::value::Serde(ctx)))
            .map_err(template_error)
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
fn html_formatter(
    out: &mut minijinja::Output,
    state: &mut minijinja::State,
    value: &minijinja::Value,
) -> std::result::Result<(), minijinja::Error> {
    if !matches!(state.auto_escape(), AutoEscape::Html) || value.is_safe() {
        return minijinja::escape_formatter(out, state, value);
    }
    let s = value.to_string();
    let mut escaped = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => escaped.push_str("&amp;"),
            '<' => escaped.push_str("&lt;"),
            '>' => escaped.push_str("&gt;"),
            '"' => escaped.push_str("&quot;"),
            '\'' => escaped.push_str("&#x27;"),
            c => escaped.push(c),
        }
    }
    out.write_str(&escaped).map_err(minijinja::Error::from)
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
        for path in Builtin::iter().filter(|p| p.starts_with("templates/")) {
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

    #[test]
    fn assets_are_served() {
        let theme = Theme::new(None);
        let (_, mime) = theme.asset("site.css").unwrap();
        assert!(mime.starts_with("text/css"));
        assert!(theme.asset("../Cargo.toml").is_none());
    }
}
