# Phase 1 완료 검증 (U15)

> 측정일 2026-11-02. 기준 커밋은 `2d8f5d7`에 푸터 폭 수정을 더한 상태다.
> 형식은 PHASE1_PLAN §8 「완료 보고 형식」을 따른다. 결과는 PASS, FAIL, 못 쟀다 셋 중 하나다.
> 「못 쟀다」는 통과로 치지 않는다.

## 1. SPEC §15 Phase 1 완료 기준

| 기준 | 결과 | 근거 (실행 명령 · 대조 건수) |
|---|---|---|
| `maru init --preset blog --yes` 후 `serve`로 블로그 운영 가능 | PASS | 깨끗한 디렉터리(`F:\…\verify\새 사이트`, 한글 경로)에서 릴리스 바이너리로 init → serve → `GET /` 200. 홈·목록·글·페이지·RSS·사이트맵 응답 확인 |
| CLI로 글 작성·발행 가능 | PASS | `maru post new --from post.md --publish` 30회 모두 성공, 목록·RSS에 노출. `e2e_publish::cli_post_lifecycle_and_input_paths` |
| 모든 쓰기가 audit_log에 남음 | PASS | 서비스 쓰기 지점 21곳 모두 `Tx` 경유. `Tx`는 감사 0건이면 commit을 거부한다(`commit_without_audit_is_rejected`). 역시험 「감사 기록 제거」가 게이트에 걸림. action 32종: post 7, page 6, board 4, menu 2, token 2, auth 4, member 1, settings·design·preset 등 |
| 통합 테스트 통과 | PASS | `cargo test --workspace`: 아래 §5 |

## 2. Phase 1 범위 대조 (SPEC §15 목록)

| 항목 | 결과 | 비고 |
|---|---|---|
| 워크스페이스·게이트(fmt, clippy, test) | PASS | `scripts/check.ps1`, pre-commit 연결 |
| CI | 못 쟀다 | 원격 저장소 연결 전 |
| 마이그레이션 11개 테이블 | PASS | site_settings, boards, posts, revisions, pages, menus, members, roles, tokens, media, audit_log + sessions, login_tickets. 모두 down 파일 있음 |
| 게시판·글·페이지·메뉴 CRUD, Markdown, 리비전, 감사 | PASS | storage 테스트 19 + 9, core 44 |
| 공개 사이트(홈, 목록, 글, 페이지), 사이트맵·RSS·메타 | PASS | `e2e_publish::seo_outputs_follow_public_state` |
| 최소 관리 화면(로그인, 글 목록·작성) | PASS | 자동 `e2e_admin` 3개 + 실제 브라우저 E2E-2 (아래 §4) |
| 기본 테마 1개 + blog 프리셋, 토큰 → CSS 변수 | PASS | `/assets/tokens.css`, 라이트·다크 대비 AA 검사 |
| CLI `init serve doctor board post page menu`, `--json` 계약 | PASS | `e2e_ops::every_command_speaks_json_with_yes` 13개 명령 |
| 미디어 업로드 | 범위 밖 | 스키마만 있다. 업로드·에디터는 다음 작업 |

## 3. SPEC §14 품질 목표 (이 PC 기준, N100 아님)

측정 장비는 AMD Ryzen 7 5800X, Windows 10이고 릴리스 빌드(`lto=thin`, `strip`)다. N100 수치는 장비가 없어 **못 쟀다**.

| 항목 | 목표 | 측정 | 결과 |
|---|---|---|---|
| 바이너리 크기 | 30MB 이하 | 12.95MB | PASS |
| 유휴 메모리 | 50MB 이하 | 작업 집합 10.4MB, 전용 2.8MB (글 30개, 부하 뒤 13.3MB) | PASS (이 PC) |
| 공개 페이지 p95 (캐시 없음) | 50ms 이하 | 연결 재사용 120회: `/` 1.04ms, `/b/blog` 1.40ms, 글 0.95ms, `/rss.xml` 2.54ms | PASS (이 PC) / N100 못 쟀다 |
| `init`부터 첫 페이지까지 | 10초 이하 | 2.4초 (init 단독 1.3초) | PASS |
| Lighthouse 95 이상 | 목표만 | 측정 안 함 | 못 쟀다 (Phase 1 완료 기준 아님) |

측정 중 알게 된 것은 두 가지다.
- 요청마다 curl을 새로 띄우면 p95가 8~94ms로 크게 흔들린다. 서버가 아니라 프로세스 생성과 연결 비용이다.
- 같은 주소에서 분당 600회를 넘기면 429가 나온다. 이 때문에 첫 측정의 일부는 제한 응답이었다. 위 수치는 한도 안에서 상태 200만 센 것이다.

## 4. 보안 기준(§7)과 E2E

