# Phase 1 구현 계획

> 상태: **승인** (2026-11-02). §9의 ADR은 `docs/DECISIONS.md` ADR-0001~0015로 옮겼다.
> 같은 날 범용 사이트 제안서(`docs/proposals/universal-site.md`) 채택으로 ADR-0016, 0017과 준비 3건을 반영했다(§2 아래쪽 행).
> 제품 수치와 요구의 정본은 `docs/SPEC.md`다. 이 문서는 순서, 설계 경계, 검증 방법만 정한다.

## 1. 실행 계약

| 항목 | 내용 |
|---|---|
| 원하는 결과 | `maru init --preset blog --yes` 후 `serve`로 실제 블로그를 운영할 수 있는 단일 바이너리 |
| 최소 완료 조건 | SPEC §15 Phase 1 완료 기준 4개를 실제 바이너리로 실행해 증거를 남긴다 |
| 필요한 증거 | 기능은 E3(실제 프로세스, HTTP, DB 흐름), 화면은 브라우저 직접 확인 |
| 제외 범위 | 위젯, 모듈, changeset, MCP, 미디어 업로드, 댓글, 회원 가입, 태그, 검색, 서비스 등록, 터널 |
| 범위 확대 조건 | 위 항목이 필요해지면 구현 전에 질문한다 |

## 2. 이전 계획에서 바뀐 것

| 항목 | 이전 | 지금 | 이유 |
|---|---|---|---|
| 작업 순서 | 서비스를 층별로 쌓고 U14에서야 전체 연결 | **M1에서 글 한 편이 끝까지 지나가는 세로 경로를 먼저 완성** | 격리된 CRUD만 쌓고 실제 통합이 없던 사고(09 문서 TIER 2) 방지 |
| 미래용 컬럼 | `group_id`, `category_id`, `changeset_id`를 미리 추가 | **읽는 코드가 생기는 Phase의 마이그레이션에서 추가** | 읽는 코드 없는 선언은 동작으로 착각하기 쉽고, SQLite는 컬럼 추가가 쉽다 |
| Rate limit | 로그인만 | **로그인과 REST 전체** | 외부 노출 API는 MVP부터 제한(09 문서 TIER 1) |
| REST 인증 | bearer와 세션 둘 다 | **bearer만** | 쿠키 인증을 빼면 REST에 CSRF 표면이 없다 |
| 감사 누락 방지 | 테스트로 확인 | **구조로 차단 + 테스트** | 쓰기 트랜잭션이 감사 기록 없이는 커밋되지 않게 한다 |
| 로그인 링크 | GET으로 바로 소비 | **GET은 확인 화면, POST에서 소비** | 메신저 미리보기와 링크 검사기가 1회용 링크를 먼저 소비하는 것을 막는다 |
| 디자인 토큰 CSS | 템플릿에 출력 | **형식 검증 후 별도 CSS로 출력** | HTML 자동 이스케이프는 CSS 문맥을 보호하지 않는다 |
| 검증 판정 | 통과/실패 | **PASS, FAIL, 못 쟀다 + 역시험** | 못 잰 것을 통과로 세지 않고, 결함을 심어 게이트가 떨어지는지 본다 |
| 열거 값 CHECK | 미정 | **늘어날 열거 값은 CHECK 없이 core 검증, 상태 기계 값만 CHECK** | SQLite는 CHECK 변경에 테이블 재작성이 필요하다 (ADR-0016) |
| 공개 URL 생성 | 위치별 조합 | **`url_for(Target)` 하나로만** | Phase 2 퍼머링크 도입 때 사용자 테마가 깨지지 않게 (ADR-0017) |
| 템플릿 로더 | 사용자 테마 우선 | **소스 목록을 순서대로 찾는 겹 구조, `theme.toml`의 `template_api = 1`** | Phase 3 changeset 오버레이를 소스 하나 추가로 끝내기 위해 (ADR-0022 준비) |
| 프리셋 형식 | 고정 세트 TOML | **`[[ops]]` 연산 목록 TOML** | Phase 3 레시피(ADR-0023)로 그대로 옮기기 위해 |
| 글 삭제 | 미정 | **`deleted` 상태로 되돌릴 수 있게, 영구 삭제 없음** | 에이전트 실수 복구 (ADR-0029) |
| 권한 범위 | 액션 단위 | **본인 글은 `post.write`, 남의 글은 `post.moderate`** | 그누보드7 대조에서 모호함 발견 (ADR-0029) |
| `posts.author_id` | 필수 | **nullable, 작성 주체는 `actor_type`·감사 로그로 추적** | local·에이전트 Actor, 이후 비회원 글 (ADR-0029) |
| 관리자 권한 실행 | 미정 | **root/Administrator면 경고 후 계속** | 사용자 결정 (ADR-0029) |

## 3. 결정 대기 질문

아래 추천안을 전제로 계획을 썼고, 다르게 답하면 해당 단위만 바뀐다. 사용자는 아직 답하지 않았다.

