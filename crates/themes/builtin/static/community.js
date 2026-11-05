// 커뮤니티 스킨 (ADR-0081): 어두운 화면 전환과 회원 막대.
// 공개 화면 HTML 은 모두에게 같다(캐시). 로그인한 사람의 이름·알림 수는 /me/bar 에서 따로 받는다.
(function () {
  "use strict";
  var root = document.documentElement;
  var KEY = "maru.mode";

  function stored() {
    try { return localStorage.getItem(KEY); } catch (e) { return null; }
  }
  function apply(mode) {
    if (mode === "dark" || mode === "light") root.setAttribute("data-mode", mode);
    else root.removeAttribute("data-mode");
  }
  apply(stored());

  function isDark() {
    var m = root.getAttribute("data-mode");
    if (m) return m === "dark";
    return window.matchMedia && window.matchMedia("(prefers-color-scheme: dark)").matches;
  }

  function el(tag, attrs, text) {
    var n = document.createElement(tag);
    for (var k in attrs) n.setAttribute(k, attrs[k]);
    if (text != null) n.textContent = text;
    return n;
  }

  function link(item) {
    var a = el("a", { href: item.url }, item.label);
    if (item.count > 0) {
      var c = el("span", { "class": "cm-count" }, item.count > 99 ? "99+" : String(item.count));
      c.setAttribute("aria-label", "읽지 않은 " + item.count + "개");
      a.appendChild(document.createTextNode(" "));
      a.appendChild(c);
    }
    return a;
  }

  function fillBar(nav, bar) {
    var items = [];
    if (bar.signed_in) {
      var me = el("a", { href: bar.me_url, "class": "cm-me" }, bar.name);
      items.push(me);
      (bar.items || []).forEach(function (it) { items.push(link(it)); });
    } else {
      items.push(el("a", { href: bar.login_url }, "로그인"));
      if (bar.signup_url) items.push(el("a", { href: bar.signup_url, "class": "cm-join" }, "회원가입"));
    }
    nav.textContent = "";
    items.forEach(function (n) { nav.appendChild(n); });
  }

  // 회원 이름 메뉴 (ADR-0082). 템플릿은 회원 이름에 data-member 만 달고, 무엇을 할 수 있는지는
  // 로그인한 사람의 막대에 모듈이 넣은 member_action 이 정한다(쪽지 보내기 등). 내 이름엔 달지 않는다.
  var openMenu = null;
  function closeMenu() {
    if (!openMenu) return;
    openMenu.menu.hidden = true;
    openMenu.button.setAttribute("aria-expanded", "false");
    openMenu = null;
  }
  function memberMenus(bar) {
    // 프로필 보기는 코어 화면이라 늘 첫 항목이다 (GAP Q2).
    var actions = [{ label: "프로필 보기", url: "/u/" }].concat(
      (bar.items || []).map(function (i) { return i.member_action; }).filter(Boolean));
    if (!bar.signed_in) return;
    var names = document.querySelectorAll("[data-member]");
    for (var i = 0; i < names.length; i++) {
      var host = names[i];
      var name = host.getAttribute("data-member");
      if (!name || name === bar.name) continue;
      var button = el("button", { type: "button", "class": "cm-member", "aria-expanded": "false", "aria-haspopup": "true" }, host.textContent);
      var menu = el("span", { "class": "cm-member-menu" });
      menu.hidden = true;
      actions.forEach(function (a) {
        menu.appendChild(el("a", { href: a.url + encodeURIComponent(name) }, a.label));
      });
      host.textContent = "";
      host.classList.add("cm-member-host");
      host.appendChild(button);
      host.appendChild(menu);
      button.addEventListener("click", (function (button, menu) {
        return function (ev) {
          ev.preventDefault();
          ev.stopPropagation();
          var same = openMenu && openMenu.menu === menu;
          closeMenu();
          if (same) return;
          // 목록 칸은 말줄임(overflow: hidden)이라 안에 두면 잘린다 → 화면 기준으로 띄운다.
          menu.hidden = false;
          var r = button.getBoundingClientRect();
          var w = menu.offsetWidth;
          menu.style.left = Math.max(8, Math.min(r.left, window.innerWidth - w - 8)) + "px";
          menu.style.top = r.bottom + 4 + "px";
          button.setAttribute("aria-expanded", "true");
          openMenu = { button: button, menu: menu };
          var first = menu.querySelector("a");
          if (first) first.focus({ preventScroll: true });
        };
      })(button, menu));
    }
    document.addEventListener("click", function (ev) {
      if (openMenu && !openMenu.menu.contains(ev.target)) closeMenu();
    });
    window.addEventListener("scroll", closeMenu, true);
    window.addEventListener("resize", closeMenu);
    document.addEventListener("keydown", function (ev) {
      if (ev.key === "Escape" && openMenu) {
        var b = openMenu.button;
        closeMenu();
        b.focus();
      }
    });
  }

  // 지금 보는 게시판의 메뉴 칸에 표시. 메뉴 HTML 도 캐시하므로 주소를 보고 여기서 단다.
  // 메뉴 줄과 서랍(ADR-0098)에 하나씩.
  function markCurrent() {
    var here = location.pathname;
    [".cm-nav-groups", ".cm-drawer-nav"].forEach(function (box) {
      var links = document.querySelectorAll(box + " a[href^='/']");
      for (var i = 0; i < links.length; i++) {
        var href = links[i].getAttribute("href").replace(/\/+$/, "");
        if (href && (here === href || here.indexOf(href + "/") === 0)) {
          links[i].setAttribute("aria-current", "page");
          return;
        }
      }
    });
  }

  // 휴대폰 서랍 메뉴·검색 덮개 (GAP Q13, ADR-0098). <dialog> 를 쓸 수 있을 때만 단추를 보이고
  // .cm-js 를 단다(그래야 CSS 가 메뉴 줄·검색 칸을 감춘다). Esc·초점 가두기는 브라우저가 한다.
  function sheets() {
    var ok = false;
    [[".cm-drawer-open", "cm-drawer"], [".cm-search-open", "cm-search-sheet"]].forEach(function (p) {
      var btn = document.querySelector(p[0]);
      var dlg = document.getElementById(p[1]);
      if (!btn || !dlg || typeof dlg.showModal !== "function") return;
      ok = true;
      btn.hidden = false;
      btn.addEventListener("click", function () {
        if (p[1] === "cm-search-sheet") recentSearches(dlg);
        dlg.showModal();
        btn.setAttribute("aria-expanded", "true");
        var input = dlg.querySelector("input[type=search]");
        if (input) input.focus();
      });
      dlg.addEventListener("close", function () { btn.setAttribute("aria-expanded", "false"); });
      // 바깥(뒤 막) 누름이나 닫기 단추
      dlg.addEventListener("click", function (ev) {
        if (ev.target === dlg || (ev.target.closest && ev.target.closest("[data-close]"))) dlg.close();
      });
    });
    if (ok) root.classList.add("cm-js");
  }

  // 최근 검색어: 이 브라우저 localStorage 에만 10개. 서버로 보내지 않는다.
  var QKEY = "maru.recentSearches";
  function loadQ() {
    try {
      var v = JSON.parse(localStorage.getItem(QKEY) || "[]");
      return Array.isArray(v) ? v.filter(function (s) { return typeof s === "string" && s.trim() !== ""; }) : [];
    } catch (e) { return []; }
  }
  function saveQ(list) {
    try {
      if (list.length) localStorage.setItem(QKEY, JSON.stringify(list));
      else localStorage.removeItem(QKEY);
    } catch (e) { /* 저장 못 함 */ }
  }
  function rememberSearches() {
    var forms = document.querySelectorAll("form[role=search]");
    for (var i = 0; i < forms.length; i++) {
      forms[i].addEventListener("submit", function (ev) {
        var input = ev.currentTarget.querySelector("input[name=q]");
        var q = input ? input.value.trim().slice(0, 100) : "";
        if (!q) return;
        saveQ([q].concat(loadQ().filter(function (x) { return x !== q; })).slice(0, 10));
      });
    }
  }
  function recentSearches(dlg) {
    var box = dlg.querySelector("[data-recent-searches]");
    var form = dlg.querySelector("form[role=search]");
    if (!box || !form) return;
    var ul = box.querySelector("ul");
    var list = loadQ();
    ul.textContent = "";
    box.hidden = list.length === 0;
    list.forEach(function (q) {
      var li = el("li", {});
      li.appendChild(el("a", { href: form.getAttribute("action") + "?q=" + encodeURIComponent(q) }, q));
      var del = el("button", { type: "button", "aria-label": q + " 지우기" }, "지우기");
      del.addEventListener("click", function () {
        saveQ(loadQ().filter(function (x) { return x !== q; }));
        recentSearches(dlg);
      });
      li.appendChild(del);
      ul.appendChild(li);
    });
    var clear = box.querySelector("[data-clear]");
    if (clear && !clear.dataset.bound) {
      clear.dataset.bound = "1";
      clear.addEventListener("click", function () {
        saveQ([]);
        recentSearches(dlg);
        var input = dlg.querySelector("input[type=search]");
        if (input) input.focus();
      });
    }
  }

  // 전체 메뉴 (GAP Q12, ADR-0097). details 라 스크립트 없이도 열고 닫힌다. 여기선 바깥 누름·Esc 로 닫기만.
  function allMenu() {
    var d = document.querySelector(".cm-allmenu");
    if (!d) return;
    document.addEventListener("click", function (ev) {
      if (d.open && !d.contains(ev.target)) d.open = false;
    });
    document.addEventListener("keydown", function (ev) {
      if (ev.key === "Escape" && d.open) {
        d.open = false;
        d.querySelector("summary").focus();
      }
    });
  }

  document.addEventListener("DOMContentLoaded", function () {
    markCurrent();
    allMenu();
    sheets();
    rememberSearches();
    var btn = document.querySelector(".cm-mode");
    if (btn) {
      btn.hidden = false;
      btn.setAttribute("aria-pressed", String(isDark()));
      btn.addEventListener("click", function () {
        var next = isDark() ? "light" : "dark";
        apply(next);
        try { localStorage.setItem(KEY, next); } catch (e) { /* 저장 못 하면 이번 화면만 */ }
        btn.setAttribute("aria-pressed", String(next === "dark"));
      });
    }
    var nav = document.querySelector(".cm-account[data-bar]");
    var url = nav && nav.getAttribute("data-bar");
    if (!url || !window.fetch) return;
    fetch(url, { credentials: "same-origin", headers: { "Accept": "application/json" } })
      .then(function (r) { return r.ok ? r.json() : null; })
      .then(function (bar) { if (bar) { fillBar(nav, bar); memberMenus(bar); } })
      .catch(function () { /* 그대로 「내 계정」 링크 */ });
  });
})();
