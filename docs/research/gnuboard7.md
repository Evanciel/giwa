# 그누보드7 소스 분석 — Maru 설계 근거 자료

> 작성 2026-11-02. 분석 대상: 공식 저장소 `https://github.com/gnuboard/g7` `main` 브랜치 HEAD
> `a6abba698ddfe8c1b9c01e0946c17eaeb6659478` (커밋 시각 2026-11-01 21:55 +0900, "Merge branch 'develop'").
> 로컬 사본: `F:\claude-temp\rust-cms\ext\gnuboard7` (`git clone --depth 1`, 실행·설치 안 함).
> 아래 `경로:줄`은 저장소 루트 기준이다. 일부 파일은 작업 트리 체크아웃이 끝나기 전에 `git show HEAD:<경로>`로 읽었다(내용은 같은 커밋).
> 그누보드 코드는 옮기지 않았다. 구조 설명과 짧은 식별자만 쓴다.
> 「못 봤다」 = 이번 분석에서 소스로 확인하지 못했다는 뜻이지, 없다고 단정한다는 뜻이 아니다. 「없다」로 쓴 것은 저장소 전체 검색(`git grep`, 마이그레이션 목록)으로 흔적이 없었던 것이다.

---

## 0. 요약

- 그누보드7은 그누보드5의 개정판이 아니라 **Laravel 12 + React 19로 새로 만든 플랫폼**이다. 게시판조차 코어가 아니라 번들 모듈(`sirsoft-board`)이다.
- 스스로를 「개발자와 에이전트를 위한 클린 오픈소스 CMS」라고 소개한다. 하지만 에이전트 기능은 **그누보드7을 개발·확장하는 코딩 에이전트**를 위한 것이다(AGENTS.md, 개발 스킬, 프론트 상태 디버깅용 MCP). **사이트를 운영하는 에이전트**를 위한 MCP, 스코프 토큰, 승인·롤백 흐름은 없다.
- 실행 환경은 PHP 8.2+, MySQL/MariaDB, Apache/Nginx, Composer가 필수다(Node, Redis는 선택). 단일 바이너리나 SQLite와는 거리가 멀다.
- Maru SPEC §2가 「그누보드에서 버린다」고 판정한 것(게시판별 테이블, 레벨 1~10, 답글 정렬 문자열) 대부분은 **그누보드7도 이미 버렸다**. §2 표의 「그누보드」 열은 사실상 그누보드5 기준이다.

---

## 1. 정체 확인

| 항목 | 확인 결과 | 근거 |
|---|---|---|
| 공식 출처 | GitHub 조직 `gnuboard`의 저장소 `g7`. 공식 사이트 `https://sir.kr/g7`이 이 저장소, AGENTS.md, INSTALL.md를 링크한다 | sir.kr/g7 페이지(WebFetch 2026-11-02), `README.ko.md` 「만든 사람들」 → SIRSOFT(sir.kr) |
| 개발사 | (주)에스아이알소프트 / SIRSOFT | `LICENSE:3`, `LICENSE:17` |
| 라이선스 | **MIT** (한국어 번역문 + 영어 원문, 영어 원문 우선) | `LICENSE:5-40` |
| 첫 공개 | 7.0.0-alpha.1, 2026-03-07 (CHANGELOG 기준) | `CHANGELOG.md:2947` |
| 오픈베타 | 7.0.0-beta.1, 2026-04-01. GeekNews 기사(2026-04-04)도 4월 1일 공개라고 전하며 sir.kr/g7을 출처로 단다 | `CHANGELOG.md:2514`, news.hada.io/topic?id=28172 |
| 정식(Stable) | 7.0.0, 2026-07-01 | `CHANGELOG.md:1880` |
| 현재 버전 | **7.1.4, 2026-11-01** (README 배지, sir.kr 다운로드 버튼과 일치) | `CHANGELOG.md:7`, `README.ko.md` 배지 |
| 백엔드 | PHP ^8.2, Laravel ^12.0, Sanctum ^4.2(Bearer 토큰), Scout ^11.1(검색), Reverb ^1.6(WebSocket) | `composer.json:12-20` |
| DB | MySQL 8.0+ / MariaDB 10.3+ (utf8mb4). `.env.example` 기본값 `DB_CONNECTION=mysql`, 세션·큐·캐시도 DB 드라이버 | `README.ko.md` 요구사항, `.env.example:46,67,76,105` |
| 프론트엔드 | React ^19.2, Vite ^7, Tailwind CSS ^4(다크 모드). SPA | `package.json:40-53` |
| 실행 요건 | 웹서버(문서 루트 `public/`), Composer 2.x, PHP 확장 16개 필수. Node 20+는 에셋을 직접 빌드할 때만, Redis는 선택 | `README.ko.md` 「시스템 요구사항」 |
| 규모 | 1,040커밋, 추적 파일 16,571개(`git ls-tree -r HEAD` 기준) | GitHub 페이지, 로컬 집계 |

보조 출처: sir.kr/g7은 「AI 주도 개발을 위한 코어 솔루션」을 표어로 쓰고, AI/데이터 파트너 모집을 한다. 이 페이지에는 MCP 언급이 없다. sir.kr 안의 별도 출시 공지 게시물은 검색으로 찾지 못했다.

---

## 2. 아키텍처

### 2.1 디렉터리

| 경로 | 역할 |
|---|---|
| `app/` | 코어(Laravel). `Extension/`(모듈·플러그인·템플릿 관리자, 훅), `Seo/`, `Analytics`·`Privacy`·`ActivityLog`, `Services/`, `Repositories/` |
| `modules/_bundled/` | 번들 모듈 원본: `sirsoft-board`, `sirsoft-ecommerce`, `sirsoft-page`, `gnuboard7-hello_module` |
| `plugins/_bundled/` | 번들 플러그인 원본 14개(결제 4, 본인확인 2, TOTP, 우편번호, 마케팅, 환율, CKEditor5, GDPR, 비즈뿌리오, hello) |
| `templates/_bundled/` | `sirsoft-admin_basic`, `sirsoft-basic`, hello 템플릿 2종 |
| `lang-packs/` | 번들 언어팩(일본어 등) |
| `resources/js/core/` | 템플릿 렌더링 엔진(React) |
| `public/install/` | 웹·CLI 설치기 |
| `docs/` | 백엔드 41, 프론트 47, 확장 37개 문서(AGENTS.md 문서 지도 기준) |

확장은 `_bundled/`(Git 추적 원본), `_pending/`(다운로드 대기), `vendor-name/`(활성 설치본) 세 단계로 관리한다(`AGENTS.md` 「기술 스택·구조」). 확장은 `/modules`, `/plugins` 디렉터리 스캔으로 찾고 composer.json에 하드코딩하지 않는다(`AGENTS.md` 핵심 원칙 1).

### 2.2 요청 처리 흐름

