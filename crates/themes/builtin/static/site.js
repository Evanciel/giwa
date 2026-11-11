// Maru 내장 테마 스크립트 (ADR-0060). 팝업과 최근 본 게시판(ADR-0096), 로그인 상자(ADR-0100).
// 「N일 보지 않기」 기한은 이 브라우저의 localStorage 에만 둔다. 저장소를 못 쓰면 닫기만 된다.
// 아래 이름(MARU_EN, T)이 전역으로 새지 않게 파일 전체를 블록으로 감싼다. 사용자 테마 스크립트와 겹치지 않는다.
{
// 영어 사이트 문구 (ADR-0134). `<html lang>` 을 보고, 표에 없으면 한국어 그대로. {n} 은 값 자리.
const MARU_EN = document.documentElement.lang.startsWith("en") ? {
  "{n} 지우기": "Remove {n}",
  "공유": "Share",
  "글 화면으로": "Open post",
  "내 정보": "My account",
  "다음": "Next",
  "닫기": "Close",
  "복사": "Copy",
  "코드 복사": "Copy code",
  "복사했습니다": "Copied",
  "Ctrl+C 로 복사하세요": "Press Ctrl+C to copy",
  "로그아웃": "Log out",
  "보기": "Show",
  "복사됨": "Copied",
  "불러오는 중…": "Loading…",
  "빠른 보기": "Quick view",
  "사진 크게 보기": "Enlarge photo",
  "{n}일": "{n}d",
  "{n}시간": "{n}h",
  "{n}분 남았습니다": "{n}m left",
  "이전": "Previous",
  "읽지 않은 {n}개": "{n} unread",
  "자동 넘김 다시": "Resume slideshow",
  "자동 넘김 멈춤": "Pause slideshow",
  "접기": "Less",
  "즐겨찾기": "Favorites",
  "즐겨찾기에 넣기": "Add to favorites",
  "즐겨찾기에서 빼기": "Remove from favorites",
  "지우기": "Clear",
  "차단한 회원의 글입니다.": "Post from a blocked member.",
  "차단한 회원의 댓글입니다.": "Comment from a blocked member.",
  "최근 본 게시판": "Recently viewed",
  "펼치기": "More",
} : null;
const T = (s, n) => ((MARU_EN && MARU_EN[s]) || s).replace("{n}", n);

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
  label.textContent = T("최근 본 게시판");
  const ul = document.createElement("ul");
  const clear = document.createElement("button");
  const more = document.createElement("button");
  const drop = () => {
    label.remove();
    ul.remove();
    more.remove();
    clear.remove();
    if (!box.children.length) nav.hidden = true;
  };
  // 칩 하나씩 지우기 (GAP Q27, ADR-0111): 지운 칩 자리의 다음 칩(없으면 앞 칩)으로 초점을 옮긴다.
  for (const b of list) {
    const li = document.createElement("li");
    const a = document.createElement("a");
    a.href = b.url;
    a.textContent = b.name.slice(0, 40);
    if (current && b.url === current.url) a.setAttribute("aria-current", "page");
    const x = document.createElement("button");
    x.type = "button";
    x.className = "recent-board-drop";
    x.setAttribute("aria-label", T("{n} 지우기", b.name.slice(0, 40)));
    x.textContent = "×";
    x.addEventListener("click", () => {
      list = list.filter((v) => v.url !== b.url);
      save(list);
      const next = li.nextElementSibling || li.previousElementSibling;
      li.remove();
      if (!list.length) return drop();
      const focus = next && next.querySelector(".recent-board-drop");
      if (focus) focus.focus();
      fit();
    });
    li.append(a, x);
    ul.append(li);
  }
  // 넘치면 「펼치기」로 여러 줄로 다 보인다.
  more.type = "button";
  more.className = "recent-boards-more";
  more.setAttribute("aria-expanded", "false");
  more.textContent = T("펼치기");
  more.hidden = true;
  more.addEventListener("click", () => {
    const open = !nav.classList.contains("is-open");
    nav.classList.toggle("is-open", open);
    more.setAttribute("aria-expanded", open ? "true" : "false");
    more.textContent = open ? T("접기") : T("펼치기");
  });
  const fit = () => {
    if (!nav.classList.contains("is-open")) more.hidden = ul.scrollWidth <= ul.clientWidth + 1;
  };
  clear.type = "button";
  clear.className = "recent-boards-clear";
  clear.textContent = T("지우기");
  clear.addEventListener("click", () => {
    list = [];
    save([]);
    drop();
  });
  box.append(label, ul, more, clear);
  nav.hidden = false;
  fit();
  addEventListener("resize", fit, { passive: true });
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
      hello.append(...(MARU_EN ? ["Welcome, ", name, "."] : [name, "님, 반갑습니다."]));
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
          c.setAttribute("aria-label", T("읽지 않은 {n}개", count));
          a.append(" ", c);
        }
        li.append(a);
        ul.append(li);
      };
      add(T("내 정보"), b.me_url);
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
        s.textContent = T("로그아웃");
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
    show.textContent = T("보기");
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
        fold(parts, row.firstChild, row, row.classList.contains("comment") ? T("차단한 회원의 댓글입니다.") : T("차단한 회원의 글입니다."));
        return;
      }
      // 글 화면: 머리(제목·글쓴이)는 두고 본문 쪽만 접는다.
      const article = host.closest("article.post");
      if (!article || !host.closest("article.post > header")) return;
      const parts = Array.from(article.querySelectorAll(":scope > .post-fields, :scope > .prose, :scope > .tags, :scope > .post-signature"));
      if (parts.length) fold(parts, parts[0], article, T("차단한 회원의 글입니다."));
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
      dlg.setAttribute("aria-label", T("빠른 보기"));
      const bar = document.createElement("div");
      bar.className = "quick-view-bar";
      const shut = document.createElement("button");
      shut.type = "button";
      shut.textContent = T("닫기");
      shut.addEventListener("click", close);
      const full = document.createElement("a");
      full.textContent = T("글 화면으로");
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
    body.textContent = T("불러오는 중…");
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
  const buttons = document.querySelectorAll("[data-share]");
  if (!buttons.length) return;
  const canShare = typeof navigator.share === "function";
  if (!canShare && !(navigator.clipboard && navigator.clipboard.writeText)) return;
  for (const b of buttons) {
    b.hidden = false;
    // 글 머리 아이콘(GAP Q30)은 글자 대신 이름표로 「복사됨」을 알린다.
    const icon = b.classList.contains("head-share");
    const say = (text) => {
      if (icon) {
        b.setAttribute("aria-label", text);
        b.title = text;
      } else b.textContent = text;
    };
    b.addEventListener("click", () => {
      const url = (document.querySelector("link[rel=canonical]") || location).href;
      if (canShare) {
        navigator.share({ title: document.title, url }).catch(() => {});
        return;
      }
      navigator.clipboard.writeText(url).then(() => {
        say(T("복사됨"));
        setTimeout(() => say(T("공유")), 1500);
      }, () => {});
    });
  }
})();

