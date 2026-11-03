# Maru 글 에디터

관리 화면 글쓰기 에디터. ProseMirror 위에 Maru 층(도구 막대·단축키·이미지 업로드·자동 저장)을 얹었다. 설계와 이유는 `docs/DECISIONS.md` ADR-0033.

Node 는 **빌드할 때만** 필요하다. 바이너리는 커밋된 번들을 rust-embed 로 품으므로 `cargo build`·실행에는 필요 없다.

```bash
cd editor
npm ci          # 정확한 버전으로 설치 (package-lock.json)
npm test        # Markdown 왕복 검사 (Node, 브라우저 없음)
npm run build   # ../crates/themes/admin/static/editor.js + editor.licenses.txt
```

`src/` 를 고쳤으면 빌드 결과 두 파일을 **함께 커밋**한다. 스타일은 `crates/themes/admin/static/editor.css`(빌드 없음).

| 파일 | 하는 일 |
|---|---|
| `src/markdown.ts` | 스키마, Markdown ↔ 문서 변환, 손실 검사(`load`) |
| `src/commands.ts` | 서식 명령, 단축키, 입력 규칙 |
| `src/upload.ts` | 이미지 업로드(`POST /admin/media`)와 업로드 중 자리 표시 |
| `src/main.ts` | 도구 막대, 모드 전환, 미리보기, 자동 저장, 페이지 연결 |
| `scripts/licenses.mjs` | 번들에 들어간 패키지의 라이선스 전문 모으기 |
| `test/roundtrip.ts` | 열 수 있는 글은 두 번 써도 같고, 못 여는 글은 Markdown 으로 보내는지 |

자동 검증이 닿지 않는 것: 한글 IME 조합, 실제 끌어 놓기·파일 선택, 붙여 넣기. 바꾸면 브라우저에서 직접 확인한 뒤 커밋한다.
