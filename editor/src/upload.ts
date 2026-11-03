// 이미지 업로드. TOAST UI 의 구조처럼 "파일을 서버에 올리는 일"과 "문서에 넣는 일"을 나눈다.
// 다만 기본값이 base64 본문 삽입이 아니라 서버 업로드다 (본문이 부풀지 않고, 다시 인코딩으로 메타데이터가 지워진다).

import { Plugin, PluginKey, EditorState, Transaction } from "prosemirror-state";
import { Decoration, DecorationSet, EditorView } from "prosemirror-view";

import { schema } from "./markdown";

export const MAX_BYTES = 10 * 1024 * 1024;
const ACCEPT = ["image/jpeg", "image/png", "image/gif", "image/webp"];

export interface Uploaded {
  url: string;
  alt_text: string | null;
  width: number;
  height: number;
}

/** 클립보드에서 온 그림처럼 이름이 의미 없으면 대체 텍스트를 비워 둔다 (나중에 직접 쓰게). */
export function altFromName(name: string): string {
  const stem = name.replace(/\.[^.]+$/, "").trim();
  if (!stem || /^(image|img|screenshot|스크린샷|clipboard)[\s_-]*\d*$/i.test(stem) || /^IMG[_-]?\d+$/i.test(stem)) {
    return "";
  }
  return stem.replace(/[_-]+/g, " ");
}

export function imageFiles(list: FileList | null | undefined): File[] {
  return Array.from(list ?? []).filter((f) => ACCEPT.includes(f.type));
}

export async function uploadFile(file: File, csrf: string): Promise<Uploaded> {
  if (!ACCEPT.includes(file.type)) {
    throw new Error("jpg, png, gif, webp 이미지만 올릴 수 있습니다.");
  }
  if (file.size > MAX_BYTES) {
    throw new Error("10MB 이하 이미지만 올릴 수 있습니다.");
  }
  const alt = altFromName(file.name);
  const q = new URLSearchParams({ filename: file.name || "image.png" });
  if (alt) q.set("alt", alt);
  let res: Response;
  try {
    res = await fetch(`/admin/media?${q}`, {
      method: "POST",
      credentials: "same-origin",
      headers: { "X-CSRF-Token": csrf, "Content-Type": file.type },
      body: file,
    });
  } catch {
    throw new Error("서버에 연결하지 못했습니다.");
  }
  let body: { ok?: boolean; data?: Uploaded; error?: { message?: string } } = {};
  try {
    body = await res.json();
  } catch {
    // 413 처럼 본문이 JSON 이 아닌 응답
  }
  if (!res.ok || !body.ok || !body.data) {
    if (res.status === 413) throw new Error("10MB 이하 이미지만 올릴 수 있습니다.");
    if (res.status === 401) throw new Error("로그인이 끝났습니다. 새 탭에서 다시 로그인한 뒤 시도하세요.");
    throw new Error(body.error?.message || `업로드하지 못했습니다 (${res.status}).`);
  }
  return body.data;
}

// ---- 첨부 파일 ----
// 서버(`services::files`)와 같은 확장자만 고른다. 내용 서명은 서버가 다시 본다.

export const ATTACH_EXTENSIONS = ["pdf", "hwp", "hwpx", "docx", "xlsx", "pptx", "doc", "xls", "ppt", "zip", "txt", "csv", "md"];
export const ATTACH_ACCEPT = ATTACH_EXTENSIONS.map((e) => `.${e}`).join(",");

export function isAttachable(file: File): boolean {
  return ATTACH_EXTENSIONS.includes(file.name.split(".").pop()?.toLowerCase() ?? "");
}

/** 붙여 넣거나 끌어다 놓은 파일을 그림·첨부·받을 수 없는 것으로 나눈다. */
export function sortFiles(list: FileList | null | undefined): { images: File[]; attachments: File[]; rejected: File[] } {
  const images: File[] = [];
  const attachments: File[] = [];
  const rejected: File[] = [];
  for (const f of Array.from(list ?? [])) {
    if (ACCEPT.includes(f.type)) images.push(f);
    else if (isAttachable(f)) attachments.push(f);
    else rejected.push(f);
  }
  return { images, attachments, rejected };
}

export interface Attached {
  url: string;
  filename: string;
  size: number;
}

