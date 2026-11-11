//! 섹션 블록 (ADR-0125, SPEC §8.5). 홈페이지를 이루는 큰 조각을 위젯 종류로 둔다.
//!
//! ADR-0020 대로 블록과 위젯은 렌더 계약이 하나다. 에이전트는 CSS 를 쓰지 않고 이 종류들을
//! 고르고 칸을 채운다. 글자 칸은 길이만 보고 그대로 두며 테마가 이스케이프한다. 마크다운을
//! 받는 칸(`faq.a`, `image_text.body`)은 web 이 정화 렌더러로 그린다.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::widgets::{bad_config, check_link, check_time, parse, schema_of, short_text};
use crate::directives;
use crate::error::Result;
use crate::services::posts;

pub const KINDS: [(&str, &str, &str); 20] = [
    (
        "hero",
        "첫 인상",
        "사이트 맨 위 큰 제목·설명·단추. 글자만, 배경 그림, 좌우 나눔",
    ),
    (
        "features",
        "소개 묶음",
        "강점·서비스·절차를 카드·줄·벤토·단계로, 분류·바로가기는 작은 칸으로",
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
    // ADR-0156
    ("video", "동영상", "유튜브·비메오나 올린 동영상 하나와 설명"),
    (
        "gallery",
        "사진 전시",
        "올린 사진 여러 장을 격자나 옆으로 넘기는 띠로",
    ),
    ("logos", "함께하는 곳", "협력사·고객사·후원처 로고 줄"),
    (
        "countdown",
        "남은 날",
        "행사·개업·마감까지 남은 날짜와 시간",
    ),
    (
        "social",
        "SNS 링크",
        "인스타그램·유튜브·블로그·카카오톡 채널 같은 바로가기 단추",
    ),
    ("quote", "큰 인용", "한두 문장을 크게. 대표 인사말·좌우명"),
];

/// 동영상 비율 (ADR-0156)
pub const VIDEO_RATIOS: [&str; 4] = ["16x9", "4x3", "1x1", "9x16"];
/// 사진 전시 모양 (ADR-0156)
pub const GALLERY_STYLES: [&str; 2] = ["grid", "strip"];
/// SNS 링크 종류와 단추 글자 (ADR-0156). 상표 그림은 쓰지 않는다(원칙 7).
pub const SOCIAL_SERVICES: [(&str, &str); 12] = [
    ("instagram", "인스타그램"),
    ("youtube", "유튜브"),
    ("blog", "블로그"),
    ("kakao", "카카오톡 채널"),
    ("x", "X"),
    ("facebook", "페이스북"),
    ("threads", "스레드"),
    ("tiktok", "틱톡"),
    ("band", "밴드"),
    ("email", "이메일"),
    ("phone", "전화"),
    ("link", "바로가기"),
];

/// 띠 색: 바탕 그대로, 옅은 면, 주 색
pub const TONES: [&str; 3] = ["plain", "soft", "strong"];
/// 띠 위아래 여백 (ADR-0148)
pub const SPACES: [&str; 3] = ["tight", "normal", "roomy"];
/// 띠 글 정렬 (ADR-0148)
pub const ALIGNS: [&str; 2] = ["start", "center"];
pub const HERO_STYLES: [&str; 3] = ["type", "image", "split"];
pub const FEATURE_STYLES: [&str; 5] = ["cards", "rows", "bento", "steps", "tiles"];

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
    /// 위아래 여백: `tight`·`normal`(기본)·`roomy` (ADR-0148)
    #[serde(default = "normal_space")]
    pub space: String,
    /// 글 정렬: `start`(기본)·`center` (ADR-0148)
    #[serde(default = "start_align")]
    pub align: String,
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
    /// `cards`, `rows`(번호 없는 줄), `bento`(첫 칸이 큰 바둑판), `steps`(번호 붙은 단계),
    /// `tiles`(분류·바로가기를 작은 칸으로 촘촘히, ADR-0140)
    #[serde(default = "feature_style")]
    pub style: String,
    /// 묶음 설명, 200자
    #[serde(default)]
    pub lead: String,
    /// 1~12개
    pub items: Vec<FeatureItem>,
    #[serde(default = "plain")]
    pub tone: String,
    /// 위아래 여백: `tight`·`normal`(기본)·`roomy` (ADR-0148)
    #[serde(default = "normal_space")]
    pub space: String,
    /// 글 정렬: `start`(기본)·`center` (ADR-0148)
    #[serde(default = "start_align")]
    pub align: String,
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
    /// 위아래 여백: `tight`·`normal`(기본)·`roomy` (ADR-0148)
    #[serde(default = "normal_space")]
    pub space: String,
    /// 글 정렬: `start`(기본)·`center` (ADR-0148)
    #[serde(default = "start_align")]
    pub align: String,
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
    /// 위아래 여백: `tight`·`normal`(기본)·`roomy` (ADR-0148)
    #[serde(default = "normal_space")]
    pub space: String,
    /// 글 정렬: `start`(기본)·`center` (ADR-0148)
    #[serde(default = "start_align")]
    pub align: String,
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
    /// 위아래 여백: `tight`·`normal`(기본)·`roomy` (ADR-0148)
    #[serde(default = "normal_space")]
    pub space: String,
    /// 글 정렬: `start`(기본)·`center` (ADR-0148)
    #[serde(default = "start_align")]
    pub align: String,
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
    /// 위아래 여백: `tight`·`normal`(기본)·`roomy` (ADR-0148)
    #[serde(default = "normal_space")]
    pub space: String,
    /// 글 정렬: `start`(기본)·`center` (ADR-0148)
    #[serde(default = "start_align")]
    pub align: String,
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
    /// 위아래 여백: `tight`·`normal`(기본)·`roomy` (ADR-0148)
    #[serde(default = "normal_space")]
    pub space: String,
    /// 글 정렬: `start`(기본)·`center` (ADR-0148)
    #[serde(default = "start_align")]
    pub align: String,
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
    /// 위아래 여백: `tight`·`normal`(기본)·`roomy` (ADR-0148)
    #[serde(default = "normal_space")]
    pub space: String,
    /// 글 정렬: `start`(기본)·`center` (ADR-0148)
    #[serde(default = "start_align")]
    pub align: String,
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
    /// 위아래 여백: `tight`·`normal`(기본)·`roomy` (ADR-0148)
    #[serde(default = "normal_space")]
    pub space: String,
    /// 글 정렬: `start`(기본)·`center` (ADR-0148)
    #[serde(default = "start_align")]
    pub align: String,
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
    /// 위아래 여백: `tight`·`normal`(기본)·`roomy` (ADR-0148)
    #[serde(default = "normal_space")]
    pub space: String,
    /// 글 정렬: `start`(기본)·`center` (ADR-0148)
    #[serde(default = "start_align")]
    pub align: String,
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
    /// 위아래 여백: `tight`·`normal`(기본)·`roomy` (ADR-0148)
    #[serde(default = "normal_space")]
    pub space: String,
    /// 글 정렬: `start`(기본)·`center` (ADR-0148)
    #[serde(default = "start_align")]
    pub align: String,
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
    /// 위아래 여백: `tight`·`normal`(기본)·`roomy` (ADR-0148)
    #[serde(default = "normal_space")]
    pub space: String,
    /// 글 정렬: `start`(기본)·`center` (ADR-0148)
    #[serde(default = "start_align")]
    pub align: String,
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
    /// 위아래 여백: `tight`·`normal`(기본)·`roomy` (ADR-0148)
    #[serde(default = "normal_space")]
    pub space: String,
    /// 글 정렬: `start`(기본)·`center` (ADR-0148)
    #[serde(default = "start_align")]
    pub align: String,
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
    /// 위아래 여백: `tight`·`normal`(기본)·`roomy` (ADR-0148)
    #[serde(default = "normal_space")]
    pub space: String,
    /// 글 정렬: `start`(기본)·`center` (ADR-0148)
    #[serde(default = "start_align")]
    pub align: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct VideoConfig {
    /// 유튜브·비메오 주소, 또는 올린 mp4·webm 동영상(/media/…)
    pub url: String,
    /// 올린 동영상을 소리 없이 저절로 되풀이 (움직이는 배경처럼). 유튜브·비메오에는 쓰지 않는다
    #[serde(default)]
    pub loop_muted: bool,
    /// 동영상 아래 설명, 300자
    #[serde(default)]
    pub text: String,
    /// 화면 비율. `16x9`(기본) · `4x3` · `1x1` · `9x16`(쇼츠)
    #[serde(default = "wide")]
    pub ratio: String,
    #[serde(default = "plain")]
    pub tone: String,
    /// 위아래 여백: `tight`·`normal`(기본)·`roomy` (ADR-0148)
    #[serde(default = "normal_space")]
    pub space: String,
    /// 글 정렬: `start`(기본)·`center` (ADR-0148)
    #[serde(default = "start_align")]
    pub align: String,
}

fn wide() -> String {
    "16x9".into()
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct GalleryItem {
    /// 올린 그림 주소(`/media/...`). 비운 칸은 공개 화면에 보이지 않는다
    #[serde(default)]
    pub image: String,
    /// 그림 설명, 120자
    #[serde(default)]
    pub alt: String,
    /// 그림 아래 글, 120자
    #[serde(default)]
    pub caption: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct GalleryConfig {
    /// 모양. `grid`(격자, 기본) · `strip`(옆으로 넘기는 띠)
    #[serde(default = "grid")]
    pub style: String,
    /// 한 줄 칸 수, 2~4 (격자)
    #[serde(default = "three")]
    pub columns: u8,
    /// 1~24장
    pub items: Vec<GalleryItem>,
    #[serde(default = "plain")]
    pub tone: String,
    /// 위아래 여백: `tight`·`normal`(기본)·`roomy` (ADR-0148)
    #[serde(default = "normal_space")]
    pub space: String,
    /// 글 정렬: `start`(기본)·`center` (ADR-0148)
    #[serde(default = "start_align")]
    pub align: String,
}

fn grid() -> String {
    "grid".into()
}

fn three() -> u8 {
    3
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct LogoItem {
    /// 이름, 40자. 그림이 없으면 이름을 글자로 보인다
    pub name: String,
    /// 올린 로고 그림 주소(`/media/...`)
    #[serde(default)]
    pub image: String,
    #[serde(default)]
    pub link: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct LogosConfig {
    /// 1~24개
    pub items: Vec<LogoItem>,
    /// 로고를 흑백으로 가라앉혀 보인다(올리면 원래 색)
    #[serde(default = "yes")]
    pub mono: bool,
    #[serde(default = "plain")]
    pub tone: String,
    /// 위아래 여백: `tight`·`normal`(기본)·`roomy` (ADR-0148)
    #[serde(default = "normal_space")]
    pub space: String,
    /// 글 정렬: `start`(기본)·`center` (ADR-0148)
    #[serde(default = "center_align")]
    pub align: String,
}

fn yes() -> bool {
    true
}

fn center_align() -> String {
    "center".into()
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CountdownConfig {
    /// 목표 시각
    #[schemars(extend("format" = "date-time"))]
    pub target: String,
    /// 숫자 아래 설명, 200자
    #[serde(default)]
    pub text: String,
    /// 지난 뒤 숫자 대신 보일 글, 60자. 비우면 「끝났습니다」
    #[serde(default)]
    pub done: String,
    #[serde(default)]
    pub button: Button,
    #[serde(default = "soft")]
    pub tone: String,
    /// 위아래 여백: `tight`·`normal`(기본)·`roomy` (ADR-0148)
    #[serde(default = "normal_space")]
    pub space: String,
    /// 글 정렬: `start`·`center`(기본) (ADR-0148)
    #[serde(default = "center_align")]
    pub align: String,
}

fn soft() -> String {
    "soft".into()
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SocialItem {
    /// 종류. `instagram`(인스타그램) · `youtube`(유튜브) · `blog`(블로그) · `kakao`(카카오톡 채널) · `x`(X) ·
    /// `facebook`(페이스북) · `threads`(스레드) · `tiktok`(틱톡) · `band`(밴드) · `email`(이메일) ·
    /// `phone`(전화) · `link`(바로가기)
    pub service: String,
    /// 주소. https 주소, 이메일은 메일 주소, 전화는 번호
    pub url: String,
    /// 단추 글자, 20자. 비우면 종류 이름
    #[serde(default)]
    pub label: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SocialConfig {
    /// 1~12개
    pub items: Vec<SocialItem>,
    /// 단추 위 글, 200자
    #[serde(default)]
    pub text: String,
    #[serde(default = "plain")]
    pub tone: String,
    /// 위아래 여백: `tight`·`normal`(기본)·`roomy` (ADR-0148)
    #[serde(default = "normal_space")]
    pub space: String,
    /// 글 정렬: `start`·`center`(기본) (ADR-0148)
    #[serde(default = "center_align")]
    pub align: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct QuoteConfig {
    /// 인용할 말, 300자
    pub text: String,
    /// 누가 했는지, 40자
    #[serde(default)]
    pub name: String,
    /// 그 사람의 자리, 60자
    #[serde(default)]
    pub role: String,
    #[serde(default = "soft")]
    pub tone: String,
    /// 위아래 여백: `tight`·`normal`(기본)·`roomy` (ADR-0148)
    #[serde(default = "normal_space")]
    pub space: String,
    /// 글 정렬: `start`·`center`(기본) (ADR-0148)
    #[serde(default = "center_align")]
    pub align: String,
}

/// 모든 섹션이 함께 받는 칸 (ADR-0156). 종류별 구조체 밖에서 읽어 검사하고 다시 붙인다.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Common {
    /// 구역 이름. 영문 소문자·숫자·`-` 32자. 메뉴 링크를 `#s-이름` 으로 쓰면 이 블록으로 바로 간다
    #[serde(default)]
    pub anchor: String,
    /// 배경 그림 (ADR-0157). 보관소 그림 주소. 있으면 어두운 덮개를 깔고 글자를 밝게 해 대비를 지킨다
    #[serde(default)]
    pub bg_image: String,
    /// 배경 덮개 짙기: `light`·`medium`(기본)·`strong`. 배경 그림이 있을 때만 쓴다
    #[serde(default = "medium")]
    pub bg_dim: String,
    /// 등장 효과: `none`(기본)·`fade`·`rise`. 움직임 줄이기 설정이면 움직이지 않는다
    #[serde(default = "no_motion")]
    pub motion: String,
}

const COMMON_KEYS: [&str; 4] = ["anchor", "bg_image", "bg_dim", "motion"];
pub const BG_DIMS: [&str; 3] = ["light", "medium", "strong"];
pub const MOTIONS: [&str; 3] = ["none", "fade", "rise"];

fn medium() -> String {
    "medium".into()
}

fn no_motion() -> String {
    "none".into()
}

/// 종류의 설정 schema 에 공통 칸을 더한다.
fn with_common(mut s: Value) -> Value {
    let common = schema_of::<Common>();
    if let (Some(props), Some(extra)) = (
        s.get_mut("properties").and_then(Value::as_object_mut),
        common.get("properties").and_then(Value::as_object),
    ) {
        for (k, v) in extra {
            props.insert(k.clone(), v.clone());
        }
    }
    s
}

pub(super) fn schema(kind: &str) -> Option<Value> {
    if !is_section(kind) {
        return None;
    }
    Some(with_common(match kind {
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
        "video" => schema_of::<VideoConfig>(),
        "gallery" => schema_of::<GalleryConfig>(),
        "logos" => schema_of::<LogosConfig>(),
        "countdown" => schema_of::<CountdownConfig>(),
        "social" => schema_of::<SocialConfig>(),
        _ => schema_of::<QuoteConfig>(),
    }))
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

fn normal_space() -> String {
    "normal".into()
}

fn start_align() -> String {
    "start".into()
}

/// 띠 여백·정렬 (ADR-0148). 비면 기본값이다.
fn look(kind: &str, space: &mut String, align: &mut String) -> Result<()> {
    *space = space.trim().to_owned();
    *align = align.trim().to_owned();
    if space.is_empty() {
        *space = normal_space();
    }
    if align.is_empty() {
        *align = start_align();
    }
    if !SPACES.contains(&space.as_str()) {
        return Err(bad_config(kind, "space 는 tight·normal·roomy"));
    }
    if !ALIGNS.contains(&align.as_str()) {
        return Err(bad_config(kind, "align 은 start·center"));
    }
    Ok(())
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

fn check_inner(kind: &str, mut config: Value) -> Result<Value> {
    let common = take_common(kind, &mut config)?;
    let mut out = check_kind(kind, config)?;
    if let Some(m) = out.as_object_mut() {
        if !common.anchor.is_empty() {
            m.insert("anchor".into(), json!(common.anchor));
        }
        if !common.bg_image.is_empty() {
            m.insert("bg_image".into(), json!(common.bg_image));
            m.insert("bg_dim".into(), json!(common.bg_dim));
        }
        if common.motion != "none" {
            m.insert("motion".into(), json!(common.motion));
        }
    }
    Ok(out)
}

/// 공통 칸을 설정에서 빼내 검사한다. 종류별 구조체는 모르는 칸을 거절하므로 먼저 뺀다.
fn take_common(kind: &str, config: &mut Value) -> Result<Common> {
    let mut picked = serde_json::Map::new();
    if let Some(m) = config.as_object_mut() {
        for k in COMMON_KEYS {
            if let Some(v) = m.remove(k) {
                picked.insert(k.into(), v);
            }
        }
    }
    let mut c: Common = parse(kind, Value::Object(picked))?;
    c.anchor = c.anchor.trim().to_ascii_lowercase();
    let a = c.anchor.as_str();
    let ok = a.is_empty()
        || (a.len() <= 32
            && a.bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
            && !a.starts_with('-')
            && !a.ends_with('-'));
    if !ok {
        return Err(bad_config(kind, "anchor 는 영문 소문자·숫자·- 로 32자까지"));
    }
    image(&mut c.bg_image)?;
    for (field, v, allowed, default) in [
        ("bg_dim", &mut c.bg_dim, &BG_DIMS, "medium"),
        ("motion", &mut c.motion, &MOTIONS, "none"),
    ] {
        *v = v.trim().to_owned();
        if v.is_empty() {
            *v = default.into();
        }
        if !allowed.contains(&v.as_str()) {
            return Err(bad_config(
                kind,
                format!("{field} 는 {}", allowed.join("·")),
            ));
        }
    }
    Ok(c)
}

/// SNS 단추 주소 (ADR-0156). 이메일은 `mailto:`, 전화는 `tel:` 로 바꾸고, 나머지는 http(s) 주소만 받는다.
/// `link`(바로가기)는 사이트 안 주소도 받는다.
fn social_url(kind: &str, service: &str, url: &str) -> Result<String> {
    let u = url.trim();
    let bad = || bad_config(kind, format!("SNS 주소가 올바르지 않습니다: {u}"));
    if u.is_empty() || u.len() > 300 || u.chars().any(char::is_control) {
        return Err(bad());
    }
    match service {
        "email" => {
            let addr = u.strip_prefix("mailto:").unwrap_or(u);
            let (name, host) = addr.split_once('@').ok_or_else(bad)?;
            let clean = |s: &str| {
                !s.is_empty()
                    && s.chars()
                        .all(|c| c.is_alphanumeric() || matches!(c, '.' | '_' | '-' | '+'))
            };
            if !clean(name) || !clean(host) || !host.contains('.') {
                return Err(bad());
            }
            Ok(format!("mailto:{addr}"))
        }
        "phone" => {
            let n = u.strip_prefix("tel:").unwrap_or(u);
            if !n
                .chars()
                .all(|c| c.is_ascii_digit() || matches!(c, '+' | '-' | ' ' | '(' | ')'))
            {
                return Err(bad());
            }
            let digits: String = n
                .chars()
                .filter(|c| c.is_ascii_digit() || *c == '+')
                .collect();
            let count = digits.chars().filter(char::is_ascii_digit).count();
            if !(3..=15).contains(&count) || digits[1..].contains('+') {
                return Err(bad());
            }
            Ok(format!("tel:{digits}"))
        }
        "link" => check_link(kind, u),
        _ => {
            if !(u.starts_with("https://") || u.starts_with("http://")) {
                return Err(bad());
            }
            check_link(kind, u)
        }
    }
}

fn check_kind(kind: &str, config: Value) -> Result<Value> {
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
            look(kind, &mut c.space, &mut c.align)?;
            json!(c)
        }
        "features" => {
            let mut c: FeaturesConfig = parse(kind, config)?;
            c.style = c.style.trim().to_owned();
            if !FEATURE_STYLES.contains(&c.style.as_str()) {
                return Err(bad_config(kind, "style 은 cards·rows·bento·steps·tiles"));
            }
            text(kind, "lead", &mut c.lead, 200)?;
            count(kind, "items", c.items.len(), 1, 12)?;
            for i in &mut c.items {
                required(kind, "title", &mut i.title, 60)?;
                text(kind, "text", &mut i.text, 300)?;
                i.link = check_link(kind, &i.link)?;
            }
            tone(kind, &mut c.tone)?;
            look(kind, &mut c.space, &mut c.align)?;
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
            look(kind, &mut c.space, &mut c.align)?;
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
            look(kind, &mut c.space, &mut c.align)?;
            json!(c)
        }
        "cta" => {
            let mut c: CtaConfig = parse(kind, config)?;
            text(kind, "text", &mut c.text, 300)?;
            button(kind, &mut c.button)?;
            button(kind, &mut c.button2)?;
            tone(kind, &mut c.tone)?;
            look(kind, &mut c.space, &mut c.align)?;
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
            look(kind, &mut c.space, &mut c.align)?;
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
            look(kind, &mut c.space, &mut c.align)?;
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
            look(kind, &mut c.space, &mut c.align)?;
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
            look(kind, &mut c.space, &mut c.align)?;
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
            look(kind, &mut c.space, &mut c.align)?;
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
            look(kind, &mut c.space, &mut c.align)?;
            json!(c)
        }
        "org_chart" => {
            let mut c: OrgChartConfig = parse(kind, config)?;
            count(kind, "nodes", c.nodes.len(), 1, 8)?;
            let mut total = 0;
            org_nodes(kind, &mut c.nodes, 1, &mut total)?;
            tone(kind, &mut c.tone)?;
            look(kind, &mut c.space, &mut c.align)?;
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
            look(kind, &mut c.space, &mut c.align)?;
            json!(c)
        }
        "image_text" => {
            let mut c: ImageTextConfig = parse(kind, config)?;
            image(&mut c.image)?;
            text(kind, "image_alt", &mut c.image_alt, 120)?;
            markdown(kind, "body", &mut c.body)?;
            button(kind, &mut c.button)?;
            tone(kind, &mut c.tone)?;
            look(kind, &mut c.space, &mut c.align)?;
            json!(c)
        }
        "video" => {
            let mut c: VideoConfig = parse(kind, config)?;
            required(kind, "url", &mut c.url, 300)?;
            if directives::video_embed(&c.url).is_none() && !directives::local_video(&c.url) {
                return Err(bad_config(
                    kind,
                    "url 은 유튜브·비메오 주소나 올린 동영상(/media/…mp4·webm)만 받습니다",
                ));
            }
            text(kind, "text", &mut c.text, 300)?;
            c.ratio = c.ratio.trim().to_owned();
            if !VIDEO_RATIOS.contains(&c.ratio.as_str()) {
                return Err(bad_config(kind, "ratio 는 16x9·4x3·1x1·9x16"));
            }
            tone(kind, &mut c.tone)?;
            look(kind, &mut c.space, &mut c.align)?;
            json!(c)
        }
        "gallery" => {
            let mut c: GalleryConfig = parse(kind, config)?;
            c.style = c.style.trim().to_owned();
            if !GALLERY_STYLES.contains(&c.style.as_str()) {
                return Err(bad_config(kind, "style 은 grid·strip"));
            }
            if !(2..=4).contains(&c.columns) {
                return Err(bad_config(kind, "columns 는 2~4"));
            }
            count(kind, "items", c.items.len(), 1, 24)?;
            for i in &mut c.items {
                image(&mut i.image)?;
                text(kind, "alt", &mut i.alt, 120)?;
                text(kind, "caption", &mut i.caption, 120)?;
            }
            tone(kind, &mut c.tone)?;
            look(kind, &mut c.space, &mut c.align)?;
            json!(c)
        }
        "logos" => {
            let mut c: LogosConfig = parse(kind, config)?;
            count(kind, "items", c.items.len(), 1, 24)?;
            for i in &mut c.items {
                required(kind, "name", &mut i.name, 40)?;
                image(&mut i.image)?;
                i.link = check_link(kind, &i.link)?;
            }
            tone(kind, &mut c.tone)?;
            look(kind, &mut c.space, &mut c.align)?;
            json!(c)
        }
        "countdown" => {
            let mut c: CountdownConfig = parse(kind, config)?;
            c.target = check_time(kind, "target", &c.target)?;
            if c.target.is_empty() {
                return Err(bad_config(kind, "target 이 비었습니다"));
            }
            text(kind, "text", &mut c.text, 200)?;
            text(kind, "done", &mut c.done, 60)?;
            button(kind, &mut c.button)?;
            tone(kind, &mut c.tone)?;
            look(kind, &mut c.space, &mut c.align)?;
            json!(c)
        }
        "social" => {
            let mut c: SocialConfig = parse(kind, config)?;
            count(kind, "items", c.items.len(), 1, 12)?;
            for i in &mut c.items {
                i.service = i.service.trim().to_owned();
                if !SOCIAL_SERVICES.iter().any(|(id, _)| *id == i.service) {
                    return Err(bad_config(
                        kind,
                        format!("모르는 SNS 종류입니다: {}", i.service),
                    ));
                }
                i.url = social_url(kind, &i.service, &i.url)?;
                text(kind, "label", &mut i.label, 20)?;
            }
            text(kind, "text", &mut c.text, 200)?;
            tone(kind, &mut c.tone)?;
            look(kind, &mut c.space, &mut c.align)?;
            json!(c)
        }
        _ => {
            let mut c: QuoteConfig = parse(kind, config)?;
            required(kind, "text", &mut c.text, 300)?;
            text(kind, "name", &mut c.name, 40)?;
            text(kind, "role", &mut c.role, 60)?;
            tone(kind, &mut c.tone)?;
            look(kind, &mut c.space, &mut c.align)?;
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
    fn new_kinds_check_their_values() {
        let ok = |kind: &str, v: Value| check(kind, v).unwrap().unwrap();
        let bad = |kind: &str, v: Value| check(kind, v).unwrap().is_err();
        let v = ok("video", json!({ "url": "https://youtu.be/dQw4w9WgXcQ" }));
        assert_eq!(v["ratio"], "16x9");
        assert!(bad("video", json!({ "url": "https://example.com/a.mp4" })));
        let f = ok(
            "video",
            json!({ "url": "/media/2026/11/abc.mp4", "loop_muted": true }),
        );
        assert_eq!(f["loop_muted"], true);
        assert!(bad("video", json!({ "url": "/media/tmp/abc.part" })));
        assert!(bad("video", json!({ "url": "/media/../a.mp4" })));
        assert!(bad(
            "video",
            json!({ "url": "https://youtu.be/dQw4w9WgXcQ", "ratio": "21x9" })
        ));
        let g = ok(
            "gallery",
            json!({ "items": [{ "caption": "그림은 나중에" }] }),
        );
        assert_eq!(
            (g["style"].as_str(), g["columns"].as_u64()),
            (Some("grid"), Some(3))
        );
        assert!(bad("gallery", json!({ "items": [], "columns": 3 })));
        assert!(bad("gallery", json!({ "items": [{}], "columns": 5 })));
        assert!(bad(
            "gallery",
            json!({ "items": [{ "image": "https://example.com/a.png" }] })
        ));
        let l = ok("logos", json!({ "items": [{ "name": "협력사" }] }));
        assert_eq!(
            (l["mono"].as_bool(), l["align"].as_str()),
            (Some(true), Some("center"))
        );
        assert!(bad("logos", json!({ "items": [{ "name": "" }] })));
        let c = ok(
            "countdown",
            json!({ "target": "2026-12-24T18:00:00+09:00" }),
        );
        assert_eq!(c["target"], "2026-12-24T09:00:00.000000Z");
        assert!(bad("countdown", json!({ "target": "" })));
        assert!(bad("countdown", json!({ "target": "다음 주" })));
        assert!(ok("quote", json!({ "text": "늘 처음처럼" }))["tone"] == "soft");
        assert!(bad("quote", json!({ "text": " " })));
    }

    #[test]
    fn social_links_are_normalized() {
        let one = |service: &str, url: &str| {
            check(
                "social",
                json!({ "items": [{ "service": service, "url": url }] }),
            )
            .unwrap()
            .map(|v| v["items"][0]["url"].as_str().unwrap_or("").to_owned())
        };
        assert_eq!(
            one("email", "hello@example.com").unwrap(),
            "mailto:hello@example.com"
        );
        assert_eq!(one("phone", "02-123-4567").unwrap(), "tel:021234567");
        assert_eq!(
            one("phone", "+82 10 1234 5678").unwrap(),
            "tel:+821012345678"
        );
        assert_eq!(
            one("instagram", "https://instagram.com/x").unwrap(),
            "https://instagram.com/x"
        );
        assert_eq!(one("link", "/p/about").unwrap(), "/p/about");
        assert!(one("instagram", "/p/about").is_err());
        assert!(one("instagram", "javascript:alert(1)").is_err());
        assert!(one("email", "a@b").is_err());
        assert!(one("email", "a<b>@c.d").is_err());
        assert!(one("phone", "12").is_err());
        assert!(one("phone", "1+234").is_err());
        assert!(one("myspace", "https://example.com").is_err());
    }

    #[test]
    fn anchors_are_common_to_every_section() {
        for (id, ..) in KINDS {
            let s = schema(id).unwrap();
            assert!(s["properties"]["anchor"].is_object(), "{id}");
        }
        let v = check("cta", json!({ "anchor": " Contact-Us " }))
            .unwrap()
            .unwrap();
        assert_eq!(v["anchor"], "contact-us");
        let v = check("cta", json!({ "anchor": "" })).unwrap().unwrap();
        assert!(v.get("anchor").is_none());
        for a in ["오시는길", "-a", "a-", "a b", "a_b", &"a".repeat(33)] {
            assert!(
                check("cta", json!({ "anchor": a })).unwrap().is_err(),
                "{a}"
            );
        }
    }

    #[test]
    fn backgrounds_and_motion_are_common() {
        for (id, ..) in KINDS {
            let s = schema(id).unwrap();
            for k in ["bg_image", "bg_dim", "motion"] {
                assert!(s["properties"][k].is_object(), "{id} {k}");
            }
        }
        // 기본값은 저장하지 않는다. 덮개 짙기는 배경 그림이 있을 때만 남는다.
        let v = check("cta", json!({ "bg_dim": "strong", "motion": "none" }))
            .unwrap()
            .unwrap();
        assert!(
            v.get("bg_dim").is_none() && v.get("motion").is_none(),
            "{v}"
        );
        let v = check(
            "quote",
            json!({ "text": "한 문장", "bg_image": "/media/2026/11/a.jpg", "motion": "rise" }),
        )
        .unwrap()
        .unwrap();
        assert_eq!(v["bg_image"], "/media/2026/11/a.jpg");
        assert_eq!(v["bg_dim"], "medium");
        assert_eq!(v["motion"], "rise");
        for bad in [
            json!({ "bg_image": "https://example.com/a.jpg" }),
            json!({ "bg_image": "/media/a.jpg", "bg_dim": "black" }),
            json!({ "motion": "spin" }),
        ] {
            assert!(check("cta", bad.clone()).unwrap().is_err(), "{bad}");
        }
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
