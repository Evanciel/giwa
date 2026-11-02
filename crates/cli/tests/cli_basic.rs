//! 바이너리 기본 동작.

// 통합 테스트의 도우미 함수는 clippy.toml 의 allow-*-in-tests 대상이 아니다.
#![allow(clippy::expect_used, clippy::unwrap_used)]

use assert_cmd::Command;
use predicates::prelude::*;

fn maru() -> Command {
    Command::cargo_bin("maru").expect("maru 바이너리")
}

fn json(out: &[u8]) -> serde_json::Value {
    let text = std::str::from_utf8(out).expect("stdout 은 UTF-8");
    serde_json::from_str(text.trim()).expect("stdout 은 JSON 한 줄")
}

#[test]
fn usage_error_is_json_with_exit_code_2() {
    let out = maru().args(["--no-such-flag", "--json"]).assert().code(2);
    let v = json(&out.get_output().stdout);
    assert_eq!(v["ok"], false);
    assert_eq!(v["error"]["code"], "USAGE");
    assert!(
        v["error"]["message"]
            .as_str()
            .unwrap()
            .contains("--no-such-flag"),
        "{v}"
    );
    assert_eq!(v["error"]["next"][0], "maru --help");
}

#[test]
fn usage_error_human_goes_to_stderr() {
    maru()
        .arg("--no-such-flag")
        .assert()
        .code(2)
        .stdout(predicate::str::is_empty())
        .stderr(predicate::str::contains("오류 [USAGE]"))
        .stderr(predicate::str::contains("다음: maru --help"));
}

#[test]
fn no_command_is_usage_error() {
    let out = maru().arg("--json").assert().code(2);
    assert_eq!(json(&out.get_output().stdout)["error"]["code"], "USAGE");
}

#[test]
fn version_prints_crate_version() {
    maru()
        .arg("--version")
        .assert()
        .success()
        .stdout(predicate::str::contains(env!("CARGO_PKG_VERSION")));
}
