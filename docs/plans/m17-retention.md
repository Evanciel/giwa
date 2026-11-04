# M17 개인정보 보유기간 파기·휴면 회원

## 배경

- 탈퇴는 이미 그 자리에서 익명화한다(ADR-0051). 남은 것: 오래 안 쓴 회원, 다 보낸 편지, 모듈이 쌓은 오래된 기록.
- 2023-09-15 개인정보보호법 개정으로 「1년 미이용 시 파기·분리 보관(유효기간제)」 의무가 없어졌다. 그래서 휴면은 **켜는 설정(기본 끔)**이다. 운영자가 약관으로 정한 경우에 쓴다.
- 접속 기록(감사 기록)은 안전성 확보 조치 기준상 1년 이상 남겨야 하므로 이번에 지우지 않는다.

## 결정 (추천안대로)

| 항목 | 결정 |
|---|---|
| 휴면 설정 | `dormant_days` 0(끔) 또는 180~3650. 회원 관리 권한 |
| 마지막 로그인 | `members.last_login_at` 을 `start_session` 에서 적는다. 마이그레이션이 감사의 `auth.login` 으로 채운다 |
| 미리 알림 | 휴면 30일 전에 편지(`mail.dormant_notice`), `members.dormant_notice_at` 에 적는다. 알린 뒤 30일이 지나야 휴면 |
| 분리 보관 | `member_dormant(member_id, email, dormant_at)` 로 메일 주소를 옮기고 `members.email` 은 `dormant+<id>@invalid`. 세션을 끊는다 |
| 대상 | 쓰는 중(active)이고 탈퇴하지 않은, 직원 역할(rank > member)이 없는 회원 |
| 깨우기 | 로그인 경로는 모두 `start_session` 을 지나므로 거기서 메일 주소를 되돌린다. 메일로 찾기(`member_by_email`)는 분리 표도 본다 — 비밀번호 로그인·재설정 메일·중복 가입 확인이 그대로 된다 |
| 휴면 중 편지 | `@invalid` 주소에는 쌓지 않는다(탈퇴 회원에도 해당) |
| 편지함 보존 | `mail_retention_days` 7~3650, 기본 90. 보냈거나 실패한 편지를 지운다(1회용 링크가 본문에 있다) |
| 모듈 정리 | `Module::purge(ctx, now)` 기본 구현은 아무것도 안 함. 알림 모듈은 설정 `keep_days`(기본 180) 지난 알림을 지운다 |
| 실행 | 서버가 하루 한 번. `maru privacy run`, `POST /api/v1/privacy/run` 으로 바로 |
| 보이기 | `MemberView` 에 `dormant`·`last_login_at`. 관리 화면 회원 표에 「휴면」·마지막 로그인, 설정 화면에 두 칸 |

## 순서

1. 마이그레이션(열 둘·표 하나, down) → 모델·Store/WriteTx → sqlite
2. 코어 `services/retention.rs`, settings 두 칸, auth `start_session` 깨우기, mail 가드
3. 모듈 `purge` 훅, 알림 모듈 구현
4. 웹: 정리 루프, REST, 관리 화면(설정·회원 표) / CLI `privacy run`, settings 인자
5. 시험: storage/tests/retention.rs, e2e_privacy.rs, 브라우저
