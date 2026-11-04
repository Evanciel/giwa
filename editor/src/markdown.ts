// Markdown ↔ ProseMirror 문서 변환과 무손실 검사.
//
// 저장 형식은 언제나 Markdown 이다. 서식 편집기는 Markdown 을 문서로 읽고, 고칠 때마다 다시
// Markdown 으로 쓴다. 다시 쓴 결과가 원문과 다르게 그려지면(표, HTML, 할 일 목록 등)
// 서식 편집기로 열지 않고 Markdown 편집으로 연다. 사용자의 글이 몰래 바뀌는 일을 막는다.

import MarkdownIt from "markdown-it";
import {
  MarkdownParser,
  MarkdownSerializer,
  defaultMarkdownParser,
  defaultMarkdownSerializer,
  schema as baseSchema,
} from "prosemirror-markdown";
import { Node, NodeSpec, Schema } from "prosemirror-model";

import { maruDirectives, maruSyntax, spanValueOk } from "./syntax";

// 새로 만드는 목록은 촘촘하게(항목 사이 빈 줄 없이) 쓴다. 읽어 온 목록은 원문의 간격을 따른다.
function tightByDefault(name: string) {
  const spec = baseSchema.spec.nodes.get(name);
  return { ...spec, attrs: { ...spec?.attrs, tight: { default: true } } };
}

// 할 일 항목은 `checked` 가 true/false, 보통 항목은 null.
const listItem: NodeSpec = {
  content: "block+",
  defining: true,
  attrs: { checked: { default: null } },
  parseDOM: [
    {
      tag: "li",
      getAttrs: (dom) => {
        const v = (dom as HTMLElement).getAttribute("data-checked");
        return { checked: v === null ? null : v === "true" };
      },
    },
  ],
  toDOM: (node) =>
    node.attrs.checked === null ? ["li", 0] : ["li", { class: "me-task", "data-checked": String(node.attrs.checked) }, 0],
};

const footnoteRef: NodeSpec = {
  inline: true,
  group: "inline",
  atom: true,
  attrs: { label: {} },
  parseDOM: [{ tag: "sup.me-fnref", getAttrs: (dom) => ({ label: (dom as HTMLElement).dataset.label }) }],
  toDOM: (node) => ["sup", { class: "me-fnref", "data-label": node.attrs.label, title: `각주 ${node.attrs.label}` }, node.attrs.label],
};

const footnoteDef: NodeSpec = {
  group: "block",
  content: "inline*",
  defining: true,
  attrs: { label: {} },
  parseDOM: [{ tag: "div.me-fndef", getAttrs: (dom) => ({ label: (dom as HTMLElement).dataset.label }) }],
  toDOM: (node) => ["div", { class: "me-fndef", "data-label": node.attrs.label }, 0],
};

// 강조 상자(info·tip·warn·danger)와 정렬(center·right). 공개 화면과 같은 클래스를 쓴다.
const box: NodeSpec = {
  group: "block",
  content: "block+",
  defining: true,
  attrs: { kind: {} },
  parseDOM: [{ tag: "div[data-box]", getAttrs: (dom) => ({ kind: (dom as HTMLElement).dataset.box }) }],
  toDOM: (node) => ["div", { "data-box": node.attrs.kind, class: boxClass(node.attrs.kind) }, 0],
};

export function boxClass(kind: string): string {
  if (kind === "gallery") return "gallery";
  if (kind === "details") return "fold";
  if (kind.startsWith("quote-")) return `quote ${kind}`;
  return kind === "center" || kind === "right" ? `align-${kind}` : `callout callout-${kind}`;
}

/** 사진 크기 단계(%). 100% 는 조각 없이 쓴다. 서버 `markdown::IMAGE_WIDTHS` 와 같아야 한다. */
export const IMAGE_WIDTHS = [75, 50] as const;

/** `사진.jpg#w50` → ["사진.jpg", 50]. 정해진 값이 아니면 조각을 그대로 둔다. */
export function splitWidth(src: string): [string, number | null] {
  const m = /#w(\d+)$/.exec(src);
  if (m && (IMAGE_WIDTHS as readonly number[]).includes(Number(m[1]))) return [src.slice(0, m.index), Number(m[1])];
  return [src, null];
}

