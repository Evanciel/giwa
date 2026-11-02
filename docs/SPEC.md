# Maru 설계서 (SPEC)

> 위치: `docs/SPEC.md`. 프로젝트 지침은 루트 `CLAUDE.md`.
> 상태: 초안 v0.1 (2026-11-02). 프로젝트명 `maru`는 가칭.

---

## 0. 비전과 범위

**에이전트에 한마디 하면, 사용자의 컴퓨터에 한국형 홈페이지·게시판·커뮤니티·블로그가 몇 분 만에 멋지게 만들어지는 러스트 CMS 코어.**

| 키워드 | 의미 | 승부처 |
|---|---|---|
| 에이전트 우선 | 설치·세팅·콘텐츠·디자인 변경을 에이전트가 수행 | CLI·MCP 설계, changeset 승인 흐름 |
| 한국형 | 게시판 문화, 네이버·카카오 연동, 한글 타이포, 네이버 검색 노출 | 공식 모듈과 프리셋 |
| 빠르고 간편 | 한마디 → 몇 분 안에 완성 | 사이트 프리셋 |
| 멋진 | 기본 결과물이 요즘 디자인 수준 | 디자인 토큰 + 섹션 블록 + 테마 품질 |

**첫 타깃**: 에이전트를 이미 쓰는 개발자·에이전시·1인 사업자. 비개발자 대상 호스팅(SaaS)은 범위 밖(검증 후 결정).

**범위 밖 (당분간)**: 쇼핑몰, SMS, 본인확인, 멀티 테넌트 호스팅, WASM 외부 모듈(Phase 5).

---

## 1. 핵심 사용자 시나리오

### 1.1 설치부터 공개까지
1. 사용자: "내 컴퓨터에 동네 카페 홈페이지 만들어줘. 공지랑 갤러리 게시판 있고, 카카오 로그인 되게."
2. 에이전트가 저장소의 `AGENTS.md`(또는 `llms.txt`)를 읽는다.
3. 설치 스크립트로 바이너리 설치 → `maru init --preset cafe --name "OO카페" --yes --json`
4. `maru mcp install --client claude-code` 로 MCP 연결
5. 에이전트가 MCP 툴로 게시판·페이지·메뉴·위젯 구성, `social_login` 모듈 활성화 (모두 하나의 changeset)
6. `http://127.0.0.1:8080/__preview/<changeset>` 링크를 사용자에게 제시
7. 사용자 승인 → `changeset apply`
8. `maru service install` (상시 실행), `maru expose --cloudflare` (외부 공개)

### 1.2 운영
- "이번 주 신메뉴 공지 써줘" → `create_draft` → 사용자 확인 → `publish`
- "메인에 갤러리 최신 6장 넣어줘" → `place_widget` (changeset, 미리보기)
- "댓글 승인 대기 정리해줘" → `list_pending_comments` → 스팸 처리 (댓글 본문은 데이터로만 취급)
- "디자인 좀 더 따뜻하게" → 디자인 토큰 변경 changeset → 미리보기 → 승인

### 1.3 되돌리기
- "어제 바꾼 디자인 원래대로" → `revert_changeset`

---

## 2. 그누보드에서 가져온 것과 바꾼 것

| 그누보드 요소 | 판정 | Maru에서의 형태 |
|---|---|---|
| 게시판 = 컬렉션, 스킨으로 재활용 | 개선해서 가져옴 | `boards.kind` + 뷰 템플릿 |
| 게시판별 테이블 (`g5_write_{bo_table}`) | 버림 | 단일 `posts` 테이블 + `board_id` |
| 여분필드 `wr_1~10` | 개선 | 게시판별 `field_schema`(JSON Schema) + `posts.meta`(JSON) |
| 회원 레벨 1~10 | 개선 | 역할 + 액션 권한, 사람·에이전트 공통 |
| 답글 정렬 문자열 | 버림 | `parent_id` + `thread_path` |
| HTML 에디터 저장 | 버림 | Markdown 원본 저장 |
| 내용관리, 메뉴관리 | 가져옴 | `pages`, `menus` |
| 최신글 `latest()` | 개선 | 위젯 시스템의 기본 위젯 |
| 테마·스킨 | 개선 | 테마 + 영역(region) + 위젯 + 섹션 블록 + 디자인 토큰 |
| extend·훅·플러그인 | 개선 | 모듈 시스템 (§7) |
| 포인트·쪽지·스크랩·추천·투표·팝업 등 | 모듈 | 공식 모듈 |
| 소셜 로그인 | 모듈 | 공식 모듈 (네이버·카카오·구글) |
| 적응형 모바일 별도 템플릿 | 버림 | 반응형만 |
| 영카트·SMS·본인확인 | 범위 밖 | 별도 패키지 후보 |

