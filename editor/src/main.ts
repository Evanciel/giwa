// Maru 글 에디터. 관리 화면의 `textarea[data-editor]` 를 서식 편집기로 바꾼다.
//
// - 저장되는 값은 언제나 textarea 의 Markdown 이다. 스크립트가 꺼져 있으면 textarea 가 그대로 동작한다.
// - 세 모드: 서식(ProseMirror), Markdown(원문 textarea), 미리보기(서버 렌더러 = 공개 화면과 같은 결과).
// - 서식 편집기가 원문을 바꾸지 않고는 열 수 없는 글(표·HTML 등)은 Markdown 으로 연다 (markdown.ts).

import { history, redo, undo } from "prosemirror-history";
import { Fragment, Node, Slice } from "prosemirror-model";
import { Command, EditorState, Selection } from "prosemirror-state";
import { EditorView } from "prosemirror-view";
import { setBlockType, toggleMark } from "prosemirror-commands";

import { CHAR_GROUPS, recentChars, rememberChar, splitChars } from "./chars";
import {
  blockActive,
  boxKind,
  buildPlugins,
  setBox,
  setSpan,
  spanValue,
  currentLink,
  gotoFootnote,
  insertFootnote,
  insertRule,
  insertTable,
  addRow,
  addColumn,
  deleteRow,
  deleteColumn,
  deleteTable,
  setColumnAlign,
  columnAlign,
  tableContext,
  linkHref,
  linkRange,
  markActive,
  removeLink,
  selectedImage,
  setImageAlt,
  setImageAttrs,
  setLink,
  taskActive,
  toggleChecked,
  toggleCodeBlock,
  toggleList,
  toggleQuote,
  toggleTask,
  wrappedIn,
} from "./commands";
import { IMAGE_WIDTHS, load, parse, schema, serialize } from "./markdown";
import { videoEmbed } from "./syntax";
import {
  altFromName,
  ATTACH_ACCEPT,
  ATTACH_EXTENSIONS,
  attachFile,
  attachLabel,
  findPlaceholder,
  imageFiles,
  insertUploads,
  placeholderKey,
  placeholderPlugin,
  sortFiles,
  uploadFile,
} from "./upload";
import type { Attached } from "./upload";
import { icon } from "./icons";
import { bindPublish } from "./publish";
import { fetchTemplates, saveTemplate } from "./templates";
import type { Template } from "./templates";

type Mode = "rich" | "md" | "preview";

interface Tool {
  label: string;
  name: string;
  /** 넣기 줄(아이콘 + 이름)인지 꾸미기 줄(아이콘만)인지 */
  row: "insert" | "format";
  /** icons.ts 의 이름. 없으면 label 글자를 그대로 보인다(B·I·S). */
  icon?: string;
  /** 단추 글자를 상태에 따라 바꾼다 (단락: 본문·제목·소제목). */
  labelOf?: (s: EditorState, mode: Mode) => string;
  /** 아이콘 아래 색 막대의 클래스 (글자색·형광펜: 지금 고른 색). null 이면 막대를 숨긴다. */
  swatchOf?: (s: EditorState) => string | null;
  /** 아이콘 옆 작은 글자 (글자 크기: 지금 크기 숫자). */
  badgeOf?: (s: EditorState) => string;
  keys?: string;
  run: (ed: Editor) => void;
  /** Markdown 모드에서 할 일. 없으면 그 모드에서 버튼을 끈다. */
  md?: (ed: Editor) => void;
  active?: (s: EditorState) => boolean;
  enabled?: (s: EditorState) => boolean;
}

const MOD = /Mac|iPhone|iPad/.test(navigator.platform) ? "⌘" : "Ctrl";

function el<K extends keyof HTMLElementTagNameMap>(
  tag: K,
  attrs: Record<string, string> = {},
  text = "",
): HTMLElementTagNameMap[K] {
  const e = document.createElement(tag);
  for (const [k, v] of Object.entries(attrs)) e.setAttribute(k, v);
  if (text) e.textContent = text;
  return e;
}

function cmd(c: Command) {
  return (ed: Editor) => {
    c(ed.view.state, ed.view.dispatch, ed.view);
    ed.view.focus();
  };
}

function can(c: Command) {
  return (s: EditorState) => c(s);
}

const n = schema.nodes;
const m = schema.marks;

/** 지금 문단의 종류 이름 (단락 단추 글자). */
function paraLabel(s: EditorState, mode: Mode): string {
  if (mode !== "rich") return "단락";
  if (blockActive(s, n.heading, { level: 2 })) return "제목";
  if (blockActive(s, n.heading, { level: 3 })) return "소제목";
  if (blockActive(s, n.code_block)) return "코드";
  return "본문";
}

// 두 줄: 위는 본문에 무언가를 넣는 단추(아이콘 + 이름), 아래는 글자·문단을 꾸미는 단추(아이콘만).
// 사진 크기·캡션·대체 텍스트, 표 편집처럼 고른 것이 있어야 쓰는 단추는 그것을 고르면 옆에 뜨는 말풍선에 있다.
const TOOLS: (Tool | "|")[] = [
  { row: "insert", icon: "photo", label: "사진", name: "사진 올리기 (끌어다 놓거나 붙여 넣어도 됩니다)", run: (ed) => ed.pickFiles(), md: (ed) => ed.pickFiles() },
  { row: "insert", icon: "file", label: "파일", name: "파일 첨부 (PDF·한글·오피스·zip 등, 10MB 이하)", run: (ed) => ed.pickAttachments(), md: (ed) => ed.pickAttachments() },
  { row: "insert", icon: "video", label: "동영상", name: "유튜브·비메오 동영상 넣기", run: (ed) => ed.openVideo(), md: (ed) => ed.openVideo() },
  { row: "insert", icon: "place", label: "장소", name: "장소(이름·주소·지도 링크) 넣기", run: (ed) => ed.openPlace(), md: (ed) => ed.openPlace() },
  { row: "insert", icon: "table", label: "표", name: "표 넣기 (칸 사이는 Tab 으로 이동)", run: cmd(insertTable()), md: (ed) => ed.mdInsert("\n\n| 제목 | 제목 |\n| --- | --- |\n| 내용 | 내용 |\n\n"), enabled: can(insertTable()) },
  { row: "insert", icon: "quote", label: "인용구", name: "인용", run: cmd(toggleQuote), md: (ed) => ed.mdLines("> "), active: (s) => wrappedIn(s, n.blockquote), enabled: can(toggleQuote) },
  { row: "insert", icon: "rule", label: "구분선", name: "구분선", run: cmd(insertRule), md: (ed) => ed.mdInsert("\n\n---\n\n") },
  { row: "insert", icon: "box", label: "상자", name: "강조 상자 (알림·도움말·주의·경고)", run: (ed) => ed.openBoxMenu("callout"), md: (ed) => ed.openBoxMenu("callout"), active: (s) => !!boxKind(s, "callout") },
  { row: "insert", icon: "template", label: "틀", name: "글 틀 불러오기·저장 (여행기·리뷰·공지·레시피 등)", run: (ed) => void ed.openTemplates(), md: (ed) => void ed.openTemplates() },
  { row: "insert", icon: "emoji", label: "특수문자", name: "이모지·특수문자", run: (ed) => ed.toggleChars(), md: (ed) => ed.toggleChars() },

  { row: "format", icon: "caret", label: "본문", labelOf: paraLabel, name: "문단 종류 (본문·제목·소제목)", run: (ed) => ed.openParaMenu(), md: (ed) => ed.openParaMenu() },
  { row: "format", icon: "caret", label: "기본 서체", labelOf: fontLabel, name: "서체", run: (ed) => ed.openSpanMenu("font"), md: (ed) => ed.openSpanMenu("font") },
  "|",
  { row: "format", label: "B", name: "굵게", keys: `${MOD}+B`, run: cmd(toggleMark(m.strong)), md: (ed) => ed.mdWrap("**", "**", "굵은 글"), active: (s) => markActive(s, m.strong), enabled: can(toggleMark(m.strong)) },
  { row: "format", label: "I", name: "기울임", keys: `${MOD}+I`, run: cmd(toggleMark(m.em)), md: (ed) => ed.mdWrap("*", "*", "기울인 글"), active: (s) => markActive(s, m.em), enabled: can(toggleMark(m.em)) },
  { row: "format", label: "S", name: "취소선", keys: `${MOD}+Shift+X`, run: cmd(toggleMark(m.strike)), md: (ed) => ed.mdWrap("~~", "~~", "지운 글"), active: (s) => markActive(s, m.strike), enabled: can(toggleMark(m.strike)) },
  { row: "format", icon: "color", label: "글자색", name: "글자 색", run: (ed) => ed.openSpanMenu("color"), md: (ed) => ed.openSpanMenu("color"), active: (s) => !!spanValue(s, m.color), swatchOf: (s) => spanClass(s, "color") },
  { row: "format", icon: "highlight", label: "형광펜", name: "형광펜", run: (ed) => ed.openSpanMenu("mark"), md: (ed) => ed.openSpanMenu("mark"), active: (s) => !!spanValue(s, m.highlight), swatchOf: (s) => spanClass(s, "mark") },
  { row: "format", icon: "size", label: "크기", name: "글자 크기", run: (ed) => ed.openSpanMenu("size"), md: (ed) => ed.openSpanMenu("size"), active: (s) => !!spanValue(s, m.size), badgeOf: (s) => sizeBadge(spanValue(s, m.size)) },
  "|",
  { row: "format", icon: "link", label: "링크", name: "링크", keys: `${MOD}+K`, run: (ed) => ed.openLink(), md: (ed) => ed.mdLink(), active: (s) => markActive(s, m.link) },
  { row: "format", icon: "code", label: "코드", name: "코드", keys: `${MOD}+E`, run: cmd(toggleMark(m.code)), md: (ed) => ed.mdWrap("`", "`", "코드"), active: (s) => markActive(s, m.code), enabled: can(toggleMark(m.code)) },
  "|",
  { row: "format", icon: "align", label: "정렬", name: "문단 정렬", run: (ed) => ed.openBoxMenu("align"), md: (ed) => ed.openBoxMenu("align"), active: (s) => !!boxKind(s, "align") },
  { row: "format", icon: "bullet", label: "목록", name: "글머리 목록", run: cmd(toggleList(n.bullet_list)), md: (ed) => ed.mdLines("- "), active: (s) => wrappedIn(s, n.bullet_list), enabled: can(toggleList(n.bullet_list)) },
  { row: "format", icon: "ordered", label: "번호", name: "번호 목록", run: cmd(toggleList(n.ordered_list)), md: (ed) => ed.mdLines((i) => `${i + 1}. `), active: (s) => wrappedIn(s, n.ordered_list), enabled: can(toggleList(n.ordered_list)) },
  { row: "format", icon: "task", label: "할 일", name: "할 일 목록", keys: `${MOD}+Shift+9`, run: cmd(toggleTask), md: (ed) => ed.mdLines("- [ ] "), active: taskActive, enabled: can(toggleTask) },
  { row: "format", icon: "more", label: "더보기", name: "더보기 (코드 블록·각주·갤러리)", run: (ed) => ed.openMore(), md: (ed) => ed.openMore() },
  "|",
  { row: "format", icon: "undo", label: "되돌리기", name: "실행 취소", keys: `${MOD}+Z`, run: cmd(undo), md: (ed) => ed.mdHistory("undo"), enabled: can(undo) },
  { row: "format", icon: "redo", label: "다시", name: "다시 실행", keys: `${MOD}+Shift+Z`, run: cmd(redo), md: (ed) => ed.mdHistory("redo"), enabled: can(redo) },
];