// 그림. 크기(`width`)는 주소 끝 조각(`#w50`)으로, 캡션은 제목(`"캡션"`)으로 저장한다.
const image: NodeSpec = {
  inline: true,
  group: "inline",
  draggable: true,
  attrs: { src: {}, alt: { default: null }, title: { default: null }, width: { default: null } },
  parseDOM: [
    {
      tag: "img[src]",
      getAttrs: (dom) => {
        const el = dom as HTMLElement;
        const w = Number(el.getAttribute("data-width"));
        return {
          src: el.getAttribute("src"),
          title: el.getAttribute("title"),
          alt: el.getAttribute("alt"),
          width: (IMAGE_WIDTHS as readonly number[]).includes(w) ? w : null,
        };
      },
    },
  ],
  toDOM: (node) => {
    const { src, alt, title, width } = node.attrs;
    return ["img", { src, alt, title, "data-width": width, class: width ? `img-w${width}` : null }];
  },
};

// 표(GFM). 칸 병합은 Markdown 표에 없으므로 다루지 않는다. 첫 줄이 머리글이고, 정렬은 열마다 하나다.
// 관리 화면 CSP 가 인라인 style 을 막으므로 정렬은 data-align 속성 + CSS 로 보인다.
export type Align = "left" | "center" | "right" | null;

const table: NodeSpec = {
  group: "block",
  content: "table_row+",
  isolating: true,
  parseDOM: [{ tag: "table" }],
  toDOM: () => ["div", { class: "me-table" }, ["table", ["tbody", 0]]],
};

const tableRow: NodeSpec = {
  content: "table_cell+",
  parseDOM: [{ tag: "tr" }],
  toDOM: () => ["tr", 0],
};

const tableCell: NodeSpec = {
  content: "inline*",
  isolating: true,
  attrs: { header: { default: false }, align: { default: null } },
  parseDOM: [
    { tag: "th", getAttrs: (dom) => ({ header: true, align: (dom as HTMLElement).getAttribute("data-align") }) },
    { tag: "td", getAttrs: (dom) => ({ header: false, align: (dom as HTMLElement).getAttribute("data-align") }) },
  ],
  toDOM: (node) => [node.attrs.header ? "th" : "td", { "data-align": node.attrs.align }, 0],
};

// 동영상·장소 (ADR-0036). 편집 화면에서는 카드로만 보여 준다(관리 화면 CSP 가 외부 iframe 을 막는다).
const video: NodeSpec = {
  group: "block",
  atom: true,
  attrs: { url: {} },
  parseDOM: [{ tag: "div.me-video", getAttrs: (dom) => ({ url: (dom as HTMLElement).dataset.url }) }],
  toDOM: (node) => ["div", { class: "me-embed me-video", "data-url": node.attrs.url }, `▶ 동영상 · ${node.attrs.url}`],
};

const place: NodeSpec = {
  group: "block",
  atom: true,
  attrs: { name: {}, addr: { default: "" } },
  parseDOM: [
    {
      tag: "div.me-place",
      getAttrs: (dom) => ({ name: (dom as HTMLElement).dataset.name, addr: (dom as HTMLElement).dataset.addr ?? "" }),
    },
  ],
  toDOM: (node) => [
    "div",
    { class: "me-embed me-place", "data-name": node.attrs.name, "data-addr": node.attrs.addr },
    ["strong", `📍 ${node.attrs.name}`],
    ["span", node.attrs.addr],
  ],
};

function alignOf(style: string | null): Align {
  const m = /text-align:\s*(left|center|right)/.exec(style ?? "");
  return m ? (m[1] as Align) : null;
}

/** 글자 꾸밈 마크. `prefix` 는 공개 화면 클래스 앞머리(tc-red, hl-yellow, fs-large). */
function spanMark(name: string, prefix: string) {
  return {
    attrs: { value: {} },
    parseDOM: [
      {
        tag: `span[data-${name}]`,
        // 붙여 넣은 HTML 의 모르는 값은 버린다 (저장하면 문법이 글자로 남는다).
        getAttrs: (dom: HTMLElement | string) => {
          const value = (dom as HTMLElement).getAttribute(`data-${name}`) ?? "";
          return spanValueOk(name, value) ? { value } : false;
        },
      },
    ],
    toDOM: (mark: { attrs: { value: string } }) => ["span", { [`data-${name}`]: mark.attrs.value, class: `${prefix}-${mark.attrs.value}` }, 0] as const,
  };
}

export const schema = new Schema({
  nodes: baseSchema.spec.nodes
    .update("bullet_list", tightByDefault("bullet_list"))
    .update("ordered_list", tightByDefault("ordered_list"))
    .update("list_item", listItem)
    .update("image", image)
    .addToEnd("footnote_ref", footnoteRef)
    .addToEnd("footnote_def", footnoteDef)
    .addToEnd("box", box)
    .addToEnd("table", table)
    .addToEnd("table_row", tableRow)
    .addToEnd("table_cell", tableCell)
    .addToEnd("video", video)
    .addToEnd("place", place),
  marks: baseSchema.spec.marks
    .addToEnd("strike", {
      parseDOM: [{ tag: "s" }, { tag: "del" }, { tag: "strike" }, { style: "text-decoration=line-through" }],
      toDOM: () => ["s", 0],
    })
    .addToEnd("color", spanMark("color", "tc"))
    .addToEnd("highlight", spanMark("mark", "hl"))
    .addToEnd("size", spanMark("size", "fs"))
    .addToEnd("font", spanMark("font", "ff")),
});