| 항목 | 결과 | 근거 |
|---|---|---|
| E2E-1 | PASS | `e2e1_init_rest_draft_cli_publish_public_page` |
| E2E-2 실제 브라우저 | PASS | 링크 확인 → 로그인 → 비밀번호 설정 → 글 작성·발행 → 공개 페이지 → 로그아웃 → `/admin` 차단 → 틀린 비밀번호 거부 → 재로그인. 375px 다크에서 관리 화면 확인 |
| 세션 쿠키 속성 | PASS | `HttpOnly; SameSite=Lax; Path=/; Max-Age=1209600`, http에서는 Secure 없음 |
| CSRF | PASS | 토큰 없는 POST, 다른 세션 토큰 모두 403 |
| 비밀번호 대입 | PASS | 같은 실패 메시지, 로그인 10회/분 뒤 429, `X-Forwarded-For`를 바꿔도 우회 안 됨 |
| 로그인 링크 | PASS | 재사용·이전 링크 401, 동시 8회 사용 중 1회만 성공, `Referrer-Policy: no-referrer` |
| Open Redirect | PASS | `//evil.test/x` → `/admin` |
| 관리 화면 헤더 | PASS | `no-store`, `frame-ancestors 'none'`(iframe 로드 차단 확인) |
| 로그·감사에 비밀 값 없음 | PASS | 서버 로그(debug 수준)·audit_log 전체에서 비밀번호·세션·링크·CSRF 원문 검색 0건 |
| RSS·사이트맵 주입 | PASS | `]]> <script>` 본문, CDATA 미사용 |

## 5. 테스트 집계

`cargo test --workspace` 결과: 120개 통과, 실패 0 (core 44, storage services 19, storage auth 9, themes 11, web 6, cli e2e_publish 7, e2e_admin 3, e2e_ops 6, cli_basic 4, 기타 단위 11).

검증 중 `login_attempts_are_rate_limited_per_connection_address` 가 한 번 실패했다. 분 단위 고정 창이라 테스트 도중 분이 바뀌면 카운트가 초기화되는 흔들림이었다. 분 경계를 넘어도 맞는 단언(21회 안에 429, 그 전은 모두 401)으로 고쳤다.

## 6. 역시험 (§8)

결함을 하나씩 심고 지정 게이트가 실패하는지 본 뒤 되돌렸다. 스크립트는 `F:\claude-temp\rust-cms\reverse.sh`다.

| 심은 결함 | 떨어진 게이트 | 결과 |
|---|---|---|
| 정화 호출 제거 | `markdown::tests::xss_vectors_are_removed` | PASS |
| 감사 기록 제거 | storage `services` (commit 거부) | PASS |
| 공개 조회에 초안 포함 | `e2e1_init_rest_draft_cli_publish_public_page` | PASS |
| 권한 함수를 항상 허용 | `agent_cannot_issue_login_link` 외 | PASS |
| 로그아웃 세션 삭제 누락 | `password_login_and_change_drop_other_sessions` | PASS |
| XML 이스케이프 해제 | `seo_outputs_follow_public_state` | PASS |
| `core`에 `sqlx` 추가 | `check.ps1` 의존 방향 (`maru-core -> sqlx`) | PASS |

## 7. UI-1

| 항목 | 결과 | 근거 |
|---|---|---|
| 375px·1120px × 홈·목록·글·페이지 | PASS | 가로 넘침 0, h1 있음, alt 없는 이미지 0, em-dash·이모지 0 |
| 라이트·다크 | PASS | 앞선 U11 확인 + 이번 다크 375px |
| 머리·본문·푸터 왼쪽 선 | PASS (수정 후) | 푸터가 본문보다 좁던 버그를 고쳤다. 원인은 `--measure`가 em이라 푸터 글자 크기만큼 폭이 줄어든 것. 회귀 테스트 `wrap_containers_keep_body_font_size` 추가 |
| Pretendard 실제 로드 | 못 쟀다 | woff2 파일을 아직 넣지 않았다 (다운로드 승인 필요). 시스템 글꼴로 대체됨 |

## 8. 못 쟀다 목록

- N100 실측 성능 (장비 없음)
- GitHub CI (원격 저장소 연결 전)
- macOS 빌드
- Windows 정상 종료 신호: Ctrl+C·창 닫기 처리 코드는 있으나, 테스트에서 자식 프로세스에 콘솔 신호를 보낼 수 없다. Unix SIGTERM은 테스트가 있다(이 PC에서는 실행 안 됨).
- Lighthouse
- Pretendard 로드
- 디스크 여유 공간 (doctor도 「못 쟀다」로 표시)

## 9. 다음 Phase 전에 알아 둘 것

- 공개 터널 뒤에서는 모든 요청이 `127.0.0.1`로 보인다. 그러면 분당 600회 한도를 방문자 전체가 나눠 쓴다. `maru expose`(Phase 3)에서 믿을 전달 헤더를 ADR로 정해야 한다 (PHASE1_PLAN §7에 이미 적힌 항목).
- 관리 화면 에디터는 Markdown 입력 칸뿐이다. 미리보기, 서식, 이미지 업로드가 없다.
- SPEC §15는 다음 Phase 전에 실제 사이트 운영을 요구한다. 운영 기간은 사용자가 정한다.
