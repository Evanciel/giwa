// markdown-it 에 서버(pulldown-cmark)와 같은 문법 두 가지를 더한다: 할 일 목록, 각주.
// 서식 편집기 전용이다. 비교 렌더러(compareMd)에는 붙이지 않는다.

import type MarkdownIt from "markdown-it";
import type StateBlock from "markdown-it/lib/rules_block/state_block.mjs";
import type StateCore from "markdown-it/lib/rules_core/state_core.mjs";
import type StateInline from "markdown-it/lib/rules_inline/state_inline.mjs";

const TASK = /^\[([ xX])\](?:[ \t]+|$)/;

/** `- [ ] 할 일`, `- [x] 한 일`. 목록 항목의 첫 문단 앞머리만 본다. */
function taskLists(state: StateCore) {
  const t = state.tokens;
  for (let i = 2; i < t.length; i++) {
    if (t[i].type !== "inline" || t[i - 1].type !== "paragraph_open" || t[i - 2].type !== "list_item_open") continue;
    const m = t[i].content.match(TASK);
    if (!m) continue;
    const first = t[i].children?.[0];
    if (!first || first.type !== "text" || !first.content.startsWith(m[0].trimEnd())) continue;
    t[i - 2].attrSet("checked", m[1] === " " ? "false" : "true");
    t[i].content = t[i].content.slice(m[0].length);
    first.content = first.content.slice(m[0].trimEnd().length).replace(/^[ \t]+/, "");
    if (!first.content) t[i].children?.shift();
  }
}

const DEF = /^\[\^([^\]\s]+)\]:[ \t]*/;
const REF = /^\[\^([^\]\s]+)\]/;

/** `[^1]: 각주 내용` (한 문단짜리). 여러 문단 각주는 다루지 않는다 → 손실 검사가 Markdown 으로 보낸다. */
function footnoteDef(state: StateBlock, startLine: number, endLine: number, silent: boolean): boolean {
  const pos = state.bMarks[startLine] + state.tShift[startLine];
  const max = state.eMarks[startLine];
  if (state.sCount[startLine] - state.blkIndent >= 4) return false;
  const m = state.src.slice(pos, max).match(DEF);
  if (!m) return false;
  let next = startLine + 1;
  const lines = [state.src.slice(pos + m[0].length, max)];
  while (next < endLine && !state.isEmpty(next)) {
    const s = state.bMarks[next] + state.tShift[next];
    const line = state.src.slice(s, state.eMarks[next]);
    if (DEF.test(line)) break;
    lines.push(line);
    next++;
  }
  // 빈 줄 뒤에 들여쓴 줄이 오면 각주의 둘째 문단이다(서버는 그렇게 읽는다). 다루지 않는다.
  let after = next;
  while (after < endLine && state.isEmpty(after)) after++;
  if (after > next && after < endLine && state.sCount[after] - state.blkIndent >= 4) return false;
  if (silent) return true;
  const open = state.push("footnote_def_open", "div", 1);
  open.meta = { label: m[1] };
  open.map = [startLine, next];
  const inline = state.push("inline", "", 0);
  inline.content = lines.join("\n").trim();
  inline.map = [startLine, next];
  inline.children = [];
  state.push("footnote_def_close", "div", -1);
  state.line = next;
  return true;
}

/** 본문의 `[^1]`. */
function footnoteRef(state: StateInline, silent: boolean): boolean {
  if (state.src.charCodeAt(state.pos) !== 0x5b || state.src.charCodeAt(state.pos + 1) !== 0x5e) return false;
  const m = state.src.slice(state.pos).match(REF);
  if (!m) return false;
  if (!silent) {
    const tok = state.push("footnote_ref", "sup", 0);
    tok.meta = { label: m[1] };
  }
  state.pos += m[0].length;
  return true;
}

export function maruSyntax(md: MarkdownIt) {
  md.core.ruler.push("maru_tasks", taskLists);
  md.block.ruler.before("reference", "footnote_def", footnoteDef, { alt: ["paragraph", "reference"] });
  md.inline.ruler.before("link", "footnote_ref", footnoteRef);
}

// ---- 확장 문법 (ADR-0034). 서버 렌더러(crates/core/src/markdown.rs)와 같은 규칙이어야 한다 ----

/** 블록 상자: 강조 상자 4종, 정렬 2종, 사진 격자, 접기(첫 문단이 요약, ADR-0041). */
export const BOX_KINDS = ["info", "tip", "warn", "danger", "center", "right", "gallery", "details"] as const;
/** 글자 꾸밈: 이름 → 쓸 수 있는 값. 테마 팔레트 안의 이름만 받는다 (ADR-0046).
 *  크기의 small·large·xlarge 는 예전 글을 위해 남긴 값이라 메뉴에는 없다. */
