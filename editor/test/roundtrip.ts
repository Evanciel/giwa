// Markdown 왕복 검사. `npm test` 로 돈다 (Node, 브라우저 없음).
// 서식 편집기로 열 수 있는 글은 다시 써도 같은 HTML 이 되고, 두 번 써도 결과가 같아야 한다.
// 열 수 없는 글(표·HTML·할 일·각주)은 Markdown 편집으로 보내야 한다.

import { readFileSync } from "fs";

import { load, parse, serialize } from "../src/markdown";
import { videoEmbed } from "../src/syntax";
import { splitTags } from "../src/tags";

let failed = 0;
function check(name: string, cond: boolean, detail = "") {
  if (cond) {
    console.log(`ok   ${name}`);
  } else {
    failed++;
    console.log(`FAIL ${name}\n${detail}`);
  }
}

const rich: Record<string, string> = {
  "문단·한글": "안녕하세요. 첫 글입니다.\n\n두 번째 문단이에요.",
  "제목": "# 큰 제목\n\n## 중간 제목\n\n### 작은 제목\n\n본문",
  "굵게·기울임·취소선·코드": "**굵게** *기울임* ~~취소~~ `코드` 섞어서 **_둘 다_**",
  "링크": "[마루](https://example.test/a?b=1&c=2) 와 [상대 경로](/b/blog)",
  "이미지": "![노란 벽](/media/2026/11/01abc.jpg)\n\n![](/media/2026/11/x.png \"제목\")",
  "인용": "> 인용문 한 줄\n>\n> 두 번째 문단",
  "목록": "- 하나\n- 둘\n  - 둘의 하나\n- 셋\n\n1. 첫째\n2. 둘째",
  "느슨한 목록": "* 하나\n\n* 둘\n\n3. 셋째\n\n4. 넷째",
  "코드 블록": "```rust\nfn main() {\n    println!(\"안녕\");\n}\n```",
  "구분선": "위\n\n---\n\n아래",
  "줄바꿈": "첫 줄  \n둘째 줄",
  "특수 문자": "별표 \\* 와 밑줄 \\_ 그리고 1\\. 숫자 점",
  "빈 글": "",
  "할 일 목록": "- [ ] 장보기\n- [x] 청소\n  - [ ] 창문 닦기\n\n1. [ ] 첫째",
  "빈 할 일": "- [ ] \n- [x] 끝",
  "각주": "본문에 각주[^1] 두 개[^메모]\n\n[^1]: 첫 번째 각주 **굵게**\n\n[^메모]: 두 번째",
  "강조 상자": ":::info\n알아 두세요.\n\n- 목록도 됩니다\n:::\n\n:::warn\n**조심**\n:::",
  "겹친 상자·정렬": ":::tip\n바깥\n\n:::center\n가운데\n:::\n:::\n\n:::right\n오른쪽\n:::",
  "글자 꾸밈": "이건 :color[빨간 글자]{red} 와 :mark[형광펜 **굵게**]{yellow} 그리고 :size[큰 글자]{large}",
  "닫히지 않은 상자는 글자": ":::info\n닫는 줄 없음",
  "사진 크기·캡션": "![바다](/media/2026/11/a.jpg#w50 \"동해 바다\")\n\n글 ![산](/media/2026/11/b.jpg#w75) 끝",
  "갤러리": ":::gallery\n![a](/media/a.jpg) ![b](/media/b.jpg)\n![c](/media/c.jpg)\n:::",
  "정해지지 않은 크기 조각은 주소 그대로": "![x](/media/a.jpg#w33)",
  "표": "| 이름 | 값 |\n|---|---|\n| 가 | 1 |",
  "표 정렬·서식·빈 칸": "| 항목 | 금액 | 메모 |\n| :--- | ---: | :---: |\n| **숙소** | 12만 |  |\n| `a|b` | 3\\|4 | [링크](/x) |",
  "열 수가 다른 줄": "| a | b |\n|---|---|\n| 1 |\n| 1 | 2 |",
  "동영상·장소": "위\n\n::video[https://youtu.be/dQw4w9WgXcQ]\n\n::place[강릉 안목해변]{강원 강릉시 창해로14번길}\n\n::place[집 앞 카페]",
  "상자 안 동영상": ":::center\n::video[https://www.youtube.com/watch?v=dQw4w9WgXcQ&t=3]\n:::",
  "문단에 붙은 동영상 줄은 글자": "글\n::video[https://youtu.be/dQw4w9WgXcQ]",
  "모르는 주소는 글자": "::video[https://example.com/v/1]",
  "첨부 파일 링크": "[견적서 \[최종\].pdf (12KB)](/files/01m6eft46wt5sp5seq1xjbbryw) 받아 보세요",
};

