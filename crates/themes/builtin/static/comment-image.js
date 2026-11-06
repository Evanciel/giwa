// 댓글에 그림 한 장 붙이기 (GAP Q8, ADR-0116). 올려서 받은 주소를 숨은 칸에 둔다.
// 스크립트가 없으면 칸이 숨은 채로 남고 댓글은 그대로 된다.
(function () {
  document.querySelectorAll("[data-comment-image]").forEach(function (box) {
    var hidden = box.querySelector('input[name="image"]');
    var file = box.querySelector('input[type="file"]');
    var preview = box.querySelector("[data-image-preview]");
    var pic = preview.querySelector("img");
    var status = box.querySelector("[data-image-status]");

    function show(src) {
      hidden.value = src;
      preview.hidden = !src;
      if (src) pic.src = src;
      else pic.removeAttribute("src");
    }
    show(hidden.value);

    box.querySelector("[data-image-remove]").addEventListener("click", function () {
      show("");
      status.textContent = "그림을 뺐습니다.";
      if (file) file.focus();
    });

    if (file) {
      file.addEventListener("change", async function () {
        var f = file.files[0];
        if (!f) return;
        file.disabled = true;
        status.textContent = f.name + " 올리는 중…";
        try {
          var res = await fetch(file.dataset.upload + "?filename=" + encodeURIComponent(f.name), {
            method: "POST",
            body: f,
            credentials: "same-origin",
          });
          var out = await res.json();
          if (res.ok && out.ok) {
            show(out.data.url);
            status.textContent = "그림을 붙였습니다. 댓글을 보내면 함께 올라갑니다.";
          } else {
            status.textContent = (out.error && out.error.message) || "올리지 못했습니다.";
          }
        } catch (e) {
          status.textContent = "올리지 못했습니다. 연결을 확인하세요.";
        }
        file.value = "";
        file.disabled = false;
      });
    }

    // 글 화면은 손님 기준으로 같아서 이 사람이 붙일 수 있는지 물어본다.
    if (!box.dataset.ask) {
      box.hidden = false;
      return;
    }
    fetch(box.dataset.ask, { credentials: "same-origin", headers: { Accept: "application/json" } })
      .then(function (r) {
        return r.ok ? r.json() : null;
      })
      .then(function (out) {
        if (out && out.data && out.data.allowed) box.hidden = false;
      })
      .catch(function () {});
  });
})();
