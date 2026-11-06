# Phase 3 changeset

SPEC §6·§5.3·§10·§11, 원칙 4. 에이전트의 구조·디자인·모듈 변경을 연산 목록으로 묶어 미리보기 → 승인 → 적용, 한 번에 롤백한다.

## 결정 (추천안대로)

| 항목 | 결정 |
|---|---|
| 저장 | `changesets` 표: 제목, 만든 주체, 상태, `ops`(JSON), `inverse`(JSON), 시각. `audit_log.changeset_id` 추가 |
| 상태 | `preview` → `applied`·`discarded`, `applied` → `reverted`. 적용 도중 멈추면 `failed` 로 남기고 `revert` 로 되돌린다 |
| 연산 | `board.create·update·delete`, `page.create·update·publish·hide·delete`, `menu.set`, `tokens.set·reset`, `settings.set`, `widget.place·configure·remove`, `module.enable·disable`, `preset.apply`(프리셋의 ensure 연산 묶음) |
| 역연산 | 적용하면서 연산마다 바뀌기 전 값을 읽어 만든다. `revert` 는 역연산을 거꾸로 실행한다 |
| 적용 방식 | **사본에서 먼저 돌려 본 뒤 실제 DB 에 차례로 적용하고, 실패하면 이미 한 것을 역연산으로 되돌린다.** SPEC 의 「트랜잭션으로 한 번에」와 다르다. 서비스마다 자기 트랜잭션을 열고 읽기는 읽기 풀을 쓰므로 한 트랜잭션으로 묶을 수 없다(ADR-0121) |
| 되돌리기 충돌 | 그 뒤에 글이 생긴 게시판을 지우는 역연산처럼 데이터를 잃는 경우는 멈추고 원인과 다음 명령을 알려 준다 |
| 미리보기 | `VACUUM INTO` 로 만든 DB 사본에 연산을 적용하고, 같은 라우터를 그 사본으로 띄운다. 미디어는 원본을 읽기만 한다 |
| 미리보기 접속 | `/__preview/<id>` 는 소유자·관리자 세션 또는 1회용 토큰으로 연다. 쿠키로 미리보기 모드에 들어가 링크가 그대로 통하고, 위쪽 띠에서 적용·끝내기를 한다 |
| 미리보기 쓰기 | GET·HEAD 만 받는다. 사본 파일은 최근 3개만 두고 적용·버리기 때 지운다 |
| 적용 권한 | 사람(소유자·관리자), 로컬 CLI, `auto_apply` 토큰. 그 밖의 토큰은 만들기·보기·버리기까지 |
| 토큰의 직접 쓰기 | `structure`·`design`·`admin` 토큰은 서비스를 직접 부를 수 없고 changeset 으로만 반영된다 (SPEC §5.3). 기존 REST 경로도 같다 |
| 만들기 | `maru changeset new --title --ops-file`, `add`. MCP 쓰기 툴은 `changeset_id` 를 받거나 새로 만들고 `changeset_id`·`preview_url` 을 돌려준다 |

## 순서

작업 단위마다 게이트를 통과시켜 커밋한다.

1. **코어:** 마이그레이션 → 모델·Store → `services/changesets.rs`(만들기·더하기·목록·보기·버리기·적용·되돌리기) → CLI → `e2e_changeset.rs`
2. **미리보기:** 사본 만들기·보관 → `/__preview` 라우트·모드 쿠키·위쪽 띠·1회용 토큰 → 브라우저 확인
3. **표면:** REST(`/api/v1/changesets`) → 관리 화면(목록·미리보기·적용·버리기·되돌리기) → 토큰 직접 쓰기 막기 → `auto_apply` 토큰 → MCP 구조·디자인·모듈 툴
4. **설치:** `service install`, `AGENTS.md`, `llms.txt`, 설치 스크립트, 릴리스 빌드

## 시험

- 사람이 A 를 하면 B 가 DB 에 남는지: 연산마다 적용 → 실제 화면·DB 확인 → 되돌리기 → 원래 값
- 중간 연산이 실패하면 앞 연산도 원래대로
- 미리보기 중 실제 사이트는 그대로이고, 미리보기에는 변경이 보인다. 미리보기에서 POST 는 거절
- 스코프별: `structure` 토큰으로 직접 쓰기 거절, changeset 만들기 가능, 적용 거절. `auto_apply` 토큰은 적용 가능
- 완료 기준(SPEC §15): 잘못된 디자인 변경을 `revert` 한 번으로 복원. 빈 PC 에서 에이전트 한마디로 설치 → 미리보기 → 적용