- **웹 라우트는 SPA 셸**이다. `/admin/{any?}`와 사용자 `/{any?}` catch-all이 같은 HTML 셸을 돌려주고, 등록되지 않은 경로는 셸 본문에 HTTP 404를 붙인다(`routes/web.php:28-41`, `routes/web.php:54-70`).
- 사용자 라우트 미들웨어 순서는 `canonical.host → canonical.path → template.dependencies:user → analytics.bots → seo → analytics.fallback`이다(`routes/web.php:54`). 봇이면 서버가 만든 정적 HTML을, 사람이면 SPA를 준다(README 「SEO」, `app/Seo/BotDetectorCustomProvider.php:13`, jaybizzle/crawler-detect 기반).
- **실제 기능은 모두 REST API**다. 코어 `routes/api.php`(1,155줄)는 `auth`, `user`, `me`, `identity`, `admin/*`(settings, menus, roles, permissions, modules, plugins, templates, analytics 등) 그룹으로 나뉜다(`routes/api.php:133-1084` prefix 목록). 게시판 API는 모듈 안 `modules/_bundled/sirsoft-board/src/routes/api.php`(Route 127개)에 있고 `/api/modules/sirsoft-board/...`로 노출된다(홈 레이아웃의 data-source endpoint로 확인: `templates/_bundled/sirsoft-basic/layouts/home.g7.html:47`).
- 백엔드 계층 규칙은 `Controller → FormRequest → Service → RepositoryInterface → Repository → Model`이다. 검증은 FormRequest, 권한은 `permission` 미들웨어가 맡는다(`AGENTS.md` 핵심 원칙 3, 백엔드 절대 금지 목록).
- 화면은 `.g7.html` 마크업으로 쓰고, 설치할 때 JSON 레이아웃(「런타임 계약」)으로 컴파일해 DB `template_layouts.content`에 저장한다(`database/migrations/2026_04_01_000021_create_template_layouts_table.php:19`). 서버는 상속(`extends`), partial, 모듈 주입(`layout_extensions`)을 병합하고 **권한이 없는 컴포넌트를 제거한 JSON**을 내려준다. 프론트 `DynamicRenderer`는 이 JSON을 해석해 데이터 소스(API)를 호출하고 React 컴포넌트로 렌더링한다(README 「템플릿 엔진 동작 흐름」).

### 2.3 그누보드5 대비 바뀐 점

| 영역 | 그누보드5 | 그누보드7 |
|---|---|---|
| 기반 | 순수 PHP, 서버 렌더링, 스킨 PHP 파일 | Laravel + React SPA, 봇에만 서버 HTML |
| 게시판 | 코어 기능, `g5_write_{bo_table}` 게시판별 테이블 | 번들 모듈, `board_posts` 단일 테이블 + `board_id` |
| 회원 권한 | `mb_level` 1~10 | 역할 + 권한 + 스코프, `guest`도 역할 |
| 화면 | 테마/스킨 PHP | 선언형 레이아웃 JSON, DB 저장, 버전 관리, 위지윅 편집기 |
| 확장 | extend 파일, 훅 일부 | 모듈·플러그인·템플릿·언어팩 매니저, 의존성·버전 제약, Action/Filter 훅 |
| 쇼핑몰 | 영카트(별도 배포) | 번들 모듈 `sirsoft-ecommerce` |
| 다국어 | 사실상 없음 | 모든 이름 필드가 다국어 JSON, 언어팩 시스템 |
| 설정 | `g5_config` 테이블 | 카테고리별 JSON 파일(§3.5) |

---

## 3. 데이터 모델

### 3.1 게시판 (`sirsoft-board` 모듈)

| 테이블 | 핵심 내용 | 근거 |
|---|---|---|
| `board_types` | 게시판 유형 사전(`slug`: basic, card, gallery …, `name` 다국어) | `modules/_bundled/sirsoft-board/database/migrations/2026_04_01_000001_create_board_types_table.php:16-19` |
| `boards` | `slug` unique(50자), `name`·`description` 다국어 JSON, `type`, `categories`(배열), `secret_mode`(disabled/enabled/always), `use_comment`, `use_reply`, `max_reply_depth`(기본 5), `max_comment_depth`(기본 10), 제목·본문·댓글 길이 제한, 파일 업로드 설정(`max_file_size`, `max_file_count`, `allowed_extensions`), `blocked_keywords`, 알림 설정, `per_page`·`per_page_mobile` | `…/2026_04_01_000002_create_boards_table.php:17-57` |
| `board_posts` | **단일 테이블**, PK `(id, board_id)`. `category`, `title`, `content`(longText), `content_mode`(text/html), `user_id` nullable, 비회원용 `author_name`·`password`(bcrypt), `ip_address`, `is_notice`, `is_secret`, `status`(published/blinded/deleted), `trigger_type`(report/admin/system/auto_hide/user), `action_logs`(JSON 이력), `view_count`, `parent_id`, `depth`, soft delete | `…/2026_04_01_000004_create_board_posts_table.php:21-55` |
| `board_comments` | 글과 **별도 테이블**. `post_id`, `parent_id`, `depth`, 비회원 `author_name`·`password`, `is_secret`, `status`, `trigger_type`, `action_logs`, soft delete | `…/2026_04_01_000005_create_board_comments_table.php:22-47` |
| `board_attachments`, `board_post_scraps`, `board_reactions`, `board_reaction_types`, `boards_reports`, `boards_report_logs`, `board_stats`, `board_user_notification_settings` | 첨부, 스크랩, 반응(추천), 신고, 통계, 알림 설정 | 마이그레이션 파일 목록 |

- **게시판별 테이블 구조는 유지하지 않았다.** 처음에는 `board_posts`, `board_comments`, `board_attachments`를 MySQL `PARTITION BY LIST (board_id)`로 만들었다(`…create_board_posts_table.php:58`, `…create_board_comments_table.php:50`). 2주 뒤 `remove_partitions_from_board_tables` 마이그레이션으로 파티션을 모두 걷어냈다(`…/2026_04_17_000001_remove_partitions_from_board_tables.php:20-26`). 지금은 평범한 단일 테이블에 `(board_id, created_at)`, `(board_id, status)`, `(board_id, is_notice)` 인덱스를 둔 구조다. 모듈 인터페이스의 `getDynamicTables()`(`app/Contracts/Extension/ModuleInterface.php:44-56`)와 반응 테이블 주석의 「동적 테이블 ID」는 이전 설계의 흔적으로 보인다.
- **여분필드(`wr_1~10`)와 그 대체물은 없다.** board 모듈 `database`·`src` 전체에서 `extra_field`, `custom_field`, `wr_1` 검색 결과가 없다. 게시판별 사용자 정의 필드 기능은 못 봤다.
- **게시판 그룹은 없다.** `board_group`, `group_id` 검색 결과가 없다.
- **답글·스레드**: 글 답변과 댓글 모두 `parent_id` + `depth`만 쓰고, 정렬용 경로 문자열은 없다. 답글 트리는 애플리케이션이 **레벨별로 반복 쿼리**해서 모은다(`modules/_bundled/sirsoft-board/src/Repositories/PostRepository.php:1084-1152`, 「모든 하위 답글 조회」 후 `parent_id`로 그룹화).
- 비회원 글쓰기는 유지했다(`author_name` + 해시 비밀번호).
- 카운터 컬럼이 비정규화되어 있다(`add_count_columns…`, `add_reaction_counts…`, `add_scrap_count…` 마이그레이션).