// 확장 문법이 노드·마크로 읽혀야 한다.
{
  const r = load(":::warn\n주의 :color[빨강]{red}\n:::");
  const names: string[] = [];
  if (r.ok) r.doc.descendants((node) => void names.push(node.type.name, ...node.marks.map((x) => `${x.type.name}=${x.attrs.value}`)));
  check("상자·꾸밈 노드", r.ok && names.includes("box") && names.includes("color=red"), names.join(","));
  const bad = load("이건 :color[가]{magenta}");
  check("팔레트 밖 색은 글자 그대로", bad.ok && bad.doc.textContent.includes("{magenta}"), bad.ok ? bad.doc.textContent : "");
}

// 사진 크기는 속성으로, 캡션은 제목으로 읽혀야 한다.
{
  const r = load("![바다](/media/a.jpg#w50 \"동해\")");
  let attrs: Record<string, unknown> = {};
  if (r.ok) r.doc.descendants((node) => void (node.type.name === "image" && (attrs = node.attrs)));
  check("사진 크기·캡션 속성", attrs.src === "/media/a.jpg" && attrs.width === 50 && attrs.title === "동해", JSON.stringify(attrs));
  check("사진 크기 다시 쓰기", r.ok && serialize(r.doc) === "![바다](/media/a.jpg#w50 \"동해\")", r.ok ? serialize(r.doc) : "");
  const g = load(":::gallery\n![a](/a.png)\n:::");
  check("갤러리 상자", g.ok && g.doc.firstChild?.type.name === "box" && g.doc.firstChild.attrs.kind === "gallery");
  // 접기: 새 노드 없이 상자 하나. 첫 문단(요약)이 첫 자식으로 남아야 서버와 같은 뜻이 된다.
  const faq = ":::details\n환불은 **언제** 되나요?\n\n결제 후 7일 안에 됩니다.\n:::";
  const d = load(faq);
  check("접기 상자", d.ok && d.doc.firstChild?.type.name === "box" && d.doc.firstChild.attrs.kind === "details");
  check("접기 요약은 첫 문단", d.ok && d.doc.firstChild?.firstChild?.type.name === "paragraph" && d.doc.firstChild.firstChild.textContent === "환불은 언제 되나요?");
  check("접기 다시 쓰기", d.ok && serialize(d.doc) === faq, d.ok ? serialize(d.doc) : "");
}

