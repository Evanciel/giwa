//! 사용자 대면 문장 카탈로그. 키로 찾고 `{name}` 자리를 채운다.
//!
//! 지금은 한국어만 있다. 영어는 출시 직전에 같은 키로 추가한다.

use std::collections::BTreeMap;

/// 키에 해당하는 한국어 문장.
pub fn lookup(key: &str) -> Option<&'static str> {
    let text = match key {
        "error.internal" => "내부 오류가 발생했습니다. 요청 ID와 함께 로그를 확인하세요.",
        "error.usage" => "명령 사용법이 올바르지 않습니다.",
        "error.validation" => "입력 값이 올바르지 않습니다.",
        "error.not_found" => "대상을 찾을 수 없습니다.",
        "error.unauthorized" => "인증이 필요하거나 인증 정보가 올바르지 않습니다.",
        "error.forbidden" => "이 작업을 할 권한이 없습니다.",
        "error.conflict" => "이미 존재하거나 현재 상태와 충돌합니다.",
        "error.port_in_use" => "{port} 포트를 다른 프로그램이 사용 중입니다.",
        "error.schema_too_new" => "데이터베이스가 이 maru 버전보다 새롭습니다.",
        "error.rate_limited" => "요청이 너무 많습니다. 잠시 후 다시 시도하세요.",
        "error.external" => "외부 연결에 실패했습니다.",
        "hint.port_in_use" => "다른 포트로 실행하세요.",
        "error.usage_detail" => "명령 사용법이 올바르지 않습니다: {detail}",
        "hint.usage" => "`maru --help` 로 사용법을 확인하세요.",
        "hint.schema_too_new" => "더 새로운 maru 로 실행하거나 백업에서 복원하세요.",
        "error.io" => "파일을 읽거나 쓰지 못했습니다: {path}",
        "error.config_parse" => "설정 파일을 해석하지 못했습니다 ({path}): {detail}",
        "error.config_site_url" => "site_url 은 http:// 또는 https:// 로 시작해야 합니다: {value}",
        "error.config_bind" => "bind 는 `127.0.0.1:8080` 형식이어야 합니다: {value}",
        "error.data_dir_not_initialized" => "데이터 디렉터리가 초기화되지 않았습니다: {path}",
        "error.forbidden_permission" => "이 작업에는 `{permission}` 권한이 필요합니다.",
        "error.forbidden_board" => "이 토큰은 해당 게시판에 `{permission}` 작업을 할 수 없습니다.",
        "error.field_length" => "{field} 값이 비었거나 {max}자를 넘습니다.",
        "error.slug_invalid" => "슬러그에는 글자, 숫자, `-` 만 쓸 수 있습니다 (최대 80자): {value}",
        "error.slug_taken" => "{board} 게시판에 이미 `{slug}` 슬러그가 있습니다.",
        "error.board_kind" => "게시판 종류 `{value}` 는 지원하지 않습니다. 가능한 값: {allowed}",
        "error.board_policy_role" => "게시판 정책의 역할 `{value}` 를 알 수 없습니다.",
        "error.board_exists" => "`{slug}` 게시판이 이미 있습니다.",
        "error.board_not_found" => "`{slug}` 게시판을 찾을 수 없습니다.",
        "error.post_not_found" => "글을 찾을 수 없습니다: {id}",
        "error.post_transition" => "`{from}` 상태의 글은 `{to}` 상태로 바꿀 수 없습니다.",
        "error.body_too_large" => "본문이 너무 큽니다 (최대 {max}바이트).",
        "error.timestamp" => "시각은 RFC3339 형식이어야 합니다: {value}",
        "error.token_invalid" => "토큰이 없거나 만료·폐기되었습니다.",
        "error.template_not_found" => "템플릿을 찾을 수 없습니다: {name}",
        "error.json_body" => "요청 본문 JSON 이 올바르지 않습니다: {detail}",
        "error.post_status" => "알 수 없는 글 상태입니다: {value}",
        "error.preset_unknown" => "알 수 없는 프리셋입니다: {value}",
        "error.board_not_empty" => "`{slug}` 게시판에 글이 {count}개 있어 지울 수 없습니다.",
        "error.scope_unknown" => {
            "알 수 없는 스코프입니다: {value} (read, draft, publish, structure, design, admin)"
        }
        "error.input_encoding" => "본문이 UTF-8 이 아닙니다: {path}",
        "hint.input_encoding" => "편집기에서 UTF-8 로 저장한 뒤 `--from` 으로 다시 넘기세요.",
        _ => return None,
    };
    Some(text)
}

/// 키를 찾아 `{name}` 자리를 채운다. 없는 키는 키 자체를 돌려준다(누락이 화면에 드러나도록).
pub fn render(key: &str, args: &BTreeMap<&'static str, String>) -> String {
    let Some(template) = lookup(key) else {
        return key.to_owned();
    };
    let mut out = template.to_owned();
    for (name, value) in args {
        out = out.replace(&format!("{{{name}}}"), value);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fills_placeholders() {
        let mut args = BTreeMap::new();
        args.insert("port", "8080".to_owned());
        assert_eq!(
            render("error.port_in_use", &args),
            "8080 포트를 다른 프로그램이 사용 중입니다."
        );
    }

    #[test]
    fn missing_key_is_visible() {
        assert_eq!(render("no.such.key", &BTreeMap::new()), "no.such.key");
    }
}
