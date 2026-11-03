# 계획: 분류 · 태그 · 공지 · 비밀글 (GAP C1~C4) — 2026-11-03

> 근거: `docs/GAP_PLAN.md` §2 C1~C4(단위 6의 앞 절반), SPEC §3 데이터 모델(`categories`, `posts.is_notice·is_secret·category_id`),
> `docs/research/blog-editors.md` §4 제안 1. C6 조회수·C7 이전/다음 글은 이 계획에 넣지 않는다(단위 6 뒷부분으로 따로).
> 예상 토큰: 약 50만.

## 0. 먼저 정할 것 (사용자 결정)

| # | 질문 | 추천 | 다른 안 |
|---|---|---|---|
| D1 | G7 브랜치(`claude/g7-creative-5290af`, 커밋 3개)를 먼저 main 에 합칠까 | **먼저 합친다.** `public.rs`·`post.html`·`model.rs`·`settings.rs`·CLI·에디터 번들·DECISIONS·GAP_PLAN 이 겹친다. 특히 G3 의 ETag·304 는 비밀글 응답과 얽힌다(§4) | 합치지 않고 진행 → 나중에 충돌을 손으로 푼다 |
| D2 | 「비밀글」의 뜻 | **그누보드식**: 목록에는 🔒 제목이 보이고, 본문은 작성자·관리자만. 네이버식 「비공개」는 이미 있는 `hidden` 상태를 발행 서랍에 「비공개」로 보여 준다 | 비밀글 = 목록에서도 숨김 (그러면 hidden 과 같아져 의미가 겹친다) |
| D3 | 태그 범위 | **사이트 전체 하나** (`/tags/{태그}` 에 모든 게시판의 글) — 네이버·티스토리와 같다 | 게시판마다 따로 |

아래는 추천안 기준이다.

## 1. 데이터 (마이그레이션 1개 + down)

```sql
CREATE TABLE categories (
  id TEXT PRIMARY KEY NOT NULL, board_id TEXT NOT NULL REFERENCES boards(id) ON DELETE CASCADE,
  name TEXT NOT NULL, slug TEXT NOT NULL, sort_order INTEGER NOT NULL DEFAULT 0,
  created_at TEXT NOT NULL, updated_at TEXT NOT NULL, UNIQUE (board_id, slug)) STRICT;
CREATE TABLE tags (id TEXT PRIMARY KEY NOT NULL, name TEXT NOT NULL, slug TEXT NOT NULL UNIQUE, created_at TEXT NOT NULL) STRICT;
CREATE TABLE post_tags (post_id TEXT NOT NULL REFERENCES posts(id) ON DELETE CASCADE,
  tag_id TEXT NOT NULL REFERENCES tags(id) ON DELETE CASCADE, PRIMARY KEY (post_id, tag_id)) STRICT, WITHOUT ROWID;
ALTER TABLE posts ADD COLUMN category_id TEXT REFERENCES categories(id) ON DELETE SET NULL;
ALTER TABLE posts ADD COLUMN is_notice INTEGER NOT NULL DEFAULT 0;
ALTER TABLE posts ADD COLUMN is_secret INTEGER NOT NULL DEFAULT 0;
-- 인덱스: posts(board_id, category_id), posts(board_id, is_notice) WHERE is_notice = 1, post_tags(tag_id)
```

- 분류 slug 는 이름에서 만든다(한글 그대로, 기존 `slug::from_title` 규칙). 태그 slug 는 소문자·공백→`-`·앞 `#` 제거.
- 쓰지 않게 된 태그는 지우지 않는다(태그 목록에서 글 수 0 은 숨김).

## 2. core 서비스 (권한·감사는 여기서만)

| 함수 | 권한 | 비고 |
|---|---|---|
| `categories::list(board)` | board.list | 정렬 순 |
| `categories::create / rename / reorder / delete` | structure.edit | 같은 이름 다시 만들면 기존 것을 돌려준다(멱등). 지우면 글의 분류는 비워진다. 모두 감사 기록 |
| `tags::list()` | site.view | 공개 글 수 포함, 0 개는 빼고 |
| `posts` 입력 확장 | | `NewPost`·`PostPatch` 에 `category`(slug), `tags`(문자열 배열), `notice`, `secret` |
| 공지 지정 | post.publish | 권한 없으면 `Forbidden` + 다음 명령 안내 |
| 비밀글 읽기 | 작성자 본인 또는 post.moderate | 그 외에는 `SecretPost` 에러(제목·게시판만 담음) |
| `public_posts(board, page, category?)` | | 공지는 **모든 쪽 맨 위**에 따로(`notices`), 일반 목록에서는 빠진다 |
| `public_posts_by_tag(tag, page)` | | 비밀글은 🔒 제목만, 요약·대표 이미지 없음 |

