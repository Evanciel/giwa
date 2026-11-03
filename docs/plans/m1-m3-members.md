# 계획: 회원 가입·로그인·메일 인증·내 정보 (GAP M1·M2·M3·S5) — 2026-11-03

> 근거: `docs/GAP_PLAN.md` §3 M1·M2·M3, §4 S5, 진행 순서 단위 8. C5 는 74f79ce 에서 끝났다.
> 예상 토큰: 약 80만.

## 확인한 사실

- `members`(email NOCASE UNIQUE, display_name, password_hash NULL 가능, status active|disabled), `roles` 시드에
  `member`(rank 10: 읽기·쓰기·댓글·올리기)가 이미 있다. `posts.author_id` → `members.id`.
- 로그인은 운영자용 `/admin/login`·로그인 링크(`login_tickets`, 10분 1회)·세션 쿠키 `maru_session`(HttpOnly, Lax) 이 있다.
  `auth::login_password` 는 없는 계정도 더미 해시로 같은 시간을 쓴다.
- 공개 화면은 손님 기준으로 그려 ETag(본문 해시)·`no-cache` 로 나간다. 머리글이 사람마다 달라지면 이 전제가 깨진다.
- 메일 발송 크레이트(lettre 등)·TLS 크레이트는 Cargo.lock 에 없다 → **SMTP 는 새 다운로드 승인이 필요**하다.
- members 표를 다시 만들면 `ON DELETE CASCADE` 로 세션·역할이 지워질 수 있다 → 열 더하기(`ADD COLUMN`)만 쓴다.

## 0. 먼저 정할 것 (추천안대로 진행)

| # | 질문 | 추천 | 다른 안 |
|---|---|---|---|
| D1 | 가입 방식 | **사이트 설정 `signup`: `off`(기본) · `open`.** 커뮤니티 프리셋만 `open`. 가입하면 `member` 역할 | 관리자 승인제 — 필요해지면 `approval` 을 더한다 |
| D2 | 메일 인증 | **`signup_verify`(기본 켬): 메일 링크(24시간, 1회)를 누르기 전에는 로그인 못 한다.** 인증 전 로그인 시도는 비밀번호가 맞으면 새 링크를 다시 보낸다 | 인증 없이 바로 가입 |
| D3 | 계정 있는지 새는 것 | **가입·비밀번호 찾기는 언제나 같은 답(「메일을 보냈습니다」).** 이미 있는 이메일로 가입하면 「이미 가입돼 있습니다, 비밀번호 찾기」 메일을 보낸다 | 「이미 있는 이메일」 바로 알림 — 편하지만 회원 여부가 샌다 |
| D4 | 닉네임 | **닉네임(`display_name`)은 사이트 안에서 겹치지 않게(대소문자 무시), 2~20자.** 운영자 이름과 같은 닉네임 막기 포함 | 겹쳐도 됨 |
| D5 | 탈퇴 | **익명화: 이메일·비밀번호·세션을 지우고 닉네임은 「탈퇴한 회원 #xxxx」, 상태 비활성. 쓴 글은 남는다.** 운영자(owner)는 탈퇴 못 한다 | 글까지 지우기 — 게시판 흐름이 끊긴다 |
| D6 | 메일 보내기(S5) | **보낼 편지함(`mail_outbox`) + 전송 방식 설정.** 지금은 `file`(데이터 디렉터리 `mail/` 에 .eml 저장, 기본)만. SMTP 는 lettre 승인 뒤. 편지 본문(링크 포함)은 REST 로 내보내지 않고 로컬 CLI(`maru mail list/show`)로만 본다 | 처음부터 SMTP — 다운로드 승인 필요 |
| D7 | 머리글 | **「내 계정」 링크 하나(가입이 켜져 있을 때).** 누르면 로그인했으면 `/me`, 아니면 `/login`. 공개 화면은 계속 손님 기준이라 ETag·캐시가 그대로다. 사람마다 다른 머리글은 댓글(M5) 때 다시 본다 | 로그인 상태별 머리글 — 모든 공개 응답이 `private` 이 된다 |

## 1. 데이터 (마이그레이션 1개 + down)

- `members`: `email_verified_at TEXT`, `withdrawn_at TEXT` 더하기. 기존 회원은 `email_verified_at = created_at`.
  `display_name` NOCASE 고유 색인.
- `login_tickets`: `purpose TEXT NOT NULL DEFAULT 'login' CHECK (purpose IN ('login','verify','reset'))`.
  열린 표 지우기·쓰기는 목적별로.
- `mail_outbox(id, idem_key UNIQUE, to_addr, subject, body, status queued|sent|failed, attempts, last_error, created_at, sent_at)`.
- down: 색인·열·표를 거꾸로 지운다.

## 2. core

