//! 활동 기록 (감사 로그, ADR-0062·0137). 동작 이름 대신 사람 말로 「무엇을 했는지」를 보이고,
//! 바뀐 칸만 「이전 → 이후」로 줄인다. 원본 JSON 은 접어 둔다. 조회는 REST·CLI 와 같은 `audit_log::list`.

use axum::Router;
use axum::extract::{Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::Response;
use axum::routing::get;
use maru_core::clock;
use maru_core::services::audit_log::{self, AuditQuery};
use maru_core::services::settings;
use serde::Deserialize;
use serde_json::{Value, json};

use super::{base, fail, merge, render, require};
use crate::AppState;

pub(super) fn routes() -> Router<AppState> {
    Router::new().route("/admin/audit", get(page))
}

#[derive(Debug, Default, Deserialize)]
struct PageQuery {
    #[serde(default)]
    action: String,
    #[serde(default)]
    actor_type: String,
    #[serde(default)]
    target_type: String,
    #[serde(default)]
    target_id: String,
    #[serde(default)]
    page: Option<u32>,
}

/// 위쪽 고르기 단추: (동작 앞부분, 이름)
const AREAS: &[(&str, &str)] = &[
    ("post", "글"),
    ("comment", "댓글"),
    ("member", "회원"),
    ("auth", "로그인"),
    ("board", "게시판"),
    ("page", "페이지"),
    ("design", "스킨·디자인"),
    ("widget", "위젯"),
    ("menu", "메뉴"),
    ("settings", "설정"),
    ("changeset", "변경 묶음"),
    ("module", "기능"),
    ("backup", "백업"),
];

const ACTORS: &[(&str, &str)] = &[
    ("human", "사람"),
    ("agent", "에이전트"),
    ("local", "이 컴퓨터"),
];

fn actor_label(t: &str) -> &'static str {
    match t {
        "human" => "사람",
        "agent" => "에이전트",
        "local" => "이 컴퓨터(CLI)",
        "anonymous" => "손님",
        _ => "자동 작업",
    }
}

async fn page(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<PageQuery>,
) -> Response {
    let who = match require(&state, &headers, "/admin/audit").await {
        Ok(w) => w,
        Err(r) => return *r,
    };
    let some = |v: &str| (!v.trim().is_empty()).then(|| v.trim().to_owned());
    let query = AuditQuery {
        action: some(&q.action),
        actor_type: some(&q.actor_type),
        target_type: some(&q.target_type),
        target_id: some(&q.target_id),
        page: q.page,
        ..Default::default()
    };
    let list = match audit_log::list(&state.ctx, &who.actor, &query).await {
        Ok(l) => l,
        Err(e) => return fail(&state, Some(&who), &e).await,
    };
    let tz = settings::site(&state.ctx)
        .await
        .map(|s| s.tz_offset_min)
        .unwrap_or(540);

    // 같은 날 기록을 한 묶음으로
    let mut days: Vec<Value> = Vec::new();
    for e in &list.items {
        let shown = clock::display_datetime(&e.at, tz);
        let (day, time) = shown.rsplit_once(' ').unwrap_or((shown.as_str(), ""));
        let (icon, what) = describe(&e.action);
        let row = json!({
            "icon": icon,
            "what": what,
            "object": object_name(&e.target_type, e.target_id.as_deref(), &e.diff),
            "who": if e.actor_name.is_empty() { actor_label(&e.actor_type).to_owned() } else { e.actor_name.clone() },
            "kind": actor_label(&e.actor_type),
            "agent": e.actor_type == "agent",
            "at": e.at,
            "time": time,
            "changes": changes(&e.diff),
            "raw": raw(&e.diff),
            "action": e.action,
            "target_type": e.target_type,
            "target_id": e.target_id,
        });
        match days.last_mut() {
            Some(d) if d["day"] == day => {
                if let Some(rows) = d["rows"].as_array_mut() {
                    rows.push(row);
                }
            }
            _ => days.push(json!({ "day": day, "rows": [row] })),
        }
    }

    // 쪽 이동·고르기 링크에 조건을 그대로 붙인다.
    let link = |action: &str, actor: &str| {
        let mut s = String::from("/admin/audit?");
        for (k, v) in [
            ("action", action),
            ("actor_type", actor),
            ("target_type", q.target_type.as_str()),
            ("target_id", q.target_id.as_str()),
        ] {
            if !v.trim().is_empty() {
                s.push_str(&format!("{k}={}&", super::encode(v.trim())));
            }
        }
        s
    };
    let area_now = q.action.trim();
    let actor_now = q.actor_type.trim();
    let mut areas =
        vec![json!({ "label": "전체", "href": link("", actor_now), "on": area_now.is_empty() })];
    areas.extend(AREAS.iter().map(
        |(id, label)| json!({ "label": label, "href": link(id, actor_now), "on": area_now == *id }),
    ));
    let mut actors =
        vec![json!({ "label": "누구나", "href": link(area_now, ""), "on": actor_now.is_empty() })];
    actors.extend(ACTORS.iter().map(
        |(id, label)| json!({ "label": label, "href": link(area_now, id), "on": actor_now == *id }),
    ));
    let filtered = !(q.action.trim().is_empty()
        && q.actor_type.trim().is_empty()
        && q.target_type.trim().is_empty()
        && q.target_id.trim().is_empty());
    let ctx = merge(
        base(&state, Some(&who), "audit").await,
        json!({
            "days": days,
            "total": list.total,
            "page": list.page,
            "pages": list.total_pages(),
            "q": { "action": q.action, "actor_type": q.actor_type, "target_type": q.target_type, "target_id": q.target_id },
            "areas": areas,
            "actors": actors,
            "filtered": filtered,
            "base_query": link(area_now, actor_now),
        }),
    );
    render(&state, StatusCode::OK, "audit.html", &ctx)
}

