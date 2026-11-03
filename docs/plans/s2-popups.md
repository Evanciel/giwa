# S2 팝업(레이어) — 계획 (단위 12b)

> GAP S2. 한국 홈페이지에 흔한 「공지 팝업」: 기간을 정해 첫 화면 등에 띄우고, 방문자가 「오늘 하루 보지 않기」로 닫는다. 그누보드 7 에는 없다.

## 1. 결정 (추천안 채택)

| # | 결정 | 추천안(채택) | 대안 |
|---|---|---|---|
| D1 | 모양 | 위젯 종류 `popup` + 전용 영역 `popup`. `popup` 종류는 `popup` 영역에만, `popup` 영역에는 `popup` 종류만 놓인다. 범위·켜고 끄기·순서·감사·CLI·REST·관리 화면을 위젯(ADR-0059)과 같이 쓴다 | 팝업 표·서비스 따로 — 다섯 경로를 하나 더 만든다 |
| D2 | 설정 | `body`(Markdown, 필수), `image`(`/media/…`, 선택), `link`, `starts_at`·`ends_at`(RFC3339, 선택, 시작 < 끝), `dismiss_days`(0~30, 기본 1 — 「N일 보지 않기」, 0 이면 닫기만), `position`(`center`·`bottom-left`·`bottom-right`, 기본 `center`) | |
| D3 | 기간 | 렌더할 때 서버가 `starts_at ≤ 지금 < ends_at` 만 고른다. 공개 화면 ETag 는 본문 해시라 기간이 바뀌면 응답이 저절로 바뀐다 | 브라우저가 기간 판단 — 시계가 틀린 PC 에서 어긋난다 |
| D4 | 보이기·닫기 | HTML 에는 `hidden` 으로 들어가고 `/assets/site.js`(CSP `script-src 'self'` 안)가 `localStorage` 의 `maru-popup:<id>` 기한을 보고 연다. 「N일 보지 않기」는 그 기한을 저장, 「닫기」는 이번 화면만. 저장소를 못 쓰면 닫기만 된다. JS 가 없으면 팝업이 뜨지 않는다(본문은 그대로) | 쿠키 — 서버가 사람마다 다른 응답을 만들어 ETag·손님 기준 렌더가 깨진다 |
| D5 | 접근성 | `role="dialog"` + `aria-labelledby`(제목, 없으면 「알림」), 열리면 첫 단추에 초점, Esc 로 닫기, 닫으면 원래 초점으로. 여러 개면 겹쳐 쌓되 화면을 막는 덮개는 두지 않는다(본문을 계속 쓸 수 있게) | 모달 덮개 — 팝업 여러 개가 본문을 완전히 가린다 |
| D6 | 모바일 | 좁은 화면에서는 위치와 상관없이 아래쪽 판(bottom sheet), 최대 높이 70vh 스크롤 | |

## 2. 다섯 경로

- **데이터**: 새 표 없음(`widgets`).
- **core**: `widgets` 에 `popup` 종류·영역 짝 검사, `PopupConfig`, 기간 검사, `for_screen` 이 기간 밖 팝업을 뺀다.
- **REST·CLI**: 위젯과 같음 (`maru widget place popup --region popup --config …`).
- **화면**: `base.html` 끝 `popup` 영역, `widgets/popup.html`, `/assets/site.js`, CSS. 관리 화면 영역 목록에 「팝업」.
- **MCP**: Phase 3.

## 3. 작업 단위

| 단위 | 내용 | 테스트 |
|---|---|---|
| T1 | core 검사·기간 | 저장소: 종류·영역 짝 거부, 기간 순서·형식 거부, dismiss_days 범위, 기간 밖은 for_screen 에서 빠짐 |
| T2 | 테마·JS·관리 화면 | e2e: CLI 놓기 → 첫 화면에 dialog(hidden)·site.js, 범위 밖 없음, 끝난 팝업 없음 |
| T3 | 브라우저(열림·N일 보지 않기·Esc·초점·모바일), ADR-0060, GAP | 게이트 |

함께 고칠 것(S1): 사진 모음 열 수를 `style` 속성(CSP `style-src 'self'` 가 막음) 대신 `cols-N` 클래스로.

완료 기준: 기간 안 팝업이 첫 화면에 뜨고, 「오늘 하루 보지 않기」를 누르면 새로 고쳐도 뜨지 않으며, 기간이 지나면 서버 응답에서 빠진다.

## 4. 진행

- [x] T1~T3 — ADR-0060.
- 계획과 달라진 점: 「보지 않기」 저장 키에 고친 시각을 섞어(`<id>:<updated_at>`) 내용을 고치면 다시 뜨게 했다. 팝업이 없는 화면에는 `site.js` 를 싣지 않는다.
