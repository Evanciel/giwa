// 화면 편집기 (ADR-0138). 가운데 iframe 은 같은 출처의 실제 공개 화면이라, 캔버스에 스크립트를 넣지 않고
// 이 스크립트가 iframe 문서에 직접 고르기·끌어다 놓기를 얹는다. 초안이 바뀌면 /admin/editor/frame 에
// 초안을 보내 다시 그린 body 만 갈아 끼운다(스크롤 그대로). 발행은 /admin/editor/publish 가 변경 묶음 하나로 한다.
(function () {
  "use strict";
  var root = document.querySelector("[data-editor]");
  var dataEl = document.getElementById("editor-data");
  if (!root || !dataEl) return;
  var data;
  try { data = JSON.parse(dataEl.textContent); } catch (e) { return; }

  var csrf = root.dataset.csrf || "";
  var screen = data.screen;
  var frame = root.querySelector("[data-frame]");
  var stage = root.querySelector("[data-stage]");
  var statusEl = root.querySelector("[data-status]");
  var publishBtn = root.querySelector("[data-publish]");
  var undoBtn = root.querySelector("[data-undo]");
  var redoBtn = root.querySelector("[data-redo]");
  var inspector = root.querySelector("[data-inspector]");
  var layersEl = root.querySelector("[data-layers]");
  var paletteEl = root.querySelector("[data-palette]");

  var widgets = data.widgets.slice();
  var saved = JSON.stringify(widgets);
  // 사이트 공통 부분 초안 (ADR-0145): { name, description, menus: { header, quick, footer } }
  var site = JSON.parse(JSON.stringify(data.site));
  var savedSite = JSON.stringify(site);
  // 사이트 디자인 초안 (ADR-0148): { primary, font_body, font_heading, radius: { sm, md, lg }, density }
  var design = JSON.parse(JSON.stringify(data.design));
  var savedDesign = JSON.stringify(design);
  var MENU_LOCS = ["header", "quick", "footer"];
  var PARTS = [
    ["name", "사이트 이름", "머리글과 바닥글에 보이는 이름입니다."],
    ["description", "사이트 설명", "검색 결과와 공유 미리보기에 쓰이고, 테마에 따라 머리글에 보입니다."],
    ["menu.header", "위 메뉴", "머리글의 주 메뉴입니다."],
    ["menu.quick", "바로가기 단추", "검색 칸 옆 단추 줄입니다. 그리지 않는 테마도 있습니다."],
    ["menu.footer", "아래 메뉴", "바닥글의 메뉴입니다."]
  ];
  function isSite(id) { return typeof id === "string" && id.indexOf("site:") === 0; }
  function partOf(id) { var k = id.slice(5); return PARTS.filter(function (p) { return p[0] === k; })[0] || [k, k, ""]; }
  // 저장된 것과 다른 메뉴 위치만 보낸다.
  function siteBody() {
    var before = JSON.parse(savedSite), menus = {};
    MENU_LOCS.forEach(function (l) {
      if (JSON.stringify(site.menus[l] || []) !== JSON.stringify(before.menus[l] || [])) menus[l] = site.menus[l] || [];
    });
    return { name: site.name, description: site.description, menus: menus };
  }
  function siteDirty() {
    var before = JSON.parse(savedSite), n = 0;
    if (site.name !== before.name) n += 1;
    if (site.description !== before.description) n += 1;
    MENU_LOCS.forEach(function (l) { if (JSON.stringify(site.menus[l] || []) !== JSON.stringify(before.menus[l] || [])) n += 1; });
    return n;
  }
  var selected = null;
  var invalid = {};
  var past = [], future = [];
  var lastMark = null;
  var uid = 0;
  var STORE = "maru-editor:" + screen;
  var CANVAS_REGIONS = ["header", "hero", "main", "sidebar", "footer", "masthead", "sponsor"];

  var kinds = {};
  data.kinds.forEach(function (k) { kinds[k.id] = k; });
  var regionInfo = {};
  data.regions.forEach(function (r) { regionInfo[r.id] = r; });

  // ---- 작은 도구 ----
  function el(tag, attrs, kids, doc) {
    var n = (doc || document).createElement(tag);
    if (attrs) Object.keys(attrs).forEach(function (k) {
      if (k === "text") n.textContent = attrs[k];
      else if (k === "cls") n.className = attrs[k];
      else n.setAttribute(k, attrs[k]);
    });
    (kids || []).forEach(function (c) { if (c) n.appendChild(c); });
    return n;
  }
  function byId(id) { for (var i = 0; i < widgets.length; i++) if (widgets[i].id === id) return widgets[i]; return null; }
  function indexOf(id) { for (var i = 0; i < widgets.length; i++) if (widgets[i].id === id) return i; return -1; }
  function hash(s) { var h = 5381; for (var i = 0; i < s.length; i++) h = ((h << 5) + h + s.charCodeAt(i)) | 0; return String(h); }
  function newId() { uid += 1; return "new-" + Date.now().toString(36) + "-" + uid; }
  function nameOf(w) {
    var k = kinds[w.kind];
    var kindName = k ? k.name : w.kind;
    return w.title ? w.title + " · " + kindName : kindName;
  }

  // 화면 범위 규칙: core 의 Screen::matches 와 같다.
  function onScreen(scope) {
    if (scope === "all") return true;
    if (screen === "home") return scope === "home";
    if (screen.indexOf("board:") === 0) return scope === "boards" || scope === screen;
    return scope === screen;
  }
  function scopeName(scope) {
    if (scope === "all") return "모든 화면";
    if (scope === "home") return "첫 화면";
    if (scope === "boards") return "게시판·글 화면 모두";
    return data.scope_names[scope] || scope;
  }
  function visibleIn(region) {
    return widgets.filter(function (w) { return w.region === region && onScreen(w.scope); });
  }

  // ---- 초안 기록 ----
  function snapshot() { return JSON.stringify({ w: widgets, s: site, d: design }); }
  function restoreSnap(snap) { var st = JSON.parse(snap); widgets = st.w; site = st.s; design = st.d; }
  // `mark` 가 같으면(같은 칸을 이어 치는 중) 기록을 하나로 합친다.
  // `quiet` 이면 캔버스와 오른쪽 판을 다시 그리지 않는다(화면 위 글자를 치는 중).
  function change(fn, mark, quiet) {
    if (!mark || mark !== lastMark) {
      past.push(snapshot());
      if (past.length > 50) past.shift();
      future = [];
    }
    lastMark = mark || null;
    fn();
    if (quiet) changed(false, false); else changed(true);
  }
  function undo() {
    if (!past.length) return;
    future.push(snapshot());
    restoreSnap(past.pop());
    lastMark = null;
    drawDesign();
    if (selected && !isSite(selected) && !byId(selected)) selected = null;
    form = null;
    changed(true, true);
  }
  function redo() {
    if (!future.length) return;
    past.push(snapshot());
    restoreSnap(future.pop());
    lastMark = null;
    drawDesign();
    if (selected && !isSite(selected) && !byId(selected)) selected = null;
    form = null;
    changed(true, true);
  }
  function dirtyCount() {
    var before = {};
    JSON.parse(saved).forEach(function (w, i) { before[w.id] = JSON.stringify(w) + "@" + i; });
    var n = 0, seen = {};
    widgets.forEach(function (w) {
      seen[w.id] = true;
      var b = before[w.id];
      if (!b || b.slice(0, b.lastIndexOf("@")) !== JSON.stringify(w)) n += 1;
    });
    Object.keys(before).forEach(function (id) { if (!seen[id]) n += 1; });
    if (!n && JSON.stringify(widgets) !== saved) n = 1; // 순서만 바뀜
    return n + siteDirty() + (Object.keys(designPatch()).length ? 1 : 0);
  }

  var renderTimer = null;
  function changed(redraw, rebuildInspector) {
    var n = dirtyCount();
    statusEl.textContent = n ? "발행하지 않은 고침 " + n + "곳" : "고친 곳 없음";
    publishBtn.disabled = !n;
    undoBtn.disabled = !past.length;
    redoBtn.disabled = !future.length;
    try {
      if (n) localStorage.setItem(STORE, JSON.stringify({ base: hash(saved + savedSite + savedDesign), widgets: widgets, site: site, design: design }));
      else localStorage.removeItem(STORE);
    } catch (e) { /* 저장소를 못 쓰면 이 창에서만 */ }
    drawLayers();
    if (rebuildInspector !== false) drawInspector();
    if (JSON.stringify(design) !== shownDesign) { shownDesign = JSON.stringify(design); previewDesign(); }
    if (redraw) {
      clearTimeout(renderTimer);
      renderTimer = setTimeout(renderFrame, 180);
    }
  }

  // ---- 캔버스 ----
  var renderSeq = 0;
  function fdoc() { try { return frame.contentDocument; } catch (e) { return null; } }

  function renderFrame() {
    var seq = ++renderSeq;
    fetch("/admin/editor/frame?screen=" + encodeURIComponent(screen), {
      method: "POST",
      credentials: "same-origin",
      headers: { "content-type": "application/json", "x-csrf-token": csrf },
      body: JSON.stringify({ widgets: widgets, site: siteBody() })
    }).then(function (r) {
      var bad = r.headers.get("x-maru-invalid") || "";
      return r.text().then(function (t) { return { ok: r.ok, text: t, bad: bad }; });
    }).then(function (res) {
      if (seq !== renderSeq) return;
      // 화면 위 글자를 치는 중이면 갈아 끼우지 않는다. 다 치면 다시 그린다.
      if (textEdit) return;
      if (!res.ok) { statusEl.textContent = "화면을 다시 그리지 못했습니다. 새로 고침해 보세요."; return; }
      invalid = {};
      res.bad.split(",").filter(Boolean).forEach(function (id) { invalid[id] = true; });
      var doc = fdoc();
      if (!doc || !doc.body) return;
      var fresh = new DOMParser().parseFromString(res.text, "text/html");
      doc.body.className = fresh.body.className;
      doc.body.innerHTML = fresh.body.innerHTML;
      decorate();
      drawLayers();
      drawInspector();
    }).catch(function () {
      if (seq === renderSeq) statusEl.textContent = "서버에 닿지 않습니다. 고친 것은 이 브라우저에 남아 있습니다.";
    });
  }

  function regionOf(node) { return node.closest("[data-maru-region]"); }
  // 고를 수 있는 것: 블록(data-maru-w)과 사이트 공통 부분(data-maru-s, ADR-0145). 부분의 id 는 "site:<키>".
  var HOST = "[data-maru-w], [data-maru-s]";
  function hostId(n) { return n.hasAttribute("data-maru-w") ? n.getAttribute("data-maru-w") : "site:" + n.getAttribute("data-maru-s"); }
  function nodeOf(id) {
    var doc = fdoc();
    if (!doc || !id) return null;
    return doc.querySelector(isSite(id) ? '[data-maru-s="' + id.slice(5) + '"]' : '[data-maru-w="' + id + '"]');
  }

  // 그려진 화면에 고르기 표시·빈 자리·못 그린 블록 자리를 얹는다.
  function decorate() {
    var doc = fdoc();
    if (!doc || !doc.body) return;
    if (!doc.querySelector("link[data-maru-editor]")) {
      var link = doc.createElement("link");
      link.rel = "stylesheet";
      link.href = "/admin/assets/page-editor-frame.css";
      link.setAttribute("data-maru-editor", "");
      doc.head.appendChild(link);
    }
    doc.documentElement.classList.add("maru-editing");
    applyDesign();
    doc.querySelectorAll("[data-maru-region]").forEach(function (reg) {
      var id = reg.getAttribute("data-maru-region");
      var box = reg.querySelector(".wrap, .cm-inner, .sponsor-items") || reg;
      var list = visibleIn(id).filter(function (w) { return w.enabled; });
      // 서버가 그리지 못한 블록(설정이 덜 됨 등)은 자리 표시로 끼운다.
      list.forEach(function (w, i) {
        if (reg.querySelector('[data-maru-w="' + w.id + '"]')) return;
        var ph = el("div", { cls: "maru-pending", "data-maru-w": w.id }, [
          el("strong", { text: nameOf(w) }, null, doc),
          el("span", { text: invalid[w.id] ? "설정을 마저 채우면 보입니다." : "손님에게는 지금 보일 내용이 없습니다." }, null, doc)
        ], doc);
        var next = null;
        for (var j = i + 1; j < list.length && !next; j++) next = reg.querySelector('[data-maru-w="' + list[j].id + '"]');
        if (next) next.parentNode.insertBefore(ph, next); else box.appendChild(ph);
      });
      if (!reg.querySelector("[data-maru-w]")) {
        var info = regionInfo[id] || { label: id };
        box.appendChild(el("div", { cls: "maru-empty", "data-maru-empty": id }, [
          el("strong", { text: info.label }, null, doc),
          el("span", { text: "블록을 여기로 끌어다 놓으세요" }, null, doc)
        ], doc));
      }
    });
    markSelection();
    if (!doc.__maruBound) bindFrame(doc);
  }

  function markSelection() {
    var doc = fdoc();
    if (!doc) return;
    doc.querySelectorAll(".maru-selected").forEach(function (n) { n.classList.remove("maru-selected"); });
    var bar = doc.querySelector(".maru-bar");
    if (bar) bar.remove();
    if (!selected) return;
    var node = nodeOf(selected);
    if (!node) return;
    node.classList.add("maru-selected");
    if (isSite(selected)) {
      var sb = el("div", { cls: "maru-bar", role: "toolbar", "aria-label": "고른 부분" }, [
        el("span", { cls: "maru-bar-name", text: partOf(selected)[1] + " · 모든 화면" }, null, doc)
      ], doc);
      doc.body.appendChild(sb);
      placeBar(node, sb);
      return;
    }
    var w = byId(selected);
    var b = el("div", { cls: "maru-bar", role: "toolbar", "aria-label": "고른 블록" }, [
      el("span", { cls: "maru-bar-name", text: nameOf(w) }, null, doc),
      el("button", { type: "button", "data-act": "up", title: "위로" , text: "↑" }, null, doc),
      el("button", { type: "button", "data-act": "down", title: "아래로", text: "↓" }, null, doc),
      el("button", { type: "button", "data-act": "copy", title: "복제", text: "복제" }, null, doc),
      el("button", { type: "button", "data-act": "hide", title: "숨기기(지우지 않음)", text: "숨기기" }, null, doc),
      el("button", { type: "button", "data-act": "remove", title: "지우기 (Delete)", text: "지우기" }, null, doc)
    ], doc);
    doc.body.appendChild(b);
    placeBar(node, b);
  }

  function placeBar(node, bar) {
    var doc = fdoc(), win = doc.defaultView;
    var r = node.getBoundingClientRect();
    var top = r.top + win.scrollY - bar.offsetHeight - 4;
    if (top < win.scrollY + 2) top = r.top + win.scrollY + 4;
    var left = Math.max(4, Math.min(r.left + win.scrollX, win.scrollX + doc.documentElement.clientWidth - bar.offsetWidth - 4));
    bar.style.top = top + "px";
    bar.style.left = left + "px";
  }

  function bindFrame(doc) {
    doc.__maruBound = true;
    var win = doc.defaultView;
    // 편집 중에는 링크로 떠나거나 폼을 보내지 않는다.
    doc.addEventListener("submit", function (e) { e.preventDefault(); }, true);
    doc.addEventListener("click", function (e) {
      var act = e.target.closest(".maru-bar [data-act]");
      e.preventDefault();
      e.stopPropagation();
      if (act) { blockAction(act.getAttribute("data-act"), selected); return; }
      if (e.target.closest(".maru-linkchip")) { pickLink(); return; }
      if (e.target.closest(".maru-bar")) return;
      if (justDragged) { justDragged = false; return; }
      if (textEdit && textEdit.node.contains(e.target)) return;
      var w = e.target.closest(HOST);
      var id = w ? hostId(w) : null;
      // 고른 블록·부분의 글자 칸을 한 번 더 누르면 그 자리에서 고친다.
      var f = e.target.closest("[data-maru-f]");
      if (f && id && id === selected && f.closest(HOST) === w) { startText(f, e); return; }
      // 그림 칸(ADR-0147): 고른 블록의 그림을 누르거나 빈 그림 자리를 누르면 그림 판을 연다.
      var im = e.target.closest("[data-maru-img]");
      if (im && id && !isSite(id) && im.closest(HOST) === w && (id === selected || im.classList.contains("maru-img-empty"))) {
        if (id !== selected) select(id);
        pickImage(id, im);
        return;
      }
      select(id);
    }, true);
    doc.addEventListener("mouseover", function (e) {
      var w = e.target.closest(HOST);
      doc.querySelectorAll(".maru-hover").forEach(function (n) { if (n !== w) n.classList.remove("maru-hover"); });
      if (w) w.classList.add("maru-hover");
    });
    doc.addEventListener("keydown", keys);
    win.addEventListener("resize", function () { markSelection(); });
    // 화면 안 블록 끌기: 누른 채 6px 넘게 움직이면 끌기.
    doc.addEventListener("pointerdown", function (e) {
      if (e.button !== 0 || e.target.closest(".maru-bar, [contenteditable]")) return;
      var w = e.target.closest("[data-maru-w]");
      if (!w) return;
      var id = w.getAttribute("data-maru-w");
      var sx = e.clientX, sy = e.clientY, started = false;
      var move = function (ev) {
        if (!started && Math.abs(ev.clientX - sx) + Math.abs(ev.clientY - sy) < 6) return;
        if (!started) { started = true; w.classList.add("maru-dragging"); }
        ev.preventDefault();
        dragOver(ev.clientX, ev.clientY);
      };
      var up = function (ev) {
        doc.removeEventListener("pointermove", move);
        doc.removeEventListener("pointerup", up);
        w.classList.remove("maru-dragging");
        if (!started) return;
        justDragged = true;
        setTimeout(function () { justDragged = false; }, 0);
        drop(ev.clientX, ev.clientY, { move: id });
      };
      doc.addEventListener("pointermove", move);
      doc.addEventListener("pointerup", up);
    });
  }
  var justDragged = false;

  // ---- 화면 위 글자 바로 고치기 (ADR-0144) ----
  // 템플릿이 편집 중에만 글자 칸에 data-maru-f="<경로>" 를 붙인다. `@title` 은 블록 제목, 나머지는 설정 안 점 경로.
  var textEdit = null;
  function readPath(obj, path) {
    var cur = obj;
    path.split(".").forEach(function (k) { cur = cur == null ? undefined : cur[k]; });
    return typeof cur === "string" ? cur : "";
  }
  function writePath(obj, path, value) {
    var keys = path.split("."), cur = obj;
    for (var i = 0; i < keys.length - 1; i++) {
      if (cur[keys[i]] == null || typeof cur[keys[i]] !== "object") cur[keys[i]] = {};
      cur = cur[keys[i]];
    }
    cur[keys[keys.length - 1]] = value;
  }
  // 고칠 곳: 블록이면 제목(@title)이나 설정, 사이트 공통 부분이면 사이트 초안.
  function getPath(id, path) {
    if (isSite(id)) return readPath(site, path);
    var w = byId(id);
    if (!w) return "";
    return path === "@title" ? w.title || "" : readPath(w.config, path);
  }
  function setPath(id, path, value) {
    if (isSite(id)) { writePath(site, path, value); return; }
    var w = byId(id);
    if (!w) return;
    if (path === "@title") w.title = value;
    else writePath(w.config || (w.config = {}), path, value);
  }
  function textOf(node) { return node.textContent.replace(/[\r\n]+/g, " "); }

  function startText(node, ev) {
    if (textEdit) finishText(false);
    var host = node.closest(HOST);
    var id = host && hostId(host);
    if (!id || (!isSite(id) && !byId(id))) return;
    var doc = fdoc();
    var t = { node: node, id: id, path: node.getAttribute("data-maru-f"), before: null, shown: node.textContent, typed: false };
    t.before = getPath(id, t.path);
    t.onInput = function () {
      t.typed = true;
      var v = textOf(node);
      change(function () { setPath(t.id, t.path, v); }, "text:" + t.id + ":" + t.path, true);
    };
    t.onKey = function (e) {
      if (e.key === "Enter") { e.preventDefault(); e.stopPropagation(); finishText(false); }
      else if (e.key === "Escape") { e.preventDefault(); e.stopPropagation(); finishText(true); }
    };
    t.onBlur = function () { finishText(false); };
    textEdit = t;
    try { node.contentEditable = "plaintext-only"; } catch (e) { /* 지원하지 않는 브라우저 */ }
    if (node.contentEditable !== "plaintext-only") node.contentEditable = "true";
    node.classList.add("maru-text-editing");
    node.addEventListener("input", t.onInput);
    node.addEventListener("keydown", t.onKey);
    node.addEventListener("blur", t.onBlur);
    node.focus();
    if (!isSite(id) && node.closest("a")) showLinkChip(t);
    // 누른 자리에 커서를 둔다. 못 찾으면 글 전체를 고른다.
    var sel = doc.defaultView.getSelection();
    var r = ev && doc.caretRangeFromPoint ? doc.caretRangeFromPoint(ev.clientX, ev.clientY) : null;
    if (!r || !node.contains(r.startContainer)) { r = doc.createRange(); r.selectNodeContents(node); }
    sel.removeAllRanges();
    sel.addRange(r);
    statusEl.textContent = "화면 위 글자를 고치는 중입니다. Enter 로 마치고 Esc 로 되돌립니다.";
  }

  function finishText(cancel) {
    var t = textEdit;
    if (!t) return;
    textEdit = null;
    var node = t.node;
    node.removeEventListener("input", t.onInput);
    node.removeEventListener("keydown", t.onKey);
    node.removeEventListener("blur", t.onBlur);
    node.removeAttribute("contenteditable");
    node.classList.remove("maru-text-editing");
    if (t.chip) t.chip.remove();
    if (t.typed && (isSite(t.id) || byId(t.id))) {
      var v = cancel ? t.before : textOf(node).trim();
      if (cancel) node.textContent = t.shown;
      if (getPath(t.id, t.path) !== v) change(function () { setPath(t.id, t.path, v); }, "text:" + t.id + ":" + t.path, true);
    }
    lastMark = null;
    form = null; // 오른쪽 판 칸을 고친 값으로 새로 만든다
    changed(true);
  }

  // ---- 그림·링크 고르기 (ADR-0147) ----
  // 템플릿이 편집 중에만 그림 칸에 data-maru-img="<경로>"(설명 칸이 있으면 data-maru-alt)를 붙인다.
  // 대화 상자는 media-picker.js 다. 고르면 그 경로를 초안에 쓰고 캔버스를 다시 그린다.
  function pickImage(id, node) {
    if (!window.MaruMedia) return;
    var path = node.getAttribute("data-maru-img"), altPath = node.getAttribute("data-maru-alt");
    window.MaruMedia.open({
      url: getPath(id, path), alt: altPath ? getPath(id, altPath) : null, csrf: csrf,
      required: node.hasAttribute("data-maru-required"),
      onPick: function (url, alt) {
        if (!byId(id)) return;
        form = null;
        change(function () { setPath(id, path, url); if (altPath && alt !== null) setPath(id, altPath, alt); });
      }
    });
  }
  // 단추·링크 글자를 고치는 동안 「링크 바꾸기」를 띄운다. 링크 칸은 글자 칸 옆의 `link` (button.label → button.link).
  function showLinkChip(t) {
    var doc = fdoc(), win = doc.defaultView;
    var chip = el("button", { type: "button", cls: "maru-linkchip", text: "링크 바꾸기" }, null, doc);
    chip.addEventListener("pointerdown", function (e) { e.preventDefault(); }); // 글자 칸이 초점을 잃지 않게
    doc.body.appendChild(chip);
    var r = t.node.getBoundingClientRect();
    chip.style.top = (r.bottom + win.scrollY + 6) + "px";
    chip.style.left = Math.max(4, Math.min(r.left + win.scrollX, win.scrollX + doc.documentElement.clientWidth - chip.offsetWidth - 4)) + "px";
    t.chip = chip;
  }
  function pickLink() {
    var t = textEdit;
    if (!t || !window.MaruLinks) return;
    var id = t.id, path = t.path.replace(/\.[^.]+$/, ".link");
    if (path === t.path || path.indexOf(".") < 0) return;
    finishText(false);
    window.MaruLinks.open({
      link: getPath(id, path),
      targets: { pages: data.pages || [], boards: data.boards || [] },
      onPick: function (v) {
        if (!byId(id)) return;
        form = null;
        change(function () { setPath(id, path, v); });
      }
    });
  }

  // ---- 놓을 곳 찾기 (캔버스 좌표) ----
  function dropTarget(x, y) {
    var doc = fdoc();
    if (!doc) return null;
    var regs = Array.prototype.slice.call(doc.querySelectorAll("[data-maru-region]")).filter(function (r) {
      var b = r.getBoundingClientRect();
      return b.width > 0 && b.height > 0;
    });
    if (!regs.length) return null;
    // 점을 품은 자리 중 가장 작은 것, 없으면 세로로 가장 가까운 것.
    var best = null, bestScore = Infinity;
    regs.forEach(function (r) {
      var b = r.getBoundingClientRect();
      var inside = x >= b.left && x <= b.right && y >= b.top && y <= b.bottom;
      var score = inside ? b.width * b.height / 1e9 : 1 + Math.min(Math.abs(y - b.top), Math.abs(y - b.bottom)) + (x < b.left || x > b.right ? 2000 : 0);
      if (score < bestScore) { bestScore = score; best = r; }
    });
    var region = best.getAttribute("data-maru-region");
    var items = Array.prototype.slice.call(best.querySelectorAll("[data-maru-w]")).filter(function (n) { return regionOf(n) === best; });
    var index = items.length;
    for (var i = 0; i < items.length; i++) {
      var b = items[i].getBoundingClientRect();
      if (y < b.top + b.height / 2) { index = i; break; }
    }
    return { el: best, region: region, items: items, index: index };
  }

  function showLine(t) {
    var doc = fdoc();
    if (!doc) return;
    var line = doc.querySelector(".maru-drop");
    if (!t) { if (line) line.remove(); doc.querySelectorAll(".maru-drop-region").forEach(function (n) { n.classList.remove("maru-drop-region"); }); return; }
    if (!line) { line = el("div", { cls: "maru-drop" }, null, doc); doc.body.appendChild(line); }
    var win = doc.defaultView, rb = t.el.getBoundingClientRect(), y;
    if (!t.items.length) y = rb.top + rb.height / 2;
    else if (t.index < t.items.length) y = t.items[t.index].getBoundingClientRect().top - 3;
    else y = t.items[t.items.length - 1].getBoundingClientRect().bottom + 1;
    line.style.top = (y + win.scrollY) + "px";
    line.style.left = (rb.left + win.scrollX + 4) + "px";
    line.style.width = Math.max(40, rb.width - 8) + "px";
    doc.querySelectorAll(".maru-drop-region").forEach(function (n) { if (n !== t.el) n.classList.remove("maru-drop-region"); });
    t.el.classList.add("maru-drop-region");
  }

  function autoscroll(y) {
    var doc = fdoc();
    if (!doc) return;
    var h = doc.documentElement.clientHeight;
    if (y < 48) doc.defaultView.scrollBy(0, -18);
    else if (y > h - 48) doc.defaultView.scrollBy(0, 18);
  }

  function dragOver(x, y) {
    autoscroll(y);
    showLine(dropTarget(x, y));
  }

  // what: { move: id } 또는 { add: kind }
  function drop(x, y, what) {
    var t = dropTarget(x, y);
    showLine(null);
    if (!t) return;
    var kind = what.add || (byId(what.move) || {}).kind;
    if (kind === "popup" || t.region === "popup") {
      if (kind === "popup" && what.add) addBlock(kind, { region: "popup" });
      else statusEl.textContent = "팝업은 팝업 자리에만 둡니다.";
      return;
    }
    var visible = visibleIn(t.region).filter(function (w) { return w.enabled && w.id !== what.move; });
    // 그려진 순서(t.items)로 앞 블록을 찾는다. 자리 표시도 data-maru-w 라 함께 센다.
    var ids = t.items.map(function (n) { return n.getAttribute("data-maru-w"); }).filter(function (id) { return id !== what.move; });
    var origIndex = t.items.slice(0, t.index).filter(function (n) { return n.getAttribute("data-maru-w") !== what.move; }).length;
    var before = ids[origIndex] || null;
    if (before && !byId(before)) before = null;
    var mutate = function (w) {
      var from = indexOf(w.id);
      if (from >= 0) widgets.splice(from, 1);
      w.region = t.region;
      var at;
      if (before) at = indexOf(before);
      else if (visible.length) at = indexOf(visible[visible.length - 1].id) + 1;
      else {
        var last = -1;
        widgets.forEach(function (x, i) { if (x.region === t.region) last = i; });
        at = last + 1 || widgets.length;
      }
      widgets.splice(at, 0, w);
    };
    if (what.move) {
      var w = byId(what.move);
      if (!w) return;
      change(function () { mutate(w); });
      select(w.id);
    } else {
      var nw = blankBlock(what.add, t.region);
      change(function () { mutate(nw); });
      select(nw.id);
    }
  }

  // ---- 새 블록 ----
  // 빈 설정으로는 거절되는 종류에 넣을 자리 글. 사이트 글처럼 보이지 않게 「적으세요」로 쓴다.
  function sample(kind) {
    var firstBoard = (data.boards[0] || {}).id || "";
    var photo = data.boards.filter(function (b) { return b.id === "photo" || b.id === "gallery"; })[0];
    switch (kind) {
      case "faq": return { items: [{ q: "자주 받는 질문을 적으세요", a: "답을 적으세요" }] };
      case "features": return { items: [{ title: "첫째 장점", text: "설명을 적으세요" }, { title: "둘째 장점", text: "설명을 적으세요" }, { title: "셋째 장점", text: "설명을 적으세요" }] };
      case "hours": return { rows: [{ days: "월~금", time: "09:00~18:00" }] };
      case "image_text": return { body: "소개 글을 적으세요" };
      case "markdown_block": return { body: "글을 적으세요" };
      case "menu": return { location: "header" };
      case "notice_bar": return { text: "알릴 내용을 적으세요" };
      case "org_chart": return { nodes: [{ name: "맨 위 자리", children: [{ name: "아래 자리" }] }] };
      case "plans": return { plans: [{ name: "요금제 이름", price: "가격" }] };
      case "popup": return { body: "알릴 내용을 적으세요" };
      case "post_tabs": return { tabs: [{ label: "새 글" }] };
      case "price_list": return { groups: [{ name: "묶음 이름", items: [{ name: "항목을 적으세요" }] }] };
      case "stats": return { items: [{ value: "0", label: "숫자 설명" }] };
      case "team": return { items: [{ name: "이름을 적으세요", role: "맡은 일" }] };
      case "testimonials": return { items: [{ quote: "받은 후기를 적으세요" }] };
      case "timeline": return { items: [{ date: "연도", title: "있었던 일을 적으세요" }] };
      case "visit": return { address: "주소를 적으세요" };
      case "gallery_grid": return { board: (photo && photo.id) || firstBoard };
      case "latest_posts": case "popular_posts": return {};
      default: return null;
    }
  }
  function blankBlock(kind, region) {
    var schema = data.types[kind];
    var config = sample(kind);
    if (!config) config = schema && window.MaruBlank ? window.MaruBlank(schema, schema) : {};
    return { id: newId(), kind: kind, region: kind === "popup" ? "popup" : region, scope: screen, title: "", config: config, enabled: true };
  }
  // 누르기로 더하기: 고른 블록 아래, 없으면 본문 끝.
  function addBlock(kind, opts) {
    var region = (opts && opts.region) || "main";
    var at = -1;
    var sel = selected && byId(selected);
    if (sel && kind !== "popup" && sel.region !== "popup") { region = sel.region; at = indexOf(sel.id) + 1; }
    var w = blankBlock(kind, region);
    change(function () {
      if (at < 0) {
        var vis = visibleIn(w.region);
        at = vis.length ? indexOf(vis[vis.length - 1].id) + 1 : widgets.length;
      }
      widgets.splice(at, 0, w);
    });
    select(w.id);
  }

  // ---- 블록 일 ----
  function blockAction(act, id) {
    var w = byId(id);
    if (!w) return;
    if (act === "up" || act === "down") {
      var vis = visibleIn(w.region);
      var i = vis.indexOf(w), j = act === "up" ? i - 1 : i + 1;
      if (j < 0 || j >= vis.length) return;
      change(function () {
        var a = indexOf(w.id), b = indexOf(vis[j].id);
        widgets[a] = vis[j]; widgets[b] = w;
      });
      markSelection();
    } else if (act === "copy") {
      var c = JSON.parse(JSON.stringify(w));
      c.id = newId();
      c.scope = w.scope;
      change(function () { widgets.splice(indexOf(w.id) + 1, 0, c); });
      select(c.id);
    } else if (act === "hide" || act === "show") {
      change(function () { w.enabled = act === "show"; });
    } else if (act === "remove") {
      change(function () { widgets.splice(indexOf(w.id), 1); });
      select(null);
    }
  }

  function select(id) {
    if (textEdit && textEdit.id !== id) finishText(false);
    selected = id;
    markSelection();
    drawLayers();
    drawInspector();
  }

  // ---- 왼쪽: 더하기 ----
  function drawPalette() {
    var groups = {}, order = [];
    data.kinds.forEach(function (k) {
      if (!groups[k.group]) { groups[k.group] = []; order.push(k.group); }
      groups[k.group].push(k);
    });
    var want = ["첫 인상", "소개", "글 목록", "가게·연락", "기본", "더 보기"];
    order.sort(function (a, b) { return want.indexOf(a) - want.indexOf(b); });
    order.forEach(function (g) {
      paletteEl.appendChild(el("h3", { cls: "ed-group", text: g }));
      var box = el("div", { cls: "ed-kinds" });
      groups[g].forEach(function (k) {
        var card = el("button", { type: "button", cls: "ed-kind", "data-kind": k.id, title: k.description }, [
          el("span", { cls: "ed-kind-ico", "aria-hidden": "true", text: k.name.slice(0, 1) }),
          el("span", { cls: "ed-kind-name", text: k.name })
        ]);
        paletteEl && box.appendChild(card);
        bindPaletteDrag(card, k.id);
      });
      paletteEl.appendChild(box);
    });
  }

  // 판의 카드를 화면으로 끌기. 끄는 동안 iframe 이 포인터를 먹지 않게 하고 좌표를 캔버스 좌표로 바꾼다.
  function bindPaletteDrag(card, kind) {
    card.addEventListener("pointerdown", function (e) {
      if (e.button !== 0) return;
      var sx = e.clientX, sy = e.clientY, started = false, ghost = null;
      var toFrame = function (ev) {
        var r = frame.getBoundingClientRect();
        return { x: ev.clientX - r.left, y: ev.clientY - r.top, inside: ev.clientX >= r.left && ev.clientX <= r.right && ev.clientY >= r.top && ev.clientY <= r.bottom };
      };
      var move = function (ev) {
        if (!started && Math.abs(ev.clientX - sx) + Math.abs(ev.clientY - sy) < 6) return;
        if (!started) {
          started = true;
          root.classList.add("ed-dragging");
          ghost = el("div", { cls: "ed-ghost", text: (kinds[kind] || {}).name || kind });
          document.body.appendChild(ghost);
        }
        ghost.style.left = ev.clientX + 12 + "px";
        ghost.style.top = ev.clientY + 12 + "px";
        var p = toFrame(ev);
        if (p.inside) dragOver(p.x, p.y); else showLine(null);
      };
      var up = function (ev) {
        card.removeEventListener("pointermove", move);
        card.removeEventListener("pointerup", up);
        card.removeEventListener("pointercancel", cancel);
        if (!started) { addBlock(kind); return; }
        root.classList.remove("ed-dragging");
        if (ghost) ghost.remove();
        var p = toFrame(ev);
        if (p.inside) drop(p.x, p.y, { add: kind }); else showLine(null);
      };
      var cancel = function () {
        card.removeEventListener("pointermove", move);
        card.removeEventListener("pointerup", up);
        card.removeEventListener("pointercancel", cancel);
        root.classList.remove("ed-dragging");
        if (ghost) ghost.remove();
        showLine(null);
      };
      // 포인터를 카드에 붙잡아 둔다. 그래야 iframe 위로 가도 움직임이 이 문서로 온다.
      try { card.setPointerCapture(e.pointerId); } catch (err) { /* 붙잡지 못해도 판 위에서는 움직인다 */ }
      card.addEventListener("pointermove", move);
      card.addEventListener("pointerup", up);
      card.addEventListener("pointercancel", cancel);
      e.preventDefault();
    });
    card.addEventListener("keydown", function (e) {
      if (e.key === "Enter" || e.key === " ") { e.preventDefault(); addBlock(kind); }
    });
  }

  // ---- 왼쪽: 층 ----
  function drawLayers() {
    layersEl.textContent = "";
    // 사이트 공통 부분: 모든 화면에 함께 보인다. 테마가 그리지 않는 부분도 여기서 고른다.
    var common = el("ol", { cls: "ed-layer-list" });
    PARTS.forEach(function (p) {
      var id = "site:" + p[0];
      common.appendChild(el("li", { cls: "ed-layer is-part" + (id === selected ? " is-selected" : "") + (invalid[id] ? " is-invalid" : ""), "data-id": id }, [
        el("button", { type: "button", cls: "ed-layer-name", "data-pick": id, text: p[1] })
      ]));
    });
    layersEl.appendChild(el("section", { cls: "ed-layer-region" }, [
      el("h3", { cls: "ed-group", text: "사이트 공통", title: "모든 화면에 함께 보이는 이름·설명·메뉴" }), common
    ]));
    data.regions.forEach(function (r) {
      var list = widgets.filter(function (w) { return w.region === r.id && onScreen(w.scope); });
      if (!list.length && CANVAS_REGIONS.indexOf(r.id) < 0) return;
      var sec = el("section", { cls: "ed-layer-region", "data-region": r.id }, [
        el("h3", { cls: "ed-group", text: r.label, title: r.hint })
      ]);
      var ol = el("ol", { cls: "ed-layer-list" });
      if (!list.length) ol.appendChild(el("li", { cls: "ed-layer-empty", text: "비어 있음" }));
      list.forEach(function (w) {
        var li = el("li", { cls: "ed-layer" + (w.id === selected ? " is-selected" : "") + (w.enabled ? "" : " is-hidden") + (invalid[w.id] ? " is-invalid" : ""), draggable: "true", "data-id": w.id }, [
          el("span", { cls: "ed-grip", "aria-hidden": "true", text: "⋮⋮" }),
          el("button", { type: "button", cls: "ed-layer-name", "data-pick": w.id, text: nameOf(w) }),
          w.scope !== screen ? el("span", { cls: "ed-shared", title: scopeName(w.scope) + "에 보임", text: w.scope === "all" ? "모든 화면" : "여러 화면" }) : null,
          hiddenOn(w).length ? el("span", { cls: "ed-devoff" + (offHere(w) ? " is-here" : ""), title: hiddenOn(w).map(deviceName).join("·") + "에서 숨김", text: hiddenOn(w).map(deviceName).join("·") + " 숨김" }) : null,
          el("button", { type: "button", cls: "ed-eye", "data-eye": w.id, "aria-pressed": String(!w.enabled), title: w.enabled ? "숨기기" : "다시 보이기", text: w.enabled ? "보임" : "숨김" })
        ]);
        ol.appendChild(li);
      });
      sec.appendChild(ol);
      layersEl.appendChild(sec);
    });
  }

  layersEl.addEventListener("click", function (e) {
    var pick = e.target.closest("[data-pick]");
    if (pick) { select(pick.getAttribute("data-pick")); scrollToSelected(); return; }
    var eye = e.target.closest("[data-eye]");
    if (eye) {
      var w = byId(eye.getAttribute("data-eye"));
      if (w) blockAction(w.enabled ? "hide" : "show", w.id);
    }
  });
  // 층 끌어 옮기기 (같은 문서라 HTML5 끌기로 충분하다)
  var layerDrag = null;
  layersEl.addEventListener("dragstart", function (e) {
    var li = e.target.closest(".ed-layer");
    if (!li) return;
    layerDrag = li.getAttribute("data-id");
    e.dataTransfer.effectAllowed = "move";
    e.dataTransfer.setData("text/plain", layerDrag);
    li.classList.add("is-dragging");
  });
  layersEl.addEventListener("dragend", function () {
    layerDrag = null;
    layersEl.querySelectorAll(".drop-before, .drop-after, .is-dragging").forEach(function (n) { n.classList.remove("drop-before", "drop-after", "is-dragging"); });
  });
  function layerSpot(e) {
    var li = e.target.closest(".ed-layer"), sec = e.target.closest(".ed-layer-region");
    if (!sec || !sec.getAttribute("data-region")) return null;
    if (!li) return { region: sec.getAttribute("data-region"), before: null, li: null };
    var b = li.getBoundingClientRect(), after = e.clientY > b.top + b.height / 2;
    var target = after ? li.nextElementSibling : li;
    while (target && !target.classList.contains("ed-layer")) target = target.nextElementSibling;
    return { region: sec.getAttribute("data-region"), before: target ? target.getAttribute("data-id") : null, li: li, after: after };
  }
  layersEl.addEventListener("dragover", function (e) {
    if (!layerDrag) return;
    var s = layerSpot(e);
    if (!s) return;
    e.preventDefault();
    layersEl.querySelectorAll(".drop-before, .drop-after").forEach(function (n) { n.classList.remove("drop-before", "drop-after"); });
    if (s.li) s.li.classList.add(s.after ? "drop-after" : "drop-before");
  });
  layersEl.addEventListener("drop", function (e) {
    if (!layerDrag) return;
    var s = layerSpot(e);
    if (!s) return;
    e.preventDefault();
    var w = byId(layerDrag);
    if (!w || s.before === w.id) return;
    if ((w.kind === "popup") !== (s.region === "popup")) { statusEl.textContent = "팝업은 팝업 자리에만 둡니다."; return; }
    change(function () {
      widgets.splice(indexOf(w.id), 1);
      w.region = s.region;
      var at;
      if (s.before) at = indexOf(s.before);
      else {
        var vis = visibleIn(s.region).filter(function (x) { return x !== w; });
        at = vis.length ? indexOf(vis[vis.length - 1].id) + 1 : widgets.length;
      }
      widgets.splice(at, 0, w);
    });
  });

  function scrollToSelected() {
    var n = nodeOf(selected);
    if (n) n.scrollIntoView({ block: "center", behavior: "smooth" });
  }

  // ---- 기기별 숨김 (ADR-0146) ----
  // 이름은 서버의 widgets::DEVICES, 테마의 .hide-<기기>, 위 막대의 data-device 와 같다.
  var DEVICES = [["desktop", "데스크톱"], ["tablet", "태블릿"], ["mobile", "휴대폰"]];
  function deviceName(d) { var x = DEVICES.filter(function (p) { return p[0] === d; })[0]; return x ? x[1] : d; }
  function hiddenOn(w) { return w.hide_on || []; }
  function viewing() { return stage.getAttribute("data-device") || "desktop"; }
  function offHere(w) { return hiddenOn(w).indexOf(viewing()) >= 0; }

  // ---- 꾸미기 (ADR-0148) ----
  // 섹션 블록(설정에 tone 이 있는 것)의 띠 색·모양·여백·정렬을 칩으로 고른다. 값은 sections.rs 의 목록과 같다.
  var LOOK = [
    ["tone", "띠 색", [["plain", "바탕 그대로"], ["soft", "옅은 면"], ["strong", "주 색"]]],
    ["space", "위아래 여백", [["tight", "좁게"], ["normal", "보통"], ["roomy", "넓게"]]],
    ["align", "글 정렬", [["start", "왼쪽"], ["center", "가운데"]]]
  ];
  var STYLES = {
    hero: [["type", "글자만"], ["image", "배경 그림"], ["split", "글과 그림 나란히"]],
    features: [["cards", "카드"], ["rows", "줄"], ["bento", "벤토"], ["steps", "단계"], ["tiles", "작은 칸"]]
  };
  var inspTab = "content";
  function propsOf(kind) { return ((data.types[kind] || {}).properties) || {}; }
  function hasLook(kind) { return !!propsOf(kind).tone; }
  function canvasColors() {
    var doc = fdoc();
    if (!doc || !doc.defaultView) return null;
    var cs = doc.defaultView.getComputedStyle(doc.documentElement);
    return { bg: cs.getPropertyValue("--bg").trim(), primary: cs.getPropertyValue("--primary").trim() };
  }
  function lookPanel(w) {
    var box = el("div", { cls: "ed-look" });
    var groups = [];
    if (STYLES[w.kind]) groups.push(["style", "모양", STYLES[w.kind]]);
    LOOK.forEach(function (g) { groups.push(g); });
    var colors = canvasColors();
    groups.forEach(function (g) {
      var key = g[0], p = propsOf(w.kind)[key] || {};
      var cur = (w.config || {})[key] || p.default || g[2][0][0];
      var fs = el("fieldset", { cls: "ed-field ed-chips" }, [el("legend", { text: g[1] })]);
      g[2].forEach(function (c) {
        var sw = key === "tone" ? el("span", { cls: "ed-swatch", "aria-hidden": "true" }) : null;
        if (sw && colors && colors.bg) {
          sw.style.background = c[0] === "strong" ? colors.primary
            : c[0] === "soft" ? "color-mix(in srgb, " + colors.primary + " 12%, " + colors.bg + ")" : colors.bg;
        }
        var b = el("button", { type: "button", cls: "ed-chip", "aria-pressed": String(cur === c[0]) }, [sw, el("span", { text: c[1] })]);
        b.addEventListener("click", function () {
          var cw = byId(w.id);
          if (!cw || ((cw.config || {})[key] || p.default) === c[0]) return;
          form = null; // 설정 칸이 옛 값을 다시 쓰지 않게 새로 만든다
          change(function () { (cw.config || (cw.config = {}))[key] = c[0]; });
        });
        fs.appendChild(b);
      });
      box.appendChild(fs);
    });
    return box;
  }
  function showInspTab() {
    inspector.querySelectorAll("[data-insp-tab]").forEach(function (b) {
      b.setAttribute("aria-selected", String(b.getAttribute("data-insp-tab") === inspTab));
    });
    inspector.querySelectorAll("[data-in-tab]").forEach(function (n) { n.hidden = n.getAttribute("data-in-tab") !== inspTab; });
  }

  // ---- 사이트 디자인 (ADR-0148) ----
  // 고른 값은 저장하지 않고 /admin/editor/tokens 가 검사해 돌려준 CSS 를 캔버스에 덧입힌다. 발행하면 tokens.set 연산.
  var SWATCHES = ["#2f5d50", "#1d4ed8", "#6d28d9", "#b42318", "#b45309", "#0f766e", "#be185d", "#1f2937"];
  var RADII = [["square", "각지게", { sm: 2, md: 4, lg: 6 }], ["normal", "보통", { sm: 6, md: 12, lg: 20 }], ["round", "둥글게", { sm: 12, md: 20, lg: 32 }]];
  var DENSITY = [["compact", "촘촘하게"], ["normal", "보통"], ["spacious", "넉넉하게"]];
  var shownDesign = JSON.stringify(design), designCss = null, designBad = "", designSeq = 0;
  function designPatch() {
    var b = JSON.parse(savedDesign), p = {};
    if (design.primary !== b.primary) p.color = { primary: design.primary };
    if (design.font_body !== b.font_body || design.font_heading !== b.font_heading) p.font = { body: design.font_body, heading: design.font_heading };
    if (JSON.stringify(design.radius) !== JSON.stringify(b.radius)) p.radius = design.radius;
    if (design.density !== b.density) p.layout = { density: design.density };
    return p;
  }
  function applyDesign() {
    var doc = fdoc();
    if (!doc || !doc.defaultView || !("adoptedStyleSheets" in doc)) return;
    var win = doc.defaultView;
    var sheets = Array.prototype.filter.call(doc.adoptedStyleSheets, function (s) { return !s.__maru; });
    if (designCss) {
      try {
        var sh = new win.CSSStyleSheet();
        sh.replaceSync(designCss);
        sh.__maru = true;
        sheets.push(sh);
      } catch (e) { /* 입히지 못하면 발행 뒤에 보인다 */ }
    }
    doc.adoptedStyleSheets = sheets;
  }
  function designMsg(t) {
    var m = root.querySelector("[data-design-msg]");
    if (m) { m.textContent = t; m.hidden = !t; }
  }
  function previewDesign() {
    var p = designPatch(), seq = ++designSeq;
    if (!Object.keys(p).length) { designCss = null; designBad = ""; designMsg(""); applyDesign(); return; }
    fetch("/admin/editor/tokens", {
      method: "POST",
      credentials: "same-origin",
      headers: { "content-type": "application/json", "x-csrf-token": csrf },
      body: JSON.stringify({ tokens: p })
    }).then(function (r) {
      return r.ok ? r.text().then(function (t) { return { ok: true, t: t }; })
        : r.json().then(function (j) { return { ok: false, t: (j.error && j.error.message) || "이 값은 쓸 수 없습니다." }; });
    }).then(function (res) {
      if (seq !== designSeq) return;
      designBad = res.ok ? "" : res.t;
      if (res.ok) designCss = res.t;
      designMsg(designBad);
      applyDesign();
      if (selected && !isSite(selected) && hasLook((byId(selected) || {}).kind)) { form = null; drawInspector(); }
    }).catch(function () { /* 다음 고침에 다시 */ });
  }
  function chipRow(legend, items, isOn, pick) {
    var fs = el("fieldset", { cls: "ed-field ed-chips" }, [el("legend", { text: legend })]);
    items.forEach(function (c) {
      var b = el("button", { type: "button", cls: "ed-chip", "aria-pressed": String(isOn(c)) }, [el("span", { text: c[1] })]);
      b.addEventListener("click", function () { pick(c); drawDesign(); });
      fs.appendChild(b);
    });
    return fs;
  }
  function fontSelect(label, key) {
    var s = el("select", { "aria-label": label });
    (data.fonts || []).forEach(function (f) { s.appendChild(el("option", { value: f.id, text: f.label })); });
    if (!Array.prototype.some.call(s.options, function (o) { return o.value === design[key]; })) s.appendChild(el("option", { value: design[key], text: design[key] }));
    s.value = design[key];
    s.addEventListener("change", function () { change(function () { design[key] = s.value; }, null, true); });
    return el("label", { cls: "ed-field" }, [el("span", { text: label }), s]);
  }
  function drawDesign() {
    var box = root.querySelector("[data-design]");
    if (!box) return;
    box.textContent = "";
    var fs = el("fieldset", { cls: "ed-field ed-chips ed-colors" }, [el("legend", { text: "주 색" })]);
    SWATCHES.forEach(function (c) {
      var b = el("button", { type: "button", cls: "ed-color", title: c, "aria-label": "주 색 " + c, "aria-pressed": String(design.primary.toLowerCase() === c) });
      b.style.background = c;
      b.addEventListener("click", function () { change(function () { design.primary = c; }, null, true); drawDesign(); });
      fs.appendChild(b);
    });
    var own = el("input", { type: "color", value: design.primary, "aria-label": "주 색 직접 고르기" });
    own.addEventListener("input", function () { change(function () { design.primary = own.value; }, "design:primary", true); });
    own.addEventListener("change", function () { drawDesign(); });
    fs.appendChild(el("label", { cls: "ed-color-own" }, [own, el("span", { text: "직접" })]));
    fs.appendChild(el("p", { cls: "hint", text: "단추·링크·강조 띠의 색입니다. 바탕과 대비가 모자라면 쓸 수 없다고 알려 드립니다." }));
    box.appendChild(fs);
    box.appendChild(fontSelect("제목 글꼴", "font_heading"));
    box.appendChild(fontSelect("본문 글꼴", "font_body"));
    box.appendChild(chipRow("모서리", RADII, function (c) { return JSON.stringify(c[2]) === JSON.stringify(design.radius); },
      function (c) { change(function () { design.radius = JSON.parse(JSON.stringify(c[2])); }, null, true); }));
    box.appendChild(chipRow("간격", DENSITY, function (c) { return c[0] === design.density; },
      function (c) { change(function () { design.density = c[0]; }, null, true); }));
    var msg = el("p", { cls: "ed-design-msg", role: "alert", "data-design-msg": "" });
    msg.textContent = designBad;
    msg.hidden = !designBad;
    box.appendChild(msg);
    box.appendChild(el("p", { cls: "hint" }, [
      el("span", { text: "어두운 화면 색, 글자 크기 같은 자세한 값은 " }),
      el("a", { href: "/admin/design", text: "디자인 화면" }),
      el("span", { text: "에서 고칩니다." })
    ]));
  }

  // ---- 오른쪽: 고른 블록 ----
  var form = null;
  function drawInspector() {
    if (isSite(selected)) {
      if (form && form.__id === selected) syncInspector(null); else drawSiteInspector();
      return;
    }
    var w = selected && byId(selected);
    if (form && form.__id === (w && w.id)) { syncInspector(w); return; }
    inspector.textContent = "";
    form = null;
    if (!w) {
      inspector.appendChild(el("p", { cls: "ed-empty-pick", text: "화면에서 블록을 누르면 여기서 고칩니다." }));
      return;
    }
    var k = kinds[w.kind] || { name: w.kind, description: "" };
    var head = el("div", { cls: "ed-insp-head" }, [
      el("h2", { text: k.name }),
      k.description ? el("p", { cls: "hint", text: k.description }) : null
    ]);
    inspector.appendChild(head);
    var doc = fdoc();
    var q = '[data-maru-w="' + w.id + '"]';
    if (doc && doc.querySelector(q + "[data-maru-f], " + q + " [data-maru-f]")) {
      head.appendChild(el("p", { cls: "hint", text: "화면의 글자를 한 번 더 누르면 그 자리에서 고칩니다." }));
    }
    var note = el("p", { cls: "ed-insp-note", "data-note": "" });
    inspector.appendChild(note);
    var looks = hasLook(w.kind);
    if (looks) {
      var tabs = el("div", { cls: "ed-insp-tabs", role: "tablist", "aria-label": "고른 블록" }, [
        el("button", { type: "button", role: "tab", "data-insp-tab": "content", text: "내용" }),
        el("button", { type: "button", role: "tab", "data-insp-tab": "look", text: "꾸미기" })
      ]);
      tabs.addEventListener("click", function (e) {
        var b = e.target.closest("[data-insp-tab]");
        if (b) { inspTab = b.getAttribute("data-insp-tab"); showInspTab(); }
      });
      inspector.appendChild(tabs);
      var lp = lookPanel(w);
      lp.setAttribute("data-in-tab", "look");
      inspector.appendChild(lp);
    }

    var title = el("input", { type: "text", maxlength: "200", value: w.title, placeholder: "비우면 제목 없이" });
    title.addEventListener("input", function () {
      var cur = byId(w.id);
      if (cur) change(function () { cur.title = title.value; }, "title:" + w.id);
    });
    inspector.appendChild(el("label", { cls: "ed-field", "data-in-tab": "content" }, [el("span", { text: "제목" }), title]));

    // 보이는 곳
    var scopes = [[screen, "이 화면에서만"], ["all", "모든 화면"]];
    if (w.scope !== screen && w.scope !== "all") scopes.push([w.scope, scopeName(w.scope)]);
    var fs = el("fieldset", { cls: "ed-field segment", "data-in-tab": "look" }, [el("legend", { text: "보이는 곳" })]);
    scopes.forEach(function (s) {
      var r = el("input", { type: "radio", name: "ed-scope", value: s[0] });
      if (w.scope === s[0]) r.checked = true;
      r.addEventListener("change", function () {
        var cur = byId(w.id);
        if (r.checked && cur) change(function () { cur.scope = s[0]; });
      });
      fs.appendChild(el("label", { cls: "choice" }, [r, el("span", { text: s[1] })]));
    });
    inspector.appendChild(fs);

    // 기기에서 숨기기: 지우지 않고 그 폭에서만 감춘다
    var dfs = el("fieldset", { cls: "ed-field ed-devices-hide", "data-in-tab": "look" }, [el("legend", { text: "이 기기에서 숨기기" })]);
    var devs = {};
    DEVICES.forEach(function (d) {
      var c = el("input", { type: "checkbox", value: d[0] });
      c.checked = hiddenOn(w).indexOf(d[0]) >= 0;
      c.addEventListener("change", function () {
        var cur = byId(w.id);
        if (!cur) return;
        var next = DEVICES.map(function (x) { return x[0]; }).filter(function (x) { return devs[x].checked; });
        if (next.length === DEVICES.length) {
          c.checked = false;
          statusEl.textContent = "세 기기 모두에서 감추려면 「숨기기」를 누르세요.";
          return;
        }
        change(function () { cur.hide_on = next; });
      });
      devs[d[0]] = c;
      dfs.appendChild(el("label", { cls: "choice" }, [c, el("span", { text: d[1] })]));
    });
    dfs.appendChild(el("p", { cls: "hint", text: "지우지 않고 고른 기기 폭에서만 감춥니다. 위의 기기 단추로 바꿔 보며 확인하세요." }));
    inspector.appendChild(dfs);

    // 설정 칸: 위젯 화면과 같은 schema 폼
    var f = el("form", { cls: "widget-form ed-config", "data-kind": w.kind }, [
      el("input", { type: "hidden", name: "_csrf", value: csrf }),
      el("div", { "data-schema-fields": "" }),
      el("textarea", { "data-config": "", hidden: "", "aria-hidden": "true", tabindex: "-1" })
    ]);
    f.addEventListener("submit", function (e) { e.preventDefault(); });
    f.querySelector("[data-config]").value = JSON.stringify(w.config || {});
    inspector.appendChild(el("h3", { cls: "ed-group", "data-in-tab": "content", text: "설정" }));
    f.setAttribute("data-in-tab", "content");
    inspector.appendChild(f);
    if (window.MaruForm && data.types[w.kind]) {
      var mf = new window.MaruForm(f, data);
      if (looks) mf.skip = { tone: 1, space: 1, align: 1, style: STYLES[w.kind] ? 1 : 0 };
      var starting = true;
      mf.onsave = function (v) {
        if (starting) return;
        var cur = byId(w.id);
        if (!cur) return;
        var next = JSON.parse(JSON.stringify(v));
        if (JSON.stringify(next) === JSON.stringify(cur.config)) return;
        change(function () { cur.config = next; }, "config:" + w.id);
      };
      mf.start(false);
      starting = false;
    } else {
      f.appendChild(el("p", { cls: "hint", text: "이 블록은 따로 고를 설정이 없습니다." }));
    }

    var acts = el("div", { cls: "ed-insp-acts" }, [
      el("button", { type: "button", cls: "secondary", "data-act": w.enabled ? "hide" : "show", text: w.enabled ? "숨기기" : "다시 보이기" }),
      el("button", { type: "button", cls: "secondary", "data-act": "copy", text: "복제" }),
      el("button", { type: "button", cls: "warn", "data-act": "remove", text: "지우기" })
    ]);
    acts.addEventListener("click", function (e) {
      var b = e.target.closest("[data-act]");
      if (b) blockAction(b.getAttribute("data-act"), w.id);
    });
    inspector.appendChild(acts);
    form = { __id: w.id, title: title, acts: acts, note: note, devs: devs };
    if (looks) showInspTab();
    else inspector.querySelectorAll("[data-in-tab]").forEach(function (n) { n.hidden = false; });
    syncInspector(w);
  }
  // 같은 블록을 고친 채로 다시 그릴 때는 칸을 새로 만들지 않는다(입력 중인 칸의 포커스를 지킨다).
  function syncInspector(w) {
    if (form && isSite(form.__id)) {
      var key = form.__id.slice(5), msgs = [];
      if (invalid[form.__id]) {
        msgs.push(key === "name" ? "사이트 이름은 1~80자로 적어야 저장됩니다."
          : key === "description" ? "사이트 설명은 300자까지입니다."
          : "이름이 빈 줄, 40자가 넘는 이름, 맞지 않는 링크(https:// 또는 / 로 시작)가 있으면 저장되지 않습니다.");
      }
      if (!nodeOf(form.__id)) msgs.push("지금 화면에는 이 부분이 보이지 않습니다. 고치면 이 부분을 그리는 화면·테마에 보입니다.");
      form.note.textContent = msgs.join(" ");
      form.note.hidden = !msgs.length;
      return;
    }
    if (!form || !w) return;
    var notes = [];
    if (!w.enabled) notes.push("숨긴 블록입니다. 손님에게 보이지 않습니다.");
    if (invalid[w.id]) notes.push("설정이 덜 돼서 화면에 아직 안 보입니다. 빈 칸을 채우세요.");
    if (w.scope !== screen) notes.push(scopeName(w.scope) + "에 함께 보이는 블록입니다. 고치면 그 화면들도 바뀝니다.");
    if (offHere(w)) notes.push("지금 보는 " + deviceName(viewing()) + " 화면에서는 숨긴 블록이라 캔버스에 보이지 않습니다.");
    if (form.devs) Object.keys(form.devs).forEach(function (d) { form.devs[d].checked = hiddenOn(w).indexOf(d) >= 0; });
    form.note.textContent = notes.join(" ");
    form.note.hidden = !notes.length;
    var hide = form.acts.querySelector("[data-act=hide], [data-act=show]");
    if (hide) { hide.setAttribute("data-act", w.enabled ? "hide" : "show"); hide.textContent = w.enabled ? "숨기기" : "다시 보이기"; }
  }

  // ---- 오른쪽: 사이트 공통 부분 (ADR-0145) ----
  function drawSiteInspector() {
    var id = selected, p = partOf(id), key = p[0];
    inspector.textContent = "";
    inspector.appendChild(el("div", { cls: "ed-insp-head" }, [
      el("h2", { text: p[1] }),
      el("p", { cls: "hint", text: p[2] + " 모든 화면에 함께 바뀝니다." })
    ]));
    var note = el("p", { cls: "ed-insp-note", "data-note": "" });
    inspector.appendChild(note);
    form = { __id: id, note: note };
    if (key === "name" || key === "description") {
      var input = key === "name"
        ? el("input", { type: "text", maxlength: "80" })
        : el("textarea", { rows: "3", maxlength: "300" });
      input.value = site[key] || "";
      input.addEventListener("input", function () { change(function () { site[key] = input.value; }, "site:" + key); });
      inspector.appendChild(el("label", { cls: "ed-field" }, [el("span", { text: p[1] }), input]));
      var n = nodeOf(id);
      if (n && n.hasAttribute("data-maru-f")) inspector.appendChild(el("p", { cls: "hint", text: "화면의 글자를 한 번 더 누르면 그 자리에서 고칩니다." }));
    } else {
      drawMenuEditor(key.slice(5));
    }
    syncInspector(null);
  }

  function destOf(link) {
    if (!link || link.kind === "home") return "home";
    if (link.kind === "url") return "url";
    return link.kind + ":" + link.slug;
  }
  function menuRows(loc) {
    var rows = [];
    var row = function (it, child) { return { label: it.label, link: it.link || { kind: "home" }, child: child }; };
    (site.menus[loc] || []).forEach(function (it) {
      rows.push(row(it, false));
      (it.children || []).forEach(function (c) { rows.push(row(c, true)); });
    });
    return rows;
  }
  // 「하위」 줄은 바로 앞의 윗줄 아래로 들어간다(2단까지).
  function treeOf(rows) {
    var out = [];
    rows.forEach(function (r) {
      var item = { label: r.label, link: r.link, children: [] };
      if (r.child && out.length) out[out.length - 1].children.push(item); else out.push(item);
    });
    return out;
  }

  function drawMenuEditor(loc) {
    var rows = menuRows(loc);
    var commit = function (mark) { change(function () { site.menus[loc] = treeOf(rows); }, mark); };
    var rebuild = function () { form = null; drawInspector(); };
    var list = el("ol", { cls: "ed-menu" });
    if (!rows.length) list.appendChild(el("li", { cls: "ed-layer-empty", text: "메뉴가 없습니다." }));
    rows.forEach(function (r, i) {
      var n = i + 1;
      var label = el("input", { type: "text", maxlength: "40", "aria-label": n + "번째 메뉴 이름" });
      label.value = r.label;
      label.addEventListener("input", function () { r.label = label.value; commit("menu:" + loc + ":label:" + i); });
      var sel = el("select", { "aria-label": n + "번째 메뉴 누르면 갈 곳" }, [el("option", { value: "home", text: "첫 화면" })]);
      var group = function (name, kind, items) {
        if (!items.length) return;
        var g = el("optgroup", { label: name });
        items.forEach(function (x) { g.appendChild(el("option", { value: kind + ":" + x.id, text: x.label })); });
        sel.appendChild(g);
      };
      group("게시판", "board", data.boards);
      group("페이지", "page", data.pages || []);
      var dest = destOf(r.link);
      if (dest !== "home" && dest !== "url" && !sel.querySelector('option[value="' + dest.replace(/"/g, "") + '"]')) {
        sel.appendChild(el("option", { value: dest, text: "지금 값: " + r.link.slug }));
      }
      sel.appendChild(el("option", { value: "url", text: "직접 링크 쓰기…" }));
      sel.value = dest;
      var url = el("input", { type: "text", maxlength: "300", placeholder: "https://… 또는 /recent", "aria-label": n + "번째 메뉴 링크 주소", spellcheck: "false" });
      url.value = r.link.kind === "url" ? r.link.href || "" : "";
      url.hidden = dest !== "url";
      sel.addEventListener("change", function () {
        var v = sel.value, at = v.indexOf(":");
        if (v === "url") r.link = { kind: "url", href: url.value };
        else if (v === "home") r.link = { kind: "home" };
        else r.link = { kind: v.slice(0, at), slug: v.slice(at + 1) };
        url.hidden = v !== "url";
        commit();
        if (v === "url") url.focus();
      });
      url.addEventListener("input", function () { r.link = { kind: "url", href: url.value }; commit("menu:" + loc + ":url:" + i); });
      var child = el("input", { type: "checkbox" });
      child.checked = r.child;
      child.disabled = i === 0;
      child.addEventListener("change", function () { r.child = child.checked; commit(); rebuild(); });
      var move = function (to) {
        return function () {
          if (to < 0 || to >= rows.length) return;
          var x = rows[i]; rows[i] = rows[to]; rows[to] = x;
          if (rows[0]) rows[0].child = false;
          commit(); rebuild();
        };
      };
      var up = el("button", { type: "button", cls: "secondary", title: "위로", "aria-label": n + "번째 메뉴 위로", text: "↑" });
      var down = el("button", { type: "button", cls: "secondary", title: "아래로", "aria-label": n + "번째 메뉴 아래로", text: "↓" });
      var del = el("button", { type: "button", cls: "warn", title: "빼기", "aria-label": n + "번째 메뉴 빼기", text: "빼기" });
      up.disabled = i === 0;
      down.disabled = i === rows.length - 1;
      up.addEventListener("click", move(i - 1));
      down.addEventListener("click", move(i + 1));
      del.addEventListener("click", function () {
        rows.splice(i, 1);
        if (rows[0]) rows[0].child = false;
        commit(); rebuild();
      });
      list.appendChild(el("li", { cls: "ed-menu-row" + (r.child ? " is-child" : "") }, [
        el("div", { cls: "ed-menu-line" }, [label, up, down, del]),
        el("div", { cls: "ed-menu-line" }, [sel, el("label", { cls: "choice compact", title: "바로 위 메뉴 아래에 붙입니다(2단까지)" }, [child, el("span", { text: "하위" })])]),
        url
      ]));
    });
    inspector.appendChild(list);
    var add = el("button", { type: "button", cls: "secondary", text: "+ 메뉴 더하기" });
    add.addEventListener("click", function () {
      rows.push({ label: "새 메뉴", link: { kind: "home" }, child: false });
      commit(); rebuild();
    });
    inspector.appendChild(el("div", { cls: "ed-insp-acts" }, [add]));
    inspector.appendChild(el("p", { cls: "hint", text: "「하위」를 켜면 바로 위 메뉴를 눌렀을 때 펼쳐지는 아래 메뉴가 됩니다(2단까지). 화면의 메뉴 글자를 한 번 더 누르면 이름을 그 자리에서 고칩니다." }));
  }

  // ---- 위 막대 ----
  root.querySelectorAll("[data-device]").forEach(function (b) {
    b.addEventListener("click", function () {
      stage.setAttribute("data-device", b.getAttribute("data-device"));
      root.querySelectorAll("[data-device]").forEach(function (x) { x.setAttribute("aria-pressed", String(x === b)); });
      drawLayers();
      if (!isSite(selected)) syncInspector(selected && byId(selected));
      setTimeout(markSelection, 260);
    });
  });
  root.querySelectorAll("[data-tab]").forEach(function (t) {
    t.addEventListener("click", function () {
      var id = t.getAttribute("data-tab");
      root.querySelectorAll("[data-tab]").forEach(function (x) { x.setAttribute("aria-selected", String(x === t)); });
      root.querySelectorAll("[data-panel]").forEach(function (p) { p.hidden = p.getAttribute("data-panel") !== id; });
    });
  });
  var pick = root.querySelector("[data-screen-pick]");
  if (pick) pick.addEventListener("change", function () {
    if (dirtyCount() && !confirm("발행하지 않은 고침은 이 브라우저에 남겨 두고 다른 화면으로 갑니다.")) { pick.value = screen; return; }
    location.href = "/admin/editor?screen=" + encodeURIComponent(pick.value);
  });
  var view = root.querySelector("[data-view]");
  var current = data.screens.filter(function (s) { return s.id === screen; })[0];
  if (view) view.href = screen === "home" ? "/" : screen.indexOf("page:") === 0 ? "/p/" + encodeURIComponent(screen.slice(5)) : "/b/" + encodeURIComponent(screen.slice(6));
  undoBtn.addEventListener("click", undo);
  redoBtn.addEventListener("click", redo);

  function typing(e) {
    var t = e.target;
    return t && (t.isContentEditable || /^(INPUT|TEXTAREA|SELECT)$/.test(t.tagName));
  }
  function keys(e) {
    var mod = e.ctrlKey || e.metaKey;
    if (mod && !typing(e) && (e.key === "z" || e.key === "Z")) { e.preventDefault(); if (e.shiftKey) redo(); else undo(); }
    else if (mod && !typing(e) && (e.key === "y" || e.key === "Y")) { e.preventDefault(); redo(); }
    else if (mod && (e.key === "s" || e.key === "S")) { e.preventDefault(); statusEl.textContent = "고친 것은 이 브라우저에 저장돼 있습니다. 사이트에 내보내려면 「발행」을 누르세요."; }
    else if ((e.key === "Delete" || e.key === "Backspace") && selected && !typing(e)) { e.preventDefault(); blockAction("remove", selected); }
    else if (e.key === "Escape" && selected) select(null);
  }
  document.addEventListener("keydown", keys);

  // ---- 발행 ----
  publishBtn.addEventListener("click", function () {
    var seen = {}, twin = null;
    widgets.forEach(function (w) {
      var key = JSON.stringify([w.kind, w.region, w.scope, w.title, w.config]);
      if (seen[key] && !twin) twin = w;
      seen[key] = true;
    });
    if (twin) { select(twin.id); statusEl.textContent = "똑같은 블록이 둘 있습니다. 하나를 고치거나 지운 뒤 발행하세요."; return; }
    var bad = widgets.filter(function (w) { return invalid[w.id]; })[0];
    if (bad) { select(bad.id); statusEl.textContent = "설정이 덜 된 블록이 있습니다. 채우거나 지운 뒤 발행하세요."; return; }
    if (designBad) { root.querySelector("[data-tab=design]").click(); statusEl.textContent = "쓸 수 없는 디자인 값이 있습니다. 고친 뒤 발행하세요."; return; }
    var badPart = Object.keys(invalid).filter(isSite)[0];
    if (badPart) { select(badPart); statusEl.textContent = "저장할 수 없는 사이트 공통 값이 있습니다. 고친 뒤 발행하세요."; return; }
    publishBtn.disabled = true;
    statusEl.textContent = "발행하는 중…";
    fetch("/admin/editor/publish", {
      method: "POST",
      credentials: "same-origin",
      headers: { "content-type": "application/json", "x-csrf-token": csrf },
      body: JSON.stringify({ screen: screen, widgets: widgets, site: siteBody(), tokens: Object.keys(designPatch()).length ? designPatch() : null })
    }).then(function (r) { return r.json().then(function (j) { return { ok: r.ok, j: j }; }); }).then(function (res) {
      if (!res.ok) throw new Error((res.j.error && res.j.error.message) || "발행하지 못했습니다.");
      try { localStorage.removeItem(STORE); } catch (e) { /* 무시 */ }
      saved = JSON.stringify(widgets);
      savedSite = JSON.stringify(site);
      savedDesign = JSON.stringify(design);
      var id = res.j.data && res.j.data.changeset;
      location.href = "/admin/editor?screen=" + encodeURIComponent(screen) + (id ? "&done=" + encodeURIComponent(id) : "");
    }).catch(function (e) {
      statusEl.textContent = e.message;
      publishBtn.disabled = false;
    });
  });

  window.addEventListener("beforeunload", function (e) {
    if (dirtyCount()) { e.preventDefault(); e.returnValue = ""; }
  });

  // ---- 시작 ----
  drawPalette();
  drawLayers();
  drawDesign();
  frame.addEventListener("load", function () { decorate(); });
  if (fdoc() && fdoc().readyState === "complete" && fdoc().querySelector("[data-maru-region]")) decorate();

  // 지난번 초안이 있으면 이어서 고칠지 묻는다. 그 사이 사이트가 바뀌었으면 버린다.
  var restore = root.querySelector("[data-restore]");
  try {
    var kept = JSON.parse(localStorage.getItem(STORE) || "null");
    var keptSite = kept && kept.site ? JSON.stringify(kept.site) : savedSite;
    var keptDesign = kept && kept.design ? JSON.stringify(kept.design) : savedDesign;
    if (kept && kept.base === hash(saved + savedSite + savedDesign) && (JSON.stringify(kept.widgets) !== saved || keptSite !== savedSite || keptDesign !== savedDesign)) {
      restore.hidden = false;
      restore.querySelector("[data-restore-yes]").addEventListener("click", function () {
        restore.hidden = true;
        change(function () { widgets = kept.widgets; site = JSON.parse(keptSite); design = JSON.parse(keptDesign); });
        drawDesign();
      });
      restore.querySelector("[data-restore-no]").addEventListener("click", function () {
        restore.hidden = true;
        try { localStorage.removeItem(STORE); } catch (e) { /* 무시 */ }
      });
    } else if (kept) {
      localStorage.removeItem(STORE);
    }
  } catch (e) { /* 저장소를 못 쓰면 묻지 않는다 */ }
  if (current) document.title = "화면 편집 · " + current.label;
})();
