# M19 운영 신호

## 결정 (추천안대로)

| 항목 | 결정 |
|---|---|
| 댓글 급증 | 코어. 한 글에 지난 1시간 댓글(공개·승인 대기)이 `signal_comment_burst`(기본 20, 0 끔, 3~1000)개 이상 |
| 신고 집중·묵은 신고 | 신고 모듈의 `Module::signals`. 설정 `crowd`(기본 3)·`stale_hours`(기본 24), 0 이면 끔 |
| 확장 지점 | `Extensions::signals(ctx, actor)` → 켜진 모듈의 `Module::signals` |
| 보기 권한 | 게시판 관리(`post.moderate`). 다룰 수 없는 게시판의 것은 빠진다. 토큰은 `publish` 스코프 |
| 보이는 곳 | 관리 화면 첫 쪽 「살펴볼 일」, `maru signals`, `GET /api/v1/signals` |
| 메일 | 정리 루프(10분)마다 `signals::alert`. 처음 보는 `key` 만 묶어 관리자·소유자에게. `signal_mail`(기본 켬)로 끈다 |
| 되풀이 막기 | `signal_sent(key)` 30일. 댓글 급증은 글·날짜, 신고 집중은 대상, 묵은 신고는 날짜가 `key` |

## 순서

마이그레이션(`signal_sent`, `comments_created` 색인) → 모델·Store → `services/signals.rs` → 신고 모듈 → 웹(첫 쪽·REST·루프·설정) → CLI → 시험(storage/tests/signals.rs, e2e_signals.rs) → 브라우저