// 자기소개 펼치기 (GAP Q30, ADR-0113): 세 줄이 넘으면 접고 「펼치기」. 스크립트가 없으면 다 보인다.
(() => {
  for (const box of document.querySelectorAll("[data-clamp]")) {
    box.classList.add("is-clamped");
    const text = box.firstElementChild;
    if (!text || text.scrollHeight <= text.clientHeight + 1) {
      box.classList.remove("is-clamped");
      continue;
    }
    const more = document.createElement("button");
    more.type = "button";
    more.className = "clamp-toggle";
    more.textContent = T("펼치기");
    more.setAttribute("aria-expanded", "false");
    more.addEventListener("click", () => {
      const open = box.classList.toggle("is-clamped") === false;
      more.textContent = open ? T("접기") : T("펼치기");
      more.setAttribute("aria-expanded", open ? "true" : "false");
    });
    box.append(more);
  }
})();

// 게시판 즐겨찾기 (GAP Q26, ADR-0108). 로그인한 회원의 /me/bar 에 board_star 가 있으면(스크랩 모듈)
// 게시판 머리에 별 단추를, 머리글 아래 줄 맨 앞에 즐겨찾는 게시판을 보인다. 공개 HTML 은 모두에게 같다.
(() => {
  if (!window.fetch) return;
  const head = document.querySelector("[data-board]");
  const nav = document.querySelector("[data-recent-boards]");
  if (!head && !nav) return;
  const bar = window.maruBar || (window.maruBar = fetch("/me/bar", { credentials: "same-origin", headers: { "Accept": "application/json" } })
    .then((r) => (r.ok ? r.json() : null))
    .catch(() => null));
  const inside = (u) => typeof u === "string" && /^\/(?!\/)/.test(u);
  bar.then((me) => {
    if (!me || !me.signed_in || !inside(me.board_star)) return;
    let favs = (Array.isArray(me.favorite_boards) ? me.favorite_boards : [])
      .filter((b) => b && inside(b.url) && typeof b.name === "string" && b.name.trim() !== "");
    let group = null;
    const draw = () => {
      if (!nav) return;
      const box = nav.firstElementChild || nav;
      if (group) group.remove();
      group = null;
      if (!favs.length) {
        if (!box.children.length) nav.hidden = true;
        return;
      }
      group = document.createElement("div");
      group.className = "fav-boards";
      const label = document.createElement("span");
      label.className = "recent-boards-label";
      label.textContent = T("즐겨찾기");
      const ul = document.createElement("ul");
      for (const b of favs) {
        const li = document.createElement("li");
        const a = document.createElement("a");
        a.href = b.url;
        a.textContent = b.name.slice(0, 40);
        if (location.pathname === b.url) a.setAttribute("aria-current", "page");
        li.append(a);
        ul.append(li);
      }
      group.append(label, ul);
      box.prepend(group);
      nav.hidden = false;
    };
    draw();
    if (!head || !head.dataset.board) return;
    const url = head.dataset.visit;
    let on = favs.some((b) => b.url === url);
    const btn = document.createElement("button");
    btn.type = "button";
    btn.className = "board-star";
    const paint = () => {
      btn.setAttribute("aria-pressed", on ? "true" : "false");
      btn.textContent = (on ? "★" : "☆") + " " + T("즐겨찾기");
      btn.title = on ? T("즐겨찾기에서 빼기") : T("즐겨찾기에 넣기");
    };
    paint();
    btn.addEventListener("click", () => {
      btn.disabled = true;
      fetch(me.board_star + encodeURIComponent(head.dataset.board), { method: "POST", credentials: "same-origin", headers: { "Accept": "application/json" } })
        .then((r) => (r.ok ? r.json() : null))
        .then((v) => {
          if (!v || typeof v.favorite !== "boolean") return;
          on = v.favorite;
          favs = favs.filter((b) => b.url !== url);
          if (on) favs.push({ url, name: (head.dataset.visitName || head.dataset.board).slice(0, 40) });
          paint();
          draw();
        })
        .catch(() => {})
        .finally(() => { btn.disabled = false; });
    });
    const title = head.querySelector(".page-title");
    if (title) title.after(btn);
    else head.append(btn);
  });
})();

