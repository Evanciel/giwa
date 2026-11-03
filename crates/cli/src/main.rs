//! `maru` 바이너리.

mod commands;
mod doctor;
mod output;

use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Args, Parser, Subcommand};
use maru_core::{Code, Error};

/// 에이전트가 설치하고 운영하는 한국형 CMS.
#[derive(Debug, Parser)]
#[command(name = "maru", version, about)]
struct Cli {
    /// 데이터 디렉터리
    #[arg(long, global = true, env = "MARU_DATA", default_value = "./maru-data")]
    data: PathBuf,
    /// 결과를 SPEC §10.1 JSON 형식으로 출력
    #[arg(long, global = true)]
    json: bool,
    /// 확인 질문 없이 진행
    #[arg(long, global = true)]
    yes: bool,
    /// 진행 메시지를 줄인다
    #[arg(long, global = true)]
    quiet: bool,
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// 데이터 디렉터리를 만들고 프리셋을 적용한다 (여러 번 실행해도 안전)
    Init(InitArgs),
    /// 웹 서버를 실행한다
    Serve(ServeArgs),
    /// 글
    #[command(subcommand)]
    Post(PostCmd),
    /// 게시판
    #[command(subcommand)]
    Board(BoardCmd),
    /// 게시판 분류
    #[command(subcommand)]
    Category(CategoryCmd),
    /// 태그
    #[command(subcommand)]
    Tag(TagCmd),
    /// API 토큰
    #[command(subcommand)]
    Token(TokenCmd),
    /// 이미지 (업로드, 대체 텍스트)
    #[command(subcommand)]
    Media(MediaCmd),
    /// 첨부 파일 (PDF·문서·압축 등, 내려받기 전용)
    #[command(subcommand)]
    File(FileCmd),
    /// 글 틀 (새 글 본문 뼈대)
    #[command(subcommand)]
    Template(TemplateCmd),
    /// 고정 페이지 (소개, 연락처 등)
    #[command(subcommand)]
    Page(PageCmd),
    /// 헤더·푸터 메뉴
    #[command(subcommand)]
    Menu(MenuCmd),
    /// 사이트 이름·설명·시간대 등
    #[command(subcommand)]
    Site(SiteCmd),
    /// 디자인 토큰 (색, 글꼴, 글자 크기, 여백)
    #[command(subcommand)]
    Design(DesignCmd),
    /// 올린 서체 (글 안 꾸밈·사이트 본문 서체에 쓴다)
    #[command(subcommand)]
    Font(FontCmd),
    /// 사이트 검색 (공개 화면 `/search` 와 같은 결과). `maru search reindex` 는 색인 다시 만들기
    Search(SearchArgs),
    /// 관리 화면 로그인 링크를 만든다 (10분, 1회용)
    LoginLink(LoginLinkArgs),
    /// 설치 상태를 점검한다 (데이터, DB, 포트, 운영자, 테마)
    Doctor,
}

#[derive(Debug, Args)]
pub struct InitArgs {
    /// 사이트 프리셋
    #[arg(long, default_value = "blog")]
    pub preset: String,
    /// 사이트맵·RSS 의 절대 URL 기준 (기본: http://<bind>)
    #[arg(long)]
    pub site_url: Option<String>,
    /// 서버 주소 (기본: 127.0.0.1:8080)
    #[arg(long)]
    pub bind: Option<String>,
    /// 사이트 이름 (주면 설정에 반영한다)
    #[arg(long)]
    pub name: Option<String>,
    /// 운영자 이메일. 회원이 없을 때 만든다 (기본: owner@localhost)
    #[arg(long)]
    pub owner_email: Option<String>,
    /// 운영자 표시 이름
    #[arg(long, default_value = "운영자")]
    pub owner_name: String,
}

#[derive(Debug, Args)]
#[command(args_conflicts_with_subcommands = true)]
pub struct SearchArgs {
    #[command(subcommand)]
    pub cmd: Option<SearchCmd>,
    /// 검색어. 띄어 쓰면 모두, `OR` 는 어느 하나, `"구"` 는 붙은 그대로, `-낱말` 은 빼기
    pub q: Option<String>,
    /// 이 게시판 안에서만 (게시판 슬러그)
    #[arg(long)]
    pub board: Option<String>,
    /// 찾을 곳: all(제목·내용·태그), title, body, tag
    #[arg(long = "in", default_value = "all")]
    pub range: String,
    /// 순서: rank(정확도), new(최신)
    #[arg(long, default_value = "rank")]
    pub sort: String,
    #[arg(long, default_value_t = 1)]
    pub page: u32,
}

