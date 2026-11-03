// 발행 설정 서랍. 글쓰기 화면(edit.html)의 `section#publish` 를 오른쪽에서 나오는 서랍으로 바꾼다.
//
// - 스크립트가 없으면 이 칸은 본문 아래에 그대로 보이고 그대로 동작한다.
// - 주소: 입력하는 동안 겹치는지 서버에 묻는다 (`/admin/slug-check`, core `posts::check_slug`).
// - 요약·대표 이미지: 저장 전 본문을 서버 미리보기(`/admin/preview`)에 보내 자동 요약과 본문 사진을 받는다.
// - 분류: 새 글은 고른 게시판의 분류만 보인다. 권한이 있으면 그 자리에서 새 분류를 만든다(`/admin/categories`).
// - 태그: 쉼표·Enter 로 칩을 만든다. 값은 언제나 원래 칸(`name=tags`)에 쉼표로 이어 둔다.
// - 서랍은 값만 고친다. 저장은 언제나 폼 제출(발행·예약·저장 단추)이다.

import { TAG_CHARS, TAG_MAX, splitTags, tagKey } from "./tags";
import { uploadFile } from "./upload";

const SUMMARY_MIN = 80;
const SUMMARY_MAX = 150;

interface SlugCheck {
  slug: string;
  available: boolean;
  suggestion: string | null;
  derived: boolean;
  path: string;
}

interface PreviewData {
  summary: string;
  images: string[];
}

function el<K extends keyof HTMLElementTagNameMap>(tag: K, attrs: Record<string, string> = {}, text = ""): HTMLElementTagNameMap[K] {
  const e = document.createElement(tag);
  for (const [k, v] of Object.entries(attrs)) e.setAttribute(k, v);
  if (text) e.textContent = text;
  return e;
}

/** 주소의 %EA%B0… 를 한글로 보인다 (보이기만 한다). */
function readable(url: string): string {
  try {
    return decodeURI(url);
  } catch {
    return url;
  }
}

async function getJson<T>(url: string, csrf: string, init: RequestInit = {}): Promise<T> {
  const res = await fetch(url, {
    credentials: "same-origin",
    ...init,
    headers: { "X-CSRF-Token": csrf, ...(init.headers ?? {}) },
  });
  const body = await res.json().catch(() => ({}));
  if (!res.ok || !body.ok) throw new Error(body?.error?.message || `서버가 응답하지 않습니다 (${res.status}).`);
  return body.data as T;
}

