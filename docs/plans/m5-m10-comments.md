# 단위 9 — 댓글·비회원 댓글·자체 캡차 (GAP M5·M6·M10)

> 2026-11-03. 단위 8(회원 가입) 위에 얹는다. 결정 표는 추천안으로 진행(사용자 지시).

## 1. 결정

| # | 결정 | 추천안(채택) | 대안 |
|---|---|---|---|
| D1 | 댓글을 어디에 | **따로 `comments` 표.** `posts` 에는 공개 댓글 수 `comment_count` 만 더한다 | SPEC §4 처럼 `posts(kind=comment)` — 목록·검색 색인·피드·이전/다음·주소 UNIQUE 쿼리마다 kind 조건이 필요하고, `actor_type` CHECK 에 손님이 없어 표를 다시 만들어야 한다 |
| D2 | 깊이 | 댓글 + 대댓글 한 단계. 대댓글에 답하면 같은 뿌리 아래 붙고 「@이름」 으로 누구에게인지 보인다 | 무한 트리 — 모바일에서 들여쓰기가 무너진다 |
| D3 | 본문 | 평문(최대 2000자), 줄바꿈 유지, 템플릿 자동 이스케이프. 마크다운·HTML 없음 | 마크다운 — 손님 입력에 링크·이미지 스팸 표면이 커진다 |
| D4 | 게시판 정책 | `comment`(기본 `member`, `guest`·`none` 가능), `comment_approval`(`off`·`guest`·`all`, 기본 `guest`) | 승인 기본 끔 — 비회원 스팸이 바로 보인다 |
| D5 | 비회원 | 이름(2~20자, 예약어 금지) + 비밀번호(4자 이상, argon2id). 수정·삭제는 그 비밀번호로. IP 는 저장하지 않는다(M9 에서 결정) | IP 해시 저장 — IPv4 는 해시를 되돌리기 쉬워 개인정보가 된다 |
| D6 | 공개 글 화면 캐시 | 글 화면은 계속 손님으로 렌더(ETag 유지). 댓글 폼은 화면마다 같은 내용 | 로그인 상태별 화면 — 304 가 깨진다 |
| D7 | CSRF | 댓글 폼은 화면별 토큰이 없다(D6). **세션 쿠키 SameSite=Lax + `Sec-Fetch-Site`/`Origin` 같은 출처 확인**. 손님 요청은 신원이 없어 CSRF 대상이 아니고 캡차가 막는다 | 화면마다 토큰 — D6 과 충돌 |
| D8 | 캡차 | **자체 질문 캡차**: 한글 셈 문제(「일곱 더하기 다섯은?」, 답은 숫자). 1회용 도전(`challenges` 표, 해시만, 10분), 숨은 함정 칸, 폼을 연 지 2초 안 제출 거부. 손님 댓글은 제출 뒤 **확인 단계 화면**에서 풀고(글 화면 캐시 유지), 가입 폼에는 바로 넣는다. 설정 `captcha`(`off`·`guest`·`all`, 기본 `guest`) | 그림 캡차 — 화면 낭독기로 못 푼다. 외부 서비스(reCAPTCHA 등) — 원칙 5·6 |
| D9 | 수정·삭제 | 댓글마다 「수정」·「삭제」 링크 → `/c/{id}/edit`·`/c/{id}/delete` 확인 화면(no-store, 진짜 세션으로 렌더). 회원은 자기 것, 손님은 비밀번호, 관리자는 전부. 지울 때 대댓글이 있으면 「삭제된 댓글입니다」 자리만 남기고 본문은 비운다 | 글 화면 안 버튼 — D6 때문에 누구 것인지 모른다 |
| D10 | 관리 | 승인 대기 큐: 관리 화면 `/admin/comments`, REST `GET /api/v1/comments?status=pending`·`POST /comments/{id}/{approve|spam|hide|delete}`, CLI `maru comment list|approve|spam|hide|delete`. 에이전트 응답은 `untrusted: true` + 본문 별도 필드(SPEC §5.3) | |

