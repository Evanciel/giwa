// 서식 명령, 단축키, 입력 규칙.

import {
  baseKeymap,
  chainCommands,
  exitCode,
  lift,
  setBlockType,
  toggleMark,
  wrapIn,
} from "prosemirror-commands";
import { redo, undo } from "prosemirror-history";
import {
  InputRule,
  inputRules,
  textblockTypeInputRule,
  undoInputRule,
  wrappingInputRule,
} from "prosemirror-inputrules";
import { keymap } from "prosemirror-keymap";
import { Mark, MarkType, Node, NodeType } from "prosemirror-model";
import { liftListItem, sinkListItem, splitListItem, wrapInList } from "prosemirror-schema-list";
import { Command, EditorState, NodeSelection, Plugin, TextSelection } from "prosemirror-state";

import { Align, schema } from "./markdown";

const n = schema.nodes;
const m = schema.marks;

export function markActive(state: EditorState, type: MarkType): boolean {
  const { from, $from, to, empty } = state.selection;
  if (empty) return !!type.isInSet(state.storedMarks || $from.marks());
  return state.doc.rangeHasMark(from, to, type);
}

/** 선택이 이 종류의 블록 안에 있는가. `attrs` 가 있으면 속성까지 같아야 한다. */
export function blockActive(state: EditorState, type: NodeType, attrs?: Record<string, unknown>): boolean {
  const { $from, to } = state.selection;
  if (to > $from.end()) return false;
  const node = $from.parent;
  if (node.type !== type) return false;
  return !attrs || Object.entries(attrs).every(([k, v]) => node.attrs[k] === v);
}

/** 선택을 감싸는 조상 중에 이 종류가 있는가 (목록·인용). */
export function wrappedIn(state: EditorState, type: NodeType): boolean {
  const { $from } = state.selection;
  for (let d = $from.depth; d > 0; d--) {
    if ($from.node(d).type === type) return true;
  }
  return false;
}

/** 가장 가까운 목록의 종류. */
function closestList(state: EditorState): NodeType | null {
  const { $from } = state.selection;
  for (let d = $from.depth; d > 0; d--) {
    const t = $from.node(d).type;
    if (t === n.bullet_list || t === n.ordered_list) return t;
  }
  return null;
}

export function toggleHeading(level: number): Command {
  return (state, dispatch) =>
    blockActive(state, n.heading, { level })
      ? setBlockType(n.paragraph)(state, dispatch)
      : setBlockType(n.heading, { level })(state, dispatch);
}

export const toggleCodeBlock: Command = (state, dispatch) =>
  blockActive(state, n.code_block)
    ? setBlockType(n.paragraph)(state, dispatch)
    : setBlockType(n.code_block)(state, dispatch);

export const toggleQuote: Command = (state, dispatch) =>
  wrappedIn(state, n.blockquote) ? lift(state, dispatch) : wrapIn(n.blockquote)(state, dispatch);

/** 같은 목록 안이면 풀고, 다른 목록 안이면 종류를 바꾸고, 밖이면 감싼다. */
export function toggleList(type: NodeType): Command {
  return (state, dispatch, view) => {
    const current = closestList(state);
    if (current === type) return liftListItem(n.list_item)(state, dispatch);
    if (current) {
      const { $from } = state.selection;
      for (let d = $from.depth; d > 0; d--) {
        if ($from.node(d).type === current) {
          if (dispatch) dispatch(state.tr.setNodeMarkup($from.before(d), type));
          return true;
        }
      }
    }
    return wrapInList(type)(state, dispatch, view);
  };
}

export const insertRule: Command = (state, dispatch) => {
  if (dispatch) dispatch(state.tr.replaceSelectionWith(n.horizontal_rule.create()).scrollIntoView());
  return true;
};

const hardBreak: Command = chainCommands(exitCode, (state, dispatch) => {
  if (dispatch) dispatch(state.tr.replaceSelectionWith(n.hard_break.create()).scrollIntoView());
  return true;
});

