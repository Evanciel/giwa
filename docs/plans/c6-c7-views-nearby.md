# 계획: 조회수 · 이전글·다음글 · 관련 글 (GAP C6·C7) — 2026-11-03

> 근거: `docs/GAP_PLAN.md` §2 C6·C7(단위 6의 뒤 절반). C1~C4 는 04bf645 에서 끝났다.
> 예상 토큰: 약 35만.

## 0. 먼저 정할 것 (사용자 결정)

| # | 질문 | 추천 | 다른 안 |
|---|---|---|---|
| D1 | 조회수를 어디서 세나 | **서버가 글 화면(GET)을 그릴 때 센다.** 자바스크립트 없이도 세고, 한국형 게시판과 같은 방식 | 화면의 스크립트가 따로 알림(비콘) → 봇은 덜 세지만 스크립트 끈 독자는 빠진다 |
| D2 | 「새로고침·같은 사람」을 어떻게 빼나 | **메모리에서 (방문자 표시, 글) 를 24시간 기억.** 방문자 표시 = SHA-256(하루마다 바뀌는 무작위 소금 + IP + User-Agent). IP 원문·해시는 DB 에 남기지 않는다. 서버를 다시 켜면 기억이 지워진다(그날 다시 보면 한 번 더 센다) | DB 테이블에 기록 → 재시작에도 정확하지만 개인정보 보관·쓰기 부담이 생긴다 |
| D3 | 감사 로그 | **조회수는 통계라 `audit_log` 에 남기지 않는다** (CLAUDE.md 「모든 쓰기 → 감사」의 예외로 ADR 기록). 조회수 고치기·초기화 같은 사람의 쓰기는 감사에 남긴다 | 조회마다 감사 기록 → 감사 로그가 조회 기록으로 덮인다 |
| D4 | 누구의 조회는 빼나 | **봇(User-Agent 목록: bot·crawl·spider·slurp·preview·headless 등), 로그인한 운영자·편집자, HEAD 요청, 304 응답** | 운영자도 센다 |
| D5 | 어디에 보이나 | **글 화면, 게시판 목록(목록형), 관리 화면 글 목록, REST·CLI 글 조회.** 사이트 설정 `site.show_views`(기본 켬)로 공개 화면에서만 숨길 수 있다 | 관리 화면에서만 |
| D6 | 관련 글 기준 | **같은 태그가 많이 겹치는 순 → 같은 분류 → 같은 게시판 최신 순, 최대 4개.** 비밀글·공지는 넣지 않는다. 태그도 분류도 없으면 같은 게시판 최신 글 | 같은 게시판 최신만 |

아래는 추천안 기준이다.

## 1. 데이터 (마이그레이션 1개 + down)

```sql
ALTER TABLE posts ADD COLUMN view_count INTEGER NOT NULL DEFAULT 0;
CREATE INDEX posts_board_published ON posts (board_id, status, kind, published_at, id);  -- 이전·다음 글
```

- `view_count` 를 올릴 때 `updated_at` 은 건드리지 않는다(글 고친 시각이 아니다). 리비전도 만들지 않는다.
- down: 인덱스 삭제, `ALTER TABLE posts DROP COLUMN view_count`.

## 2. core

- `services::views`
  - `Visitor` = 방문자 표시(32바이트 해시). 만드는 함수는 web 쪽(IP·UA 를 아는 곳)에서 core 의 `visitor_key(salt, ip, ua)` 를 부른다.
  - `ViewCounter`(Ctx 에 하나): 기억 표(최대 10만 항목, 넘으면 오래된 것부터 버림) + 쌓인 증가분. `record(post_id, visitor) -> bool`.
  - 증가분은 **20초마다 한 트랜잭션으로 DB 에 반영**(`flush`, 예약 발행 루프와 같은 방식). 서버 종료 때도 한 번 반영. N100 에서 조회마다 쓰기를 하지 않는다.
  - 소금은 날짜(KST)가 바뀌면 새로 만든다. 메모리에만 있다.
  - `set_views(ctx, actor, post, n)` — post.moderate 권한, 감사 `post.views.set`. (CLI `maru post views <id> --set 0`)
- `services::posts::neighbors(ctx, actor, post) -> { prev, next }` — 같은 게시판·공개 글·`published_at, id` 순. 비밀글은 제목만(🔒), 공지도 순서에 들어간다.
- `services::posts::related(ctx, actor, post, limit=4)` — D6 규칙. 한 쿼리(태그 겹침 수 + 분류 일치 + 최신)로.
- 둘 다 게시판 읽기 권한을 확인한다(손님이 못 읽는 게시판이면 빈 결과).

## 3. 다섯 경로

- **공개 화면**: 글 화면 아래 「이전글 / 다음글」 줄과 「관련 글」 카드(대표 이미지 있으면 썸네일), 글 머리에 조회수. 게시판 목록에 조회수 칸.
  - 조회수가 바뀌면 화면도 바뀌므로 ETag 는 그대로 둔다(본문 해시 기반이라 자연스럽게 새로 계산된다).
- **REST**: 글 조회 응답에 `view_count`, `GET /api/v1/boards/{board}/posts/{slug}/nearby` → `{ prev, next, related }`. 조회수 고치기 `PATCH` 에 `view_count`(moderate).
- **CLI**: `maru post show` 에 조회수, `maru post nearby <board> <slug>`, `maru post views <id> --set N`. 전부 `--json`·멱등.
- **관리 화면**: 글 목록에 조회수 칸.
- **MCP**: 아직 없음(Phase 3). 서비스만 맞춘다.

## 4. 작업 단위와 순서 (각 단위 게이트 통과, 커밋은 사용자 확인 뒤)

| 단위 | 내용 | 테스트 |
|---|---|---|
| T1 | 마이그레이션·모델·storage 쿼리(증가분 반영, 이웃, 관련), `cargo sqlx prepare` | storage: 올리고 내리기, 반영해도 updated_at 그대로, 이웃 순서(같은 시각은 id), 관련 글 순위 |
| T2 | core `views`(기억 표·소금·한도·반영)·`neighbors`·`related`·`set_views` | 단위: 24시간 안 같은 사람 한 번, 봇 제외, 한도 넘으면 오래된 것 버림, 권한·감사 |
| T3 | web: 글 화면에서 세기(D4 제외 조건), 종료 때 반영, 공개 테마(이웃·관련·조회수), `site.show_views` | e2e(실제 바이너리): 두 번 열어도 1, 다른 UA 는 2, 봇 UA 0, 운영자 0, 숨김 설정 |
| T4 | REST·CLI | e2e: CLI·REST 같은 답, `--json` 계약 |
| T5 | 관리 화면 글 목록 조회수, 브라우저 확인 | **브라우저 확인 → 사용자 확인 뒤 커밋** |
| T6 | 문서: ADR(조회수 세는 방식·감사 예외·개인정보), GAP_PLAN C6·C7 ✅ | 게이트 |

완료 기준: 손님이 글을 열면 20초 안에 DB `posts.view_count` 가 1 오르고, 같은 사람이 새로고침해도 그대로이며, 봇·운영자는 세지 않는다.
글 화면 아래에 같은 게시판의 이전·다음 글과 관련 글이 보이고, 비밀글 본문은 그 어느 곳에도 나오지 않는다.
