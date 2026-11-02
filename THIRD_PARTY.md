# 제3자 자산

Maru 바이너리에 포함되는 외부 자산(폰트, 아이콘, 이미지, CSS)과 라이선스를 적는다.
원칙 7에 따라 재배포 가능한 라이선스(MIT, Apache-2.0, OFL, CC0 등)만 쓴다.
Rust 크레이트 라이선스는 `cargo deny check licenses` 로 따로 검사한다.

| 자산 | 위치 | 출처 | 라이선스 | 비고 |
|---|---|---|---|---|
| (없음) | | | | 내장 테마 CSS·템플릿은 자체 제작 |

## 예정

- Pretendard (본문 폰트, SPEC §8.4): SIL Open Font License 1.1. 받기 전에 파일명, 출처, 크기를 확인받는다.
  받기 전까지 토큰 `font.body = "Pretendard"` 는 사용자 PC에 설치된 Pretendard, 없으면 시스템 한글 글꼴로 떨어진다.
