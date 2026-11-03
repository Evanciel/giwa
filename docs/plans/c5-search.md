# 계획: 사이트 검색 (GAP C5) — 2026-11-03

> 근거: `docs/GAP_PLAN.md` §2 C5, SPEC §5(「전문 검색은 SQLite FTS5 가상 테이블 `posts_fts(title, body_md)`」),
> SPEC §9 MCP `search_posts`. C6·C7 은 35b4a0e 에서 끝났다.
> 예상 토큰: 약 40만.

## 확인한 사실

- 번들 SQLite 는 3.51.3, `libsqlite3-sys` 0.37 빌드 옵션에 `SQLITE_ENABLE_FTS5` 가 있다. 새 의존성 없이 FTS5 를 쓸 수 있다.
- FTS5 기본 토크나이저(`unicode61`)는 띄어쓰기로만 자른다. 「제주도에서」 를 한 낱말로 넣으므로 「제주」 로 찾으면 안 나온다.
  내장 `trigram` 은 3글자부터라 한국어에 흔한 2글자 검색어(「제주」·「국밥」·「후기」)는 색인을 못 쓴다.
- 글 내용이 바뀌는 길은 storage `Tx::insert_post`·`Tx::update_post` 두 곳이다(만들기·고치기·리비전 되돌리기·발행·숨기기·지우기 모두 여기를 지난다). 글을 DB 에서 아주 지우는 길은 없다.
- 페이지(`/p/{slug}`)는 `pages` 표에 따로 있다.
- 유니코드 정규화(`unicode-normalization` 0.1)는 이미 Cargo.lock 에 있다(간접 의존). core 의 직접 의존으로 올려도 새로 받는 것이 없다.

## 0. 먼저 정할 것 (사용자 결정)

| # | 질문 | 추천 | 다른 안 |
|---|---|---|---|
| D1 | 한글을 어떻게 자르나 | **core(Rust)가 2글자 단위로 잘라 넣는다.** 한글·한자·가나 덩어리는 「제주도에서 → 제주 주도 도에 에서」, 영문·숫자는 낱말 그대로(소문자). 검색어도 같은 함수로 잘라 **이어진 구(phrase)** 로 찾는다(「제주도」 → `"제주 주도"`). 1글자 검색어는 앞부분 일치(`"길"*`). FTS5 는 기본 `unicode61` 로 띄어쓰기만 자른다 | ① 내장 `trigram` + 2글자 이하는 `LIKE` 전체 훑기 — 코드가 적지만 흔한 2글자 검색이 색인 없이 돌고 순위가 없다 ② 형태소 분석기(lindera 등) — 정확하지만 사전이 수십 MB 라 단일 바이너리가 커진다 |
| D2 | 무엇을 찾나 | **공개된 글 + 공개된 페이지의 제목·본문(마크다운을 걷어낸 글자)·태그.** 손님이 읽을 수 있는 게시판만. **비밀글은 공개 검색에서 통째로 뺀다**(피드·사이트맵·태그와 같다, ADR-0043). 관리 화면 검색은 권한 안에서 초안·비밀글까지 | 글만(페이지 빼기) / 비밀글 제목은 🔒 로 보이기 |
| D3 | 검색어 문법 | **띄어 쓴 낱말은 모두 들어간 글(AND). `OR` 를 끼우면 어느 하나. `"큰따옴표"` 는 붙은 그대로. `-낱말` 은 빼기.** 범위 고르기: 제목+내용(기본)·제목·내용·태그. 사용자 입력은 FTS5 문법으로 직접 넘기지 않고 core 가 안전한 질의로 다시 만든다 | AND 만 |
| D4 | 순서 | **정확도 순(bm25, 제목 ×5·태그 ×3·본문 ×1), 같으면 최신.** 「최신순」 으로 바꿀 수 있다. 한 쪽 20개 | 최신순만 |
| D5 | 어디서 쓰나 | **사이트 전체 `/search?q=` 화면 + 머리글 검색 칸, 게시판 화면 아래 게시판 안 검색(범위 고르기), 관리 화면 글 목록 검색 칸, REST, CLI.** 결과에 제목·게시판·날짜·검색어 둘레 본문 한 토막(찾은 말 강조) | 사이트 전체만 |
| D6 | 검색어 기록 | **남기지 않는다**(개인정보·쓰기 부담). 인기 검색어는 필요해지면 S3 방문 통계와 함께 따로 | 검색어 집계 표 |

아래는 추천안 기준이다.

## 1. 데이터 (마이그레이션 1개 + down)

