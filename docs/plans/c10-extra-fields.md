# C10 여분 필드 — 계획 (단위 11d)

> GAP C10. 게시판마다 정한 칸(가격·지역·행사일·연락처 등)을 글에 붙인다. ADR-0018 필드 타입의 첫 구현이다.
> 컬렉션 일반화(ADR-0019: 뷰·퍼머링크·`index: true` 정렬·필터)는 Phase 2(b) 그대로 두고, 여기서는 **정의·검증·입력·표시**만 한다.

## 1. 결정 (추천안 채택)

| # | 결정 | 추천안(채택) | 대안 |
|---|---|---|---|
| D1 | 저장 | `boards.field_schema`(JSON 배열, 기본 `[]`), `posts.meta`(JSON 객체, 기본 `{}`) | 필드별 행 테이블(EAV) — 읽기마다 조인, SPEC 이 버림 |
| D2 | 필드 정의 | `{key, label, type, required, options, help}`. key 는 `^[a-z][a-z0-9_]{0,31}$`, 게시판당 30개, 겹침 금지 | 임의 JSON Schema — 렌더 계약 불가(ADR-0018) |
| D3 | 이번 타입 | `text`·`textarea`·`number`·`boolean`·`date`·`datetime`·`select`·`multiselect`·`url`·`email`·`phone`. `markdown`·`media`·`ref`·`group`·`list` 는 `error.field_type_unsupported`(다음 단계) | 전부 한 번에 — `ref`·`media` 는 참조 무결성·미디어 정리 설계가 따로 필요 |
| D4 | 검증 | core `fields::validate(schema, meta)` 하나를 관리·공개·REST·CLI 가 같이 쓴다. 폼은 문자열로 오므로 타입별로 바꾼다(숫자·참거짓·날짜 형식). 정의에 없는 키는 거부, 필수 칸 빈 값 거부, 빈 선택 칸은 지운다 | 화면마다 검증 — 경로마다 결과가 달라진다 |
| D5 | 정의를 바꿀 때 | 기존 글의 `meta` 는 건드리지 않는다. 표시는 지금 정의에 있는 키만, 다음 저장 때 지금 정의로 다시 검증 | 정의 변경 시 일괄 변환 — 되돌리기 어렵다 |
| D6 | 표시 | 글 화면 본문 위에 `dl` 표(라벨: 값), select 는 선택지 라벨, url 은 링크(rel=nofollow ugc), email·phone 은 `mailto:`·`tel:`. 비밀글은 값도 숨긴다 | 테마마다 따로 — 기본 테마가 먼저 보여야 한다 |
| D7 | 입력 | 공개 글쓰기·관리 편집기 모두 정의대로 칸을 그린다(`field.<key>` 이름). CLI `--field key=value`(여러 번), REST `meta` 객체 | |

## 2. 다섯 경로

- **데이터**: 마이그레이션 `boards_fields`(두 열, down 은 열 지우기).
- **core**: `fields` 모듈(`FieldDef`·`FieldType`·`check_schema`·`validate`·`display`), `Board.field_schema`, `Post.meta`, `NewBoard`·`BoardPatch`·`NewPost`·`PostPatch`·`BoardWrite`·`BoardEdit` 의 칸, 리비전에도 `meta`.
- **REST**: 기존 게시판·글 경로의 JSON 칸.
- **CLI**: `board update --fields <파일|->`·`board fields <slug>`(보기), `post new|edit --field key=value`.
- **화면**: 공개 글 화면 표·글쓰기 칸, 관리 편집기 칸.
- **MCP**: Phase 3.

## 3. 작업 단위

| 단위 | 내용 | 테스트 |
|---|---|---|
| T1 | 마이그레이션·모델·storage | 올리고 내리기 |
| T2 | `fields` 모듈 | 단위: key 규칙·타입별 변환·필수·모르는 키·선택지 |
| T3 | 서비스 연결 (게시판 정의, 글 만들기·고치기·공개 쓰기) | 저장소: 정의 저장 → 글 meta 저장·거부·정의 바뀐 뒤 표시 |
| T4 | CLI·REST·화면 | e2e: CLI 정의 → CLI 글 `--field` → DB meta → 공개 화면 표, 공개 폼 칸 |
| T5 | 브라우저, ADR-0057, GAP | 게이트 |

완료 기준: 게시판에 「가격(number, 필수)·지역(select)」을 정의하면 관리·공개·CLI 어디서 써도 같은 규칙으로 검증되어 DB `posts.meta` 에 남고, 글 화면에 라벨과 값이 보인다.

## 4. 진행

- [x] T1~T5 — ADR-0057.
- 계획과 달라진 점: 리비전에는 `meta` 를 넣지 않았다(리비전 비교는 제목·본문 줄 단위라 칸 값은 감사 기록 diff 로 본다). 답변글도 `strict` 가 아니다(질문의 항목을 다시 채울 일이 없다). `board fields` 명령 대신 `board show` 에 칸 목록을 붙였다.