/** 커서가 링크 안에 있으면 그 링크가 걸린 글자 범위. */
export function linkRange(state: EditorState): { from: number; to: number; href: string } | null {
  const { $from, empty } = state.selection;
  if (!empty) return null;
  const parent = $from.parent;
  const at = $from.parentOffset;
  const kids: { from: number; to: number; marks: readonly Mark[] }[] = [];
  parent.forEach((child, offset) => kids.push({ from: offset, to: offset + child.nodeSize, marks: child.marks }));
  // 커서 뒤 조각을 먼저, 없으면 앞 조각을 본다 (링크 끝에 커서가 있어도 잡히게).
  let i = kids.findIndex((k) => k.from <= at && at < k.to && m.link.isInSet(k.marks));
  if (i < 0) i = kids.findIndex((k) => k.from < at && at <= k.to && m.link.isInSet(k.marks));
  if (i < 0) return null;
  const mark = m.link.isInSet(kids[i].marks);
  if (!mark) return null;
  // 같은 링크가 걸린 이웃 조각(굵게가 섞인 링크 등)까지 넓힌다.
  let a = i;
  let b = i;
  while (a > 0 && mark.isInSet(kids[a - 1].marks)) a--;
  while (b < kids.length - 1 && mark.isInSet(kids[b + 1].marks)) b++;
  const base = $from.start();
  return { from: base + kids[a].from, to: base + kids[b].to, href: mark.attrs.href as string };
}

export const removeLink: Command = (state, dispatch) => {
  const range = linkRange(state);
  const { from, to } = range ?? state.selection;
  if (from === to) return false;
  if (dispatch) dispatch(state.tr.removeMark(from, to, m.link).scrollIntoView());
  return true;
};

/** 선택한 글자에 링크를 건다. 선택이 비었으면 주소를 글자로 넣는다. 빈 주소는 링크를 지운다.
 *  커서가 이미 링크 안에 있으면 그 링크의 주소를 바꾼다. */
export function setLink(href: string): Command {
  return (state, dispatch) => {
    const existing = linkRange(state);
    let { from, to } = existing ?? state.selection;
    // 두 번 눌러 단어를 고르면 뒤 공백까지 잡힌다. 링크 글자에는 앞뒤 공백을 넣지 않는다.
    while (from < to && /\s/.test(state.doc.textBetween(from, from + 1))) from++;
    while (to > from && /\s/.test(state.doc.textBetween(to - 1, to))) to--;
    const empty = from === to;
    if (!dispatch) return true;
    const tr = state.tr;
    if (!href) {
      tr.removeMark(from, to, m.link);
    } else if (empty) {
      tr.insert(from, schema.text(href, [m.link.create({ href })]));
    } else {
      tr.removeMark(from, to, m.link).addMark(from, to, m.link.create({ href }));
    }
    dispatch(tr.scrollIntoView());
    return true;
  };
}

/** 선택 위치의 링크 주소 (없으면 빈 문자열). */
export function currentLink(state: EditorState): string {
  const { $from, from, to } = state.selection;
  let href = "";
  const found = m.link.isInSet($from.marks());
  if (found) return found.attrs.href as string;
  state.doc.nodesBetween(from, to, (node) => {
    const mk = m.link.isInSet(node.marks);
    if (mk && !href) href = mk.attrs.href as string;
  });
  return href;
}

/** 선택한 그림 노드. */
export function selectedImage(state: EditorState) {
  const sel = state.selection;
  return sel instanceof NodeSelection && sel.node.type === n.image ? sel : null;
}

export function setImageAlt(alt: string): Command {
  return setImageAttrs({ alt });
}

/** 선택한 그림의 속성(대체 텍스트·캡션·크기)을 바꾸고 그림을 계속 고른 상태로 둔다. */
export function setImageAttrs(patch: { alt?: string; title?: string | null; width?: number | null }): Command {
  return (state, dispatch) => {
    const sel = selectedImage(state);
    if (!sel) return false;
    if (dispatch) {
      const tr = state.tr.setNodeMarkup(sel.from, undefined, { ...sel.node.attrs, ...patch });
      tr.setSelection(NodeSelection.create(tr.doc, sel.from));
      dispatch(tr);
    }
    return true;
  };
}

// ---- 할 일 목록 ----

