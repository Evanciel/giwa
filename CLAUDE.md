# CLAUDE.md — 프로젝트 지침 (가칭: Maru)

> 이 파일은 Claude Code가 매 세션 읽는 프로젝트 지침이다. 상세 설계는 `docs/SPEC.md`, 결정 기록은 `docs/DECISIONS.md`.
> 프로젝트명 `maru`는 가칭이다. 바꾸면 크레이트명·바이너리명·문서를 일괄 변경한다.

## 한 줄 목표

**에이전트에 한마디 하면, 사용자의 컴퓨터에 한국형 홈페이지·게시판·커뮤니티·블로그가 몇 분 만에 멋지게 만들어지는 러스트 CMS 코어.**

- 첫 타깃: 에이전트(Claude Code, Claude Desktop, Cursor 등)를 이미 쓰는 개발자·에이전시·1인 사업자
- 실행 환경: 사용자 PC 또는 미니PC(N100급) 셀프호스팅, 단일 바이너리 + SQLite 파일 하나
- 사람이 관리 화면에서 클릭하는 CMS가 아니라, **에이전트가 설치·세팅·운영하는 CMS**

## 현재 단계

Phase 1 — 코어 뼈대. 단계별 산출물과 완료 기준은 `docs/SPEC.md` §15 로드맵.

## 절대 원칙 (위반하는 구현은 하지 않는다)

1. **API 우선**: 웹 화면, REST, CLI, MCP가 모두 같은 서비스 레이어(`core`)를 호출한다. 특정 인터페이스 전용 비즈니스 로직 금지.
2. **최소 코어 + 모듈**: 코어 도메인 6개(콘텐츠, 사이트 구조, 사용자·권한, 미디어, 표현, 플랫폼) 외 기능은 모듈로 만든다. 모듈은 정해진 확장 지점만 쓰고 코어 내부를 직접 건드리지 않는다.
3. **에이전트는 1급 사용자**: 모든 CLI 명령은 비대화형(`--yes`)으로 끝까지 실행 가능하고, `--json` 출력 계약을 지키며, 여러 번 실행해도 안전(멱등)하고, 에러에는 원인과 다음에 실행할 명령이 들어간다.
4. **에이전트 쓰기는 changeset**: 에이전트의 구조·디자인·발행 변경은 changeset으로 묶어 미리보기 → 승인 → 적용, 그리고 한 번에 롤백 가능해야 한다.
5. **단일 바이너리**: 외부 서비스(DB 서버, Redis, Node 등) 없이 바이너리 하나와 데이터 디렉터리 하나로 동작한다.
6. **안전한 기본값**: 기본 바인딩 `127.0.0.1`, 관리자(root/Administrator) 권한 불필요, 외부 공개는 터널(`maru expose`)로만. 사용자 입력(댓글 등)은 에이전트에게 지시가 아닌 데이터로만 전달한다.
7. **디자인 복제 금지**: 특정 웹사이트의 코드·이미지·일러스트·화면 구성을 복제하지 않는다. 여러 사례에서 공통 패턴만 추출해 우리 디자인 토큰·섹션 블록으로 새로 설계한다. 외부 테마·UI 키트는 MIT·CC0 등 재배포 가능한 라이선스만 쓰고 출처를 `THIRD_PARTY.md`에 기록한다.

## 기술 스택

| 영역 | 선택 | 메모 |
|---|---|---|
| 언어 | Rust stable, 최신 edition | |
| 웹 | axum, tower-http | |
| DB | SQLite + sqlx | 마이그레이션은 `sqlx migrate`, 쿼리는 컴파일 타임 검증 매크로, `cargo sqlx prepare` 결과(`.sqlx/`) 커밋 |
| 템플릿 | minijinja (런타임 로딩) | 에이전트가 테마를 수정·생성할 수 있어야 하므로 컴파일 타임 템플릿 대신 런타임 |
| 마크다운 | pulldown-cmark + ammonia | 렌더 결과는 반드시 ammonia로 정화 |
| CLI | clap (derive) | |
| MCP | rmcp (공식 Rust MCP SDK) | API는 추측하지 말고 docs.rs·소스 확인 |
| 인증 | argon2 | 토큰은 무작위 생성 → 해시만 저장 |
| 스키마 | serde, serde_json, schemars | 공개 입력 타입의 JSON Schema를 MCP 툴 스키마와 설정 폼에 재사용 |
| 에러 | thiserror (라이브러리), anyhow (바이너리) | |
| 로깅 | tracing, tracing-subscriber | |
| 이미지 | image | 썸네일, webp 변환 |
| 자산 내장 | rust-embed | 기본 테마·관리 화면 정적 파일 |
| 테스트 | cargo test, insta | 통합 테스트는 임시 디렉터리의 SQLite |

