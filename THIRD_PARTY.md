# 제3자 자산

Giwa 바이너리에 포함되는 외부 자산(폰트, 아이콘, 이미지, CSS)과 라이선스를 적는다.
원칙 7에 따라 재배포 가능한 라이선스(MIT, Apache-2.0, OFL, CC0 등)만 쓴다.
Rust 크레이트 라이선스는 `cargo deny check licenses` 로 따로 검사한다.

| 자산 | 위치 | 출처 | 라이선스 | 비고 |
|---|---|---|---|---|
| ProseMirror (prosemirror-model·state·view·transform·commands·history·inputrules·keymap·schema-list·markdown) | `crates/themes/admin/static/editor.js` (번들) | https://prosemirror.net | MIT | 글 에디터 편집 엔진. ADR-0033 |
| markdown-it, linkify-it, mdurl, uc.micro, punycode.js | 같은 번들 | https://github.com/markdown-it | MIT | 에디터 안의 Markdown 해석 |
| entities | 같은 번들 | https://github.com/fb55/entities | BSD-2-Clause | markdown-it 의존 |
| orderedmap, rope-sequence, w3c-keyname | 같은 번들 | https://github.com/marijnh | MIT | ProseMirror 의존 |
| 나눔손글씨 펜 → **Giwa Pen** | `crates/themes/builtin/static/fonts/maru-pen-1.woff2` | NHN(네이버) 나눔글꼴, https://github.com/google/fonts `ofl/nanumpenscript/NanumPenScript-Regular.ttf` (2026-11-03 받음) | SIL OFL 1.1 | 서체 「손글씨」. ADR-0047 |
| 나눔손글씨 붓 → **Giwa Brush** | `…/fonts/maru-brush-1.woff2` | 같은 저장소 `ofl/nanumbrushscript/NanumBrushScript-Regular.ttf` | SIL OFL 1.1 | 서체 「붓글씨」 |
| 나눔명조 → **Giwa Serif** | `…/fonts/maru-serif-1.woff2` | 같은 저장소 `ofl/nanummyeongjo/NanumMyeongjo-Regular.ttf` | SIL OFL 1.1 | 서체 「명조」 |
| Cat Emoticons → **고양이 스티커** | `crates/themes/builtin/static/stickers/cat/*.png` | Magory, https://magory.itch.io/cat-emoticons (`catemojis.zip`, 2026-11-06 받음, sha256 `36f9adc3…dd285b`) | CC0 (묶음 안내 파일에 표기, 표시 의무 없음) | 노랑 128px 64칸 중 빈 얼굴을 뺀 63개. 이름표는 자체 작성. ADR-0115 |

세 폰트는 `scripts/subset-fonts.py` 로 한글 11172자·호환 자모·영문·문장부호만 남기고 힌팅을 빼 woff2 로 줄였다.
OFL 의 예약 글꼴 이름(Nanum, NanumPen, NanumBrush, NanumMyeongjo 등) 때문에 변경본은 폰트 안 이름을
`Giwa Pen`·`Giwa Brush`·`Giwa Serif` 로 바꿨다(저작권·라이선스·상표 표기는 그대로 둠). 라이선스 전문은
`crates/themes/builtin/static/fonts/OFL.txt` 이고 공개 사이트에서 `/assets/fonts/OFL.txt` 로 나간다.

운영자가 관리 화면이나 `giwa font add` 로 올린 서체는 바이너리에 들어가지 않는 사이트 데이터다. 서체마다 라이선스와
「웹에 내보낼 권리」 확인을 DB 에 남기고, 붙인 라이선스 전문은 `/fonts/{slug}/LICENSE.txt` 로 함께 낸다 (ADR-0048).

내장 테마 CSS·템플릿과 에디터의 Giwa 층(도구 막대·업로드·자동 저장·CSS)은 자체 제작이다.
번들에 들어간 패키지의 버전과 라이선스 전문은 빌드할 때 `crates/themes/admin/static/editor.licenses.txt` 로
자동으로 모인다(esbuild 메타파일 기준). 관리 화면은 이 파일을 `/admin/assets/editor.licenses.txt` 로 내보내고, 번들 첫 줄이 그 위치를 가리킨다.
TOAST UI Editor(MIT)는 구조만 분석했고 코드는 가져오지 않았다.

## 예정

- Pretendard (본문 폰트, SPEC §8.4): SIL Open Font License 1.1. 받기 전에 파일명, 출처, 크기를 확인받는다.
  받기 전까지 토큰 `font.body = "Pretendard"` 는 사용자 PC에 설치된 Pretendard, 없으면 시스템 한글 글꼴로 떨어진다.