- **Q1 근거:** 원칙 4(`CLAUDE.md`)와 SPEC §6 첫 줄은 발행을 changeset에 포함하지만, SPEC §1.2, §5.3, §6 끝, §11은 발행을 스코프와 감사 로그로 다룬다. Phase 1에는 changeset이 없으므로 구조 변경은 바로 적용하고 변경 전 값을 `audit_log.diff`에 남겨 Phase 3 역연산에 재사용한다.
- **Q2 근거:** SPEC §3.1의 CLI는 DB를 직접 열어, 셸을 쓰는 에이전트가 §5.3의 스코프와 "적용은 사람만" 규칙을 우회할 수 있다. 같은 OS 계정에서는 보안 경계가 될 수 없으므로 안전장치로만 다루고, Phase 3에서 에이전트 쓰기를 MCP로 유도한다. `posts.actor_type`에 `local` 값이 추가된다.
- **Q3 근거:** SPEC §15 Phase 1에는 REST가 없지만 `CLAUDE.md`와 착수 프롬프트에는 있다. SPEC §10의 `init`은 초기 토큰을 발급하는데 `maru token`은 Phase 1 CLI 목록에 없다. `blog` 프리셋의 태그, 검색, 최신글 위젯도 Phase 1에 없고 태그는 데이터 모델에도 없다.
- **Q4 근거:** SPEC §10의 `init`에는 owner 입력 인자가 없다. 생성한 비밀번호를 JSON으로 내면 에이전트 대화 기록에 남고 분실 시 복구 명령도 없다. 초기 토큰은 `read`, `draft` 스코프로 1회만 표시한다.
- **Q5 근거:** crates.io에서 `maru`(0.0.1, 2023)와 `nuri`(0.1.0, 2024)는 등록돼 있고 `madang`, `maru-core`, `maru-cli`는 비어 있다(2026-11-02). 바이너리 이름 `maru`는 쓸 수 있다. 호스팅 SaaS가 범위 밖이라 무임승차 위험이 낮고 에이전시 납품이 쉬워야 해 허용형을 추천한다. 공개 후 AGPL로 바꿔도 이미 낸 릴리스에는 소급되지 않는다.

| # | 질문 | 추천안 | 영향 단위 |
|---|---|---|---|
| Q1 | 글 발행도 changeset인가 | 아니다. §11을 따르고 원칙 4의 범위를 구조, 디자인, 모듈, 설정으로 좁힌다 | U4, U8, U9 |
| Q2 | 로컬 CLI의 Actor는 무엇인가 | `actor_type=local`, 운영자 권한. `MARU_TOKEN`이 있으면 스코프 강제 | U4, U6 |
| Q3 | Phase 1 범위 | 최소 REST와 토큰 포함. 태그, 검색, 위젯은 Phase 2. 홈 최신글은 템플릿 고정 | U6, U10, U14 |
| Q4 | owner 초기 자격증명 | `init`은 비밀번호를 만들지 않고 1회용 로그인 링크 사용, `maru login-link`로 재발급 | U6, U12 |
| Q5 | 이름과 라이선스 | `maru`, `MIT OR Apache-2.0`. 공개 전 확정 | U1 |

## 4. 핵심 설계

### 4.1 의존 방향

```text
cli ─┬─> web ─┬─> core
     │        └─> themes ─> core
     └─> storage ─> core
```

- `core`에는 `sqlx`, `axum`, `minijinja`가 들어오지 않는다. U1 게이트가 `cargo tree -p maru-core`로 검사한다.
- 구현체 조립은 `cli`에서만 한다. `web`은 `Arc<dyn Store>`를 담은 `Ctx`만 받는다.
- `modules`, `agent` 크레이트는 Phase 2, 3에서 추가한다.

### 4.2 쓰기 경계

모든 쓰기 서비스는 한 경로만 지난다. 저장소의 쓰기 메서드는 `WriteTx`를 통해서만 호출할 수 있다.

```rust
let mut tx = ctx.store.begin_write().await?;     // BEGIN IMMEDIATE
let before = tx.posts().get(input.id).await?;
authorize(&actor, Permission::PostPublish, &before.scope())?;
let after = tx.posts().set_status(input.id, Status::Published).await?;
tx.audit(&actor, "post.publish", Target::post(after.id), diff(&before, &after))?;
tx.commit().await?;                              // 감사 기록이 0건이면 Err
```

- 권한 검사, 쓰기, 감사 기록이 한 트랜잭션이다. 감사 기록이 실패하면 쓰기도 롤백된다.
- `audit_log.diff`에서 `password_hash`, `token_hash`와 원문 토큰은 제거한다.
- **감사 대상:** 도메인 쓰기, 로그인 성공과 실패, 비밀번호 변경, 토큰 발급과 폐기.
- **감사 제외:** `last_used_at` 갱신, 세션 만료 정리처럼 시스템이 자동으로 하는 기록.

### 4.3 Actor와 권한

```rust
pub enum ActorKind {
    Anonymous,
    Human { member_id: Id },
    Agent { token_id: Id },
    Local,                     // Q2
}
```