export async function attachFile(file: File, csrf: string): Promise<Attached> {
  if (!isAttachable(file)) {
    throw new Error(`첨부할 수 없는 형식입니다: ${file.name} (가능: ${ATTACH_EXTENSIONS.join(", ")})`);
  }
  if (file.size > MAX_BYTES) {
    throw new Error("10MB 이하 파일만 첨부할 수 있습니다.");
  }
  const q = new URLSearchParams({ filename: file.name });
  let res: Response;
  try {
    res = await fetch(`/admin/files?${q}`, {
      method: "POST",
      credentials: "same-origin",
      headers: { "X-CSRF-Token": csrf, "Content-Type": "application/octet-stream" },
      body: file,
    });
  } catch {
    throw new Error("서버에 연결하지 못했습니다.");
  }
  let body: { ok?: boolean; data?: Attached; error?: { message?: string } } = {};
  try {
    body = await res.json();
  } catch {
    // 413 처럼 본문이 JSON 이 아닌 응답
  }
  if (!res.ok || !body.ok || !body.data) {
    if (res.status === 413) throw new Error("10MB 이하 파일만 첨부할 수 있습니다.");
    if (res.status === 401) throw new Error("로그인이 끝났습니다. 새 탭에서 다시 로그인한 뒤 시도하세요.");
    throw new Error(body.error?.message || `첨부하지 못했습니다 (${res.status}).`);
  }
  return body.data;
}

/** 본문에 넣을 링크 글자: `견적서.pdf (120KB)` */
export function attachLabel(a: Attached): string {
  const kb = a.size / 1024;
  const size = kb >= 1024 ? `${(kb / 1024).toFixed(1)}MB` : `${Math.max(1, Math.round(kb))}KB`;
  return `${a.filename} (${size})`;
}

// ---- 업로드 중 자리 표시 ----
// 업로드하는 동안 문서가 바뀌어도 자리를 따라가도록 장식(decoration)으로 표시하고, 끝나면 그 자리에 그림을 넣는다.

type Action = { add: { id: object; pos: number; label?: string } } | { remove: { id: object } };

export const placeholderKey = new PluginKey<DecorationSet>("maru-upload");

export const placeholderPlugin = new Plugin<DecorationSet>({
  key: placeholderKey,
  state: {
    init: () => DecorationSet.empty,
    apply(tr: Transaction, set: DecorationSet) {
      set = set.map(tr.mapping, tr.doc);
      const action = tr.getMeta(placeholderKey) as Action | undefined;
      if (action && "add" in action) {
        const el = document.createElement("span");
        el.className = "me-uploading";
        el.textContent = action.add.label ?? "이미지 올리는 중…";
        el.setAttribute("role", "status");
        const deco = Decoration.widget(action.add.pos, el, { id: action.add.id });
        set = set.add(tr.doc, [deco]);
      } else if (action && "remove" in action) {
        set = set.remove(set.find(undefined, undefined, (spec) => spec.id === action.remove.id));
      }
      return set;
    },
  },
  props: {
    decorations(state) {
      return this.getState(state);
    },
  },
});

export function findPlaceholder(state: EditorState, id: object): number | null {
  const set = placeholderKey.getState(state);
  const found = set?.find(undefined, undefined, (spec) => spec.id === id);
  return found && found.length ? found[0].from : null;
}

/** 파일을 올리고 `pos`(없으면 현재 선택)에 그림을 넣는다. 실패하면 자리 표시를 지우고 알린다. */
export async function insertUploads(
  view: EditorView,
  files: File[],
  csrf: string,
  report: (message: string, error?: boolean) => void,
  pos?: number,
): Promise<void> {
  for (const file of files) {
    const id = {};
    const tr = view.state.tr;
    if (pos === undefined && !tr.selection.empty) tr.deleteSelection();
    const at = pos ?? tr.selection.from;
    tr.setMeta(placeholderKey, { add: { id, pos: at } });
    view.dispatch(tr);
    report("이미지를 올리는 중입니다…");
    try {
      const up = await uploadFile(file, csrf);
      const where = findPlaceholder(view.state, id);
      if (where === null) continue;
      const node = schema.nodes.image.create({ src: up.url, alt: up.alt_text ?? "" });
      const tr = view.state.tr.setMeta(placeholderKey, { remove: { id } });
      const $at = tr.doc.resolve(where);
      if ($at.parent.type === schema.nodes.paragraph && $at.parent.content.size > 0) {
        // 글자가 있는 문단이면 그 아래 새 문단에 넣는다. 블로그 사진은 보통 한 줄을 차지한다.
        tr.insert($at.after(), schema.nodes.paragraph.create(null, node));
      } else {
        tr.replaceWith(where, where, node);
      }
      view.dispatch(tr);
      report(up.alt_text ? "이미지를 넣었습니다." : "이미지를 넣었습니다. 그림을 누르고 대체 텍스트를 써 주세요.");
    } catch (e) {
      view.dispatch(view.state.tr.setMeta(placeholderKey, { remove: { id } }));
      report((e as Error).message, true);
    }
  }
}