/** 링크 주소를 고친다. 도메인만 쓰면 https 를 붙이고, 스크립트 주소는 거부한다. */
export function normalizeHref(raw: string): string | null {
  const v = raw.trim();
  if (!v) return "";
  if (/^(https?:|mailto:)/i.test(v) || v.startsWith("/") || v.startsWith("#")) return v;
  if (/^[a-z][a-z0-9+.-]*:/i.test(v)) return null;
  return `https://${v}`;
}

/** 메뉴 단추 하나. `face` 가 있으면 그것을 보이고 `label` 은 읽어 주는 이름·툴팁이 된다. */
interface MenuItem {
  label: string;
  face?: HTMLElement;
  cls?: string;
  current?: boolean;
  /** 남는 메뉴에서 고를 때마다 다시 묻는 눌림 상태 */
  isCurrent?: () => boolean;
  run: () => void;
}

/** 꾸밈 메뉴: 문법 이름 → (값, 보이는 이름). `none` 은 맨 앞의 「꾸밈 없음」 단추 이름.
 *  크기의 small·large·xlarge 는 예전 글에만 남아 있어 메뉴에 두지 않는다 (ADR-0046). */
type SpanKind = "color" | "mark" | "size" | "font";
const SIZE_STEPS = ["11", "13", "15", "16", "19", "24", "28", "30", "34", "38"];
const SPAN_MENU: Record<SpanKind, { title: string; mark: "color" | "highlight" | "size" | "font"; prefix: string; none: string; items: [string, string][] }> = {
  color: {
    title: "글자 색",
    mark: "color",
    prefix: "tc",
    none: "기본",
    items: [["red", "빨강"], ["orange", "주황"], ["ochre", "황토"], ["brown", "갈색"], ["green", "초록"], ["teal", "청록"], ["blue", "파랑"], ["navy", "남색"], ["purple", "보라"], ["wine", "자주"], ["pink", "분홍"], ["gray", "회색"]],
  },
  mark: {
    title: "형광펜",
    mark: "highlight",
    prefix: "hl",
    none: "없음",
    items: [["yellow", "노랑"], ["orange", "주황"], ["green", "연두"], ["mint", "민트"], ["blue", "하늘"], ["purple", "연보라"], ["pink", "분홍"], ["gray", "회색"]],
  },
  size: { title: "글자 크기", mark: "size", prefix: "fs", none: "본문", items: SIZE_STEPS.map((v) => [v, v]) },
  font: { title: "서체", mark: "font", prefix: "ff", none: "기본 서체", items: [["serif", "명조"], ["pen", "손글씨"], ["brush", "붓글씨"], ["mono", "고정폭"]] },
};

/** 지금 글자의 꾸밈 클래스 (tc-red 같은 것). 툴바 색 막대에 쓴다. */
function spanClass(s: EditorState, kind: "color" | "mark"): string | null {
  const spec = SPAN_MENU[kind];
  const v = spanValue(s, m[spec.mark]);
  return v ? `${spec.prefix}-${v}` : null;
}

/** 서체 단추 글자: 지금 글자의 서체 이름. */
function fontLabel(s: EditorState, mode: Mode): string {
  if (mode !== "rich") return "서체";
  const v = spanValue(s, m.font);
  return SPAN_MENU.font.items.find(([value]) => value === v)?.[1] ?? SPAN_MENU.font.none;
}

/** 툴바 크기 단추 옆 숫자. 예전 값(small 등)은 숫자가 없어 비운다. */
function sizeBadge(v: string | null): string {
  return v && SIZE_STEPS.includes(v) ? v : "";
}

const BOX_MENU: Record<"callout" | "align", { title: string; none: string; items: [string, string][] }> = {
  callout: { title: "강조 상자", none: "상자 없애기", items: [["info", "알림"], ["tip", "도움말"], ["warn", "주의"], ["danger", "경고"]] },
  align: { title: "문단 정렬", none: "왼쪽(기본)", items: [["center", "가운데"], ["right", "오른쪽"]] },
};

function mdImage(alt: string, url: string): string {
  return `![${alt.replace(/([\[\]\\])/g, "\\$1")}](${url})`;
}

class Editor {
  view: EditorView;
  mode: Mode = "rich";
  private root: HTMLElement;
  private pm: HTMLElement;
  private preview: HTMLElement;
  private notice: HTMLElement;
  private dropLine: HTMLElement;
  private status: HTMLElement;
  private count: HTMLElement;
  private saved: HTMLElement;
  private pop: HTMLFormElement;
  /** 도구 막대와 입력 칸이 놓인 곳 (화면이 마련한 자리면 편집 칸 밖에 있다). */
  private head!: HTMLElement;
  /** 지금 열린 메뉴가 고른 뒤에도 남는 꾸밈 메뉴인지. 바깥을 누르면 닫는다. */
  private popSticky = false;
  /** 열어 둔 메뉴의 눌림 표시를 지금 선택에 맞춘다 (편집 칸에서 다른 글자를 고를 때). */
  private popSync: (() => void) | null = null;
  private openedByPointer = false;
  private file: HTMLInputElement;
  private attach: HTMLInputElement;
  private buttons: { tool: Tool; button: HTMLButtonElement }[] = [];
  private modeButtons = new Map<Mode, HTMLButtonElement>();
  private tools: HTMLElement;
  private chars: HTMLElement;
  private bubble: HTMLElement;
  private spellButton: HTMLButtonElement;
  /** 미리보기를 끄면 돌아갈 편집 방식 */
  private lastEdit: Mode = "rich";
  private spell = false;
  private syncTimer = 0;
  private draftTimer = 0;
  private submitting = false;
  /** 올리는 중인 사진·파일 묶음 수. 0 이 아니면 저장을 막는다 (빠진 채로 저장되지 않게). */
  private uploading = 0;
  private initialMd: string;
  private initialTitle: string;
  private draftKey: string;