```sql
CREATE VIRTUAL TABLE search_fts USING fts5(
  source UNINDEXED,   -- 'post' | 'page'
  ref_id UNINDEXED,   -- posts.id | pages.id
  title, body, tags,  -- core 가 2글자 단위로 자른 글자
  tokenize = 'unicode61 remove_diacritics 0'
);
```

- 색인에는 상태와 상관없이 **내용만** 넣는다. 공개 여부·비밀글·게시판 권한은 찾을 때 `posts`/`pages` 와 이어 붙여(join) 거른다. 그래서 발행·숨기기·지우기·예약 발행 때는 색인을 고치지 않아도 된다.
- 마이그레이션은 빈 표만 만든다(SQL 로는 2글자 자르기를 못 한다). 색인 버전을 `settings` 의 `search.index_version` 에 두고, 서버·CLI 가 DB 를 열 때 버전이 다르면 전부 다시 만든다(글 1만 개 기준 몇 초). 자르는 규칙을 바꾸면 버전만 올린다.
- down: `DROP TABLE search_fts`, 버전 설정 삭제.

## 2. core

- `search` 모듈(DB 비의존)
  - `index_text(&str) -> String` — NFC 정규화 → 소문자 → 한글·한자·가나 덩어리는 2글자 단위(1글자 덩어리는 그대로), 그 밖은 낱말 그대로. 마크다운은 기존 `markdown::plain_text` 로 걷어낸 뒤 넣는다(지시자 `:::`·`:font[]` 꾸밈 기호 포함).
  - `Document::of_post(&Post)` / `of_page(&Page)` — 색인할 제목·본문·태그.
  - `Query::parse(&str) -> Result<Query>` — D3 문법을 해석해 낱말·구·OR·빼기 목록으로. 100자·낱말 10개 한도, 빈 질의·빼기만 있는 질의는 검증 오류(`error.search_query`, 다음 명령 안내 포함).
  - `Query::to_fts(range) -> String` — 모든 토막을 큰따옴표로 감싸 만든다(사용자가 FTS5 연산자·열 이름을 직접 못 넣는다).
  - `snippet(plain, &Query, max_chars) -> Vec<Segment { text, hit }>` — 본문 원문에서 찾은 말 둘레 한 토막. HTML 을 만들지 않고 토막만 돌려준다(템플릿이 `<mark>` 로 감싸고 자동 이스케이프한다, `|safe` 없음).
- `services::search::search(ctx, actor, SearchInput { q, board?, range, sort, page }) -> SearchPage { items, total, page }`
  - 손님은 읽을 수 있는 게시판(`max_read_rank`)·공개 글·비밀글 아님·공개 페이지만. 로그인한 운영자·편집자의 관리 화면 검색은 `admin: true` 로 초안·비밀글까지(권한 검사는 서비스에서만).
  - 읽기라 감사 기록 없음.
- `services::search::reindex(ctx, actor) -> { posts, pages }` — 사이트 관리 권한, 감사 `search.reindex`(몇 개 다시 만들었는지만). 멱등.
- `Store`/`Tx`: `search_upsert(source, ref_id, doc)`, `search_query(...)`, `search_rebuild(...)`. storage 의 `insert_post`·`update_post`·페이지 쓰기가 같은 트랜잭션에서 `search_upsert` 를 부른다(색인과 글이 어긋나지 않게).

## 3. 다섯 경로

- **공개 화면**
  - `/search?q=&in=all|title|body|tag&sort=rank|new&page=` — 결과 목록(제목·게시판·날짜·본문 토막), 몇 건인지, 없으면 「다른 말로 찾아 보세요」. `<meta name="robots" content="noindex">`, 사이트맵 제외.
  - 머리글 검색 칸(`<form role="search">`, 스크립트 없이 동작). 사이트 설정 `site.show_search`(기본 켬)로 숨김.
  - 게시판 화면 아래 게시판 안 검색: `/b/{board}?q=&in=` — 범위 고르기 + 검색 칸, 결과는 같은 목록 틀(`_list.html`).
- **REST**: `GET /api/v1/search?q=&board=&in=&sort=&page=` → `{ items, total, page }`. `POST /api/v1/search/reindex`(관리 권한).
- **CLI**: `maru search "<q>" [--board] [--in] [--sort] [--page]`, `maru search reindex`. 전부 `--json`·멱등. 문법 오류에는 고친 예시를 넣는다.
- **관리 화면**: 글 목록 위 검색 칸(`/admin?q=`), 초안·비밀글 포함.
- **MCP**: 아직 없음(Phase 3, SPEC 의 `search_posts`). 서비스만 맞춘다.