// 광고 자리 (GAP Q24, ADR-0109): 후원 띠 접기(이 브라우저에 기억), 휴대폰 아래 고정 광고 닫기(이번 방문 동안),
// 긴 화면의 맨 위·맨 아래 단추. 스크립트가 없으면 후원 띠는 펼친 채, 아래 광고는 닫기 단추 없이 둔다.
(() => {
  const keep = (store, key, value) => {
    try {
      if (value) store.setItem(key, value);
      else store.removeItem(key);
    } catch { /* 저장 못 함: 이번 화면만 */ }
  };
  const read = (store, key) => {
    try { return store.getItem(key); } catch { return null; }
  };
  const band = document.querySelector("[data-sponsor]");
  if (band) {
    const btn = band.querySelector(".sponsor-toggle");
    const items = band.querySelector(".sponsor-items");
    const KEY = "maru.sponsorFolded";
    const paint = (folded) => {
      items.hidden = folded;
      btn.setAttribute("aria-expanded", folded ? "false" : "true");
      btn.textContent = folded ? T("펼치기") : T("접기");
    };
    paint(read(localStorage, KEY) === "1");
    btn.hidden = false;
    btn.addEventListener("click", () => {
      const folded = !items.hidden;
      paint(folded);
      keep(localStorage, KEY, folded ? "1" : "");
    });
  }
  const sticky = document.querySelector("[data-sticky-ad]");
  if (sticky) {
    const KEY = "maru.stickyClosed";
    if (read(sessionStorage, KEY) === "1") {
      sticky.hidden = true;
    } else {
      document.body.classList.add("has-sticky-ad");
      const close = sticky.querySelector(".sticky-close");
      const measure = () => document.body.style.setProperty("--sticky-h", sticky.offsetHeight + "px");
      measure();
      addEventListener("resize", measure, { passive: true });
      close.hidden = false;
      close.addEventListener("click", () => {
        sticky.hidden = true;
        document.body.classList.remove("has-sticky-ad");
        keep(sessionStorage, KEY, "1");
      });
    }
  }
  const jump = document.querySelector(".page-jump");
  if (jump) {
    const check = () => { jump.hidden = document.documentElement.scrollHeight < innerHeight * 2 || scrollY < innerHeight / 2; };
    check();
    addEventListener("scroll", check, { passive: true });
    addEventListener("resize", check, { passive: true });
  }
})();

