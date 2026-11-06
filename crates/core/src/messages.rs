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
        "error.service" => "서비스 등록에 실패했습니다: {detail}",
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
        "error.board_layout" => {
            "게시판 형태 `{value}` 는 지원하지 않습니다. 가능한 값: {allowed} (비우면 종류의 기본값)"
        }
        "error.board_order" => {
            "목록 순서 `{value}` 는 지원하지 않습니다. 가능한 값: {allowed} (비우면 종류의 기본값)"
        }
        "error.post_locked" => "잠긴 글입니다. 관리자만 댓글·답변을 달거나 고칠 수 있습니다.",
        "error.accept_kind" => "답변 채택은 질문(qna)·포럼(forum) 게시판에서만 됩니다.",
        "error.accept_who" => "글쓴이나 게시판 관리자만 답변을 채택할 수 있습니다.",
        "error.accept_comment" => "이 글에 공개된 댓글만 채택할 수 있습니다: {id}",
        "error.board_policy_role" => "게시판 정책의 역할 `{value}` 를 알 수 없습니다.",
        "error.board_exists" => "`{slug}` 게시판이 이미 있습니다.",
        "error.board_not_found" => "`{slug}` 게시판을 찾을 수 없습니다.",
        "error.post_not_found" => "글을 찾을 수 없습니다: {id}",
        "error.field_count" => "여분 칸은 {max}개까지 만들 수 있습니다.",
        "error.field_key" => {
            "칸 키 \"{value}\" 는 쓸 수 없습니다. 영문 소문자로 시작하고 영문 소문자·숫자·_ 로 32자까지입니다."
        }
        "error.field_dup" => "같은 키({key})의 칸이 두 번 있습니다.",
        "error.field_type" => {
            "{key} 칸의 형식 {value} 은(는) 쓸 수 없습니다. 쓸 수 있는 형식: {allowed}"
        }
        "error.field_label" => "{key} 칸에 {max}자 안쪽의 이름을 붙여 주세요.",
        "error.field_options" => {
            "{key} 칸의 선택지를 확인해 주세요. 1~50개, 값은 비우지 않고 겹치지 않게 100자까지입니다."
        }
        "error.field_unknown" => "이 게시판에 없는 칸입니다: {key}",
        "error.field_required" => "{label} 칸을 채워 주세요.",
        "error.field_value" => "{label} 칸 값이 올바르지 않습니다 ({hint}).",
        "error.field_arg" => "--field 는 키=값 형식입니다: {value}",
        "error.field_json" => "여분 칸 정의 JSON 을 읽지 못했습니다: {detail}",
        "error.reply_closed" => "이 게시판은 답변글을 받지 않습니다.",
        "error.reply_depth" => "답변은 {max}단계까지만 달 수 있습니다.",
        "error.revision_not_found" => "이 글의 리비전이 아닙니다: {id}",
        "error.bulk_size" => "한 번에 1개에서 {max}개까지 고를 수 있습니다.",
        "error.bulk_action" => {
            "일괄 동작 `{value}` 는 지원하지 않습니다. 가능한 값: publish, hide, delete, restore, move"
        }
        "error.bulk_board" => "옮기려면 게시판을 정해야 합니다.",
        "error.post_transition" => "`{from}` 상태의 글은 `{to}` 상태로 바꿀 수 없습니다.",
        "error.body_too_large" => "본문이 너무 큽니다 (최대 {max}바이트).",
        "error.timestamp" => "시각은 RFC3339 형식이어야 합니다: {value}",
        "error.schedule_range" => "예약 시각은 지금 이후 {days}일 안쪽이어야 합니다: {value}",
        "error.schedule_pick" => "예약할 날짜와 시각을 고르세요.",
        "error.post_schedule_status" => "`{status}` 상태의 글은 예약할 수 없습니다.",
        "hint.post_schedule_status" => "공개된 글은 숨긴 뒤 예약하세요.",
        "error.cover_url" => {
            "대표 이미지는 이 사이트에 올린 사진 주소(/media/...)여야 합니다: {value}"
        }
        "error.token_invalid" => "토큰이 없거나 만료·폐기되었습니다.",
        "error.token_not_found" => "`{id}` 토큰을 찾을 수 없습니다.",
        "error.post_template_builtin" => {
            "기본 틀 `{name}` 은 고치거나 지울 수 없습니다. 다른 이름으로 저장하세요."
        }
        "error.post_template_not_found" => "`{name}` 틀을 찾을 수 없습니다.",
        "error.category_not_found" => "{board} 게시판에 `{slug}` 분류가 없습니다.",
        "error.category_exists" => "{board} 게시판에 이미 `{slug}` 분류가 있습니다.",
        "error.category_too_many" => "분류는 게시판마다 {max}개까지 만들 수 있습니다.",
        "error.tag_length" => "태그는 {max}자까지입니다: {value}",
        "error.tag_invalid" => "글자나 숫자가 없는 태그는 쓸 수 없습니다: {value}",
        "error.tag_too_many" => "태그는 글마다 {max}개까지입니다.",
        "error.tag_not_found" => "`{slug}` 태그를 찾을 수 없습니다.",
        "error.post_secret" => "비밀글입니다. 작성자와 관리자만 읽을 수 있습니다.",
        "error.post_views" => "조회수는 0 이상 {max} 이하의 정수여야 합니다.",
        "hint.post_secret" => "작성자라면 로그인한 뒤 다시 여세요.",
        "error.search_query" => {
            "검색어가 비었거나, 빼기(-)만 있거나, {max}자·낱말 10개를 넘습니다: {query}"
        }
        "error.search_option" => {
            "검색 `{name}` 값 `{value}` 는 쓸 수 없습니다. 가능한 값: {allowed}"
        }
        "hint.search_query" => "예: `제주 국밥`, `제주 OR 부산`, `\"올레 7코스\"`, `국밥 -부산`",
        "error.signup_closed" => "이 사이트는 지금 회원 가입을 받지 않습니다.",
        "error.name_invalid" => {
            "닉네임은 {min}~{max}자로, 제어 문자와 @ < > \" \\ 없이 지어 주세요: {value}"
        }
        "error.name_reserved" => "`{value}` 는 쓸 수 없는 닉네임입니다.",
        "error.bio_invalid" => "자기소개는 {max}자 이하로, 제어 문자 없이 적어 주세요.",
        "error.signature_invalid" => {
            "서명은 {max}자, {lines}줄 이하로, 제어 문자 없이 적어 주세요."
        }
        "error.board_description" => {
            "게시판 안내 문구는 {max}자 이하 평문으로, 제어 문자 없이 적어 주세요."
        }
        "error.field_role" => {
            "`{key}` 칸의 역할 `{value}` 는 쓸 수 없습니다. url 칸은 sponsored·source, date 칸은 deadline, 역할마다 한 칸까지입니다."
        }
        "error.field_filter" => {
            "칸 `{key}`: 거르기(filter)는 select·multiselect·boolean 칸에만 켤 수 있습니다."
        }
        "error.name_taken" => "`{value}` 닉네임은 이미 쓰이고 있습니다.",
        "error.email_unverified" => {
            "메일 인증이 아직 끝나지 않았습니다. 인증 메일을 다시 보냈으니 메일함을 확인하세요."
        }
        "error.ticket_invalid" => "링크가 만료됐거나 이미 쓰였습니다. 다시 요청하세요.",
        "error.password_wrong" => "지금 비밀번호가 맞지 않습니다.",
        "error.owner_withdraw" => "운영자 계정은 탈퇴할 수 없습니다.",
        "error.withdraw_name" => "확인하려면 지금 닉네임을 그대로 입력하세요.",
        "error.email_taken_external" => {
            "이미 가입된 이메일입니다. 그 계정으로 로그인한 뒤 내 정보에서 소셜 로그인을 연결하세요."
        }
        "error.member_self" => "자기 계정의 상태·역할은 여기서 바꿀 수 없습니다.",
        "error.member_owner" => "운영자 계정의 상태·역할은 바꿀 수 없습니다.",
        "error.member_withdrawn" => "탈퇴한 회원입니다.",
        "error.member_role" => "역할 `{value}` 은 줄 수 없습니다. 가능한 값: {allowed}",
        "error.mail_not_found" => "`{id}` 편지를 찾을 수 없습니다.",
        "error.mail_from" => "보낸 사람 주소 `{value}` 는 메일 주소 모양이 아닙니다.",
        "error.mail_smtp_missing" => {
            "transport 가 smtp 이면 [mail.smtp] 의 host·port·security 가 있어야 합니다."
        }
        "error.mail_smtp_host" => "SMTP 서버 이름 `{value}` 가 올바르지 않습니다.",
        "error.mail_smtp_port" => "SMTP 포트가 비었습니다. 보통 587(starttls) 또는 465(tls)입니다.",
        "error.mail_smtp_plain" => {
            "암호화 없는 SMTP(security = none)는 이 컴퓨터 안의 중계 서버에만 쓸 수 있습니다: {host}"
        }
        "error.link_not_found" => {
            "링크 카드 `{url}` 정보가 없습니다. 본문에 넣은 글을 한 번 연 뒤(또는 `maru link list`) 다시 하세요."
        }
        "error.link_status" => "링크 상태 `{value}` 는 없습니다. 가능한 값: pending, ok, failed",
        "error.mail_test_failed" => "시험 편지를 보내지 못했습니다: {detail}",
        "error.mail_transport" => "편지 보내는 방식 `{value}` 는 없습니다. 가능한 값: file, smtp",
        "error.mail_security" => {
            "SMTP 보안 방식 `{value}` 는 없습니다. 가능한 값: starttls, tls, none"
        }
        "error.mail_to" => "받는 사람 주소 `{value}` 는 메일 주소 모양이 아닙니다.",
        "mail.test.subject" => "[{site}] 시험 편지",
        "mail.test.body" => {
            "{site}에서 보낸 시험 편지입니다. 이 편지가 왔다면 편지 보내기 설정이 맞습니다.\n\n가입 인증·비밀번호 찾기 편지도 같은 길로 나갑니다.\n"
        }
        "error.mail_local_only" => {
            "편지 본문에는 1회용 링크가 들어 있어 이 컴퓨터의 `maru mail` 로만 볼 수 있습니다."
        }
        "error.banned_word" => "금지어 「{word}」가 들어 있어 저장할 수 없습니다.",
        "error.banned_word_long" => "금지어는 {max}자까지 적을 수 있습니다.",
        "error.banned_words_many" => "금지어는 {max}개까지 둘 수 있습니다.",
        "error.flood" => "너무 빨리 쓰고 있습니다. {seconds}초 뒤에 다시 쓰세요.",
        "error.flood_seconds" => "쓰기 간격은 0~3600초입니다: {value}",
        "error.same_comment" => "바로 앞에 쓴 댓글과 같습니다.",
        "error.comments_closed" => "이 게시판은 댓글을 받지 않습니다.",
        "error.comment_agent" => {
            "에이전트 토큰으로는 댓글을 쓰지 않습니다. 승인·숨기기·스팸 처리만 할 수 있습니다."
        }
        "error.comment_not_found" => "댓글을 찾을 수 없습니다: {id}",
        "error.comment_deleted" => "지운 댓글은 되돌리지 않습니다.",
        "error.cloudflared_missing" => {
            "cloudflared 가 없습니다. 공식 안내대로 설치한 뒤 다시 실행하세요: {url}"
        }
        "error.expose_no_server" => {
            "{target} 에서 maru 서버가 답하지 않습니다. 서버를 먼저 켜세요."
        }
        "error.expose_failed" => "터널을 열지 못했습니다: {detail}",
        "error.expose_hostname" => "도메인 이름이 올바르지 않습니다: {value}",
        "error.expose_local" => {
            "외부 공개는 이 컴퓨터의 운영자만 합니다. 토큰으로는 할 수 없습니다."
        }
        "error.mcp_token" => "MCP 서버에는 토큰이 필요합니다. 운영자 권한으로는 돌지 않습니다.",
        "error.comment_length" => "댓글은 1~{max}자로 써 주세요.",
        "error.comment_image" => {
            "댓글에는 이 사이트에 올린 그림(jpg·png·gif·webp)만 붙일 수 있습니다."
        }
        "error.comment_password" => "비밀번호가 맞지 않습니다.",
        "error.post_password" => "비밀번호가 맞지 않습니다.",
        "error.guest_secret" => "비회원은 비밀글을 쓸 수 없습니다. 로그인하고 써 주세요.",
        "error.post_agent_board" => {
            "에이전트 토큰은 게시판 글쓰기 화면을 쓰지 않습니다. 초안으로 쓰고 발행 권한으로 올리세요."
        }
        "error.guest_required" => "로그인하지 않았다면 이름과 비밀번호를 적어 주세요.",
        "error.guest_password" => {
            "비밀번호는 {min}자 이상으로 정해 주세요. 나중에 고치거나 지울 때 씁니다."
        }
        "error.captcha_wrong" => "확인 질문의 답이 맞지 않습니다.",
        "error.captcha_option" => {
            "확인 질문 방식 `{value}` 는 쓸 수 없습니다. 가능한 값: {allowed}"
        }
        "error.comment_policy" => "승인 방식 `{value}` 는 쓸 수 없습니다. 가능한 값: {allowed}",
        "error.signup_option" => "가입 방식 `{value}` 는 쓸 수 없습니다. 가능한 값: {allowed}",
        "error.skin_unknown" => "스킨 `{value}` 는 없습니다. 가능한 값: {allowed}",
        "mail.verify.subject" => "[{site}] 메일 주소를 확인해 주세요",
        "mail.verify.body" => {
            "{name} 님, {site}에 가입해 주셔서 고맙습니다.\n\n아래 링크를 누르면 가입이 끝납니다. 24시간 안에 한 번만 쓸 수 있습니다.\n\n{link}\n\n가입한 적이 없다면 이 메일은 지우셔도 됩니다.\n"
        }
        "mail.exists.subject" => "[{site}] 이미 가입된 메일 주소입니다",
        "mail.exists.body" => {
            "누군가 이 메일 주소로 {site}에 가입하려고 했습니다. 이 주소는 이미 가입돼 있습니다.\n\n비밀번호가 생각나지 않으면 아래 링크에서 다시 정하세요. 1시간 안에 한 번만 쓸 수 있습니다.\n\n{link}\n\n직접 하신 일이 아니라면 이 메일은 지우셔도 됩니다.\n"
        }
        "mail.dormant_notice.subject" => "[{site}] 계정이 30일 뒤 휴면으로 바뀝니다",
        "mail.dormant_notice.body" => {
            "{name} 님, {site}에 오랫동안 로그인하지 않으셨습니다.\n\n{days}일 동안 로그인하지 않은 계정은 휴면으로 바뀌고, 메일 주소를 다른 곳에 따로 보관합니다. 30일 안에 로그인하시면 그대로 쓰실 수 있고, 휴면이 된 뒤에도 로그인하면 바로 돌아옵니다.\n\n{link}\n"
        }
        "mail.signals.subject" => "[{site}] 살펴볼 일 {count}건",
        "mail.signals.body" => {
            "{name} 님, 사이트에 살펴볼 일이 생겼습니다.\n\n{lines}\n\n관리 화면: {admin}\n\n사이트 설정에서 「운영 신호 메일」을 끄면 이 메일이 오지 않습니다.\n"
        }
        "mail.reset.subject" => "[{site}] 비밀번호 다시 정하기",
        "mail.reset.body" => {
            "{name} 님, 비밀번호를 다시 정하려면 아래 링크를 누르세요. 1시간 안에 한 번만 쓸 수 있습니다.\n\n{link}\n\n요청한 적이 없다면 이 메일은 지우셔도 됩니다. 비밀번호는 그대로입니다.\n"
        }
        "error.file_not_found" => "`{id}` 첨부 파일을 찾을 수 없습니다.",
        "error.file_type" => "첨부할 수 없는 파일입니다: {name} (가능한 형식: {allowed})",
        "error.file_content" => "파일 내용이 확장자({ext})와 맞지 않습니다: {name}",
        "error.file_name" => "파일 이름이 비었습니다 (최대 {max}자).",
        "error.media_not_found" => "`{id}` 미디어를 찾을 수 없습니다.",
        "error.media_type" => "이미지 파일(jpg, png, gif, webp)만 올릴 수 있습니다: {name}",
        "error.media_too_large" => "파일이 너무 큽니다. {max}MB 이하만 올릴 수 있습니다.",
        "error.not_wiki" => {
            "「{board}」 게시판은 위키가 아닙니다. 게시판 종류를 위키(wiki)로 바꾸세요."
        }
        "error.media_decode" => "이미지를 읽지 못했습니다. 파일이 깨졌거나 너무 큽니다: {detail}",
        "error.media_rotate" => "돌리기는 0·90·180·270도만 됩니다 (받은 값: {value}).",
        "error.media_crop_arg" => "--crop 은 `x,y,가로,세로` 네 개의 0 이상 정수입니다.",
        "error.media_edit_empty" => "돌릴 각도나 자를 영역을 하나 이상 주세요.",
        "error.media_crop" => {
            "자를 영역이 사진({width}×{height}) 밖으로 나갑니다. 돌린 뒤의 크기 기준입니다."
        }
        "error.template_not_found" => "템플릿을 찾을 수 없습니다: {name}",
        "error.json_body" => "요청 본문 JSON 이 올바르지 않습니다: {detail}",
        "error.post_status" => "알 수 없는 글 상태입니다: {value}",
        "error.preset_unknown" => "알 수 없는 프리셋입니다: {value}",
        "error.board_not_empty" => "`{slug}` 게시판에 글이 {count}개 있어 지울 수 없습니다.",
        "error.page_exists" => "`{slug}` 페이지가 이미 있습니다.",
        "error.page_not_found" => "`{slug}` 페이지를 찾을 수 없습니다.",
        "error.page_layout" => "레이아웃 `{value}` 는 지원하지 않습니다. 가능한 값: {allowed}",
        "error.menu_location" => "메뉴 위치 `{value}` 는 지원하지 않습니다. 가능한 값: {allowed}",
        "error.menu_depth" => "메뉴는 {max}단까지만 만들 수 있습니다.",
        "error.changeset_preview_token" => {
            "미리보기 주소가 만료되었거나 이미 쓰였습니다. 새 주소를 받으세요."
        }
        "error.changeset_required" => {
            "토큰은 `{action}` 을(를) 바로 할 수 없습니다. 변경 묶음에 담아 사람의 승인을 받으세요."
        }
        "error.changeset_not_found" => "변경 묶음 {id} 을(를) 찾을 수 없습니다.",
        "error.changeset_status" => "변경 묶음 {id} 은(는) {status} 상태라 이 일을 할 수 없습니다.",
        "error.changeset_op" => "변경 연산이 올바르지 않습니다: {detail}",
        "error.changeset_title" => "변경 묶음 제목은 1~200자로 적어 주세요.",
        "error.changeset_failed" => "{index}번째 연산에서 멈췄습니다: {detail}",
        "error.changeset_apply_agent" => {
            "에이전트 토큰은 변경 묶음을 적용할 수 없습니다. 사람이 승인하거나 자동 적용 토큰을 쓰세요."
        }
        "error.menu_too_many" => "메뉴 항목은 {max}개까지입니다.",
        "error.widget_kind" => "알 수 없는 위젯 종류입니다: {value} (가능: {allowed})",
        "error.widget_region" => "알 수 없는 영역입니다: {value} (가능: {allowed})",
        "error.widget_scope" => {
            "위젯 범위는 all, home, boards, board:<주소>, page:<주소> 중 하나입니다: {value}"
        }
        "error.widget_config" => "`{kind}` 위젯 설정이 올바르지 않습니다: {reason}",
        "error.audit_actor_type" => "행위자 종류는 {allowed} 중 하나입니다 (받은 값 {value}).",
        "error.audit_name" => {
            "{field} 에는 영문 소문자·숫자·`.`·`_`·`-` 만 씁니다 (받은 값 {value})."
        }
        "error.audit_time" => {
            "{field} 는 RFC3339 시각입니다 (예: 2026-11-04T09:00:00+09:00, 받은 값 {value})."
        }
        "error.board_upload_role" => {
            "파일 올리기 역할은 회원 이상이거나 none 이어야 합니다: {value}. 손님 업로드는 받지 않습니다."
        }
        "error.upload_quota" => "파일은 하루에 {max}개까지 올릴 수 있습니다. 내일 다시 시도하세요.",
        "error.module_message" => "{text}",
        "error.module_not_found" => "모듈을 찾을 수 없습니다: {id}",
        "error.module_disabled" => "모듈이 켜져 있지 않습니다: {id}",
        "error.module_tool_not_found" => "{module} 모듈에 없는 툴입니다: {tool}",
        "error.module_remove_confirm" => {
            "제거하면 {id} 모듈의 데이터가 사라집니다. --purge --yes 를 함께 적으세요. 데이터를 남기려면 disable 을 쓰세요."
        }
        "error.form_number" => "`{field}` 칸에는 숫자를 넣으세요: {value}",
        "error.form_choice" => "`{field}` 칸의 값 `{value}` 는 고를 수 없습니다.",
        "error.menu_child_first" => "하위 항목 「{label}」 위에 첫 단계 항목이 있어야 합니다.",
        "error.restore_confirm" => {
            "복원은 지금 데이터를 옮기고 백업으로 바꿉니다. 확인했으면 --yes 를 붙여 다시 실행하세요."
        }
        "error.backup_not_found" => "`{name}` 백업 파일이 없습니다.",
        "error.backup_invalid" => "Maru 백업 파일이 아니거나 깨졌습니다: {path} ({detail})",
        "error.backup_newer" => {
            "이 백업은 더 새 Maru({version}, 스키마 {schema})로 만들었습니다. 이 바이너리는 스키마 {known} 까지 압니다. Maru 를 올린 뒤 복원하세요."
        }
        "error.server_running" => {
            "이 데이터 디렉터리({path})로 서버가 이미 켜져 있습니다. 서버를 끈 뒤 다시 실행하세요."
        }
        "error.stats_days" => "통계 기간은 1~{max}일입니다.",
        "error.stats_retention" => "통계 보존 기간은 30~3650일입니다 (받은 값 {value}).",
        "error.dormant_days" => "휴면 기간은 0(끔) 또는 180~3650일입니다 (받은 값 {value}).",
        "error.mail_retention" => "편지 보존 기간은 7~3650일입니다 (받은 값 {value}).",
        "error.signal_burst" => "댓글 급증 기준은 0(끔) 또는 3~1000개입니다 (받은 값 {value}).",
        "signal.comment_burst" => "댓글이 몰립니다: 「{title}」에 지난 1시간 댓글 {count}개",
        "error.widget_popup_region" => {
            "팝업은 popup 영역에만, popup 영역에는 팝업만 놓을 수 있습니다 (종류 {kind}, 영역 {region})."
        }
        "error.widget_not_found" => "위젯을 찾을 수 없습니다: {id}",
        "error.widget_too_many" => "위젯은 {max}개까지 놓을 수 있습니다.",
        "error.menu_url" => "메뉴 주소는 https://, http:// 또는 / 로 시작해야 합니다: {value}",
        "error.menu_item" => "메뉴 항목 형식이 올바르지 않습니다: {value}",
        "error.token_value" => "디자인 토큰 `{field}` 값이 올바르지 않습니다: {value}",
        "error.owner_exists" => "운영자 계정이 이미 있습니다. 로그인 링크를 다시 받으세요.",
        "error.member_email_required" => "회원이 여럿이라 이메일을 지정해야 합니다.",
        "error.member_not_found" => "회원을 찾을 수 없습니다.",
        "error.member_disabled" => "비활성화된 회원입니다.",
        "error.link_invalid" => "로그인 링크가 만료됐거나 이미 사용됐습니다. 새 링크를 받으세요.",
        "error.login_failed" => "이메일 또는 비밀번호가 올바르지 않습니다.",
        "error.totp_code" => "인증 코드가 맞지 않습니다. 앱의 지금 코드나 복구 코드를 넣으세요.",
        "error.totp_code_left" => "인증 코드가 맞지 않습니다. {left}번 더 넣을 수 있습니다.",
        "error.totp_locked" => "인증 코드를 여러 번 틀렸습니다. 처음부터 다시 로그인하세요.",
        "error.totp_expired" => "인증 단계가 끝났습니다. 처음부터 다시 로그인하세요.",
        "error.totp_already" => "2단계 인증이 이미 켜져 있습니다.",
        "error.totp_not_started" => "2단계 인증 설정을 먼저 시작하세요.",
        "error.totp_off" => "2단계 인증이 꺼져 있습니다.",
        "error.password_length" => "비밀번호는 {min}자 이상 {max}자 이하여야 합니다.",
        "error.email" => "이메일 형식이 올바르지 않습니다: {value}",
        "error.csrf" => "요청을 확인할 수 없습니다. 화면을 새로 고친 뒤 다시 시도하세요.",
        "error.setting_verification" => {
            "`{field}` 값은 영문·숫자·`-`·`_` 100자 이내여야 합니다: {value}"
        }
        "error.token_contrast" => "색 대비가 부족합니다: {pair} = {ratio} (최소 {min})",
        "error.preset_invalid" => "프리셋 `{id}` 을 읽을 수 없습니다: {detail}",
        "error.setting_lang" => "언어 태그가 올바르지 않습니다: {value}",
        "error.setting_tz" => "시간대는 -720 ~ 840 분 사이여야 합니다: {value}",
        "error.scope_unknown" => {
            "알 수 없는 스코프입니다: {value} (read, draft, publish, structure, design, admin)"
        }
        "error.font_not_found" => "`{slug}` 서체를 찾을 수 없습니다.",
        "error.font_file_not_found" => "`{slug}` 서체에 굵기 {weight} 파일이 없습니다.",
        "error.font_file" => "서체 파일이 아니거나 깨졌습니다. 가능한 형식: {allowed}",
        "error.font_weight" => "굵기는 100~900 사이의 100 단위여야 합니다: {value}",
        "error.font_name" => "서체 이름은 1~{max}자여야 합니다.",
        "error.font_slug" => {
            "서체 주소 이름은 영소문자로 시작하는 영소문자·숫자·`-` 2~32자여야 하고, 내장 서체 이름은 쓸 수 없습니다: {value}"
        }
        "error.font_fallback" => "대체 계열이 올바르지 않습니다: {value} (가능: {allowed})",
        "error.font_license" => "라이선스가 올바르지 않습니다: {value} (가능: {allowed})",
        "error.font_license_text" => "라이선스 전문은 {max}KB 이하여야 합니다.",
        "error.font_rights" => "이 서체를 웹에 올려 내보낼 권리가 있는지 확인해야 합니다.",
        "error.font_in_use" => {
            "`{slug}` 서체는 디자인 토큰 `{field}` 에서 쓰고 있어 지울 수 없습니다."
        }
        "warn.font_large" => {
            "서체 파일이 {size}MB 입니다. 방문자 화면이 느릴 수 있으니 woff2 로 줄여 올리세요 (scripts/subset-fonts.py)."
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
