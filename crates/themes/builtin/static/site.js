// Maru 내장 테마 스크립트 (ADR-0060). 팝업과 최근 본 게시판(ADR-0096), 로그인 상자(ADR-0100).
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

// 로그인 상자 위젯 (GAP Q15, ADR-0100). HTML 은 손님 모양으로 모두에게 같고(캐시), 로그인했으면
// /me/bar 로 이름·알림·포인트·로그아웃으로 바꾼다. 막대 스크립트와 같은 요청을 함께 쓴다(window.maruBar).
(() => {
  const boxes = document.querySelectorAll("[data-account-box]");
  if (!boxes.length || !window.fetch) return;
  const next = location.pathname + location.search;
  boxes.forEach((box) => {
    const a = box.querySelector(".account-box-login");
    if (a && next !== "/") a.href = a.getAttribute("href") + "?next=" + encodeURIComponent(next);
  });
  const url = boxes[0].dataset.accountBox;
  if (!/^\/(?!\/)/.test(url)) return;
  const bar = window.maruBar || (window.maruBar = fetch(url, { credentials: "same-origin", headers: { "Accept": "application/json" } })
    .then((r) => (r.ok ? r.json() : null))
    .catch(() => null));
  bar.then((b) => {
    if (!b || !b.signed_in) return;
    const items = Array.isArray(b.items) ? b.items : [];
    boxes.forEach((box) => {
      const guest = box.querySelector(".account-box-guest");
      if (!guest) return;
      const me = document.createElement("div");
      me.className = "account-box-member";
      const hello = document.createElement("p");
      hello.className = "account-box-hello";
      const name = document.createElement("strong");
      name.textContent = b.name;
      hello.append(name, "님, 반갑습니다.");
      me.append(hello);
      for (const i of items) {
        if (!i.note) continue;
        const n = document.createElement("p");
        n.className = "account-box-note";
        n.textContent = i.note;
        me.append(n);
      }
      const ul = document.createElement("ul");
      ul.className = "account-box-mine";
      const add = (label, href, count) => {
        if (typeof href !== "string" || !/^\/(?!\/)/.test(href)) return;
        const li = document.createElement("li");
        const a = document.createElement("a");
        a.href = href;
        a.textContent = label;
        if (count > 0) {
          const c = document.createElement("span");
          c.className = "account-box-count";
          c.textContent = count > 99 ? "99+" : String(count);
          c.setAttribute("aria-label", "읽지 않은 " + count + "개");
          a.append(" ", c);
        }
        li.append(a);
        ul.append(li);
      };
      add("내 정보", b.me_url);
      for (const i of items) if (!i.menu) add(i.label, i.url, i.count);
      for (const i of items) if (i.menu) add(i.label, i.url);
      me.append(ul);
      if (b.logout && typeof b.logout.url === "string") {
        const f = document.createElement("form");
        f.method = "post";
        f.action = b.logout.url;
        const t = document.createElement("input");
        t.type = "hidden";
        t.name = "_csrf";
        t.value = b.logout.csrf;
        const s = document.createElement("button");
        s.type = "submit";
        s.className = "account-box-logout";
        s.textContent = "로그아웃";
        f.append(t, s);
        me.append(f);
      }
      guest.replaceWith(me);
    });
  });
})();
