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
      .then(function (bar) { if (bar) fillBar(nav, bar); })
      .catch(function () { /* 그대로 「내 계정」 링크 */ });
  });
})();