### 3.2 회원·권한 (코어)

| 테이블 | 핵심 내용 | 근거 |
|---|---|---|
| `users` | `name`, `nickname`, `email` unique, `password`, `language`, `is_super`, `timezone`, `country`, `status`(active/inactive/blocked/withdrawn), 연락처·주소, `signature`, `bio`, `admin_memo`. **레벨 컬럼 없음** | `database/migrations/2026_04_01_000000_create_users_table.php:16-41` |
| `roles` | `identifier` unique, 다국어 `name`, `extension_type`·`extension_identifier`(소유 확장), `user_overrides`, `is_active` | `…000011_create_roles_table.php:16-24` |
| `permissions` | 계층형(`parent_id`), `identifier` unique, `type`(admin/user), `resource_route_key`, `owner_key`(소유자 판정 컬럼), 소유 확장 | `…000009_create_permissions_table.php:16-32` |
| `role_permissions` | `scope_type` = null(전체) / self(본인) / role(소유 역할) | `…000012_create_role_permissions_table.php:19` |
| `user_roles` | 다대다 | `…000016_create_user_roles_table.php:16-23` |
| `permission_hooks` | 권한과 훅 이름 매핑 | `…000010_create_permission_hooks_table.php:16-21` |
| `personal_access_tokens` | Sanctum 토큰(`abilities` 텍스트) | `…000008_create_personal_access_tokens_table.php:16-23` |

### 3.3 페이지·메뉴

- `sirsoft-page` 모듈: `pages`(slug, 다국어 `title`·`content`, `content_mode` 기본 html, `published`, `seo_meta`, `current_version`) + `page_versions`(스냅샷, `changes_summary`) (`modules/_bundled/sirsoft-page/database/migrations/2026_04_01_000001_create_pages_table.php:16-33`, `…000002_create_page_versions_table.php:16-30`).
- 코어 `menus` 테이블은 **관리자 메뉴**다. 테이블 주석이 「시스템 메뉴 관리」이고, `role_menus`·`menu_permissions`와 묶이며, 모듈은 `getAdminMenus()`로 항목을 등록한다(`database/migrations/2026_04_01_000013_create_menus_table.php:16-38`, `ModuleInterface.php:169`).
- **사용자 사이트 메뉴 관리 기능은 못 봤다.** 기본 사용자 템플릿 헤더는 `/api/modules/sirsoft-board/boards/board-menu` API로 게시판 목록을 받아 메뉴를 그린다(`templates/_bundled/sirsoft-basic/layouts/_user_base.g7.html:15`). 그 밖의 항목은 레이아웃 편집으로 바꾸는 구조로 보인다.

### 3.4 화면·확장 관리

`templates`, `template_layouts`(JSON, `extends`, `source_type` template/module/plugin, soft delete), `template_layout_versions`(자동 증가 버전, 스냅샷, `changes_summary`), `template_layout_previews`(UUID `token`, 편집 중 JSON, `admin_id`, `expires_at`), `template_layout_extensions`(+versions), `modules`·`plugins`(status: active/inactive/installing/uninstalling/updating, `config`, 업데이트 출처)
(`…000021`, `…000022`, `…000658_create_template_layout_previews_table.php:15-25`, `…000018_create_modules_table.php:16-32`).

### 3.5 설정

설정은 DB가 아니라 **카테고리별 JSON 파일**이다. `JsonConfigRepository`가 `settings` 디스크에 `{category}.json`을 읽고 쓰며, 기본값은 `config/settings/defaults.json`에 있다(`app/Repositories/JsonConfigRepository.php:22,32,552-555`). 부팅할 때 `g7_settings.core.*` Config로 올리고 `g7_core_settings()` 헬퍼로 읽는다(`app/Helpers/settings_helpers.php:56-63`). 백업·복원은 이 JSON을 ZIP으로 묶는다(`JsonConfigRepository.php:381-423`).

### 3.6 공통 패턴

- 이름·설명 필드는 거의 모두 **다국어 JSON**이다(`{"ko": …, "en": …}`).
- `user_overrides`: 시더나 확장 업데이트가 덮어쓰면 안 되는, 운영자가 직접 고친 필드 목록이다(`roles`, `menus`, `board_reaction_types`, `notification_definitions`, `schedules` 등). 확장을 다시 시딩해도 운영자 편집은 남는다(README 「언어팩 시스템」 사용자 수정 보존).
- 활동 로그 `activity_logs`: `log_type`(admin/user/system), `loggable` morph, `action`, `properties`(old/new) (`…000027_create_activity_logs_table.php:15-24`).

---

## 4. 권한

- **구조**: 역할 → 권한(identifier 문자열) → 스코프(self/role/전체) 3단계다(`README.ko.md` 「권한 제어」, `role_permissions.scope_type`).
- **코어 역할 4개**: `admin`(모든 리프 권한), `manager`(회원·메뉴·대시보드 일부, 다수가 `self` 스코프), `user`, `guest`. `guest`는 실제 역할 행이고 관리자가 권한을 줄 수 있다(`config/core.php:626-690`).
- **권한 판정**: 로그인 사용자는 Laravel Gate로, 비회원은 `guest` 역할의 권한을 직접 확인한다(`app/Providers/AuthServiceProvider.php:46-58`). OR/AND 중첩 구조 판정도 지원한다(`app/Helpers/PermissionHelper.php:65`).
- **게시판별 권한**: 게시판을 만들 때 `sirsoft-board.{slug}.{key}` 형식의 권한 행을 **동적으로 생성**한다(`modules/_bundled/sirsoft-board/config/board.php:14-112`, `BoardPermissionService::ensureBoardPermissions` `src/Services/BoardPermissionService.php:83`).
  - 사용자 키: `posts.read`, `posts.write`, `posts.read-secret`, `posts.write-notice`, `comments.read`, `comments.write`, `attachments.upload`, `attachments.download`, `manager`
  - 관리자 키: `admin.posts.read`, `admin.posts.write`, `admin.posts.read-secret`, `admin.comments.read`, `admin.comments.write`, `admin.attachments.upload`, `admin.attachments.download`, `admin.manage`
  - 기본 매핑: 읽기·댓글 읽기·다운로드 = admin+user+guest, 쓰기·업로드 = admin+user, 비밀글 열람·공지·관리 = admin (`modules/_bundled/sirsoft-board/config/settings/defaults.json:53-71`)
  - 게시판마다 `sirsoft-board.{slug}.manager`(게시판 관리자), `sirsoft-board.{slug}.step`(스텝) 역할이 생긴다. 스텝은 `admin.manage`·`manager`를 뺀 권한을 받는다(`BoardPermissionService.php:384-410`).
  - 소유자 판정은 권한 정의의 `scope.owner_key`(예: `user_id`, `created_by`)로 한다(`config/board.php:32,74`).
