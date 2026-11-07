// 관리 화면 도우미 (ADR-0137). 의존성 없는 바닐라 JS, CSP `script-src 'self'` 안에서 돈다.
// 1) 위젯 설정 폼: 위젯 종류의 JSON Schema 로 칸을 그리고, 값은 숨은 「설정 원문」 textarea 에 JSON 으로 쓴다.
// 2) 메뉴 편집: 위아래 단추로 줄을 옮기고 순서 값을 다시 매긴다. 가리킬 곳 고르기를 종류·주소 칸에 옮긴다.
// JS 가 꺼져 있어도 폼은 그대로 제출된다(원문 칸과 기본 칸이 남는다).
(function () {
  "use strict";

  var LABELS = {
    board: "게시판", limit: "보일 개수", view: "모양", style: "모양", days: "기간(일)",
    pages: "넘겨 볼 쪽 수", auto: "저절로 넘기기", show_date: "날짜 보이기", columns: "한 줄 칸 수",
    location: "어느 메뉴", text: "글", title: "제목", body: "내용", link: "링크", tone: "바탕",
    items: "항목", rows: "줄", groups: "묶음", nodes: "조직", children: "아래 조직", slides: "슬라이드",
    tabs: "탭", plans: "요금제", links: "바로가기", position: "뜨는 자리", starts_at: "보이기 시작",
    ends_at: "보이기 끝", address: "주소", phone: "전화번호", intro: "소개 글", placeholder: "검색 칸 안내 글",
    label: "이름", source: "고를 글", alt: "그림 설명", image: "그림", image_alt: "그림 설명",
    dismiss_days: "「다시 보지 않기」 기간(일)", button: "단추", button2: "두 번째 단추",
    eyebrow: "제목 위 작은 글", lead: "머리말", value: "숫자", name: "이름", quote: "한마디", role: "역할",
    a: "답", q: "질문", note: "덧붙임", time: "시간", parking: "주차 안내", transit: "대중교통 안내",
    price: "가격", featured: "추천으로 돋보이기", features: "들어 있는 것", period: "기간", date: "날짜",
    bio: "소개", flip: "그림을 오른쪽에"
  };
  var VALUE_LABELS = {
    tone: { plain: "기본", soft: "옅은 바탕", strong: "진한 바탕" },
    position: { center: "가운데", "bottom-left": "왼쪽 아래", "bottom-right": "오른쪽 아래" },
    location: { header: "머리글 메뉴", footer: "바닥글 메뉴" }
  };
  var FIRST = ["name", "label", "q", "title", "eyebrow", "text", "lead", "body", "a", "quote", "role", "date", "value",
    "price", "board", "source", "view", "style", "limit", "days", "columns", "location", "image", "image_alt", "link",
    "items", "slides", "tabs", "button", "button2"];
  var LONG = { body: 6, quote: 3, bio: 3, a: 3, intro: 2, transit: 2, lead: 2 };

  function el(tag, attrs, kids) {
    var n = document.createElement(tag);
    if (attrs) Object.keys(attrs).forEach(function (k) {
      if (k === "text") n.textContent = attrs[k];
      else if (k === "cls") n.className = attrs[k];
      else n.setAttribute(k, attrs[k]);
    });
    (kids || []).forEach(function (c) { if (c) n.appendChild(c); });
    return n;
  }
  var uid = 0;
  function nextId() { uid += 1; return "sf-" + uid; }

  // ---- schema 풀기 ----
  function defsOf(root) { return root.$defs || root.definitions || {}; }
  function resolve(s, root) {
    if (!s) return {};
    var out = {};
    var base = s;
    if (s.$ref) base = defsOf(root)[s.$ref.split("/").pop()] || {};
    else if (s.allOf && s.allOf.length === 1) base = resolve(s.allOf[0], root);
    else if (s.anyOf || s.oneOf) {
      var alt = (s.anyOf || s.oneOf).filter(function (x) { return x.type !== "null"; });
      if (alt.length === 1) base = resolve(alt[0], root);
    }
    Object.keys(base).forEach(function (k) { out[k] = base[k]; });
    Object.keys(s).forEach(function (k) { if (k !== "$ref" && k !== "allOf") out[k] = s[k]; });
    if (Array.isArray(out.type)) out.type = out.type.filter(function (t) { return t !== "null"; })[0];
    if (!out.type && out.properties) out.type = "object";
    return out;
  }
  function blank(s, root) {
    s = resolve(s, root);
    if (s.default !== undefined && s.default !== null) return JSON.parse(JSON.stringify(s.default));
    switch (s.type) {
      case "boolean": return false;
      case "integer": case "number": return s.minimum || 0;
      case "array": return [];
      case "object":
        var o = {};
        Object.keys(s.properties || {}).forEach(function (k) { o[k] = blank(s.properties[k], root); });
        return o;
      default: return "";
    }
  }
  // 「`list`(제목 줄), `card`(사진 카드)」 또는 「lines(제목 줄, 기본)·cards(그림 카드)」 꼴의 고르기
  function choicesOf(key, s) {
    if (s.enum) return s.enum.map(function (v) { return [v, (VALUE_LABELS[key] || {})[v] || v]; });
    if (VALUE_LABELS[key]) return Object.keys(VALUE_LABELS[key]).map(function (v) { return [v, VALUE_LABELS[key][v]]; });
    var d = s.description || "", out = [], m;
    var re = /`([a-z0-9_-]+)`(?:\(([^)]*)\))?/g;
    while ((m = re.exec(d))) out.push([m[1], (m[2] || m[1]).split(",")[0].trim()]);
    if (out.length < 2) {
      out = [];
      re = /\b([a-z_]+)\(([^)]*)\)/g;
      while ((m = re.exec(d))) out.push([m[1], m[2].split(",")[0].trim()]);
    }
    return out.length >= 2 ? out : null;
  }
  function range(s) {
    var lo = s.minimum, hi = s.maximum;
    var m = /(\d+)\s*~\s*(\d+)/.exec(s.description || "");
    if (m) { if (lo === undefined) lo = +m[1]; if (hi === undefined) hi = +m[2]; }
    return [lo, hi];
  }
  // 설명에서 개발 메모(GAP·ADR 번호, 코드 표시)를 덜어 낸다.
  function hintOf(s) {
    return (s.description || "")
      .replace(/\s*\((?:GAP|ADR)[^)]*\)/g, "")
      .replace(/`/g, "")
      .trim();
  }

  // ---- 칸 그리기 ----
  function Form(form, data) {
    this.form = form;
    this.data = data;
    this.box = form.querySelector("[data-schema-fields]");
    this.raw = form.querySelector("[data-config]");
    this.csrf = (form.querySelector("input[name=_csrf]") || {}).value || "";
  }
  Form.prototype.kind = function () {
    if (this.form.dataset.kind) return this.form.dataset.kind;
    var r = this.form.querySelector("input[name=kind]:checked");
    return r ? r.value : "";
  };
  Form.prototype.start = function (fresh) {
    var root = this.data.types[this.kind()];
    this.root = root;
    this.box.textContent = "";
    if (!root) return;
    var value = null;
    if (!fresh) try { value = JSON.parse(this.raw.value || "null"); } catch (e) { value = null; }
    if (!value || typeof value !== "object") value = blank(root, root);
    this.value = value;
    var props = root.properties || {};
    if (!Object.keys(props).length) {
      this.box.appendChild(el("p", { cls: "hint", text: "따로 고를 설정이 없습니다." }));
    }
    this.renderObject(this.box, root, value);
    this.save();
  };
  Form.prototype.save = function () {
    if (!this.value) return;
    this.raw.value = JSON.stringify(this.value, null, 2);
    if (this.onsave) this.onsave(this.value);
  };
  Form.prototype.redraw = function () { this.save(); this.start(false); };

  Form.prototype.renderObject = function (box, schema, obj) {
    var self = this, s = resolve(schema, this.root);
    // schema 는 이름 차례라, 사람이 먼저 볼 칸(제목·글·게시판·모양)을 앞으로 당긴다.
    var rank = function (k) { var i = FIRST.indexOf(k); return i < 0 ? (k === "tone" ? 999 : 100) : i; };
    Object.keys(s.properties || {}).sort(function (a, b) { return rank(a) - rank(b); }).forEach(function (key) {
      if (obj[key] === undefined) obj[key] = blank(s.properties[key], self.root);
      box.appendChild(self.field(key, s.properties[key], obj));
    });
  };

  Form.prototype.field = function (key, schema, obj) {
    var self = this, root = this.root, s = resolve(schema, root);
    var id = nextId(), label = LABELS[key] || hintOf(s).split(/[.,(]/)[0].slice(0, 24) || key;
    var hint = hintOf(s), input;
    var set = function (v) { obj[key] = v; self.save(); };
    var wrap = el("div", { cls: "sf-field sf-" + (s.type || "text") });

    if (s.type === "boolean") {
      input = el("input", { type: "checkbox", id: id });
      input.checked = !!obj[key];
      input.addEventListener("change", function () { set(input.checked); });
      wrap.appendChild(el("label", { cls: "choice" }, [input, el("span", { cls: "choice-text", text: label })]));
      return wrap;
    }
    if (s.type === "object") {
      var fs = el("fieldset", { cls: "sf-group" }, [el("legend", { text: label })]);
      if (!obj[key] || typeof obj[key] !== "object") obj[key] = blank(s, root);
      this.renderObject(fs, s, obj[key]);
      return fs;
    }
    if (s.type === "array") return this.list(key, s, obj, label);

    wrap.appendChild(el("label", { "for": id, text: label }));
    var choices = s.type === "string" ? choicesOf(key, s) : null;
    if (key === "board") {
      input = el("select", { id: id });
      input.appendChild(el("option", { value: "", text: s.default === "" ? "모든 게시판" : "고르세요" }));
      this.data.boards.forEach(function (b) { input.appendChild(el("option", { value: b.id, text: b.label })); });
      input.value = obj[key] || "";
      if (input.value !== (obj[key] || "")) {
        input.appendChild(el("option", { value: obj[key], text: obj[key] }));
        input.value = obj[key];
      }
      hint = "";
    } else if (choices) {
      input = el("div", { cls: "segment", role: "radiogroup", "aria-label": label });
      choices.forEach(function (c) {
        var r = el("input", { type: "radio", name: id, value: c[0] });
        r.checked = obj[key] === c[0];
        r.addEventListener("change", function () { if (r.checked) set(c[0]); });
        input.appendChild(el("label", {}, [r, el("span", { text: c[1] })]));
      });
      wrap.appendChild(input);
      return wrap;
    } else if (s.type === "integer" || s.type === "number") {
      var lr = range(s);
      input = el("input", { type: "number", id: id, inputmode: "numeric" });
      if (lr[0] !== undefined) input.min = lr[0];
      if (lr[1] !== undefined) input.max = lr[1];
      input.value = obj[key];
      input.addEventListener("input", function () {
        var n = input.value === "" ? 0 : Number(input.value);
        set(s.type === "integer" ? Math.round(n) : n);
      });
    } else if (key === "starts_at" || key === "ends_at" || s.format === "date-time") {
      input = el("input", { type: "datetime-local", id: id });
      input.value = toLocal(obj[key], this.data.tz);
      input.addEventListener("change", function () { set(fromLocal(input.value, self.data.tz)); });
    } else if (LONG[key] || /(\d{3,})자/.test(s.description || "") && +/(\d{3,})자/.exec(s.description)[1] >= 200) {
      input = el("textarea", { id: id, rows: String(LONG[key] || 3) });
      input.value = obj[key] || "";
      input.addEventListener("input", function () { set(input.value); });
    } else {
      input = el("input", { type: "text", id: id, autocomplete: "off" });
      if (key === "link" || /링크|주소/.test(label) && key !== "address") input.placeholder = "/board/notice 또는 https://…";
      input.value = obj[key] == null ? "" : obj[key];
      input.addEventListener("input", function () { set(input.value); });
    }
    wrap.appendChild(input);
    if (key === "image" && input.tagName === "INPUT") wrap.appendChild(this.upload(input, set));
    if (hint && !choices) wrap.appendChild(el("p", { cls: "hint", text: hint }));
    return wrap;
  };

  // 그림 올리기: 편집기와 같은 `/admin/media` 로 올리고 주소를 칸에 넣는다.
  Form.prototype.upload = function (input, set) {
    var self = this;
    var file = el("input", { type: "file", accept: "image/*", cls: "visually-hidden" });
    var btn = el("button", { type: "button", cls: "secondary small", text: "그림 올리기" });
    var preview = el("img", { cls: "sf-preview", alt: "" });
    var show = function () { preview.hidden = !input.value; if (input.value) preview.src = input.value; };
    btn.addEventListener("click", function () { file.click(); });
    file.addEventListener("change", function () {
      var f = file.files && file.files[0];
      if (!f) return;
      btn.disabled = true; btn.textContent = "올리는 중…";
      fetch("/admin/media?filename=" + encodeURIComponent(f.name), {
        method: "POST", body: f, credentials: "same-origin", headers: { "x-csrf-token": self.csrf }
      }).then(function (r) { return r.json(); }).then(function (j) {
        var url = (j.data && j.data.url) || j.url;
        if (!url) throw new Error((j.error && j.error.message) || "올리지 못했습니다");
        input.value = url; set(url); show();
      }).catch(function (e) { alert(e.message); }).then(function () {
        btn.disabled = false; btn.textContent = "그림 올리기"; file.value = "";
      });
    });
    input.addEventListener("input", show);
    show();
    return el("div", { cls: "sf-upload" }, [btn, file, preview]);
  };

  Form.prototype.list = function (key, s, obj, label) {
    var self = this, root = this.root, item = resolve(s.items, root);
    if (!Array.isArray(obj[key])) obj[key] = [];
    var arr = obj[key];
    var fs = el("fieldset", { cls: "sf-list" }, [el("legend", { text: label })]);
    if (item.type !== "object") {
      // 글자 목록은 한 줄에 하나
      var ta = el("textarea", { rows: "4", "aria-label": label });
      ta.value = arr.join("\n");
      ta.addEventListener("input", function () {
        obj[key] = ta.value.split("\n").map(function (x) { return x.trim(); }).filter(Boolean);
        self.save();
      });
      fs.appendChild(ta);
      fs.appendChild(el("p", { cls: "hint", text: "한 줄에 하나씩 씁니다." }));
      return fs;
    }
    if (!arr.length && s.minItems) arr.push(blank(item, root));
    arr.forEach(function (v, i) {
      var box = el("div", { cls: "sf-item" });
      var bar = el("div", { cls: "sf-item-bar" }, [el("strong", { text: (LABELS[key] || "항목") + " " + (i + 1) })]);
      [["↑", "위로", -1], ["↓", "아래로", 1]].forEach(function (b) {
        var j = i + b[2];
        if (j < 0 || j >= arr.length) return;
        var btn = el("button", { type: "button", cls: "icon-button", title: b[1], "aria-label": (i + 1) + "번째 " + b[1], text: b[0] });
        btn.addEventListener("click", function () { var t = arr[i]; arr[i] = arr[j]; arr[j] = t; self.redraw(); });
        bar.appendChild(btn);
      });
      var del = el("button", { type: "button", cls: "icon-button warn", title: "빼기", "aria-label": (i + 1) + "번째 빼기", text: "✕" });
      del.addEventListener("click", function () { arr.splice(i, 1); self.redraw(); });
      bar.appendChild(del);
      box.appendChild(bar);
      self.renderObject(box, item, v);
      fs.appendChild(box);
    });
    var max = s.maxItems || 50;
    if (arr.length < max) {
      var add = el("button", { type: "button", cls: "secondary small", text: "+ " + (LABELS[key] || "항목") + " 더하기" });
      add.addEventListener("click", function () { arr.push(blank(item, root)); self.redraw(); });
      fs.appendChild(add);
    }
    return fs;
  };

  // RFC3339 ↔ datetime-local. 사이트 시간대(분)로 읽고 쓴다.
  function pad(n) { return (n < 10 ? "0" : "") + n; }
  function toLocal(v, tz) {
    if (!v) return "";
    var t = Date.parse(v);
    if (isNaN(t)) return "";
    var d = new Date(t + tz * 60000);
    return d.getUTCFullYear() + "-" + pad(d.getUTCMonth() + 1) + "-" + pad(d.getUTCDate()) + "T" + pad(d.getUTCHours()) + ":" + pad(d.getUTCMinutes());
  }
  function fromLocal(v, tz) {
    if (!v) return "";
    var sign = tz < 0 ? "-" : "+", a = Math.abs(tz);
    return v + ":00" + sign + pad(Math.floor(a / 60)) + ":" + pad(a % 60);
  }

  function widgets() {
    var data = document.getElementById("widget-schemas");
    if (!data) return;
    try { data = JSON.parse(data.textContent); } catch (e) { return; }
    document.querySelectorAll("form.widget-form").forEach(function (form) {
      var f = new Form(form, data);
      var started = false;
      var go = function (fresh) { f.start(fresh); started = true; };
      // 접힌 고치기 칸은 열 때 그린다.
      var det = form.closest("details.wcard-edit");
      if (det && !det.open) det.addEventListener("toggle", function () { if (det.open && !started) go(false); });
      else go(false);
      form.querySelectorAll("input[name=kind]").forEach(function (r) {
        r.addEventListener("change", function () { f.raw.value = ""; go(true); });
      });
      f.raw.addEventListener("change", function () { go(false); });
      form.classList.add("has-js");
    });
  }

  // ---- 메뉴 ----
  function menus() {
    document.querySelectorAll("form.menu-form").forEach(function (form) {
      var body = form.querySelector("tbody");
      var renumber = function () {
        Array.prototype.forEach.call(body.rows, function (row, i) {
          var o = row.querySelector("[data-order]");
          if (o) o.value = String((i + 1) * 10);
          var up = row.querySelector("[data-move=up]"), down = row.querySelector("[data-move=down]");
          if (up) up.disabled = i === 0;
          if (down) down.disabled = i === body.rows.length - 1;
        });
      };
      body.addEventListener("click", function (e) {
        var b = e.target.closest("[data-move]");
        if (!b) return;
        var row = b.closest("tr");
        if (b.dataset.move === "up" && row.previousElementSibling) body.insertBefore(row, row.previousElementSibling);
        if (b.dataset.move === "down" && row.nextElementSibling) body.insertBefore(row.nextElementSibling, row);
        renumber();
        b.focus();
      });
      // 가리킬 곳 고르기 → 숨은 종류·주소 칸
      body.addEventListener("change", function (e) {
        var sel = e.target.closest("[data-dest]");
        if (!sel) return;
        var row = sel.closest("tr");
        var kind = row.querySelector("[data-kind]"), target = row.querySelector("[data-target]");
        var v = sel.value, i = v.indexOf(":");
        var k = i < 0 ? v : v.slice(0, i), t = i < 0 ? "" : v.slice(i + 1);
        kind.value = k;
        var url = row.querySelector("[data-url]");
        if (url) url.hidden = k !== "url";
        if (k !== "url") target.value = t;
        else if (url) { target.value = url.querySelector("input").value; url.querySelector("input").focus(); }
      });
      body.addEventListener("input", function (e) {
        var u = e.target.closest("[data-url] input");
        if (u) u.closest("tr").querySelector("[data-target]").value = u.value;
      });
      form.classList.add("has-js");
      renumber();
    });
  }

  // 화면 편집기(page-editor.js, ADR-0138)가 오른쪽 판에서 같은 칸을 쓴다.
  window.MaruForm = Form;
  window.MaruBlank = blank;

  function init() { widgets(); menus(); }
  if (document.readyState === "loading") document.addEventListener("DOMContentLoaded", init);
  else init();
})();
