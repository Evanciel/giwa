//! Maru 기본 테마와 프리셋
//!
//! 템플릿은 소스 목록을 순서대로 찾는 겹 구조다: 사용자 테마 디렉터리 → 내장 테마 (PHASE1_PLAN §2).
//! Phase 3 changeset 미리보기는 이 목록 앞에 오버레이 소스 하나를 더하면 된다.

use std::borrow::Cow;
use std::path::{Component, Path, PathBuf};

use maru_core::{Code, Error, Result};
use minijinja::{Environment, UndefinedBehavior};
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
        let mut env = Environment::new();
        env.set_undefined_behavior(UndefinedBehavior::Chainable);
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

/// 사용자 테마 파일. 경로 탈출이면 없는 것으로 본다.
fn read_user(dir: &Path, sub: &str, name: &str) -> Option<String> {
    let path = safe_join(&dir.join(sub), name)?;
    std::fs::read_to_string(path).ok()
}

/// `..`, 절대 경로, 드라이브 접두어, 역슬래시를 거부한다.
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

    #[test]
    fn assets_are_served() {
        let theme = Theme::new(None);
        let (_, mime) = theme.asset("site.css").unwrap();
        assert!(mime.starts_with("text/css"));
        assert!(theme.asset("../Cargo.toml").is_none());
    }
}