---

## 3. 아키텍처

### 3.1 레이어
```
[웹 화면 /admin]  [공개 사이트]  [REST /api/v1]  [CLI maru]  [MCP 서버]
          \             |              |              |           /
                         ───── core 서비스 레이어 ─────
                  (권한 검사 · changeset · 이벤트 발행 · 감사 로그)
                                     |
                       storage (sqlx/SQLite)   modules (레지스트리·이벤트 버스)
```
- CLI는 기본적으로 **로컬 데이터 디렉터리를 직접 열어** 서비스 레이어를 호출한다(서버 미실행 상태에서도 동작). 서버 실행 중이면 SQLite WAL 모드로 동시 접근을 허용한다.
- MCP 서버는 `maru mcp serve`(STDIO)와 `serve` 내장 HTTP 엔드포인트(`/mcp`, 토큰 인증) 두 가지를 지원한다.

### 3.2 코어 도메인

| 도메인 | 담당 |
|---|---|
| 콘텐츠 | boards, posts(글·댓글), revisions, categories |
| 사이트 구조 | pages, menus, site_settings |
| 사용자·권한 | members, roles, tokens, 권한 정책 |
| 미디어 | media 업로드, 썸네일, webp 변환 |
| 표현 | themes, design tokens, regions, widget_instances, blocks |
| 플랫폼 | module registry, event bus, changesets, audit_log |

### 3.3 데이터 디렉터리
```
<data>/
├─ maru.db            # SQLite (WAL)
├─ media/             # 업로드 원본·파생 이미지
├─ themes/            # 사용자 테마 (기본 테마는 바이너리 내장, 수정 시 여기로 복사)
├─ modules/           # (Phase 5) 외부 모듈
├─ backups/
└─ config.toml        # 바인딩 주소, 포트, 사이트 URL 등
```

---

## 4. 데이터 모델 (초안)

공통 컬럼: `id`(ULID 문자열), `created_at`, `updated_at`(UTC, RFC3339).

| 테이블 | 핵심 컬럼 | 비고 |
|---|---|---|
| site_settings | key, value(JSON) | 사이트명, 설명, 언어, 활성 테마, 토큰 오버라이드 등 |
| boards | slug, name, kind, view_template, field_schema(JSON), policy(JSON), group_id, sort_order | kind: blog, notice, gallery, free, qna, faq |
| board_groups | slug, name, policy(JSON) | 그누보드 게시판 그룹 대응 |
| categories | board_id, name, sort_order | |
| posts | board_id, parent_id, thread_path, kind(post/comment), status, title, slug, body_md, excerpt, meta(JSON), author_id, actor_type(human/agent), is_notice, is_secret, category_id, published_at, view_count | status: draft, review, published, hidden, pending(댓글 승인 대기), spam |
| revisions | post_id(또는 page_id), body_md, meta, actor, changeset_id | 글·페이지 공용 |
| pages | slug, title, body_md, layout, blocks(JSON), status | 블록 기반 페이지 지원 |
| menus | location(header/footer), items(JSON 트리) | |
| members | email, display_name, password_hash, status | |
| roles / member_roles | name, permissions(JSON) | 기본 역할: owner, admin, editor, member, guest |
| tokens | name, token_hash, scopes(JSON), board_ids(JSON, null=전체), expires_at, last_used_at | 에이전트·자동화용 |
| media | path, mime, size, width, height, alt_text, variants(JSON) | |
| widget_instances | type, region, page_scope, position, config(JSON), enabled | |
| modules | id, version, enabled, settings(JSON), installed_at | |
| changesets | title, actor, status(preview/applied/reverted/discarded), ops(JSON), created_at, applied_at | |
| audit_log | actor, actor_type, action, target_type, target_id, diff(JSON), changeset_id, at | |

인덱스: `posts(board_id, status, published_at)`, `posts(parent_id)`, `posts(slug)`, 전문 검색은 SQLite FTS5 가상 테이블 `posts_fts(title, body_md)`.