  constructor(
    private textarea: HTMLTextAreaElement,
    private form: HTMLFormElement,
    private csrf: string,
    private title: HTMLInputElement | null,
  ) {
    this.initialMd = textarea.value;
    this.initialTitle = title?.value ?? "";
    this.draftKey = `maru:draft:${form.getAttribute("action") ?? location.pathname}`;

    this.root = el("div", { class: "me" });
    // 도구 막대: 넣기 줄과 꾸미기 줄
    this.tools = el("div", { class: "me-tools", role: "toolbar", "aria-label": "글쓰기 도구" });
    const rows = { insert: el("div", { class: "me-row me-row-insert" }), format: el("div", { class: "me-row me-row-format" }) };
    let lastRow: "insert" | "format" = "insert";
    for (const t of TOOLS) {
      if (t === "|") {
        rows[lastRow].append(el("span", { class: "me-sep", "aria-hidden": "true" }));
        continue;
      }
      lastRow = t.row;
      const title = t.keys ? `${t.name} (${t.keys})` : t.name;
      const b = el("button", { type: "button", class: `me-tool me-tool-${t.row}`, title, "aria-label": t.name });
      const pic = t.icon ? icon(t.icon) : null;
      if (t.labelOf) {
        // 단락: 글자 + ▾
        b.classList.add("me-tool-para");
        b.append(el("span", { class: "me-tool-text" }, t.label));
        if (pic) b.append(pic);
      } else if (pic) {
        b.append(pic);
        if (t.row === "insert") b.append(el("span", { class: "me-tool-text" }, t.label));
      } else {
        b.textContent = t.label;
      }
      if (t.swatchOf) b.append(el("span", { class: "me-tool-swatch", hidden: "", "aria-hidden": "true" }));
      if (t.badgeOf) b.append(el("span", { class: "me-tool-badge", "aria-hidden": "true" }));
      if (t.active) b.setAttribute("aria-pressed", "false");
      // 버튼을 눌러도 편집 영역의 선택이 풀리지 않게 한다.
      b.addEventListener("mousedown", (e) => e.preventDefault());
      b.addEventListener("click", (e) => {
        // 마우스로 연 메뉴는 편집 칸에 포커스를 남겨 고른 글자가 계속 보이게 한다 (detail 0 = 키보드).
        this.openedByPointer = e.detail > 0;
        if (this.mode === "rich") t.run(this);
        else t.md?.(this);
      });
      rows[t.row].append(b);
      this.buttons.push({ tool: t, button: b });
    }
    this.tools.append(rows.insert, rows.format);

    // 편집 방식(서식·Markdown)과 미리보기, 맞춤법은 아래 막대에 둔다.
    const modes = el("div", { class: "me-modes", role: "group", "aria-label": "편집 방식" });
    for (const [mode, label] of [["rich", "서식"], ["md", "Markdown"]] as [Mode, string][]) {
      const b = el("button", { type: "button", class: "me-mode", "aria-pressed": "false" }, label);
      b.addEventListener("click", () => this.setMode(mode));
      modes.append(b);
      this.modeButtons.set(mode, b);
    }
    const previewButton = el("button", { type: "button", class: "me-mode me-mode-icon", "aria-pressed": "false", title: "공개 화면과 같은 모양으로 미리 보기" });
    previewButton.append(icon("eye") ?? "", el("span", {}, "미리보기"));
    previewButton.addEventListener("click", () => this.setMode(this.mode === "preview" ? this.lastEdit : "preview"));
    this.modeButtons.set("preview", previewButton);
    this.spellButton = el("button", { type: "button", class: "me-mode me-mode-icon", "aria-pressed": "false", title: "브라우저 맞춤법 검사 표시" });
    this.spellButton.append(icon("spell") ?? "", el("span", {}, "맞춤법"));
    this.spellButton.addEventListener("click", () => this.setSpell(!this.spell));

    this.chars = el("div", { class: "me-chars", hidden: "", role: "dialog", "aria-label": "이모지·특수문자" });
    this.bubble = el("div", { class: "me-bubble", hidden: "", role: "toolbar", "aria-label": "링크" });
    this.pop = el("form", { class: "me-pop", hidden: "" });
    this.notice = el("div", { class: "me-notice", role: "status", hidden: "" });
    this.pm = el("div", { class: "me-rich" });
    this.preview = el("div", { class: "me-preview prose", hidden: "", tabindex: "0", "aria-label": "미리보기" });
    this.file = el("input", { type: "file", accept: "image/jpeg,image/png,image/gif,image/webp", multiple: "", hidden: "" });
    this.attach = el("input", { type: "file", accept: ATTACH_ACCEPT, multiple: "", hidden: "" });
    this.status = el("span", { class: "me-status", role: "status", "aria-live": "polite" });
    this.count = el("span", { class: "me-count" });
    this.saved = el("span", { class: "me-saved" });

    // 화면이 자리를 마련해 두었으면(data-editor-*) 거기에, 없으면 편집 칸 위·아래에 붙인다.
    const slot = (name: string) => {
      const s = form.querySelector<HTMLElement>(`[data-editor-${name}]`);
      s?.classList.add("me");
      return s;
    };
    const head = slot("toolbar") ?? el("div", { class: "me-head" });
    const foot = slot("foot") ?? el("div", { class: "me-foot" });
    const savedSlot = form.querySelector<HTMLElement>("[data-editor-saved]");
    head.append(this.tools, this.pop, this.chars);
    this.head = head;
    const view = el("div", { class: "me-view" });
    view.append(modes, previewButton, this.spellButton);
    foot.append(this.status, view, this.count);
    if (savedSlot) savedSlot.append(this.saved);
    else foot.append(this.saved);

    textarea.before(this.root);
    this.dropLine = el("div", { class: "me-dropline", hidden: "", "aria-hidden": "true" });
    const dropTip = el("div", { class: "me-droptip", "aria-hidden": "true" }, "놓으면 사진은 본문에, 문서(PDF·한글·오피스 등)는 첨부 링크로 들어갑니다");
    if (!head.isConnected) this.root.append(head);
    this.root.append(this.notice, this.pm, textarea, this.preview, this.file, this.attach, dropTip);
    if (!foot.isConnected) this.root.append(foot);
    this.pm.append(this.bubble, this.dropLine);
    // 말풍선의 빈 곳을 눌러도 편집 칸이 포커스를 잃지 않게 한다 (잃으면 이어서 친 글자가 사라진다).
    this.bubble.addEventListener("mousedown", (e) => {
      if (!(e.target instanceof Element && e.target.closest("a"))) e.preventDefault();
    });
    // 사진이 늦게 뜨거나 창 크기가 바뀌면 아래 내용이 밀리므로 말풍선 자리를 다시 잡는다.
    this.pm.addEventListener("load", () => this.updateBubble(), true);
    window.addEventListener("resize", () => this.updateBubble());
    textarea.classList.add("me-source");

    const label = document.querySelector<HTMLLabelElement>(`label[for="${textarea.id}"]`);
    if (label) {
      label.id ||= `${textarea.id}-label`;
      label.querySelector(".hint")?.remove();
    }

    const loaded = load(textarea.value);
    this.view = new EditorView(this.pm, {
      state: this.createState(loaded.ok ? loaded.doc : parse("")),
      dispatchTransaction: (tr) => {
        const next = this.view.state.apply(tr);
        this.view.updateState(next);
        if (tr.docChanged) {
          this.scheduleSync();
          this.scheduleDraft();
        }
        this.refresh();
      },
      attributes: (state) => ({
        class: state.doc.childCount === 1 && state.doc.firstChild?.isTextblock && state.doc.firstChild.content.size === 0 ? "me-doc me-empty" : "me-doc",
        role: "textbox",
        "aria-multiline": "true",
        ...(label ? { "aria-labelledby": label.id } : { "aria-label": "본문" }),
        "data-placeholder": "여기에 글을 쓰세요. 이미지는 끌어다 놓거나 붙여 넣으면 올라갑니다.",
        spellcheck: String(this.spell),
      }),
      nodeViews: {
        // 그림은 캡션(제목)을 아래에 보여 준다. 캡션은 편집 내용이 아니라 표시만 한다(캡션 단추로 고친다).
        image: (node) => {
          const wrap = el("span", { class: "me-img" });
          const img = el("img", {});
          const cap = el("span", { class: "me-cap", contenteditable: "false" });
          const paint = (n2: typeof node) => {
            const { src, alt, title, width } = n2.attrs;
            img.setAttribute("src", src);
            img.setAttribute("alt", alt ?? "");
            wrap.className = width ? `me-img img-w${width}` : "me-img";
            cap.textContent = title ?? "";
            cap.hidden = !title;
          };
          paint(node);
          wrap.append(img, cap);
          return {
            dom: wrap,
            update: (next) => {
              if (next.type !== node.type) return false;
              paint(next);
              return true;
            },
          };
        },
        // 할 일 항목은 누를 수 있는 체크 상자를 앞에 붙인다. 상자는 편집 내용이 아니다.
        list_item: (node, view, getPos) => {
          const li = document.createElement("li");
          if (node.attrs.checked === null) return { dom: li, contentDOM: li };
          li.className = "me-task";
          li.dataset.checked = String(node.attrs.checked);
          const box = el("input", { type: "checkbox", contenteditable: "false", "aria-label": "완료 표시" });
          box.checked = node.attrs.checked;
          box.addEventListener("mousedown", (e) => e.preventDefault());
          // 클릭의 기본 동작(체크 표시 바꾸기)은 그대로 두고 문서에도 같은 값을 적는다.
          // preventDefault 하면 브라우저가 클릭 뒤에 표시를 되돌려 문서와 화면이 어긋난다.
          box.addEventListener("click", () => {
            const pos = getPos();
            if (pos !== undefined) toggleChecked(pos)(view.state, view.dispatch);
          });
          const body = el("div", { class: "me-task-body" });
          li.append(box, body);
          return {
            dom: li,
            contentDOM: body,
            update: (next) => {
              if (next.type !== node.type || next.attrs.checked === null) return false;
              box.checked = next.attrs.checked;
              li.dataset.checked = String(next.attrs.checked);
              return true;
            },
            stopEvent: (e) => e.target === box,
            ignoreMutation: (m) => m.target === box || m.target === li,
          };
        },
      },
      handleClickOn: (view, _pos, node) => {
        if (node.type !== n.footnote_ref) return false;
        return gotoFootnote(node.attrs.label)(view.state, view.dispatch);
      },
      handlePaste: (view, event) => {
        if (this.takeFiles(event.clipboardData?.files)) return true;
        // 주소 하나만 붙여 넣으면: 고른 글자가 있으면 그 글자에 링크를, 없으면 주소를 링크로 넣는다.
        const text = event.clipboardData?.getData("text/plain") ?? "";
        const href = linkHref(text);
        if (!href || view.state.selection.$from.parent.type.spec.code) return false;
        const { empty, from } = view.state.selection;
        if (!empty) {
          setLink(href)(view.state, view.dispatch);
        } else {
          view.dispatch(view.state.tr.insert(from, schema.text(text.trim(), [m.link.create({ href })])).removeStoredMark(m.link));
        }
        return true;
      },
      handleDrop: (view, event, _slice, moved) => {
        if (moved) return false;
        const pos = view.posAtCoords({ left: event.clientX, top: event.clientY })?.pos;
        // 파일은 무엇이든 여기서 받는다. 놓아 두면 브라우저가 그 파일을 탭에 열어 쓰던 화면을 떠난다.
        if (!this.takeFiles(event.dataTransfer?.files, pos)) return false;
        event.preventDefault();
        return true;
      },
      // 메모장 등에서 붙여 넣은 글은 Markdown 으로 읽는다. Shift 를 누르고 붙이면 글자 그대로.
      clipboardTextParser: (text, $context, plain) => {
        if (!plain && !$context.parent.type.spec.code) {
          try {
            const doc = parse(text);
            return Slice.maxOpen(doc.content);
          } catch {
            // HTML 이 섞인 글은 아래처럼 문단으로
          }
        }
        if ($context.parent.type.spec.code) return new Slice(Fragment.from(schema.text(text)), 0, 0);
        const paras = text.split(/\r?\n/).map((line) => n.paragraph.create(null, line ? schema.text(line) : null));
        return Slice.maxOpen(Fragment.from(paras));
      },
    });

    this.bindSource();
    this.bindFile();
    this.bindAttach();
    this.bindDropHint();
    this.bindForm();
    this.bindPopover();
    this.bindChars();
    try {
      this.setSpell(localStorage.getItem("maru:editor-spell") === "on");
    } catch {
      this.setSpell(false);
    }

    if (loaded.ok) {
      this.setMode(localStorage.getItem("maru:editor-mode") === "md" ? "md" : "rich", false);
    } else {
      this.setMode("md", false);
      this.showNotice(`이 글에는 Markdown 으로만 고칠 수 있는 부분(${loaded.reason})이 있어 Markdown 으로 엽니다.`);
    }
    this.offerDraft();
    this.refresh();
  }