// 서식 편집기가 읽는 문법: CommonMark + 취소선 + Maru 확장. HTML 은 토큰으로 받아서 "다룰 수 없음" 으로 걸러낸다.
const readMd = MarkdownIt("commonmark", { html: true })
  .enable(["strikethrough", "table"])
  .use(maruSyntax)
  .use(maruDirectives);

const parser = new MarkdownParser(schema, readMd, {
  ...defaultMarkdownParser.tokens,
  s: { mark: "strike" },
  image: {
    node: "image",
    getAttrs: (tok) => {
      const [src, width] = splitWidth(tok.attrGet("src") ?? "");
      return {
        src,
        width,
        title: tok.attrGet("title") || null,
        alt: (tok.children?.[0] && tok.children[0].content) || null,
      };
    },
  },
  list_item: {
    block: "list_item",
    getAttrs: (tok) => {
      const v = tok.attrGet("checked");
      return { checked: v === null ? null : v === "true" };
    },
  },
  footnote_ref: { node: "footnote_ref", getAttrs: (tok) => ({ label: tok.meta.label }) },
  footnote_def: { block: "footnote_def", getAttrs: (tok) => ({ label: tok.meta.label }) },
  maru_box: { block: "box", getAttrs: (tok) => ({ kind: tok.meta.kind }) },
  maru_video: { node: "video", getAttrs: (tok) => ({ url: tok.meta.url }) },
  maru_place: { node: "place", getAttrs: (tok) => ({ name: tok.meta.name, addr: tok.meta.addr }) },
  table: { block: "table" },
  thead: { ignore: true },
  tbody: { ignore: true },
  tr: { block: "table_row" },
  th: { block: "table_cell", getAttrs: (tok) => ({ header: true, align: alignOf(tok.attrGet("style")) }) },
  td: { block: "table_cell", getAttrs: (tok) => ({ header: false, align: alignOf(tok.attrGet("style")) }) },
  maru_color: { mark: "color", getAttrs: (tok) => ({ value: tok.meta.value }) },
  maru_mark: { mark: "highlight", getAttrs: (tok) => ({ value: tok.meta.value }) },
  maru_size: { mark: "size", getAttrs: (tok) => ({ value: tok.meta.value }) },
  maru_font: { mark: "font", getAttrs: (tok) => ({ value: tok.meta.value }) },
});

/** `:이름[` … `]{값}`. 값은 닫을 때 쓴다. */
function spanSyntax(name: string) {
  return {
    open: `:${name}[`,
    close: (_s: unknown, mark: { attrs: { value: string } }) => `]{${mark.attrs.value}}`,
    mixable: true,
    expelEnclosingWhitespace: true,
  };
}