- **비밀글**: 게시판 `secret_mode`(사용 안 함/사용/항상)와 글 `is_secret`. 열람은 작성자 본인 또는 `posts.read-secret` 권한자만 할 수 있다(`src/Support/SecretContentGate.php`, `AttachmentService.php:447` 주석). 비밀 답글 노출 정책 컬럼은 `add_secret_reply_visibility_to_boards_table`에서 추가됐다.
- **공지**: `is_notice` + `posts.write-notice` 권한.
- **댓글 승인 대기는 없다.** 댓글 `status`에 pending이 없고 board 모듈에 승인 로직 흔적이 없다(「approv/승인」 검색 결과는 신고 상태 enum 1건뿐). 대신 신고 누적 자동 블라인드(`report_policy.auto_hide_threshold` 기본 5), 금지어(`BlockedKeywordsRule`), 작성 쿨다운(`spam_security`)이 있다(`defaults.json:73-90`).
- **API 토큰 스코프는 없다.** 로그인할 때 발급하는 Sanctum 토큰의 abilities가 항상 `['*']`이다(`app/Services/AuthService.php:840,1001,1105`). 사용자가 범위를 좁힌 자동화용 토큰을 발급하는 화면이나 API는 못 봤다.

---

## 5. 테마·스킨·위젯·페이지·메뉴

- **템플릿** = 관리자용(`sirsoft-admin_basic`)과 사용자용(`sirsoft-basic`). 사용자 템플릿 매니페스트에는 컴포넌트 목록, 에셋, 선택 연동(integrations: board, ecommerce, page, daum_postcode)이 들어 있다(`templates/_bundled/sirsoft-basic/template.json`).
- **라우트**는 템플릿의 `routes.json`이 정의한다. 게시판 경로는 `/boards`, `/board/:slug`, `/board/:slug/:id`, 페이지는 `/page/:slug`, 쇼핑은 `/{shop}/…`, 마이페이지는 `/mypage/*`다. 라우트별 `requires`(modules/plugins)를 적으면 그 확장이 꺼졌을 때 라우트를 숨긴다(`templates/_bundled/sirsoft-basic/routes.json:7-454`, `CHANGELOG.md` 7.1.4).
- **최신글 위젯 함수(`latest()`)는 없다.** 홈 레이아웃이 `recent_posts` 데이터 소스로 `/api/modules/sirsoft-board/boards/posts/recent?limit=5`를 직접 호출하고 마크업으로 그린다(`templates/_bundled/sirsoft-basic/layouts/home.g7.html:47`). 인기 게시판, 통계, 방문자 요약, 인기·신상품도 같은 방식이다(`home.g7.html:22-113`).
- 그누보드7에서 「위젯」은 **레이아웃 편집기의 속성 컨트롤**(image, classToken, cssVar 등)을 뜻한다(`docs/rules/editor-widgets.md`). Maru의 「배치 가능한 콘텐츠 위젯」과는 다른 개념이다.
- **위지윅 레이아웃 편집기**는 7.0.0에서 도입됐다. 캔버스 실렌더링, 반응형 폭 전환, 샘플 데이터 자동 채움, 실행 취소, 버전 기록·복구, 저장 전 미리보기를 지원한다(`CHANGELOG.md:1880-1984`).
- **디자인 토큰 체계는 못 봤다.** 스타일은 Tailwind 클래스이고, 편집기가 `classToken`·`cssVar`로 클래스와 CSS 변수를 바꾸는 수준이다. 「브랜드 색 하나를 바꾸면 전체가 바뀌는」 토큰 파일은 확인하지 못했다.
- 모바일 전용 템플릿은 없다(반응형). `per_page_mobile` 같은 값만 남아 있다.

---

## 6. 확장 구조와 API

- **세 가지 확장 + 언어팩**: 모듈(도메인 기능), 플러그인(결제·인증 같은 연동), 템플릿(UI), 언어팩. 확장마다 manifest(`module.json` 등)와 PHP 클래스(`module.php`)를 둔다. 예: board의 `module.json`에는 `identifier`, `version` 1.2.2, `license` MIT, `g7_version >=7.1.1`, `dependencies`, `integrations`, `assets`, `loading`이 들어 있다.
- **모듈 인터페이스가 선언하는 것**: install/uninstall/activate/deactivate, upgrades, routes, migrations, dependencies, integrations(+버전 제약), permissions, roles, config, admin menus, hook listeners, schedules, seeders, layout extensions, settings schema(`app/Contracts/Extension/ModuleInterface.php:14-300`). 모듈은 권한·역할·메뉴·스케줄·본인확인 정책·알림 정의를 **선언만 하고**, 설치·활성화 때 코어가 DB에 반영한다.
- **훅**: `HookManager`의 Action(`doAction`, 부수 작업)과 Filter(`applyFilters`, 값 변형). 우선순위가 있고, 권한 연동판(`doActionWithPermission`)과 브로드캐스트를 지원한다(`app/Extension/HookManager.php:44,94,125,154,334,376`). 확장의 `Listeners/` 클래스가 `getSubscribedHooks()`로 구독하면 `HookListenerRegistrar`가 자동 등록한다(`app/Extension/HookListenerRegistrar.php:55`). 큐를 통한 비동기 실행과 컨텍스트 복원도 지원한다(README 「훅 시스템」). 훅은 서비스 계층에서 발행하는 것이 규칙이다.
- **확장 미들웨어**는 `getMiddleware()`에 부착 대상을 명시한다(board: `EnsureBoardExists`, `modules/_bundled/sirsoft-board/module.php:118-140`).
- **레이아웃 확장점**: 모듈·플러그인이 `extension_point`로 다른 화면에 UI를 주입한다. 관리자 화면에서 버전 관리하며 편집할 수 있다.
- **설치 경로**: ZIP 업로드, GitHub URL, 번들. `--with-dependencies`, `--with-language-packs` 옵션이 있다(CHANGELOG 7.1.4). 언어팩은 PHP 실행 코드가 들어 있으면 설치를 막는다(README). **모듈·플러그인 코드는 샌드박스 없이 PHP로 실행된다.**
- **API**: 모든 기능이 REST(`/api/...`)로 노출된다. 문서는 Markdown 레퍼런스(`docs/backend/api/*.md`)이고 Swagger/OpenAPI는 쓰지 않는다(`docs/backend/api-documentation.md:15`). 인증은 Sanctum Bearer 토큰이다.
- **CLI(artisan)**:
  - 설치 `php g7-install` / `core:install`: 대화형과 비대화형(`--no-interaction`, 환경변수, `--config`), `--output=json` 줄 단위 진행 출력, `--skip-if-installed`, `--resume`. 종료 코드는 0 완료, 1 실패, 2 입력 오류, 3 이미 설치됨, 4 요구사항 미충족, 5 충돌, 6 중단, 7 root 실행 거부다(`public/install/includes/cli/app.php:20-30`). 비밀번호·토큰은 명령줄 인자로 받지 않는다.
  - 운영: `settings:get/set`, `user:reset-password`, `module:install/activate`, `plugin:*`, `template:*`, `core:update`, `layout:check-installed --json` 등 명령 클래스 118개. `--json`을 지원하는 명령은 9개다(`app/Console/Commands` 집계).
  - **글·게시판을 만드는 CLI는 못 봤다.** board 모듈 명령은 통계 집계, 첨부 정리, Playwright 시드뿐이다(`modules/_bundled/sirsoft-board/src/Console/Commands/`).

