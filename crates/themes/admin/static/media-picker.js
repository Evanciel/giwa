// 그림·링크 고르기 (ADR-0147). 화면 편집기 캔버스와 설정 칸(admin-ui.js Form)이 함께 쓴다.
//   MaruMedia.open({ url, alt, csrf, required, onPick: function (url, alt) {} })
//     올려 둔 그림을 고르거나 새로 올린다. alt 가 null 이면 그림 설명 칸을 보이지 않는다.
//     url 이 "" 로 오면 「그림 빼기」다(required 면 단추가 없다).
//   MaruLinks.open({ link, targets: { pages: [{id,label}], boards: [{id,label}] }, onPick: function (link) {} })
//     첫 화면·게시판·페이지·바깥 주소 가운데 고른다. link 가 "" 로 오면 「링크 빼기」다.
(function () {
  "use strict";
  function el(tag, attrs, kids) {
    var n = document.createElement(tag);
    if (attrs) Object.keys(attrs).forEach(function (k) {
      if (k === "text") n.textContent = attrs[k];
      else if (k === "cls") n.className = attrs[k];
      else if (attrs[k] !== null && attrs[k] !== undefined) n.setAttribute(k, attrs[k]);
    });
    (kids || []).forEach(function (c) { if (c) n.appendChild(c); });
    return n;
  }
  // 대화 상자는 하나만 연다. 닫으면 지운다(다시 열 때 새로 그린다).
  function dialog(title, cls) {
    var d = el("dialog", { cls: "mp " + cls, "aria-labelledby": "mp-title" });
    var close = el("button", { type: "button", cls: "mp-close", "aria-label": "닫기", text: "×" });
    close.addEventListener("click", function () { d.close(); });
    d.appendChild(el("header", { cls: "mp-head" }, [el("h2", { id: "mp-title", text: title }), close]));
    d.addEventListener("close", function () { d.remove(); });
    document.body.appendChild(d);
    return d;
  }
  function thumbOf(m) {
    var v = (m.variants || []).filter(function (x) { return x.url; }).sort(function (a, b) { return a.width - b.width; })[0];
    return v ? v.url : m.url;
  }

  // ---- 그림 ----
  function openMedia(opts) {
    var d = dialog("그림 고르기", "mp-media");
    var picked = opts.url || "";
    var status = el("p", { cls: "mp-status", role: "status" });
    var grid = el("ul", { cls: "mp-grid", "aria-label": "올린 그림" });
    var more = el("button", { type: "button", cls: "secondary small mp-more", text: "더 보기", hidden: "" });
    var file = el("input", { type: "file", accept: "image/jpeg,image/png,image/gif,image/webp", cls: "visually-hidden", tabindex: "-1" });
    var upBtn = el("button", { type: "button", cls: "secondary", text: "컴퓨터에서 올리기" });
    var drop = el("div", { cls: "mp-drop" }, [
      el("span", { text: "그림 파일을 여기로 끌어다 놓거나" }), upBtn, file,
      el("small", { text: "jpg·png·gif·webp, 10MB 이하. 위치 정보는 지우고 저장합니다." })
    ]);
    var alt = null;
    if (opts.alt !== null && opts.alt !== undefined) {
      alt = el("input", { type: "text", maxlength: "200", id: "mp-alt" });
      alt.value = opts.alt || "";
    }
    var use = el("button", { type: "button", text: "이 그림 쓰기" });
    var remove = !opts.required && opts.url ? el("button", { type: "button", cls: "secondary mp-remove", text: "그림 빼기" }) : null;
    var body = el("div", { cls: "mp-body" }, [drop, status, grid, more]);
    d.appendChild(body);
    if (alt) {
      d.appendChild(el("div", { cls: "mp-alt" }, [
        el("label", { for: "mp-alt", text: "그림 설명" }), alt,
        el("p", { cls: "hint", text: "화면을 읽어 주는 프로그램과 검색에 쓰입니다. 꾸밈 그림이면 비워 두세요." })
      ]));
    }
    d.appendChild(el("footer", { cls: "mp-foot" }, [remove, el("span", { cls: "mp-gap" }), use]));

    var byUrl = {};
    var mark = function () {
      grid.querySelectorAll("[data-url]").forEach(function (b) { b.setAttribute("aria-pressed", String(b.getAttribute("data-url") === picked)); });
      use.disabled = !picked;
    };
    var card = function (m, first) {
      byUrl[m.url] = m;
      var b = el("button", { type: "button", cls: "mp-item", "data-url": m.url, title: (m.alt_text || "") + " " + m.width + "×" + m.height }, [
        el("img", { src: thumbOf(m), alt: m.alt_text || "", loading: "lazy", width: "160", height: "120" })
      ]);
      b.addEventListener("click", function () {
        picked = m.url;
        // 그림 보관함에 적어 둔 설명이 있고 칸이 비었으면 채운다.
        if (alt && !alt.value && m.alt_text) alt.value = m.alt_text;
        mark();
      });
      b.addEventListener("dblclick", function () { picked = m.url; finish(); });
      var li = el("li", {}, [b]);
      if (first && grid.firstChild) grid.insertBefore(li, grid.firstChild); else grid.appendChild(li);
    };
    var page = 1;
    var load = function () {
      more.disabled = true;
      status.textContent = "그림을 불러오는 중…";
      fetch("/admin/editor/media?page=" + page, { credentials: "same-origin", headers: { "x-csrf-token": opts.csrf || "" } })
        .then(function (r) { return r.json().then(function (j) { return { ok: r.ok, j: j }; }); })
        .then(function (res) {
          if (!res.ok) throw new Error((res.j.error && res.j.error.message) || "그림 목록을 받지 못했습니다");
          var items = res.j.data.items || [];
          items.forEach(function (m) { if (!byUrl[m.url]) card(m); });
          status.textContent = grid.children.length ? "" : "아직 올린 그림이 없습니다. 위에서 올려 주세요.";
          more.hidden = !res.j.data.more;
          more.disabled = false;
          page += 1;
          mark();
        })
        .catch(function (e) { status.textContent = e.message; more.disabled = false; });
    };
    more.addEventListener("click", load);
    var upload = function (f) {
      if (!f) return;
      if (!/^image\/(jpeg|png|gif|webp)$/.test(f.type)) { status.textContent = "jpg·png·gif·webp 그림만 올릴 수 있습니다."; return; }
      upBtn.disabled = true;
      status.textContent = "「" + f.name + "」 올리는 중…";
      fetch("/admin/media?filename=" + encodeURIComponent(f.name), {
        method: "POST", body: f, credentials: "same-origin", headers: { "x-csrf-token": opts.csrf || "" }
      }).then(function (r) { return r.json(); }).then(function (j) {
        var m = j.data;
        if (!m || !m.url) throw new Error((j.error && j.error.message) || "올리지 못했습니다");
        card(m, true);
        picked = m.url;
        status.textContent = "올렸습니다. 「이 그림 쓰기」를 누르세요.";
        mark();
        if (alt) alt.focus();
      }).catch(function (e) { status.textContent = e.message; }).then(function () {
        upBtn.disabled = false;
        file.value = "";
      });
    };
    upBtn.addEventListener("click", function () { file.click(); });
    file.addEventListener("change", function () { upload(file.files && file.files[0]); });
    d.addEventListener("dragover", function (e) { e.preventDefault(); drop.classList.add("is-over"); });
    d.addEventListener("dragleave", function (e) { if (!d.contains(e.relatedTarget)) drop.classList.remove("is-over"); });
    d.addEventListener("drop", function (e) {
      e.preventDefault();
      drop.classList.remove("is-over");
      upload(e.dataTransfer && e.dataTransfer.files && e.dataTransfer.files[0]);
    });
    var finish = function () {
      if (!picked) return;
      var a = alt ? alt.value.trim() : null;
      d.close();
      opts.onPick(picked, a);
    };
    use.addEventListener("click", finish);
    if (remove) remove.addEventListener("click", function () { d.close(); opts.onPick("", alt ? "" : null); });
    d.showModal();
    mark();
    load();
  }

  // ---- 링크 ----
  // 위젯 링크 칸은 사이트 안 주소(`/…`)나 https 주소 글자다. 게시판은 /b/<주소>, 페이지는 /p/<주소>.
  function openLinks(opts) {
    var d = dialog("링크 고르기", "mp-links");
    var t = opts.targets || {};
    var now = opts.link || "";
    var sel = el("select", { id: "lp-dest" }, [el("option", { value: "/", text: "첫 화면" })]);
    var group = function (name, prefix, items) {
      if (!items || !items.length) return;
      var g = el("optgroup", { label: name });
      items.forEach(function (x) { g.appendChild(el("option", { value: prefix + encodeURIComponent(x.id), text: x.label })); });
      sel.appendChild(g);
    };
    group("게시판", "/b/", t.boards);
    group("페이지", "/p/", t.pages);
    // 이 화면의 구역 (ADR-0156): 값이 이미 완성된 주소(/…#s-이름)다.
    if (t.anchors && t.anchors.length) {
      var ag = el("optgroup", { label: "이 화면의 구역" });
      t.anchors.forEach(function (x) { ag.appendChild(el("option", { value: x.value, text: x.label })); });
      sel.appendChild(ag);
    }
    sel.appendChild(el("option", { value: "url", text: "직접 주소 쓰기…" }));
    var url = el("input", { type: "text", id: "lp-url", maxlength: "300", spellcheck: "false", placeholder: "https://… 또는 /recent" });
    var urlRow = el("div", { cls: "mp-alt" }, [el("label", { for: "lp-url", text: "주소" }), url,
      el("p", { cls: "hint", text: "사이트 안이면 / 로 시작하고, 바깥 사이트면 https:// 로 시작합니다." })]);
    var known = Array.prototype.some.call(sel.options, function (o) { return o.value === now; });
    sel.value = known ? now : (now ? "url" : "/");
    url.value = known ? "" : now;
    urlRow.hidden = sel.value !== "url";
    sel.addEventListener("change", function () {
      urlRow.hidden = sel.value !== "url";
      if (sel.value === "url") url.focus();
    });
    var use = el("button", { type: "button", text: "이 링크 쓰기" });
    var remove = now ? el("button", { type: "button", cls: "secondary mp-remove", text: "링크 빼기" }) : null;
    var status = el("p", { cls: "mp-status", role: "status" });
    d.appendChild(el("div", { cls: "mp-body" }, [
      el("div", { cls: "mp-alt" }, [el("label", { for: "lp-dest", text: "누르면 갈 곳" }), sel]), urlRow, status
    ]));
    d.appendChild(el("footer", { cls: "mp-foot" }, [remove, el("span", { cls: "mp-gap" }), use]));
    use.addEventListener("click", function () {
      var v = sel.value === "url" ? url.value.trim() : sel.value;
      if (!/^(\/|https:\/\/)/.test(v)) { status.textContent = "주소는 / 나 https:// 로 시작해야 합니다."; url.focus(); return; }
      d.close();
      opts.onPick(v);
    });
    if (remove) remove.addEventListener("click", function () { d.close(); opts.onPick(""); });
    d.showModal();
    sel.focus();
  }

  window.MaruMedia = { open: openMedia };
  window.MaruLinks = { open: openLinks };
})();