- 공개 화면도 `Anonymous` Actor로 같은 읽기 서비스를 부른다. 공개 여부 판단이 웹 코드로 새지 않는다.
- 권한 함수는 `core`에 하나만 둔다. REST, CLI, 관리 화면은 호출만 한다.
- 게시판 `policy`는 Phase 1에서 실제로 읽는 키(`list`, `read`, `write`, `moderate`)만 정의한다.

### 4.4 SQLite 연결

- WAL, `foreign_keys=ON`, `busy_timeout`을 적용한다.
- 쓰기는 `BEGIN IMMEDIATE`로 시작한다. 읽다가 쓰기로 승격할 때 생기는 충돌을 피한다.
- 쓰기 풀 1개, 읽기 풀 소수로 나눈다. 정확한 값은 U4 측정으로 정한다.
- `sqlx::migrate!`로 마이그레이션을 바이너리에 내장한다. 테이블은 `STRICT`로 만든다.
- DB 스키마가 바이너리보다 새로우면 `SCHEMA_TOO_NEW`로 멈추고 `next`를 준다.
- 콘텐츠는 프로세스 안에 캐시하지 않는다. CLI로 발행한 글이 실행 중인 서버에 바로 보여야 하기 때문이다. Markdown도 요청마다 렌더링하고, 성능 문제가 측정되면 그때 캐시를 넣는다.

### 4.5 에러 출력

- `core::Error`는 안정된 `code`, 메시지 키, 선택적 `hint`, `next`를 가진다.
- CLI는 SPEC §10.1 형식으로 출력한다. REST는 같은 `code`를 쓰되 내부 정보는 내보내지 않는다.
- 예상하지 못한 오류는 사용자에게 `INTERNAL`과 요청 ID만 주고, 상세 원인은 로그에 남긴다.
- 로그인 실패는 계정 유무를 구분하지 않는 같은 응답을 준다.

### 4.6 상태 생명주기

| 상태 | 넣는 곳 | 빼는 트리거 | 역경로 테스트 |
|---|---|---|---|
| 세션 | 로그인 | 로그아웃, 만료, 비밀번호 변경, 회원 비활성, 주기 정리 | 트리거마다 이전 쿠키가 거부되는가 |
| 로그인 링크 | `init`, `login-link` | 사용, 만료, 새 링크 발급, 주기 정리 | 재사용과 이전 링크가 거부되는가 |
| API 토큰 | `init`, `token create` | 폐기, 만료 | 폐기 직후 같은 토큰이 거부되는가 |
| Rate limit 키 | 요청 | 주기 정리, 종료 | 키가 무한히 늘지 않는가 |
| 글 공개 상태 | 발행 | 숨김, 삭제(`deleted`), 복원 | 목록, 글, RSS, 사이트맵에서 함께 사라지고 복원 시 돌아오는가 |
| 사용자 테마 | 테마 복사 | 파일 삭제 | 내장 테마로 되돌아가는가 |
| 서버 자원 | `serve` | 종료 신호, 시작 실패 | 연결을 닫고 정상 종료하는가 |

공개 글 조회는 함수 하나로 통일한다. 홈, 목록, RSS, 사이트맵이 같은 함수를 부르게 해서 공개 범위가 갈라지지 않게 한다. 테스트는 같은 함수를 호출하지 않고 실제 HTTP 응답으로 확인한다.

### 4.7 공개 URL과 슬러그

- `/`, `/b/{board}`, `/b/{board}/{post}`, `/p/{page}`, `/sitemap.xml`, `/rss.xml`, `/robots.txt`, `/admin`, `/api/v1`.
- 한글 슬러그를 허용하고 NFC로 정규화한다. macOS의 분해형 한글 입력이 다른 슬러그가 되는 것을 막는다.
- 슬러그 유일성은 `(board_id, slug)`다. 글 슬러그를 바꾸면 옛 주소는 Phase 1에서 404다. 리다이렉트는 범위 밖이다.
- 절대 URL은 항상 `config.toml`의 `site_url`로 만들고 `Host` 헤더를 쓰지 않는다.

## 5. 스키마 범위

| 마이그레이션 | 단위 | 내용 |
|---|---|---|
| `init_core` | U3 | `site_settings`, `members`, `roles`, `member_roles`, `tokens`, `audit_log`, `boards`, `posts`, `revisions` |
| `init_site` | U9 | `pages`, `menus` |
| `init_media` | U3 | `media`. SPEC 목록에 있어 만들지만 Phase 1에는 읽는 코드가 없다 |
| `init_auth` | U12 | `sessions`, `login_tickets`. SPEC §4에 없어 ADR로 제안 |

Phase 1에 넣지 않는 컬럼은 다음과 같다. 해당 Phase에서 읽는 코드와 함께 추가한다.

- `boards`: `field_schema`, `group_id`, `view_template`
- `posts`: `parent_id`, `thread_path`, `meta`, `is_notice`, `is_secret`, `category_id`, `view_count`
- `revisions`, `audit_log`: `changeset_id`
- `pages`: `blocks`

`revisions`는 `post_id`, `page_id`를 nullable로 두고 CHECK로 정확히 하나만 허용한다.

CHECK 제약 범위 (ADR-0016):