/** 선택을 감싼 가장 가까운 목록 항목의 위치. */
function closestItem(state: EditorState): { pos: number; checked: boolean | null } | null {
  const { $from } = state.selection;
  for (let d = $from.depth; d > 0; d--) {
    const node = $from.node(d);
    if (node.type === n.list_item) return { pos: $from.before(d), checked: node.attrs.checked };
  }
  return null;
}

export function taskActive(state: EditorState): boolean {
  const item = closestItem(state);
  return !!item && item.checked !== null;
}

/** 할 일 목록으로 바꾸거나 되돌린다. 목록 밖이면 글머리 목록으로 감싼 뒤 할 일로 만든다. */
export const toggleTask: Command = (state, dispatch, view) => {
  const item = closestItem(state);
  if (item) {
    if (dispatch) {
      const next = item.checked === null ? false : null;
      // 선택이 여러 항목에 걸치면 같은 목록의 항목을 모두 바꾼다.
      const tr = state.tr;
      const { from, to } = state.selection;
      let touched = false;
      state.doc.nodesBetween(from, to, (node, pos) => {
        if (node.type === n.list_item) {
          tr.setNodeMarkup(pos, undefined, { ...node.attrs, checked: next });
          touched = true;
        }
      });
      if (!touched) tr.setNodeMarkup(item.pos, undefined, { checked: next });
      dispatch(tr);
    }
    return true;
  }
  if (!wrapInList(n.bullet_list)(state)) return false;
  if (dispatch) {
    wrapInList(n.bullet_list)(state, (tr) => {
      const after = state.apply(tr);
      const it = closestItem(after);
      if (it) tr.setNodeMarkup(it.pos, undefined, { checked: false });
      dispatch(tr);
    }, view);
  }
  return true;
};

/** 목록 항목을 나눈다. 새 할 일 항목은 언제나 「안 함」으로 시작한다. */
const splitItem: Command = (state, dispatch, view) =>
  splitListItem(n.list_item)(
    state,
    dispatch &&
      ((tr) => {
        const it = closestItem(state.apply(tr));
        if (it && it.checked === true) tr.setNodeMarkup(it.pos, undefined, { checked: false });
        dispatch(tr);
      }),
    view,
  );

/** 체크 상자를 눌렀을 때. */
export function toggleChecked(pos: number): Command {
  return (state, dispatch) => {
    const node = state.doc.nodeAt(pos);
    if (!node || node.type !== n.list_item || node.attrs.checked === null) return false;
    if (dispatch) dispatch(state.tr.setNodeMarkup(pos, undefined, { checked: !node.attrs.checked }));
    return true;
  };
}

// ---- 각주 ----

/** 커서 자리에 각주 번호를 넣고 글 끝에 각주 칸을 만든 뒤 그리로 옮긴다. */
export const insertFootnote: Command = (state, dispatch) => {
  if (!dispatch) return true;
  let max = 0;
  state.doc.descendants((node) => {
    if (node.type === n.footnote_ref || node.type === n.footnote_def) {
      const v = Number(node.attrs.label);
      if (Number.isInteger(v)) max = Math.max(max, v);
    }
  });
  const label = String(max + 1);
  const tr = state.tr.replaceSelectionWith(n.footnote_ref.create({ label }), false);
  const end = tr.doc.content.size;
  tr.insert(end, n.footnote_def.create({ label }));
  tr.setSelection(TextSelection.create(tr.doc, end + 1));
  dispatch(tr.scrollIntoView());
  return true;
};

/** 각주 번호를 누르면 글 끝의 그 각주로 간다. */
export function gotoFootnote(label: string): Command {
  return (state, dispatch) => {
    let target = -1;
    state.doc.descendants((node, pos) => {
      if (target < 0 && node.type === n.footnote_def && node.attrs.label === label) target = pos + 1 + node.content.size;
    });
    if (target < 0) return false;
    if (dispatch) dispatch(state.tr.setSelection(TextSelection.create(state.doc, target)).scrollIntoView());
    return true;
  };
}

// ---- 강조 상자·정렬·글자 꾸밈 ----

export type BoxGroup = "align" | "callout" | "gallery";

function boxGroup(kind: string): BoxGroup {
  if (kind === "gallery") return "gallery";
  return kind === "center" || kind === "right" ? "align" : "callout";
}