#[derive(Debug, Subcommand)]
pub enum SearchCmd {
    /// 검색 색인을 처음부터 다시 만든다 (여러 번 해도 같다). 보통은 서버가 스스로 맞춘다
    Reindex,
}

#[derive(Debug, Args)]
pub struct LoginLinkArgs {
    /// 회원 이메일. 회원이 한 명이면 생략한다
    #[arg(long)]
    pub email: Option<String>,
}

#[derive(Debug, Args)]
pub struct ServeArgs {
    /// 서버 주소. 생략하면 config.toml 의 bind. 포트 0 이면 빈 포트를 고른다
    #[arg(long)]
    pub bind: Option<String>,
}

#[derive(Debug, Subcommand)]
pub enum PostCmd {
    /// 초안을 만든다
    New(PostNewArgs),
    /// 글을 발행한다
    Publish { id: String },
    /// 글 목록
    List(PostListArgs),
    /// 글 하나
    Show { id: String },
    /// 글을 고친다
    Edit(PostEditArgs),
    /// 검토를 요청한다 (초안 → 검토)
    Submit { id: String },
    /// 공개 글을 숨긴다
    Hide { id: String },
    /// 글을 삭제 상태로 (되돌릴 수 있다)
    Delete { id: String },
    /// 삭제·숨김을 되돌린다
    Restore { id: String },
    /// 글 주소가 게시판에서 비어 있는지 확인한다 (바꾸는 것 없음)
    CheckSlug(PostCheckSlugArgs),
    /// 공개 글의 이전·다음 글과 관련 글
    Nearby {
        /// 게시판 슬러그
        board: String,
        /// 글 주소(슬러그)
        slug: String,
    },
    /// 조회수를 정한 값으로 바꾼다 (예: 시험 조회를 0 으로). 감사 기록에 남는다
    Views {
        id: String,
        #[arg(long)]
        set: i64,
    },
    /// 저장하지 않고 본문을 공개 화면과 같은 HTML 로 그린다
    Preview {
        /// 본문 파일 (UTF-8). `-` 이면 표준 입력
        #[arg(long)]
        from: PathBuf,
    },
}

#[derive(Debug, Args)]
pub struct PostCheckSlugArgs {
    /// 게시판 슬러그
    #[arg(long)]
    pub board: String,
    /// 확인할 주소. 생략하면 --title 에서 만들 주소를 알려 준다
    #[arg(long)]
    pub slug: Option<String>,
    #[arg(long)]
    pub title: Option<String>,
    /// 고치는 글의 ID (그 글 자신의 주소는 겹친 것으로 보지 않는다)
    #[arg(long)]
    pub post: Option<String>,
}

#[derive(Debug, Args)]
pub struct PostEditArgs {
    pub id: String,
    #[arg(long)]
    pub title: Option<String>,
    #[arg(long)]
    pub slug: Option<String>,
    #[arg(long, conflicts_with = "from")]
    pub body: Option<String>,
    /// 본문 파일 (UTF-8). `-` 이면 표준 입력
    #[arg(long)]
    pub from: Option<PathBuf>,
    /// 빈 문자열이면 요약을 지운다
    #[arg(long)]
    pub excerpt: Option<String>,
    /// 대표 이미지 주소 (`/media/...`). 빈 문자열이면 지운다 (본문 첫 사진을 쓴다)
    #[arg(long)]
    pub cover: Option<String>,
    /// 예약 발행 시각. RFC3339(`2026-11-05T09:00:00+09:00`) 또는 사이트 시간대 `2026-11-05T09:00`.
    /// 빈 문자열이면 예약을 취소한다. `maru serve` 가 돌고 있어야 그 시각에 발행된다
    #[arg(long)]
    pub schedule: Option<String>,
    /// 분류 슬러그 (`maru category list --board <게시판>`). 빈 문자열이면 분류를 뺀다
    #[arg(long)]
    pub category: Option<String>,
    /// 태그를 이것들로 바꾼다 (여러 번 쓸 수 있다)
    #[arg(long = "tag")]
    pub tags: Vec<String>,
    /// 태그를 모두 뺀다
    #[arg(long, conflicts_with = "tags")]
    pub clear_tags: bool,
    /// 게시판 맨 위 고정 (true/false). 발행 권한이 필요하다
    #[arg(long)]
    pub notice: Option<bool>,
    /// 비밀글 (true/false). 본문은 작성자와 관리자만 읽는다
    #[arg(long)]
    pub secret: Option<bool>,
}

