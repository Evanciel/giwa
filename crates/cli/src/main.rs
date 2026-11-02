//! `maru` 바이너리.

use clap::Parser;

/// 에이전트가 설치하고 운영하는 한국형 CMS.
#[derive(Debug, Parser)]
#[command(name = "maru", version, about)]
struct Cli {}

fn main() {
    let _cli = Cli::parse();
}
