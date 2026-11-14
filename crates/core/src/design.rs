//! 디자인 토큰 (SPEC §8.2). 에이전트는 CSS 를 쓰지 않고 토큰만 바꾼다.
//!
//! 모든 값은 형식이 정해진 타입(hex 색, 범위 안 숫자, 허용 목록 폰트)으로만 들어오므로
//! CSS 출력에 사용자 문자열이 그대로 실리지 않는다. 저장 전에 WCAG AA 대비를 검사한다.

use std::fmt::Write as _;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::error::{Code, Error, Result};
use crate::model::Font;

/// 본문 대비 최소값 (WCAG 2.x AA, 일반 글자).
pub const MIN_CONTRAST: f64 = 4.5;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Tokens {
    pub color: Palette,
    pub color_dark: Palette,
    pub font: Fonts,
    pub type_scale: TypeScale,
    pub radius: Radius,
    pub space: Space,
    pub layout: Layout,
    pub motion: Motion,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Palette {
    pub bg: String,
    pub fg: String,
    pub primary: String,
    pub muted: String,
    pub surface: String,
    pub border: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Fonts {
    pub body: String,
    pub heading: String,
    pub mono: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct TypeScale {
    /// 본문 px (14~22)
    pub base: u8,
    /// 제목 단계 배율 (1.1~1.5)
    pub ratio: f64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Radius {
    pub sm: u8,
    pub md: u8,
    pub lg: u8,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Space {
    /// 기본 간격 px (4~12)
    pub unit: u8,
    /// 섹션 간격 px (24~200)
    pub section: u16,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Layout {
    /// 헤더·넓은 레이아웃 최대 폭 px (640~1600)
    pub max_width: u16,
    /// 본문 한 줄 최대 글자 폭 em (28~48)
    pub measure: u8,
    /// sticky, static
    pub header: String,
    /// compact, comfortable, spacious
    pub density: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Motion {
    /// none, subtle, normal
    pub level: String,
}

/// 토큰에 쓸 수 있는 폰트 이름과 실제 CSS 스택. 사용자 문자열은 스택에 들어가지 않는다.
pub const FONTS: &[(&str, &str)] = &[
    (
        "Pretendard",
        "\"Pretendard Variable\", Pretendard, -apple-system, BlinkMacSystemFont, \"Apple SD Gothic Neo\", \"Malgun Gothic\", sans-serif",
    ),
    (
        "system",
        "-apple-system, BlinkMacSystemFont, \"Apple SD Gothic Neo\", \"Malgun Gothic\", \"Noto Sans KR\", sans-serif",
    ),
    (
        "serif",
        "\"Noto Serif KR\", \"Nanum Myeongjo\", \"AppleMyungjo\", serif",
    ),
    (
        "D2Coding",
        "D2Coding, ui-monospace, SFMono-Regular, Consolas, monospace",
    ),
    ("mono", "ui-monospace, SFMono-Regular, Consolas, monospace"),
];

/// 올린 서체가 쓸 수 없는 이름. 내장 서체 값(`:font[]{serif}` 등)·토큰 이름·CSS 키워드와 겹치지 않게 한다.
pub const RESERVED_FONT_SLUGS: &[&str] = &[
    "serif", "pen", "brush", "mono", "system", "sans", "cursive", "none", "default", "inherit",
    "initial", "unset", "revert",
];

/// 올린 서체 파일을 못 받았을 때 떨어질 계열과 그 스택 (ADR-0048).
pub const FONT_FALLBACKS: &[(&str, &str)] = &[
    ("sans", FONTS[1].1),
    (
        "serif",
        "\"Noto Serif KR\", AppleMyungjo, Batang, \"바탕\", serif",
    ),
    ("mono", "D2Coding, ui-monospace, Consolas, monospace"),
    ("cursive", "cursive"),
];

/// 올린 서체 slug 규칙: 영소문자로 시작하고 영소문자·숫자로 끝나는 2~32자, 가운데는 `-` 도 된다.
/// 예약어는 안 된다. 이 모양이면 CSS class·글꼴 이름·URL 조각에 그대로 써도 안전하다.
pub fn is_font_slug(s: &str) -> bool {
    let b = s.as_bytes();
    (2..=32).contains(&b.len())
        && b[0].is_ascii_lowercase()
        && b[b.len() - 1].is_ascii_alphanumeric()
        && b.iter()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || *c == b'-')
        && !s.contains("--")
        && !RESERVED_FONT_SLUGS.contains(&s)
}

/// 올린 서체의 CSS 글꼴 이름. 사용자 PC 에 설치된 글꼴과 겹치지 않게 앞머리를 붙인다.
pub fn uploaded_font_family(slug: &str) -> String {
    format!("maru-u-{slug}")
}

/// 올린 서체의 CSS 스택. `fallback` 이 목록에 없으면 고딕 계열.
pub fn uploaded_font_stack(slug: &str, fallback: &str) -> String {
    let tail = FONT_FALLBACKS
        .iter()
        .find(|(n, _)| *n == fallback)
        .map_or(FONT_FALLBACKS[0].1, |(_, s)| *s);
    format!("\"{}\", {tail}", uploaded_font_family(slug))
}

const HEADER: &[&str] = &["sticky", "static"];
const DENSITY: &[&str] = &["compact", "comfortable", "spacious"];
const MOTION: &[&str] = &["none", "subtle", "normal"];

impl Default for Tokens {
    fn default() -> Self {
        Self {
            color: Palette {
                bg: "#fbfbfa".into(),
                fg: "#1d1f23".into(),
                primary: "#2f5d50".into(),
                muted: "#5c626d".into(),
                surface: "#ffffff".into(),
                border: "#e4e5e7".into(),
            },
            color_dark: Palette {
                bg: "#16181b".into(),
                fg: "#e6e7e9".into(),
                primary: "#8cc7b5".into(),
                muted: "#a1a7b0".into(),
                surface: "#1d2024".into(),
                border: "#2c3036".into(),
            },
            font: Fonts {
                body: "Pretendard".into(),
                heading: "Pretendard".into(),
                mono: "D2Coding".into(),
            },
            type_scale: TypeScale {
                base: 17,
                ratio: 1.25,
            },
            radius: Radius {
                sm: 6,
                md: 12,
                lg: 20,
            },
            space: Space {
                unit: 8,
                section: 96,
            },
            layout: Layout {
                max_width: 1120,
                measure: 40,
                header: "static".into(),
                density: "comfortable".into(),
            },
            motion: Motion {
                level: "subtle".into(),
            },
        }
    }
}

impl Tokens {
    /// 형식, 범위, 허용 목록, 대비를 모두 검사한다.
    pub fn validate(&self) -> Result<()> {
        for (mode, p) in [("color", &self.color), ("color_dark", &self.color_dark)] {
            for (name, value) in p.entries() {
                parse_hex(value).ok_or_else(|| invalid(&format!("{mode}.{name}"), value))?;
            }
            // 힌트는 토큰 이름을 모르는 사람이 읽는 말이다(편집기 디자인 판이 그대로 보여 준다).
            for (a, b, hint) in [
                ("fg", "bg", "hint.token_contrast_text"),
                ("fg", "surface", "hint.token_contrast_text"),
                ("muted", "bg", "hint.token_contrast_muted"),
                ("primary", "bg", "hint.token_contrast_primary"),
            ] {
                let ratio = contrast(p.get(a), p.get(b)).unwrap_or(0.0);
                if ratio < MIN_CONTRAST {
                    return Err(Error::new(Code::Validation)
                        .with_message("error.token_contrast")
                        .with_hint(hint)
                        .arg("pair", format!("{mode}.{a} / {mode}.{b}"))
                        .arg("ratio", format!("{ratio:.2}"))
                        .arg("min", format!("{MIN_CONTRAST}")));
                }
            }
        }
        for (name, value) in [
            ("font.body", &self.font.body),
            ("font.heading", &self.font.heading),
            ("font.mono", &self.font.mono),
        ] {
            // 올린 서체(slug)는 모양만 본다. 실제로 있는지는 저장할 때 서비스가 DB 로 확인한다.
            if font_stack(value).is_none() && !is_font_slug(value) {
                return Err(invalid(name, value).arg(
                    "allowed",
                    FONTS.iter().map(|(n, _)| *n).collect::<Vec<_>>().join(", "),
                ));
            }
        }
        range(
            "type_scale.base",
            f64::from(self.type_scale.base),
            14.0,
            22.0,
        )?;
        range("type_scale.ratio", self.type_scale.ratio, 1.1, 1.5)?;
        for (name, v) in [
            ("radius.sm", self.radius.sm),
            ("radius.md", self.radius.md),
            ("radius.lg", self.radius.lg),
        ] {
            range(name, f64::from(v), 0.0, 40.0)?;
        }
        range("space.unit", f64::from(self.space.unit), 4.0, 12.0)?;
        range("space.section", f64::from(self.space.section), 24.0, 200.0)?;
        range(
            "layout.max_width",
            f64::from(self.layout.max_width),
            640.0,
            1600.0,
        )?;
        range("layout.measure", f64::from(self.layout.measure), 28.0, 48.0)?;
        one_of("layout.header", &self.layout.header, HEADER)?;
        one_of("layout.density", &self.layout.density, DENSITY)?;
        one_of("motion.level", &self.motion.level, MOTION)?;
        Ok(())
    }

    /// CSS 변수 파일. `validate` 를 통과한 값만 넣는다. `uploaded` 는 올린 서체 목록(slug 를 스택으로 바꿀 때 쓴다).
    /// 목록에 없는 slug 는 시스템 고딕으로 떨어진다.
    pub fn to_css(&self, uploaded: &[Font]) -> String {
        let mut css = String::from(
            "/* Giwa 디자인 토큰. 직접 고치지 말고 `giwa design set` 을 쓴다. */\n:root {\n",
        );
        push_palette(&mut css, &self.color);
        let stack = |name: &str| -> String {
            font_stack(name).map_or_else(
                || {
                    uploaded.iter().find(|f| f.slug == name).map_or_else(
                        || FONTS[1].1.to_owned(),
                        |f| uploaded_font_stack(&f.slug, &f.fallback),
                    )
                },
                str::to_owned,
            )
        };
        let _ = writeln!(css, "  --font-body: {};", stack(&self.font.body));
        let _ = writeln!(css, "  --font-heading: {};", stack(&self.font.heading));
        let _ = writeln!(css, "  --font-mono: {};", stack(&self.font.mono));
        let base = f64::from(self.type_scale.base);
        let r = self.type_scale.ratio;
        for (name, size) in [
            ("sm", base / r.sqrt()),
            ("base", base),
            ("lg", base * r),
            ("h3", base * r * r),
            ("h2", base * r.powi(3)),
            ("h1", base * r.powi(4)),
        ] {
            let _ = writeln!(css, "  --fs-{name}: {:.3}rem;", size / 16.0);
        }
        let _ = writeln!(css, "  --radius-sm: {}px;", self.radius.sm);
        let _ = writeln!(css, "  --radius-md: {}px;", self.radius.md);
        let _ = writeln!(css, "  --radius-lg: {}px;", self.radius.lg);
        let density = match self.layout.density.as_str() {
            "compact" => 0.75,
            "spacious" => 1.375,
            _ => 1.0,
        };
        let unit = f64::from(self.space.unit) * density;
        let _ = writeln!(css, "  --space: {unit:.1}px;");
        let _ = writeln!(
            css,
            "  --space-section: {:.0}px;",
            f64::from(self.space.section) * density
        );
        let _ = writeln!(css, "  --max-width: {}px;", self.layout.max_width);
        let _ = writeln!(css, "  --measure: {}em;", self.layout.measure);
        let _ = writeln!(
            css,
            "  --header-position: {};",
            if self.layout.header == "sticky" {
                "sticky"
            } else {
                "static"
            }
        );
        let ms = match self.motion.level.as_str() {
            "none" => 0,
            "normal" => 240,
            _ => 160,
        };
        let _ = writeln!(css, "  --motion: {ms}ms;");
        css.push_str("  color-scheme: light dark;\n}\n");
        // 스킨의 화면 모드 단추(ADR-0081)가 <html data-mode> 를 정하면 그것이 기기 설정보다 먼저다.
        let mut dark = String::new();
        push_palette(&mut dark, &self.color_dark);
        css.push_str(":root[data-mode=\"light\"] { color-scheme: light; }\n");
        css.push_str(":root[data-mode=\"dark\"] {\n");
        css.push_str(&dark);
        css.push_str("  color-scheme: dark;\n}\n");
        css.push_str(
            "@media (prefers-color-scheme: dark) {\n  :root:not([data-mode=\"light\"]) {\n",
        );
        for line in dark.lines() {
            let _ = writeln!(css, "  {line}");
        }
        css.push_str("  }\n}\n");
        css.push_str("@media (prefers-reduced-motion: reduce) {\n  :root { --motion: 0ms; }\n}\n");
        css
    }
}

impl Palette {
    fn entries(&self) -> [(&'static str, &str); 6] {
        [
            ("bg", &self.bg),
            ("fg", &self.fg),
            ("primary", &self.primary),
            ("muted", &self.muted),
            ("surface", &self.surface),
            ("border", &self.border),
        ]
    }

    fn get(&self, name: &str) -> &str {
        self.entries()
            .into_iter()
            .find(|(n, _)| *n == name)
            .map(|(_, v)| v)
            .unwrap_or_default()
    }
}

fn push_palette(css: &mut String, p: &Palette) {
    for (name, value) in p.entries() {
        let _ = writeln!(css, "  --{name}: {};", value.to_ascii_lowercase());
    }
}

pub fn font_stack(name: &str) -> Option<&'static str> {
    FONTS.iter().find(|(n, _)| *n == name).map(|(_, s)| *s)
}

/// `#rgb` 또는 `#rrggbb` 만 받는다.
fn parse_hex(s: &str) -> Option<(u8, u8, u8)> {
    let h = s.strip_prefix('#')?;
    if !h.chars().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }
    let digit = |i: usize, len: usize| u8::from_str_radix(h.get(i..i + len)?, 16).ok();
    match h.len() {
        3 => Some((digit(0, 1)? * 17, digit(1, 1)? * 17, digit(2, 1)? * 17)),
        6 => Some((digit(0, 2)?, digit(2, 2)?, digit(4, 2)?)),
        _ => None,
    }
}

fn luminance((r, g, b): (u8, u8, u8)) -> f64 {
    let ch = |c: u8| {
        let c = f64::from(c) / 255.0;
        if c <= 0.04045 {
            c / 12.92
        } else {
            ((c + 0.055) / 1.055).powf(2.4)
        }
    };
    0.2126 * ch(r) + 0.7152 * ch(g) + 0.0722 * ch(b)
}

/// WCAG 대비. 둘 중 하나라도 색이 아니면 `None`.
pub fn contrast(a: &str, b: &str) -> Option<f64> {
    let (la, lb) = (luminance(parse_hex(a)?), luminance(parse_hex(b)?));
    let (hi, lo) = if la > lb { (la, lb) } else { (lb, la) };
    Some((hi + 0.05) / (lo + 0.05))
}

fn invalid(field: &str, value: &str) -> Error {
    Error::new(Code::Validation)
        .with_message("error.token_value")
        .arg("field", field.to_owned())
        .arg("value", value.chars().take(40).collect::<String>())
}

fn range(field: &str, v: f64, min: f64, max: f64) -> Result<()> {
    if (min..=max).contains(&v) {
        return Ok(());
    }
    Err(invalid(field, &v.to_string()).arg("allowed", format!("{min} ~ {max}")))
}

fn one_of(field: &str, v: &str, allowed: &[&str]) -> Result<()> {
    if allowed.contains(&v) {
        return Ok(());
    }
    Err(invalid(field, v).arg("allowed", allowed.join(", ")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_pass_validation() {
        Tokens::default().validate().unwrap();
    }

    #[test]
    fn css_injection_is_rejected() {
        let mut t = Tokens::default();
        t.color.primary = "#fff;}</style><script>alert(1)</script>".into();
        assert_eq!(t.validate().unwrap_err().code(), Code::Validation);
        let mut t = Tokens::default();
        t.font.body = "x; } body { display:none".into();
        assert_eq!(t.validate().unwrap_err().code(), Code::Validation);
    }

    #[test]
    fn low_contrast_is_rejected() {
        let mut t = Tokens::default();
        t.color.muted = "#c8c8c8".into();
        let err = t.validate().unwrap_err();
        assert_eq!(err.code(), Code::Validation);
        let mut t = Tokens::default();
        t.color_dark.fg = "#333333".into();
        assert!(t.validate().is_err());
        // 사람이 읽는 힌트: 토큰 이름 없이 무엇이 안 보이는지와 대비 수치.
        let mut t = Tokens::default();
        t.color.primary = "#f4f4f4".into();
        let hint = t.validate().unwrap_err().hint().unwrap();
        assert!(hint.starts_with("주 색이 바탕과 너무 비슷해"), "{hint}");
        assert!(hint.contains(":1") && !hint.contains("color."), "{hint}");
    }

    #[test]
    fn contrast_matches_known_values() {
        let black_white = contrast("#000", "#ffffff").unwrap();
        assert!((black_white - 21.0).abs() < 0.01);
        assert!((contrast("#777777", "#ffffff").unwrap() - 4.48).abs() < 0.01);
    }

    #[test]
    fn css_has_both_modes_and_no_raw_input() {
        let css = Tokens::default().to_css(&[]);
        assert!(css.contains("--primary: #2f5d50;"));
        assert!(css.contains("prefers-color-scheme: dark"));
        assert!(css.contains(":root[data-mode=\"dark\"]"));
        assert!(css.contains(":root:not([data-mode=\"light\"])"));
        assert!(css.contains("--fs-h1:"));
        assert!(!css.contains("<"));
    }

    #[test]
    fn font_slugs_are_safe_and_avoid_reserved_names() {
        for ok in ["company-gothic", "ab", "font2", "a-1"] {
            assert!(is_font_slug(ok), "{ok}");
        }
        for bad in [
            "a",
            "serif",
            "system",
            "Gothic",
            "1abc",
            "ab-",
            "a--b",
            "a b",
            "a;b",
            "가나",
            &"a".repeat(33),
        ] {
            assert!(!is_font_slug(bad), "{bad}");
        }
    }

    #[test]
    fn uploaded_font_token_becomes_its_stack() {
        let mut t = Tokens::default();
        t.font.body = "company-gothic".into();
        t.validate().unwrap();
        let font = Font {
            id: "f".into(),
            slug: "company-gothic".into(),
            name: "회사 고딕".into(),
            fallback: "serif".into(),
            license: "OFL-1.1".into(),
            license_path: None,
            license_url: None,
            files: vec![],
            created_at: String::new(),
            updated_at: String::new(),
        };
        let css = t.to_css(&[font]);
        assert!(css.contains("--font-body: \"maru-u-company-gothic\", \"Noto Serif KR\""));
        // 목록에 없으면 시스템 고딕
        let css = t.to_css(&[]);
        assert!(css.contains(&format!("--font-body: {};", FONTS[1].1)));
    }
}
