# 계획: 서체 올리기 (사이트 운영자가 폰트를 직접 추가) — 2026-11-03

> 근거: ADR-0047(내장 서체 넷), ADR-0034(꾸밈 지시문 허용 목록), 원칙 1(다섯 경로가 같은 서비스)·4(changeset)·7(재배포 가능 자산).
> 사용자 요청: 「cms 내 기능으로 폰트 알아서 추가할 수 있게」. 내장 넷(명조·손글씨·붓글씨·고정폭)은 그대로 두고, 운영자가 올린 서체를 더한다.
> 예상 토큰: 약 40만.
> **진행:** 2026-11-03 추천안대로 구현(ADR-0048). 달라진 점 — REST·관리 화면의 파일 올리기는 multipart 대신 `/media` 와 같은
> 원본 바이트 + 쿼리, 관리 화면 올리기는 스크립트 필요(없으면 CLI 안내), CLI 는 묻지 않고 `--yes` 가 권리 확인.
> `docs/SPEC.md` 는 읽기 전용이라 §8.2 메모를 넣지 않았다(ADR-0048 이 대신한다).

## 0. 먼저 정할 것 (사용자 결정)

| # | 질문 | 추천 | 다른 안 |
|---|---|---|---|
| D1 | 올린 서체가 글 안에서 어떻게 정화기(ammonia)를 통과하나 | **모양으로 허용**: `:font[글자]{slug}` 의 값이 내장 넷이면 지금처럼, 아니면 slug 모양(`[a-z][a-z0-9-]{1,31}`)일 때 `class="ff-{slug}"` 로 낸다. 렌더는 DB 를 보지 않는다(순수 함수 유지). 서체를 지우면 그 class 의 CSS 가 없어져 **기본 서체로 떨어질 뿐 글은 그대로** | 렌더할 때마다 DB 의 서체 목록으로 허용 목록을 만든다 → 렌더에 DB 가 필요해지고, 저장해 둔 HTML 이 서체 추가·삭제 때마다 낡는다 |
| D2 | 에이전트가 서체를 올릴 때 changeset 을 거치나 | **지금은 디자인 토큰과 같게** `design.edit` 권한으로 바로 적용 + 감사 기록. Phase 3 에서 디자인 변경 전체를 changeset 으로 감쌀 때 서체 추가·삭제도 같이 넣는다(SPEC §15 로드맵) | 지금 서체만 따로 changeset → changeset 코어가 아직 없어 이 작업이 커진다 |
| D3 | 파일 형식과 크기 | **woff2·woff·ttf·otf 를 받은 그대로 낸다**(새 의존성 없음). 한 파일 20MB 까지, 3MB 넘으면 「느릴 수 있음 — woff2 로 줄여 올리세요」 경고와 `scripts/subset-fonts.py` 안내 | 서버에서 woff2 로 바꾸고 한글만 남긴다 → Rust 쪽 woff2 인코더·서브셋 크레이트를 새로 들여야 한다(라이선스·품질 확인 필요). 나중 단위로 미룬다 |
| D4 | 라이선스를 어떻게 확인하나 | **필수 칸 두 개**: 라이선스 고르기(SIL OFL 1.1 · Apache-2.0 · MIT · CC0 · 「기타 — 웹폰트 사용권을 직접 확인함」) + 「이 서체를 웹에 올려 내보낼 권리가 있습니다」 체크. 라이선스 전문(선택)을 붙이면 `/fonts/{slug}/LICENSE.txt` 로 함께 낸다(OFL 은 배포 시 전문 동봉을 요구) | 확인 없이 받는다 → 운영자가 상용 폰트를 무심코 공개 배포할 수 있다 |
| D5 | 사이트 전체 본문·제목 서체로도 쓰나 | **쓴다.** `font.body`·`font.heading` 토큰에 올린 서체 slug 를 넣을 수 있게 한다. 지우려는 서체가 토큰에 쓰이고 있으면 `FontInUse` 에러 + 「먼저 `maru design set font.body Pretendard`」 안내 | 글 안 부분 꾸밈에만 쓴다 → 「우리 회사 서체로 사이트 전체」라는 가장 흔한 바람을 못 채운다 |
| D6 | 굵기(보통·굵게) | **데이터는 처음부터 서체 하나에 파일 여러 개**(굵기 100~900 · 기울임). 첫 화면은 「파일 추가」로 굵기를 하나씩 더한다. 굵게 파일이 없으면 브라우저가 흉내 낸 굵게(한글은 번져 보임)로 나간다고 화면에 알린다 | 굵기 하나만 → 본문 서체로 쓰면 제목·굵게가 뭉개진다. 나중에 표를 바꾸는 비용이 더 크다 |

