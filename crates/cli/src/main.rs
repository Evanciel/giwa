//! `maru` 바이너리.

mod commands;
mod doctor;
mod expose;
mod mcp;
mod modules;
mod output;
mod service;
mod update;

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
    /// MCP 서버와 에이전트 클라이언트 연결
    #[command(subcommand)]
    Mcp(McpCmd),
    /// 사이트를 터널로 인터넷에 공개
    Expose(ExposeArgs),
    /// 로그인하면 서버가 뜨도록 사용자 권한으로 등록
    #[command(subcommand)]
    Service(ServiceCmd),
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
    /// 옮긴 주소
    #[command(subcommand)]
    Redirect(RedirectCmd),
    /// 헤더·푸터 메뉴
    #[command(subcommand)]
    Menu(MenuCmd),
    /// 위젯 (최신 글·인기 글·글 상자 등을 영역에 놓기)
    #[command(subcommand)]
    Widget(WidgetCmd),
    /// 방문자 통계 (쿠키 없는 날짜별 집계)
    Stats(StatsArgs),
    /// 감사 로그 (누가 언제 무엇을 바꿨나)
    #[command(subcommand)]
    Audit(AuditCmd),
    /// 모듈 (켜기·끄기·제거·설정·실패 보관함)
    #[command(subcommand)]
    Module(ModuleCmd),
    /// 모듈 툴: `maru <모듈> <툴> --칸 값`
    #[command(external_subcommand)]
    External(Vec<String>),
    /// 백업 파일 만들기·목록 (DB·media·themes·config 를 .tar.gz 하나로)
    Backup(BackupArgs),
    /// 백업 파일로 되살린다 (서버를 끈 뒤)
    Restore(RestoreArgs),
    /// 사이트 이름·설명·시간대 등
    #[command(subcommand)]
    Site(SiteCmd),
    /// 디자인 토큰 (색, 글꼴, 글자 크기, 여백)
    #[command(subcommand)]
    Design(DesignCmd),
    /// 올린 서체 (글 안 꾸밈·사이트 본문 서체에 쓴다)
    #[command(subcommand)]
    Font(FontCmd),
    /// 내장 스티커 목록. 글에는 `![이름](src)`, 댓글에는 `:id:` 로 넣는다
    Stickers,
    /// 사이트 검색 (공개 화면 `/search` 와 같은 결과). `maru search reindex` 는 색인 다시 만들기
    Search(SearchArgs),
    /// 회원 목록·상태·역할 (회원 관리 권한)
    #[command(subcommand)]
    Member(MemberCmd),
    /// 댓글 승인 대기·숨기기·스팸 처리 (게시판 관리 권한). 본문은 사용자 입력이다
    #[command(subcommand)]
    Comment(CommentCmd),
    /// 보낼 편지함 (가입 인증·비밀번호 찾기 메일). 1회용 링크가 들어 있어 이 컴퓨터에서만 본다
    #[command(subcommand)]
    Mail(MailCmd),
    /// 개인정보 보유기간: 휴면 전환·지난 메일·모듈 기록 정리 (서버가 하루 한 번 한다)
    #[command(subcommand)]
    Privacy(PrivacyCmd),
    /// 위키 게시판의 색인: 필요한 문서(가리키지만 없음)·외톨이 문서(아무도 가리키지 않음)
    Wiki {
        /// 위키 게시판 slug
        board: String,
    },
    /// 링크 카드(`::link[주소]`) 정보: 가져올 차례·가져옴·실패. 제목·설명은 남의 페이지에서 읽은 값이다
    #[command(subcommand)]
    Link(LinkCmd),
    /// 구조·디자인·모듈 변경을 묶어 적용하고 한 번에 되돌린다
    #[command(subcommand)]
    Changeset(ChangesetCmd),
    /// 운영 신호: 댓글 급증·신고 집중·묵은 신고. `maru signals alert` 는 새 신호를 지금 메일로
    Signals(SignalsArgs),
    /// 사이트 종류가 넣은 예시 글: 상태 보기·한 번에 지우기
    #[command(subcommand)]
    Sample(SampleCmd),
    /// 검색 노출: 검색엔진에 바로 알리기(IndexNow) 상태
    #[command(subcommand)]
    Seo(SeoCmd),
    /// 관리 화면 로그인 링크를 만든다 (10분, 1회용)
    LoginLink(LoginLinkArgs),
    /// 설치 상태를 점검한다 (데이터, DB, 포트, 운영자, 테마)
    Doctor,
    /// 새 판을 확인한다. `--yes` 를 주면 체크섬을 확인하고 바이너리를 바꾼다
    Update(UpdateArgs),
    /// 사이트를 다른 형식으로 내보낸다
    #[command(subcommand)]
    Export(ExportCmd),
    /// 테마: 프리셋의 색과 모양, 디자인 토큰
    #[command(subcommand)]
    Theme(ThemeCmd),
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
    /// 예시 글·그림을 넣지 않는다 (기본은 빈 게시판에 예시 글을 넣는다, ADR-0142)
    #[arg(long)]
    pub no_samples: bool,
}