- **CHECK 유지:** 상태 기계 값 `posts.status`, `posts.kind`, `posts.actor_type`, `audit_log.actor_type`, `members.status`, 그리고 위의 `revisions` 대상 하나 규칙.
- **CHECK 없음, core 타입으로 검증:** `boards.kind`, `pages.layout`, `menus.location`, `audit_log.target_type`, `audit_log.action`. DB에서 알 수 없는 값을 읽으면 `INTERNAL`이 아니라 경고로 처리한다.

## 6. 작업 단위

각 단위는 끝날 때 `scripts/check.ps1`을 통과하고 커밋한다. M1 이후 기능은 `core`, `storage`, REST, CLI, 테스트 순서로 한 단위 안에서 세로로 완성한다.

### M0 기반

| 단위 | 산출물 | 의존 | 테스트 | 커밋 메시지 |
|---|---|---|---|---|
| U0 저장소·툴체인 | git, `.gitignore`, `.gitattributes`, `rust-toolchain.toml`, 로컬 빌드 경로, `.env.example`, 포트 3139 | 없음 | 툴체인 버전, `*.sql` LF 고정, 포트 비어 있음 | `chore: 저장소와 툴체인 고정` |
| U1 워크스페이스·게이트 | 크레이트 5개, `scripts/check.ps1`, pre-commit, CI, `deny.toml` | U0 | `maru --version`. **역시험:** 포맷 위반 커밋과 `core`의 금지 의존이 실제로 막히는가 | `chore: 워크스페이스와 품질 게이트 구성` |
| U2 출력 계약 | 에러 타입, 메시지 키, `--json` 형식, 종료 코드 0~5 | U1 | 성공·실패 스냅샷, 종료 코드 매핑, REST 내부 정보 비노출 | `feat(core): 도메인 에러와 출력 계약` |

### M1 걷는 뼈대

M1이 끝나면 글 한 편이 실제로 끝까지 지나간다. 이후 단위는 이 경로를 넓히는 작업이다.

| 단위 | 산출물 | 의존 | 테스트 | 커밋 메시지 |
|---|---|---|---|---|
| U3 데이터·스키마 | 데이터 디렉터리, `config.toml`, `init_core`, `init_media`, `.sqlx/` | U2 | up-down-up 왕복, 실패 시 다음 재실행, 한글·공백 경로, WAL·FK 확인 | `feat(storage): 초기 스키마와 데이터 디렉터리` |
| U4 쓰기 경계 스파이크 | `Ctx`, `Actor`, `Store`, `WriteTx`, 권한, 감사 기록 | U3 | 감사 실패 시 롤백, 감사 없는 커밋 거부, 두 프로세스 동시 쓰기, 올바른 사유의 권한 거부 | `feat(core): 쓰기 경계와 감사 로그` |
| U5 글 최소 서비스 | 초안 작성, 발행, 공개 조회의 서비스와 저장소 구현 | U4 | 상태 전이, 리비전 생성, 초안 비공개 | `feat(core): 글 초안과 발행 서비스` |
| U6 세로 연결 | 최소 `init`, 토큰 검증, `POST /api/v1/posts`, `post new/publish`, `serve`, 공개 글 페이지 | U5 | **E2E-1** (§8) | `feat: init부터 공개 페이지까지 연결` |

**U4 중단 기준:** 다음 중 하나라도 풀리지 않으면 U5로 넘어가지 않고 ADR로 대안을 제안한다.

1. `dyn Store`로 트랜잭션을 안전하게 소유할 수 있는가
2. `query!` 매크로를 쓰는 저장소 구현이 트레이트 뒤에서 깔끔한가
3. 서버 실행 중 CLI 동시 쓰기가 `SQLITE_BUSY` 없이 끝나는가

대안은 `Ctx<S: Store>` 제네릭 구조다.

### M2 콘텐츠

| 단위 | 산출물 | 의존 | 테스트 | 커밋 메시지 |
|---|---|---|---|---|
| U7 Markdown | 렌더링, ammonia 정화, 정화된 값 전용 타입 | U6 | XSS 벡터 스냅샷, 한글 본문. **역시험:** 정화를 끄면 테스트가 실패하는가 | `feat(core): 정화된 Markdown 렌더링` |
| U8 게시판·글 완성 | 게시판 CRUD, 글 수정·제출·숨김·복원, 슬러그, 권한 정책, REST, CLI, `--from`, `--if-not-exists` | U7 | 전이 전체, 복원, 비어 있지 않은 게시판 삭제 거부, 권한 매트릭스, 한글 입력 경로 | `feat: 게시판과 글 전체 기능` |
| U9 페이지·메뉴·설정 | `init_site`, 서비스, REST, CLI, 메뉴 트리 검증 | U7 | CRUD, 잘못된 트리 거부, 초안 페이지 비공개, 감사 로그 | `feat: 페이지·메뉴·사이트 설정` |

### M3 표현