/** 선택을 감싼 가장 가까운 상자 가운데 `group`(정렬·강조·사진 격자)에 드는 것. */
function closestBox(state: EditorState, group: BoxGroup): { pos: number; kind: string; depth: number } | null {
  const { $from } = state.selection;
  for (let d = $from.depth; d > 0; d--) {
    const node = $from.node(d);
    if (node.type !== n.box) continue;
    if (boxGroup(node.attrs.kind) === group) return { pos: $from.before(d), kind: node.attrs.kind, depth: d };
  }
  return null;
}

export function boxKind(state: EditorState, group: BoxGroup): string | null {
  return closestBox(state, group)?.kind ?? null;
}

/** 상자로 감싼다. 같은 종류 안이면 풀고, 다른 종류 안이면 종류만 바꾼다. `kind` 가 null 이면 푼다. */
export function setBox(group: BoxGroup, kind: string | null): Command {
  return (state, dispatch) => {
    const cur = closestBox(state, group);
    if (cur && (kind === null || kind === cur.kind)) {
      // 상자를 벗긴다: 안의 블록을 상자 자리에 그대로 둔다.
      if (dispatch) {
        const node = state.doc.nodeAt(cur.pos);
        if (!node) return false;
        dispatch(state.tr.replaceWith(cur.pos, cur.pos + node.nodeSize, node.content).scrollIntoView());
      }
      return true;
    }
    if (kind === null) return false;
    if (cur) {
      if (dispatch) dispatch(state.tr.setNodeMarkup(cur.pos, undefined, { kind }));
      return true;
    }
    return wrapIn(n.box, { kind })(state, dispatch);
  };
}

// ---- 표 ----
// Markdown 표는 칸 병합이 없으므로 표를 칸 노드의 2차원 배열로 다루고, 고칠 때마다 표를 통째로 다시 만든다.
// 첫 줄이 머리글이다(어느 줄을 지워도 다시 만들 때 첫 줄이 머리글이 된다).

interface TableCtx {
  table: Node;
  pos: number;
  row: number;
  col: number;
}

/** 선택이 든 표와 칸 위치. */
export function tableContext(state: EditorState): TableCtx | null {
  const { $from } = state.selection;
  for (let d = $from.depth; d > 2; d--) {
    if ($from.node(d).type === n.table_cell) {
      return { table: $from.node(d - 2), pos: $from.before(d - 2), row: $from.index(d - 2), col: $from.index(d - 1) };
    }
  }
  return null;
}

function grid(table: Node): Node[][] {
  const rows: Node[][] = [];
  table.forEach((row) => {
    const cells: Node[] = [];
    row.forEach((c) => cells.push(c));
    rows.push(cells);
  });
  return rows;
}

function emptyCell(align: Align = null): Node {
  return n.table_cell.create({ header: false, align });
}

/** 칸 배열로 표를 다시 만들고 `focus`(줄, 칸) 칸 안에 커서를 둔다. */
function rebuild(state: EditorState, ctx: TableCtx, rows: Node[][], focus: [number, number]) {
  const table = n.table.create(
    null,
    rows.map((cells, r) =>
      n.table_row.create(
        null,
        cells.map((c) => n.table_cell.create({ ...c.attrs, header: r === 0 }, c.content, c.marks)),
      ),
    ),
  );
  const tr = state.tr.replaceWith(ctx.pos, ctx.pos + ctx.table.nodeSize, table);
  const [fr, fc] = focus;
  let at = ctx.pos + 1;
  table.forEach((row, _o, r) => {
    if (r < fr) at += row.nodeSize;
  });
  at += 1;
  table.child(fr).forEach((c, _o, i) => {
    if (i < fc) at += c.nodeSize;
  });
  tr.setSelection(TextSelection.create(tr.doc, at + 1));
  return tr.scrollIntoView();
}

function colAlign(rows: Node[][], col: number): Align {
  return (rows[0]?.[col]?.attrs.align as Align) ?? null;
}