  private createState(doc: Node): EditorState {
    return EditorState.create({
      doc,
      plugins: [...buildPlugins(() => this.openLink()), history(), placeholderPlugin],
    });
  }

  /** 현재 Markdown. 서식 모드면 문서에서 다시 쓴다. */
  markdown(): string {
    return this.mode === "rich" ? serialize(this.view.state.doc) : this.textarea.value;
  }

  private sync() {
    window.clearTimeout(this.syncTimer);
    if (this.mode === "rich") this.textarea.value = serialize(this.view.state.doc);
  }

  private scheduleSync() {
    window.clearTimeout(this.syncTimer);
    this.syncTimer = window.setTimeout(() => this.sync(), 150);
  }

  setMode(mode: Mode, focus = true) {
    if (mode === this.mode && this.root.dataset.mode) return;
    if (mode === "rich" && this.mode !== "rich") {
      const loaded = load(this.textarea.value);
      if (!loaded.ok) {
        this.showNotice(`이 글에는 Markdown 으로만 고칠 수 있는 부분(${loaded.reason})이 있어 서식 편집기로 바꾸지 않습니다. 바꾸면 원문이 달라집니다.`);
        return;
      }
      // 새 상태는 커서가 글 맨 앞이다. Markdown 에서 이어 쓰던 흐름대로 글 끝에 둔다.
      const fresh = this.createState(loaded.doc);
      this.view.updateState(fresh.apply(fresh.tr.setSelection(Selection.atEnd(fresh.doc))));
      this.hideNotice();
    }
    // 처음 부를 때는 원문이 textarea 에 있다. 빈 문서(열 수 없는 글)나 정규화된 문서로 덮어쓰지 않는다.
    if (this.mode === "rich" && this.root.dataset.mode) this.sync();
    this.mode = mode;
    this.root.dataset.mode = mode;
    if (mode !== "preview") {
      this.lastEdit = mode;
      try {
        localStorage.setItem("maru:editor-mode", mode);
      } catch {
        // 저장소를 못 쓰면 다음에도 기본값
      }
    }
    for (const [k, b] of this.modeButtons) b.setAttribute("aria-pressed", String(k === mode || (k === this.lastEdit && mode === "preview")));
    this.modeButtons.get("preview")?.setAttribute("aria-pressed", String(mode === "preview"));
    this.pm.hidden = mode !== "rich";
    this.textarea.hidden = mode !== "md";
    this.preview.hidden = mode !== "preview";
    this.tools.hidden = mode === "preview";
    this.closePop();
    this.chars.hidden = true;
    this.chars.replaceChildren();
    if (mode === "preview") void this.renderPreview();
    if (focus) {
      if (mode === "rich") this.view.focus();
      if (mode === "md") this.textarea.focus();
    }
    this.refresh();
  }

  private async renderPreview() {
    const md = this.markdown();
    this.preview.textContent = "그리는 중…";
    try {
      const res = await fetch("/admin/preview", {
        method: "POST",
        credentials: "same-origin",
        headers: { "Content-Type": "application/json", "X-CSRF-Token": this.csrf },
        body: JSON.stringify({ body_md: md }),
      });
      const body = await res.json();
      if (!res.ok || !body.ok) throw new Error(body?.error?.message || `미리보기를 그리지 못했습니다 (${res.status}).`);
      // 서버가 공개 화면과 같은 정화(ammonia)를 거친 HTML 이다.
      this.preview.innerHTML = body.data.html || "<p class=\"me-muted\">본문이 비어 있습니다.</p>";
    } catch (e) {
      this.preview.textContent = (e as Error).message;
    }
  }

  /** 툴바 상태와 글자 수. */
  refresh() {
    const s = this.view.state;
    const rich = this.mode === "rich";
    for (const { tool, button } of this.buttons) {
      button.disabled = rich ? !!tool.enabled && !tool.enabled(s) : !tool.md;
      if (tool.active) button.setAttribute("aria-pressed", String(rich && tool.active(s)));
      if (tool.labelOf) {
        const text = button.querySelector(".me-tool-text");
        const label = tool.labelOf(s, this.mode);
        if (text && text.textContent !== label) text.textContent = label;
      }
      const swatch = tool.swatchOf && button.querySelector<HTMLElement>(".me-tool-swatch");
      if (swatch) {
        const cls = rich ? tool.swatchOf!(s) : null;
        swatch.hidden = !cls;
        swatch.className = `me-tool-swatch ${cls ?? ""}`;
      }
      const badge = tool.badgeOf && button.querySelector(".me-tool-badge");
      if (badge) {
        const text = rich ? tool.badgeOf!(s) : "";
        if (badge.textContent !== text) badge.textContent = text;
      }
    }
    this.popSync?.();
    const text = rich ? s.doc.textContent : this.textarea.value;
    // 공백을 뺀 글자 수로 읽는 시간을 어림한다(한국어 묵독 약 분당 500자).
    const solid = text.replace(/\s/g, "").length;
    const minutes = Math.max(1, Math.round(solid / 500));
    this.count.textContent = `${text.length.toLocaleString("ko-KR")}자 · 공백 빼고 ${solid.toLocaleString("ko-KR")}자 · 약 ${minutes}분`;
    this.updateBubble();
  }

  // ---- 말풍선: 링크 안에 커서가 있거나, 사진을 골랐거나, 표 안에 있으면 그것에 쓰는 단추를 옆에 띄운다 ----

  /** 같은 대상이면 다시 그리지 않는다 (누르는 사이에 단추가 바뀌지 않게). */
  private bubbleKey = "";

  private updateBubble() {
    const s = this.view.state;
    const on = this.mode === "rich" && (this.view.hasFocus() || this.bubble.contains(document.activeElement));
    const range = on ? linkRange(s) : null;
    const image = on && !range ? selectedImage(s) : null;
    const table = on && !range && !image ? tableContext(s) : null;
    if (!range && !image && !table) {
      this.bubble.hidden = true;
      this.bubbleKey = "";
      return;
    }
    const btn = (label: string, run: () => void, current = false) => {
      const b = el("button", { type: "button", class: "me-chip", "aria-pressed": String(current) }, label);
      b.addEventListener("mousedown", (e) => e.preventDefault());
      b.addEventListener("click", () => {
        run();
        this.refresh();
      });
      return b;
    };
    const box = this.pm.getBoundingClientRect();
    let anchor: { left: number; top: number; bottom: number };
    /** 사진은 크면 아래가 화면 밖이라 사진 위쪽 안에 띄운다. */
    let inside = false;
    let key: string;
    if (range) {
      key = `link:${range.from}:${range.href}`;
      if (key !== this.bubbleKey) {
        const a = el("a", { href: range.href, target: "_blank", rel: "noopener noreferrer", class: "me-bubble-url", title: range.href }, range.href);
        const del = btn("링크 빼기", () => {
          removeLink(this.view.state, this.view.dispatch);
          this.view.focus();
        });
        this.bubble.replaceChildren(a, btn("고치기", () => this.openLink()), del);
        this.bubble.setAttribute("aria-label", "링크");
      }
      anchor = this.view.coordsAtPos(range.from);
    } else if (image) {
      const { width, title, alt } = image.node.attrs as { width: number | null; title: string | null; alt: string | null };
      key = `img:${image.from}:${width}:${title}:${alt}:${boxKind(s, "gallery")}`;
      if (key !== this.bubbleKey) {
        const sizes = el("span", { class: "me-chips", role: "group", "aria-label": "사진 크기" });
        for (const w of [100, ...IMAGE_WIDTHS]) {
          sizes.append(btn(`${w}%`, () => setImageAttrs({ width: w === 100 ? null : w })(this.view.state, this.view.dispatch), (width ?? 100) === w));
        }
        this.bubble.replaceChildren(
          sizes,
          btn(title ? "캡션 고치기" : "캡션", () => this.openCaption()),
          btn(alt ? "대체 텍스트 ✓" : "대체 텍스트", () => this.openAlt()),
          btn("갤러리로 묶기", () => setBox("gallery", "gallery")(this.view.state, this.view.dispatch), !!boxKind(s, "gallery")),
        );
        this.bubble.setAttribute("aria-label", "사진");
      }
      const dom = this.view.nodeDOM(image.from);
      const r = dom instanceof HTMLElement ? dom.getBoundingClientRect() : null;
      anchor = r ? { left: r.left + 8, top: r.top, bottom: r.bottom } : this.view.coordsAtPos(image.from);
      inside = !!r;
    } else {
      const t = table as NonNullable<typeof table>;
      const align = columnAlign(s);
      key = `table:${t.pos}:${t.row}:${t.col}:${align}`;
      if (key !== this.bubbleKey) {
        const run = (c: Command) => () => {
          c(this.view.state, this.view.dispatch);
          this.view.focus();
        };
        const aligns = el("span", { class: "me-chips", role: "group", "aria-label": "이 칸 줄 정렬" });
        aligns.append(
          btn("왼쪽", run(setColumnAlign(null)), align === null || align === "left"),
          btn("가운데", run(setColumnAlign("center")), align === "center"),
          btn("오른쪽", run(setColumnAlign("right")), align === "right"),
        );
        this.bubble.replaceChildren(
          btn("+ 위 줄", run(addRow(false))),
          btn("+ 아래 줄", run(addRow(true))),
          btn("+ 왼쪽 칸", run(addColumn(false))),
          btn("+ 오른쪽 칸", run(addColumn(true))),
          btn("줄 지우기", run(deleteRow)),
          btn("칸 지우기", run(deleteColumn)),
          aligns,
          btn("표 지우기", run(deleteTable)),
        );
        this.bubble.setAttribute("aria-label", "표 편집");
      }
      // 표 아래에 띄운다. 칸을 옮겨 다녀도 말풍선이 따라 움직이지 않는다.
      const dom = this.view.nodeDOM(t.pos);
      const r = dom instanceof HTMLElement ? dom.getBoundingClientRect() : null;
      anchor = r ? { left: r.left, top: r.top, bottom: r.bottom } : this.view.coordsAtPos(t.pos + 1);
    }
    this.bubbleKey = key;
    this.bubble.hidden = false;
    const width = this.bubble.offsetWidth;
    this.bubble.style.left = `${Math.max(0, Math.min(anchor.left - box.left, box.width - width))}px`;
    this.bubble.style.top = `${(inside ? anchor.top + 8 : anchor.bottom + 6) - box.top + this.pm.scrollTop}px`;
  }

