//! 표준 출력 계약. `--json`이면 stdout에 JSON 한 줄, 아니면 사람용 문장을 stderr에.

use std::io::Write;
use std::process::ExitCode;

use maru_core::output::{Failure, Success};
use maru_core::{Code, Error};
use serde::Serialize;

#[expect(dead_code, reason = "U3 init 에서 첫 사용")]
pub fn success<T: Serialize>(data: T, json: bool, human: impl FnOnce(&T) -> String) -> ExitCode {
    if json {
        print_json(&Success::new(data));
    } else {
        println!("{}", human(&data));
    }
    ExitCode::SUCCESS
}

pub fn error(err: &Error, json: bool) -> ExitCode {
    if err.code() == Code::Internal {
        tracing::error!(error = ?err, source = ?std::error::Error::source(err), "내부 오류");
    }
    let body = err.body(None);
    if json {
        print_json(&Failure::new(body));
    } else {
        let mut stderr = std::io::stderr().lock();
        let _ = writeln!(stderr, "오류 [{}]: {}", body.code.as_str(), body.message);
        if let Some(hint) = &body.hint {
            let _ = writeln!(stderr, "힌트: {hint}");
        }
        for cmd in &body.next {
            let _ = writeln!(stderr, "다음: {cmd}");
        }
    }
    ExitCode::from(err.exit_code() as u8)
}

/// clap 오류. 도움말·버전 출력은 오류가 아니다.
pub fn clap_error(e: clap::Error, json: bool) -> ExitCode {
    use clap::error::ErrorKind;
    if matches!(e.kind(), ErrorKind::DisplayHelp | ErrorKind::DisplayVersion) {
        let _ = e.print();
        return ExitCode::SUCCESS;
    }
    let detail = e
        .render()
        .to_string()
        .lines()
        .next()
        .unwrap_or_default()
        .trim_start_matches("error: ")
        .to_owned();
    let err = Error::new(Code::Usage)
        .with_message("error.usage_detail")
        .arg("detail", detail)
        .with_hint("hint.usage")
        .next("maru --help");
    error(&err, json)
}

fn print_json<T: Serialize>(value: &T) {
    match serde_json::to_string(value) {
        Ok(text) => println!("{text}"),
        // 직렬화 실패는 계약 위반이라 최소 형태로라도 낸다.
        Err(_) => {
            println!(r#"{{"ok":false,"error":{{"code":"INTERNAL","message":"출력 직렬화 실패"}}}}"#)
        }
    }
}