// 첫 화면 판 (GAP Q23, ADR-0110). [data-page-size] 목록은 그 수만큼 쪽으로 나누고, [data-scroller] 띠(회전 배너·카드 줄)는
// 이전·다음 단추로 넘긴다. [data-auto] 면 저절로 넘기되 멈춤 단추를 두고, 가리키거나 초점이 들어오면 쉬며,
// 움직임 줄이기 설정이면 아예 넘기지 않는다. 스크립트가 없으면 목록은 다 보이고 띠는 손으로 넘긴다.
(() => {
  const reduce = window.matchMedia && matchMedia("(prefers-reduced-motion: reduce)").matches;
  const AUTO_MS = 6000;
  const nav = (host, list, total, show, auto) => {
    let at = 0;
    const box = document.createElement("div");
    box.className = "panel-nav";
    const btn = (text, label) => {
      const b = document.createElement("button");
      b.type = "button";
      b.textContent = text;
      b.setAttribute("aria-label", label);
      return b;
    };
    const prev = btn("‹", T("이전"));
    const next = btn("›", T("다음"));
    const count = document.createElement("span");
    count.className = "panel-count";
    count.setAttribute("aria-live", "polite");
    const go = (i, fromScroll) => {
      at = (i + total) % total;
      count.textContent = at + 1 + " / " + total;
      if (!fromScroll) show(at);
    };
    prev.addEventListener("click", () => go(at - 1));
    next.addEventListener("click", () => go(at + 1));
    box.append(prev, count, next);
    let timer = null;
    let paused = false;
    let resting = false;
    if (auto && !reduce) {
      const pause = btn("❚❚", T("자동 넘김 멈춤"));
      const tick = () => {
        clearInterval(timer);
        timer = null;
        if (!paused && !resting && !document.hidden) timer = setInterval(() => go(at + 1), AUTO_MS);
      };
      pause.addEventListener("click", () => {
        paused = !paused;
        pause.textContent = paused ? "▶" : "❚❚";
        pause.setAttribute("aria-label", paused ? T("자동 넘김 다시") : T("자동 넘김 멈춤"));
        tick();
      });
      const rest = (v) => () => { resting = v; tick(); };
      host.addEventListener("mouseenter", rest(true));
      host.addEventListener("mouseleave", rest(false));
      host.addEventListener("focusin", rest(true));
      host.addEventListener("focusout", rest(false));
      document.addEventListener("visibilitychange", tick);
      box.append(pause);
      tick();
    }
    const head = host.querySelector(":scope > .widget-head");
    if (head) head.insertBefore(box, head.querySelector(".widget-more"));
    else {
      box.classList.add("panel-nav-below");
      list.after(box);
    }
    go(0);
    return go;
  };
  for (const list of document.querySelectorAll("[data-page-size]")) {
    const size = parseInt(list.dataset.pageSize, 10);
    const items = [...list.children];
    if (!(size > 0) || items.length <= size) continue;
    const total = Math.ceil(items.length / size);
    // 글 탭(GAP Q29)은 탭 칸마다 따로 넘긴다: 단추는 그 칸의 목록 아래.
    const host = list.closest(".tab-panel, .widget") || list.parentElement;
    nav(host, list, total, (p) => items.forEach((li, i) => { li.hidden = Math.floor(i / size) !== p; }), list.hasAttribute("data-auto"));
  }
  for (const strip of document.querySelectorAll("[data-scroller]")) {
    const pages = () => Math.max(1, Math.round(strip.scrollWidth / Math.max(1, strip.clientWidth)));
    const total = pages();
    if (total < 2) continue;
    const host = strip.closest(".widget") || strip.parentElement;
    const go = nav(host, strip, total, (p) => strip.scrollTo({ left: p * strip.clientWidth, behavior: reduce ? "auto" : "smooth" }), strip.hasAttribute("data-auto"));
    let wait = null;
    strip.addEventListener("scroll", () => {
      clearTimeout(wait);
      wait = setTimeout(() => go(Math.round(strip.scrollLeft / Math.max(1, strip.clientWidth)), true), 120);
    }, { passive: true });
  }
})();

