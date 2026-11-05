// Maru 내장 테마 스크립트 (ADR-0060). 팝업과 최근 본 게시판(ADR-0096).
// 「N일 보지 않기」 기한은 이 브라우저의 localStorage 에만 둔다. 저장소를 못 쓰면 닫기만 된다.
(() => {
  const PREFIX = "maru-popup:";
  const store = {
    get(k) { try { return localStorage.getItem(PREFIX + k); } catch { return null; } },
    set(k, v) { try { localStorage.setItem(PREFIX + k, v); } catch { /* 저장 못 함: 닫기만 */ } },
  };
  const popups = [...document.querySelectorAll("[data-popup]")];
  const before = document.activeElement;
  const open = popups.filter((el) => {
    const until = Number(store.get(el.dataset.popup) || 0);
    return !(until && Date.now() < until);
  });
  if (!open.length) return;
  const close = (el) => {
    el.hidden = true;
    const rest = open.filter((p) => !p.hidden);
    if (rest.length) {
      rest[rest.length - 1].querySelector("button")?.focus();
    } else if (before && before.focus) {
      before.focus();
    }
  };
  for (const el of open) {
    el.hidden = false;
    el.querySelector("[data-popup-close]")?.addEventListener("click", () => close(el));
    el.querySelector("[data-popup-dismiss]")?.addEventListener("click", () => {
      const days = Number(el.dataset.dismissDays || 0);
      store.set(el.dataset.popup, String(Date.now() + days * 86400000));
      close(el);
    });
  }
  open[open.length - 1].querySelector("button")?.focus();
  document.addEventListener("keydown", (e) => {
    if (e.key !== "Escape") return;
    const top = open.filter((p) => !p.hidden).pop();
    if (top) close(top);
  });
})();

// 최근 본 게시판 (GAP Q11, ADR-0096). 게시판 화면의 [data-visit] 을 이 브라우저 localStorage 에 쌓고
// 머리글 아래 [data-recent-boards] 에 링크로 보인다. 서버에는 아무것도 보내지 않는다.
(() => {
  const KEY = "maru.recentBoards";
  const MAX = 8;
  const load = () => {
    try {
      const v = JSON.parse(localStorage.getItem(KEY) || "[]");
      return Array.isArray(v) ? v : [];
    } catch { return []; }
  };
  const save = (list) => {
    try {
      if (list.length) localStorage.setItem(KEY, JSON.stringify(list));
      else localStorage.removeItem(KEY);
    } catch { /* 저장 못 함: 이번 화면만 */ }
  };
  // 이 사이트 안 주소(`/` 로 시작, `//` 아님)만 받는다.
  const ok = (b) => b && typeof b.url === "string" && /^\/(?!\/)/.test(b.url)
    && typeof b.name === "string" && b.name.trim() !== "";
  let list = load().filter(ok);
  const here = document.querySelector("[data-visit]");
  const current = here && { url: here.dataset.visit, name: (here.dataset.visitName || "").slice(0, 40) };
  if (current && ok(current)) {
    list = [current, ...list.filter((b) => b.url !== current.url)].slice(0, MAX);
    save(list);
  }
  const nav = document.querySelector("[data-recent-boards]");
  if (!nav || !list.length) return;
  const box = nav.firstElementChild || nav;
  const label = document.createElement("span");
  label.className = "recent-boards-label";
  label.textContent = "최근 본 게시판";
  const ul = document.createElement("ul");
  for (const b of list) {
    const li = document.createElement("li");
    const a = document.createElement("a");
    a.href = b.url;
    a.textContent = b.name.slice(0, 40);
    if (current && b.url === current.url) a.setAttribute("aria-current", "page");
    li.append(a);
    ul.append(li);
  }
  const clear = document.createElement("button");
  clear.type = "button";
  clear.className = "recent-boards-clear";
  clear.textContent = "지우기";
  clear.addEventListener("click", () => {
    save([]);
    nav.hidden = true;
  });
  box.append(label, ul, clear);
  nav.hidden = false;
})();
