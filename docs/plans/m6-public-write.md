# 단위 9b — 공개 화면 글쓰기 (회원·비회원, GAP M6 나머지)

> 2026-11-04. 단위 9(댓글) 위에 얹는다. 결정 표는 추천안으로 진행(사용자 지시).
> 커뮤니티 테마(로드맵 3단계)의 바탕: 지금은 글을 관리 화면·CLI·REST 로만 쓸 수 있다.

## 1. 결정

| # | 결정 | 추천안(채택) | 대안 |
|---|---|---|---|
| D1 | 경로 | `/b/{board}/write`, `/b/{board}/{post}/edit`, `/b/{board}/{post}/delete` (GET 폼 · POST). no-store, 진짜 세션으로 렌더, noindex | 관리 화면 편집기를 회원에게 열기 — 관리 화면은 운영자용(권한·메뉴가 섞인다) |
| D2 | 서비스 | core `posts::submit`·`revise`·`withdraw`. 공개 화면 전용 비즈니스 로직이 아니라 「게시판 정책대로 자기 글을 바로 올리는」 쓰기다. 사람·손님·로컬만, **에이전트는 거부**(에이전트는 초안 → 발행·changeset 경로) | 기존 `create_draft` + `publish` — 회원에게 발행 권한(`post.publish`)을 줘야 해서 관리 글까지 열린다 |
| D3 | 바로 보일지 | 게시판 정책 `write_approval`(`off`·`guest`·`all`, 기본 `guest`) — 댓글과 같은 `Approval` 형. 승인 대기는 기존 상태 `pending`, 운영자가 `maru post publish`·관리 화면에서 발행 | 늘 바로 공개 — 비회원 스팸이 바로 보인다 |
| D4 | 비회원 글 | 게시판 `write` 가 `guest` 일 때만. 이름 + 비밀번호(댓글과 같은 규칙) + 확인 질문(폼에 바로, 60분). `posts` 에 `guest_name`·`guest_pw_hash` 열을 더하고 `actor_type` 은 `human`(사람이다), `author_id` 는 비운다. 비회원 비밀글·공지는 받지 않는다 | `actor_type` 에 `guest` 추가 — SQLite 는 CHECK 를 바꾸려면 FK 가 걸린 `posts` 를 통째로 다시 만들어야 한다 |
| D5 | 고치기·지우기 | 회원은 자기 글, 손님은 비밀번호, 게시판 관리자는 전부(기존 `authorize_post`). 지우기는 상태 `deleted`(기존 `delete` 와 같음). 손님 글은 누구의 「자기 글」 도 아니다 | |
| D6 | 본문 | 마크다운(관리 글과 같은 렌더러·ammonia). 첨부·사진 올리기는 다음 단위(M4 업로드 권한과 함께) | 평문 — 게시판 글은 문단·목록·링크가 필요하다 |
| D7 | 작성자 표시 | 글 화면·목록에 작성자 이름(회원 닉네임·손님 이름 + 「비회원」). 블로그 게시판(`kind=blog`)은 지금처럼 숨긴다 | |
| D8 | CSRF·남용 | 회원은 세션 CSRF, 손님은 로그인 전 CSRF 쿠키 + 확인 질문 + 함정 칸 + 같은 출처 확인. 요청 제한 `Login` 칸에 `POST /b/*/write|edit|delete` | |

## 2. 데이터

- 마이그레이션 `posts_guest`: `ALTER TABLE posts ADD COLUMN guest_name TEXT; ADD COLUMN guest_pw_hash TEXT;` (down: DROP COLUMN).
- `Post.guest_name`(공개), `Post.guest_pw_hash`(`#[serde(skip)]`).
- 게시판 정책 `write_approval`. `CommentApproval` 을 `Approval` 로 이름을 바꿔 함께 쓴다.

## 3. 다섯 경로

- **공개 화면**: 목록 「글쓰기」 단추(게시판 `write` 가 `guest`·`member` 일 때), 쓰기·고치기·지우기 화면, 글 화면 「고치기·지우기」 링크.
- **REST**: 사람·손님 전용 쓰기라 새 끝점 없음(에이전트는 기존 `POST /posts`). 승인 대기 글은 기존 `GET /posts?status=pending`·`POST /posts/{id}/publish`.
- **CLI**: `maru board update --write-approval`, 대기 글은 `maru post list --status pending` · `maru post publish`.
- **MCP**: Phase 3.

## 4. 작업 단위

| 단위 | 내용 | 테스트 |
|---|---|---|
| T1 | 마이그레이션·storage·모델 | 올리고 내리기 |
| T2 | core `submit`·`revise`·`withdraw`, 정책, 발행이 `pending` 을 받기 | 저장소 테스트: 회원 글 바로 공개·자기 글 고치기·남의 글 거부, 정책 `editor` 에서 회원 거부, 손님은 `guest` 정책·확인 질문·비밀번호, 손님 비밀글 거부, `write_approval` 대기 → 발행, 에이전트 거부, 지우기, 감사 |
| T3 | 웹 화면·템플릿·CSS·작성자 표시 | e2e: 손님 글쓰기 → 확인 질문 → 대기 → CLI 발행 → 목록·글 화면에 이름, 비밀번호로 고치기, 회원 글쓰기 바로 공개 → DB, 다른 출처·함정 칸 거부 |
| T4 | CLI 정책 | e2e 안에서 |
| T5 | 브라우저(데스크톱·375px), ADR-0053, GAP | 게이트 |

완료 기준: 비회원 글쓰기를 연 게시판에서 손님이 이름·비밀번호·확인 질문으로 글을 올리면 (승인 뒤) 목록과 글 화면에 손님 이름으로 보이고, 같은 비밀번호로 고치면 DB 의 `posts.body_md` 가 바뀐다.

## 진행 (2026-11-04)

- T1~T5 완료. ADR-0053.
- 계획과 다르게: 새 글 주소 `/write/{board}`(슬러그 `write` 와 겹침), 서비스 이름 `board_posts::write`(`posts::submit` 은 이미 있음).
- 브라우저(데스크톱·375px): 데모 `board create free --kind free` · `board update free --write guest` · 분류 2개 → 목록 「글쓰기」 → 손님으로 분류·제목·본문·태그·이름·비밀번호·확인 질문 → 「글을 받았습니다」 → 관리 글 목록 「승인 대기」 → 지금 발행 → 목록에 「궁금한손님 · 비회원 · 질문」, 글 화면 목록·고치기·지우기 → 고치기 화면에 분류·태그·비밀번호 칸. 가로 넘침 없음.
- 남은 것: 비회원 비밀글(비밀번호로 열기), 첨부·사진 올리기·서식 도구(M4), 관리 화면에 손님 이름 표시.