#[derive(Debug, Args)]
pub struct PostNewArgs {
    /// 게시판 슬러그
    #[arg(long, default_value = "blog")]
    pub board: String,
    #[arg(long)]
    pub title: String,
    /// 생략하면 제목에서 만든다
    #[arg(long)]
    pub slug: Option<String>,
    /// Markdown 본문
    #[arg(long, conflicts_with = "from")]
    pub body: Option<String>,
    /// 본문 파일 (UTF-8). `-` 이면 표준 입력
    #[arg(long)]
    pub from: Option<PathBuf>,
    #[arg(long)]
    pub excerpt: Option<String>,
    /// 대표 이미지 주소 (`/media/...`). 생략하면 본문 첫 사진
    #[arg(long)]
    pub cover: Option<String>,
    /// 만든 뒤 바로 발행
    #[arg(long)]
    pub publish: bool,
    /// 만든 뒤 이 시각에 발행하도록 예약 (`post edit --schedule` 과 같은 형식)
    #[arg(long, conflicts_with = "publish")]
    pub schedule: Option<String>,
    /// 같은 슬러그의 글이 있으면 만들지 않고 그 글을 돌려준다 (`--slug` 필요)
    #[arg(long, requires = "slug")]
    pub if_not_exists: bool,
    /// 분류 슬러그 (`maru category list --board <게시판>`)
    #[arg(long)]
    pub category: Option<String>,
    /// 태그 (여러 번 쓸 수 있다: `--tag 제주 --tag 여행`)
    #[arg(long = "tag")]
    pub tags: Vec<String>,
    /// 게시판 맨 위에 고정한다 (발행 권한 필요)
    #[arg(long)]
    pub notice: bool,
    /// 비밀글: 목록에는 제목만, 본문은 작성자와 관리자만
    #[arg(long)]
    pub secret: bool,
}

#[derive(Debug, Subcommand)]
pub enum CategoryCmd {
    /// 게시판의 분류 목록
    List {
        #[arg(long)]
        board: String,
    },
    /// 분류를 만든다. 같은 주소가 있으면 그것을 돌려준다
    Add {
        #[arg(long)]
        board: String,
        name: String,
        /// 생략하면 이름에서 만든다
        #[arg(long)]
        slug: Option<String>,
    },
    /// 분류 이름을 바꾼다 (주소는 그대로, 바꾸려면 --new-slug)
    Rename {
        #[arg(long)]
        board: String,
        slug: String,
        name: String,
        #[arg(long)]
        new_slug: Option<String>,
    },
    /// 분류 순서를 바꾼다 (작을수록 앞)
    Move {
        #[arg(long)]
        board: String,
        slug: String,
        #[arg(long, allow_negative_numbers = true)]
        order: i64,
    },
    /// 분류를 지운다. 그 분류의 글은 분류가 비워진다
    Remove {
        #[arg(long)]
        board: String,
        slug: String,
    },
}

#[derive(Debug, Subcommand)]
pub enum TagCmd {
    /// 공개 글이 달린 태그와 글 수
    List,
}

#[derive(Debug, Args)]
pub struct PostListArgs {
    #[arg(long)]
    pub board: Option<String>,
    /// draft, review, published, hidden, deleted ...
    #[arg(long)]
    pub status: Option<String>,
    /// 제목·내용·태그에 이 말이 든 글만 (초안·비밀글 포함)
    #[arg(long)]
    pub q: Option<String>,
    #[arg(long, default_value_t = 1)]
    pub page: u32,
    #[arg(long, default_value_t = 20)]
    pub per_page: u32,
}

#[derive(Debug, Subcommand)]
pub enum MediaCmd {
    /// 이미지를 올린다 (jpg, png, gif, webp, 10MB 이하). 다시 인코딩해 위치 정보 등 메타데이터를 지운다
    Upload(MediaUploadArgs),
    /// 미디어 목록 (최근 것부터)
    List {
        #[arg(long, default_value_t = 1)]
        page: u32,
        #[arg(long, default_value_t = 20)]
        per_page: u32,
    },
    /// 미디어 하나
    Show { id: String },
    /// 대체 텍스트를 바꾼다. 빈 문자열이면 지운다
    Alt { id: String, text: String },
    /// 기록과 파일을 지운다. 본문에서 쓰던 곳은 깨진다
    Delete { id: String },
}