const serializer = new MarkdownSerializer(
  {
    ...defaultMarkdownSerializer.nodes,
    // 한국어 글에서 흔한 `-` 글머리표로 쓴다.
    bullet_list(state, node) {
      state.renderList(node, "  ", () => "- ");
    },
    list_item(state, node) {
      if (node.attrs.checked !== null) state.write(node.attrs.checked ? "[x] " : "[ ] ");
      state.renderContent(node);
    },
    image(state, node, parent, index) {
      // 기본 직렬화(대체 텍스트·주소·제목 이스케이프)를 그대로 쓰고 주소 끝에 크기 조각만 붙인다.
      const width = node.attrs.width as number | null;
      const src = width ? `${node.attrs.src}#w${width}` : node.attrs.src;
      const withWidth = { ...node, attrs: { ...node.attrs, src } } as unknown as Node;
      defaultMarkdownSerializer.nodes.image(state, withWidth, parent, index);
    },
    video(state, node) {
      state.write(`::video[${node.attrs.url}]`);
      state.closeBlock(node);
    },
    place(state, node) {
      const addr = node.attrs.addr as string;
      state.write(`::place[${node.attrs.name}]${addr ? `{${addr}}` : ""}`);
      state.closeBlock(node);
    },
    table(state, node) {
      // 첫 줄을 머리글로, 그 줄의 정렬을 열 정렬로 쓴다. 열 수는 가장 긴 줄에 맞춘다.
      const rows: string[][] = [];
      node.forEach((row) => {
        const cells: string[] = [];
        row.forEach((cell) => cells.push(cellText(cell)));
        rows.push(cells);
      });
      const cols = Math.max(1, ...rows.map((r) => r.length));
      const aligns: Align[] = [];
      node.firstChild?.forEach((cell) => aligns.push(cell.attrs.align as Align));
      const line = (cells: string[]) =>
        `| ${Array.from({ length: cols }, (_, i) => cells[i] ?? "").join(" | ")} |`;
      const rule = Array.from({ length: cols }, (_, i) => {
        const a = aligns[i] ?? null;
        return a === "center" ? ":---:" : a === "right" ? "---:" : a === "left" ? ":---" : "---";
      });
      const out = [line(rows[0] ?? []), `| ${rule.join(" | ")} |`, ...rows.slice(1).map(line)];
      state.write(out.join("\n"));
      state.closeBlock(node);
    },
    footnote_ref(state, node) {
      state.write(`[^${node.attrs.label}]`);
    },
    footnote_def(state, node) {
      state.write(`[^${node.attrs.label}]: `);
      state.renderInline(node);
      state.closeBlock(node);
    },
    box(state, node) {
      state.write(`:::${node.attrs.kind}`);
      state.ensureNewLine();
      state.renderContent(node);
      // 마지막 문단 뒤 빈 줄 없이 바로 닫는다(사람이 쓰는 모양).
      state.flushClose(1);
      state.write(":::");
      state.closeBlock(node);
    },
  },
  {
    ...defaultMarkdownSerializer.marks,
    strike: { open: "~~", close: "~~", mixable: true, expelEnclosingWhitespace: true },
    color: spanSyntax("color"),
    highlight: spanSyntax("mark"),
    size: spanSyntax("size"),
    font: spanSyntax("font"),
  },
);

/** 표 칸 하나의 Markdown. 칸 안에서는 줄을 바꿀 수 없어 줄바꿈은 빈칸으로, `|` 는 `\|` 로 쓴다. */
function cellText(cell: Node): string {
  const PIPE = "";
  const parts: Node[] = [];
  cell.forEach((child) => {
    if (child.isText) parts.push(schema.text((child.text ?? "").replace(/\|/g, PIPE), child.marks));
    else if (child.type === schema.nodes.hard_break) parts.push(schema.text(" ", child.marks));
    else parts.push(child);
  });
  const doc = schema.node("doc", null, [schema.node("paragraph", null, parts)]);
  return serializer
    .serialize(doc)
    .trim()
    .replace(/\n/g, " ")
    .split(PIPE)
    .join("\\|");
}

// 비교용 렌더러. 서버(pulldown-cmark)처럼 표·취소선까지 그린다.
// 확장 문법(상자·글자 꾸밈)은 서버와 같은 규칙으로 읽어야 비교가 뜻이 있다.
const compareMd = MarkdownIt("default", { html: true, linkify: false }).use(maruDirectives);

export function parse(md: string): Node {
  return parser.parse(md);
}

export function serialize(doc: Node): string {
  return serializer.serialize(doc, { tightLists: true });
}

function normalize(html: string): string {
  return (
    html
      .replace(/>\s+</g, "><")
      .replace(/\s+/g, " ")
      // `**_가_**` 는 `***가***` 로 다시 써진다. 겹친 순서만 다르고 보이는 모양은 같다.
      .replace(/<strong><em>/g, "<em><strong>")
      .replace(/<\/em><\/strong>/g, "</strong></em>")
      .trim()
  );
}

export type Loaded = { ok: true; doc: Node } | { ok: false; reason: string };

/** 원문을 서식 편집기로 열어도 되는지 본다. 다시 쓴 Markdown 이 같은 HTML 로 그려져야 한다. */
export function load(md: string): Loaded {
  // 비교 렌더러는 각주를 모른다. 둘째 문단이 있는 각주는 비교로 못 잡으니 먼저 거른다.
  if (/^\[\^[^\]\s]+\]:.*\n(?:[ \t]*\n)+(?: {4}|\t)/m.test(md)) return { ok: false, reason: "여러 문단 각주" };
  let doc: Node;
  try {
    doc = parse(md);
  } catch {
    return { ok: false, reason: "HTML" };
  }
  const again = serialize(doc);
  if (normalize(compareMd.render(md)) !== normalize(compareMd.render(again))) {
    return { ok: false, reason: describeDifference(md) };
  }
  return { ok: true, doc };
}

function describeDifference(md: string): string {
  if (/^\s*\|.*\|\s*$/m.test(md)) return "서식 편집기가 다루지 못하는 표(칸 안의 목록·HTML 등)";
  if (/^\[\^[^\]\s]+\]:/m.test(md)) return "서식 편집기가 다루지 못하는 각주";
  return "서식 편집기가 아직 다루지 못하는 문법";
}
