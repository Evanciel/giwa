// 미디어 보관함 (ADR-0172). 올리기는 그림 고르기 창(media-picker.js)을 그대로 쓰고, 창을 닫으면 목록을 다시 읽는다.
(function () {
  "use strict";
  document.querySelectorAll("[data-media-open]").forEach(function (b) {
    b.addEventListener("click", function () {
      if (!window.GiwaMedia) return;
      var kind = b.getAttribute("data-media-open");
      window.GiwaMedia.open({
        url: "",
        alt: null,
        required: true,
        kind: kind === "video" ? "video" : "image",
        csrf: b.getAttribute("data-csrf") || "",
        onPick: function () {}
      });
      var d = document.querySelector("dialog.mp");
      if (d) d.addEventListener("close", function () { location.reload(); });
    });
  });
  document.querySelectorAll("[data-copy]").forEach(function (b) {
    b.addEventListener("click", function () {
      var url = new URL(b.getAttribute("data-copy"), location.href).href;
      var done = function () { b.textContent = "복사했습니다"; setTimeout(function () { b.textContent = "주소 복사"; }, 1500); };
      if (navigator.clipboard) navigator.clipboard.writeText(url).then(done, function () {});
    });
  });
  document.querySelectorAll("[data-select-all]").forEach(function (i) {
    i.addEventListener("focus", function () { i.select(); });
  });
})();