/** 빈 표를 넣는다. 첫 줄은 머리글이다. */
export function insertTable(rows = 3, cols = 3): Command {
  return (state, dispatch) => {
    if (tableContext(state)) return false;
    if (dispatch) {
      const table = n.table.create(
        null,
        Array.from({ length: rows }, (_, r) =>
          n.table_row.create(null, Array.from({ length: cols }, () => n.table_cell.create({ header: r === 0 }))),
        ),
      );
      const tr = state.tr.replaceSelectionWith(table);
      // 넣은 표의 첫 칸으로 커서를 옮긴다.
      let start = -1;
      tr.doc.nodesBetween(tr.mapping.map(state.selection.from, -1), tr.doc.content.size, (node, pos) => {
        if (start < 0 && node.type === n.table_cell) start = pos + 1;
        return start < 0;
      });
      if (start > 0) tr.setSelection(TextSelection.create(tr.doc, start));
      dispatch(tr.scrollIntoView());
    }
    return true;
  };
}

export function addRow(after: boolean): Command {
  return (state, dispatch) => {
    const ctx = tableContext(state);
    if (!ctx) return false;
    if (dispatch) {
      const rows = grid(ctx.table);
      const cols = Math.max(...rows.map((r) => r.length));
      const at = after ? ctx.row + 1 : ctx.row;
      rows.splice(at, 0, Array.from({ length: cols }, (_, c) => emptyCell(colAlign(rows, c))));
      dispatch(rebuild(state, ctx, rows, [at, Math.min(ctx.col, cols - 1)]));
    }
    return true;
  };
}

export function addColumn(after: boolean): Command {
  return (state, dispatch) => {
    const ctx = tableContext(state);
    if (!ctx) return false;
    if (dispatch) {
      const rows = grid(ctx.table);
      const at = after ? ctx.col + 1 : ctx.col;
      for (const cells of rows) cells.splice(Math.min(at, cells.length), 0, emptyCell());
      dispatch(rebuild(state, ctx, rows, [ctx.row, at]));
    }
    return true;
  };
}

export const deleteTable: Command = (state, dispatch) => {
  const ctx = tableContext(state);
  if (!ctx) return false;
  if (dispatch) {
    const tr = state.tr.replaceWith(ctx.pos, ctx.pos + ctx.table.nodeSize, n.paragraph.create());
    tr.setSelection(TextSelection.create(tr.doc, ctx.pos + 1));
    dispatch(tr.scrollIntoView());
  }
  return true;
};

export const deleteRow: Command = (state, dispatch) => {
  const ctx = tableContext(state);
  if (!ctx) return false;
  if (ctx.table.childCount === 1) return deleteTable(state, dispatch);
  if (dispatch) {
    const rows = grid(ctx.table);
    rows.splice(ctx.row, 1);
    const r = Math.min(ctx.row, rows.length - 1);
    dispatch(rebuild(state, ctx, rows, [r, Math.min(ctx.col, rows[r].length - 1)]));
  }
  return true;
};

export const deleteColumn: Command = (state, dispatch) => {
  const ctx = tableContext(state);
  if (!ctx) return false;
  const rows = grid(ctx.table);
  if (Math.max(...rows.map((r) => r.length)) === 1) return deleteTable(state, dispatch);
  if (dispatch) {
    for (const cells of rows) if (cells.length > 1 && ctx.col < cells.length) cells.splice(ctx.col, 1);
    const c = Math.max(0, Math.min(ctx.col, rows[ctx.row].length - 1));
    dispatch(rebuild(state, ctx, rows, [ctx.row, c]));
  }
  return true;
};

/** 커서가 있는 열의 정렬. */
export function setColumnAlign(align: Align): Command {
  return (state, dispatch) => {
    const ctx = tableContext(state);
    if (!ctx) return false;
    if (dispatch) {
      const rows = grid(ctx.table).map((cells) =>
        cells.map((c, i) => (i === ctx.col ? n.table_cell.create({ ...c.attrs, align }, c.content, c.marks) : c)),
      );
      dispatch(rebuild(state, ctx, rows, [ctx.row, ctx.col]));
    }
    return true;
  };
}

export function columnAlign(state: EditorState): Align {
  const ctx = tableContext(state);
  return ctx ? colAlign(grid(ctx.table), ctx.col) : null;
}