#[derive(Debug, Subcommand)]
pub enum SeoCmd {
    /// 키 파일 주소, 마지막으로 보낸 시각·응답·개수, 공개 사이트인지
    Status,
}

#[derive(Debug, Subcommand)]
pub enum SampleCmd {
    /// 남아 있는 예시 글 수와 그중 고치지 않은 글 수
    Status,
    /// 고치지 않은 예시 글과 그림을 아주 지운다 (`--yes` 필요). 고친 글은 남는다
    Clear,
}

#[derive(Debug, Subcommand)]
pub enum MemberCmd {
    /// 가입 순 목록
    List {
        #[arg(long, default_value_t = 1)]
        page: u32,
    },
    /// 회원 하나 (ID 또는 이메일)
    Show { who: String },
    /// 로그인을 막고 세션을 모두 끊는다
    Disable { who: String },
    /// 다시 로그인할 수 있게 한다
    Enable { who: String },
    /// 역할을 하나로 정한다: member, editor, admin
    Role { who: String, role: String },
    /// 2단계 인증을 끈다 (인증 앱과 복구 코드를 모두 잃었을 때). 꺼져 있으면 아무 일 없음
    TotpReset { who: String },
    /// 공개 프로필 (닉네임): 가입일·자기소개·최근 공개 글과 댓글
    Profile { name: String },
}

#[derive(Debug, Subcommand)]
pub enum CommentCmd {
    /// 새것부터. 기본은 승인 대기만
    List {
        /// pending, published, hidden, spam, deleted, all
        #[arg(long, default_value = "pending")]
        status: String,
        /// 이 글의 댓글만 (글 ID)
        #[arg(long)]
        post: Option<String>,
        #[arg(long, default_value_t = 1)]
        page: u32,
    },
    /// 보이게 한다
    Approve { id: String },
    /// 숨긴다 (되돌릴 수 있다)
    Hide { id: String },
    /// 스팸으로 둔다
    Spam { id: String },
    /// 지운다 (본문을 비우고 자리만 남긴다)
    Delete { id: String },
}

#[derive(Debug, Args)]
pub struct SignalsArgs {
    #[command(subcommand)]
    pub cmd: Option<SignalsCmd>,
}

#[derive(Debug, Subcommand)]
pub enum SignalsCmd {
    /// 새 신호를 관리자·소유자에게 메일로 쌓는다 (서버가 10분마다 한다)
    Alert,
}

#[derive(Debug, Subcommand)]
pub enum PrivacyCmd {
    /// 지금 정리한다 (여러 번 해도 안전하다)
    Run,
}

#[derive(Debug, Subcommand)]
pub enum ChangesetCmd {
    /// 새 변경 묶음을 만든다. 적용하기 전에는 사이트가 그대로다
    New {
        #[arg(long)]
        title: String,
        /// 연산 JSON 배열. op: board.create·update·delete, page.create·update·publish·hide·delete·restore,
        /// menu.set, tokens.set·reset, settings.set, widget.place·configure·remove, module.enable·disable
        #[arg(long, conflicts_with = "ops_file")]
        ops: Option<String>,
        /// 연산 JSON 배열이 든 파일
        #[arg(long)]
        ops_file: Option<PathBuf>,
    },
    /// 적용 전 변경 묶음에 연산을 더한다
    Add {
        id: String,
        /// 연산 JSON 하나 또는 배열
        #[arg(long)]
        op: String,
    },
    /// 새것부터
    List {
        /// preview, applying, applied, failed, discarded, reverted
        #[arg(long)]
        status: Option<String>,
        #[arg(long, default_value_t = 50)]
        limit: i64,
    },
    Show {
        id: String,
    },
    /// 사본에서 먼저 돌려 본 뒤 사이트에 반영한다
    Apply {
        id: String,
    },
    /// 화면으로 미리 보는 1회용 주소 (30분)
    PreviewUrl {
        id: String,
    },
    /// 적용하지 않고 버린다
    Discard {
        id: String,
    },
    /// 적용한 것을 한 번에 되돌린다
    Revert {
        id: String,
    },
}

#[derive(Debug, Subcommand)]
pub enum LinkCmd {
    /// 새것부터
    List {
        /// pending, ok, failed
        #[arg(long)]
        status: Option<String>,
        #[arg(long, default_value_t = 50)]
        limit: i64,
    },
    /// 다음에 다시 가져오게 한다 (페이지 제목이 바뀐 때 등)
    Refresh { url: String },
    /// 가져올 차례인 주소를 지금 가져온다 (서버가 10초마다 하는 일)
    Fetch,
}