---

## 5. 권한 모델

### 5.1 액션
`site.view`, `board.list`, `post.read`, `post.write`, `post.comment`, `post.upload`, `post.publish`, `post.moderate`, `structure.edit`(게시판·페이지·메뉴), `design.edit`(테마·토큰·위젯), `module.manage`, `member.manage`, `site.admin`

### 5.2 게시판 정책 예시
```json
{
  "list": "guest",
  "read": "guest",
  "write": "member",
  "comment": "member",
  "upload": "member",
  "moderate": "editor",
  "comment_approval": true,
  "secret_posts": false
}
```

### 5.3 에이전트 토큰 스코프

| 스코프 | 허용 | 기본 발급 |
|---|---|---|
| `read` | 조회 전용 | ✅ |
| `draft` | 초안 작성·수정, 미디어 업로드, 리뷰 제출 | ✅ |
| `publish` | 발행·숨김·삭제 | 사용자가 명시적으로 발급 |
| `structure` | 게시판·페이지·메뉴 변경 (changeset 필수) | 사용자가 명시적으로 발급 |
| `design` | 테마·토큰·위젯 변경 (changeset 필수) | 사용자가 명시적으로 발급 |
| `admin` | 모듈 설치·제거, 회원 관리, 설정 | 사용자가 명시적으로 발급 |

- `structure`, `design`, `admin` 작업은 토큰이 있어도 **changeset으로만** 반영된다. 적용(apply)은 사람(owner/admin) 또는 `--auto-apply`가 설정된 토큰만 가능.
- 외부 사용자 콘텐츠(댓글, 문의)를 읽는 툴은 응답에 `"untrusted": true` 플래그를 붙이고, 본문을 별도 필드로 분리한다.

---

## 6. Changeset

- 에이전트의 구조·디자인·발행 변경은 `changesets.ops`에 연산 목록으로 쌓인다.
- 상태: `preview` → `applied` 또는 `discarded`, `applied` → `reverted`
- 미리보기: `/__preview/<changeset_id>` — 현재 상태에 ops를 가상 적용해 렌더링. owner/admin 세션 또는 1회용 미리보기 토큰 필요.
- 적용: `maru changeset apply <id>` 또는 관리 화면 버튼. 트랜잭션으로 한 번에 적용.
- 롤백: 적용 시 역연산을 함께 저장해 `revert` 한 번으로 복원.
- 콘텐츠 초안(`create_draft`)은 changeset 없이 `draft` 상태로 저장(발행만 별도 권한).

---

## 7. 모듈 시스템

### 7.1 매니페스트 (`module.toml`)
```toml
[module]
id = "point"
name = "포인트"
version = "0.1.0"
requires = ["core>=0.1"]

[provides]
migrations = "migrations/"
routes = ["/points"]
widgets = ["point_ranking"]
events = ["post.created", "comment.created"]
permissions = ["point.view", "point.adjust"]
mcp_tools = ["get_points", "adjust_points"]
cli = ["point"]
settings_schema = "settings.schema.json"
```

### 7.2 트레이트 (초안)
```rust
pub trait Module: Send + Sync {
    fn manifest(&self) -> &Manifest;
    fn migrations(&self) -> Vec<Migration>;
    fn routes(&self) -> Option<Router<AppState>>;
    fn widgets(&self) -> Vec<Box<dyn Widget>>;
    fn tools(&self) -> Vec<ToolDef>;          // MCP 툴과 CLI 서브커맨드를 함께 생성
    fn on_event(&self, ev: &Event, ctx: &Ctx) -> Result<(), ModuleError>;
    fn settings_schema(&self) -> Option<schemars::Schema>;
}

pub trait Widget: Send + Sync {
    fn id(&self) -> &'static str;
    fn name(&self) -> &str;                    // 한국어 표시명
    fn config_schema(&self) -> schemars::Schema; // 에이전트가 읽고 채움
    fn render(&self, cfg: &serde_json::Value, ctx: &RenderCtx) -> Result<String, WidgetError>;
}
```

### 7.3 생명주기
설치(마이그레이션 실행) → 활성화(라우트·위젯·툴·이벤트 등록) → 비활성화(등록 해제, 데이터 유지) → 제거(옵션으로 down 마이그레이션 실행). 비활성 모듈의 위젯 인스턴스는 렌더링에서 조용히 제외하고 관리 화면에 경고 표시.