## 4. 작업 단위와 순서 (각 단위 게이트 통과, 커밋은 사용자 확인 뒤)

| 단위 | 내용 | 테스트 |
|---|---|---|
| T1 | core `search`: `index_text`·`Query::parse`·`to_fts`·`snippet` | 단위(insta 스냅샷): 「제주도에서」 자르기, 영문·숫자·섞인 말, NFD 입력, 1글자, 문법(AND·OR·구·빼기·한도·빈 질의), FTS5 특수문자(`"` `*` `:` `^` `(`)가 질의를 깨지 않음, 토막 경계(한글 중간에서 자르지 않음) |
| T2 | 마이그레이션·storage(`search_upsert`·`search_query`·`search_rebuild`, 글·페이지 쓰기에 연결, 열 때 버전 확인), `cargo sqlx prepare` | storage: 올리고 내리기, 글 만들기·고치기·리비전 되돌리기 뒤 색인 일치, 「제주」로 「제주도에서」 찾기, 비밀글·초안·숨김·지운 글·못 읽는 게시판 제외, 순위(제목 일치가 위), 페이지 포함, 버전 바뀌면 다시 만들기 |
| T3 | `services::search`·`reindex`, 권한·감사 | 단위: 손님·운영자 결과 차이, 감사 `search.reindex` |
| T4 | 공개 화면(`/search`, 머리글 칸, 게시판 안 검색), `site.show_search` | e2e(실제 바이너리): 한글 2글자·구·OR·빼기, 게시판 안 검색, 비밀글 본문이 토막에도 안 나옴, noindex, 이상한 입력(`"`, `*`, 1000자)에 500 없이 안내 |
| T5 | REST·CLI | e2e: 화면·REST·CLI 같은 답, `--json` 계약, reindex 멱등 |
| T6 | 관리 화면 글 목록 검색 칸, 브라우저 확인(데스크톱·375px) | **브라우저 확인 → 사용자 확인 뒤 커밋** |
| T7 | 문서: ADR(2글자 자르기·색인 버전·검색어 기록 안 함), GAP_PLAN C5 ✅ | 게이트 |

## 위험과 대응

- **2글자 자르기의 헛걸림:** 낱말 경계를 넘는 구는 안 맞는다(「제주 도」 와 「제주도」 는 다르다). 띄어쓰기가 다른 글은 못 찾을 수 있다 → ADR 에 한계로 적고, 필요하면 나중에 띄어쓰기 없는 사본 열을 더한다.
- **색인 크기:** 본문의 약 2배. N100·SQLite 파일 하나로 글 수만 개까지 문제없다고 본다 → T2 에서 글 1만 개로 다시 만들기 시간·파일 크기를 재서 ADR 에 적는다.
- **색인과 글이 어긋남:** 같은 트랜잭션에서 고치고, 어긋나도 `maru search reindex` 로 다시 만든다. 찾을 때 `posts` 와 이어 붙이므로 지운·숨긴 글이 새어 나가지 않는다.
- **검색 남용(무거운 질의 반복):** 질의 길이·낱말 수 한도 + 기존 요청 제한(`rate_limit`)을 그대로 받는다.

완료 기준: 손님이 머리글 칸에 「제주」 를 넣으면 「제주도에서 …」 가 든 공개 글이 제목 일치 먼저 나오고, 찾은 말이 강조된다.
비밀글·초안·못 읽는 게시판 글은 제목도 본문 토막도 나오지 않는다. 같은 질의에 화면·REST·CLI 가 같은 결과를 준다.

## 진행 (2026-11-03, 추천안대로 승인)

T1~T7 끝. 계획과 달라진 점은 ADR-0050 「계획과 달라진 점」에 적었다:
- 버전은 `settings` 열쇠 대신 `search_state` 표, rowid 는 `search_docs` 표로 묶었다(VACUUM 뒤에도 안전).
- 색인에 덩어리 끝 글자 하나를 더 넣고, 검색어 마지막 덩어리는 2글자만 쓴다(1글자 앞부분 일치와 구 정렬).
- 관리 검색은 `posts::list` 에 검색 조건을 더했다. 에이전트 토큰의 REST 목록 검색은 기존 규칙대로 제 글 안에서만.
- `maru search` 는 손님 기준, 초안까지는 `maru post list --q`.
- 글 1만 개 측정(디버그): 다시 만들기 15.3초, DB 41MB → 96MB, 1만 건 검색 0.32초. 「색인은 본문의 약 2배」 예상보다 크다.