- `services::members`: `signup`, `verify_email`, `request_reset`, `reset_password`, `update_profile`(닉네임),
  `change_password`(지금 비밀번호 확인), `withdraw`(비밀번호 확인), 관리용 `list`·`get`·`set_status`·`set_role`(member.manage).
  모든 쓰기는 감사(`member.signup`·`member.verify`·`member.reset`·`member.update`·`member.withdraw`·`member.status`·`member.role`), 비밀 값은 감사에 남기지 않는다.
- `auth::login_password`: 인증 안 된 회원은 `error.email_unverified`(비밀번호가 맞을 때만 알려 주고 새 링크 발송).
- `services::mail`: `queue(tx, idem_key, to, subject, body)`(같은 트랜잭션), `deliver_pending(ctx, transport)`(재시도 5회, 실패는 `failed` 로 보관).
  편지 본문은 `messages` 키 + `{link}` 자리. 전송 방식은 `Mailer` 트레이트(core) — `FileMailer`(cli/web 가 데이터 디렉터리로 만든다).
- 설정 `SiteSettings.signup`(`off`|`open`), `signup_verify`(기본 true).

## 3. 다섯 경로

- **공개 화면(테마 틀)**: `/login`, `/signup`, `/logout`(POST), `/forgot`, `/reset/{token}`, `/verify/{token}`, `/me`(닉네임·비밀번호·내 글·탈퇴).
  CSRF 는 관리 화면과 같은 방식(로그인 전 `maru_csrf` 쿠키, 뒤 세션 값). 가입·찾기·로그인은 요청 제한 `Login` 칸. `noindex`.
- **REST**(member.manage): `GET /api/v1/members`, `GET /members/{id}`, `PATCH /members/{id}`(status·role). 메일 본문 없음.
- **CLI**: `maru member list|show|disable|enable|role`, `maru mail list|show|send`, `maru site set --signup open --signup-verify false`.
- **MCP**: Phase 3.

## 4. 작업 단위

| 단위 | 내용 | 테스트 |
|---|---|---|
| T1 | 마이그레이션·storage(회원 열, 목적별 티켓, 편지함), sqlx prepare | storage: 올리고 내리기, 목적 섞이지 않음 |
| T2 | core `mail`·`members`·`auth` 고치기, 설정 | 저장소 테스트: 가입→인증→로그인, 같은 답(D3), 닉네임 겹침, 찾기·재설정 1회, 탈퇴 익명화, owner 탈퇴 거부, 감사에 비밀 없음 |
| T3 | 웹: 공개 계정 화면·틀·CSS, 메일 배달 주기 | e2e(실제 바이너리): 가입 폼 → `mail/` 의 링크 → 인증 → 로그인 → `/me` 닉네임 바꾸기 → DB 반영, CSRF 없는 POST 거부, 가입 꺼짐이면 404 |
| T4 | REST·CLI | e2e: `member list` 와 REST 같은 답, 비활성 → 세션 끊김 |
| T5 | 브라우저 확인(데스크톱·375px), ADR-0051, GAP_PLAN | 게이트 |

## 위험과 대응

- **SMTP 없음:** 운영 사이트는 메일을 받지 못한다 → `file` 이면 서버 시작 로그·`maru doctor` 류 안내에 「메일이 파일로만 쌓인다」 를 알리고, lettre 승인을 사용자에게 묻는다.
- **계정 탈취:** 재설정 링크는 해시만 저장, 1회, 1시간. 재설정·비밀번호 변경·비활성은 그 회원의 모든 세션을 끊는다.
- **가입 남용:** 요청 제한 + 같은 이메일 메일은 10분에 한 통(멱등 키). 캡차는 M10.

완료 기준: 가입이 켜진 사이트에서 손님이 `/signup` 에 이메일·닉네임·비밀번호를 넣으면 인증 메일이 편지함에 쌓이고,
링크를 누른 뒤 로그인해 `/me` 에서 닉네임을 바꾸면 DB `members.display_name` 이 바뀐다. 같은 이메일로 다시 가입해도 화면은 같은 답을 준다.

## 진행 (2026-11-03)

- T1~T5 완료. ADR-0051.
- 브라우저(데스크톱·375px): 데모 사이트 `site set --signup open` → 손님으로 가입 → 「메일을 확인하세요」 → `mail/*.eml` 링크 → 확인 화면 → 인증하고 로그인 → `/me?done=verified`. 가로 넘침 없음. 375px 에서 「내 계정」 이 혼자 왼쪽 줄로 내려가던 것을 오른쪽에 붙였다. 편지 문구 「{사이트} 에」 띄어쓰기를 고쳤다.
- 남은 것: SMTP 전송(lettre 승인), 마이페이지 댓글·스크랩(M5·M7), 캡차(M10).