크레이트 버전은 착수 시 최신 안정 버전을 확인해 고정한다. 새 의존성 추가 시 이유를 커밋 메시지에 적는다.

## 워크스페이스 구조

```
maru/
├─ Cargo.toml              # workspace
├─ CLAUDE.md
├─ AGENTS.md               # 에이전트용 설치·운영 가이드 (Phase 3)
├─ llms.txt                # 에이전트용 요약 인덱스 (Phase 3)
├─ docs/  SPEC.md  DECISIONS.md
├─ crates/
│  ├─ core/       # 도메인 모델, 서비스, 권한, changeset, 이벤트 정의 (DB 비의존 트레이트)
│  ├─ storage/    # sqlx 구현 + migrations/
│  ├─ modules/    # Module·Widget 트레이트, 레지스트리, 이벤트 버스
│  ├─ web/        # axum: 공개 사이트 렌더링, REST API(/api/v1), 관리 화면(/admin), 미리보기(/__preview)
│  ├─ agent/      # MCP 서버, 툴 정의, 모듈 툴 자동 노출
│  ├─ themes/     # 기본 테마·블록·토큰·프리셋 (rust-embed)
│  └─ cli/        # 바이너리 `maru`
└─ mods/          # 공식 모듈 (Phase 2부터): point, social_login, ...
```

## 자주 쓰는 명령

```bash
cargo build
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all --check
cargo run -p maru-cli -- serve --data ./dev-data        # 로컬 실행 (127.0.0.1:8080)
cargo run -p maru-cli -- init --data ./dev-data --preset blog --yes
sqlx migrate add -r <name> --source crates/storage/migrations
cargo sqlx prepare --workspace                           # 쿼리 메타데이터 갱신 후 커밋
```

## 코딩 규칙

- 서비스 함수 시그니처는 `(&Ctx, Actor, input)` 형태. `Actor`는 사람/에이전트 구분과 권한 스코프를 가진다. **권한 검사는 서비스 레이어에서만** 한다.
- 모든 쓰기 작업은 `audit_log`에 기록한다(actor, action, target, diff).
- 라이브러리 코드에서 `unwrap`/`expect` 금지(테스트 제외). 에러는 도메인 에러 enum으로.
- 공개 입력·출력 타입은 `Serialize`, `Deserialize`, `JsonSchema`를 도출한다.
- 사용자 대면 문자열은 한국어 기본, 메시지 키로 관리해 영어를 추가할 수 있게 한다.
- 새 기능 추가 순서: `core` 서비스 → `storage` → REST → CLI → MCP 툴 → 테스트. 다섯 경로가 같은 서비스를 쓰는지 확인한다.
- DB 스키마 변경은 항상 마이그레이션 + 롤백(down) 파일로.
- HTML 템플릿은 접근성(시맨틱 태그, alt, 대비)과 한글 타이포 기본값(SPEC §8.4)을 지킨다.

## 작업 방식

- 큰 작업은 먼저 계획(작업 단위, 순서, 테스트)을 제시하고 승인 후 진행한다.
- 작업 단위마다 테스트를 통과시키고 커밋한다. 커밋 메시지는 한국어 또는 영어 Conventional Commits.
- Phase 완료 기준을 모두 확인한 뒤에만 다음 Phase로 넘어간다.
- 스펙과 다른 결정이 필요하면 구현 전에 질문하거나 `docs/DECISIONS.md`에 ADR(배경·결정·대안·결과)로 기록한다.
- 외부 크레이트 API, 외부 서비스(Cloudflare, 네이버, 카카오) 연동 방식은 기억에 의존하지 말고 공식 문서를 확인한다.
- 개발·테스트 데이터에 실제 개인정보·실제 비밀번호·실제 API 키를 쓰지 않는다.