### 7.4 코어 이벤트 (초안)
`post.created`, `post.published`, `post.deleted`, `comment.created`, `comment.approved`, `member.registered`, `member.login`, `changeset.applied`, `media.uploaded`

### 7.5 구현 방식
- **Phase 2~4**: 공식 모듈은 바이너리에 내장하고 런타임 on/off (`inventory` 또는 명시적 등록 목록).
- **Phase 5**: 외부 모듈은 WASM 컴포넌트(wasmtime)로 샌드박스 실행. 호스트 API는 서비스 레이어 일부만 노출.

---

## 8. 테마 · 위젯 · 블록 · 프리셋

### 8.1 테마 구조
```
themes/<theme-id>/
├─ theme.toml          # 이름, 버전, 라이선스, 지원 영역, 기본 토큰 프리셋
├─ tokens.json         # 디자인 토큰 기본값
├─ templates/          # minijinja: base, home, board_list, post, page, member/*
├─ blocks/             # 섹션 블록 템플릿
└─ assets/             # css, js, 이미지 (자체 제작 또는 재배포 가능 라이선스만)
```

### 8.2 디자인 토큰
에이전트는 CSS를 직접 쓰지 않고 토큰과 블록 조합으로 디자인을 바꾼다. 토큰은 CSS 변수로 출력.

```json
{
  "color": { "bg": "#FAF8F5", "fg": "#1C1B1A", "primary": "#B5562D", "muted": "#8A847D", "surface": "#FFFFFF", "border": "#E7E2DC" },
  "color_dark": { "...": "다크 모드 값" },
  "font": { "body": "Pretendard", "heading": "Pretendard", "mono": "D2Coding" },
  "type_scale": { "base": 17, "ratio": 1.25 },
  "radius": { "sm": 6, "md": 12, "lg": 20 },
  "space": { "unit": 8, "section": 96 },
  "layout": { "max_width": 1120, "header": "sticky", "density": "comfortable" },
  "motion": { "level": "subtle" }
}
```
토큰 값은 대비(WCAG AA) 검증을 통과해야 저장된다.

### 8.3 영역과 위젯
- 영역: `header`, `hero`, `main`, `sidebar`, `footer` (테마가 지원 영역 선언)
- 코어 기본 위젯: `latest_posts`, `markdown_block`, `menu`, `image_banner`(슬라이더), `search`, `gallery_grid`, `notice_bar`
- 위젯 배치 예: `place_widget { type: "latest_posts", region: "main", page_scope: "home", config: { board: "notice", limit: 5, view: "card" } }`

### 8.4 한글 타이포 기본값
- 본문 폰트 Pretendard(OFL) 셀프호스팅, `word-break: keep-all`, `line-height` 1.7 내외, 본문 16~18px
- 제목은 `text-wrap: balance`, 숫자는 `font-variant-numeric: tabular-nums`(표·통계)

### 8.5 섹션 블록 (초안)
히어로(타이포형·이미지형·분할형), 기능 소개 그리드, 벤토 그리드, 게시판 최신글, 갤러리, 후기·인용, FAQ 아코디언, 오시는 길(지도), 영업시간, CTA, 연락처 폼, 푸터

### 8.6 사이트 프리셋
프리셋 = 테마 + 토큰 + 게시판 + 페이지(블록 구성) + 메뉴 + 위젯 배치 + 모듈 목록. `themes/presets/<id>.toml`.

| 프리셋 | 기본 구성 |
|---|---|
| `blog` | 블로그 게시판, 소개 페이지, 태그·검색, 최신글 위젯 |
| `community` | 공지·자유·갤러리·Q&A 게시판, 회원·댓글 승인, 소셜 로그인 |
| `cafe` (소상공인 가게) | 공지·갤러리, 소개·메뉴·오시는 길·영업시간 블록, 카카오 채널 버튼 |
| `portfolio` | 작업물 갤러리, 소개, 연락처 |
| `org` (기관·단체) | 공지·자료실·FAQ, 소개·연혁·조직도 블록 |

