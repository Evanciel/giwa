//! 섹션 블록 (ADR-0125, SPEC §8.5). 홈페이지를 이루는 큰 조각을 위젯 종류로 둔다.
//!
//! ADR-0020 대로 블록과 위젯은 렌더 계약이 하나다. 에이전트는 CSS 를 쓰지 않고 이 종류들을
//! 고르고 칸을 채운다. 글자 칸은 길이만 보고 그대로 두며 테마가 이스케이프한다. 마크다운을
//! 받는 칸(`faq.a`, `image_text.body`)은 web 이 정화 렌더러로 그린다.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::widgets::{bad_config, check_link, parse, schema_of, short_text};
use crate::error::Result;
use crate::services::posts;

pub const KINDS: [(&str, &str, &str); 14] = [
    (
        "hero",
        "첫 인상",
        "사이트 맨 위 큰 제목·설명·단추. 글자만, 배경 그림, 좌우 나눔",
    ),
    (
        "features",
        "소개 묶음",
        "강점·서비스·절차를 카드·줄·벤토·단계로",
    ),
    ("stats", "숫자", "경력·고객 수 같은 숫자 몇 개"),
    ("testimonials", "후기", "손님·고객의 말"),
    ("cta", "행동 유도", "제목 한 줄과 단추. 페이지 끝맺음"),
    ("faq", "자주 묻는 질문", "접고 펴는 질문과 답"),
    ("hours", "영업시간", "요일별 시간과 안내"),
    (
        "visit",
        "오시는 길",
        "주소·전화·교통편과 네이버 지도·카카오맵 길찾기 링크",
    ),
    ("price_list", "가격표", "메뉴판처럼 묶음별 이름과 가격"),
    ("plans", "요금제", "요금제를 나란히 비교"),
    ("timeline", "연혁", "날짜순 이력"),
    ("org_chart", "조직도", "3단까지의 조직 나무"),
    ("team", "구성원", "사람 사진·이름·역할"),
    ("image_text", "그림과 글", "그림 한 장과 글을 나란히"),
];

/// 띠 색: 바탕 그대로, 옅은 면, 주 색
pub const TONES: [&str; 3] = ["plain", "soft", "strong"];
pub const HERO_STYLES: [&str; 3] = ["type", "image", "split"];
pub const FEATURE_STYLES: [&str; 4] = ["cards", "rows", "bento", "steps"];

/// 조직도 깊이·개수 상한
const ORG_DEPTH: usize = 3;
const ORG_NODES: usize = 60;
const MAX_MARKDOWN: usize = 4_000;

fn plain() -> String {
    "plain".into()
}

fn strong() -> String {
    "strong".into()
}

/// 글자와 주소가 함께 있어야 그리는 단추. 둘 다 비면 없는 것이다.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Button {
    /// 1~20자
    #[serde(default)]
    pub label: String,
    /// `/` 로 시작하는 사이트 안 주소 또는 https 주소
    #[serde(default)]
    pub link: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct HeroConfig {
    /// `type`(글자만, 기본), `image`(배경 그림), `split`(글과 그림 나란히)
    #[serde(default = "hero_style")]
    pub style: String,
    /// 제목 위 작은 글, 40자
    #[serde(default)]
    pub eyebrow: String,
    /// 120자. 비우면 사이트 이름
    #[serde(default)]
    pub title: String,
    /// 300자. 비우면 사이트 설명
    #[serde(default)]
    pub text: String,
    /// 올린 그림 주소(`/media/...`). image·split 에서 쓴다
    #[serde(default)]
    pub image: String,
    #[serde(default)]
    pub image_alt: String,
    #[serde(default)]
    pub button: Button,
    #[serde(default)]
    pub button2: Button,
    #[serde(default = "plain")]
    pub tone: String,
}