// 동영상·장소는 노드로, 주소 판별은 서버와 같은 규칙으로.
{
  const r = load("::video[https://youtu.be/dQw4w9WgXcQ]\n\n::place[카페]{서울 중구}");
  const types: string[] = [];
  if (r.ok) r.doc.forEach((node) => void types.push(node.type.name));
  check("동영상·장소 노드", types.join(",") === "video,place", types.join(","));
  const cases: [string, string | null][] = [
    ["https://youtu.be/dQw4w9WgXcQ", "https://www.youtube-nocookie.com/embed/dQw4w9WgXcQ"],
    ["https://www.youtube.com/watch?v=dQw4w9WgXcQ", "https://www.youtube-nocookie.com/embed/dQw4w9WgXcQ"],
    ["https://m.youtube.com/watch?feature=x&v=dQw4w9WgXcQ#t=1", "https://www.youtube-nocookie.com/embed/dQw4w9WgXcQ"],
    ["https://youtube.com/shorts/dQw4w9WgXcQ?si=1", "https://www.youtube-nocookie.com/embed/dQw4w9WgXcQ"],
    ["https://vimeo.com/76979871", "https://player.vimeo.com/video/76979871"],
    ["https://youtu.be/short", null],
    ["https://evil.example/youtube.com/watch?v=dQw4w9WgXcQ", null],
    ["javascript:alert(1)", null],
    ["https://www.youtube.com/watch?v=dQw4w9WgXcQ\"onload=x", null],
  ];
  for (const [url, want] of cases) check(`동영상 주소: ${url}`, videoEmbed(url) === want, String(videoEmbed(url)));
}

// 할 일·각주는 노드로 읽혀야 한다 (글자로 남으면 안 된다).
{
  const r = load("- [x] 한 일\n\n본문[^1]\n\n[^1]: 각주");
  const types: string[] = [];
  if (r.ok) r.doc.descendants((node) => void types.push(node.type.name + (node.attrs.checked != null ? `:${node.attrs.checked}` : "")));
  check("할 일·각주 노드", r.ok && types.includes("list_item:true") && types.includes("footnote_ref") && types.includes("footnote_def"), types.join(","));
  check("체크 표시가 글자로 남지 않음", r.ok && !r.doc.textContent.includes("[x]"), r.ok ? r.doc.textContent : "");
}

// 서버의 기본 글 틀(templates.rs)은 서식 모드로 열려야 한다. 원본을 직접 읽어 어긋나지 않게 한다.
{
  const rs = readFileSync("../crates/core/src/services/templates.rs", "utf8");
  const found = [...rs.matchAll(/\(\s*"(\w+)",\s*"([^"]+)",\s*"((?:[^"\\]|\\.)*)",\s*\)/g)];
  check("기본 글 틀을 찾음", found.length === 4, String(found.length));
  for (const [, , name, body] of found) rich[`기본 틀: ${name}`] = JSON.parse(`"${body}"`);
}

for (const [name, md] of Object.entries(rich)) {
  const r = load(md);
  check(`열 수 있음: ${name}`, r.ok, r.ok ? "" : r.reason);
  if (!r.ok) continue;
  const once = serialize(r.doc);
  const twice = serialize(parse(once));
  check(`두 번 써도 같음: ${name}`, once === twice, `1회:\n${once}\n2회:\n${twice}`);
}

const plain: Record<string, string> = {
  "칸 안에 HTML 이 있는 표": "| 이름 | 값 |\n|---|---|\n| <kbd>가</kbd> | 1 |",
  "HTML 블록": "<div class=\"x\">상자</div>",
  "인라인 HTML": "글자 <kbd>Ctrl</kbd> 누르기",
  "여러 문단 각주": "본문[^1]\n\n[^1]: 첫 문단\n\n    둘째 문단",
};

for (const [name, md] of Object.entries(plain)) {
  const r = load(md);
  check(`Markdown 으로 보냄: ${name}`, !r.ok, r.ok ? serialize(r.doc) : "");
}

// 태그 칸
{
  const same = (a: string[], b: string[]) => JSON.stringify(a) === JSON.stringify(b);
  const got = splitTags(" #Rust, 제주   여행 ,rust,, ##맛집 ");
  check("태그: #·공백 정리, 대소문자 겹침 제거", same(got, ["Rust", "제주 여행", "맛집"]), JSON.stringify(got));
  check("태그: 빈 값", splitTags(" , ,").length === 0);
}

if (failed > 0) {
  console.log(`\n${failed}건 실패`);
  process.exit(1);
}
console.log("\n모두 통과");