### 8.7 디자인 소싱 규칙 (중요)
- 어워즈 사이트 등 외부 사례는 **트렌드 조사 자료로만** 쓴다. 30~50개 사례에서 반복 패턴(레이아웃, 타이포 스케일, 여백, 모션, 색 조합)을 추출해 문서화하고, 그 원리로 블록과 토큰을 **새로 설계**한다.
- 특정 사이트의 코드·이미지·일러스트·고유한 화면 구성을 복제하지 않는다. 완성 테마를 참고 사례와 나란히 놓고 "특정 사이트가 떠오르는지" 점검한다.
- 외부 리소스(폰트, 아이콘, UI 키트)는 재배포 가능한 라이선스만, `THIRD_PARTY.md`에 기록.

---

## 9. 한국형 기능

| 기능 | 형태 | Phase |
|---|---|---|
| 게시판 kind (공지·자유·갤러리·Q&A·FAQ) | 코어 | 1, 4 |
| 비밀글, 공지 고정, 분류 | 코어 | 4 |
| 네이버·카카오·구글 로그인 | 공식 모듈 `social_login` | 4 |
| 카카오톡 공유, 카카오 채널 버튼 | 공식 모듈 `kakao` (위젯) | 4 |
| 네이버 지도 / 카카오맵 블록 | 공식 모듈 `map` | 4 |
| 네이버 서치어드바이저용 사이트맵·RSS·메타 | 코어 (SEO) | 1 |
| 개인정보처리방침·이용약관 템플릿 페이지 | 프리셋 콘텐츠 | 4 |
| 포인트, 추천, 스크랩, 쪽지 | 공식 모듈 | 4 이후 |
| 방문자 통계 (쿠키 없는 경량 집계) | 코어 | 2 |

외부 서비스 API 사양은 구현 시점에 공식 문서로 확인한다.

---

## 10. CLI 명세

바이너리: `maru`. 전역 옵션: `--data <dir>`(기본 `./maru-data` 또는 `MARU_DATA`), `--json`, `--yes`, `--quiet`.

| 명령 | 설명 |
|---|---|
| `maru init --preset <id> --name <사이트명> [--theme] [--yes]` | 데이터 디렉터리 생성, 마이그레이션, owner 계정·초기 토큰 발급, 프리셋 적용 |
| `maru serve [--bind 127.0.0.1:8080]` | 웹 서버 + HTTP MCP 엔드포인트 |
| `maru doctor` | DB, 포트, 권한, 디스크, 터널, 서비스 상태 점검 + 해결 명령 제시 |
| `maru board list/create/update/delete` | 게시판 |
| `maru post list/new/edit/submit/publish/hide/revert` | 글 (`--from file.md` 지원) |
| `maru page ...`, `maru menu ...` | 페이지·메뉴 |
| `maru widget types/list/place/configure/remove` | 위젯 |
| `maru theme list/apply/tokens get/tokens set` | 테마·토큰 |
| `maru module list/enable/disable/install/uninstall` | 모듈 |
| `maru changeset list/show/preview-url/apply/discard/revert` | changeset |
| `maru token create/list/revoke --scope ...` | 토큰 |
| `maru mcp serve` / `maru mcp install --client <claude-code\|claude-desktop\|cursor>` | MCP STDIO 서버 / 클라이언트 설정 등록 |
| `maru service install/uninstall/status` | systemd(user), launchd, Windows 서비스 |
| `maru expose --cloudflare` / `maru expose status` | Cloudflare Tunnel 연결 안내·설정 |
| `maru backup` / `maru restore <file>` | 백업·복원 |
| `maru update` | 새 버전 확인·교체 (서명 검증) |

### 10.1 `--json` 출력 계약
성공:
```json
{ "ok": true, "data": { "...": "..." }, "warnings": [] }
```
실패:
```json
{
  "ok": false,
  "error": {
    "code": "PORT_IN_USE",
    "message": "8080 포트를 다른 프로그램이 사용 중입니다.",
    "hint": "다른 포트로 실행하세요.",
    "next": ["maru serve --bind 127.0.0.1:8081"]
  }
}
```
- 종료 코드: 0 성공, 1 일반 오류, 2 사용법 오류, 3 권한 부족, 4 충돌(이미 존재 등), 5 외부 연결 실패
- 멱등: `init`을 기존 디렉터리에 다시 실행하면 변경 없이 현재 상태를 반환. `create` 계열은 `--if-not-exists` 지원.

---

## 11. MCP 툴 명세 (초안)