  // ---- 이모지·특수문자 ----

  private charTarget: { from: number; to: number } | null = null;

  private bindChars() {
    this.chars.addEventListener("keydown", (e) => {
      if (e.key === "Escape") {
        e.preventDefault();
        this.toggleChars(false);
      }
    });
    this.view.dom.addEventListener("blur", () => window.setTimeout(() => this.updateBubble(), 150));
  }

  toggleChars(open = this.chars.hidden) {
    if (!open) {
      this.chars.hidden = true;
      this.chars.replaceChildren();
      if (this.mode === "rich") this.view.focus();
      else this.textarea.focus();
      return;
    }
    this.closePop();
    this.charTarget =
      this.mode === "rich"
        ? { from: this.view.state.selection.from, to: this.view.state.selection.to }
        : { from: this.textarea.selectionStart, to: this.textarea.selectionEnd };
    const tabs = el("div", { class: "me-chars-tabs", role: "tablist" });
    const grid = el("div", { class: "me-chars-grid" });
    const groups: [string, string[]][] = [];
    const recent = recentChars();
    if (recent.length) groups.push(["최근", recent]);
    for (const [name, s] of CHAR_GROUPS) groups.push([name, splitChars(s)]);
    const show = (i: number) => {
      tabs.querySelectorAll("button").forEach((b, j) => b.setAttribute("aria-selected", String(i === j)));
      grid.replaceChildren(
        ...groups[i][1].map((c) => {
          const b = el("button", { type: "button", class: "me-char", "aria-label": c }, c);
          b.addEventListener("click", () => this.insertChar(c));
          return b;
        }),
      );
    };
    groups.forEach(([name], i) => {
      const t = el("button", { type: "button", role: "tab", class: "me-chars-tab" }, name);
      t.addEventListener("click", () => show(i));
      tabs.append(t);
    });
    const close = el("button", { type: "button", class: "me-link me-chars-close" }, "닫기");
    close.addEventListener("click", () => this.toggleChars(false));
    this.chars.replaceChildren(tabs, grid, close);
    show(0);
    this.chars.hidden = false;
    (grid.querySelector("button") as HTMLButtonElement | null)?.focus();
  }

  private insertChar(c: string) {
    rememberChar(c);
    const t = this.charTarget;
    if (this.mode === "rich") {
      const { state } = this.view;
      const from = t ? Math.min(t.from, state.doc.content.size) : state.selection.from;
      const to = t ? Math.min(t.to, state.doc.content.size) : state.selection.to;
      this.view.dispatch(state.tr.insertText(c, from, to));
      const pos = from + c.length;
      this.charTarget = { from: pos, to: pos };
    } else {
      const from = t?.from ?? this.textarea.selectionStart;
      const to = t?.to ?? this.textarea.selectionEnd;
      this.replaceSource(from, to, c, from + c.length, from + c.length);
      this.charTarget = { from: from + c.length, to: from + c.length };
    }
  }

  // ---- 맞춤법 ----

  private setSpell(on: boolean) {
    this.spell = on;
    this.spellButton.setAttribute("aria-pressed", String(on));
    this.textarea.spellcheck = on;
    this.view.setProps({});
    try {
      localStorage.setItem("maru:editor-spell", on ? "on" : "off");
    } catch {
      // 무시
    }
  }

  say(message: string, error = false) {
    this.status.textContent = message;
    this.status.classList.toggle("me-error", error);
  }

  private showNotice(text: string, actions: [string, () => void][] = []) {
    this.notice.replaceChildren(el("span", {}, text));
    for (const [label, fn] of actions) {
      const b = el("button", { type: "button", class: "me-link" }, label);
      b.addEventListener("click", fn);
      this.notice.append(b);
    }
    this.notice.hidden = false;
  }

  private hideNotice() {
    this.notice.hidden = true;
    this.notice.replaceChildren();
  }

  // ---- Markdown 모드 ----

  private bindSource() {
    this.textarea.addEventListener("input", () => {
      this.scheduleDraft();
      this.refresh();
    });
    const upload = async (files: File[]) => {
      for (const f of files) {
        this.say("이미지를 올리는 중입니다…");
        try {
          const up = await uploadFile(f, this.csrf);
          const ta = this.textarea;
          const text = mdImage(up.alt_text ?? altFromName(f.name), up.url);
          ta.setRangeText(text, ta.selectionStart, ta.selectionEnd, "end");
          ta.dispatchEvent(new Event("input"));
          this.say("이미지를 넣었습니다.");
        } catch (e) {
          this.say((e as Error).message, true);
        }
      }
    };
    this.uploadToSource = upload;
    this.textarea.addEventListener("paste", (e) => {
      if (this.takeFiles(e.clipboardData?.files)) e.preventDefault();
    });
    this.textarea.addEventListener("dragover", (e) => {
      if (e.dataTransfer?.types.includes("Files")) e.preventDefault();
    });
    this.textarea.addEventListener("drop", (e) => {
      if (this.takeFiles(e.dataTransfer?.files)) e.preventDefault();
    });
  }

  private uploadToSource: (files: File[]) => Promise<void> = async () => {};

  private bindFile() {
    this.file.addEventListener("change", () => {
      const files = imageFiles(this.file.files);
      this.file.value = "";
      if (!files.length) return;
      if (this.mode === "rich") {
        this.busy(insertUploads(this.view, files, this.csrf, (msg, err) => this.say(msg, err)));
      } else {
        this.busy(this.uploadToSource(files));
      }
    });
  }

  pickFiles() {
    this.file.click();
  }

  // ---- Markdown 모드 툴바: 선택한 글자에 문법을 넣는다 ----

  /** 브라우저의 실행 취소 기록에 남도록 `insertText` 로 바꾼다. 안 되면 직접 바꾼다. */
  private replaceSource(from: number, to: number, text: string, selFrom: number, selTo: number) {
    const ta = this.textarea;
    ta.focus();
    ta.setSelectionRange(from, to);
    if (!document.execCommand("insertText", false, text)) {
      ta.setRangeText(text, from, to, "end");
    }
    ta.setSelectionRange(selFrom, selTo);
    ta.dispatchEvent(new Event("input"));
  }

  mdWrap(before: string, after: string, placeholder: string) {
    const ta = this.textarea;
    const value = ta.value;
    let { selectionStart: s, selectionEnd: e } = ta;
    // 줄을 통째로 고르면 끝의 줄바꿈까지 잡힌다. 앞뒤 공백은 감싸지 않는다 (`**가 **` 는 굵게가 아니다).
    while (s < e && /\s/.test(value[s])) s++;
    while (e > s && /\s/.test(value[e - 1])) e--;
    const picked = value.slice(s, e);
    // 이미 감싸져 있으면 벗긴다.
    if (value.slice(s - before.length, s) === before && value.slice(e, e + after.length) === after) {
      this.replaceSource(s - before.length, e + after.length, picked, s - before.length, e - before.length);
      return;
    }
    const inner = picked || placeholder;
    const start = s + before.length;
    this.replaceSource(s, e, before + inner + after, start, start + inner.length);
  }