---

## 7. 한국형 기본 기능 대조

| 기능 | 그누보드7 | 위치 |
|---|---|---|
| 추천/비추천 | ✅ 「반응」으로 일반화(유형을 관리자가 정의, 아이콘, 글·댓글 각각 사용 여부) | board `board_reaction_types`, `board_reactions` |
| 스크랩 | ✅ | board `board_post_scraps`, `/mypage/scrap` |
| 신고·블라인드 | ✅ 자동 블라인드 임계치, 일일 신고 한도 | board `boards_reports`, `defaults.json:73-84` |
| 비회원 글쓰기 | ✅ 이름 + 해시 비밀번호 | `board_posts.author_name/password` |
| 비밀글·공지·분류·답변글 | ✅ | `boards`, `board_posts` |
| 포인트 | ❌ 커뮤니티 포인트는 없다. 쇼핑몰 마일리지만 있다 | ecommerce `mileage_transactions`, `mileage_balances` |
| 쪽지 | ❌ 테이블·기능 흔적 없음 | — |
| 투표 | ❌ 흔적 없음 | — |
| 팝업(레이어) | ❌ 흔적 없음(「popup」 검색 결과는 결제창·인증 팝업뿐) | — |
| 방문자 통계 | ✅ 코어에 본격 분석 기능. 방문, 액션, 전환, 봇 방문, 시간·일별 집계, 솔트, IP 마스킹·익명 모드 | `database/migrations/2026_09_16_000002_create_analytics_visits_table.php:25-46` 외 analytics_* 11개 |
| 소셜 로그인 | ❌ 번들 없음(Socialite·카카오·네이버 OAuth 흔적 없음) | — |
| 본인확인 | ✅ 코어 정책 엔진(HTTP 428 → 인증 모달 → 원 요청 재실행) + KG이니시스·NHN KCP 플러그인 | README 「본인인증」, `plugins/_bundled/sirsoft-verification_*` |
| 2단계 인증 | ✅ TOTP 플러그인 | `plugins/_bundled/sirsoft-totp` |
| 알림 | ✅ 메일, DB, 실시간(Reverb). 정의 × 템플릿 × 수신자 | `notification_*` 테이블 |
| 문자·알림톡 | ✅ 비즈뿌리오 플러그인 | `plugins/_bundled/sirsoft-message_bizppurio` |
| 결제(PG) | ✅ 이니시스, 나이스, KCP, 토스 | 결제 플러그인 4종 |
| 우편번호 | ✅ 다음 우편번호 | `sirsoft-daum_postcode` |
| 개인정보 보유기간 파기·휴면 | ✅ 코어 배치 + 도메인별 핸들러 | README 「개인정보 보유기간 처리」 |
| 쇼핑몰 | ✅ 번들 모듈(주문, 클레임, 쿠폰, 마일리지, 리뷰, 문의, 현금영수증·세금계산서) | ecommerce 마이그레이션 60여 개 |
| 캡차 | ❌ 흔적 없음 | — |
| 댓글 승인 큐 | ❌ | §4 |
| 그누보드5 데이터 이전 | 「마이그레이션 툴 제공 예정」 | `README.ko.md` 「기존 그누보드 사용자」 |

---

## 8. 보안 (소스에서 보인 것만)

- **비밀번호 해시**: `config/hashing.php`가 없어 Laravel 기본값(bcrypt)을 쓴다. `.env.example`은 `BCRYPT_ROUNDS=12`(`.env.example:37`). 비회원 글 비밀번호도 bcrypt다(`…create_board_posts_table.php:33` 주석). argon2 설정은 못 봤다.
- **비밀번호 정책**: 만료, 이력(`user_password_histories`), 역할별 만료 정책, 로그인 시도 제한·잠금(`add_login_attempt_columns_to_users_table`, `add_locked_permanently_to_users_table`) 마이그레이션이 있다.
- **CSRF**: Sanctum의 CSRF 미들웨어를 `NullMiddleware`로 바꿔 **꺼 두었다**. 주석에 따르면 SPA가 CSRF 토큰을 보내지 않고 API 인증은 Bearer 토큰이라 필요 없다는 판단이다(`config/sanctum.php:81`, `app/Http/Middleware/NullMiddleware.php:11-17`).
- **토큰 저장 위치**: 프론트가 인증 토큰을 **`localStorage`에 저장**한다(`resources/js/core/api/ApiClient.ts:136,145`). XSS가 하나라도 뚫리면 토큰이 탈취될 수 있는 구조다. 7.1.4에서 선택형 CSP(기본 「끔」)를 추가했다(`CHANGELOG.md` 7.1.4 「콘텐츠 보안 정책」).
- **XSS 정화**: 게시글 HTML은 **원본 그대로 저장**하고, 사용자 화면이 렌더링할 때 **DOMPurify(차단 목록 방식)**로 정화한다(`templates/_bundled/sirsoft-basic/src/components/composite/HtmlContent.tsx:4,112-131`). 봇용 서버 HTML은 DOMDocument 기반 `HtmlSanitizer`를 거친다(`app/Seo/HtmlSanitizer.php:21,93`). 저장 시점의 서버 측 HTML 정화(HTMLPurifier 등)는 못 봤다. board 서버 코드는 발췌문을 만들 때 `strip_tags`만 쓴다.
- **업로드**: 게시판별 확장자 허용 목록 + 파일 내용 검사 규칙 `AttachmentFileContent`(`modules/_bundled/sirsoft-board/src/Http/Requests/Concerns/ResolvesAllowedExtensions.php:57-59`). 저장 파일명은 UUID다(`src/Services/AttachmentService.php:136`). 다운로드는 해시 URL을 거치고 권한(`attachments.download`)을 검사한다.
- **기타**: 로그인·본인확인 경로별 rate limit(`routes/api.php:300-333`), 설치 후 인스톨러 차단(HTTP 410, README), 의존성 감사 명령 `SecurityAuditDependenciesCommand`, 언어팩의 PHP 코드 차단.

---

## 9. AI·에이전트 관련 기능