/** Tab·Shift-Tab: 다음·이전 칸으로. 마지막 칸에서 Tab 이면 줄을 더한다. */
export function goToCell(dir: 1 | -1): Command {
  return (state, dispatch) => {
    const ctx = tableContext(state);
    if (!ctx) return false;
    const rows = grid(ctx.table);
    let { row, col } = ctx;
    col += dir;
    if (col >= rows[row].length) {
      row += 1;
      col = 0;
    } else if (col < 0) {
      row -= 1;
      col = row >= 0 ? rows[row].length - 1 : 0;
    }
    if (row < 0) return true;
    if (dispatch) {
      if (row >= rows.length) rows.push(Array.from({ length: rows[0].length }, (_, c) => emptyCell(colAlign(rows, c))));
      dispatch(rebuild(state, ctx, rows, [row, col]));
    }
    return true;
  };
}

/** Enter: 아래 칸으로. 마지막 줄이면 줄을 더한다. */
const moveDown: Command = (state, dispatch) => {
  const ctx = tableContext(state);
  if (!ctx) return false;
  if (dispatch) {
    const rows = grid(ctx.table);
    const row = ctx.row + 1;
    if (row >= rows.length) rows.push(Array.from({ length: rows[0].length }, (_, c) => emptyCell(colAlign(rows, c))));
    dispatch(rebuild(state, ctx, rows, [row, Math.min(ctx.col, rows[row].length - 1)]));
  }
  return true;
};

/** 선택한 글자의 꾸밈 값(색 이름 등). 섞여 있으면 첫 값. */
export function spanValue(state: EditorState, type: MarkType): string | null {
  const { from, to, empty, $from } = state.selection;
  if (empty) return (type.isInSet(state.storedMarks || $from.marks())?.attrs.value as string) ?? null;
  let value: string | null = null;
  state.doc.nodesBetween(from, to, (node) => {
    const mk = type.isInSet(node.marks);
    if (mk && value === null) value = mk.attrs.value as string;
  });
  return value;
}

/** 글자 꾸밈을 바꾼다. `value` 가 null 이면 지운다. 선택이 비었으면 이어서 칠 글자에 적용한다. */
export function setSpan(type: MarkType, value: string | null): Command {
  return (state, dispatch) => {
    const { from, to, empty } = state.selection;
    if (!dispatch) return true;
    if (empty) {
      const tr = state.tr.removeStoredMark(type);
      if (value) tr.addStoredMark(type.create({ value }));
      dispatch(tr);
      return true;
    }
    const tr = state.tr.removeMark(from, to, type);
    if (value) tr.addMark(from, to, type.create({ value }));
    dispatch(tr.scrollIntoView());
    return true;
  };
}

// ---- 자동 링크 ----