export const SPAN_VALUES: Record<string, readonly string[]> = {
  color: ["red", "orange", "ochre", "brown", "green", "teal", "blue", "navy", "purple", "wine", "pink", "gray"],
  mark: ["yellow", "orange", "green", "mint", "blue", "purple", "pink", "gray"],
  size: ["11", "13", "15", "16", "19", "24", "28", "30", "34", "38", "small", "large", "xlarge"],
  font: ["serif", "pen", "brush", "mono"],
};

const BOX_OPEN = new RegExp(`^:::[ \t]*(${BOX_KINDS.join("|")})[ \t]*$`);
const BOX_CLOSE = /^:::[ \t]*$/;

/** `:::info` … `:::`. 닫는 줄이 없으면 문법이 아니다(서버도 글자 그대로 둔다). */
function boxRule(state: StateBlock, startLine: number, endLine: number, silent: boolean): boolean {
  // 서버와 같은 조건: 맨 바깥(또는 상자 안)에서, 들여쓰지 않은 줄만. 목록·인용 안의 `:::` 는 글자다.
  if (state.parentType !== "root" && state.parentType !== ("maru_box" as typeof state.parentType)) return false;
  if (state.sCount[startLine] !== 0) return false;
  const lineText = (l: number) => state.src.slice(state.bMarks[l] + state.tShift[l], state.eMarks[l]);
  // 여는 줄 앞은 빈 줄, 글의 처음, 또는 다른 상자 줄이어야 한다.
  const opensAt = (l: number) => {
    if (state.sCount[l] !== 0 || !BOX_OPEN.test(lineText(l))) return false;
    if (l === 0 || state.isEmpty(l - 1)) return true;
    const prev = lineText(l - 1);
    return state.sCount[l - 1] === 0 && (BOX_OPEN.test(prev) || BOX_CLOSE.test(prev));
  };
  if (!opensAt(startLine)) return false;
  const m = lineText(startLine).match(BOX_OPEN);
  if (!m) return false;
  // 짝이 맞는 닫는 줄을 찾는다. 안쪽 상자와 코드 울타리 안의 `:::` 는 건너뛴다.
  let depth = 1;
  let fence = "";
  let close = -1;
  for (let l = startLine + 1; l < endLine; l++) {
    const t = lineText(l);
    const f = state.sCount[l] < 4 ? t.match(/^(`{3,}|~{3,})/) : null;
    if (fence) {
      if (f && f[1][0] === fence[0] && f[1].length >= fence.length && t.slice(f[1].length).trim() === "") fence = "";
      continue;
    }
    if (f) {
      fence = f[1];
      continue;
    }
    if (state.sCount[l] !== 0) continue;
    if (opensAt(l)) depth++;
    else if (BOX_CLOSE.test(t) && --depth === 0) {
      close = l;
      break;
    }
  }
  if (close < 0) return false;
  if (silent) return true;
  const oldParent = state.parentType;
  const oldMax = state.lineMax;
  state.parentType = "maru_box" as typeof state.parentType;
  state.lineMax = close;
  const open = state.push("maru_box_open", "div", 1);
  open.block = true;
  open.meta = { kind: m[1] };
  open.map = [startLine, close + 1];
  state.md.block.tokenize(state, startLine + 1, close);
  const end = state.push("maru_box_close", "div", -1);
  end.block = true;
  state.parentType = oldParent;
  state.lineMax = oldMax;
  state.line = close + 1;
  return true;
}

// ---- 한 줄 블록: `::video[주소]`, `::place[이름]{주소}` (ADR-0036) ----

const LEAF = /^::(video|place)\[([^\]\n]*)\](?:\{([^}\n]*)\})?[ \t]*$/;

function ytId(id: string): string | null {
  return /^[A-Za-z0-9_-]{11}$/.test(id) ? `https://www.youtube-nocookie.com/embed/${id}` : null;
}

/**
 * 유튜브·비메오 주소 → 넣을 수 있는 재생기 주소. 다른 주소는 null.
 * 서버 `directives::video_embed` 와 같은 규칙이어야 한다(정규식 없이 같은 순서로 자른다).
 */
export function videoEmbed(url: string): string | null {
  if (/\s/.test(url)) return null;
  const rest = url.startsWith("https://") ? url.slice(8) : url.startsWith("http://") ? url.slice(7) : null;
  if (rest === null) return null;
  const cut = rest.search(/[?#]/);
  const path = cut < 0 ? rest : rest.slice(0, cut);
  const query = cut < 0 ? "" : rest.slice(cut);
  const slash = path.indexOf("/");
  if (slash < 0) return null;
  let host = path.slice(0, slash);
  const p = path.slice(slash + 1);
  if (host.startsWith("www.")) host = host.slice(4);
  else if (host.startsWith("m.")) host = host.slice(2);
  if (host === "youtu.be") return ytId(p);
  if (host === "youtube.com") {
    if (p === "watch") {
      if (!query.startsWith("?")) return null;
      const q = query.slice(1).split("#")[0];
      const v = q.split("&").find((kv) => kv.startsWith("v="));
      return v ? ytId(v.slice(2)) : null;
    }
    for (const pre of ["shorts/", "embed/", "live/"]) if (p.startsWith(pre)) return ytId(p.slice(pre.length));
    return null;
  }
  if (host === "vimeo.com") return /^\d{1,12}$/.test(p) ? `https://player.vimeo.com/video/${p}` : null;
  return null;
}

/** 한 줄 블록. 앞은 빈 줄·글 처음·상자 줄, 뒤는 빈 줄·글 끝·상자 닫는 줄이어야 한다(서버와 같은 조건). */
function leafRule(state: StateBlock, startLine: number, endLine: number, silent: boolean): boolean {
  if (state.parentType !== "root" && state.parentType !== ("maru_box" as typeof state.parentType)) return false;
  if (state.sCount[startLine] !== 0) return false;
  const lineText = (l: number) => state.src.slice(state.bMarks[l] + state.tShift[l], state.eMarks[l]);
  const m = lineText(startLine).match(LEAF);
  if (!m) return false;
  const prevOk =
    startLine === 0 ||
    state.isEmpty(startLine - 1) ||
    (state.sCount[startLine - 1] === 0 && (BOX_OPEN.test(lineText(startLine - 1)) || BOX_CLOSE.test(lineText(startLine - 1))));
  const next = startLine + 1;
  const nextOk = next >= endLine || state.isEmpty(next) || (state.sCount[next] === 0 && BOX_CLOSE.test(lineText(next)));
  if (!prevOk || !nextOk) return false;
  const [, kind, label, value] = m;
  if (kind === "video" && (value !== undefined || !videoEmbed(label))) return false;
  if (kind === "place" && !label.trim()) return false;
  if (silent) return true;
  const tok = state.push(`maru_${kind}`, "div", 0);
  tok.block = true;
  tok.map = [startLine, next];
  tok.meta = kind === "video" ? { url: label } : { name: label.trim(), addr: (value ?? "").trim() };
  state.line = next;
  return true;
}

const SPAN_OPEN = /^:(color|mark|size|font)\[/;

/** `:color[글자]{red}`. 안쪽 글자에는 다른 서식이 들어갈 수 있다. */
function spanRule(state: StateInline, silent: boolean): boolean {
  if (state.src.charCodeAt(state.pos) !== 0x3a) return false;
  const m = state.src.slice(state.pos, state.pos + 8).match(SPAN_OPEN);
  if (!m) return false;
  const labelStart = state.pos + m[0].length;
  const labelEnd = state.md.helpers.parseLinkLabel(state, labelStart - 1, false);
  if (labelEnd < 0) return false;
  const v = state.src.slice(labelEnd + 1, state.posMax).match(/^\{([a-z0-9]+)\}/);
  if (!v || !SPAN_VALUES[m[1]].includes(v[1])) return false;
  if (!silent) {
    const max = state.posMax;
    state.pos = labelStart;
    state.posMax = labelEnd;
    const open = state.push(`maru_${m[1]}_open`, "span", 1);
    open.meta = { value: v[1] };
    state.md.inline.tokenize(state);
    state.push(`maru_${m[1]}_close`, "span", -1);
    state.posMax = max;
  }
  state.pos = labelEnd + 1 + v[0].length;
  return true;
}

export function maruDirectives(md: MarkdownIt) {
  // 문단을 끊지 않는다(서버도 빈 줄 뒤의 `:::` 만 상자로 본다).
  md.block.ruler.before("fence", "maru_box", boxRule, { alt: ["reference", "blockquote", "list"] });
  md.block.ruler.before("fence", "maru_leaf", leafRule);
  md.inline.ruler.before("link", "maru_span", spanRule);
}