| 항목 | 내용 | 대상 |
|---|---|---|
| `AGENTS.md` | 167줄 진입 문서. 문서 지도, 핵심 원칙, 금지 패턴, **파일 유형별 필수 참조 문서 표(자동 생성)**. 「점진적 공개」로 필요한 문서만 읽게 해 토큰을 아낀다 | 그누보드7을 **개발**하는 코딩 에이전트 |
| 확장별 `AGENTS.md` | 모듈·템플릿마다 있다(board, sirsoft-basic 등) | 같음 |
| 개발 스킬 4종 | `/validate`, `/run-tests`, `/create-extension`, `/i18n` (`docs/ai-tools/skills/`) | 같음 |
| DevTools MCP 서버 | 브라우저 템플릿 엔진의 상태·액션·캐시·네트워크 덤프를 `storage/debug-dump/`에 저장하고 MCP 도구 30여 개(`g7-state`, `g7-diagnose` 등)로 조회한다. 디버그 모드와 Laravel Boost(dev 의존성)가 필요하다 (`docs/ai-tools/devtools/README.md`, `docs/ai-tools/agents/src/mcp/g7-devtools-server.ts`) | 프론트 **디버깅** |
| 멀티에이전트 오케스트레이터 | 2026-09-10 제거 (`docs/ai-tools/README.md`) | — |
| 운영용 MCP (글 쓰기, 게시판 만들기 등) | **없다.** `app`, `config`에서 MCP·LLM 관련 런타임 코드는 주석과 업데이트 보호 경로뿐이다 | — |
| 에이전트 쓰기 승인·미리보기·롤백 | **없다.** 레이아웃 미리보기 토큰과 버전 복구는 사람이 편집기에서 쓰는 기능이다 | — |
| AI 콘텐츠 생성 | 못 봤다 | — |

정리하면, 그누보드7의 「에이전트 친화」는 **코드베이스가 에이전트가 읽고 고치기 좋게 정리되어 있다**는 뜻이다. 비대화형 설치 CLI와 JSON 출력, `settings:get/set` 같은 운영 CLI는 에이전트가 설치·설정을 자동화하는 데 쓸 수 있다.

---

## 10. Maru 대조

### 10.1 SPEC §2 표 재판정

판정 열: **맞음** = §2의 서술이 그누보드7에도 맞다 / **틀림** = 그누보드7과 다르다 / **빠짐** = §2 표에 행이 없다.

| §2 행 | §2 판정 | 그누보드7 실제 | 재판정 |
|---|---|---|---|
| 게시판 = 컬렉션, 스킨으로 재활용 | 개선해서 가져옴 → `boards.kind` + 뷰 템플릿 | `boards.type` + `board_types` 사전(basic/card/gallery)으로 같은 방향 | 맞음. 그누보드7이 같은 개선을 이미 했다 |
| 게시판별 테이블 `g5_write_{bo_table}` | 버림 → 단일 `posts` | 그누보드7도 버렸다. 파티션 실험 후 단순 단일 테이블(§3.1) | 맞음(그누보드5 기준). 차별점은 아니다 |
| 여분필드 `wr_1~10` | 개선 → `field_schema` + `meta` | 그누보드7은 여분필드를 **없애고 대체물도 없다** | 틀림(「그누보드」를 7로 읽으면). Maru의 `field_schema`가 오히려 7보다 앞선다 |
| 회원 레벨 1~10 | 개선 → 역할 + 액션 권한 | 역할 + 권한 + **스코프(self/role)** + 게시판별 동적 권한·게시판 관리자/스텝 역할 + guest 역할 | 맞음(방향). 다만 7이 더 정교하다. Maru에는 소유 스코프와 게시판 단위 관리자가 없다 |
| 답글 정렬 문자열 | 버림 → `parent_id` + `thread_path` | 7은 `parent_id` + `depth`, 트리는 레벨별 반복 쿼리 | 맞음. `thread_path`가 SQL 정렬 면에서 낫다 |
| HTML 에디터 저장 | 버림 → Markdown | 7은 **HTML 저장 유지**(`content_mode` text/html, CKEditor5 플러그인) + 렌더 시 DOMPurify | 틀림(7은 버리지 않았다). Maru 고유 선택으로 남는다 |
| 내용관리, 메뉴관리 | 가져옴 → `pages`, `menus` | 페이지는 모듈(다국어, 버전 이력). 코어 `menus`는 **관리자 메뉴**이고 사용자 사이트 메뉴 관리자는 못 봤다 | 일부 틀림. 「그누보드 메뉴관리」를 7에서 찾으면 관리자 메뉴다 |
| 최신글 `latest()` | 개선 → 기본 위젯 | 7에는 `latest()` 함수도 위젯도 없다. 레이아웃이 API를 직접 호출한다 | 틀림(7 기준 대응물이 다르다) |
| 테마·스킨 | 개선 → 테마 + 영역 + 위젯 + 블록 + 토큰 | 7은 템플릿 + JSON 레이아웃(DB, 버전) + 위지윅 편집기 + 모듈 주입 확장점. 토큰 체계는 못 봤다 | 맞음(개선 방향은 서로 다르다). 7의 강점은 편집기, Maru의 강점은 토큰·프리셋 |
| extend·훅·플러그인 | 개선 → 모듈 시스템 | 7은 모듈·플러그인·템플릿·언어팩 매니저, 의존성·버전 제약, Action/Filter 훅 | 맞음. 다만 7은 이미 성숙한 모듈 시스템이다 |
| 포인트·쪽지·스크랩·추천·투표·팝업 | 모듈 | 스크랩·추천(반응) ✅, 포인트·쪽지·투표·팝업 ❌ | 부분 틀림. 7은 넷을 아직 안 갖췄다 |
| 소셜 로그인 | 모듈 | 번들 없음 | 맞음(모듈 판정). 7 대비 차별 기회 |
| 적응형 모바일 별도 템플릿 | 버림 | 7도 반응형만 | 맞음 |
| 영카트·SMS·본인확인 | 범위 밖 | 7은 셋 다 번들(ecommerce 모듈, 비즈뿌리오, IDV 코어 + 플러그인) | 맞음(Maru 범위 판정으로는). 단 7은 이 영역에서 압도적이다 |
| (없음) 다국어·언어팩 | — | 코어 전체 다국어 JSON + 언어팩 | 빠짐 |
| (없음) 신고·블라인드·금지어·쿨다운 | — | board 모듈 기본 | 빠짐 |
| (없음) 비회원 글쓰기 | — | 유지 | 빠짐 |
| (없음) 알림(메일·DB·실시간) | — | 코어 | 빠짐 |
| (없음) 활동 로그 | — | 코어 `activity_logs` (Maru `audit_log`에 대응) | 빠짐 |
| (없음) 방문자 통계 | — | 코어 본격 분석 | 빠짐(SPEC §9에는 있다) |
| (없음) 봇용 서버 HTML·사이트맵 | — | 코어 SEO | 빠짐(Maru는 서버 렌더링이라 원래 해결됨) |
| (없음) 개인정보 보유기간·휴면 | — | 코어 | 빠짐 |
| (없음) 2단계 인증 | — | TOTP 플러그인 | 빠짐 |
| (없음) 비대화형 설치 CLI | — | `g7-install --no-interaction --output=json`, 종료 코드 0~7 | 빠짐(Maru §10과 직접 비교 대상) |