모든 툴 입력은 schemars로 생성한 JSON Schema를 가진다. 쓰기 툴 응답에는 `changeset_id`(해당 시)와 `preview_url`을 포함한다.

| 그룹 | 툴 | 필요 스코프 | changeset |
|---|---|---|---|
| 사이트 | `get_site`, `create_site_from_preset`, `set_site_settings` | read / structure | 쓰기는 예 |
| 구조 | `list_boards`, `create_board`, `update_board`, `create_page`, `update_page`, `update_menu` | read / structure | 쓰기는 예 |
| 콘텐츠 | `list_posts`, `get_post`, `search_posts`, `create_draft`, `update_draft`, `submit_for_review` | read / draft | 아니오 |
| 발행 | `publish_post`, `hide_post`, `delete_post` | publish | 아니오 (감사 로그) |
| 미디어 | `upload_media`, `list_media`, `set_alt_text` | draft | 아니오 |
| 디자인 | `list_themes`, `apply_theme`, `get_tokens`, `set_tokens`, `list_blocks` | read / design | 쓰기는 예 |
| 위젯 | `list_widget_types`, `list_widgets`, `place_widget`, `configure_widget`, `remove_widget` | read / design | 쓰기는 예 |
| 모듈 | `list_modules`, `enable_module`, `disable_module` | read / admin | 쓰기는 예 |
| changeset | `list_changesets`, `get_changeset`, `discard_changeset` (apply는 사람 또는 auto-apply 토큰) | read | — |
| 운영 | `list_pending_comments`, `moderate_comment`, `stats_summary` | read / publish | 아니오 |

- 모듈이 활성화되면 그 모듈의 `tools()`가 자동으로 툴 목록에 추가되고, 비활성화되면 제거된다(툴 목록 변경 알림 지원 시 발송).
- 툴 설명(description)은 한국어+영어 병기, "언제 쓰는지 / 쓰지 말아야 할 때"를 포함한다.

---

## 12. 에이전트 설치 경험

| 구성 | 내용 |
|---|---|
| `AGENTS.md` | 에이전트가 따라 할 설치·초기화·연결·공개 절차, 실패 시 대처, 금지 사항(관리자 권한 요구 금지 등) |
| `llms.txt` | 문서 인덱스와 핵심 명령 요약 |
| Claude 스킬 (선택) | `skills/maru/SKILL.md` — 설치·운영 절차를 스킬로 패키징 |
| 설치 스크립트 | `install.sh`(macOS·Linux), `install.ps1`(Windows). OS·아키텍처 감지 → 릴리스 바이너리 다운로드 → 체크섬·서명 검증 → 사용자 디렉터리에 설치 |
| 릴리스 바이너리 | x86_64·aarch64 × Linux(musl)·macOS·Windows |
| 상시 실행 | `maru service install` — 사용자 권한 서비스 우선 |
| 외부 공개 | `maru expose --cloudflare` — cloudflared 설치 여부 확인, 터널 설정 안내/자동화. 포트포워딩 안내는 하지 않음 |
| 정적 백업 경로 | `maru export static` — 정적 HTML로 내보내 Cloudflare Pages 등에 올릴 수 있게 (PC가 꺼져도 열람 가능) |

---

## 13. 보안

- 비밀번호 argon2id, 세션 쿠키 `HttpOnly; Secure(공개 시); SameSite=Lax`, CSRF 토큰
- 토큰은 생성 시 1회만 표시, DB에는 해시
- 업로드: MIME 스니핑 + 확장자 화이트리스트, 이미지 재인코딩, 크기 제한
- Markdown 렌더 결과 ammonia 정화, 템플릿 자동 이스케이프
- 로그인·댓글·API rate limit (토큰·IP 기준)
- 외부 사용자 콘텐츠는 MCP 응답에서 `untrusted` 분리
- 백업 파일에 토큰·비밀번호 해시 포함 여부 옵션화
- 의존성 `cargo audit`, `cargo deny` CI 적용

---

## 14. 품질 목표 (N100 미니PC 기준)

| 항목 | 목표 |
|---|---|
| 유휴 메모리 | 50MB 이하 |
| 공개 페이지 응답 p95 | 50ms 이하 (캐시 미적중 기준) |
| 바이너리 크기 | 30MB 이하 |
| `init`부터 첫 페이지까지 | 10초 이하 |
| Lighthouse (기본 테마) | 성능·접근성·SEO 각 95 이상 |

