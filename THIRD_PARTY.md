# 제3자 자산

Maru 바이너리에 포함되는 외부 자산(폰트, 아이콘, 이미지, CSS)과 라이선스를 적는다.
원칙 7에 따라 재배포 가능한 라이선스(MIT, Apache-2.0, OFL, CC0 등)만 쓴다.
Rust 크레이트 라이선스는 `cargo deny check licenses` 로 따로 검사한다.

| 자산 | 위치 | 출처 | 라이선스 | 비고 |
|---|---|---|---|---|
| ProseMirror (prosemirror-model·state·view·transform·commands·history·inputrules·keymap·schema-list·markdown) | `crates/themes/admin/static/editor.js` (번들) | https://prosemirror.net | MIT | 글 에디터 편집 엔진. ADR-0033 |
| markdown-it, linkify-it, mdurl, uc.micro, punycode.js | 같은 번들 | https://github.com/markdown-it | MIT | 에디터 안의 Markdown 해석 |
| entities | 같은 번들 | https://github.com/fb55/entities | BSD-2-Clause | markdown-it 의존 |
| orderedmap, rope-sequence, w3c-keyname | 같은 번들 | https://github.com/marijnh | MIT | ProseMirror 의존 |

내장 테마 CSS·템플릿과 에디터의 Maru 층(도구 막대·업로드·자동 저장·CSS)은 자체 제작이다.
번들에 들어간 패키지의 버전과 라이선스 전문은 빌드할 때 `crates/themes/admin/static/editor.licenses.txt` 로
자동으로 모인다(esbuild 메타파일 기준). 관리 화면은 이 파일을 `/admin/assets/editor.licenses.txt` 로 내보내고, 번들 첫 줄이 그 위치를 가리킨다.
TOAST UI Editor(MIT)는 구조만 분석했고 코드는 가져오지 않았다.

## 예정

- Pretendard (본문 폰트, SPEC §8.4): SIL Open Font License 1.1. 받기 전에 파일명, 출처, 크기를 확인받는다.
  받기 전까지 토큰 `font.body = "Pretendard"` 는 사용자 PC에 설치된 Pretendard, 없으면 시스템 한글 글꼴로 떨어진다.
