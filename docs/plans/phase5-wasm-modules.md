# Phase 5 계획: 외부 모듈 (WASM)

## 배경

- SPEC §15 Phase 5 는 「WASM 모듈 런타임, 모듈 패키지 포맷, `module install <url>`(서명 검증), 공식 모듈 저장소」 네 줄뿐이고 완료 기준이 없다. SPEC §7.5 는 「외부 모듈은 WASM 컴포넌트(wasmtime)로 샌드박스 실행. 호스트 API 는 서비스 레이어 일부만 노출」로 정해 두었다.
- 지금 모듈 17개(`mods/*`)는 모두 바이너리에 들어 있다(ADR-0066). 남이 만든 기능을 붙이려면 기와를 다시 빌드해야 한다. Phase 5 는 바이너리를 바꾸지 않고, 남이 만든 모듈을 **믿지 않은 채** 붙이는 길이다.
- 가장 큰 제약은 크기다. SPEC §14 는 바이너리 30MB 이하이고, ADR-0135 때 남은 여유가 1.1MB 였다. 그 뒤 결제·예약·쇼핑·미디어 보관함이 더해졌다. **지금 윈도우 릴리스는 35.06MB(35,056,640바이트, 2026-11-24, HEAD 2fe6637)로 Phase 5 전에 이미 한도를 5MB 넘었다.** ADR-0174 가 「실제 릴리스는 35MB」라고 적었지만 한도를 다시 정한 ADR 은 없다. 그래서 크기 한도는 Phase 5 와 상관없이 먼저 정해야 한다.

## 공식 문서에서 확인한 것 (2026-11-24)

wasmtime (docs.wasmtime.dev, 저장소 `docs/*.md`, crates.io)
- 최신 안정 판 **51.0.0**(2026-11-20). 거의 달마다 큰 판이 나온다. `wasmtime-wasi` 도 51.0.0.
- 크레이트 기본 기능에 Cranelift·캐시·병렬 컴파일·GC·스레드·wat 등이 다 들어 있다. `default-features = false` 로 끄고 `runtime`·`component-model`·`async` 와 컴파일러 하나만 고를 수 있다.
- 크기(공식 최소 빌드 문서, 2024-12 Linux 기준): 기본 C API 릴리스 19MB, 기본 기능을 끄면(**컴파일러 없음**) 2.1MB, LTO 등을 더하면 1.2MB. 컴파일러가 없으면 미리 컴파일한 `*.cwasm` 만 읽는다. Cranelift 자체 크기는 줄이는 작업을 하지 않았고, 「Winch 가 훨씬 작다」고 적혀 있다.
- 등급: `x86_64-pc-windows-msvc`·`x86_64-unknown-linux-gnu`·`x86_64-apple-darwin` 과 **Cranelift·Winch 둘 다 Tier 1**. `component-model` 도 Tier 1. Winch 는 x86_64·aarch64 에서 component-model 을 지원한다(aarch64 는 Core Wasm 완성, threads 등 일부 없음).
- Pulley(인터프리터): Cranelift 보다 10배쯤 느리다. Pulley 바이트코드도 Cranelift 가 만든다. **명령 번호가 wasmtime 판마다 달라서** 바이트코드를 판끼리 옮길 수 없다. Winch 는 Pulley 바이트코드를 만들지 못한다.
- 그래서 「패키지에는 이식 가능한 `.wasm` 컴포넌트를 담고, 설치하는 PC 에서 컴파일한다」면 바이너리에 컴파일러가 하나 있어야 한다.