| 단위 | 산출물 | 의존 | 테스트 | 커밋 메시지 |
|---|---|---|---|---|
| U10 테마·프리셋 | 내장 테마, 겹 구조 로더(사용자 테마 → 내장), `template_api = 1`, 토큰 검증, 토큰 CSS, `[[ops]]` 형식 `blog` 프리셋, Pretendard, `THIRD_PARTY.md` | U8, U9 | CSS 주입 거부, WCAG AA, 경로 탈출 차단, 사용자 테마 삭제 후 복귀, 프리셋 재적용 멱등, 내장 템플릿에 `/b/`·`/p/` 경로 리터럴 없음 | `feat(themes): 기본 테마와 blog 프리셋` |
| U11 공개 사이트 | `url_for` 단일 URL 빌더, 홈, 목록, 페이지네이션, 글, 페이지, 사이트맵, RSS, 메타, robots, 404, 보안 헤더 | U10 | 숨김 글 동시 제거, XML 유효성, `]]>` 주입, 메타 스냅샷, 사이트맵·RSS·canonical·CLI 출력 URL이 모두 `url_for`와 일치, **UI-1** (§8) | `feat(web): 공개 사이트와 SEO 출력` |

U11 착수 전에 네이버 서치어드바이저 공식 문서로 사이트맵, RSS, 메타 요건을 확인한다.

### M4 인증·관리

| 단위 | 산출물 | 의존 | 테스트 | 커밋 메시지 |
|---|---|---|---|---|
| U12 세션 인증 | `init_auth`, argon2id, 세션, 로그인 링크, CSRF, 전체 rate limit, 주기 정리 | U11 | §4.6 역경로 전부, 링크 동시 사용 시 1회만 성공, 로그에 비밀 값 없음, 외부 `next` 거부 | `feat(core): 세션 인증과 요청 제한` |
| U13 관리 화면 | 로그인, 비밀번호 설정, 글 목록, 작성, 발행, 로그아웃 | U12 | **E2E-2** (§8), 비로그인 차단, CSRF 없는 POST 거부 | `feat(web): 최소 관리 화면` |

### M5 운영 마감

| 단위 | 산출물 | 의존 | 테스트 | 커밋 메시지 |
|---|---|---|---|---|
| U14 운영 CLI | `doctor`, `token`, `login-link` 완성, 정상 종료, `--quiet` | U13 | 포트 충돌 `next`, `init` 중간 실패 후 재실행, doctor의 "못 쟀다" 표시, 전 명령 `--json --yes` | `feat(cli): doctor와 운영 명령` |
| U15 완료 검증 | §8의 검증표, 릴리스 빌드, §14 수치 1회, 문서 대조 | 전체 | 깨끗한 디렉터리에서 재현, 역시험 재실행, 쓰기 경로별 감사 로그 대조 | `test: Phase 1 완료 기준 검증` |

## 7. 보안 기준