/// 동작 이름 → (아이콘, 한 일). 모르는 동작은 앞부분으로 고르고 이름을 그대로 보인다.
fn describe(action: &str) -> (&'static str, String) {
    let said = match action {
        "auth.login" => "로그인했습니다",
        "auth.login_link" => "로그인 링크를 만들었습니다",
        "auth.logout" => "로그아웃했습니다",
        "auth.password" => "비밀번호를 바꿨습니다",
        "auth.totp_start" | "auth.totp_enable" => "2단계 인증을 켰습니다",
        "auth.totp_disable" | "auth.totp_reset" => "2단계 인증을 껐습니다",
        "auth.totp_challenge" => "2단계 인증 코드를 넣었습니다",
        "auth.totp_recovery" => "2단계 인증 복구 코드를 썼습니다",
        "backup.create" => "백업을 만들었습니다",
        "backup.delete" => "백업을 지웠습니다",
        "board.create" => "게시판을 만들었습니다",
        "board.update" => "게시판 설정을 고쳤습니다",
        "board.delete" => "게시판을 지웠습니다",
        "category.create" => "분류를 만들었습니다",
        "category.update" => "분류를 고쳤습니다",
        "category.delete" => "분류를 지웠습니다",
        "changeset.create" => "변경 묶음을 만들었습니다",
        "changeset.add" => "변경 묶음에 할 일을 더했습니다",
        "changeset.apply" => "변경 묶음을 사이트에 적용했습니다",
        "changeset.discard" => "변경 묶음을 버렸습니다",
        "changeset.revert" => "변경 묶음을 되돌렸습니다",
        "changeset.preview_link" => "미리 보기 링크를 만들었습니다",
        "comment.create" => "댓글을 달았습니다",
        "comment.update" => "댓글을 고쳤습니다",
        "comment.approve" => "댓글을 승인했습니다",
        "comment.hide" => "댓글을 숨겼습니다",
        "comment.spam" => "댓글을 스팸으로 돌렸습니다",
        "comment.delete" => "댓글을 지웠습니다",
        "media.upload" => "그림을 올렸습니다",
        "media.edit" => "그림을 돌리거나 잘랐습니다",
        "page.publish" => "페이지를 공개했습니다",
        "page.hide" => "페이지를 숨겼습니다",
        "page.delete" => "페이지를 지웠습니다",
        "page.restore" => "페이지를 되살렸습니다",
        "post.publish" | "post.publish_scheduled" => "글을 발행했습니다",
        "post.submit" => "글을 검토에 올렸습니다",
        "post.hide" => "글을 숨겼습니다",
        "post.restore" => "글을 되살렸습니다",
        "post.unaccept" => "답변 채택을 풀었습니다",
        "design.tokens" => "스킨·디자인을 바꿨습니다",
        "file.upload" => "파일을 올렸습니다",
        "file.delete" => "파일을 지웠습니다",
        "font.add" | "font.file.add" => "서체를 더했습니다",
        "font.update" => "서체를 고쳤습니다",
        "font.remove" | "font.file.remove" => "서체를 뺐습니다",
        "link.refresh" => "링크 카드를 새로 읽었습니다",
        "mail.setup" => "메일 보내기를 설정했습니다",
        "mail.test" => "시험 메일을 보냈습니다",
        "media.update" => "그림 정보를 고쳤습니다",
        "media.delete" => "그림을 지웠습니다",
        "member.create" => "회원을 만들었습니다",
        "member.signup" | "member.signup_repeat" => "회원으로 가입했습니다",
        "member.role" => "회원 역할을 바꿨습니다",
        "member.status" => "회원 상태를 바꿨습니다",
        "member.update" => "회원 정보를 고쳤습니다",
        "member.verify" => "가입 메일을 확인했습니다",
        "member.verify_resend" => "가입 확인 메일을 다시 보냈습니다",
        "member.withdraw" => "탈퇴했습니다",
        "member.dormant" => "회원을 휴면으로 바꿨습니다",
        "member.dormant_notice" => "휴면 예정 메일을 보냈습니다",
        "member.wake" => "휴면에서 돌아왔습니다",
        "member.reset_request" => "비밀번호 재설정을 요청했습니다",
        "member.reset" => "비밀번호를 다시 정했습니다",
        "menu.set" => "메뉴를 바꿨습니다",
        "module.enable" => "기능을 켰습니다",
        "module.disable" => "기능을 껐습니다",
        "module.remove" => "기능을 지웠습니다",
        "module.settings" => "기능 설정을 바꿨습니다",
        "page.create" => "페이지를 만들었습니다",
        "page.update" => "페이지를 고쳤습니다",
        "post.create" | "post.write" => "글을 썼습니다",
        "post.update" => "글을 고쳤습니다",
        "post.delete" => "글을 지웠습니다",
        "post.lock" => "글을 잠갔습니다",
        "post.unlock" => "글 잠금을 풀었습니다",
        "post.move" => "글을 다른 게시판으로 옮겼습니다",
        "post.revert" => "글을 앞 판으로 되돌렸습니다",
        "post.accept" => "답변을 채택했습니다",
        "post.views.set" => "조회수를 고쳤습니다",
        "redirect.add" => "주소 옮김을 더했습니다",
        "redirect.remove" => "주소 옮김을 뺐습니다",
        "search.reindex" => "검색 색인을 다시 만들었습니다",
        "settings.update" => "사이트 설정을 바꿨습니다",
        "signal.alert" => "운영 알림이 생겼습니다",
        "site.expose" => "사이트 외부 공개를 바꿨습니다",
        "template.update" => "템플릿을 고쳤습니다",
        "template.delete" => "템플릿을 지웠습니다",
        "token.create" => "에이전트 열쇠(토큰)를 만들었습니다",
        "token.revoke" => "에이전트 열쇠(토큰)를 거뒀습니다",
        "widget.place" => "위젯을 놓았습니다",
        "widget.configure" => "위젯을 고쳤습니다",
        "widget.remove" => "위젯을 뺐습니다",
        _ => "",
    };
    let icon = match action.split('.').next().unwrap_or_default() {
        "post" => "posts",
        "comment" => "comments",
        "member" | "auth" => "members",
        "board" | "category" => "boards",
        "page" | "redirect" => "pages",
        "design" | "font" => "design",
        "widget" => "widgets",
        "menu" => "menus",
        "settings" | "site" | "mail" | "search" => "settings",
        "changeset" => "changesets",
        "module" => "modules",
        "backup" => "backups",
        "token" => "password",
        "media" | "file" | "link" => "image",
        "signal" => "bell",
        _ => "audit",
    };
    let what = if said.is_empty() {
        format!("{action} 동작")
    } else {
        said.to_owned()
    };
    (icon, what)
}

