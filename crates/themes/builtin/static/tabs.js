// 글 탭 위젯 (GAP Q3, ADR-0088). WAI-ARIA 탭 패턴: 누르거나 ←·→·Home·End 로 고른다.
// 한 화면에 위젯이 여럿이면 이 파일이 여러 번 실릴 수 있어 이미 켠 상자는 건너뛴다.
(() => {
  for (const box of document.querySelectorAll("[data-tabs]:not([data-tabs-ready])")) {
    box.setAttribute("data-tabs-ready", "");
    const list = box.querySelector("[role=tablist]");
    const tabs = [...box.querySelectorAll("[role=tab]")];
    const panels = tabs.map((t) => document.getElementById(t.getAttribute("aria-controls")));
    if (!list || !tabs.length || panels.some((p) => !p)) continue;
    const select = (i, focus) => {
      tabs.forEach((t, j) => {
        const on = i === j;
        t.setAttribute("aria-selected", on ? "true" : "false");
        t.tabIndex = on ? 0 : -1;
        panels[j].hidden = !on;
      });
      if (focus) tabs[i].focus();
    };
    list.hidden = false;
    box.classList.add("tabs-on");
    select(0, false);
    tabs.forEach((t, i) => {
      t.addEventListener("click", () => select(i, false));
      t.addEventListener("keydown", (ev) => {
        const last = tabs.length - 1;
        const next = { ArrowRight: i === last ? 0 : i + 1, ArrowLeft: i === 0 ? last : i - 1, Home: 0, End: last }[ev.key];
        if (next === undefined) return;
        ev.preventDefault();
        select(next, true);
      });
    });
  }
})();
