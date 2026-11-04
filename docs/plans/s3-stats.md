# S3 방문자 통계 — 계획 (단위 12c)

> GAP S3, SPEC §11 「방문자 통계 (쿠키 없는 경량 집계) — 코어, Phase 2」, MCP `stats_summary`(Phase 3).
> 운영자가 「요즘 몇 명이 오고, 어느 글을 보고, 어디서 들어오나」를 알 수 있게 한다. 쿠키·외부 스크립트·IP 저장 없이.

## 1. 결정 (추천안 채택)

| # | 결정 | 추천안(채택) | 대안 |
|---|---|---|---|
| D1 | 무엇을 세나 | 공개 화면 `render()` 가 200 으로 그린 HTML 화면 한 번 = 조회 1. 자산·피드·API·관리 화면·404 는 세지 않는다 | 미들웨어에서 모든 요청 — 자산·봇 요청이 섞인다 |
| D2 | 방문자 | 조회수(ADR-0049)와 같은 방식: 그날(한국 시간)의 메모리 소금 + IP + User-Agent 의 SHA-256 으로 사이트·화면별 「오늘 처음」을 가린다. 해시도 IP 도 DB 에 남기지 않는다. 메모리 상한 넘으면 오래된 것부터 잊는다 | 쿠키 ID — 동의 배너가 필요해진다 |
| D3 | 빼는 것 | 봇(`views::is_bot`), `DNT: 1`·`Sec-GPC: 1`, 미리 읽기(`Sec-Purpose`/`Purpose: prefetch`), 발행 권한이 있는 사람(운영자·편집자) | |
| D4 | 경로 | 쿼리 문자열·조각 제거, 끝 `/` 정리, 200자에서 자름. 들어온 곳은 `Referer` 의 호스트만(소문자, 우리 사이트면 「내부」로 세지 않음, 없으면 「직접」) | 전체 Referer URL — 검색어·개인 경로가 남는다 |
| D5 | 저장 | 날짜별 집계 표 3개: `stat_days(day, views, visitors)`, `stat_pages(day, path, views, visitors)`, `stat_referrers(day, host, visits)`. 메모리에 모아 조회수와 같은 주기에 더한다(실패하면 되돌려 두고 다음에) | 원시 방문 기록 — 개인정보·용량 |
| D6 | 보존 | 설정 `stats_retention_days`(30~3650, 기본 400). 더할 때 그보다 오래된 날을 지운다 | 영구 |
| D7 | 읽기 | `stats::summary(ctx, actor, range)` — 발행 권한(SPEC 의 read/publish 스코프), 기간 1~366일(기본 30). 날짜별 수, 많이 본 화면(글·페이지 제목을 붙임), 들어온 곳, 합계 | |
| D8 | 내보내기 | CSV(날짜별·화면별·들어온 곳 구역). 셀이 `= + - @ \t \r` 로 시작하면 앞에 `'` 를 붙여 수식 주입을 막는다. UTF-8 BOM(엑셀 한글) | |
| D9 | 화면 | `/admin/stats`(기간 7·30·90일, 날짜별 막대는 `<meter>` — CSP 가 style 속성을 막는다), CSV 내려받기 단추. 감사는 없다(통계는 쓰기가 아니다) | 차트 라이브러리 — 외부 JS |

## 2. 다섯 경로

- **데이터**: 마이그레이션 `stats`(표 3개 + down).
- **core**: `services/stats.rs` — `StatCounter`(메모리, `record`·`take`·`put_back`), `flush`(더하기 + 보존 정리), `summary`, `csv`, `normalize_path`, `referrer_host`. 설정 `stats_retention_days`.
- **storage**: `add_stats`, `purge_stats(before)`, `stat_days`/`stat_pages`/`stat_referrers(from, to, limit)`.
- **REST**: `GET /api/v1/stats?days=N`, `GET /api/v1/stats.csv?days=N`.
- **CLI**: `maru stats [--days N] [--csv]`.
- **화면**: 관리 화면 `/admin/stats`, 대시보드 링크.
- **MCP**: `stats_summary` 는 Phase 3.

## 3. 작업 단위

| 단위 | 내용 | 테스트 |
|---|---|---|
| T1 | core 계수기·경로·들어온 곳·CSV | 단위: 쿼리 제거·자르기, 내부/직접/외부 호스트, 하루 한 방문자, DNT·봇 제외, 상한, CSV 수식 방어 |
| T2 | 저장·더하기·보존·요약 | 저장소: 더하기 누적, 보존 지우기, 기간·순서, 권한(손님·글쓴이 거부) |
| T3 | REST·CLI·관리 화면·연결 | e2e: 실제 서버에 글 화면 GET(UA·Referer·DNT 섞어) → `maru stats --json` 수·화면·들어온 곳, CSV |
| T4 | 브라우저, ADR-0061, GAP | 게이트 |

완료 기준: 공개 화면을 열면 쿠키 없이 날짜별 방문자·조회와 화면·들어온 곳이 쌓이고, 봇·DNT·운영자는 빠지며, 관리 화면과 CLI 에서 보고 CSV 로 내려받을 수 있다.

## 4. 진행

- [x] T1~T4 — ADR-0061.
- 계획과 달라진 점: 304 응답도 센다(ETag 미들웨어가 응답 표시를 이어받는다). 보존 정리는 반영마다가 아니라 하루 한 번. 통계 읽기는 `publish` 스코프(SPEC 운영 그룹)라 기본 토큰은 못 본다. 하루 경계는 조회수와 같이 한국 시간 고정.