**권고**: §2 제목을 「그누보드5에서 가져온 것과 바꾼 것」으로 바꾸거나, 「그누보드7 대응」 열을 추가한다. 지금 표를 그대로 두면 「그누보드는 게시판별 테이블·레벨 1~10을 쓴다」는 읽힘이 2026년 기준으로 틀린다.

### 10.2 Phase별 시사점

**Phase 1 — 코어 뼈대**: 아래 §10.3에 따로 정리했다.

**Phase 2 — 모듈·위젯**
- manifest가 선언할 것: 7의 `ModuleInterface`에 있는 permissions, roles, admin menus, schedules, settings schema, 의존성 + **선택 연동(integrations, 버전 제약)**. Maru `module.toml`의 `[provides]`에 `requires`(필수)와 `integrations`(선택)를 나눠 두면 「게시판만 켠 사이트」 같은 조합을 깨지 않고 다룰 수 있다.
- 라우트 `requires`: 확장이 꺼지면 라우트·사이트맵·검색봇 화면에서 함께 빠진다. SPEC §7.3의 「비활성 모듈 위젯은 조용히 제외」를 라우트와 사이트맵까지 넓힌다.
- Action/Filter 구분: Maru 이벤트(§7.4)는 알림형(Action)뿐이다. 값 변형 훅(Filter)은 강력하지만 코어 불변식을 깨기 쉽다. 넣는다면 서비스 레이어가 정한 소수의 지점에만 둔다.
- 비활성화할 때 모듈 격리 캐시를 일괄 비우는 것(7 README 캐시 절)은 Maru가 콘텐츠를 캐시하지 않는 동안에는 필요 없다.
- 경량 방문자 통계: 7의 익명 모드(일자별 솔트 해시 방문자 키, IP 마스킹·해시, 회원 ID 가명화, `analytics_salts`) 설계가 SPEC §9 「쿠키 없는 경량 집계」의 좋은 참고다. 단 7의 규모(테이블 11개, 아카이브 배치)는 Maru 목표(N100, 50MB)에 과하다.

**Phase 3 — 에이전트 레이어** (Maru의 핵심 차별 지점)
- 7에는 운영용 MCP, 스코프 토큰, changeset이 없다. 7의 토큰은 abilities `*`인 로그인 토큰이고 `localStorage`에 저장된다. SPEC §5.3 스코프 토큰과 §6 changeset은 7 대비 그대로 차별점이다.
- 7의 AGENTS.md 구조(짧은 진입 문서 + 문서 지도 + **파일 유형 → 필수 문서 자동 생성 표**)는 Maru `AGENTS.md`·`llms.txt` 설계에 바로 참고할 만하다. Maru는 「개발용」과 「설치·운영용」 두 독자를 나눈다.
- 7의 레이아웃 미리보기(UUID 토큰, 만료 시각, 관리자 ID)와 버전 스냅샷(`changes_summary`)은 Maru `/__preview/<changeset>` 1회용 토큰 설계와 같은 형태다. 미리보기 토큰 만료 정리 배치(`CleanupLayoutPreviewsCommand`)까지 짝으로 둔다.
- 7의 설치 CLI 계약(원인별 종료 코드, 줄 단위 JSON 진행 출력, `--skip-if-installed`, 비밀은 명령줄 인자 금지, root 실행 거부)은 SPEC §10.1과 거의 같은 철학이다. Maru 종료 코드 표(0~5)에 「중단」과 「root 거부」를 넣을지 검토한다.

**Phase 4 — 한국형 커뮤니티**
- 7 board가 「한국 게시판 기본값」의 현재 기준선이다. 게시판 설정 항목(§3.1 `boards` 컬럼), 반응 유형 일반화(추천/비추천을 관리자가 정의), 스크랩, 신고 자동 블라인드 임계치, 금지어, 쿨다운, 비회원 글쓰기(이름 + 비밀번호), 비밀글 3모드(사용 안 함/사용/항상), 비밀 답글 노출 정책, 답변·댓글 깊이 제한, 게시판별 첨부 제한, 작성자·관리자 알림을 Phase 4 체크리스트로 쓴다.
- 7에 없는 것 = Maru 차별 기회: **소셜 로그인**(네이버·카카오·구글), 댓글 승인 큐, 캡차, 포인트, 쪽지, 투표, 팝업.
- 게시판 단위 관리자·스텝 역할은 커뮤니티 운영에서 실제로 쓰인다. Maru `member_roles`에 게시판 범위가 필요해질 수 있다(Phase 4 마이그레이션 후보).
- 7은 댓글을 별도 테이블로 분리했다(비밀 댓글, 깊이 제한, 카운터가 글과 다르기 때문). Maru의 `posts.kind=post/comment` 통합은 Phase 4에서 쿼리·인덱스를 실측해 유지 여부를 판단한다.

**Phase 5 — 외부 모듈 생태계**
- 7 확장은 샌드박스 없는 PHP다. ZIP·GitHub URL 설치, `_bundled`/`_pending`/활성의 3단 디렉터리, vendor-bundle이 있다. Maru WASM 샌드박스(§7.5)는 안전성 면에서 분명한 차이다.
- 7은 그누보드5 데이터 이전 도구를 「예정」으로만 적었다. Maru가 **그누보드5 가져오기(`maru import gnuboard5`)**를 먼저 내면 기존 그누보드5 사이트 운영자를 끌어오는 경로가 될 수 있다(범위 결정 필요).

### 10.3 Phase 1 스키마·권한에 지금 반영할 것

PHASE1_PLAN §4.3(권한)·§5(스키마) 기준이다. 비용이 작고, 나중에 바꾸면 비싼 것만 골랐다.