fn hero_style() -> String {
    "type".into()
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct FeatureItem {
    /// 60자
    pub title: String,
    /// 300자
    #[serde(default)]
    pub text: String,
    #[serde(default)]
    pub link: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct FeaturesConfig {
    /// `cards`, `rows`(번호 없는 줄), `bento`(첫 칸이 큰 바둑판), `steps`(번호 붙은 단계)
    #[serde(default = "feature_style")]
    pub style: String,
    /// 묶음 설명, 200자
    #[serde(default)]
    pub lead: String,
    /// 1~12개
    pub items: Vec<FeatureItem>,
    #[serde(default = "plain")]
    pub tone: String,
}

fn feature_style() -> String {
    "cards".into()
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StatItem {
    /// 큰 숫자, 20자. 예: `12년`
    pub value: String,
    /// 40자
    pub label: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StatsConfig {
    /// 1~6개
    pub items: Vec<StatItem>,
    #[serde(default = "plain")]
    pub tone: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct TestimonialItem {
    /// 400자
    pub quote: String,
    /// 40자
    #[serde(default)]
    pub name: String,
    /// 60자
    #[serde(default)]
    pub role: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct TestimonialsConfig {
    /// 1~9개
    pub items: Vec<TestimonialItem>,
    #[serde(default = "plain")]
    pub tone: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CtaConfig {
    /// 300자
    #[serde(default)]
    pub text: String,
    #[serde(default)]
    pub button: Button,
    #[serde(default)]
    pub button2: Button,
    #[serde(default = "strong")]
    pub tone: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct FaqItem {
    /// 질문, 200자
    pub q: String,
    /// 답, 마크다운 4000바이트
    pub a: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct FaqConfig {
    /// 1~30개
    pub items: Vec<FaqItem>,
    #[serde(default = "plain")]
    pub tone: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct HoursRow {
    /// 30자. 예: `평일`
    pub days: String,
    /// 40자. 예: `09:00 ~ 18:00`, `쉽니다`
    pub time: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct HoursConfig {
    /// 1~14줄
    pub rows: Vec<HoursRow>,
    /// 200자
    #[serde(default)]
    pub note: String,
    #[serde(default = "plain")]
    pub tone: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct VisitConfig {
    /// 장소 이름, 60자
    #[serde(default)]
    pub name: String,
    /// 주소, 200자. 지도 링크가 이 주소로 찾는다
    pub address: String,
    /// 전화번호, 30자
    #[serde(default)]
    pub phone: String,
    /// 대중교통 안내, 400자
    #[serde(default)]
    pub transit: String,
    /// 주차 안내, 200자
    #[serde(default)]
    pub parking: String,
    #[serde(default = "plain")]
    pub tone: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PriceItem {
    /// 60자
    pub name: String,
    /// 20자. 예: `5,500원`
    #[serde(default)]
    pub price: String,
    /// 120자
    #[serde(default)]
    pub note: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PriceGroup {
    /// 묶음 이름, 40자. 비워도 된다
    #[serde(default)]
    pub name: String,
    /// 1~40개
    pub items: Vec<PriceItem>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PriceListConfig {
    /// 1~8묶음
    pub groups: Vec<PriceGroup>,
    /// 200자
    #[serde(default)]
    pub note: String,
    #[serde(default = "plain")]
    pub tone: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Plan {
    /// 40자
    pub name: String,
    /// 20자
    pub price: String,
    /// 20자. 예: `월`
    #[serde(default)]
    pub period: String,
    /// 120자
    #[serde(default)]
    pub note: String,
    /// 줄마다 60자, 12개까지
    #[serde(default)]
    pub features: Vec<String>,
    #[serde(default)]
    pub button: Button,
    /// 눈에 띄게 그린다. 하나만
    #[serde(default)]
    pub featured: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PlansConfig {
    /// 1~4개
    pub plans: Vec<Plan>,
    #[serde(default = "plain")]
    pub tone: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct TimelineItem {
    /// 20자. 예: `2024. 3`
    pub date: String,
    /// 80자
    pub title: String,
    /// 300자
    #[serde(default)]
    pub text: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct TimelineConfig {
    /// 1~60개
    pub items: Vec<TimelineItem>,
    #[serde(default = "plain")]
    pub tone: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct OrgNode {
    /// 부서·이름, 40자
    pub name: String,
    /// 직함, 40자
    #[serde(default)]
    pub role: String,
    /// 아래 조직. 전체 3단, 60칸까지
    #[serde(default)]
    pub children: Vec<OrgNode>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct OrgChartConfig {
    /// 맨 위 칸. 보통 하나
    pub nodes: Vec<OrgNode>,
    #[serde(default = "plain")]
    pub tone: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct TeamMember {
    /// 40자
    pub name: String,
    /// 40자
    #[serde(default)]
    pub role: String,
    /// 올린 그림 주소(`/media/...`)
    #[serde(default)]
    pub image: String,
    /// 200자
    #[serde(default)]
    pub bio: String,
    #[serde(default)]
    pub link: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct TeamConfig {
    /// 1~24명
    pub items: Vec<TeamMember>,
    #[serde(default = "plain")]
    pub tone: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ImageTextConfig {
    /// 올린 그림 주소(`/media/...`). 비우면 글만 그린다
    #[serde(default)]
    pub image: String,
    #[serde(default)]
    pub image_alt: String,
    /// 마크다운 4000바이트
    pub body: String,
    #[serde(default)]
    pub button: Button,
    /// 그림을 오른쪽에
    #[serde(default)]
    pub flip: bool,
    #[serde(default = "plain")]
    pub tone: String,
}

pub(super) fn schema(kind: &str) -> Option<Value> {
    Some(match kind {
        "hero" => schema_of::<HeroConfig>(),
        "features" => schema_of::<FeaturesConfig>(),
        "stats" => schema_of::<StatsConfig>(),
        "testimonials" => schema_of::<TestimonialsConfig>(),
        "cta" => schema_of::<CtaConfig>(),
        "faq" => schema_of::<FaqConfig>(),
        "hours" => schema_of::<HoursConfig>(),
        "visit" => schema_of::<VisitConfig>(),
        "price_list" => schema_of::<PriceListConfig>(),
        "plans" => schema_of::<PlansConfig>(),
        "timeline" => schema_of::<TimelineConfig>(),
        "org_chart" => schema_of::<OrgChartConfig>(),
        "team" => schema_of::<TeamConfig>(),
        "image_text" => schema_of::<ImageTextConfig>(),
        _ => return None,
    })
}

pub fn is_section(kind: &str) -> bool {
    KINDS.iter().any(|(id, ..)| *id == kind)
}

fn tone(kind: &str, t: &mut String) -> Result<()> {
    *t = t.trim().to_owned();
    if TONES.contains(&t.as_str()) {
        return Ok(());
    }
    Err(bad_config(kind, "tone 은 plain·soft·strong"))
}

fn button(kind: &str, b: &mut Button) -> Result<()> {
    b.label = short_text(kind, "button.label", &b.label, 20, false)?;
    b.link = check_link(kind, &b.link)?;
    if b.label.is_empty() != b.link.is_empty() {
        return Err(bad_config(kind, "단추는 label 과 link 를 함께 적습니다"));
    }
    Ok(())
}

fn count(kind: &str, field: &str, n: usize, lo: usize, hi: usize) -> Result<()> {
    if (lo..=hi).contains(&n) {
        return Ok(());
    }
    Err(bad_config(kind, format!("{field} 는 {lo}~{hi}개")))
}

fn text(kind: &str, field: &str, v: &mut String, max: usize) -> Result<()> {
    *v = short_text(kind, field, v, max, false)?;
    Ok(())
}

fn required(kind: &str, field: &str, v: &mut String, max: usize) -> Result<()> {
    *v = short_text(kind, field, v, max, true)?;
    Ok(())
}

fn image(v: &mut String) -> Result<()> {
    *v = posts::check_cover(v)?.unwrap_or_default();
    Ok(())
}

fn markdown(kind: &str, field: &str, v: &mut String) -> Result<()> {
    *v = super::body_text(std::mem::take(v), MAX_MARKDOWN)?;
    if v.trim().is_empty() {
        return Err(bad_config(kind, format!("{field} 가 비었습니다")));
    }
    Ok(())
}

fn org_nodes(kind: &str, nodes: &mut [OrgNode], depth: usize, total: &mut usize) -> Result<()> {
    if !nodes.is_empty() && depth > ORG_DEPTH {
        return Err(bad_config(kind, format!("조직도는 {ORG_DEPTH}단까지")));
    }
    for n in nodes {
        *total += 1;
        if *total > ORG_NODES {
            return Err(bad_config(kind, format!("조직도는 {ORG_NODES}칸까지")));
        }
        required(kind, "name", &mut n.name, 40)?;
        text(kind, "role", &mut n.role, 40)?;
        org_nodes(kind, &mut n.children, depth + 1, total)?;
    }
    Ok(())
}

/// 섹션 종류면 설정을 검사해 정규화한 값을, 아니면 `None` 을 돌려준다.
pub(super) fn check(kind: &str, config: Value) -> Option<Result<Value>> {
    if !is_section(kind) {
        return None;
    }
    Some(check_inner(kind, config))
}

fn check_inner(kind: &str, config: Value) -> Result<Value> {
    Ok(match kind {
        "hero" => {
            let mut c: HeroConfig = parse(kind, config)?;
            c.style = c.style.trim().to_owned();
            if !HERO_STYLES.contains(&c.style.as_str()) {
                return Err(bad_config(kind, "style 은 type·image·split"));
            }
            text(kind, "eyebrow", &mut c.eyebrow, 40)?;
            text(kind, "title", &mut c.title, 120)?;
            text(kind, "text", &mut c.text, 300)?;
            text(kind, "image_alt", &mut c.image_alt, 120)?;
            image(&mut c.image)?;
            button(kind, &mut c.button)?;
            button(kind, &mut c.button2)?;
            tone(kind, &mut c.tone)?;
            json!(c)
        }
        "features" => {
            let mut c: FeaturesConfig = parse(kind, config)?;
            c.style = c.style.trim().to_owned();
            if !FEATURE_STYLES.contains(&c.style.as_str()) {
                return Err(bad_config(kind, "style 은 cards·rows·bento·steps"));
            }
            text(kind, "lead", &mut c.lead, 200)?;
            count(kind, "items", c.items.len(), 1, 12)?;
            for i in &mut c.items {
                required(kind, "title", &mut i.title, 60)?;
                text(kind, "text", &mut i.text, 300)?;
                i.link = check_link(kind, &i.link)?;
            }
            tone(kind, &mut c.tone)?;
            json!(c)
        }
        "stats" => {
            let mut c: StatsConfig = parse(kind, config)?;
            count(kind, "items", c.items.len(), 1, 6)?;
            for i in &mut c.items {
                required(kind, "value", &mut i.value, 20)?;
                required(kind, "label", &mut i.label, 40)?;
            }
            tone(kind, &mut c.tone)?;
            json!(c)
        }
        "testimonials" => {
            let mut c: TestimonialsConfig = parse(kind, config)?;
            count(kind, "items", c.items.len(), 1, 9)?;
            for i in &mut c.items {
                required(kind, "quote", &mut i.quote, 400)?;
                text(kind, "name", &mut i.name, 40)?;
                text(kind, "role", &mut i.role, 60)?;
            }
            tone(kind, &mut c.tone)?;
            json!(c)
        }
        "cta" => {
            let mut c: CtaConfig = parse(kind, config)?;
            text(kind, "text", &mut c.text, 300)?;
            button(kind, &mut c.button)?;
            button(kind, &mut c.button2)?;
            tone(kind, &mut c.tone)?;
            json!(c)
        }
        "faq" => {
            let mut c: FaqConfig = parse(kind, config)?;
            count(kind, "items", c.items.len(), 1, 30)?;
            for i in &mut c.items {
                required(kind, "q", &mut i.q, 200)?;
                markdown(kind, "a", &mut i.a)?;
            }
            tone(kind, &mut c.tone)?;
            json!(c)
        }
        "hours" => {
            let mut c: HoursConfig = parse(kind, config)?;
            count(kind, "rows", c.rows.len(), 1, 14)?;
            for r in &mut c.rows {
                required(kind, "days", &mut r.days, 30)?;
                required(kind, "time", &mut r.time, 40)?;
            }
            text(kind, "note", &mut c.note, 200)?;
            tone(kind, &mut c.tone)?;
            json!(c)
        }
        "visit" => {
            let mut c: VisitConfig = parse(kind, config)?;
            text(kind, "name", &mut c.name, 60)?;
            required(kind, "address", &mut c.address, 200)?;
            text(kind, "phone", &mut c.phone, 30)?;
            text(kind, "transit", &mut c.transit, 400)?;
            text(kind, "parking", &mut c.parking, 200)?;
            tone(kind, &mut c.tone)?;
            json!(c)
        }
        "price_list" => {
            let mut c: PriceListConfig = parse(kind, config)?;
            count(kind, "groups", c.groups.len(), 1, 8)?;
            for g in &mut c.groups {
                text(kind, "name", &mut g.name, 40)?;
                count(kind, "items", g.items.len(), 1, 40)?;
                for i in &mut g.items {
                    required(kind, "name", &mut i.name, 60)?;
                    text(kind, "price", &mut i.price, 20)?;
                    text(kind, "note", &mut i.note, 120)?;
                }
            }
            text(kind, "note", &mut c.note, 200)?;
            tone(kind, &mut c.tone)?;
            json!(c)
        }
        "plans" => {
            let mut c: PlansConfig = parse(kind, config)?;
            count(kind, "plans", c.plans.len(), 1, 4)?;
            if c.plans.iter().filter(|p| p.featured).count() > 1 {
                return Err(bad_config(kind, "featured 는 하나만"));
            }
            for p in &mut c.plans {
                required(kind, "name", &mut p.name, 40)?;
                required(kind, "price", &mut p.price, 20)?;
                text(kind, "period", &mut p.period, 20)?;
                text(kind, "note", &mut p.note, 120)?;
                count(kind, "features", p.features.len(), 0, 12)?;
                for f in &mut p.features {
                    required(kind, "features", f, 60)?;
                }
                button(kind, &mut p.button)?;
            }
            tone(kind, &mut c.tone)?;
            json!(c)
        }
        "timeline" => {
            let mut c: TimelineConfig = parse(kind, config)?;
            count(kind, "items", c.items.len(), 1, 60)?;
            for i in &mut c.items {
                required(kind, "date", &mut i.date, 20)?;
                required(kind, "title", &mut i.title, 80)?;
                text(kind, "text", &mut i.text, 300)?;
            }
            tone(kind, &mut c.tone)?;
            json!(c)
        }
        "org_chart" => {
            let mut c: OrgChartConfig = parse(kind, config)?;
            count(kind, "nodes", c.nodes.len(), 1, 8)?;
            let mut total = 0;
            org_nodes(kind, &mut c.nodes, 1, &mut total)?;
            tone(kind, &mut c.tone)?;
            json!(c)
        }
        "team" => {
            let mut c: TeamConfig = parse(kind, config)?;
            count(kind, "items", c.items.len(), 1, 24)?;
            for i in &mut c.items {
                required(kind, "name", &mut i.name, 40)?;
                text(kind, "role", &mut i.role, 40)?;
                text(kind, "bio", &mut i.bio, 200)?;
                image(&mut i.image)?;
                i.link = check_link(kind, &i.link)?;
            }
            tone(kind, &mut c.tone)?;
            json!(c)
        }
        _ => {
            let mut c: ImageTextConfig = parse(kind, config)?;
            image(&mut c.image)?;
            text(kind, "image_alt", &mut c.image_alt, 120)?;
            markdown(kind, "body", &mut c.body)?;
            button(kind, &mut c.button)?;
            tone(kind, &mut c.tone)?;
            json!(c)
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_kind_has_a_schema_and_defaults_are_filled() {
        for (id, ..) in KINDS {
            assert!(schema(id).is_some(), "{id}");
        }
        let v = check("hero", json!({})).unwrap().unwrap();
        assert_eq!(v["style"], "type");
        assert_eq!(v["tone"], "plain");
        assert!(check("latest_posts", json!({})).is_none());
    }

    #[test]
    fn rejects_bad_values() {
        let bad = |kind: &str, v: Value| check(kind, v).unwrap().is_err();
        assert!(bad("hero", json!({ "style": "video" })));
        assert!(bad("hero", json!({ "tone": "neon" })));
        assert!(bad("hero", json!({ "button": { "label": "보기" } })));
        assert!(bad(
            "hero",
            json!({ "button": { "label": "보기", "link": "javascript:alert(1)" } })
        ));
        assert!(bad("features", json!({ "items": [] })));
        assert!(bad("faq", json!({ "items": [{ "q": "왜?", "a": " " }] })));
        assert!(bad("visit", json!({ "address": "" })));
        assert!(bad("cta", json!({ "unknown": 1 })));
        assert!(bad(
            "plans",
            json!({ "plans": [
                { "name": "가", "price": "1", "featured": true },
                { "name": "나", "price": "2", "featured": true }
            ] })
        ));
    }

    #[test]
    fn org_chart_depth_is_limited() {
        let leaf = json!({ "name": "넷째" });
        let third = json!({ "name": "셋째", "children": [leaf] });
        let second = json!({ "name": "둘째", "children": [third.clone()] });
        let too_deep = json!({ "nodes": [{ "name": "첫째", "children": [second] }] });
        assert!(check("org_chart", too_deep).unwrap().is_err());
        let ok = json!({ "nodes": [{ "name": "첫째", "children": [{ "name": "둘째", "children": [{ "name": "셋째" }] }] }] });
        assert!(check("org_chart", ok).unwrap().is_ok());
    }
}