export function bindPublish(form: HTMLFormElement, csrf: string, markdown: () => string) {
  const panel = form.querySelector<HTMLElement>("section#publish");
  if (!panel) return;
  const $ = <T extends Element>(sel: string) => panel.querySelector<T>(sel);
  const slug = $<HTMLInputElement>("#slug");
  const slugInfo = $<HTMLElement>("[data-slug-info]");
  const excerpt = $<HTMLTextAreaElement>("#excerpt");
  const excerptInfo = $<HTMLElement>("[data-excerpt-info]");
  const cover = $<HTMLInputElement>("#cover");
  const pick = $<HTMLElement>("[data-cover-pick]");
  const share = $<HTMLElement>("[data-share]");
  const category = $<HTMLSelectElement>("[data-category]");
  const title = form.querySelector<HTMLInputElement>("#title");
  const boardSelect = form.querySelector<HTMLSelectElement>("#board");
  const siteUrl = (form.dataset.siteUrl ?? "").replace(/\/+$/, "");
  const postId = form.dataset.postId ?? "";
  const board = () => boardSelect?.value || form.dataset.board || "";

  // ---- 서랍 열고 닫기 ----
  // 처음 서랍으로 바꿀 때는 미끄러져 나가는 모습이 보이지 않게 애니메이션 없이.
  panel.style.transition = "none";
  panel.classList.add("is-drawer");
  void panel.offsetWidth;
  panel.style.transition = "";
  panel.setAttribute("role", "dialog");
  panel.setAttribute("aria-modal", "true");
  panel.tabIndex = -1;
  const backdrop = el("div", { class: "publish-backdrop", hidden: "" });
  panel.before(backdrop);
  let opener: HTMLElement | null = null;
  const isOpen = () => panel.classList.contains("is-open");
  const openers = Array.from(form.querySelectorAll<HTMLElement>("[data-publish-open]"));
  const open = (from?: HTMLElement) => {
    // 닫으면 포커스를 돌려줄 곳. 주소(#publish)로 바로 열었으면 발행 단추로.
    const active = document.activeElement;
    opener = from ?? (active instanceof HTMLElement && active !== document.body ? active : (openers[0] ?? null));
    panel.classList.add("is-open");
    backdrop.hidden = false;
    document.documentElement.classList.add("publish-lock");
    refresh();
    // 서랍이 미끄러져 들어온 뒤 첫 칸으로
    window.setTimeout(() => (category ?? slug ?? panel).focus(), 50);
  };
  const close = () => {
    if (!isOpen()) return;
    panel.classList.remove("is-open");
    backdrop.hidden = true;
    document.documentElement.classList.remove("publish-lock");
    opener?.focus();
  };
  for (const b of openers) {
    b.addEventListener("click", (e) => {
      e.preventDefault();
      open(b);
    });
  }
  $<HTMLElement>("[data-publish-close]")?.addEventListener("click", close);
  backdrop.addEventListener("click", close);
  panel.addEventListener("keydown", (e) => {
    if (e.key === "Escape") {
      e.preventDefault();
      close();
    }
    // 서랍 안에서만 Tab 이 돈다
    if (e.key === "Tab") {
      const items = Array.from(panel.querySelectorAll<HTMLElement>("a[href], button:not([disabled]), input:not([disabled]):not([type=hidden]), textarea, select")).filter((x) => x.offsetParent !== null);
      if (!items.length) return;
      const first = items[0];
      const last = items[items.length - 1];
      if (e.shiftKey && document.activeElement === first) {
        e.preventDefault();
        last.focus();
      } else if (!e.shiftKey && document.activeElement === last) {
        e.preventDefault();
        first.focus();
      }
    }
  });
  // 제목을 비운 채 발행을 누르면 브라우저가 제목 칸을 가리킨다. 서랍이 가리지 않게 닫는다.
  title?.addEventListener("invalid", close);

  // ---- 발행 시점: 지금 / 예약 ----
  const whenRadios = Array.from(panel.querySelectorAll<HTMLInputElement>("[data-when]"));
  const whenAt = $<HTMLElement>("[data-when-at]");
  const at = $<HTMLInputElement>("#scheduled_at");
  const nowBtn = $<HTMLButtonElement>("[data-when-now]");
  const laterBtn = $<HTMLButtonElement>("[data-when-later]");
  const syncWhen = () => {
    const later = whenRadios.some((r) => r.checked && r.value === "schedule");
    if (whenAt) whenAt.hidden = !later;
    if (nowBtn) nowBtn.hidden = later;
    if (laterBtn) laterBtn.hidden = !later;
    if (at) at.required = later;
  };
  for (const r of whenRadios) r.addEventListener("change", () => {
    syncWhen();
    if (r.checked && r.value === "schedule") at?.focus();
  });
  if (whenRadios.length) syncWhen();

  // ---- 공개 범위: 비공개면 발행하지 않으므로 예약을 끄고 단추 이름을 바꾼다 ----
  const visRadios = Array.from(panel.querySelectorAll<HTMLInputElement>("[data-visibility]"));
  const saveBtn = $<HTMLButtonElement>("[data-save]");
  const labels = new Map<HTMLElement, string>();
  for (const b of [nowBtn, saveBtn]) if (b) labels.set(b, b.textContent ?? "");
  const syncVisibility = () => {
    const priv = visRadios.some((r) => r.checked && r.value === "private");
    for (const [b, text] of labels) b.textContent = priv ? "비공개로 저장" : text;
    for (const r of whenRadios) {
      r.disabled = priv && r.value === "schedule";
      if (priv && r.value === "now") r.checked = true;
    }
    if (whenRadios.length) syncWhen();
  };
  for (const r of visRadios) r.addEventListener("change", syncVisibility);
  syncVisibility();

  // ---- 분류 ----
  const syncCategories = () => {
    if (!category) return;
    const b = board();
    for (const o of Array.from(category.options)) {
      if (!o.dataset.board) continue;
      const mine = o.dataset.board === b;
      o.hidden = !mine;
      o.disabled = !mine;
      if (!mine && o.selected) category.value = "";
    }
  };
  boardSelect?.addEventListener("change", syncCategories);
  syncCategories();
  const addCategory = $<HTMLButtonElement>("[data-category-add]");
  if (addCategory && category) {
    addCategory.hidden = false;
    addCategory.addEventListener("click", async () => {
      const name = window.prompt("새 분류 이름")?.trim();
      if (!name) return;
      addCategory.disabled = true;
      try {
        const c = await getJson<{ slug: string; name: string }>("/admin/categories", csrf, {
          method: "POST",
          headers: { "Content-Type": "application/json" },
          body: JSON.stringify({ board: board(), name }),
        });
        // 같은 주소가 이미 있으면 서버가 그 분류를 돌려준다
        const found = Array.from(category.options).find((o) => o.value === c.slug && o.dataset.board === board());
        if (!found) category.append(el("option", { value: c.slug, "data-board": board() }, c.name));
        category.value = c.slug;
      } catch (e) {
        window.alert((e as Error).message);
      } finally {
        addCategory.disabled = false;
        category.focus();
      }
    });
  }

  // ---- 태그 ----
  const tagsField = $<HTMLInputElement>("[data-tags]");
  if (tagsField) bindTags(tagsField, $<HTMLElement>("[data-tags-info]"), panel.querySelector<HTMLLabelElement>("label[for=tags]"));

  // ---- 주소 ----
  let slugTimer = 0;
  let slugSeq = 0;
  let path = "";
  const info = (target: HTMLElement | null, parts: (string | Node)[], tone: "good" | "bad" | "" = "") => {
    if (!target) return;
    target.replaceChildren(...parts);
    target.classList.toggle("is-good", tone === "good");
    target.classList.toggle("is-bad", tone === "bad");
  };
  const checkSlug = async () => {
    if (!slug || !board()) return;
    const seq = ++slugSeq;
    const q = new URLSearchParams({ board: board() });
    if (slug.value.trim()) q.set("slug", slug.value);
    if (title?.value.trim()) q.set("title", title.value);
    if (postId) q.set("post_id", postId);
    try {
      const r = await getJson<SlugCheck>(`/admin/slug-check?${q}`, csrf);
      if (seq !== slugSeq) return;
      path = r.path;
      const url = el("code", {}, readable(`${siteUrl}${r.path}`));
      if (r.derived) {
        info(slugInfo, ["비우면 제목에서 만듭니다: ", url]);
      } else if (r.available) {
        info(slugInfo, ["✓ 쓸 수 있는 주소입니다: ", url], "good");
      } else {
        const parts: (string | Node)[] = ["이미 다른 글이 쓰는 주소입니다. "];
        if (r.suggestion) {
          const next = r.suggestion;
          const use = el("button", { type: "button", class: "link" }, `‘${next}’ 쓰기`);
          use.addEventListener("click", () => {
            slug.value = next;
            void checkSlug();
            slug.focus();
          });
          parts.push(use);
        }
        info(slugInfo, parts, "bad");
      }
    } catch (e) {
      if (seq !== slugSeq) return;
      path = "";
      info(slugInfo, [(e as Error).message], "bad");
    }
    paintShare();
  };
  const scheduleSlug = () => {
    window.clearTimeout(slugTimer);
    slugTimer = window.setTimeout(() => void checkSlug(), 300);
  };
  slug?.addEventListener("input", scheduleSlug);
  boardSelect?.addEventListener("change", () => isOpen() && scheduleSlug());
  title?.addEventListener("input", () => {
    if (!isOpen()) return;
    if (slug && !slug.value.trim()) scheduleSlug();
    paintShare();
  });

  // ---- 요약 ----
  let summary = "";
  const paintExcerpt = () => {
    if (!excerpt) return;
    excerpt.placeholder = summary ? `비우면 이렇게 보입니다: ${summary}` : "비우면 본문 앞부분을 씁니다";
    const len = excerpt.value.trim().length;
    const parts: (string | Node)[] = [];
    let tone: "good" | "bad" | "" = "";
    if (len) {
      parts.push(`${len}자 · ${SUMMARY_MIN}~${SUMMARY_MAX}자를 권합니다. `);
      tone = len >= SUMMARY_MIN && len <= SUMMARY_MAX ? "good" : "";
    } else {
      parts.push("비워 두면 본문 앞부분이 목록·검색·공유 미리보기에 보입니다. ");
    }
    if (summary) {
      const take = el("button", { type: "button", class: "link" }, "본문에서 가져오기");
      take.addEventListener("click", () => {
        excerpt.value = summary.replace(/…$/, "");
        excerpt.dispatchEvent(new Event("input"));
        excerpt.focus();
      });
      parts.push(take);
    }
    info(excerptInfo, parts, tone);
  };
  excerpt?.addEventListener("input", () => {
    paintExcerpt();
    paintShare();
  });

  // ---- 대표 이미지 ----
  let images: string[] = [];
  const coverFile = el("input", { type: "file", accept: "image/jpeg,image/png,image/gif,image/webp", hidden: "" });
  panel.append(coverFile);
  const paintCover = () => {
    if (!pick || !cover) return;
    const cur = cover.value.trim();
    const choices = [...images];
    if (cur && !choices.includes(cur)) choices.unshift(cur);
    const auto = el("button", { type: "button", "aria-pressed": String(!cur), title: "본문의 첫 사진을 씁니다" }, images.length ? "자동\n(첫 사진)" : "없음");
    auto.addEventListener("click", () => setCover(""));
    const items: HTMLElement[] = [auto];
    for (const url of choices) {
      const b = el("button", { type: "button", "aria-pressed": String(url === cur), "aria-label": `대표 이미지로: ${url}` });
      b.append(el("img", { src: url, alt: "", loading: "lazy" }));
      b.addEventListener("click", () => setCover(url));
      items.push(b);
    }
    const up = el("button", { type: "button", class: "cover-upload", title: "새 사진을 올려 대표 이미지로 씁니다" }, "+ 올리기");
    up.addEventListener("click", () => coverFile.click());
    items.push(up);
    pick.replaceChildren(...items);
    pick.hidden = false;
  };
  const setCover = (url: string) => {
    if (!cover) return;
    cover.value = url;
    paintCover();
    paintShare();
  };
  coverFile.addEventListener("change", async () => {
    const f = coverFile.files?.[0];
    coverFile.value = "";
    if (!f) return;
    const label = pick?.querySelector<HTMLButtonElement>(".cover-upload");
    if (label) {
      label.disabled = true;
      label.textContent = "올리는 중…";
    }
    try {
      const m = await uploadFile(f, csrf);
      setCover(m.url);
    } catch (e) {
      paintCover();
      window.alert((e as Error).message);
    }
  });
  cover?.addEventListener("input", () => {
    paintCover();
    paintShare();
  });

  // ---- 공유 미리보기 ----
  const paintShare = () => {
    if (!share) return;
    const img = share.querySelector<HTMLElement>("[data-share-img]");
    const t = share.querySelector<HTMLElement>("[data-share-title]");
    const d = share.querySelector<HTMLElement>("[data-share-desc]");
    const u = share.querySelector<HTMLElement>("[data-share-url]");
    const pic = cover?.value.trim() || images[0] || "";
    if (img) {
      img.replaceChildren();
      img.classList.toggle("has-img", !!pic);
      img.style.backgroundImage = pic ? `url("${encodeURI(pic)}")` : "";
    }
    if (t) t.textContent = title?.value.trim() || "제목 없음";
    if (d) d.textContent = excerpt?.value.trim() || summary || "본문이 비어 있습니다.";
    if (u) u.textContent = readable(path ? `${siteUrl}${path}` : siteUrl);
    share.hidden = false;
  };

  // ---- 열 때마다 지금 본문으로 다시 계산 ----
  let previewSeq = 0;
  const refresh = () => {
    void checkSlug();
    paintExcerpt();
    paintCover();
    paintShare();
    const seq = ++previewSeq;
    getJson<PreviewData>("/admin/preview", csrf, {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ body_md: markdown() }),
    }).then(
      (p) => {
        if (seq !== previewSeq) return;
        summary = p.summary;
        images = p.images;
        paintExcerpt();
        paintCover();
        paintShare();
      },
      () => {
        // 미리보기를 못 받아도 값은 직접 쓸 수 있다
      },
    );
  };

  if (location.hash === "#publish") open();
}