태그 규칙: 글당 20개, 태그당 30자, 중복·빈 값 제거, 순서 유지. 감사 diff 에 분류·태그·공지·비밀 변화를 남긴다.

## 3. 다섯 경로 (같은 서비스)

- **REST**: `GET/POST /api/v1/boards/{board}/categories`, `PATCH/DELETE /api/v1/boards/{board}/categories/{slug}`, `GET /api/v1/tags`, 글 생성·수정에 새 필드, 글 목록에 `?category=&tag=`.
- **CLI**: `maru category list|add|rename|move|remove --board <b>`, `maru tag list`, `maru post create/update --category <c> --tag <t>... --notice --secret`. 전부 `--json`, `--yes`, 멱등, 에러에 다음 명령.
- **관리 화면(발행 서랍)**:
  - 분류: 게시판에 따라 바뀌는 고르기 + 「새 분류」 그 자리 추가(권한 있을 때)
  - 태그: 칩 입력(Enter·쉼표로 확정, 기존 태그 자동 완성, ×로 빼기)
  - 공개 범위: 공개 / 비밀글 / 비공개(= 숨김) 라디오
  - 공지로 등록: 체크(권한 있을 때만 보임)
  - 스크립트 없으면 평범한 폼 칸으로 동작(지금 서랍과 같은 원칙). `editor/src/publish.ts` 수정 → npm test·build·번들 커밋
- **MCP**: `crates/agent` 가 아직 없다(Phase 3). 서비스·스키마만 맞춰 둔다.
- 분류 이름 바꾸기·순서·지우기 화면은 관리 화면 전체 작업(단위 13)에서. 그 전에는 CLI·REST.

## 4. 공개 화면과 새는 곳 막기

- 게시판 목록: 분류 탭(전체 + 분류들, `?category=`), 공지 줄(배지), 비밀글 🔒.
- 글 화면: 분류 링크, 태그 링크. 새 `tag.html` (`/tags/{slug}`), 태그 모음 `/tags`.
- 비밀글 본문이 새지 않아야 하는 곳 — 전부 테스트로 막는다:
  공개 글 화면 · REST 글 조회(손님 토큰) · RSS · sitemap · `<meta description>`/OG · 목록 요약·대표 이미지 · 태그 목록 · 미리보기 링크 ·
  **G3 ETag/304**: 비밀글 응답은 사람마다 다르므로 `Cache-Control: private, no-store` 로 ETag 를 붙이지 않는다.
- 비밀글을 손님이 열면 200 이 아니라 403 + 「비밀글입니다」 화면(로그인 안내). 글 존재 자체는 숨기지 않는다(그누보드와 같다).

## 5. 작업 단위와 순서 (각 단위 게이트 통과, 커밋은 사용자 확인 뒤)

| 단위 | 내용 | 테스트 |
|---|---|---|
| T1 | 마이그레이션·모델·storage 쿼리, `cargo sqlx prepare` | storage: 올리고 내리기, 분류 지우면 글 분류 NULL |
| T2 | core 서비스(분류·태그·공지·비밀 규칙, 감사) | services: 권한(손님·작성자·편집자), 멱등, 태그 정규화, 공지 정렬, 비밀글 읽기 규칙, 감사 diff |
| T3 | REST·CLI | e2e: CLI·REST 같은 답, `--json` 계약, 403 + 다음 명령 |
| T4 | 공개 테마(목록·글·태그 화면, 새는 곳 차단) | e2e: §4 목록 전부 손님으로 확인, 공지 고정, 분류 필터, 태그 페이지 |
| T5 | 발행 서랍(분류·태그 칩·공개 범위·공지) | editor npm test, e2e 관리 경로, **브라우저 확인 → 사용자 확인 뒤 커밋** |
| T6 | 문서: ADR(비밀글 의미·태그 범위 — 번호는 G7 의 0041·0042 다음 0043~), GAP_PLAN C1~C4 ✅, SPEC 데이터 모델 맞춤 | 게이트 |

완료 기준: 편집자가 서랍에서 분류·태그·공지·비밀글을 고르고 발행 → DB 에 `category_id`·`post_tags`·`is_notice`·`is_secret` 이 남고 →
손님 화면에서 공지 고정·분류 필터·태그 페이지가 보이며, 비밀글 본문은 §4 어느 경로로도 보이지 않는다.