---

## 15. 로드맵과 완료 기준

각 Phase는 **실제 사이트를 운영해 검증**한 뒤 다음으로 넘어간다.

### Phase 1 — 코어 뼈대 (3~4주)
- 워크스페이스·CI(fmt, clippy, test), 데이터 디렉터리, 설정
- 마이그레이션: site_settings, boards, posts, revisions, pages, menus, members, roles, tokens, media, audit_log
- 서비스: 게시판·글·페이지·메뉴 CRUD, Markdown 렌더링, 리비전, 감사 로그
- 웹: 공개 사이트(홈, 게시판 목록, 글, 페이지), 사이트맵·RSS·메타, 최소 관리 화면(로그인, 글 목록·작성)
- 기본 테마 1개(`blog` 프리셋), 디자인 토큰 → CSS 변수 출력
- CLI: `init`, `serve`, `doctor`, `board`, `post`, `page`, `menu`, `--json` 계약
- **완료 기준**: `maru init --preset blog --yes` 후 `serve`로 블로그 운영 가능 / CLI로 글 작성·발행 가능 / 모든 쓰기가 audit_log에 남음 / 통합 테스트 통과

### Phase 2 — 모듈·위젯 시스템 (2~3주)
- `Module`·`Widget` 트레이트, 레지스트리, 이벤트 버스, 모듈별 마이그레이션, on/off
- 영역·위젯 인스턴스, 기본 위젯 7종, 블록 기반 페이지
- 샘플 모듈 `point`, 경량 방문자 통계
- **완료 기준**: `point` 모듈을 켰다 끄고 제거해도 코어 데이터 무결 / 위젯을 CLI로 배치·제거 가능

### Phase 3 — 에이전트 레이어 (2~3주)
- 스코프 토큰, changeset(미리보기 렌더링·적용·롤백), MCP 서버(STDIO·HTTP), 모듈 툴 자동 노출
- `mcp install`, `service install`, `expose`, `AGENTS.md`, `llms.txt`, 설치 스크립트, 릴리스 빌드
- **완료 기준**: 깨끗한 PC에서 에이전트에게 "블로그 만들어줘" 한마디로 설치~미리보기~적용까지 사람 개입 없이(승인 제외) 완료 / 잘못된 디자인 변경을 revert 한 번으로 복원

### Phase 4 — 한국형 커뮤니티 (3~4주)
- 회원 가입·로그인·역할, 댓글 승인 큐, 비밀글·공지·분류, 게시판 kind 확장
- 공식 모듈: `social_login`(네이버·카카오·구글), `kakao`, `map`
- 프리셋: `community`, `cafe`, `portfolio`, `org` + 디자인 패턴 조사 기반 테마 4~5종
- **완료 기준**: 프리셋 5종 각각 에이전트 한마디로 구성 가능 / 소셜 로그인 동작 / 테마 점검(§8.7) 통과

### Phase 5 — 외부 모듈 생태계
- WASM 모듈 런타임, 모듈 패키지 포맷, `module install <url>`(서명 검증), 공식 모듈 저장소

---

## 16. 결정된 기본값과 열린 질문

### 결정된 기본값
| 항목 | 결정 | 이유 |
|---|---|---|
| 인스턴스 | 사이트당 1 인스턴스 | 단순함. 멀티 사이트는 검증 후 |
| 템플릿 | minijinja 런타임 | 에이전트의 테마 수정·생성 |
| ID | ULID 문자열 | 정렬 가능, 분산 생성 |
| 콘텐츠 원본 | Markdown | 에이전트 친화, 이식성 |
| 관리 화면 | 서버 렌더링 + htmx 수준의 경량 상호작용 | 단일 바이너리 유지 |

### 열린 질문
- 프로젝트 정식 명칭 (후보: 누리 nuri, 마당 madang, 마루 maru)
- 라이선스 (MIT/Apache-2.0 이중 vs AGPL) — 생태계 확산 vs 호스팅 업체 무임승차 방지
- 관리 화면을 어디까지 만들지 (에이전트 우선이지만 승인·미리보기 UI는 필수)
- 이미지 생성·외부 AI 호출을 코어에 넣을지 (기본은 넣지 않음: 사용자의 에이전트가 담당)