| 위협 | 대응 | 확인 방법 |
|---|---|---|
| Markdown XSS | ammonia 정화, 링크 스킴 허용 목록, 정화된 값 전용 타입 | XSS 벡터 스냅샷과 역시험 |
| 템플릿 XSS | 자동 이스케이프, 내장 테마의 `\|safe`는 정화된 본문만 허용 | 내장 테마 검사 스크립트 |
| RSS·사이트맵 | `.xml` 자동 이스케이프, CDATA 미사용 | 제목에 `</title><script>`, `]]>` 삽입 |
| CSS 주입 | 토큰 값 형식 검증, 폰트 허용 목록, 별도 CSS 출력 | `}</style><script>` 토큰 거부 |
| 세션 쿠키 | `HttpOnly`, `SameSite=Lax`, https일 때 `Secure` | 쿠키 속성 검사 |
| CSRF | 세션에 묶인 토큰, 상수 시간 비교 | 토큰 없는 POST와 다른 세션 토큰 거부 |
| 비밀번호 대입 | argon2id, 동시 계산 수 제한, rate limit, 없는 계정도 더미 검증 | 제한 응답과 같은 실패 메시지 |
| 로그인 링크 유출 | 10분, 1회, 해시 저장, 확인 화면, `Referrer-Policy: no-referrer` | 재사용 거부, 로그 검사 |
| Open Redirect | `next`는 `/` 시작 내부 경로만, `//`, `/\` 거부 | 외부 주소 입력 테스트 |
| Host 헤더 조작 | 절대 URL은 `site_url`로만 생성 | 가짜 `Host`로 사이트맵 요청 |
| 프록시 헤더 위조 | 기본은 연결 주소를 키로 사용, 전달 헤더 무시 | 가짜 `X-Forwarded-For`로 우회 시도 |
| 정보 노출 | `INTERNAL`과 요청 ID만 응답 | DB 오류를 유발해 응답에 경로와 SQL이 없는지 확인 |
| 감사 로그 비밀 유출 | diff에서 민감 필드 제거 | 비밀번호 변경 후 `audit_log` 전체 검색 |
| 과대 요청 | 본문 크기, 요청 시간 제한 | 큰 본문 거부 |
| 경로 탈출 | 테마와 자산 경로 정규화 | `..`, 절대 경로, 역슬래시 |
| 응답 헤더 | CSP, `nosniff`, `Referrer-Policy`, 관리 화면 `frame-ancestors 'none'` | 헤더 스냅샷 |

공개 터널 뒤에서는 모든 요청이 `127.0.0.1`에서 온 것처럼 보인다. 따라서 Phase 3의 `maru expose`에서 신뢰할 전달 헤더를 ADR로 정해야 한다.

## 8. 검증

### 게이트

| 게이트 | 내용 | 멈추는 곳 |
|---|---|---|
| `scripts/check.ps1` | fmt, clippy `-D warnings`, 테스트, `sqlx prepare --check`, 의존 방향 | pre-commit |
| CI | 위 항목과 `cargo deny`, `cargo audit`, Windows·Linux | 원격 저장소 연결 후 |
| E2E | 실제 바이너리, 실제 포트, 실제 DB 파일 | `cargo test` |

프로젝트 언어에 맞춰 `tests/smoke.mjs` 대신 `crates/cli/tests/e2e_*.rs`를 smoke로 쓴다. 이 규칙은 U1에서 프로젝트 `CLAUDE.md`에 적는다.

### E2E 시나리오

**E2E-1 (M1 관문)**

1. 임시 디렉터리에서 `maru init --preset blog --yes --json`을 실행한다.
2. 발급된 토큰으로 `POST /api/v1/posts`를 호출해 한글 초안을 만든다.
3. 공개 URL이 404인지 확인한다.
4. `maru post publish`를 실행한다.
5. 실행 중인 서버의 공개 URL에 본문이 나오는지 확인한다.
6. `posts.status`, `revisions`, `audit_log`를 DB에서 직접 읽어 Actor와 action을 대조한다.

같은 서비스를 REST와 CLI가 함께 쓰는지 이 경로로 확인한다.

**E2E-2 (M4 관문, 실제 브라우저)**

1. 생성한 테스트 owner로 `maru login-link`를 발급한다.
2. 링크를 열어 확인 후 로그인하고 비밀번호를 설정한다.
3. 글을 작성해 발행하고 공개 페이지와 DB를 확인한다.
4. 로그아웃 후 세션 행이 지워졌는지, `/admin`이 차단되는지 확인한다.
5. 비밀번호로 다시 로그인하고 틀린 비밀번호의 응답을 확인한다.

**권한 매트릭스**

비로그인(guest), member, editor, owner × 공개 읽기, 초안 읽기, 작성, 본인 글 수정·삭제, 남의 글 수정·삭제, 발행, 구조 변경을 전수 확인한다. 상태 코드만 보지 않고 **올바른 `code`로 거절됐는지** 본다. member와 editor는 테스트 픽스처로 만든다.

**UI-1 (직접 확인)**

- 375px, 1120px × 라이트, 다크에서 홈, 목록, 글, 페이지를 직접 본다.
- Pretendard가 시스템 폰트 폴백이 아니라 실제 woff2 요청과 `document.fonts`로 로드되는지 잰다.
- 대비, 키보드 포커스, `prefers-reduced-motion`, em-dash와 이모지 부재를 확인한다.

### 역시험

| 심는 결함 | 떨어져야 할 게이트 |
|---|---|
| 정화 호출 제거 | XSS 스냅샷 |
| 서비스의 감사 기록 제거 | `WriteTx` 커밋 거부 |
| 공개 조회에 초안 포함 | E2E-1 3단계 |
| 권한 함수를 항상 허용으로 변경 | 권한 매트릭스 |
| 세션 삭제 누락 | 로그아웃 역경로 |
| XML 자동 이스케이프 해제 | RSS 주입 테스트 |
| `core`에 `sqlx` 추가 | 의존 방향 검사 |

### Windows와 한글

- 데이터 경로에 한글과 공백이 있어도 `init`, `serve`가 동작한다.
- PowerShell 5.1과 Git Bash에서 `--title` 한글, `--from`, 표준 입력을 각각 확인한다. PowerShell 파이프가 한글을 깨뜨리면 `--from` 경로를 기본 안내로 쓴다.
- `--from`은 UTF-8 BOM을 제거하고, 잘못된 인코딩은 원인과 `next`를 담아 거부한다.
- `--json` 출력이 콘솔과 파이프 양쪽에서 유효한 UTF-8 JSON인지 확인한다.
- 테스트 정리 전에 DB 풀을 닫아 Windows 파일 잠금 오류를 피한다.
- 실제 Ctrl+C 종료는 자동 테스트로 재기 어렵다. 종료 로직은 내부 신호로 테스트하고, 실제 키 입력은 수동으로 확인한다.

### 완료 보고 형식

U15는 항목마다 PASS, FAIL, 못 쟀다 중 하나와 실행 명령, 대조 건수를 적는다. 현재 예상되는 "못 쟀다"는 다음과 같다.

- N100 실측 성능 (장비 접근 전까지)
- GitHub CI (원격 저장소 연결 전까지)
- macOS 빌드
- Lighthouse 수치는 §14 목표이며 Phase 1 완료 기준은 아니다. 측정하면 별도로 보고한다.

SPEC은 다음 Phase 전에 실제 사이트 운영을 요구한다. U15는 기능 검증까지이고, 운영 기간은 사용자가 정한다.

## 9. ADR 후보

| 제목 | 결정 | 스펙과의 관계 |
|---|---|---|
| 세로 슬라이스 우선 | M1에서 전 경로를 먼저 연결 | 절차 |
| 쓰기 경계 | `WriteTx` 없이는 쓰기 불가, 감사 없는 커밋 거부 | CLAUDE.md 구체화 |
| 감사 범위 | §4.2의 대상과 제외 | "모든 쓰기" 해석 |
| 토큰 해시 | 긴 무작위 값은 SHA-256, 비밀번호는 argon2id | CLAUDE.md 표 변경 |
| 로그인 티켓 | 1회용 링크, GET 확인, POST 소비 | §10 `init` 보강, `sessions`, `login_tickets` 추가 |
| Phase별 스키마 | 읽는 코드가 생길 때 컬럼 추가 | §4를 단계 적용 |
| 공개 URL | §4.7 | SPEC에 없음 |
| 게시판 삭제 | 글이 있으면 거부 | SPEC에 없음 |
| Mono 폰트 | Phase 1은 시스템 고정폭 글꼴, D2Coding은 추후 | §8.2 예시와 다름 |
| 개발 포트 | 개발 3139, 제품 기본값 8080 유지 | CLAUDE.md 개발 명령 변경 |

### 이후 Phase에 넘기는 사항

| Phase | 사항 |
|---|---|
| 2 | `posts`는 ULID 기본 키라 rowid가 VACUUM에서 바뀔 수 있다. FTS external content 대신 자체 저장과 `post_id` 컬럼 구조를 검토한다 |
| 2 | trigram은 3글자 미만을 찾지 못한다. 2글자 LIKE 폴백을 검토한다. 태그 모델도 정한다 |
| 2 | `Module::routes()`의 `Router<AppState>`는 `modules`와 `web`의 순환 의존 위험이 있다 |
| 3 | 공개 터널 뒤 클라이언트 IP, 로컬 CLI의 `changeset apply` 경계 |
| 미디어 | `image`의 WebP 인코딩은 무손실만 지원한다. 손실 압축이 필요하면 C 의존을 ADR로 판단한다 |

## 10. 크레이트

2026-11-02 조회. 전 항목의 버전이 docs.rs와 crates.io에서 일치했다. Rust 1.99.1, edition 2024. 고정은 `rust-toolchain.toml`과 `Cargo.lock`으로 한다.

### CLAUDE.md 스택

| 크레이트 | 버전 | Rust 최소 | 라이선스 | 메모 |
|---|---|---|---|---|
| axum | 0.8.9 | 1.80 | MIT | |
| tower | 0.5.3 | 1.64 | MIT | |
| tower-http | 0.7.1 | 1.65 | MIT | axum의 선택 의존은 0.6 계열. U1에서 조합 확인 |
| tokio | 1.54.1 | 1.85 | MIT | |
| sqlx | 0.9.0 | 1.94 | MIT/Apache-2.0 | `query*()`는 정적 문자열이나 `AssertSqlSafe`만 허용 |
| libsqlite3-sys | **0.37.0** | 없음 | MIT | sqlx 제약 `<0.38`. 최신 0.39.1 사용 불가. SQLite 3.51.3, FTS5 포함 |
| minijinja | 3.0.5 | 1.70 | Apache-2.0 | `.xml`도 기본 HTML 이스케이프 대상 |
| pulldown-cmark | 0.13.6 | 1.71.1 | MIT | |
| ammonia | 4.2.4 | 1.85 | MIT/Apache-2.0 | 필수 의존 cssparser가 MPL-2.0 |
| clap | 4.6.7 | 1.85 | MIT/Apache-2.0 | |
| argon2 | 0.6.0 | 1.85 | MIT/Apache-2.0 | |
| serde | 1.0.229 | 1.56 | MIT/Apache-2.0 | |
| serde_json | 1.0.154 | 1.71 | MIT/Apache-2.0 | |
| schemars | 1.2.3 | 1.74 | MIT | rmcp `^1.0`과 호환 |
| thiserror | 2.0.21 | 1.77 | MIT/Apache-2.0 | |
| anyhow | 1.0.104 | 1.68 | MIT/Apache-2.0 | |
| tracing | 0.1.44 | 1.65 | MIT | |
| tracing-subscriber | 0.3.23 | 1.65 | MIT | |
| rust-embed | 8.13.0 | 1.80 | MIT | |
| insta | 1.49.0 | 1.66 | Apache-2.0 | 개발 의존성 |
| image | 0.25.10 | 1.88 | MIT/Apache-2.0 | 미디어 구현 시 추가 |
| rmcp | 3.8.0 | 1.88 | Apache-2.0 | Phase 3에 추가 |

### 스택 외 추가

| 크레이트 | 버전 | Rust 최소 | 라이선스 | 이유 |
|---|---|---|---|---|
| ulid | 3.0.0 | 없음 | MIT | SPEC의 ID 형식 |
| time | 0.3.55 | 1.88 | MIT/Apache-2.0 | RFC3339 UTC |
| toml | 1.1.8 | 1.85 | MIT/Apache-2.0 | 설정, 프리셋 |
| sha2 | 0.11.1 | 1.88 | MIT/Apache-2.0 | 토큰, 세션, 링크 해시 |
| getrandom | 0.4.3 | 1.85 | MIT/Apache-2.0 | 비밀 값 생성 |
| base64 | 0.23.1 | 1.71 | MIT/Apache-2.0 | 토큰 인코딩 |
| subtle | 2.6.1 | 없음 | BSD-3-Clause | CSRF 상수 시간 비교 |
| async-trait | 0.1.92 | 1.71 | MIT/Apache-2.0 | `dyn Store` |
| axum-extra | 0.12.6 | 1.80 | MIT | 쿠키 처리 |
| governor | 0.10.4 | 없음 | MIT | Rate limit |
| mime_guess | 2.0.5 | 없음 | MIT | 내장 자산 Content-Type |
| unicode-normalization | 0.1.26 | 1.36 | (MIT/Apache-2.0) AND Unicode-3.0 | 슬러그 NFC |
| fs4 | 1.1.0 | 1.75 | MIT/Apache-2.0 | `doctor` 디스크 확인 |
| tempfile | 3.27.0 | 1.63 | MIT/Apache-2.0 | 개발 의존성 |
| assert_cmd | 2.2.2 | 1.85 | MIT/Apache-2.0 | 개발 의존성 |
| predicates | 3.1.4 | 1.74 | MIT/Apache-2.0 | 개발 의존성 |
| ureq | 3.5.0 | 1.85 | MIT/Apache-2.0 | 개발 의존성, E2E HTTP 요청 |

도구: sqlx-cli 0.9.0, cargo-deny 0.20.2, cargo-audit 0.22.2.

`deny.toml` 허용 목록에는 MIT, Apache-2.0, BSD-3-Clause, Unicode-3.0, MPL-2.0을 넣고, 나머지는 실제 결과를 보고 판단한다.

## 11. 위험

| 위험 | 신호 | 대응 |
|---|---|---|
| `dyn Store`와 sqlx 트랜잭션 궁합 | U4 중단 기준 실패 | 제네릭 구조로 교체. M1이라 수정 범위가 작다 |
| `begin_with`가 `any` 피처를 요구 | U4 컴파일 실패 | 피처 활성화 또는 연결 수준 `BEGIN IMMEDIATE`. 문서 요약만으로는 확정 못함 |
| 동시 쓰기 `SQLITE_BUSY` | U4 두 프로세스 테스트 실패 | 재시도 정책과 `busy_timeout` 조정 |
| 크레이트 조합 컴파일 실패 | U1 빌드 실패 | 호환 버전으로 내리고 이유를 커밋에 기록 |
| Pretendard 용량 | 단일 파일이 크고 공개 릴리스 zip은 47MB | 필요한 woff2만 포함, `font-display: swap` |
| Pretendard 라이선스 표기 | GitHub API가 `NOASSERTION` 반환 | 받은 라이선스 파일 원문을 확인한 뒤 포함 |
| 범위 팽창 | 단위가 SPEC §15 밖으로 커짐 | 멈추고 질문 |

## 12. 실행 단위와 비용

사용자 지침의 한 작업 단위 상한은 약 150만 토큰이다. Phase 1 전체는 이를 넘을 수 있어 **마일스톤별로 끊어 실행**하고, 끝날 때마다 보고한다. 아래는 경험 기반 추정이며 측정값이 아니다.

| 마일스톤 | 단위 | 추정 토큰 | 끝나면 확인할 것 |
|---|---|---|---|
| M0 | U0~U2 | 약 15만 | 게이트가 실제로 커밋을 막는가 |
| M1 | U3~U6 | 약 40만 | E2E-1 |
| M2 | U7~U9 | 약 40만 | 권한 매트릭스, 복원 |
| M3 | U10~U11 | 약 35만 | UI-1 |
| M4 | U12~U13 | 약 35만 | E2E-2 |
| M5 | U14~U15 | 약 20만 | 완료 검증표 |

구현은 메인 세션이 직접 한다. 에이전트는 기본적으로 쓰지 않고, 필요하면 예상 토큰을 먼저 알린다.

## 13. 승인에 포함되는 환경 변경

- Rust 1.99.1 설치: `rustup-init.exe`(12.7MB, `static.rust-lang.org`), 설치 위치 `F:\rust`, 사용자 환경변수 `RUSTUP_HOME`, `CARGO_HOME`, `PATH` 변경
- 도구 설치: `sqlx-cli`, `cargo-deny`, `cargo-audit`
- 빌드 산출물: `F:\claude-temp\rust-cms\target`, 저장소에는 커밋하지 않는 로컬 설정으로 지정
- 지침 수정: `PORT-REGISTRY.md`에 3139 등록, 프로젝트 `CLAUDE.md`의 개발 명령과 smoke 규칙 수정
- U10의 Pretendard 다운로드: 받기 전에 파일명, 출처, 크기를 다시 알린다