아래는 추천안 기준이다.

## 1. 데이터 (마이그레이션 1개 + down)

```sql
CREATE TABLE fonts (
  id TEXT PRIMARY KEY NOT NULL,
  slug TEXT NOT NULL UNIQUE,              -- ff-{slug}, 토큰 값. 내장 넷·'none' 은 예약어
  name TEXT NOT NULL,                     -- 메뉴에 보일 이름 (예: 「회사 고딕」)
  fallback TEXT NOT NULL DEFAULT 'sans',  -- sans | serif | mono | cursive : 못 받았을 때 떨어질 계열
  license TEXT NOT NULL,                  -- OFL-1.1 | Apache-2.0 | MIT | CC0-1.0 | other
  license_path TEXT,                      -- 붙인 라이선스 전문 blob (선택)
  created_at TEXT NOT NULL, updated_at TEXT NOT NULL) STRICT;
CREATE TABLE font_files (
  id TEXT PRIMARY KEY NOT NULL,
  font_id TEXT NOT NULL REFERENCES fonts(id) ON DELETE CASCADE,
  weight INTEGER NOT NULL DEFAULT 400 CHECK (weight BETWEEN 100 AND 900),
  italic INTEGER NOT NULL DEFAULT 0,
  format TEXT NOT NULL,                   -- woff2 | woff | ttf | otf (첫 바이트로 판정한 값)
  path TEXT NOT NULL,                     -- blob: fonts/{slug}/{sha256 앞 16자}.{ext}
  size INTEGER NOT NULL, sha256 TEXT NOT NULL,
  created_at TEXT NOT NULL, UNIQUE (font_id, weight, italic)) STRICT;
```

- blob 경로는 내용 해시라 파일을 바꾸면 URL 도 바뀐다 → 1년 `immutable` 캐시를 그대로 쓸 수 있다.
- 같은 서체·굵기·기울임으로 다시 올리면 파일을 바꾼다(옛 blob 은 트랜잭션 커밋 뒤 지운다). 같은 해시면 아무것도 안 한다(멱등).

## 2. core 서비스 (`crates/core/src/services/fonts.rs`, 권한·감사는 여기서만)

| 함수 | 권한 | 비고 |
|---|---|---|
| `fonts::list(ctx)` | site.view | 공개 CSS·에디터 메뉴용. 파일·굵기 포함 |
| `fonts::add(ctx, actor, NewFont)` | design.edit | slug 를 주지 않으면 이름에서 만든다(한글이면 `font-1`…). 예약어·모양 검사. 같은 slug 가 있으면 이름·라이선스만 맞춰 돌려준다(멱등) |
| `fonts::add_file(ctx, actor, slug, weight, italic, bytes)` | design.edit | 첫 바이트로 형식 판정(`wOF2`·`wOFF`·`\0\1\0\0`/`true`·`OTTO`), 확장자 믿지 않음. 20MB 초과 `TooLarge`. blob 먼저 쓰고 DB 실패 시 blob 삭제(`files.rs` 와 같은 순서) |
| `fonts::update(ctx, actor, slug, FontPatch)` | design.edit | 이름·대체 계열·라이선스 |
| `fonts::remove_file / remove(ctx, actor, slug, …)` | design.edit | 토큰에서 쓰는 중이면 `FontInUse`. 글 안 `:font[]{slug}` 는 막지 않는다(기본 서체로 떨어짐, D1). 응답에 「이 서체를 쓰는 글 N개」를 알려 준다 |
| `fonts::css(ctx)` | — | `@font-face` + `.ff-{slug}` 규칙 문자열. 서체 이름은 설치된 글꼴과 겹치지 않게 `"maru-u-{slug}"` |