#[derive(Debug, Subcommand)]
pub enum TemplateCmd {
    /// 틀 목록 (기본 틀 + 저장한 틀)
    List,
    /// 틀을 저장한다. 같은 이름이 있으면 덮어쓴다
    Save {
        #[arg(long)]
        name: String,
        /// Markdown 본문
        #[arg(long, conflicts_with = "from")]
        body: Option<String>,
        /// 본문 파일 (UTF-8). `-` 이면 표준 입력
        #[arg(long)]
        from: Option<PathBuf>,
    },
    /// 저장한 틀을 지운다 (ID 또는 이름)
    Delete { id: String },
}

#[derive(Debug, Subcommand)]
pub enum FileCmd {
    /// 파일을 올린다 (pdf, hwp·hwpx, docx·xlsx·pptx, zip, txt·csv·md 등, 10MB 이하)
    Upload {
        /// 파일 경로. 파일 이름은 내려받을 때의 이름이 된다
        file: PathBuf,
    },
    /// 첨부 파일 목록 (최근 것부터)
    List {
        #[arg(long, default_value_t = 1)]
        page: u32,
        #[arg(long, default_value_t = 20)]
        per_page: u32,
    },
    /// 기록과 파일을 지운다. 본문의 링크는 깨진다
    Delete { id: String },
}

#[derive(Debug, Args)]
pub struct MediaUploadArgs {
    /// 이미지 파일 경로
    pub file: PathBuf,
    /// 대체 텍스트 (화면 낭독기가 읽는 설명)
    #[arg(long)]
    pub alt: Option<String>,
}

#[derive(Debug, Subcommand)]
pub enum BoardCmd {
    /// 게시판 목록
    List,
    /// 게시판을 만든다
    Create(BoardCreateArgs),
    /// 게시판 하나
    Show { slug: String },
    /// 게시판을 고친다
    Update(BoardUpdateArgs),
    /// 빈 게시판을 지운다
    Delete { slug: String },
}

#[derive(Debug, Args)]
pub struct BoardCreateArgs {
    pub slug: String,
    #[arg(long)]
    pub name: String,
    /// blog, notice, gallery, free, qna, faq
    #[arg(long, default_value = "blog")]
    pub kind: String,
    #[arg(long, default_value_t = 0)]
    pub sort_order: i64,
    /// 같은 슬러그가 있으면 그대로 둔다
    #[arg(long)]
    pub if_not_exists: bool,
}

#[derive(Debug, Args)]
pub struct BoardUpdateArgs {
    pub slug: String,
    #[arg(long)]
    pub name: Option<String>,
    #[arg(long)]
    pub kind: Option<String>,
    #[arg(long)]
    pub sort_order: Option<i64>,
    /// 읽기 최소 역할 (guest, member, editor, admin, owner)
    #[arg(long)]
    pub read: Option<String>,
    /// 쓰기 최소 역할
    #[arg(long)]
    pub write: Option<String>,
}

#[derive(Debug, Subcommand)]
pub enum TokenCmd {
    /// 토큰을 발급한다. 원문은 이때 한 번만 보인다
    Create(TokenCreateArgs),
    /// 토큰 목록 (원문은 보이지 않는다)
    List,
    /// 토큰을 폐기한다. 이미 폐기됐으면 그대로 성공한다
    Revoke {
        /// 토큰 id (`maru token list`)
        id: String,
    },
}

#[derive(Debug, Args)]
pub struct TokenCreateArgs {
    #[arg(long)]
    pub name: String,
    /// read, draft, publish, structure, design, admin (여러 번 지정). 생략하면 read, draft
    #[arg(long = "scope")]
    pub scopes: Vec<String>,
    /// 만료 시각 (RFC3339)
    #[arg(long)]
    pub expires_at: Option<String>,
}

#[derive(Debug, Subcommand)]
pub enum PageCmd {
    /// 페이지를 만든다 (초안)
    New(PageNewArgs),
    /// 페이지 목록
    List {
        /// draft, published, hidden, deleted
        #[arg(long)]
        status: Option<String>,
    },
    /// 페이지 하나
    Show { slug: String },
    /// 페이지를 고친다
    Edit(PageEditArgs),
    /// 페이지를 공개한다
    Publish { slug: String },
    /// 공개 페이지를 숨긴다
    Hide { slug: String },
    /// 페이지를 삭제 상태로 (되돌릴 수 있다)
    Delete { slug: String },
    /// 삭제를 되돌린다
    Restore { slug: String },
}