// 사진 전시 크게 보기 (ADR-0156). 같은 띠의 사진을 앞뒤로 넘긴다. 스크립트가 없으면 링크가 원본 그림을 연다.
(() => {
  const gals = document.querySelectorAll(".gal");
  if (!gals.length || typeof HTMLDialogElement !== "function") return;
  let dlg = null, list = [], at = 0;
  const show = () => {
    const a = list[at];
    const img = a.querySelector("img");
    const big = dlg.querySelector("img");
    big.src = a.href;
    big.alt = img ? img.alt : "";
    const cap = a.closest("figure") && a.closest("figure").querySelector("figcaption");
    const fc = dlg.querySelector("figcaption");
    fc.textContent = cap ? cap.textContent : "";
    fc.hidden = !cap;
    dlg.querySelector("[data-n]").textContent = (at + 1) + " / " + list.length;
    dlg.querySelectorAll("[data-step]").forEach((b) => { b.hidden = list.length < 2; });
  };
  const step = (d) => { at = (at + d + list.length) % list.length; show(); };
  const make = () => {
    dlg = document.createElement("dialog");
    dlg.className = "lightbox";
    dlg.setAttribute("aria-label", T("사진 크게 보기"));
    const fig = document.createElement("figure");
    fig.append(document.createElement("img"), document.createElement("figcaption"));
    const bar = document.createElement("div");
    bar.className = "lightbox-bar";
    const btn = (label, fn, attr) => {
      const b = document.createElement("button");
      b.type = "button";
      b.textContent = label;
      if (attr) b.setAttribute(attr, "");
      b.addEventListener("click", fn);
      return b;
    };
    const n = document.createElement("span");
    n.setAttribute("data-n", "");
    n.className = "visually-hidden";
    n.setAttribute("aria-live", "polite");
    bar.append(btn(T("이전"), () => step(-1), "data-step"), btn(T("닫기"), () => dlg.close()), btn(T("다음"), () => step(1), "data-step"), n);
    dlg.append(fig, bar);
    dlg.addEventListener("click", (ev) => { if (ev.target === dlg) dlg.close(); });
    dlg.addEventListener("keydown", (ev) => {
      if (ev.key === "ArrowLeft") step(-1);
      else if (ev.key === "ArrowRight") step(1);
    });
    document.body.append(dlg);
  };
  for (const g of gals) {
    g.addEventListener("click", (ev) => {
      const a = ev.target.closest && ev.target.closest("a.gal-link");
      if (!a || ev.button !== 0 || ev.ctrlKey || ev.metaKey || ev.shiftKey || ev.altKey) return;
      ev.preventDefault();
      if (!dlg) make();
      list = Array.from(g.querySelectorAll("a.gal-link"));
      at = Math.max(0, list.indexOf(a));
      show();
      dlg.showModal();
    });
  }
})();