#[derive(Debug, Subcommand)]
pub enum MailCmd {
    /// 새것부터
    List {
        /// queued, sent, failed
        #[arg(long)]
        status: Option<String>,
        #[arg(long, default_value_t = 20)]
        limit: i64,
    },
    /// 편지 한 통 (본문 포함)
    Show { id: String },
    /// 쌓인 편지를 지금 배달한다 (서버가 5초마다 하는 일)
    Send,
    /// 편지 보내는 방식을 정한다 (`config.toml` 의 [mail]). serve 를 다시 시작하면 적용된다
    Setup(MailSetupArgs),
    /// 시험 편지를 쌓고 지금 보낸다. 못 보내면 실패로 끝난다
    Test {
        /// 받을 주소
        #[arg(long)]
        to: String,
    },
}

#[derive(Debug, Args)]
pub struct MailSetupArgs {
    /// file(데이터 디렉터리에 .eml 로만 남김, 기본) 또는 smtp
    #[arg(long)]
    pub transport: String,
    /// SMTP 서버 이름
    #[arg(long)]
    pub host: Option<String>,
    /// 없으면 starttls 587 · tls 465 · none 25
    #[arg(long)]
    pub port: Option<u16>,
    /// starttls · tls · none(이 컴퓨터 안의 중계 서버만)
    #[arg(long, default_value = "starttls")]
    pub security: String,
    /// SMTP 로그인 이름
    #[arg(long)]
    pub user: Option<String>,
    /// 표준 입력 첫 줄을 SMTP 비밀번호로 `secrets/smtp-password` 에 둔다(백업에 들어가지 않음).
    /// 환경 변수 MARU_SMTP_PASSWORD 가 있으면 그것이 먼저다
    #[arg(long)]
    pub password_stdin: bool,
    /// 보낸 사람 주소. 없으면 noreply@<site_url 호스트>
    #[arg(long)]
    pub from: Option<String>,
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
    /// 글의 리비전 목록 (새것부터)
    Revisions { id: String },
    /// 리비전과 지금 글을 줄 단위로 비교한다 (바꾸는 것 없음)
    Diff {
        id: String,
        /// 리비전 id (`maru post revisions <글>`)
        revision: String,
    },
    /// 그 리비전의 제목·본문으로 되돌린다 (새 리비전이 하나 더 쌓인다)
    Revert {
        id: String,
        /// 리비전 id (`maru post revisions <글>`)
        revision: String,
    },
    /// 글을 다른 게시판으로 옮긴다. 댓글·조회수는 따라간다
    Move {
        id: String,
        #[arg(long)]
        board: String,
        /// 새 게시판의 분류 슬러그. 생략하면 분류를 비운다
        #[arg(long)]
        category: Option<String>,
    },
    /// 공개 글에 답변글 초안을 단다. 게시판·분류는 위 글을 따른다
    Reply(PostReplyArgs),
    /// 글을 잠근다. 관리자 말고는 댓글·답변·고치기·지우기를 못 한다. `--off` 로 푼다
    Lock {
        id: String,
        #[arg(long)]
        off: bool,
    },
    /// 댓글을 답변으로 채택한다 (qna·forum 게시판). `--clear` 로 채택을 뺀다
    Accept {
        id: String,
        /// 채택할 댓글 id (`maru comment list --post <글>`)
        #[arg(long, conflicts_with = "clear", required_unless_present = "clear")]
        comment: Option<String>,
        #[arg(long)]
        clear: bool,
    },
    /// 글을 다른 게시판에 초안으로 복사한다
    Copy {
        id: String,
        #[arg(long)]
        board: String,
    },
    /// 여러 글에 같은 동작 (최대 100개). 글마다 따로 처리하고 결과를 글마다 알려 준다
    Bulk(PostBulkArgs),
    /// 저장하지 않고 본문을 공개 화면과 같은 HTML 로 그린다
    Preview {
        /// 본문 파일 (UTF-8). `-` 이면 표준 입력
        #[arg(long)]
        from: PathBuf,
    },
}

#[derive(Debug, Args)]
pub struct PostReplyArgs {
    /// 위 글 id
    pub id: String,
    #[arg(long)]
    pub title: String,
    /// Markdown 본문
    #[arg(long, conflicts_with = "from")]
    pub body: Option<String>,
    /// 본문 파일 (UTF-8). `-` 이면 표준 입력
    #[arg(long)]
    pub from: Option<PathBuf>,
    /// 생략하면 제목에서 만든다
    #[arg(long)]
    pub slug: Option<String>,
    #[arg(long = "tag")]
    pub tags: Vec<String>,
    /// 비밀 답변 (위 글이 비밀글이면 늘 비밀글)
    #[arg(long)]
    pub secret: bool,
    /// 만든 뒤 바로 발행
    #[arg(long)]
    pub publish: bool,
}

