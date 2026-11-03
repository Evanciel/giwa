# S1 위젯·영역 — 계획 (단위 12a)

> GAP S1, SPEC §8.3·§11(CLI `widget`)·Phase 2. 최신글·인기글을 비롯한 기본 위젯을 영역에 놓고, 화면 범위(첫 화면·게시판·페이지)를 고른다. 커뮤니티 테마(퀘이사존 분석 뒤)의 첫 화면·곁가지가 여기에 기댄다.

## 1. 결정 (추천안 채택)

| # | 결정 | 추천안(채택) | 대안 |
|---|---|---|---|
| D1 | 저장 | 표 `widgets`(id, kind, region, scope, title, config JSON, sort_order, enabled, created_at, updated_at). 위젯마다 놓기·고치기·빼기 | 메뉴처럼 영역 전체 바꾸기 — 에이전트가 하나만 고치기 번거롭다 |
| D2 | 종류 등록 | 지금은 core 안의 고정 목록(`services::widgets::TYPES`). 종류마다 설정 구조체(serde `deny_unknown_fields` + JsonSchema)로 검사·정규화. Phase 2a 모듈 시스템이 들어오면 이 목록이 레지스트리의 코어 몫이 된다 | 모듈 레지스트리부터 — S1 이 모듈 시스템을 기다린다 |
| D3 | 종류 | `latest_posts`, `popular_posts`, `markdown_block`, `menu`, `notice_bar`, `search`, `image_banner`, `gallery_grid` (SPEC 7종 + 인기글) | |
| D4 | 인기글 | 최근 N일(기본 7, 0 이면 전체) 안에 발행된 공개 글을 조회수 → 댓글 수 → 최신 순. 날짜별 조회 기록은 두지 않는다(S3 통계와 별개) | 기간 조회수 표 — 조회수 집계를 날마다 따로 쌓아야 한다 |
| D5 | 영역 | `header`(머리글 아래 띠), `hero`, `main`(본문 위), `sidebar`(있으면 두 단), `footer`. 내장 테마가 다섯 곳 모두 그린다. 사용자 테마가 안 그리는 영역의 위젯은 조용히 빠진다 | |
| D6 | 범위 | `all`, `home`, `boards`(모든 게시판·글 화면), `board:<주소>`, `page:<주소>`. 한 화면에는 `all` + 그 화면에 맞는 범위가 모두 붙고, 영역 안 순서는 `sort_order` → 만든 순 | 화면마다 따로 배치 — 공통 곁가지를 매번 놓아야 한다 |
| D7 | 렌더 | core 는 위젯 설정을 돌려주고, web 이 종류마다 화면 값(글 목록 등)을 만들고, 테마 `_widget.html` → `widgets/<kind>.html` 이 그린다. 공개 화면은 손님 기준(ETag 그대로). 위젯 하나가 실패하면 그 위젯만 빼고 경고 로그 | core 가 HTML 을 만든다(SPEC 초안 트레이트) — 테마가 모양을 못 바꾼다 |
| D8 | 권한 | 쓰기와 목록은 `design.edit`. 감사 기록 `widget.place`·`widget.configure`·`widget.remove`. changeset 은 Phase 3 (ADR-0023 과 같음) | |
| D9 | 마크다운·주소 | `markdown_block` 은 본문과 같은 렌더(ammonia). 배너·알림 링크는 메뉴 링크 검사와 같은 규칙(`/…`, `http(s)://`), 배너 그림은 글 대표 이미지 규칙 | |

## 2. 다섯 경로

- **데이터**: 마이그레이션 `widgets` (+ down).
- **core**: `model::Widget`, `services::widgets::{types, list, place, configure, remove, for_screen}`.
- **REST**: `GET /api/v1/widget-types`, `GET|POST /api/v1/widgets`, `PATCH|DELETE /api/v1/widgets/{id}`.
- **CLI**: `maru widget types | list | place | configure | remove` (`--config` 는 JSON).
- **화면**: 내장 테마 영역 다섯 곳, 위젯 템플릿 8개, 관리 화면 `/admin/widgets`(목록·켜고 끄기·빼기·JSON 놓기).
- **MCP**: Phase 3.

## 3. 작업 단위

| 단위 | 내용 | 테스트 |
|---|---|---|
| T1 | 마이그레이션·모델·storage | 올리고 내리기 |
| T2 | 서비스(검사·정규화·범위 고르기·인기글 쿼리) | 저장소: 종류·영역·범위·설정 거부, 정규화, 권한, 감사, 화면 범위 고르기·순서·꺼진 위젯, 인기글 순서·기간·비밀글 제외 |
| T3 | REST·CLI·테마·관리 화면 | e2e: CLI 놓기 → 첫 화면·게시판 화면에 최신글·인기글·마크다운, 범위 밖 화면엔 없음, 끄기 → 사라짐, 빼기 → DB |
| T4 | 브라우저(데스크톱·모바일), ADR-0059, GAP | 게이트 |

완료 기준: CLI 로 위젯을 놓고 빼면 공개 화면에 바로 보이고 사라지며(SPEC Phase 2 완료 기준 중 위젯 몫), 곁가지 위젯이 있으면 두 단이 되고 모바일에서는 한 단으로 내려온다.

## 4. 진행

- [x] T1~T4 — ADR-0059.
- 계획과 달라진 점: `place` 는 같은 설정이 있으면 그것을 돌려준다(멱등). 관리 화면에 위로·아래로 순서 바꾸기를 더했다. 위젯은 공개 읽기 화면(`render`)에만 붙고 글쓰기·계정 화면에는 없다.
