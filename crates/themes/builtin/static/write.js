// 글쓰기 화면의 파일 붙이기 (ADR-0065). 파일을 올리고 받은 마크다운을 내용 칸에 넣는다.
// 스크립트가 없으면 칸이 숨은 채로 남고 글쓰기는 그대로 된다.
(function () {
  var box = document.querySelector("[data-attach-url]");
  var body = document.getElementById("body");
  var csrf = document.querySelector('.write-form input[name="_csrf"]');
  if (!box || !body || !csrf) return;
  var input = box.querySelector('input[type="file"]');
  var status = box.querySelector("[data-attach-status]");
  box.hidden = false;

  function insert(text) {
    var start = body.selectionStart;
    var before = body.value.slice(0, start);
    var after = body.value.slice(body.selectionEnd);
    var lead = before && !before.endsWith("\n\n") ? (before.endsWith("\n") ? "\n" : "\n\n") : "";
    var piece = lead + text + "\n\n";
    body.value = before + piece + after;
    var at = before.length + piece.length;
    body.setSelectionRange(at, at);
  }

  input.addEventListener("change", async function () {
    var files = Array.from(input.files);
    input.disabled = true;
    for (var i = 0; i < files.length; i++) {
      var f = files[i];
      status.textContent = f.name + " 올리는 중…";
      try {
        var res = await fetch(box.dataset.attachUrl + "?filename=" + encodeURIComponent(f.name), {
          method: "POST",
          headers: { "X-CSRF-Token": csrf.value },
          body: f,
          credentials: "same-origin",
        });
        var out = await res.json();
        if (!res.ok || !out.ok) {
          status.textContent = f.name + ": " + ((out.error && out.error.message) || "올리지 못했습니다.");
          break;
        }
        insert(out.data.markdown);
        status.textContent = "내용에 넣었습니다: " + f.name;
      } catch (e) {
        status.textContent = f.name + ": 올리지 못했습니다. 연결을 확인하세요.";
        break;
      }
    }
    input.value = "";
    input.disabled = false;
    body.focus();
  });
})();
