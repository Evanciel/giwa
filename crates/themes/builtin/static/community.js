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
  function markCurrent() {
    var here = location.pathname;
    var links = document.querySelectorAll(".cm-nav a[href^='/']");
    for (var i = 0; i < links.length; i++) {
      var href = links[i].getAttribute("href").replace(/\/+$/, "");
      if (href && (here === href || here.indexOf(href + "/") === 0)) {
        links[i].setAttribute("aria-current", "page");
        return;
      }
    }
  }

  document.addEventListener("DOMContentLoaded", function () {
    markCurrent();
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