| # | 항목 | 그누보드7 근거 | 제안 | 비용 |
|---|---|---|---|---|
| P1 | **글 소유 판정을 권한 함수의 기본 규칙으로** | 권한마다 `owner_key`, 역할-권한마다 `scope_type` self/role/전체 (§3.2, §4) | `core` 권한 함수에 「`write`는 자기 글 수정·삭제를 포함하고 남의 글은 `moderate`가 필요하다」를 명문화하고 테스트로 고정한다. 정책 JSON 키는 그대로 둔다 | 문서 + 테스트 몇 개 |
| P2 | **`posts.author_id` nullable** | 7은 `user_id` nullable + 비회원 `author_name`·`password` 유지 | Phase 1에서 `author_id`를 nullable로 만든다(Local·Agent Actor, 이후 비회원 글). 비회원 컬럼은 Phase 4에서 추가 | 컬럼 제약 하나 |
| P3 | **삭제를 되돌릴 수 있게** | 7은 글·댓글·페이지·레이아웃 모두 soft delete + 상태 `deleted` + 조치 주체(`trigger_type`) | Phase 1 상태 생명주기(§4.6)에 「삭제」를 넣는다. `status=deleted` 또는 `deleted_at` 중 하나로 정하고, 공개 조회 함수 하나에서 함께 배제한다. 물리 삭제는 별도 정리 명령으로 미룬다 | 상태값 하나 + 역경로 테스트 |
| P4 | **`guest` 역할 행을 실제로 시드** | 7의 `guest`는 실제 역할이고 관리자가 권한을 줄 수 있다 | SPEC §4 기본 역할에 이미 guest가 있다. `Anonymous` Actor → `guest` 정책값 매핑을 권한 함수 한 곳에 둔다(이미 계획과 일치. 확인만) | 없음 |
| P5 | **시드 출처 기록 검토** (ADR 후보) | 7의 `user_overrides`: 확장·언어팩 재시딩이 운영자 편집을 덮어쓰지 않는다 | `init --preset`을 다시 실행해도 안전하려면(원칙 3 멱등) 프리셋이 만든 행과 사용자가 고친 필드를 구분해야 한다. Phase 1은 「`init` 재실행 = 변경 없음」만 보장하고, 프리셋 갱신(Phase 4)에 `user_overrides`형 기록을 둘지는 ADR로 남긴다 | ADR 1건 |
| P6 | **세션 쿠키 + CSRF 유지 확인** | 7은 CSRF를 끄고 Bearer 토큰을 `localStorage`에 둔다 | PHASE1 §7 보안 기준(HttpOnly 세션 + CSRF)을 그대로 유지한다. 관리 화면에서 토큰을 `localStorage`에 두지 않는 것을 금지 항목으로 명시한다 | 문서 한 줄 |
| P7 | **root 실행 경고** | 7 설치기는 root 실행을 거부한다(`--web-user` 없으면 종료 코드 7) | `maru init`·`serve`·`doctor`가 root/Administrator로 실행되면 경고한다(원칙 6). 거부까지 할지는 결정 필요 | 작은 구현 |

**지금 넣지 않을 것** (7에는 있지만 Phase 1 범위 밖이거나 Maru 원칙과 맞지 않는 것)
- 다국어 JSON 컬럼: Maru는 한국어 우선이고 메시지 키로 관리한다. 콘텐츠 다국어는 출시 전에 일괄 판단한다.
- 설정 JSON 파일: Maru는 `site_settings`를 DB에 두어 단일 파일 백업을 유지한다. 7 방식으로 바꿀 이유가 없다.
- 비회원 비밀번호, 비밀글, 공지, 분류, 반응 카운터 같은 비정규화 컬럼: Phase 4에서 읽는 코드와 함께 추가한다(PHASE1 §5 원칙 유지).
- 게시판별 동적 권한 행 생성: Maru는 게시판 `policy` JSON(최소 역할)이 더 단순하다. 게시판 단위 관리자가 필요해지면 그때 `member_roles` 범위로 푼다.

### 10.4 차별점 솔직한 평가

**겹치는 부분 (위협)**
- 「에이전트 친화」라는 메시지를 7이 먼저 가져갔다. README 첫 줄이 「개발자와 에이전트를 위한」이고, sir.kr은 「AI 주도 개발을 위한 코어 솔루션」을 내건다. 「한국형 + 에이전트」라는 Maru의 첫 문장은 더 이상 유일하지 않다.
- 「한국형」 기능 범위는 7이 압도적이다. PG 4사, 본인확인 2사, 알림톡, 쇼핑몰, 다국어, 위지윅 편집기를 갖췄고, 23년 된 그누보드 브랜드와 sir.kr 커뮤니티가 있다. 기능 수로 경쟁하면 진다.
- 7은 모든 기능이 권한 검사를 거친 REST API로 이미 노출되어 있다. SIR이 운영용 MCP 서버를 얹는 것은 기술적으로 몇 주 거리다. MCP 하나만으로는 오래 가는 차별이 아니다.

**남는 차별 (Maru가 지켜야 할 것)**
1. **설치 마찰**: 7은 PHP 8.2 + 확장 16개, MySQL, 웹서버, Composer가 필요하다. 「사용자의 PC나 N100 미니PC에 바이너리 하나 + SQLite 파일 하나」는 7의 구조(Laravel + MySQL + SPA 빌드)로는 따라오기 어렵다. Maru의 가장 단단한 해자다.
2. **에이전트 운영 안전장치**: 스코프 토큰, changeset 미리보기 → 승인 → 한 번에 롤백, 사용자 입력의 `untrusted` 분리. 7에는 없고, 7의 토큰 모델(`*` abilities, `localStorage`)을 바꾸려면 인증 구조를 손대야 한다.
3. **결과물 디자인 품질**: 7은 편집기는 강하지만 디자인 토큰·프리셋 체계는 못 봤다. 「한마디 → 몇 분 → 멋진 결과」는 프리셋과 토큰 품질로 증명해야 한다.
4. **SSR + 경량**: 7은 SPA에 봇용 정적 HTML을 따로 만든다. Maru는 서버 렌더링이라 SEO, Lighthouse, 메모리 목표에서 구조적으로 유리하다.
5. **7에 없는 커뮤니티 기능**: 소셜 로그인, 댓글 승인 큐, 캡차, 포인트, 쪽지, 투표, 팝업. 이 가운데 소셜 로그인과 댓글 승인은 이미 SPEC Phase 4 범위다.

**포지셔닝 제안**: 「그누보드7의 대안」이 아니라 **「서버·PHP·DB 없이, 에이전트가 내 PC에 설치하고 운영하는 작은 사이트용 CMS」**로 좁힌다. 쇼핑몰, PG, 본인확인, 대형 커뮤니티는 7의 영역으로 인정한다. 블로그, 소상공인 홈페이지, 기관 소개, 소규모 커뮤니티에서 「설치 0분 + 승인형 에이전트 운영 + 기본 디자인 품질」로 이긴다.

---

## 11. 확인하지 못한 것

- 사용자 사이트 메뉴 관리 UI(헤더·푸터 메뉴 편집)가 레이아웃 편집기 밖에 따로 있는지
- 게시글 HTML을 저장할 때 서버 측에서 정화하는지(렌더링 시 DOMPurify와 봇용 `HtmlSanitizer`만 확인)
- Sanctum 토큰을 DB에 해시로 저장하는지(`vendor/`가 저장소에 없어 Sanctum 내부 코드를 직접 보지 못했다. 마이그레이션의 `token` 64자 컬럼만 확인)
- 디자인 토큰이나 테마 색상 일괄 변경 기능
- 실제 성능·메모리 사용량(실행하지 않았다)
- sir.kr의 공식 출시 공지 게시물(sir.kr/g7 제품 페이지와 GeekNews 기사로만 확인)
- 그누보드5 마이그레이션 도구의 실제 공개 여부(README에는 「예정」)

---

## 출처

- 공식 저장소: https://github.com/gnuboard/g7 (README, README.ko.md, LICENSE, CHANGELOG.md, AGENTS.md)
- 공식 제품 페이지: https://sir.kr/g7
- GeekNews 「라라벨 기반 그누보드7 오픈베타 출시」(2026-04-04): https://news.hada.io/topic?id=28172
- Maru 설계: `E:\AI코딩프로젝트\클로드코드\rust-cms\docs\SPEC.md` §2·§4·§5·§7·§8·§9·§15, `docs\PHASE1_PLAN.md` §4.3·§5
