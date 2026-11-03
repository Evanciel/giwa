// 관리 화면 「서체」 올리기 (ADR-0048). 파일 바이트를 그대로 보낸다(`/admin/media` 와 같은 방식).
// 고르면 바로 그 서체로 미리보기를 그린다. 목록의 단추들은 스크립트 없이 폼으로 동작한다.
(() => {
  const form = document.querySelector("[data-font-upload]");
  if (!form) return;
  const csrf = form.dataset.csrf;
  const $ = (id) => document.getElementById(id);
  const file = $("font-file");
  const name = $("font-name");
  const status = form.querySelector("[data-font-status]");
  const preview = form.querySelector("[data-font-preview]");
  const button = form.querySelector('button[type="submit"]');
  let face = null;

  file.addEventListener("change", async () => {
    preview.hidden = true;
    status.textContent = "";
    const f = file.files && file.files[0];
    if (!f) return;
    if (!name.value) name.value = f.name.replace(/\.(woff2?|ttf|otf)$/i, "").slice(0, 40);
    try {
      if (face) document.fonts.delete(face);
      face = new FontFace("maru-upload-preview", await f.arrayBuffer());
      await face.load();
      document.fonts.add(face);
      preview.style.fontFamily = '"maru-upload-preview", sans-serif';
      preview.hidden = false;
    } catch {
      status.textContent = "브라우저가 읽지 못한 파일입니다. 서체 파일이 맞는지 확인하세요.";
    }
  });

  const send = async (url, init) => {
    const res = await fetch(url, { ...init, headers: { ...init.headers, "X-CSRF-Token": csrf } });
    const body = await res.json().catch(() => null);
    if (!res.ok || !body || !body.ok) {
      throw new Error((body && body.error && body.error.message) || `처리하지 못했습니다 (${res.status})`);
    }
    return body.data;
  };

  form.addEventListener("submit", async (e) => {
    e.preventDefault();
    const f = file.files && file.files[0];
    if (!f) return;
    const q = new URLSearchParams({
      name: name.value,
      license: $("font-license").value,
      fallback: $("font-fallback").value,
      weight: $("font-weight").value,
      italic: $("font-italic").checked ? "1" : "0",
      rights: $("font-rights").checked ? "1" : "0",
    });
    button.disabled = true;
    status.textContent = "올리는 중…";
    try {
      const up = await send(`/admin/fonts/upload?${q}`, {
        method: "POST",
        headers: { "Content-Type": "application/octet-stream" },
        body: f,
      });
      const text = $("font-license-text").value.trim();
      if (text) {
        await send(`/admin/fonts/${encodeURIComponent(up.font.slug)}/license`, {
          method: "POST",
          headers: { "Content-Type": "application/json" },
          body: JSON.stringify({ license_text: text }),
        });
      }
      if (up.warnings && up.warnings.length) {
        // 경고(큰 파일 등)를 읽을 시간을 두고 목록으로 간다.
        status.textContent = `올렸습니다. ${up.warnings.join(" ")}`;
        setTimeout(() => location.assign("/admin/fonts?done=uploaded"), 4000);
      } else {
        location.assign("/admin/fonts?done=uploaded");
      }
    } catch (err) {
      status.textContent = err.message;
      button.disabled = false;
    }
  });
})();