## 2. 데이터

- `comments(id, post_id FK cascade, parent_id FK, reply_to_id, status CHECK(published|pending|hidden|spam|deleted), body, author_id FK SET NULL, guest_name, guest_pw_hash, actor_type CHECK(human|guest|local), created_at, updated_at)` + 인덱스 `(post_id, status, created_at)`, `(status, created_at)`.
- `posts.comment_count INTEGER NOT NULL DEFAULT 0` — 공개 댓글 수, 댓글 쓰기와 같은 트랜잭션에서 다시 센다.
- `challenges(id_hash PK, answer, purpose, expires_at)` — 쓸 때 꺼내 지운다(`DELETE … RETURNING`), 만들 때 만료분 정리.
- 게시판 정책에 `comment`·`comment_approval`, 설정에 `captcha`.

## 3. 다섯 경로

- **공개 화면**: 글 아래 댓글 목록·폼(`POST /b/{board}/{post}/comments`), 손님 확인 단계, `/c/{id}/edit|delete`. 목록에 `[3]` 댓글 수.
- **관리 화면**: `/admin/comments` 대기 큐.
- **REST**: 공개 `GET /api/v1/posts/{id}/comments`(손님 권한으로 보이는 것), 관리 `GET /api/v1/comments`, `POST /api/v1/comments/{id}/{action}`.
- **CLI**: `maru comment list [--status pending] [--post ID]`, `maru comment approve|spam|hide|delete <id>`.
- **MCP**: Phase 3 (`list_pending_comments`, `moderate_comment`).

## 4. 작업 단위

| 단위 | 내용 | 테스트 |
|---|---|---|
| T1 | 마이그레이션·storage | 올리고 내리기 |
| T2 | core `comments`·`captcha`, 정책·설정 | 저장소 테스트: 회원 댓글 바로 공개, 손님 댓글 대기→승인, 정책 none·member 에서 손님 거부, 대댓글 한 단계, 비밀번호 수정·삭제, 대댓글 있는 삭제는 자리만, 개수, 캡차 1회·만료·오답, 감사 |
| T3 | 웹(글 화면·확인 단계·수정/삭제·관리 큐), CSS | e2e: 손님 댓글 → 캡차 → 대기 → CLI 승인 → 글 화면에 보임 → DB, 다른 출처 POST 거부, 함정 칸 거부, 회원 댓글 바로 공개 |
| T4 | REST·CLI | e2e: REST 목록과 CLI 같은 답, 에이전트 응답 untrusted |
| T5 | 브라우저(데스크톱·375px), ADR-0052, GAP | 게이트 |

완료 기준: 손님이 글에 댓글을 쓰면 셈 문제를 풀고, 승인 대기에 들어가며, 운영자가 승인하면 글 화면에 보이고 `posts.comment_count` 가 1 늘어난다.

## 진행 (2026-11-04)

- T1~T5 완료. ADR-0052.
- 계획과 다르게: `posts.comment_count` 대신 `GROUP BY` 집계, `challenges.purpose` 대신 `issued_at`(2초 규칙), 캡차 `all` 단계 없음.
- 브라우저(데스크톱·375px): 데모 `board update blog --comment guest` → 회원으로 댓글(바로 공개) → 로그아웃 → 손님 댓글 → 확인 질문 화면 → 정답 → 「관리자가 확인한 뒤에 보입니다」 → `/admin/comments` 승인 → 글 화면에 보임. 가로 넘침 없음. 관리 큐의 상태가 영어(`pending`)로 보이던 것을 한국어로, **승인만 했는데 「· 고침」 이 붙던 버그**를 고쳤다(상태 변경이 `updated_at` 을 바꿨다 → e2e 에 회귀 검사).
- 남은 것: 비회원·회원 공개 글쓰기 폼(M6 나머지), 마이페이지 「내 댓글」(M3), MCP 툴(Phase 3).