#[derive(Debug, Args)]
pub struct PageNewArgs {
    pub slug: String,
    #[arg(long)]
    pub title: String,
    #[arg(long, conflicts_with = "from")]
    pub body: Option<String>,
    /// 본문 파일 (UTF-8). `-` 이면 표준 입력
    #[arg(long)]
    pub from: Option<PathBuf>,
    /// default, wide
    #[arg(long, default_value = "default")]
    pub layout: String,
    #[arg(long, default_value_t = 0)]
    pub sort_order: i64,
    /// 만든 뒤 바로 공개
    #[arg(long)]
    pub publish: bool,
    /// 같은 슬러그가 있으면 만들지 않고 그 페이지를 돌려준다
    #[arg(long)]
    pub if_not_exists: bool,
}

#[derive(Debug, Args)]
pub struct PageEditArgs {
    pub slug: String,
    #[arg(long)]
    pub title: Option<String>,
    /// 새 슬러그
    #[arg(long)]
    pub new_slug: Option<String>,
    #[arg(long, conflicts_with = "from")]
    pub body: Option<String>,
    #[arg(long)]
    pub from: Option<PathBuf>,
    #[arg(long)]
    pub layout: Option<String>,
    #[arg(long)]
    pub sort_order: Option<i64>,
}

#[derive(Debug, Subcommand)]
pub enum MenuCmd {
    /// 메뉴 보기
    Show {
        /// header, footer
        #[arg(default_value = "header")]
        location: String,
    },
    /// 메뉴 전체를 바꾼다 (여러 번 실행해도 같은 결과)
    Set(MenuSetArgs),
}

#[derive(Debug, Args)]
pub struct MenuSetArgs {
    /// header, footer
    pub location: String,
    /// `라벨=링크`. 링크는 home, board:<슬러그>, page:<슬러그>, /경로, https://...
    /// 하위 메뉴는 `상위라벨>라벨=링크`. 순서대로 쌓인다
    #[arg(long = "item", conflicts_with_all = ["from", "clear"])]
    pub items: Vec<String>,
    /// 메뉴 트리 JSON 파일. `-` 이면 표준 입력
    #[arg(long, conflicts_with = "clear")]
    pub from: Option<PathBuf>,
    /// 메뉴를 비운다
    #[arg(long)]
    pub clear: bool,
}

#[derive(Debug, Subcommand)]
pub enum SiteCmd {
    /// 사이트 설정 보기
    Show,
    /// 사이트 설정을 고친다
    Set(SiteSetArgs),
}

#[derive(Debug, Args)]
pub struct SiteSetArgs {
    #[arg(long)]
    pub name: Option<String>,
    #[arg(long)]
    pub description: Option<String>,
    /// 문서 언어 (ko, en ...)
    #[arg(long)]
    pub lang: Option<String>,
    /// UTC 기준 분 (한국 540)
    #[arg(long, allow_hyphen_values = true)]
    pub tz_offset: Option<i32>,
    #[arg(long)]
    pub posts_per_page: Option<u32>,
    /// 네이버 서치어드바이저 소유 확인 meta 의 content 값
    #[arg(long)]
    pub naver_verification: Option<String>,
    /// 구글 서치 콘솔 소유 확인 meta 의 content 값
    #[arg(long)]
    pub google_verification: Option<String>,
    /// h2~h4 제목이 이 개수 이상인 글·페이지에 목차를 보인다 (0 = 끔, 최대 20)
    #[arg(long)]
    pub toc_min_headings: Option<u32>,
    /// 공개 화면에 조회수를 보일지 (true/false). 꺼도 세기는 계속한다
    #[arg(long)]
    pub show_views: Option<bool>,
    /// 공개 화면 머리글에 검색 칸을 보일지 (true/false)
    #[arg(long)]
    pub show_search: Option<bool>,
}

#[derive(Debug, Subcommand)]
pub enum DesignCmd {
    /// 현재 토큰
    Show {
        /// 토큰 대신 생성되는 CSS 를 출력
        #[arg(long)]
        css: bool,
    },
    /// 토큰을 고친다. 대비(WCAG AA)를 통과해야 저장된다
    Set(DesignSetArgs),
    /// 기본값으로 되돌린다
    Reset,
}

