# Phase 2a 모듈 시스템 — 계획 (SPEC §7, GAP M7·M8·M11·M12·M13·M14)

> 코어 도메인 밖 기능(포인트·반응·스크랩·알림·쪽지·투표)을 코어 안에 넣지 않기 위한 확장 지점을 만든다.
> 커밋 단위: 16a 뼈대 + 샘플 `point`(M13) → 16b 화면 자리 + 반응·스크랩(M7·M8) → 16c 알림(M11) → 쪽지·투표(M12·M14).

## 1. 결정 (추천안 채택)

| # | 결정 | 추천안(채택) | 대안 |
|---|---|---|---|
| D1 | 크레이트 | `crates/modules`(`maru-modules`): 트레이트·매니페스트·레지스트리·서비스·이벤트 배달. sqlx 없음. 공식 모듈은 `mods/<id>`(`maru-mod-<id>`)이고 바이너리(`maru-cli`)만 안다. `maru-web` 은 `dyn Module` 만 본다 | `crates/web` 이 모듈을 직접 의존 — web 에 sqlx 가 딸려 와 의존 방향 게이트를 어긴다 |
| D2 | 모듈 데이터 | 모듈이 자기 표를 자기 마이그레이션(`mods/<id>/migrations`)과 sqlx 검증 매크로로 다룬다. 표 이름은 `mod_<id>_` 로 시작. 코어 표는 읽기 쓰기 모두 서비스로만 | 코어의 JSON 키값 표 하나 — 순위·집계·트랜잭션이 안 된다 |
| D3 | 마이그레이션 기록 | 사용자 DB 는 `module_migrations(module, version, applied_at)` 에 따로 적고 `sqlx::raw_sql` 로 실행한다. 코어의 `_sqlx_migrations` 와 섞지 않아 켜고 끄기·제거가 코어 버전 검사에 안 걸린다. 개발·게이트 DB 는 `sqlx migrate run --ignore-missing` 으로 전부 적용해 매크로를 검증한다 | 코어 마이그레이션에 모듈 표 포함 — 모듈을 안 켠 사이트에도 표가 생긴다 |
| D4 | 이벤트 | **감사 기록이 곧 이벤트다.** 배달기가 `audit_log` 의 `rowid` 를 모듈별 커서로 따라가며 구독한 동작(`post.publish`, `comment.create`, `auth.login` …)을 넘긴다. 같은 트랜잭션에 적히므로 잃어버리지 않고, 다른 프로세스(`maru` CLI)가 쓴 것도 보이고, 코어 서비스를 고칠 일이 없다. SPEC §7.4 의 이름은 실제 동작 이름으로 바꾼다 | 프로세스 안 이벤트 버스 — CLI 쓰기를 못 보고 죽으면 잃는다. 표를 따로 두는 아웃박스 — 감사와 같은 내용을 두 번 쓴다 |
| D5 | 배달 보장 | 최소 한 번. 핸들러는 `event.id` 로 멱등해야 한다. 실패하면 `module_event_failures` 에 시도 수·오류·다음 시각을 적고 커서는 넘어간다. 5번 실패하면 재시도를 멈추고 실패 보관함에 남긴다(`maru module failures`, 관리 화면). 2초마다 폴링, 서버 켜진 동안만 | 실패에서 멈춤 — 글 하나 때문에 모듈 전체가 선다 |
| D6 | 생명주기 | 공식 모듈은 늘 내장. `enable`: 마이그레이션 + 켬, 커서를 지금 끝으로(꺼져 있던 동안은 소급 안 함). `disable`: 끔, 데이터 유지. `remove --purge --yes`: down 마이그레이션 + 기록 삭제. 기본은 꺼짐. 프리셋이 켤 수 있다 | |
| D7 | 라우트 | 서버 시작 때 모두 합치고, 층 하나가 요청마다 켜짐을 DB 에서 읽어 꺼져 있으면 404. CLI 로 꺼도 재시작이 필요 없다. 주소는 `/m/<id>/…` 로 격리 | 시작 때 켜진 것만 — 재시작 필요, CLI 와 어긋남 |
| D8 | 툴 | `ToolDef{name, description, input_schema, permission, mutates}` + `call_tool`. 같은 정의로 REST(`POST /api/v1/modules/<id>/tools/<tool>`), CLI(`maru <id> <tool> --칸 값`, clap external subcommand), 나중에 MCP 를 만든다(원칙 1) | 경로마다 손으로 |
| D9 | 권한 | 모듈은 코어 `Permission`·서열만 쓴다. 매니페스트의 자체 권한은 Phase 5 외부 모듈 때. 검사는 툴 정의(`permission`)와 모듈 서비스 안에서 | 모듈 권한 표 — 지금 쓰는 곳이 없다 |
| D10 | 설정 | 모듈 설정은 `modules.settings` JSON. 모양은 `FieldDef` 목록(ADR-0018)으로 선언하고 관리 화면 폼·CLI `maru module set` 이 같은 검증을 쓴다 | |
| D11 | 위젯 | `Ctx.extensions`(기본 없음) 로 core `widgets` 가 모르는 종류를 묻는다. 모듈 위젯 값은 `Module::widget_view`, 모양은 `mod/<id>/<kind>.html`. 꺼진 모듈의 위젯은 조용히 빠지고 관리 화면이 표시한다 (SPEC §7.3) | 코어 고정 목록에 추가 — 원칙 2 위반 |
| D12 | 화면 자리(16b) | 템플릿에 이름 붙은 자리(`post_actions`·`post_footer`·`me_sections`·`admin_nav`). 모듈은 값(JSON)과 템플릿 이름을 주고, 템플릿은 모듈 크레이트에 내장, 사용자 테마가 `mod/<id>/…` 로 덮을 수 있다. HTML 을 코드에서 만들지 않는다 | SPEC 초안의 `render → String` — 테마가 모양을 못 바꾼다 |