  /** 선택한 줄마다 앞에 붙인다. 모든 줄에 이미 있으면 뗀다. */
  mdLines(prefix: string | ((i: number) => string)) {
    const ta = this.textarea;
    const { selectionStart: s, selectionEnd: e, value } = ta;
    const from = value.lastIndexOf("\n", s - 1) + 1;
    const endNl = value.indexOf("\n", e === s ? e : e - 1);
    const to = endNl === -1 ? value.length : endNl;
    const lines = value.slice(from, to).split("\n");
    const pre = (i: number) => (typeof prefix === "string" ? prefix : prefix(i));
    const strip = /^(#{1,6} |> |[-*+] |\d+\. )/;
    const all = lines.every((l, i) => l.startsWith(pre(i)));
    // 빈 접두어는 「본문으로」: 제목 표시(#)만 뗀다.
    const out =
      prefix === ""
        ? lines.map((l) => l.replace(/^#{1,6} /, "")).join("\n")
        : lines.map((l, i) => (all ? l.slice(pre(i).length) : pre(i) + l.replace(strip, ""))).join("\n");
    this.replaceSource(from, to, out, from, from + out.length);
  }

  mdFence() {
    const ta = this.textarea;
    const { selectionStart: s, selectionEnd: e, value } = ta;
    const picked = value.slice(s, e) || "코드";
    const lead = s > 0 && value[s - 1] !== "\n" ? "\n\n" : "";
    const text = `${lead}\`\`\`\n${picked}\n\`\`\`\n`;
    const start = s + lead.length + 4;
    this.replaceSource(s, e, text, start, start + picked.length);
  }

  mdLink() {
    const ta = this.textarea;
    const { selectionStart: s, selectionEnd: e, value } = ta;
    const picked = value.slice(s, e) || "링크 글자";
    const text = `[${picked}](https://)`;
    const urlStart = s + picked.length + 3;
    this.replaceSource(s, e, text, urlStart, urlStart + 8);
  }

  /** 커서 자리에 `[^n]` 을 넣고 글 끝에 `[^n]: ` 칸을 만든 뒤 그리로 옮긴다. */
  mdFootnote() {
    const ta = this.textarea;
    const labels = [...ta.value.matchAll(/\[\^(\d+)\]/g)].map((x) => Number(x[1]));
    const label = String(Math.max(0, ...labels) + 1);
    const { selectionEnd: e } = ta;
    const ref = `[^${label}]`;
    this.replaceSource(e, e, ref, e + ref.length, e + ref.length);
    const v = ta.value.replace(/\s+$/, "");
    const def = `\n\n[^${label}]: `;
    this.replaceSource(v.length, ta.value.length, def, v.length + def.length, v.length + def.length);
  }

  mdInsert(text: string) {
    const { selectionStart: s, selectionEnd: e } = this.textarea;
    this.replaceSource(s, e, text, s + text.length, s + text.length);
  }

  mdHistory(which: "undo" | "redo") {
    this.textarea.focus();
    document.execCommand(which);
    this.refresh();
  }

  // ---- 링크·대체 텍스트 입력 칸 ----

  private popApply: (value: string) => string | null = () => null;

  private bindPopover() {
    this.pop.addEventListener("submit", (e) => {
      e.preventDefault();
      const input = this.pop.querySelector("input");
      const err = this.popApply(input?.value ?? "");
      if (err) {
        this.say(err, true);
        input?.focus();
        return;
      }
      this.closePop();
      this.focusEditor();
    });
    // Esc 는 입력 칸·메뉴 안에서도, 메뉴를 열어 둔 채 편집 칸에 있어도 닫는다.
    // 먼저 잡아야 편집기의 Esc(부모 고르기)가 선택을 바꾸지 않는다.
    document.addEventListener(
      "keydown",
      (e) => {
        if (e.key !== "Escape" || this.pop.hidden || !(e.target instanceof Element)) return;
        if (!this.head.contains(e.target) && !this.root.contains(e.target)) return;
        e.preventDefault();
        e.stopPropagation();
        this.closePop();
        this.focusEditor();
      },
      true,
    );
    // 열어 둔 꾸밈 메뉴는 편집 칸·도구 막대 바깥을 누르면 닫는다.
    document.addEventListener("pointerdown", (e) => {
      if (!this.popSticky || this.pop.hidden || !(e.target instanceof Element)) return;
      if (this.head.contains(e.target) || this.root.contains(e.target)) return;
      this.closePop();
    });
  }

  private openPop(label: string, value: string, placeholder: string, apply: (v: string) => string | null, remove?: () => void) {
    const id = "me-pop-input";
    const input = el("input", { id, type: "text", value, placeholder, autocomplete: "off" });
    input.value = value;
    const ok = el("button", { type: "submit" }, "적용");
    const close = el("button", { type: "button", class: "secondary" }, "닫기");
    close.addEventListener("click", () => {
      this.closePop();
      this.focusEditor();
    });
    this.pop.replaceChildren(el("label", { for: id }, label), input, ok);
    if (remove) {
      const del = el("button", { type: "button", class: "secondary" }, "지우기");
      del.addEventListener("click", () => {
        remove();
        this.closePop();
        this.view.focus();
      });
      this.pop.append(del);
    }
    this.pop.append(close);
    this.popApply = apply;
    this.popSticky = false;
    this.popSync = null;
    this.pop.hidden = false;
    input.focus();
    input.select();
  }

  private focusEditor() {
    if (this.mode === "rich") this.view.focus();
    else if (this.mode === "md") this.textarea.focus();
  }

  /** 서식 모드면 블록 노드를, Markdown 모드면 그 문법 줄을 앞뒤 빈 줄과 함께 넣는다. */
  private insertBlock(node: ReturnType<typeof schema.node>, md: string) {
    if (this.mode === "rich") {
      // 고른 것(사진·글자)을 지우지 않고 그 뒤에 넣는다.
      const tr = this.view.state.tr;
      if (!tr.selection.empty) tr.setSelection(Selection.near(tr.doc.resolve(tr.selection.to), 1));
      this.view.dispatch(tr.replaceSelectionWith(node).scrollIntoView());
    } else if (this.mode === "md") {
      this.mdInsert(`\n\n${md}\n\n`);
    }
  }

  async openTemplates() {
    let list: Template[];
    try {
      list = await fetchTemplates(this.csrf);
    } catch (e) {
      this.say((e as Error).message, true);
      return;
    }
    this.openMenu("글 틀", [
      ...list.map((t) => ({ label: t.builtin ? t.name : `${t.name} (저장)`, run: () => this.applyTemplate(t.body_md) })),
      // 메뉴가 닫힌 뒤에 이름 입력 칸을 연다.
      { label: "지금 글을 틀로 저장…", cls: "me-menu-save", run: () => void setTimeout(() => this.saveAsTemplate(), 0) },
    ]);
  }

  /** 빈 글이면 틀로 채우고, 쓰던 글이 있으면 커서 자리에 넣는다 (실행 취소로 되돌릴 수 있다). */
  private applyTemplate(md: string) {
    const empty = this.markdown().trim() === "";
    if (this.mode === "rich") {
      const r = load(md);
      if (!r.ok) {
        this.say("이 틀은 서식 모드로 열 수 없습니다. Markdown 모드에서 넣어 주세요.", true);
        return;
      }
      const tr = this.view.state.tr;
      if (empty) tr.replaceWith(0, tr.doc.content.size, r.doc.content);
      else tr.replaceSelection(new Slice(r.doc.content, 0, 0));
      this.view.dispatch(tr.scrollIntoView());
    } else if (this.mode === "md") {
      if (empty) {
        const len = this.textarea.value.length;
        this.replaceSource(0, len, md, 0, 0);
      } else {
        this.mdInsert(`\n\n${md}\n\n`);
      }
    }
    this.say("틀을 넣었습니다.");
  }

  private saveAsTemplate() {
    const body = this.markdown();
    if (!body.trim()) {
      this.say("본문이 비어 있어 틀로 저장할 수 없습니다.", true);
      return;
    }
    this.openPop("틀 이름 (같은 이름이면 덮어씁니다)", "", "예: 주간 회고", (raw) => {
      const name = raw.trim();
      if (!name) return "이름을 적어 주세요.";
      saveTemplate(name, body, this.csrf).then(
        (t) => this.say(`'${t.name}' 틀로 저장했습니다.`),
        (e: Error) => this.say(e.message, true),
      );
      return null;
    });
  }

  pickAttachments() {
    this.attach.click();
  }

  private bindAttach() {
    this.attach.addEventListener("change", () => {
      const files = Array.from(this.attach.files ?? []);
      this.attach.value = "";
      this.busy(this.attachAll(files));
    });
  }

  private busy(task: Promise<unknown>) {
    this.uploading++;
    void task.finally(() => this.uploading--);
  }

  /**
   * 파일을 끌고 들어오면 편집 영역을 점선으로 감싸고 무엇이 되는지 알린다.
   * 서식 모드에서는 놓을 자리에 세로 막대를 그린다. 안쪽 요소를 지날 때마다 enter·leave 가 오므로 깊이를 센다.
   */
  private bindDropHint() {
    let depth = 0;
    const hasFiles = (e: DragEvent) => !!e.dataTransfer?.types.includes("Files");
    const end = () => {
      depth = 0;
      this.root.classList.remove("me-dropping");
      this.dropLine.hidden = true;
    };
    for (const area of [this.pm, this.textarea]) {
      area.addEventListener("dragenter", (e) => {
        if (!hasFiles(e)) return;
        depth++;
        this.root.classList.add("me-dropping");
      });
      area.addEventListener("dragleave", (e) => {
        if (hasFiles(e) && --depth <= 0) end();
      });
      area.addEventListener("drop", end);
    }
    this.pm.addEventListener("dragover", (e) => {
      if (!hasFiles(e)) return;
      const items = Array.from(e.dataTransfer?.items ?? []);
      const photosOnly = items.length > 0 && items.every((i) => i.kind === "file" && i.type.startsWith("image/"));
      this.showDropLine(e.clientX, e.clientY, photosOnly);
    });
    // 편집 칸 밖(제목 칸, 도구 막대 등)에 놓으면 브라우저가 그 파일을 열어 쓰던 화면을 떠난다. 막고 알린다.
    const inArea = (t: EventTarget | null) => t instanceof Element && (this.pm.contains(t) || t === this.textarea);
    document.addEventListener("dragover", (e) => {
      if (!hasFiles(e) || inArea(e.target)) return;
      e.preventDefault();
      if (e.dataTransfer) e.dataTransfer.dropEffect = "none";
    });
    document.addEventListener("drop", (e) => {
      end();
      if (!hasFiles(e) || e.defaultPrevented) return;
      e.preventDefault();
      this.say("파일은 본문 편집 칸 안에 놓아 주세요.", true);
    });
    document.addEventListener("dragend", end);
  }

  /** 사진은 글자가 있는 문단에 놓으면 그 아래 새 줄로 들어가므로(insertUploads) 문단 아래 가로선을 그린다. */
  private showDropLine(x: number, y: number, photosOnly: boolean) {
    const hit = this.view.posAtCoords({ left: x, top: y });
    if (!hit) {
      this.dropLine.hidden = true;
      return;
    }
    const box = this.pm.getBoundingClientRect();
    const dx = this.pm.scrollLeft - box.left;
    const dy = this.pm.scrollTop - box.top;
    const s = this.dropLine.style;
    const $at = this.view.state.doc.resolve(hit.pos);
    const below = photosOnly && $at.depth > 0 && $at.parent.type === schema.nodes.paragraph && $at.parent.content.size > 0;
    const block = below ? this.view.nodeDOM($at.before()) : null;
    if (block instanceof HTMLElement) {
      const r = block.getBoundingClientRect();
      this.dropLine.classList.add("me-dropline-row");
      Object.assign(s, { left: `${r.left + dx}px`, top: `${r.bottom + dy + 4}px`, width: `${r.width}px`, height: "" });
    } else {
      const at = this.view.coordsAtPos(hit.pos);
      this.dropLine.classList.remove("me-dropline-row");
      Object.assign(s, { left: `${at.left + dx}px`, top: `${at.top + dy}px`, width: "", height: `${Math.max(at.bottom - at.top, 18)}px` });
    }
    this.dropLine.hidden = false;
  }

  /** `pos` 는 끌어다 놓은 자리(서식 모드). 둘째 파일부터는 앞 링크 뒤에 붙는다. */
  private async attachAll(files: File[], pos?: number) {
    for (const f of files) {
      this.say(`${f.name} 첨부하는 중입니다…`);
      // 서식 모드에서는 사진처럼 본문 안에 「첨부하는 중」 자리를 보이고, 끝나면 그 자리에 넣는다.
      const id = {};
      const rich = this.mode === "rich";
      if (rich) {
        const at = pos ?? this.view.state.selection.from;
        this.view.dispatch(this.view.state.tr.setMeta(placeholderKey, { add: { id, pos: at, label: `${f.name} 첨부하는 중…` } }));
      }
      const where = () => {
        if (!rich) return pos;
        const found = findPlaceholder(this.view.state, id);
        this.view.dispatch(this.view.state.tr.setMeta(placeholderKey, { remove: { id } }));
        return found ?? pos;
      };
      try {
        const a = await attachFile(f, this.csrf);
        this.insertAttachment(a, where());
        pos = undefined;
        this.say("파일을 첨부했습니다. 읽는 사람은 링크를 눌러 내려받습니다.");
      } catch (e) {
        where();
        this.say((e as Error).message, true);
      }
    }
  }

  /**
   * 붙여 넣거나 끌어다 놓은 파일: 그림은 본문 그림으로, 문서는 첨부 링크로, 나머지는 이유를 알린다.
   * 파일이 하나라도 있으면 true (호출한 쪽이 브라우저 기본 동작을 막는다).
   */
  private takeFiles(list: FileList | null | undefined, pos?: number): boolean {
    const { images, attachments, rejected } = sortFiles(list);
    if (!images.length && !attachments.length && !rejected.length) return false;
    this.busy((async () => {
      if (images.length) {
        if (this.mode === "rich") {
          await insertUploads(this.view, images, this.csrf, (msg, err) => this.say(msg, err), pos);
        } else {
          await this.uploadToSource(images);
        }
      }
      // 그림을 넣고 나면 놓은 자리가 밀리므로, 첨부는 그때 커서 자리에 넣는다.
      await this.attachAll(attachments, images.length ? undefined : pos);
      if (rejected.length) {
        const names = rejected.map((f) => f.name).join(", ");
        this.say(`넣을 수 없는 파일입니다: ${names} (그림은 jpg·png·gif·webp, 첨부는 ${ATTACH_EXTENSIONS.join("·")})`, true);
      }
    })());
    return true;
  }

  /** 첨부 파일은 `파일명 (크기)` 글자의 링크로 넣는다. 공개 화면은 이 링크에 📎 표시를 붙인다. */
  private insertAttachment(a: Attached, pos?: number) {
    const label = attachLabel(a);
    if (this.mode === "rich") {
      const tr = this.view.state.tr;
      if (pos !== undefined) {
        // 올리는 동안 문서가 줄었을 수 있다.
        tr.setSelection(Selection.near(tr.doc.resolve(Math.min(pos, tr.doc.content.size))));
      } else if (!tr.selection.empty) {
        tr.setSelection(Selection.near(tr.doc.resolve(tr.selection.to), 1));
      }
      // 앞에 글자·링크·그림이 붙어 있으면 한 칸 띄운다. 링크 둘이 붙으면 하나처럼 보인다.
      const before = tr.selection.$from.nodeBefore;
      if (before && !(before.isText && /\s$/.test(before.text ?? ""))) tr.insertText(" ");
      tr.replaceSelectionWith(schema.text(label, [m.link.create({ href: a.url })]), false);
      // 뒤에 글자가 바로 이어지면 그쪽도 한 칸. 띄어쓰기에는 링크를 걸지 않는다.
      const after = tr.selection.$from.nodeAfter;
      if (after && !(after.isText && /^\s/.test(after.text ?? ""))) tr.replaceSelectionWith(schema.text(" "), false);
      this.view.dispatch(tr.removeStoredMark(m.link).scrollIntoView());
      this.view.focus();
    } else if (this.mode === "md") {
      const text = label.replace(/[\\[\]]/g, "\\$&");
      this.mdInsert(`[${text}](${a.url})`);
    }
  }

  openVideo() {
    this.openPop("동영상 주소 (유튜브·비메오)", "", "https://www.youtube.com/watch?v=…", (raw) => {
      const url = raw.trim();
      if (!videoEmbed(url)) return "유튜브(youtube.com·youtu.be)나 비메오(vimeo.com) 동영상 주소만 넣을 수 있습니다.";
      this.insertBlock(schema.nodes.video.create({ url }), `::video[${url}]`);
      return null;
    });
  }

  openPlace() {
    const clean = (v: string, bad: RegExp) => v.replace(bad, "").replace(/\s+/g, " ").trim();
    this.openPop("장소 이름", "", "예: 강릉 안목해변", (rawName) => {
      const name = clean(rawName, /[[\]]/g);
      if (!name) return "장소 이름을 써 주세요.";
      // 이 입력 칸이 닫힌 뒤 주소 칸을 연다.
      setTimeout(() => {
        this.openPop("주소 (지도 링크에 쓰입니다. 비워도 됩니다)", "", "예: 강원 강릉시 창해로14번길", (rawAddr) => {
          const addr = clean(rawAddr, /[{}]/g);
          this.insertBlock(schema.nodes.place.create({ name, addr }), `::place[${name}]${addr ? `{${addr}}` : ""}`);
          return null;
        });
      }, 0);
      return null;
    });
  }

  private closePop() {
    this.pop.hidden = true;
    this.pop.replaceChildren();
    this.popSticky = false;
    this.popSync = null;
  }

  openLink() {
    if (this.mode !== "rich") return;
    const href = currentLink(this.view.state);
    this.openPop(
      "링크 주소",
      href,
      "https://",
      (raw) => {
        const v = normalizeHref(raw);
        if (v === null) return "http, https, mailto 주소나 / 로 시작하는 사이트 안 주소만 쓸 수 있습니다.";
        setLink(v)(this.view.state, this.view.dispatch);
        return null;
      },
      href ? () => setLink("")(this.view.state, this.view.dispatch) : undefined,
    );
  }

  /** 선택지 단추를 늘어놓는 메뉴. 링크 입력 칸과 같은 자리를 쓴다.
   *  `stay` 면 고른 뒤에도 남아 다른 것을 이어서 고를 수 있다(닫기·Esc·바깥 누르기로 닫는다). */
  private openMenu(title: string, items: MenuItem[], stay = false) {
    this.chars.hidden = true;
    const box = el("div", { class: "me-menu", role: "group", "aria-label": title });
    const buttons: HTMLButtonElement[] = [];
    const sync = () => items.forEach((it, i) => buttons[i].setAttribute("aria-pressed", String(it.isCurrent ? it.isCurrent() : !!it.current)));
    for (const it of items) {
      const b = el("button", { type: "button", class: `me-menu-item ${it.cls ?? ""}` });
      if (it.face) {
        b.append(it.face);
        b.setAttribute("aria-label", it.label);
        b.title = it.label;
      } else {
        b.textContent = it.label;
      }
      b.addEventListener("mousedown", (e) => e.preventDefault());
      b.addEventListener("click", (e) => {
        it.run();
        if (stay) {
          sync();
          // 마우스로 골랐으면 편집 칸으로 돌아가 고른 글자가 보이게, 키보드면 메뉴에 남는다.
          if (e.detail > 0) this.focusEditor();
          return;
        }
        this.closePop();
        if (this.mode === "rich") this.view.focus();
      });
      buttons.push(b);
      box.append(b);
    }
    sync();
    // ← → Home End 로 단추 사이를 옮긴다.
    box.addEventListener("keydown", (e) => {
      const i = buttons.indexOf(document.activeElement as HTMLButtonElement);
      const to = ({ ArrowRight: i + 1, ArrowLeft: i - 1, Home: 0, End: buttons.length - 1 } as Record<string, number>)[e.key];
      if (i < 0 || to === undefined) return;
      e.preventDefault();
      buttons[(to + buttons.length) % buttons.length].focus();
    });
    const close = el("button", { type: "button", class: "secondary" }, "닫기");
    close.addEventListener("click", () => {
      this.closePop();
      if (this.mode === "rich") this.view.focus();
    });
    this.pop.replaceChildren(el("span", { class: "me-menu-title" }, title), box, close);
    this.popApply = () => null;
    this.popSync = stay ? sync : null;
    this.popSticky = stay;
    this.pop.hidden = false;
    if (stay && this.openedByPointer && this.mode === "rich") this.view.focus();
    else (buttons.find((b) => b.getAttribute("aria-pressed") === "true") ?? buttons[0])?.focus();
  }

  openSpanMenu(kind: SpanKind) {
    const spec = SPAN_MENU[kind];
    const type = m[spec.mark];
    const rich = this.mode === "rich";
    const now = () => (this.mode === "rich" ? spanValue(this.view.state, type) : null);
    const apply = (value: string | null) => {
      if (this.mode === "rich") {
        setSpan(type, value)(this.view.state, this.view.dispatch);
      } else if (value) {
        this.mdWrap(`:${kind}[`, `]{${value}}`, "글자");
      }
    };
    // 색은 「가」 견본, 서체는 그 서체로 쓴 이름, 크기는 숫자를 그 크기에 가깝게(24 넘으면 24 로) 보인다.
    const face = (value: string, label: string) => {
      if (kind === "font") return el("span", { class: `ff-${value}` }, label);
      if (kind !== "size") return el("span", { class: `${spec.prefix}-${value}` }, "가");
      const f = el("span", {}, value);
      f.style.fontSize = `${Math.min(Number(value), 24) / 16}rem`;
      return f;
    };
    // 서식 모드는 같은 글자에 여러 값을 바로 바꿔 볼 수 있게 메뉴를 남긴다.
    // Markdown 모드는 누를 때마다 문법이 한 겹씩 더 감싸므로 한 번 고르면 닫는다.
    this.openMenu(
      spec.title,
      [
        ...(rich ? [{ label: spec.none, cls: "me-menu-none", isCurrent: () => now() === null, run: () => apply(null) }] : []),
        ...spec.items.map(([value, label]) => ({
          label: kind === "size" ? `${label}px` : label,
          face: face(value, label),
          cls: kind === "size" ? "me-size" : kind === "font" ? "me-font" : "me-swatch",
          isCurrent: () => now() === value,
          run: () => apply(value),
        })),
      ],
      rich,
    );
  }

  openBoxMenu(group: "callout" | "align") {
    const spec = BOX_MENU[group];
    const cur = this.mode === "rich" ? boxKind(this.view.state, group) : null;
    const apply = (kind: string | null) => {
      if (this.mode === "rich") setBox(group, kind)(this.view.state, this.view.dispatch);
      else if (kind) this.mdBox(kind);
    };
    this.openMenu(spec.title, [
      ...spec.items.map(([kind, label]) => ({ label, current: cur === kind, run: () => apply(kind) })),
      ...(this.mode === "rich" ? [{ label: spec.none, run: () => apply(null) }] : []),
    ]);
  }

  /** Markdown 모드: 고른 줄을 `:::종류` … `:::` 로 감싼다. 앞뒤에 빈 줄을 둔다(상자 문법의 조건). */
  mdBox(kind: string) {
    const ta = this.textarea;
    const { selectionStart: s, selectionEnd: e, value } = ta;
    const from = value.lastIndexOf("\n", s - 1) + 1;
    const endNl = value.indexOf("\n", e === s ? e : e - 1);
    const to = endNl === -1 ? value.length : endNl;
    const inner = value.slice(from, to) || "내용";
    const before = from > 0 && value.slice(0, from).trimEnd() !== "" && !value.slice(0, from).endsWith("\n\n") ? "\n" : "";
    const text = `${before}:::${kind}\n${inner}\n:::`;
    const start = from + before.length + kind.length + 4;
    this.replaceSource(from, to, text, start, start + inner.length);
  }

  /** 단락 단추: 본문·제목·소제목. Markdown 모드에서는 고른 줄 앞에 # 을 붙이거나 뗀다. */
  openParaMenu() {
    if (this.mode === "rich") {
      const s = this.view.state;
      const to = (type: typeof n.paragraph, attrs?: Record<string, unknown>) => () => setBlockType(type, attrs)(this.view.state, this.view.dispatch);
      this.openMenu("문단 종류", [
        { label: "본문", current: blockActive(s, n.paragraph), run: to(n.paragraph) },
        { label: "제목", cls: "me-menu-h2", current: blockActive(s, n.heading, { level: 2 }), run: to(n.heading, { level: 2 }) },
        { label: "소제목", cls: "me-menu-h3", current: blockActive(s, n.heading, { level: 3 }), run: to(n.heading, { level: 3 }) },
      ]);
      return;
    }
    this.openMenu("문단 종류", [
      { label: "본문", run: () => this.mdLines("") },
      { label: "제목", cls: "me-menu-h2", run: () => this.mdLines("## ") },
      { label: "소제목", cls: "me-menu-h3", run: () => this.mdLines("### ") },
    ]);
  }

  /** 자주 쓰지 않는 블록 */
  openMore() {
    const rich = this.mode === "rich";
    const s = this.view.state;
    this.openMenu("더보기", [
      { label: "코드 블록", current: rich && blockActive(s, n.code_block), run: () => (rich ? toggleCodeBlock(this.view.state, this.view.dispatch) : this.mdFence()) },
      { label: "각주", run: () => (rich ? insertFootnote(this.view.state, this.view.dispatch) : this.mdFootnote()) },
      { label: "갤러리 (사진 여러 장을 격자로)", current: rich && !!boxKind(s, "gallery"), run: () => (rich ? setBox("gallery", "gallery")(this.view.state, this.view.dispatch) : this.mdBox("gallery")) },
    ]);
  }

  openCaption() {
    const sel = selectedImage(this.view.state);
    if (!sel) return;
    this.openPop(
      "캡션 (사진 아래에 보이는 설명. 비우면 없앤다)",
      (sel.node.attrs.title as string) ?? "",
      "예: 강릉 안목해변, 2026년 여름",
      (v) => {
        setImageAttrs({ title: v.trim() || null })(this.view.state, this.view.dispatch);
        return null;
      },
    );
  }

  openAlt() {
    const sel = selectedImage(this.view.state);
    if (!sel) return;
    this.openPop(
      "대체 텍스트 (그림을 볼 수 없는 사람에게 읽어 줄 설명)",
      (sel.node.attrs.alt as string) ?? "",
      "예: 노을 지는 바다",
      (v) => {
        setImageAlt(v.trim())(this.view.state, this.view.dispatch);
        return null;
      },
    );
  }

  // ---- 임시 저장·이탈 경고 ----

  private dirty(): boolean {
    return this.markdown() !== this.initialMd || (this.title?.value ?? "") !== this.initialTitle;
  }

  private scheduleDraft() {
    window.clearTimeout(this.draftTimer);
    this.draftTimer = window.setTimeout(() => this.saveDraft(), 1000);
  }

  private saveDraft() {
    try {
      if (!this.dirty()) {
        localStorage.removeItem(this.draftKey);
        return;
      }
      const at = Date.now();
      localStorage.setItem(this.draftKey, JSON.stringify({ md: this.markdown(), title: this.title?.value ?? "", at }));
      const t = new Date(at).toLocaleTimeString("ko-KR", { hour: "2-digit", minute: "2-digit" });
      this.saved.textContent = `${t} 임시 저장`;
      this.saved.title = "이 브라우저에만 남아 있습니다. 서버에 남기려면 저장 버튼을 누르세요.";
    } catch {
      // 저장소가 가득 찼거나 막혀 있으면 임시 저장 없이 계속
    }
  }

  private offerDraft() {
    let draft: { md: string; title: string; at: number } | null = null;
    try {
      draft = JSON.parse(localStorage.getItem(this.draftKey) || "null");
    } catch {
      draft = null;
    }
    if (!draft || (draft.md === this.initialMd && draft.title === this.initialTitle)) return;
    const when = new Date(draft.at).toLocaleString("ko-KR", { month: "long", day: "numeric", hour: "2-digit", minute: "2-digit" });
    const saved = draft;
    this.showNotice(`${when}에 이 브라우저에 임시 저장한, 서버에 저장하지 않은 내용이 있습니다.`, [
      [
        "되살리기",
        () => {
          if (this.title && saved.title) this.title.value = saved.title;
          this.textarea.value = saved.md;
          const loaded = load(saved.md);
          if (this.mode === "rich" && loaded.ok) {
            this.view.updateState(this.createState(loaded.doc));
          } else if (this.mode === "rich") {
            this.mode = "md";
            this.setMode("md");
          }
          this.hideNotice();
          this.refresh();
          this.say("임시 저장한 내용을 되살렸습니다. 저장 버튼을 눌러야 서버에 남습니다.");
        },
      ],
      [
        "버리기",
        () => {
          try {
            localStorage.removeItem(this.draftKey);
          } catch {
            // 무시
          }
          this.hideNotice();
        },
      ],
    ]);
  }

  private bindForm() {
    this.title?.addEventListener("input", () => this.scheduleDraft());
    const buttons = () => Array.from(this.form.querySelectorAll<HTMLButtonElement>("button[type=submit]"));
    this.form.addEventListener("submit", (e) => {
      if (this.uploading > 0) {
        e.preventDefault();
        this.say("사진·파일을 올리는 중입니다. 다 올라간 뒤에 저장해 주세요.", true);
        return;
      }
      this.sync();
      this.submitting = true;
      try {
        localStorage.removeItem(this.draftKey);
      } catch {
        // 무시
      }
      // 누른 버튼에 진행을 보이고, 두 번 눌러 글이 두 개 생기지 않게 잠근다.
      // 지금 잠그면 누른 버튼의 intent 값이 빠지므로 제출이 시작된 다음에 잠근다.
      const by = e.submitter;
      if (by instanceof HTMLButtonElement) {
        by.dataset.label = by.textContent ?? "";
        by.textContent = "저장하는 중…";
        by.setAttribute("aria-busy", "true");
      }
      setTimeout(() => buttons().forEach((b) => (b.disabled = true)));
    });
    // 뒤로 가기로 이 화면에 돌아오면(bfcache) 잠긴 버튼을 푼다.
    window.addEventListener("pageshow", (e) => {
      if (!e.persisted) return;
      this.submitting = false;
      for (const b of buttons()) {
        b.disabled = false;
        b.removeAttribute("aria-busy");
        if (b.dataset.label) b.textContent = b.dataset.label;
      }
    });
    window.addEventListener("beforeunload", (e) => {
      if (this.submitting || !this.dirty()) return;
      e.preventDefault();
      e.returnValue = "";
    });
  }
}

function start() {
  const textarea = document.querySelector<HTMLTextAreaElement>("textarea[data-editor]");
  const form = textarea?.closest("form");
  const csrf = form?.querySelector<HTMLInputElement>('input[name="_csrf"]')?.value;
  if (!textarea || !form || !csrf) return;
  const title = form.querySelector<HTMLInputElement>('input[name="title"]');
  const editor = new Editor(textarea, form, csrf, title);
  bindPublish(form, csrf, () => editor.markdown());
  // 브라우저 확인·자동 검사용. 기능에는 쓰지 않는다.
  (window as unknown as { maruEditor: Editor }).maruEditor = editor;
}

if (document.readyState === "loading") {
  document.addEventListener("DOMContentLoaded", start);
} else {
  start();
}
