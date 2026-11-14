//! `giwa service` (ADR-0124). 시험은 실제 등록을 하지 않는다: 사용자의 컴퓨터에 자동 실행을 남기면 안 된다.
#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::path::Path;
use std::process::Command;

use serde_json::Value;

const BIN: &str = env!("CARGO_BIN_EXE_giwa");

fn run(data: &Path, args: &[&str]) -> Value {
    let out = Command::new(BIN)
        .arg("--data")
        .arg(data)
        .args(args)
        .arg("--json")
        .env_remove("GIWA_TOKEN")
        .output()
        .expect("giwa 실행");
    let stdout = String::from_utf8(out.stdout).expect("UTF-8 출력");
    serde_json::from_str(stdout.trim()).unwrap_or_else(|e| panic!("JSON 아님: {e}\n{stdout}"))
}

#[test]
fn 초기화_전에는_등록하지_않고_다음_명령을_말한다() {
    let tmp = tempfile::tempdir().unwrap();
    let v = run(&tmp.path().join("없음"), &["service", "install", "--print"]);
    assert_eq!(v["ok"], false, "{v}");
    assert!(v["error"]["next"].to_string().contains("giwa init"), "{v}");
}

#[test]
fn 등록_내용을_미리_보고_없으면_지우기도_성공이다() {
    let tmp = tempfile::tempdir().unwrap();
    let data = tmp.path().join("사이트");
    assert_eq!(
        run(
            &data,
            &["init", "--no-samples", "--preset", "blog", "--yes"]
        )["ok"],
        true
    );

    let v = run(
        &data,
        &["service", "install", "--print", "--bind", "127.0.0.1:3177"],
    );
    assert_eq!(v["ok"], true, "{v}");
    let plan = &v["data"];
    assert_eq!(plan["registered"], false);
    assert!(
        plan["name"].as_str().unwrap().starts_with("giwa-"),
        "{plan}"
    );
    let content = plan["content"].as_str().unwrap();
    assert!(content.contains("127.0.0.1:3177"), "{content}");
    assert!(content.contains("serve"), "{content}");
    assert!(content.contains("사이트"), "데이터 디렉터리: {content}");
    if cfg!(windows) {
        assert!(content.contains("LeastPrivilege"), "{content}");
    }

    // 정해진 주소가 없으면 띄운 뒤 찾을 수 없다.
    let v = run(
        &data,
        &["service", "install", "--print", "--bind", "127.0.0.1:0"],
    );
    assert_eq!(v["ok"], false, "{v}");

    let status = run(&data, &["service", "status"]);
    assert_eq!(status["data"]["registered"], false, "{status}");
    assert_eq!(status["data"]["running"], false, "{status}");

    let gone = run(&data, &["service", "uninstall"]);
    assert_eq!(gone["ok"], true, "{gone}");
    assert_eq!(gone["data"]["removed"], false, "{gone}");

    let doctor = run(&data, &["doctor"]);
    let service = doctor["data"]["checks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["id"] == "service")
        .expect("doctor 항목");
    assert_eq!(service["status"], "unknown", "{service}");
}