#[derive(Debug, Args)]
pub struct PostBulkArgs {
    /// publish, hide, delete, restore, move
    pub action: String,
    /// 글 id (여러 번)
    #[arg(long = "id", required = true)]
    pub ids: Vec<String>,
    /// move 일 때 옮겨 갈 게시판
    #[arg(long)]
    pub board: Option<String>,
    /// move 일 때 새 게시판의 분류
    #[arg(long)]
    pub category: Option<String>,
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
    /// 여분 칸 값 `키=값` (여러 번. `maru board show <게시판>` 의 칸 정의를 따른다. 값을 비우면 지운다)
    #[arg(long = "field", value_name = "KEY=VALUE")]
    pub fields: Vec<String>,
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
    /// 여분 칸 값 `키=값` (여러 번. `maru board show <게시판>` 의 칸 정의를 따른다. 값을 비우면 지운다)
    #[arg(long = "field", value_name = "KEY=VALUE")]
    pub fields: Vec<String>,
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
    /// 이미지(jpg, png, gif, webp, 10MB 이하. 다시 인코딩해 위치 정보 등 메타데이터를 지운다)나
    /// 동영상(mp4, webm, 200MB 이하. 다시 인코딩하지 않는다)을 올린다
    Upload(MediaUploadArgs),
    /// 미디어 목록 (최근 것부터)
    List {
        #[arg(long, default_value_t = 1)]
        page: u32,
        #[arg(long, default_value_t = 20)]
        per_page: u32,
        /// image 또는 video 만 본다
        #[arg(long, value_parser = ["image", "video"])]
        kind: Option<String>,
    },
    /// 미디어 하나
    Show { id: String },
    /// 대체 텍스트를 바꾼다. 빈 문자열이면 지운다
    Alt { id: String, text: String },
    /// 돌리거나 잘라 새 미디어로 둔다. 원본은 그대로 남는다. 먼저 돌리고, 돌린 사진 기준으로 자른다
    Transform {
        id: String,
        /// 시계 방향 각도: 0·90·180·270
        #[arg(long, default_value_t = 0)]
        rotate: u16,
        /// 남길 영역 `x,y,가로,세로` (픽셀)
        #[arg(long)]
        crop: Option<String>,
    },
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
pub enum ModuleCmd {
    /// 이 바이너리에 든 모듈과 상태
    List,
    /// 모듈 하나의 설정·툴·위젯
    Show { id: String },
    /// 표를 만들고 켠다. 꺼져 있던 동안의 일은 소급하지 않는다 (여러 번 실행해도 안전)
    Enable { id: String },
    /// 끈다. 데이터는 남는다
    Disable { id: String },
    /// 모듈의 데이터를 지운다. `--purge --yes` 를 함께 적어야 한다
    Remove {
        id: String,
        #[arg(long)]
        purge: bool,
    },
    /// 설정을 바꾼다: `--field post=20`
    Set {
        id: String,
        #[arg(long)]
        field: Vec<String>,
    },
    /// 처리하지 못한 이벤트
    Failures { id: Option<String> },
    /// 밀린 이벤트를 지금 넘긴다 (서버가 켜져 있으면 2초마다 저절로 한다)
    Deliver,
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
    /// blog, notice, gallery, free, qna, faq, forum, wiki
    #[arg(long, default_value = "blog")]
    pub kind: String,
    #[arg(long, default_value_t = 0)]
    pub sort_order: i64,
    /// 목록 형태: list(번호 목록), webzine(그림 + 요약), card(카드 격자), gallery(그림 격자). 생략하면 종류의 기본값
    #[arg(long)]
    pub layout: Option<String>,
    /// 목록 순서: latest(새 글부터), activity(마지막 댓글·답변부터). 생략하면 종류의 기본값(forum 만 activity)
    #[arg(long)]
    pub order: Option<String>,
    /// 같은 슬러그가 있으면 그대로 둔다
    #[arg(long)]
    pub if_not_exists: bool,
    /// 여분 칸 정의 JSON 배열 파일 (`-` 이면 표준 입력). 예: `[{"key":"price","label":"가격","type":"number","required":true}]`
    #[arg(long)]
    pub fields: Option<PathBuf>,
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
    /// 목록 형태: list(번호 목록), webzine(그림 + 요약), card(카드 격자), gallery(그림 격자). 빈 값("")이면 종류의 기본값으로
    #[arg(long)]
    pub layout: Option<String>,
    /// 목록 순서: latest, activity. 빈 값("")이면 종류의 기본값으로
    #[arg(long)]
    pub order: Option<String>,
    /// 목록 위 안내 문구(평문 300자, 줄바꿈 가능). 빈 값("")이면 지운다
    #[arg(long)]
    pub description: Option<String>,
    /// 읽기 최소 역할 (guest, member, editor, admin, owner)
    #[arg(long)]
    pub read: Option<String>,
    /// 쓰기 최소 역할
    #[arg(long)]
    pub write: Option<String>,
    /// 댓글 최소 역할 (guest 면 비회원도, none 이면 댓글을 받지 않음)
    #[arg(long)]
    pub comment: Option<String>,
    /// 댓글 승인: off(바로 보임), guest(비회원 것만 승인 뒤), all(모두 승인 뒤)
    #[arg(long)]
    pub comment_approval: Option<String>,
    /// 공개 화면 글쓰기 승인: off, guest(비회원 글만 승인 뒤), all
    #[arg(long)]
    pub write_approval: Option<String>,
    /// 답변글 최소 역할 (none 이면 받지 않음. 문의 게시판 기본은 editor)
    #[arg(long)]
    pub reply: Option<String>,
    /// 공개 글쓰기에 파일을, 댓글에 그림을 붙일 수 있는 최소 역할 (member 이상 또는 none)
    #[arg(long)]
    pub upload: Option<String>,
    /// 여분 칸 정의를 통째로 바꾼다. JSON 배열 파일 (`-` 이면 표준 입력). 예: `[{"key":"price","label":"가격","type":"number","required":true}]`
    #[arg(long)]
    pub fields: Option<PathBuf>,
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
#[command(args_conflicts_with_subcommands = true)]
pub struct ExposeArgs {
    #[command(subcommand)]
    pub cmd: Option<ExposeCmd>,
    /// Cloudflare Tunnel 을 쓴다 (cloudflared 필요). 혼자 쓰면 고를 수 있는 길을 보여 준다
    #[arg(long)]
    pub cloudflare: bool,
    /// 계정 없이 임시 주소로 공개한다. 끌 때까지 앞에서 돈다
    #[arg(long, requires = "cloudflare", conflicts_with = "hostname")]
    pub quick: bool,
    /// 내 도메인. site_url 을 맞추고 cloudflared 명령 순서를 돌려준다
    #[arg(long, requires = "cloudflare")]
    pub hostname: Option<String>,
    /// --hostname 에 쓸 터널 이름
    #[arg(long, default_value = "maru")]
    pub tunnel: String,
    /// 터널을 보낼 서버 주소 (생략하면 설정의 bind)
    #[arg(long)]
    pub to: Option<String>,
}

#[derive(Debug, Subcommand)]
pub enum ServiceCmd {
    /// 등록하고 바로 띄운다. 다시 실행하면 정의만 새로 쓴다
    Install(ServiceInstallArgs),
    /// 등록을 지우고 끈다. 없어도 성공이다
    Uninstall,
    /// 등록 여부와 서버 상태
    Status,
}

#[derive(Debug, Args)]
pub struct ServiceInstallArgs {
    /// 서버 주소. 생략하면 config.toml 의 bind
    #[arg(long)]
    pub bind: Option<String>,
    /// 아무것도 바꾸지 않고 등록할 내용만 보여 준다
    #[arg(long)]
    pub print: bool,
}

#[derive(Debug, Subcommand)]
pub enum ExposeCmd {
    /// cloudflared·서버·터널 상태
    Status,
    /// 임시 터널을 끈다. 없어도 성공이다
    Stop,
}

#[derive(Debug, Subcommand)]
pub enum McpCmd {
    /// 표준 입출력으로 MCP 서버를 돌린다. 토큰이 있어야 한다
    Serve(McpServeArgs),
    /// 클라이언트 몫의 토큰을 만들고 클라이언트 설정에 연결한다. 여러 번 해도 안전하다
    Install(McpInstallArgs),
}

#[derive(Debug, Args)]
pub struct McpServeArgs {
    /// 토큰 원문이 든 파일 (`mcp install` 이 만든다). 없으면 환경 변수 MARU_TOKEN
    #[arg(long)]
    pub token_file: Option<PathBuf>,
}

#[derive(Debug, Args)]
pub struct McpInstallArgs {
    /// claude-code, claude-desktop, cursor
    #[arg(long)]
    pub client: String,
    /// read, draft, publish 등 (여러 번 지정). 생략하면 새 토큰은 read, draft, 있던 토큰은 그대로
    #[arg(long = "scope")]
    pub scopes: Vec<String>,
    /// 클라이언트 설정 파일 (생략하면 클라이언트의 기본 자리)
    #[arg(long)]
    pub config: Option<PathBuf>,
    /// 설정 파일을 고치지 않고 넣을 내용만 보여 준다
    #[arg(long)]
    pub print: bool,
}

#[derive(Debug, Args)]
pub struct TokenCreateArgs {
    #[arg(long)]
    pub name: String,
    /// read, draft, publish, structure, design, admin (여러 번 지정). 생략하면 read, draft
    #[arg(long = "scope")]
    pub scopes: Vec<String>,
    /// 사람 승인 없이 변경 묶음을 적용하게 한다. 혼자 운영하며 위험을 아는 경우에만
    #[arg(long)]
    pub auto_apply: bool,
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
    /// default, wide, blocks(섹션 블록), docs(문서)
    #[arg(long, default_value = "default")]
    pub layout: String,
    #[arg(long, default_value_t = 0)]
    pub sort_order: i64,
    /// 위 페이지의 주소
    #[arg(long)]
    pub parent: Option<String>,
    /// 만든 뒤 바로 공개
    #[arg(long)]
    pub publish: bool,
    /// 같은 슬러그가 있으면 만들지 않고 그 페이지를 돌려준다
    #[arg(long)]
    pub if_not_exists: bool,
    /// 새 페이지 틀: blank, about, service, contact, terms(이용약관), refund(환불 규정). 블록 틀은 섹션 블록 모양으로, 약관 틀은 본문 글로 만든다
    /// (새로 만들 때만, ADR-0149)
    #[arg(long)]
    pub starter: Option<String>,
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
    /// 위 페이지의 주소. 빈 값이면 맨 위로
    #[arg(long)]
    pub parent: Option<String>,
    /// 검색 결과·공유 미리보기 설명. 빈 값이면 본문 앞부분 (ADR-0160)
    #[arg(long)]
    pub description: Option<String>,
    /// 공유 미리보기 그림 (`/media/…` 또는 https). 빈 값이면 사이트 공유 그림
    #[arg(long)]
    pub share_image: Option<String>,
    /// 검색 엔진에 내보내지 않기 (true|false)
    #[arg(long)]
    pub noindex: Option<bool>,
}

#[derive(Debug, Subcommand)]
pub enum RedirectCmd {
    /// 옮긴 주소 목록
    List,
    /// 옛 주소를 새 주소로 보낸다
    Add { from: String, to: String },
    /// 옮긴 주소를 지운다
    Remove { from: String },
}

#[derive(Debug, Subcommand)]
pub enum MenuCmd {
    /// 메뉴 보기
    Show {
        /// header, footer, quick(바로가기 단추)
        #[arg(default_value = "header")]
        location: String,
    },
    /// 메뉴 전체를 바꾼다 (여러 번 실행해도 같은 결과)
    Set(MenuSetArgs),
}

#[derive(Debug, Args)]
pub struct MenuSetArgs {
    /// header, footer, quick(바로가기 단추)
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

#[derive(Debug, Args)]
pub struct BackupArgs {
    #[command(subcommand)]
    pub cmd: Option<BackupCmd>,
    /// 세션·토큰·비밀번호 해시·로그인 링크·보낼 편지를 뺀다 (남에게 줄 사본용)
    #[arg(long)]
    pub no_secrets: bool,
}

#[derive(Debug, Subcommand)]
pub enum BackupCmd {
    /// 백업 파일 목록 (최신 순)
    List,
    /// 백업 파일을 지운다
    Delete { name: String },
}

#[derive(Debug, Args)]
pub struct RestoreArgs {
    /// 백업 파일 (.tar.gz). 지금 데이터는 backups/before-restore-… 로 옮긴다 (`--yes` 필요)
    pub file: PathBuf,
}

#[derive(Debug, Subcommand)]
pub enum AuditCmd {
    /// 최신 순 감사 기록
    List(AuditListArgs),
}

#[derive(Debug, Args)]
pub struct AuditListArgs {
    /// 동작 이름 또는 앞부분 (post → post.create, post.publish …)
    #[arg(long)]
    pub action: Option<String>,
    /// anonymous, human, agent, local, system
    #[arg(long)]
    pub actor_type: Option<String>,
    /// 회원 ID 또는 토큰 ID
    #[arg(long)]
    pub actor_id: Option<String>,
    /// post, board, page, member, token, site …
    #[arg(long)]
    pub target_type: Option<String>,
    #[arg(long)]
    pub target_id: Option<String>,
    /// 이 시각부터 (RFC3339)
    #[arg(long)]
    pub since: Option<String>,
    /// 이 시각 전까지 (RFC3339)
    #[arg(long)]
    pub until: Option<String>,
    #[arg(long, default_value_t = 1)]
    pub page: u32,
    /// 1~100
    #[arg(long, default_value_t = 50)]
    pub per_page: u32,
}

#[derive(Debug, Args)]
pub struct StatsArgs {
    /// 오늘까지 며칠 (1~366, 기본 30)
    #[arg(long)]
    pub days: Option<u32>,
    /// 이 파일에 CSV 로도 쓴다 (UTF-8 BOM, 엑셀용)
    #[arg(long)]
    pub csv: Option<PathBuf>,
}

#[derive(Debug, Subcommand)]
pub enum WidgetCmd {
    /// 놓을 수 있는 위젯 종류와 설정 모양 (`--json` 이면 JSON Schema)
    Types,
    /// 놓인 위젯 목록
    List,
    /// 위젯을 놓는다. 같은 설정의 위젯이 이미 있으면 그것을 돌려준다
    Place(WidgetPlaceArgs),
    /// 위젯을 고친다 (준 칸만)
    Configure(WidgetConfigureArgs),
    /// 위젯을 뺀다
    Remove { id: String },
    /// 내 블록 보관함: 화면 편집기에서 다른 화면에 다시 넣는 블록 (ADR-0159)
    #[command(subcommand)]
    Saved(SavedBlockCmd),
}

#[derive(Debug, Subcommand)]
pub enum SavedBlockCmd {
    /// 보관함 목록 (새것부터)
    List,
    /// 블록을 보관함에 저장한다. 같은 내용이 이미 있으면 그것을 돌려준다
    Save(SavedBlockArgs),
    /// 보관함에서 지운다. 화면에 넣은 블록은 그대로다
    Delete { id: String },
}

#[derive(Debug, Args)]
pub struct SavedBlockArgs {
    /// 보관함에 보일 이름 (40자)
    #[arg(long)]
    pub name: String,
    /// 블록 종류 (`maru widget types`)
    pub kind: String,
    /// 놓을 자리
    #[arg(long, default_value = "main")]
    pub region: String,
    #[arg(long, default_value = "")]
    pub title: String,
    /// 설정 JSON
    #[arg(long, conflicts_with = "config_file")]
    pub config: Option<String>,
    /// 설정 JSON 파일. `-` 이면 표준 입력
    #[arg(long)]
    pub config_file: Option<PathBuf>,
}

#[derive(Debug, Args)]
pub struct WidgetPlaceArgs {
    /// latest_posts, popular_posts, post_tabs, markdown_block, menu, notice_bar, search, account_box,
    /// image_banner, gallery_grid, popup
    pub kind: String,
    /// header, hero, main, sidebar, footer, masthead(머리글 이름 옆), popup,
    /// sponsor(바닥글 위 후원 띠), rail(넓은 화면 양옆), sticky(휴대폰 아래 고정)
    #[arg(long)]
    pub region: String,
    /// all, home, boards, board:<주소>, page:<주소>
    #[arg(long, default_value = "all")]
    pub scope: String,
    #[arg(long, default_value = "")]
    pub title: String,
    /// 설정 JSON (예: '{"board":"notice","limit":5}')
    #[arg(long, conflicts_with = "config_file")]
    pub config: Option<String>,
    /// 설정 JSON 파일. `-` 이면 표준 입력
    #[arg(long)]
    pub config_file: Option<PathBuf>,
    /// 영역 안 순서. 비우면 맨 뒤
    #[arg(long, allow_hyphen_values = true)]
    pub position: Option<i64>,
    /// 꺼 둔 채로 놓는다
    #[arg(long)]
    pub disabled: bool,
    /// 이 기기에서만 숨긴다. 쉼표로 여럿: desktop, tablet, mobile
    #[arg(long, value_delimiter = ',')]
    pub hide_on: Vec<String>,
}

#[derive(Debug, Args)]
pub struct WidgetConfigureArgs {
    pub id: String,
    #[arg(long)]
    pub region: Option<String>,
    #[arg(long)]
    pub scope: Option<String>,
    #[arg(long)]
    pub title: Option<String>,
    /// 설정 JSON 전체를 바꾼다
    #[arg(long, conflicts_with = "config_file")]
    pub config: Option<String>,
    #[arg(long)]
    pub config_file: Option<PathBuf>,
    #[arg(long, allow_hyphen_values = true)]
    pub position: Option<i64>,
    #[arg(long, conflicts_with = "disable")]
    pub enable: bool,
    #[arg(long)]
    pub disable: bool,
    /// 숨길 기기 목록을 바꾼다(desktop, tablet, mobile). `--hide-on ""` 이면 모든 기기에 보인다
    #[arg(long, value_delimiter = ',')]
    pub hide_on: Option<Vec<String>>,
}

#[derive(Debug, Subcommand)]
// 명령 줄에서 한 번 만드는 값이라 크기 차이는 상관없다.
#[allow(clippy::large_enum_variant)]
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
    /// 첫 화면으로 쓸 페이지 주소. 빈 값이면 최신 글 목록
    #[arg(long)]
    pub home_page: Option<String>,
    /// 글 화면 아래에 그 게시판 목록을 다시 보일지 (true/false). 켜면 이전·다음 글 링크는 숨긴다
    #[arg(long)]
    pub list_under_post: Option<bool>,
    /// 표 목록 왼쪽 칸에 글 번호 대신 추천 수를 보일지 (true/false). 반응 모듈이 꺼져 있으면 번호
    #[arg(long)]
    pub list_votes: Option<bool>,
    /// 댓글·공개 글쓰기의 스티커를 쓸지 (true/false)
    #[arg(long)]
    pub stickers: Option<bool>,
    /// 방문자 통계를 며칠 남길지 (30~3650)
    #[arg(long)]
    pub stats_retention_days: Option<u32>,
    /// 이 날수 동안 로그인하지 않은 회원을 휴면으로 (0 = 끔, 180~3650)
    #[arg(long)]
    pub dormant_days: Option<u32>,
    /// 보냈거나 실패한 메일을 남길 날수 (7~3650)
    #[arg(long)]
    pub mail_retention_days: Option<u32>,
    /// 한 글에 지난 1시간 댓글이 이만큼이면 운영 신호 (0 = 끔, 3~1000)
    #[arg(long)]
    pub signal_comment_burst: Option<u32>,
    /// 새 운영 신호를 관리자·소유자에게 메일로 (true/false)
    #[arg(long)]
    pub signal_mail: Option<bool>,
    /// 링크 카드(`::link[주소]`)의 제목·그림을 서버가 가져온다 (true/false)
    #[arg(long)]
    pub link_cards: Option<bool>,
    /// 공유 미리보기 그림: `/media/…` 또는 `https://…`. 빈 값이면 사이트 색으로 그린 그림
    #[arg(long)]
    pub share_image: Option<String>,
    /// 공개한 사이트의 새 글 주소를 IndexNow 로 검색엔진에 바로 알린다 (true/false)
    #[arg(long)]
    pub indexnow: Option<bool>,
    /// 휴대폰 홈 화면에 추가(앱 정보·오프라인 안내 서비스 워커)를 둘지 (true/false)
    #[arg(long)]
    pub pwa: Option<bool>,
    /// 사업자 정보: 상호. 빈 값이면 지운다 (ADR-0167)
    #[arg(long)]
    pub business_name: Option<String>,
    /// 사업자 정보: 대표자 성명. 빈 값이면 지운다 (ADR-0167)
    #[arg(long)]
    pub business_owner: Option<String>,
    /// 사업자 정보: 영업소 주소. 빈 값이면 지운다 (ADR-0167)
    #[arg(long)]
    pub business_address: Option<String>,
    /// 사업자 정보: 전화. 빈 값이면 지운다 (ADR-0167)
    #[arg(long)]
    pub business_phone: Option<String>,
    /// 사업자 정보: 전자우편. 빈 값이면 지운다 (ADR-0167)
    #[arg(long)]
    pub business_email: Option<String>,
    /// 사업자 정보: 사업자등록번호 숫자 10자리. 빈 값이면 지운다 (ADR-0167)
    #[arg(long)]
    pub business_reg_no: Option<String>,
    /// 사업자 정보: 통신판매업 신고번호. 면제면 비운다. 빈 값이면 지운다 (ADR-0167)
    #[arg(long)]
    pub business_sales_no: Option<String>,
    /// 사업자 정보: 호스팅 제공자 상호. 직접 운영하면 자기 상호. 빈 값이면 지운다 (ADR-0167)
    #[arg(long)]
    pub business_hosting: Option<String>,
    /// 회원 가입: off(받지 않음), open(누구나)
    #[arg(long)]
    pub signup: Option<String>,
    /// 가입한 사람이 메일 링크를 눌러야 로그인할지 (true/false)
    #[arg(long)]
    pub signup_verify: Option<bool>,
    /// 비회원 댓글·회원 가입에 확인 질문을 낼지: off, guest
    #[arg(long)]
    pub captcha: Option<String>,
    /// 금지어. 쉼표로 나눈다. 빈 값이면 모두 지운다
    #[arg(long)]
    pub banned_words: Option<String>,
    /// 회원이 글·댓글을 쓰는 최소 간격(초, 0~3600). 0 이면 끔
    #[arg(long)]
    pub flood_seconds: Option<u32>,
    /// 공개 화면 스킨: default(기본), community(커뮤니티)
    #[arg(long)]
    pub skin: Option<String>,
}

#[derive(Debug, Args)]
pub struct UpdateArgs {
    /// 릴리스 주소. 비우면 환경 변수 MARU_BASE_URL, 그다음 공식 릴리스
    #[arg(long)]
    pub base_url: Option<String>,
    /// 새 판이 있으면 받아서 바꾼다
    #[arg(long)]
    pub yes: bool,
}

#[derive(Debug, Subcommand)]
pub enum ExportCmd {
    /// 손님이 보는 화면을 파일로 내린다. 로그인·댓글·검색·폼은 동작하지 않는다
    Static {
        /// 내보낼 폴더
        #[arg(long)]
        out: PathBuf,
        /// 폴더가 비어 있지 않아도 덮어쓴다
        #[arg(long)]
        yes: bool,
    },
}

#[derive(Debug, Subcommand)]
pub enum ThemeCmd {
    /// 고를 수 있는 테마
    List,
    /// 테마의 색과 모양을 사이트에 입힌다. 글과 구조는 그대로다
    Apply { id: String },
    /// 디자인 토큰. `maru design` 과 같다
    #[command(subcommand)]
    Tokens(ThemeTokensCmd),
}

#[derive(Debug, Subcommand)]
pub enum ThemeTokensCmd {
    /// 현재 토큰
    Get {
        /// 토큰 대신 생성되는 CSS 를 출력
        #[arg(long)]
        css: bool,
    },
    /// 토큰을 고친다. 대비(WCAG AA)를 통과해야 저장된다
    Set(DesignSetArgs),
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
    /// 디자인 모음: 프리셋의 첫 화면 블록과 색·모양. id 를 주면 그 디자인의 블록·토큰 (ADR-0158)
    Gallery {
        /// `maru design gallery` 목록의 id
        id: Option<String>,
    },
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
    // 모듈 툴 명령은 clap 이 뒤쪽 인자를 읽지 않으므로 --json 을 여기서 살린다.
    let external = matches!(cli.command, Some(Command::External(_)));
    let g = Global {
        data: cli.data,
        json: cli.json || (external && json),
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