// 남은 날 (ADR-0156). 서버가 그린 D-day 아래에 남은 일·시간·분을 채우고 1분마다 고친다. 0 이 되면 새로 읽지 않고 멈춘다.
(() => {
  const boxes = document.querySelectorAll("[data-countdown]");
  if (!boxes.length) return;
  const tick = () => {
    for (const box of boxes) {
      const left = Date.parse(box.dataset.countdown) - Date.now();
      const out = box.querySelector(".cd-left");
      if (!out || !(left > 0)) { if (out) out.hidden = true; continue; }
      const m = Math.floor(left / 60000);
      const d = Math.floor(m / 1440), h = Math.floor((m % 1440) / 60), mm = m % 60;
      out.textContent = (d ? T("{n}일", d) + " " : "") + T("{n}시간", h) + " " + T("{n}분 남았습니다", mm);
      out.hidden = false;
    }
  };
  tick();
  setInterval(tick, 60000);
})();

// 섹션 등장 효과 (ADR-0157). 처음 화면 아래에 있는 블록만 숨겼다가 들어올 때 보인다. 움직임 줄이기면 아무것도 하지 않는다.
(() => {
  const secs = document.querySelectorAll(".sec.motion-fade, .sec.motion-rise");
  if (!secs.length || !("IntersectionObserver" in window) || matchMedia("(prefers-reduced-motion: reduce)").matches) return;
  const io = new IntersectionObserver((entries) => {
    for (const e of entries) {
      if (!e.isIntersecting) continue;
      e.target.classList.remove("motion-wait");
      io.unobserve(e.target);
    }
  }, { rootMargin: "0px 0px -8% 0px" });
  for (const s of secs) {
    if (s.getBoundingClientRect().top < innerHeight) continue;
    s.classList.add("motion-wait");
    io.observe(s);
  }
})();

// 글 코드 블록 복사 단추 (ADR-0161). 스크립트가 있을 때만 붙고, 클립보드를 못 쓰면 글자를 골라 둔다.
(() => {
  const blocks = document.querySelectorAll(".prose pre > code");
  for (const code of blocks) {
    const pre = code.parentElement;
    if (pre.parentElement && pre.parentElement.classList.contains("code-wrap")) continue;
    const wrap = document.createElement("div");
    wrap.className = "code-wrap";
    pre.replaceWith(wrap);
    wrap.append(pre);
    const btn = document.createElement("button");
    btn.type = "button";
    btn.className = "code-copy";
    btn.textContent = T("복사");
    btn.setAttribute("aria-label", T("코드 복사"));
    btn.addEventListener("click", async () => {
      const done = (msg) => { btn.textContent = msg; setTimeout(() => { btn.textContent = T("복사"); }, 2000); };
      try {
        await navigator.clipboard.writeText(code.innerText);
        done(T("복사했습니다"));
      } catch (e) {
        const r = document.createRange();
        r.selectNodeContents(code);
        const s = getSelection();
        s.removeAllRanges();
        s.addRange(r);
        done(T("Ctrl+C 로 복사하세요"));
      }
    });
    wrap.append(btn);
  }
})();
}