#[derive(Debug, Subcommand)]
pub enum FontCmd {
    /// 올린 서체 목록
    List,
    /// 서체 하나
    Show { slug: String },
    /// 서체 파일을 올린다 (woff2·woff·ttf·otf, 20MB 이하). 서체가 없으면 만든다.
    /// 웹에 올려 내보낼 권리가 있다는 확인으로 `--yes` 가 필요하다
    Add(FontAddArgs),
    /// 이름·대체 계열·라이선스를 고친다
    Update(FontUpdateArgs),
    /// 서체를 지운다. `--weight` 를 주면 그 굵기 파일만 뗀다. 글 안 꾸밈은 기본 서체로 보인다
    Remove {
        slug: String,
        #[arg(long)]
        weight: Option<i64>,
        #[arg(long)]
        italic: bool,
    },
}

#[derive(Debug, Args)]
pub struct FontAddArgs {
    /// 서체 파일 경로
    pub file: PathBuf,
    /// 메뉴에 보일 이름 (예: "회사 고딕")
    #[arg(long)]
    pub name: String,
    /// OFL-1.1, Apache-2.0, MIT, CC0-1.0, other(웹폰트 사용권을 직접 확인함)
    #[arg(long)]
    pub license: String,
    /// 글·토큰에 쓸 이름 (영소문자·숫자·`-`). 비우면 이름에서 만든다
    #[arg(long)]
    pub slug: Option<String>,
    /// 못 받았을 때 떨어질 계열: sans, serif, mono, cursive
    #[arg(long)]
    pub fallback: Option<String>,
    /// 굵기 100~900
    #[arg(long, default_value_t = 400)]
    pub weight: i64,
    #[arg(long)]
    pub italic: bool,
    /// 라이선스 전문 파일 (OFL.txt 등). 공개 사이트에서 함께 내보낸다
    #[arg(long)]
    pub license_file: Option<PathBuf>,
}

#[derive(Debug, Args)]
pub struct FontUpdateArgs {
    pub slug: String,
    #[arg(long)]
    pub name: Option<String>,
    #[arg(long)]
    pub fallback: Option<String>,
    #[arg(long)]
    pub license: Option<String>,
    /// 라이선스 전문 파일을 바꾼다
    #[arg(long)]
    pub license_file: Option<PathBuf>,
    /// 붙인 라이선스 전문을 뗀다
    #[arg(long, conflicts_with = "license_file")]
    pub no_license_file: bool,
}

#[derive(Debug, Args)]
pub struct DesignSetArgs {
    /// `경로=값` (예: color.primary=#2f5d50, type_scale.base=18, font.body=system)
    #[arg(long = "set")]
    pub sets: Vec<String>,
    /// 부분 토큰 JSON 파일. `-` 이면 표준 입력
    #[arg(long)]
    pub from: Option<PathBuf>,
}

pub struct Global {
    pub data: PathBuf,
    pub json: bool,
    /// `--yes`: 확인이 필요한 명령(서체 올리기의 배포 권리 확인)을 진행한다
    pub yes: bool,
    pub quiet: bool,
}

fn main() -> ExitCode {
    // 로그는 stderr 로만 보낸다. stdout 은 --json 출력 계약 전용이다.
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("warn")),
        )
        .init();

    // 사용법 오류도 JSON으로 내야 하므로 파싱 전에 --json 여부를 본다.
    let json = std::env::args_os().any(|a| a == "--json");
    let cli = match Cli::try_parse() {
        Ok(cli) => cli,
        Err(e) => return output::clap_error(e, json),
    };
    // 모든 명령은 비대화형이다. 확인이 필요한 명령은 묻지 않고 `--yes` 가 없으면 다음 명령을 안내한다.
    let g = Global {
        data: cli.data,
        json: cli.json,
        yes: cli.yes,
        quiet: cli.quiet,
    };
    let Some(command) = cli.command else {
        return output::error(
            &Error::new(Code::Usage)
                .with_hint("hint.usage")
                .next("maru --help"),
            g.json,
        );
    };
    let runtime = match tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
    {
        Ok(rt) => rt,
        Err(e) => return output::error(&Error::internal(e), g.json),
    };
    runtime.block_on(commands::run(&g, command))
}