/** 태그 칸을 칩 상자로 바꾼다. 원래 칸은 숨겨 두고 제출할 값만 담는다. */
function bindTags(field: HTMLInputElement, hint: HTMLElement | null, label: HTMLLabelElement | null) {
  let tags = splitTags(field.value);
  const box = el("div", { class: "tag-box" });
  const typing = el("input", { type: "text", id: "tags-typing", autocomplete: "off", spellcheck: "false", enterkeyhint: "done" });
  box.append(typing);
  field.type = "hidden";
  field.after(box);
  if (label) label.htmlFor = typing.id;
  let warning = "";

  const paint = () => {
    const chips = tags.map((t, i) => {
      const chip = el("span", { class: "tag-chip" }, `#${t}`);
      const x = el("button", { type: "button", "aria-label": `${t} 태그 빼기` }, "×");
      x.addEventListener("click", () => {
        tags.splice(i, 1);
        sync();
        typing.focus();
      });
      chip.append(x);
      return chip;
    });
    // 입력칸은 떼지 않는다(떼었다 붙이면 쓰던 중에 포커스가 빠진다). 칩만 바꾼다.
    for (const old of Array.from(box.querySelectorAll(".tag-chip"))) old.remove();
    for (const chip of chips) box.insertBefore(chip, typing);
    const full = tags.length >= TAG_MAX;
    typing.placeholder = full ? "더 넣을 수 없습니다" : tags.length ? "" : "태그 입력 후 Enter";
    typing.readOnly = full;
    if (hint) {
      hint.textContent = warning || `${tags.length}/${TAG_MAX}개 · Enter 나 쉼표로 넣고 Backspace 로 뺍니다.`;
      hint.classList.toggle("is-bad", !!warning);
    }
  };
  const sync = () => {
    field.value = tags.join(", ");
    paint();
  };
  const add = (text: string) => {
    warning = "";
    for (const t of splitTags(text)) {
      if (tags.some((x) => tagKey(x) === tagKey(t))) continue;
      if ([...t].length > TAG_CHARS) {
        warning = `‘${t.slice(0, 12)}…’ 는 ${TAG_CHARS}자를 넘어 넣지 않았습니다.`;
        continue;
      }
      if (tags.length >= TAG_MAX) {
        warning = `태그는 ${TAG_MAX}개까지입니다.`;
        break;
      }
      tags.push(t);
    }
    typing.value = "";
    sync();
  };

  // 한글 조합 중 Enter 는 글자를 확정할 뿐이다. 확정된 뒤(compositionend)에 칩으로 만든다.
  let addAfterCompose = false;
  typing.addEventListener("keydown", (e) => {
    if (e.key === "Enter" || e.key === ",") {
      // 조합 중에는 막지 않는다(막으면 확정이 안 되는 브라우저가 있다). 조합 중 Enter 는 폼을 제출하지 않는다.
      if (e.isComposing) {
        addAfterCompose = e.key === "Enter";
        return;
      }
      // Enter 가 폼을 제출(임시저장)하지 않게 한다
      e.preventDefault();
      if (typing.value.trim()) add(typing.value);
    } else if (e.key === "Backspace" && !typing.value && tags.length && !e.isComposing) {
      tags = tags.slice(0, -1);
      warning = "";
      sync();
    }
  });
  typing.addEventListener("compositionend", () => {
    if (!addAfterCompose) return;
    addAfterCompose = false;
    window.setTimeout(() => typing.value.trim() && add(typing.value), 0);
  });
  // 붙여 넣기 등으로 쉼표가 들어오면 바로 나눈다
  typing.addEventListener("input", (e) => {
    if (!(e as InputEvent).isComposing && typing.value.includes(",")) add(typing.value);
  });
  typing.addEventListener("blur", () => typing.value.trim() && add(typing.value));
  box.addEventListener("click", (e) => e.target === box && typing.focus());
  // 칩이 되기 전에 저장을 눌러도 쓰던 글자를 넣는다
  field.form?.addEventListener("submit", () => typing.value.trim() && add(typing.value));
  paint();
}