- 모두 `tx.audit(actor, "font.add" | "font.file.add" | "font.update" | "font.remove", Target::new("font", slug), diff)`.
- `design::update` 의 `font_stack` 검사에 올린 서체를 더한다: 내장 FONTS 에 없으면 DB 의 slug 를 찾아 `"maru-u-{slug}", {대체 계열 스택}` 을 만든다. `to_css` 는 이 스택을 받도록 인자를 늘린다.
- `directives.rs`: `font` 값 검사를 「내장 넷 또는 slug 모양」으로. 다른 지시문(color·mark·size)은 그대로 고정 목록. `allowed_classes()` 는 정해진 목록이라 정화기 `attribute_filter` 에서 `ff-` 접두사 + slug 모양만 따로 통과시킨다(다른 접두사는 지금처럼 목록 대조).

## 3. 다섯 경로 (같은 서비스)

- **REST**: `GET /api/v1/fonts`, `POST /api/v1/fonts`(이름·라이선스), `POST /api/v1/fonts/{slug}/files`(multipart: file·weight·italic), `PATCH /api/v1/fonts/{slug}`, `DELETE /api/v1/fonts/{slug}`, `DELETE /api/v1/fonts/{slug}/files/{weight}[?italic=1]`.
- **CLI**: `maru font add <파일> --name "회사 고딕" --license OFL-1.1 [--slug company-gothic] [--weight 700] [--italic] [--license-file OFL.txt] --yes`,
  `maru font list`, `maru font rename|remove <slug>`. 전부 `--json`, 멱등, 에러에 다음 명령(예: 라이선스 빠짐 → `--license OFL-1.1` 예시).
  `--yes` 없이는 「이 서체를 웹에 내보낼 권리가 있습니까」를 묻고, 비대화형이면 `--yes` 로 D4 체크를 대신한다.
- **관리 화면** `/admin/fonts`(새 쪽, 머리 메뉴에 「서체」):
  - 목록: 서체마다 「가나다 ABC 123 다람쥐 헌 쳇바퀴에 타고파」 미리보기, 굵기 칩, 크기, 라이선스, 「본문에 쓰기」·「제목에 쓰기」 버튼(D5), 지우기
  - 올리기 폼: 파일 · 이름 · 굵기 · 라이선스 고르기 · 전문(선택) · 권리 체크 → 올린 직후 그 서체로 미리보기
  - 스크립트 없이도 평범한 multipart 폼으로 동작. 미리보기만 스크립트(`FontFace` 로 바로 읽기)
- **에디터 서체 메뉴**: `edit.html` 이 `<script type="application/json" id="maru-fonts">` 로 목록을 싣는다. `main.ts` 의 SPAN_MENU.font 는 내장 넷 + 올린 서체를 구분선으로 나눠 보여 주고, 항목은 그 서체로 이름을 그린다(지금과 같은 방식). `syntax.ts` 값 정규식에 `-` 추가, `markdown.ts` 의 `font` mark 는 내장 넷 또는 slug 모양을 받는다. 목록에 없는 slug(지워진 서체)는 mark 를 지우지 않고 메뉴에 「(지워진 서체)」로 보인다 → 저장해도 글이 바뀌지 않는다.
  관리 화면 `_base.html` 에 `/assets/fonts.css` 를 걸어 편집 중에도 실제 서체로 보인다.
- **MCP**: `crates/agent` 는 Phase 3. 입력 타입(`NewFont`·`FontPatch`)에 `JsonSchema` 만 맞춰 둔다. 에이전트가 「이 폰트 넣어 줘」를 받으면 CLI `maru font add` 로 할 수 있다.

## 4. 공개 화면 · 캐시 · 보안