/// 대상의 이름: diff 안의 이름·제목·메일, 없으면 메뉴 위치 같은 알려진 id.
fn object_name(target_type: &str, target_id: Option<&str>, diff: &Value) -> String {
    // 사이트 설정·디자인은 대상이 사이트 하나라 이름을 붙이지 않는다.
    if target_type == "site" {
        return String::new();
    }
    for side in ["after", "before"] {
        for k in ["name", "title", "label", "email", "display_name"] {
            if let Some(s) = diff[side][k].as_str().filter(|s| !s.is_empty()) {
                return s.chars().take(60).collect();
            }
        }
    }
    for k in ["title", "name", "email", "slug", "id"] {
        if let Some(s) = diff[k].as_str().filter(|s| !s.is_empty()) {
            return s.chars().take(60).collect();
        }
    }
    match (target_type, target_id) {
        ("menu", Some("header")) => "머리글 메뉴".into(),
        ("menu", Some("footer")) => "바닥글 메뉴".into(),
        ("menu", Some("quick")) => "바로가기 메뉴".into(),
        _ => String::new(),
    }
}

/// 이전·이후가 다 있는 기록에서 바뀐 칸만 「칸: 이전 → 이후」로.
fn changes(diff: &Value) -> Vec<Value> {
    let (Some(before), Some(after)) = (diff["before"].as_object(), diff["after"].as_object())
    else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for (k, a) in after {
        if matches!(k.as_str(), "updated_at" | "created_at" | "id" | "version") {
            continue;
        }
        let b = before.get(k).unwrap_or(&Value::Null);
        if a == b {
            continue;
        }
        out.push(json!({ "field": field_label(k), "before": short(b), "after": short(a) }));
        if out.len() == 8 {
            break;
        }
    }
    out
}

