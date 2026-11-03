// Maru 내장 테마 스크립트 (ADR-0060). 지금은 팝업만 다룬다.
// 「N일 보지 않기」 기한은 이 브라우저의 localStorage 에만 둔다. 저장소를 못 쓰면 닫기만 된다.
(() => {
  const PREFIX = "maru-popup:";
  const store = {
    get(k) { try { return localStorage.getItem(PREFIX + k); } catch { return null; } },
    set(k, v) { try { localStorage.setItem(PREFIX + k, v); } catch { /* 저장 못 함: 닫기만 */ } },
  };
  const popups = [...document.querySelectorAll("[data-popup]")];
  const before = document.activeElement;
  const open = popups.filter((el) => {
    const until = Number(store.get(el.dataset.popup) || 0);
    return !(until && Date.now() < until);
  });
  if (!open.length) return;
  const close = (el) => {
    el.hidden = true;
    const rest = open.filter((p) => !p.hidden);
    if (rest.length) {
      rest[rest.length - 1].querySelector("button")?.focus();
    } else if (before && before.focus) {
      before.focus();
    }
  };
  for (const el of open) {
    el.hidden = false;
    el.querySelector("[data-popup-close]")?.addEventListener("click", () => close(el));
    el.querySelector("[data-popup-dismiss]")?.addEventListener("click", () => {
      const days = Number(el.dataset.dismissDays || 0);
      store.set(el.dataset.popup, String(Date.now() + days * 86400000));
      close(el);
    });
  }
  open[open.length - 1].querySelector("button")?.focus();
  document.addEventListener("keydown", (e) => {
    if (e.key !== "Escape") return;
    const top = open.filter((p) => !p.hidden).pop();
    if (top) close(top);
  });
})();