- `/assets/fonts.css`: `fonts::css` 결과. `no-cache` + ETag(내용 해시). 서체가 없으면 빈 CSS. `base.html` 에서 `tokens.css` 다음에 건다.
- `/fonts/{slug}/{hash}.{ext}`: blob 을 낸다. `Content-Type` 은 판정한 형식(`font/woff2`·`font/woff`·`font/ttf`·`font/otf`), `public, max-age=31536000, immutable`, `X-Content-Type-Options: nosniff`. 경로는 DB 에 있는 것만(사용자 입력으로 blob 경로를 만들지 않는다).
- `/fonts/{slug}/LICENSE.txt`: 붙인 전문(`text/plain; charset=utf-8`). 없으면 404.
- CSP 는 `default-src 'self'` 그대로 — 같은 출처 폰트라 바꿀 것 없음. 외부 웹폰트 URL 연결은 이 계획에 넣지 않는다(외부 서비스 의존 · 원칙 5).
- `font-display: swap` 과 대체 계열 스택으로, 파일을 못 받아도 글은 읽힌다.
- 폰트 파일은 브라우저 폰트 해석기로 들어가므로 형식 판정 외에 `ttf`/`otf` 표 머리(테이블 수·오프셋이 파일 안에 있는지)만 가볍게 검사한다. 깊은 검증은 브라우저(OTS)에 맡긴다.

## 5. 작업 단위와 순서 (각 단위 게이트 통과, 커밋은 사용자 확인 뒤)

| 단위 | 내용 | 테스트 |
|---|---|---|
| T1 | 마이그레이션·모델·storage 쿼리, `cargo sqlx prepare` | storage: 올리고 내리기, 서체 지우면 파일 행도 지워짐, (font, 굵기, 기울임) 유일 |
| T2 | core `fonts` 서비스, `design` 토큰 연결, `directives`·정화기 slug 허용 | services: 권한(손님·글쓴이·디자인 편집자), 형식 판정(확장자 속임 거부), 20MB, 멱등(같은 해시), 예약어, `FontInUse`, 감사 diff. directives: `{company-gothic}` → `ff-company-gothic`, `{A B}`·`{x;}` 는 글자로 남음, 정화기가 `ff-<script>` 류를 버림 |
| T3 | REST·CLI | e2e: `maru font add` → `/assets/fonts.css` 에 `@font-face` → 파일 200·`font/woff2`·immutable → `:font[가]{slug}` 글이 공개 화면에서 `class="ff-slug"`. REST 와 CLI 같은 답, `--json` 계약, 권한 없으면 403 + 다음 명령 |
| T4 | 공개 테마(`base.html` 링크) · 토큰에 올린 서체 · 지운 서체 대체 | e2e: `maru design set font.body <slug>` → `tokens.css` 에 `"maru-u-<slug>"`, 쓰는 중 삭제 거부, 글에만 쓰인 서체 삭제 후 글 화면 200·글자 그대로 |
| T5 | 관리 화면 `/admin/fonts` · 에디터 서체 메뉴 | editor npm test(올린 서체 왕복·지워진 서체 보존), e2e 관리 경로(multipart 업로드 → DB 행), **브라우저 확인 → 사용자 확인 뒤 커밋** |
| T6 | 문서: ADR-0048(올린 서체 — D1·D3·D4 근거), SPEC 디자인 절·데이터 모델, AGENTS 예정 메모에 `maru font add`, `THIRD_PARTY.md` 에 「운영자가 올린 서체는 운영자 책임, 라이선스는 서체마다 DB 에」 한 줄 | 게이트 |

완료 기준: 운영자가 관리 화면(또는 `maru font add`)에서 woff2 하나를 올리면 → DB 에 `fonts`·`font_files` 행과 blob 이 남고 감사 기록이 생기며 →
에디터 서체 메뉴에 그 이름이 그 서체로 보이고, 골라 발행한 글이 손님 화면에서 그 서체로 그려진다. 사이트 본문 서체로 지정하면 전체가 바뀌고,
서체를 지워도 글은 깨지지 않고 기본 서체로 보인다.

## 6. 이 계획에 넣지 않는 것

- 서버 쪽 woff2 변환·한글 서브셋(D3 다른 안) — 수요를 보고 별도 단위
- Google Fonts 등 외부 웹폰트 연결 — 외부 의존(원칙 5)·개인정보(방문자 IP 가 외부로) 문제
- 서체별 줄 간격·자간 보정 값(`size-adjust` 등) — 내장 손글씨·붓글씨처럼 필요하면 서체 편집 칸으로 다음에