wasmi (crates.io 2.0.2): 인터프리터라 작지만, 컴포넌트 모델은 자체 지원이 없다(이슈 #897, 외부 크레이트 `wasm_component_layer` 로 우회). SPEC 이 정한 컴포넌트 모델과 맞지 않아 대안으로만 둔다.

Rust `wasm32-wasip2` 대상: Tier 2, 결과물이 바로 컴포넌트다. 모듈 작성자는 `cargo build --target wasm32-wasip2` 로 만든다.

서명: `minisign-verify` 0.3.0(검증 전용, Ed25519, 의존성 없음). 작성자는 널리 쓰는 `minisign` 도구로 서명하고 기와는 검증만 한다.

## 결정 (추천안, 0단계 크기 실험으로 확정)

| 항목 | 결정 |
|---|---|
| 런타임 | `wasmtime` 51, `default-features = false` + `anyhow`·`runtime`·`component-model`·`async`·`std`·**`cranelift`**, `wasmtime-wasi` 51 `p2` 만. 캐시·병렬 컴파일·GC·스레드·wat·coredump·profiling 은 끈다. 처음에는 Winch 를 추천했지만 0단계에서 재 보니 Cranelift 보다 0.27MB 만 작고 만든 코드는 더 느렸다(아래 「0단계 결과」, ADR-0176) |
| 컴파일 | 설치할 때 한 번 컴파일해 `<데이터>/modules/<id>/<판>/module.cwasm` 에 둔다. 기와를 업데이트해 wasmtime 판이 바뀌면 시작할 때 다시 컴파일한다(엔진 호환 해시로 판단). `.cwasm` 은 우리가 만든 파일만 읽는다(남이 준 `.cwasm` 은 받지 않는다. 검증 없이 기계어를 읽는 것이라) |
| 크기 한도 | 30MB 를 버리고 **46MB**(지금 35.06MB + wasmtime 8.73MB = 43.8MB 예상 + 여유 2MB, 반올림)로 정한다(사용자 승인 2026-11-24, ADR-0176). 검사는 `scripts/size.ps1`(윈도우 릴리스, 1MB = 10⁶바이트)과 CI 작업 `release-size`. 릴리스 빌드가 12분이라 커밋마다 도는 게이트에는 넣지 않는다. 1단계에서 실제로 붙인 크기를 다시 재고, 예상보다 작으면 한도를 「잰 값 + 2MB」로 내린다 |
| 샌드박스 | WASI 는 **빈 상자**로만 준다: 폴더·소켓·환경 변수·명령줄 인자 없음(소켓·이름 조회는 `WasiCtxBuilder` 기본값이 꺼짐), 표준 출력·오류는 모듈 로그(크기 제한)로. 0단계에서 WASI 없이 링크하니 `std` 로 만든 손님이 `wasi:io/poll` 을 가져와 인스턴스를 만들지 못했다. 그래서 WASI(+1.4MB)는 뺄 수 없다 |
| 호스트 API (WIT `giwa:module@0.1.0`) | 모듈이 내놓는 것: `on-event`·`call-tool`·`handle`(`/m/<id>/…` 요청)·`widget-view`·`slot`·`check-settings`·`purge`. 지금 `Module` 트레이트와 같은 모양이고, 값은 JSON 글자로 주고받는다(툴 입력 JSON Schema 를 그대로 쓴다). 모듈이 부르는 것: `log`, `clock`(지금·한국 날짜), `store`(자기 키-값), `content`(글·게시판·페이지·공개 회원 정보 읽기, 권한이 있으면 초안·댓글 쓰기), `http`(목록에 적은 도메인만), `settings`(자기 설정 읽기) |
| 저장 | v1 에는 **SQL 을 주지 않는다**. 모듈마다 키-값 저장소(코어 표 `ext_module_kv(module, key, value)`, 키 200B·값 64KB·모듈당 기본 50MB). 모르는 코드가 코어 DB 에 SQL 을 보내게 할 수 없어서다. 지우기는 표에서 그 모듈 행만 |
| 권한 | `module.toml` 의 `[permissions]` 에 코어 `Permission` 이름(`post_read`, `post_write`, `post_comment` …)과 `net = ["api.example.com"]` 을 적는다. 설치 화면이 이것을 한국어로 보이고 사람이 승인한다. 이벤트·툴은 「모듈 신분」(받은 권한만)으로, 화면 요청은 「보는 사람 권한 ∩ 모듈 권한」으로 서비스를 부른다. 모든 쓰기는 감사 기록에 `actor_type = module` 로 남는다. 발행(`post_publish`)·회원 관리·사이트 설정 권한은 v1 에서 외부 모듈에 주지 않는다 |
| 화면 | 모듈이 HTML 을 직접 내지 못한다. `handle` 은 `page{template,title,data}`(호스트가 테마로 그린다)·JSON·글자·다른 주소로 보내기만 돌려준다. 패키지의 템플릿은 `mod/<id>/` 아래만, 설치할 때 `|safe`·`autoescape false` 가 있으면 거절한다. 정적 파일은 그림·CSS·글꼴만(`/m/<id>/static/…`). **v1 에 외부 모듈 스크립트 없음**(CSP nonce 를 남의 코드에 주지 않는다) |
| 한도 | 호출마다 새 인스턴스. 시간: 이벤트 2초·화면 1초·툴 5초(epoch 끊기, 100ms 눈금). 메모리 64MB(StoreLimits). 동시 실행 2개(N100). 응답 1MB·로그 호출당 16KB. 연달아 5번 실패(트랩·시간 초과)하면 그 모듈을 끄고 운영 신호를 띄운다. 모듈이 죽어도 화면은 나간다(지금 위젯·자리와 같다) |
| 패키지 | `<id>-<판>.giwa` = tar.gz: `module.toml`, `module.wasm`, `templates/`, `static/`, `LICENSE`. 10MB 이하. 옆에 `<파일>.minisig`. `module.toml` 에 `api = "giwa:module@0.1"`, `min_giwa`, 권한, 설정 칸(`FieldDef` 와 같은 모양), 위젯 종류, 툴 정의 |
| 서명·신뢰 | 공식 공개 키를 바이너리에 넣는다. 사용자가 다른 키를 더할 수 있다(`giwa module trust add <키> --name`, 사이트 관리 권한, 감사 기록). 서명이 없거나 틀리면 설치하지 않는다. 내 PC 의 파일 경로만 `--allow-unsigned` 로 받는다(개발용, URL 은 안 됨) |
| 설치 | `giwa module install <URL|경로>`: https 만, 공개 주소만(지금 `fetch` 의 PublicOnly), 10MB 한도 → 서명 → 매니페스트·템플릿 검사 → 컴파일 → 권한 보이기 → `--yes`. 설치와 켜기는 따로다(지금 켜기·끄기·제거를 그대로 쓴다). 업데이트에서 권한이 늘면 다시 승인을 받는다. 내장 모듈 id 와 겹치면 거절 |
| 에이전트 | 설치는 changeset 연산 `module.install`(미리보기에 권한 목록, 사람이 적용). 에이전트 토큰은 직접 설치하지 못한다(절대 원칙 4). MCP `install_module` 은 changeset 을 만든다 |
| 다섯 경로 | core 서비스 → storage(`ext_module_kv`, 설치 기록 표) → REST `/api/v1/modules/install` 등 → CLI `giwa module install·trust·search` → MCP → 관리 화면 「모듈 더하기」(권한 확인 화면) |
| 경로 붙이기 | 지금은 시작할 때 `/m/<id>` 를 다 붙인다. 외부 모듈은 서버를 켠 뒤에도 설치되므로 `/m/{id}/{*rest}` 하나가 받아 외부 모듈 목록에서 찾는다. 레지스트리는 내장(고정) + 외부(바뀜, `RwLock`) 두 칸 |
| 공식 저장소 | 서버 없이 정적 파일: 서명한 `index.json`(id·판·주소·sha256·권한 요약)과 패키지. `giwa module search`·`info`. 자리는 GitHub 저장소(예: `Evanciel/giwa-modules`)의 릴리스. 공개 저장소를 만드는 것은 사용자 몫 |
| 작성 도구 | `crates/module-sdk`(WIT + `wit-bindgen` 도우미), `giwa module new <id>`(뼈대), `giwa module pack`(묶기·검사). 서명은 `minisign` 사용법을 문서에 적는다 |
| 예시 모듈 | `examples/modules/guestbook`(방명록): 키-값, 화면, 툴, 위젯, 이벤트(새 글 수 세기), 설정을 다 쓴다. CI 가 `wasm32-wasip2` 로 빌드해 e2e 가 설치한다 |

## 0단계 결과 (2026-11-24, 개발 PC 윈도우, wasmtime 51.0.0)

실험 크레이트 `G:/claude-temp/giwa-p5/spike`(커밋하지 않음): 손님은 `wit-bindgen` 0.63 + `wasm32-wasip2`(69KB 컴포넌트), 호스트는 기와와 같은 릴리스 설정(opt-level z, LTO fat, codegen-units 1, strip)에 tokio.

| 변형 | 릴리스 크기 | 아무것도 없는 판보다 |
|---|---|---|
| tokio 만 | 0.27MB | - |
| 런타임만(컴파일러 없음) | 1.09MB | +0.82MB |
| 런타임 + WASI | 2.53MB | +2.26MB |
| Winch | 7.32MB | +7.05MB |
| Winch + WASI | 8.74MB | +8.47MB |
| **Cranelift + WASI** | 9.01MB | **+8.73MB** |

| 시험 | Winch + WASI | Cranelift + WASI |
|---|---|---|
| 컴파일(69KB 컴포넌트) | 80~90ms | 295ms |
| `.cwasm` 크기 | 341KB | 264KB |
| `.cwasm` 다시 읽기 | 1ms | 1ms |
| 첫 호출(인스턴스 만들기 포함) | 0.8ms | 0.8ms |
| 호출 1회 평균(인스턴스 새로, 1000번) echo / 비동기 호스트 함수 / JSON 3KB | 180 / 180 / 248µs | 172 / 235 / 255µs |
| 계산(2천만 번 반복) | 60ms | 38ms |
| 무한 루프(눈금 10ms × 10) | 98ms 에 끊김 | 106ms 에 끊김 |
| 메모리 64MB 한도 넘기기 / 패닉 | 트랩, 다음 호출 정상 | 트랩, 다음 호출 정상 |

- WASI 없이 링크하면 손님이 `wasi:io/poll@0.2.12` 를 못 찾아 인스턴스를 만들지 못한다.
- Cranelift 로 만든 `.cwasm` 은 컴파일러 없는 런타임에서도 읽힌다. Winch 로 만든 것은 「Winch calling convention」이 켜진 엔진만 읽는다. 나중에 컴파일러를 빼는 길(공식 빌드가 `.cwasm` 을 만들어 주기)이 Cranelift 쪽에만 열려 있다.
- N100 은 개발 PC 보다 2~3배 느리다고 보고 6단계에서 미니PC 에서 다시 잰다.

## 순서

| 단계 | 내용 | 끝 표시 |
|---|---|---|
| 0 ✅ | 크기·속도 실험(위 「0단계 결과」) | ADR-0176, `scripts/size.ps1`, CI `release-size` |
| 1 | WIT 세계, `crates/wasm`(`giwa-wasm`): 엔진·한도·키-값 표 마이그레이션, 외부 모듈이 `Module` 트레이트를 구현, 레지스트리 두 칸, `/m/{id}/{*rest}` | 손으로 만든 시험 컴포넌트가 이벤트·툴·화면을 받는다 |
| 2 | 패키지·서명·신뢰 키·설치·업데이트·제거(CLI·REST·MCP changeset·관리 화면) | 서명 틀림·권한 늘림·id 겹침 거절 e2e |
| 3 | 호스트 API: content·http·settings·템플릿·정적 파일, 권한 교집합, 감사 기록 | 권한 밖 호출 거절, 쓰기가 감사에 남음 |
| 4 | 작성 도구·방명록 예시·문서(AGENTS.md, llms.txt, 스킬) | 예시를 URL 로 설치해 켜기·끄기·제거 |
| 5 | 공식 저장소 형식·`search`·`info`·업데이트 알림, CI 빌드 | 서명한 색인을 읽어 설치 |
| 6 | 나쁜 모듈 시험(무한 루프, 메모리 폭주, 큰 응답, 느린 호출, 막힌 도메인, 트랩 반복으로 자동 끄기), 미니PC 속도·메모리, 마무리 ADR·GAP | 아래 완료 기준 |

단계마다 게이트를 통과하면 커밋하고, 브라우저로 확인할 것이 있으면 확인한다(0단계는 커밋하지 않고 결과만 ADR 로).

## 완료 기준 (SPEC 에 없어 여기서 정한다)

- 서명한 예시 모듈을 URL 하나로 설치 → 켜기 → 화면·툴·위젯·이벤트 동작 → 끄기·제거 뒤 코어 표 행 수가 그대로.
- 서명 틀림·권한 밖 호출·무한 루프·메모리 폭주 모듈이 사이트를 멈추지 않는다(e2e, 실제 바이너리).
- 바이너리가 ADR-0176 에서 정한 새 한도 이하(게이트가 잰다). 외부 모듈이 없을 때 유휴 메모리 50MB 이하.
- 에이전트에게 「방명록 모듈 달아줘」 한마디 → changeset 미리보기(권한 목록) → 사람 승인 → 설치·켜기.

## 사용자 몫 (막지 않고 남은 일로 적는다)

- ~~크기 한도~~: 「잰 값 + 2MB」로 승인받았다(2026-11-24).
- 공식 저장소를 GitHub 에 공개로 만드는 것, 공식 서명 키 만들기와 개인 키 보관(우리는 공개 키만 커밋한다).

## 하지 않는 것 (v1)

- 외부 모듈의 SQL·자기 표, 스크립트, 발행·회원 관리·사이트 설정 권한, 다른 모듈 부르기(결제 `Checkout` 포함), 메일 보내기.
- 남이 준 `.cwasm`, 서명 없는 URL 설치, 유료 모듈·마켓.
- 내장 모듈 17개를 WASM 으로 옮기기(그대로 둔다).

## 위험

- wasmtime 이 달마다 큰 판을 낸다. 판을 올릴 때마다 시작할 때 다시 컴파일(N100 에서 걸리는 시간을 0단계·6단계에서 잰다)하고, 보안 권고는 지금처럼 `cargo deny`·`cargo audit` 로 본다.
- 컴파일러(Cranelift)가 바이너리에 들어가므로 wasmtime 보안 권고의 대부분이 우리 몫이 된다. 판 올리기를 미루지 않는다.
- 호스트 API 를 한 번 내놓으면 바꾸기 어렵다. WIT 패키지에 판(`@0.1.0`)을 달고, 기와는 한 판 이상을 같이 받는다.