## 2. 다섯 경로

- 서비스: `maru_modules::service::{list, enable, disable, remove, settings, failures, call_tool}` — `(&Ctx, &Actor, …)`, `module.manage` 권한, 감사 기록.
- 저장소: `Store` 에 `module_state`·`module_set_state`·`module_migrate`·`audit_after`·실패 보관함.
- REST: `GET /api/v1/modules`, `POST /api/v1/modules/{id}/enable|disable`, `DELETE /api/v1/modules/{id}?purge=true`, 툴 경로.
- CLI: `maru module list|enable|disable|remove|set|failures`, `maru <id> <tool>`.
- 관리 화면: `/admin/modules`.

## 3. 샘플 `point` (M13)

- 표: `mod_point_ledger(id, member_id, amount, reason, source_event UNIQUE, at)`, `mod_point_balance(member_id, balance)`.
- 이벤트: `post.write`·`post.publish`(글), `comment.create`(댓글), `auth.login`(하루 한 번). 금액·레벨 기준은 설정.
- 툴: `balance`, `ranking`, `adjust`(`member.manage`). 위젯 `point_ranking`. 화면 `/m/point/ranking`.

## 4. 작업 단위

| 단위 | 내용 | 테스트 |
|---|---|---|
| 16a | 뼈대 + point | 켜기 → 글 → 배달 → 잔액, 끄기 → 경로 404·위젯 빠짐·데이터 유지, 다시 켜기 → 소급 없음, 제거 → 표 사라짐·코어 행 수 같음, 실패 5번 → 보관함, CLI 프로세스 쓰기도 배달 |
| 16b | 화면 자리, 반응·스크랩 | 공개 글 화면에서 누르기 → DB, 끄면 자리 사라짐 |
| 16c | 알림 | 멱등 키·재시도·실패 보관함, 사이트 안·메일 |
| 16d | 쪽지·투표 | 모듈 경로·권한 |

완료 기준(SPEC Phase 2): `point` 모듈을 켰다 끄고 제거해도 코어 데이터가 그대로이고, 위젯을 CLI 로 놓고 뺄 수 있다.

## 5. 진행

- [x] 16a 뼈대 + point (ADR-0066, 0067)
- [x] 16b 화면 자리, 반응·스크랩 (ADR-0068)
- [x] 16c 알림
- [ ] 16d 쪽지·투표
