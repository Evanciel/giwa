// 스티커 고르기 (GAP E15, ADR-0115). `textarea[data-stickers]` 옆에 단추를 붙인다. 스크립트가 없으면 글만 쓰면 된다.
// 댓글은 글자라 `:cat-1:` 을, 공개 글쓰기는 Markdown 이라 그림 문법을 넣는다.
(function () {
  "use strict";
  // 영어 사이트 문구 (ADR-0134). `<html lang>` 을 보고, 표에 없으면 한국어 그대로. {n} 은 값 자리.
  var MARU_EN = document.documentElement.lang.indexOf("en") === 0 ? {
    "스티커": "Stickers",
    "스티커 고르기": "Choose a sticker",
    "스티커를 불러오지 못했습니다.": "Could not load stickers.",
  } : null;
  function T(s, n) { return ((MARU_EN && MARU_EN[s]) || s).replace("{n}", n); }
  var loaded = null;

  function load() {
    if (!loaded) {
      loaded = fetch("/assets/stickers/index.json").then(function (r) {
        if (!r.ok) throw new Error(String(r.status));
        return r.json();
      });
    }
    return loaded;
  }

  function insert(box, text) {
    var start = box.selectionStart;
    var end = box.selectionEnd;
    box.value = box.value.slice(0, start) + text + box.value.slice(end);
    var pos = start + text.length;
    box.setSelectionRange(pos, pos);
    box.focus();
    box.dispatchEvent(new Event("input", { bubbles: true }));
  }

  function fill(panel, box, data, format) {
    data.sets.forEach(function (set) {
      set.items.forEach(function (s) {
        var b = document.createElement("button");
        b.type = "button";
        b.title = s.label;
        var img = document.createElement("img");
        img.src = s.src;
        img.alt = s.label;
        img.width = 64;
        img.height = 64;
        img.loading = "lazy";
        b.appendChild(img);
        b.addEventListener("click", function () {
          insert(box, format === "markdown" ? "![" + s.label + "](" + s.src + ")" : ":" + s.id + ":");
        });
        panel.appendChild(b);
      });
    });
  }

  function attach(box) {
    if (box.dataset.stickersReady) return;
    box.dataset.stickersReady = "1";
    var open = document.createElement("button");
    open.type = "button";
    open.className = "sticker-open";
    open.textContent = T("스티커");
    open.setAttribute("aria-expanded", "false");
    var panel = document.createElement("div");
    panel.className = "sticker-panel";
    panel.setAttribute("role", "group");
    panel.setAttribute("aria-label", T("스티커 고르기"));
    panel.hidden = true;

    function toggle(show) {
      panel.hidden = !show;
      open.setAttribute("aria-expanded", String(show));
    }

    open.addEventListener("click", function () {
      var show = panel.hidden;
      toggle(show);
      if (show && !panel.firstChild) {
        load()
          .then(function (data) {
            fill(panel, box, data, box.dataset.stickers);
            var first = panel.querySelector("button");
            if (first) first.focus();
          })
          .catch(function () {
            panel.textContent = T("스티커를 불러오지 못했습니다.");
          });
      }
    });
    panel.addEventListener("keydown", function (e) {
      if (e.key === "Escape") {
        e.preventDefault();
        toggle(false);
        open.focus();
      }
    });
    box.after(open, panel);
  }

  document.querySelectorAll("textarea[data-stickers]").forEach(attach);
})();
