//! `maru` 바이너리.

mod commands;
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
    /// API 토큰
    #[command(subcommand)]
    Token(TokenCmd),
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
    /// 만든 뒤 바로 발행
    #[arg(long)]
    pub publish: bool,
}

#[derive(Debug, Args)]
pub struct PostListArgs {
    #[arg(long)]
    pub board: Option<String>,
    /// draft, review, published, hidden, deleted ...
    #[arg(long)]
    pub status: Option<String>,
    #[arg(long, default_value_t = 1)]
    pub page: u32,
    #[arg(long, default_value_t = 20)]
    pub per_page: u32,
}

#[derive(Debug, Subcommand)]
pub enum BoardCmd {
    /// 게시판 목록
    List,
}

#[derive(Debug, Subcommand)]
pub enum TokenCmd {
    /// 토큰을 발급한다. 원문은 이때 한 번만 보인다
    Create(TokenCreateArgs),
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

pub struct Global {
    pub data: PathBuf,
    pub json: bool,
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
    let _ = cli.yes; // 확인 질문이 있는 명령이 아직 없다. 모든 명령은 비대화형이다.
    let g = Global {
        data: cli.data,
        json: cli.json,
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
