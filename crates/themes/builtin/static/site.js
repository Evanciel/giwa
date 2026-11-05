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

// 차단한 회원의 글·댓글 접기 (GAP Q18, ADR-0103). 공개 HTML 은 모두에게 같아(캐시) 누구를 접을지는
// 로그인한 사람의 /me/bar(hidden_members)를 보고 여기서 정한다. 이름 표지(data-member)가 있는 화면만 묻는다.
(() => {
  const hosts = document.querySelectorAll("[data-member]");
  if (!hosts.length || !window.fetch) return;
  const bar = window.maruBar || (window.maruBar = fetch("/me/bar", { credentials: "same-origin", headers: { "Accept": "application/json" } })
    .then((r) => (r.ok ? r.json() : null))
    .catch(() => null));
  const fold = (parts, before, where, text) => {
    if (!parts.length || where.querySelector(":scope > .member-fold")) return;
    const note = document.createElement("p");
    note.className = "member-fold";
    const show = document.createElement("button");
    show.type = "button";
    show.textContent = "보기";
    note.append(text + " ", show);
    parts.forEach((p) => p.classList.add("member-folded"));
    where.insertBefore(note, before);
    show.addEventListener("click", () => {
      parts.forEach((p) => p.classList.remove("member-folded"));
      note.remove();
    });
  };
  bar.then((b) => {
    const names = new Set(b && b.signed_in && Array.isArray(b.hidden_members) ? b.hidden_members : []);
    if (!names.size) return;
    hosts.forEach((host) => {
      if (!names.has(host.getAttribute("data-member"))) return;
      const row = host.closest(".comment, .post-row, .post-list > li, .post-grid > li");
      if (row) {
        const parts = Array.from(row.children).filter((c) => !c.classList.contains("member-fold"));
        fold(parts, row.firstChild, row, row.classList.contains("comment") ? "차단한 회원의 댓글입니다." : "차단한 회원의 글입니다.");
        return;
      }
      // 글 화면: 머리(제목·글쓴이)는 두고 본문 쪽만 접는다.
      const article = host.closest("article.post");
      if (!article || !host.closest("article.post > header")) return;
      const parts = Array.from(article.querySelectorAll(":scope > .post-fields, :scope > .prose, :scope > .tags, :scope > .post-signature"));
      if (parts.length) fold(parts, parts[0], article, "차단한 회원의 글입니다.");
    });
  });
})();

// 갤러리 빠른 보기와 공유 (GAP Q20, ADR-0105). 갤러리 칸을 누르면 목록을 떠나지 않고 글을 대화 상자로 연다.
// 주소에 ?quick=<글 주소 이름> 이 붙어 뒤로 가기로 닫히고, 그 주소로 들어와도 열린다. 새 탭·수정 키는 그대로.
(() => {
  const grid = document.querySelector(".post-grid-gallery");
  if (!grid || !window.fetch || !window.DOMParser || typeof HTMLDialogElement !== "function") return;
  let dlg = null;
  let pushed = false;
  const close = () => { if (dlg && dlg.open) dlg.close(); };
  const slugOf = (href) => decodeURIComponent(new URL(href, location.href).pathname.split("/").pop() || "");
  const open = (href, push) => {
    if (!dlg) {
      dlg = document.createElement("dialog");
      dlg.className = "quick-view";
      dlg.setAttribute("aria-label", "빠른 보기");
      const bar = document.createElement("div");
      bar.className = "quick-view-bar";
      const shut = document.createElement("button");
      shut.type = "button";
      shut.textContent = "닫기";
      shut.addEventListener("click", close);
      const full = document.createElement("a");
      full.textContent = "글 화면으로";
      const body = document.createElement("div");
      body.className = "quick-view-body";
      bar.append(shut, full);
      dlg.append(bar, body);
      document.body.append(dlg);
      dlg.addEventListener("click", (ev) => { if (ev.target === dlg) close(); });
      dlg.addEventListener("close", () => {
        if (!new URLSearchParams(location.search).has("quick")) return;
        if (pushed) { history.back(); return; }
        const u = new URL(location.href);
        u.searchParams.delete("quick");
        history.replaceState(null, "", u);
      });
    }
    const body = dlg.querySelector(".quick-view-body");
    dlg.querySelector(".quick-view-bar a").href = href;
    body.textContent = "불러오는 중…";
    pushed = push;
    if (push) {
      const u = new URL(location.href);
      u.searchParams.set("quick", slugOf(href));
      history.pushState({ quick: href }, "", u);
    }
    if (!dlg.open) dlg.showModal();
    fetch(href, { credentials: "same-origin" })
      .then((r) => (r.ok ? r.text() : Promise.reject(r.status)))
      .then((html) => {
        // 같은 사이트가 그린 글 화면에서 글 부분만 옮긴다(본문은 서버가 정화한 HTML).
        const doc = new DOMParser().parseFromString(html, "text/html");
        const art = doc.querySelector("article.post");
        body.textContent = "";
        if (art) body.append(document.importNode(art, true));
        else location.href = href;
      })
      .catch(() => { location.href = href; });
  };
  grid.addEventListener("click", (ev) => {
    const a = ev.target.closest && ev.target.closest("a.tile, h2 a");
    if (!a || ev.button !== 0 || ev.ctrlKey || ev.metaKey || ev.shiftKey || ev.altKey) return;
    ev.preventDefault();
    open(a.href, true);
  });
  window.addEventListener("popstate", () => {
    const q = new URLSearchParams(location.search).get("quick");
    if (!q) { if (dlg && dlg.open) { const d = dlg; dlg = null; d.close(); d.remove(); } return; }
  });
  const q = new URLSearchParams(location.search).get("quick");
  if (q) {
    const a = Array.from(grid.querySelectorAll("h2 a")).find((x) => slugOf(x.href) === q);
    if (a) open(a.href, false);
  }
})();

(() => {
  const b = document.querySelector("[data-share]");
  if (!b) return;
  const canShare = typeof navigator.share === "function";
  if (!canShare && !(navigator.clipboard && navigator.clipboard.writeText)) return;
  b.hidden = false;
  b.addEventListener("click", () => {
    const url = (document.querySelector("link[rel=canonical]") || location).href;
    if (canShare) {
      navigator.share({ title: document.title, url }).catch(() => {});
      return;
    }
    navigator.clipboard.writeText(url).then(() => {
      b.textContent = "복사됨";
      setTimeout(() => { b.textContent = "공유"; }, 1500);
    }, () => {});
  });
})();
