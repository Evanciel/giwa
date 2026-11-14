//! 이름을 바꾸기 전(maru) 설치가 그대로 이어지는지 (ADR-0173): `maru.db` 를 `giwa.db` 로 옮기고,
//! `MARU_*` 환경 변수와 `maru_` 토큰을 받는다. 실제 바이너리·실제 DB 파일로 본다.
#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::path::Path;
use std::process::Command;

use serde_json::Value;

const BIN: &str = env!("CARGO_BIN_EXE_giwa");

fn run(data: &Path, args: &[&str], env: &[(&str, &str)]) -> Value {
    let mut cmd = Command::new(BIN);
    cmd.arg("--data").arg(data).args(args).arg("--json");
    for k in ["GIWA_TOKEN", "MARU_TOKEN", "GIWA_LANG", "MARU_LANG"] {
        cmd.env_remove(k);
    }
    for (k, v) in env {
        cmd.env(k, v);
    }
    let out = cmd.output().expect("giwa 실행");
    let stdout = String::from_utf8(out.stdout).expect("UTF-8 출력");
    serde_json::from_str(stdout.trim()).unwrap_or_else(|e| panic!("JSON 아님: {e}\n{stdout}"))
}

#[test]
fn old_maru_install_keeps_working() {
    let tmp = tempfile::tempdir().unwrap();
    let data = tmp.path().join("site");
    let init = run(&data, &["init", "--no-samples", "--yes"], &[]);
    assert_eq!(init["ok"], true, "{init}");
    let token = init["data"]["token"]["secret"].as_str().unwrap().to_owned();
    assert!(token.starts_with("giwa_"), "{token}");

    // 예전 이름으로 되돌려 놓는다.
    std::fs::rename(data.join("giwa.db"), data.join("maru.db")).unwrap();

    // 처음 여는 명령이 maru.db 를 giwa.db 로 옮기고 그대로 쓴다.
    let status = run(&data, &["post", "list"], &[]);
    assert_eq!(status["ok"], true, "{status}");
    assert!(data.join("giwa.db").is_file());
    assert!(!data.join("maru.db").exists());

    // 예전 환경 변수 MARU_TOKEN
    let as_agent = run(&data, &["post", "list"], &[("MARU_TOKEN", &token)]);
    assert_eq!(as_agent["ok"], true, "{as_agent}");
    let wrong = run(&data, &["post", "list"], &[("MARU_TOKEN", "maru_0000")]);
    assert_eq!(wrong["error"]["code"], "UNAUTHORIZED", "{wrong}");

    // GIWA_ 가 MARU_ 보다 먼저다: 틀린 GIWA_TOKEN 이면 맞는 MARU_TOKEN 이 있어도 거절.
    let both = run(
        &data,
        &["post", "list"],
        &[("GIWA_TOKEN", "giwa_wrong"), ("MARU_TOKEN", &token)],
    );
    assert_eq!(both["ok"], false, "{both}");

    // MARU_LANG 도 읽는다.
    let en = run(&data, &["media", "show", "nope"], &[("MARU_LANG", "en")]);
    assert_eq!(en["ok"], false, "{en}");
    assert!(en["error"]["message"].as_str().unwrap().is_ascii(), "{en}");
}
