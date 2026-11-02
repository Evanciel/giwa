//! 바이너리 기본 동작.

// 통합 테스트의 도우미 함수는 clippy.toml 의 allow-*-in-tests 대상이 아니다.
#![allow(clippy::expect_used, clippy::unwrap_used)]

use assert_cmd::Command;
use predicates::prelude::*;

fn maru() -> Command {
    Command::cargo_bin("maru").expect("maru 바이너리")
}

#[test]
fn version_prints_crate_version() {
    maru()
        .arg("--version")
        .assert()
        .success()
        .stdout(predicate::str::contains(env!("CARGO_PKG_VERSION")));
}
