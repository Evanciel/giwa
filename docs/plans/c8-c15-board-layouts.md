# 단위 11a — 게시판 형태 · 목록 꾸밈 (GAP C8·C15)

> 2026-11-04. 결정 표는 추천안으로 진행(사용자 지시). 커뮤니티 테마(로드맵 3단계)가 기대는 목록 모양.

## 1. 결정

| # | 결정 | 추천안(채택) | 대안 |
|---|---|---|---|
| D1 | 어디에 둘지 | `boards.layout` 열(빈 값 = 종류 기본값). 표현이지 권한이 아니라 정책 JSON 에 넣지 않는다 | 정책 JSON — 마이그레이션은 없지만 「정책」 의 뜻이 흐려진다 |
| D2 | 형태 | `list`(번호·제목·글쓴이·날짜·조회 줄), `webzine`(지금 모양: 썸네일 + 요약), `card`(격자 카드: 그림·제목·요약), `gallery`(격자 그림 + 제목) | 테마마다 따로 — 형태는 사이트 구조라 코어가 이름을 정하고 테마가 그린다 |
| D3 | 기본값 | blog → webzine, gallery → gallery, 나머지 → list | 모두 webzine — 한국형 게시판은 표 목록이 기본 |
| D4 | 대체 썸네일 | 카드·갤러리에서 그림 없는 글은 분류 이름(없으면 제목 첫 글자) 타일, CSS 만 | 기본 그림 파일 — 자산이 필요하고 사이트마다 안 어울린다 |
| D5 | 새 글 | 발행 24시간 안이면 `is_new`, 제목 옆 「N」 표시 + 숨은 글자 「새 글」. 본문 해시 ETag 라 시간이 지나면 저절로 바뀐다 | 사이트 설정 시간 — 다음 단위에서 필요하면 |
| D6 | 글 번호 | `list` 형태에서 `전체 - (쪽-1)×쪽당 - 순서` (공지 제외) | 글 id — 사람이 읽을 수 없다 |
| D7 | 떠 있는 단추 | 글 화면 오른쪽 아래 「맨 위」(`#main`)·「목록」, 스크립트 없이 `position: fixed`, 인쇄 때 숨김 | 스크립트로 스크롤 감지 — 원칙상 스크립트 없이 |

## 2. 다섯 경로

- **데이터**: 마이그레이션 `boards_layout` (`ALTER TABLE boards ADD COLUMN layout TEXT NOT NULL DEFAULT ''`, down: DROP COLUMN).
- **core**: `Board.layout`, `BOARD_LAYOUTS`, `Board::effective_layout()`, `NewBoard.layout`·`BoardPatch.layout` 검증(`error.board_layout`).
- **REST**: 기존 `POST/PATCH /boards` 가 같은 입력 타입을 쓴다 — 저절로 열린다.
- **CLI**: `maru board create|update --layout`, `board show` 에 형태.
- **공개 화면**: `board.layout`, `post.is_new`·`post.number`, `_list.html` 형태별 분기, `post.html` 떠 있는 단추.
- **MCP**: Phase 3.

## 3. 작업 단위

| 단위 | 내용 | 테스트 |
|---|---|---|
| T1 | 마이그레이션·storage·모델·서비스 검증 | 저장소: 기본값·바꾸기·잘못된 값 거부·감사 |
| T2 | CLI | e2e 안에서 |
| T3 | 공개 JSON·템플릿·CSS | e2e: `--layout gallery` → 격자·대체 타일, `list` → 번호, 새 글 표시, 글 화면 떠 있는 단추 |
| T4 | 브라우저(데스크톱·375px), ADR-0054, GAP | 게이트 |

완료 기준: `maru board update free --layout card` 하면 DB `boards.layout` 이 `card` 이고 목록이 카드 격자로, 그림 없는 글은 대체 타일로 보인다.

## 진행 (2026-11-04)

- T1~T4 완료. ADR-0054.
- 계획과 다르게: 비밀글도 카드·갤러리에서 그림 대신 자물쇠 타일(대표 그림이 내용을 드러낼 수 있다). 좁은 화면 표 목록은 빈 글쓴이 칸을 숨긴다(들여쓰기처럼 보였다).
- 브라우저(데스크톱·375px): 데모 자유게시판에 글 3개 → 표 목록(번호 4·3·2·1, [분류], N, 비회원 글쓴이) → `--layout gallery` 분류 이름 타일 격자 → `card` 요약 3줄 → 글 화면 맨 위·목록 단추. 375px 가로 넘침 없음.