const URL_RE = /^(?:https?:\/\/|www\.)[^\s<>]*[^\s<>.,;:!?'")\]]$/i;

export function linkHref(text: string): string | null {
  const t = text.trim();
  if (!URL_RE.test(t)) return null;
  return /^www\./i.test(t) ? `https://${t}` : t;
}

/** 주소를 치고 띄어 쓰면 링크로 만든다. */
const autoLinkRule = new InputRule(/(?:^|\s)((?:https?:\/\/|www\.)[^\s<>]*[^\s<>.,;:!?'")\]])\s$/i, (state, match, start, end) => {
  const url = match[1];
  const $start = state.doc.resolve(start);
  if ($start.parent.type.spec.code) return null;
  const from = start + match[0].indexOf(url);
  const to = from + url.length;
  if (state.doc.rangeHasMark(from, to, m.link) || state.doc.rangeHasMark(from, to, m.code)) return null;
  const href = linkHref(url);
  if (!href) return null;
  const typed = match[0].slice(-1);
  return state.tr.addMark(from, to, m.link.create({ href })).insertText(typed, end).removeStoredMark(m.link);
});

// `**굵게**`, `*기울임*`, `~~취소~~`, `` `코드` `` 를 치면 바로 서식으로 바꾼다.
function markRule(pattern: RegExp, type: MarkType): InputRule {
  return new InputRule(pattern, (state, match, start, end) => {
    const text = match[1];
    if (!text) return null;
    const tr = state.tr.replaceWith(start, end, schema.text(text, [type.create()]));
    return tr.removeStoredMark(type);
  });
}

export function buildPlugins(onLink: () => void): Plugin[] {
  const rules = inputRules({
    rules: [
      textblockTypeInputRule(/^(#{1,6})\s$/, n.heading, (match) => ({ level: match[1].length })),
      wrappingInputRule(/^\s*>\s$/, n.blockquote),
      wrappingInputRule(/^\s*([-+*])\s$/, n.bullet_list),
      wrappingInputRule(
        /^(\d+)\.\s$/,
        n.ordered_list,
        (match) => ({ order: +match[1] }),
        (match, node) => node.childCount + node.attrs.order === +match[1],
      ),
      textblockTypeInputRule(/^```([a-z0-9+#-]*)\s$/i, n.code_block, (match) => ({ params: match[1] || "" })),
      new InputRule(/^(?:---|\*\*\*|___)\s$/, (state, _match, start, end) => {
        const $start = state.doc.resolve(start);
        if ($start.parent.type !== n.paragraph) return null;
        return state.tr.replaceRangeWith($start.before(), end, n.horizontal_rule.create());
      }),
      markRule(/\*\*([^*\s][^*]*[^*\s]|[^*\s])\*\*$/, m.strong),
      markRule(/(?<![*\w])\*([^*\s][^*]*[^*\s]|[^*\s])\*$/, m.em),
      markRule(/~~([^~\s][^~]*[^~\s]|[^~\s])~~$/, m.strike),
      markRule(/`([^`]+)`$/, m.code),
      // 목록 항목 맨 앞에서 `[ ] `·`[x] ` 를 치면 할 일로. 목록 밖에서 `[] ` 는 새 할 일 목록.
      new InputRule(/^\[( |x|X)?\]\s$/, (state, match, start, end) => {
        const $start = state.doc.resolve(start);
        if ($start.parent.type !== n.paragraph) return null;
        const checked = !!match[1] && match[1] !== " ";
        const tr = state.tr.delete(start, end);
        const item = closestItem(state.apply(tr));
        if (item && $start.index($start.depth - 1) === 0) {
          return tr.setNodeMarkup(item.pos, undefined, { checked });
        }
        if (item) return null;
        const wrapped = state.apply(tr);
        let out = null as ReturnType<typeof state.tr.setNodeMarkup> | null;
        wrapInList(n.bullet_list)(wrapped, (wtr) => {
          for (const step of wtr.steps) tr.step(step);
          const it = closestItem(state.apply(tr));
          if (it) tr.setNodeMarkup(it.pos, undefined, { checked });
          out = tr;
        });
        return out;
      }),
      autoLinkRule,
    ],
  });

  const keys = keymap({
    "Mod-z": undo,
    "Shift-Mod-z": redo,
    "Mod-y": redo,
    Backspace: undoInputRule,
    "Mod-b": toggleMark(m.strong),
    "Mod-i": toggleMark(m.em),
    "Mod-e": toggleMark(m.code),
    "Mod-Shift-x": toggleMark(m.strike),
    "Mod-Shift-9": toggleTask,
    "Mod-k": () => {
      onLink();
      return true;
    },
    "Mod-Alt-0": setBlockType(n.paragraph),
    "Mod-Alt-1": toggleHeading(1),
    "Mod-Alt-2": toggleHeading(2),
    "Mod-Alt-3": toggleHeading(3),
    "Shift-Enter": hardBreak,
    "Mod-Enter": hardBreak,
    // 표 칸에서 Enter 는 아래 칸으로 간다(칸을 쪼개지 않는다). 마지막 줄이면 줄을 더한다.
    Enter: chainCommands(moveDown, splitItem),
    // 표 안에서는 칸을 옮겨 다니고, 목록 안에서는 들여쓴다. 밖에서는 Tab 이 다음 요소로 초점을 옮긴다 (키보드 사용자).
    Tab: chainCommands(goToCell(1), sinkListItem(n.list_item)),
    "Shift-Tab": chainCommands(goToCell(-1), liftListItem(n.list_item)),
    "Mod-[": liftListItem(n.list_item),
    "Mod-]": sinkListItem(n.list_item),
  });

  return [rules, keys, keymap(baseKeymap)];
}