fn field_label(k: &str) -> String {
    let s = match k {
        "name" => "이름",
        "title" => "제목",
        "status" => "상태",
        "slug" => "주소",
        "description" => "소개",
        "body_md" | "body" => "본문",
        "role" => "역할",
        "email" => "메일",
        "display_name" => "표시 이름",
        "enabled" => "켜짐",
        "config" => "설정",
        "scope" => "보일 곳",
        "region" => "자리",
        "sort_order" => "순서",
        "color" => "색",
        "color_dark" => "어두운 화면 색",
        "font" => "서체",
        "radius" => "모서리",
        "layout" => "배치",
        "lang" => "언어",
        "skin" => "배치",
        "policy" => "권한",
        "board_id" => "게시판",
        "published_at" => "발행 때",
        "tags" => "태그",
        "tz_offset_min" => "시간대(분)",
        "show_views" => "조회수 보이기",
        "show_search" => "검색 칸 보이기",
        "list_under_post" => "글 아래 게시판 목록",
        "list_votes" => "목록에 추천 수",
        "stickers" => "스티커",
        "link_cards" => "링크 카드",
        "posts_per_page" => "한 쪽 글 수",
        "toc_min_headings" => "목차 소제목 수",
        "home_page" => "첫 화면 페이지",
        "signup" => "회원 가입",
        "signup_verify" => "가입 메일 확인",
        "captcha" => "로봇 막기 질문",
        "naver_verification" => "네이버 확인 값",
        "google_verification" => "구글 확인 값",
        "stats_retention_days" => "통계 보관 날수",
        "banned_words" => "금지어",
        "flood_seconds" => "글 사이 최소 간격(초)",
        "dormant_days" => "휴면 날수",
        "mail_retention_days" => "메일 보관 날수",
        "signal_comment_burst" => "댓글 몰림 알림",
        "signal_mail" => "알림 메일",
        _ => return k.to_owned(),
    };
    s.to_owned()
}

fn short(v: &Value) -> String {
    match v {
        Value::Null => "없음".into(),
        Value::Bool(true) => "켬".into(),
        Value::Bool(false) => "끔".into(),
        Value::String(s) if s.is_empty() => "비어 있음".into(),
        Value::String(s) => {
            let t: String = s.chars().take(40).collect();
            if s.chars().count() > 40 {
                format!("{t}…")
            } else {
                t
            }
        }
        Value::Number(n) => n.to_string(),
        Value::Array(a) => format!("{}개", a.len()),
        Value::Object(_) => "여러 값".into(),
    }
}

fn raw(diff: &Value) -> String {
    if diff.is_null() || diff.as_object().is_some_and(|o| o.is_empty()) {
        String::new()
    } else {
        serde_json::to_string_pretty(diff).unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_like_a_sentence() {
        assert_eq!(describe("board.create").1, "게시판을 만들었습니다");
        assert_eq!(describe("board.create").0, "boards");
        assert_eq!(describe("thing.odd").1, "thing.odd 동작");
        let diff = json!({ "before": { "name": "공지", "updated_at": "a" }, "after": { "name": "공지사항", "updated_at": "b" } });
        assert_eq!(object_name("board", None, &diff), "공지사항");
        let c = changes(&diff);
        assert_eq!(c.len(), 1);
        assert_eq!(c[0]["field"], "이름");
        assert_eq!(c[0]["before"], "공지");
        assert_eq!(
            object_name("menu", Some("header"), &json!({})),
            "머리글 메뉴"
        );
    }
}
