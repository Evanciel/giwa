// 도구 막대 아이콘. 외부 아이콘 묶음을 받지 않고 단순한 선 그림으로 직접 그렸다 (24×24, 선 굵기 1.75).
// 색은 글자색(currentColor)을 따른다. 글자 버튼과 같이 쓰이므로 장식이며 이름은 버튼의 aria-label 이 갖는다.

const PATHS: Record<string, string> = {
  photo: '<rect x="3" y="5" width="18" height="14" rx="2"/><circle cx="8.5" cy="10" r="1.5"/><path d="M21 16l-5-5-10 8"/>',
  file: '<path d="M20.5 11.5l-8.4 8.4a5 5 0 0 1-7.1-7.1l8.4-8.4a3.4 3.4 0 0 1 4.8 4.8l-8.4 8.4a1.8 1.8 0 0 1-2.5-2.5l7.7-7.7"/>',
  video: '<rect x="3" y="6" width="13" height="12" rx="2"/><path d="M16 10.5l5-3v9l-5-3z"/>',
  card: '<rect x="3" y="5" width="18" height="14" rx="2"/><rect x="5.5" y="8" width="5" height="8" rx="1"/><path d="M13 9h5.5M13 12.5h5.5M13 16h3"/>',
  place: '<path d="M12 21s-6.5-5.6-6.5-11a6.5 6.5 0 0 1 13 0c0 5.4-6.5 11-6.5 11z"/><circle cx="12" cy="10" r="2.2"/>',
  table: '<rect x="3" y="4" width="18" height="16" rx="2"/><path d="M3 9.5h18M3 14.5h18M9.5 4v16"/>',
  quote: '<path d="M10 7H6.5A1.5 1.5 0 0 0 5 8.5V12h5V7zM10 12c0 3-1.5 5-4.5 5.5M19 7h-3.5A1.5 1.5 0 0 0 14 8.5V12h5V7zM19 12c0 3-1.5 5-4.5 5.5"/>',
  rule: '<path d="M3 12h18"/><path d="M7 7.5h10M7 16.5h10" opacity=".35"/>',
  box: '<rect x="3" y="4.5" width="18" height="15" rx="3"/><path d="M12 9v.01M12 11.5V16"/>',
  template: '<rect x="3.5" y="3.5" width="17" height="17" rx="2"/><path d="M3.5 9h17M9.5 9v11.5"/>',
  emoji: '<circle cx="12" cy="12" r="8.5"/><path d="M8.5 14.2a4.4 4.4 0 0 0 7 0M9.2 9.6v.01M14.8 9.6v.01"/>',
  gallery: '<rect x="4" y="4" width="7" height="7" rx="1.5"/><rect x="13" y="4" width="7" height="7" rx="1.5"/><rect x="4" y="13" width="7" height="7" rx="1.5"/><rect x="13" y="13" width="7" height="7" rx="1.5"/>',
  code: '<path d="M8.5 7.5L4 12l4.5 4.5M15.5 7.5L20 12l-4.5 4.5"/>',
  link: '<path d="M10 14a4 4 0 0 0 5.7 0l3-3a4 4 0 0 0-5.7-5.7l-1.1 1.1M14 10a4 4 0 0 0-5.7 0l-3 3a4 4 0 0 0 5.7 5.7l1.1-1.1"/>',
  color: '<path d="M6.5 16L12 4l5.5 12M8.6 11.5h6.8"/><path d="M4 20.5h16" stroke-width="3"/>',
  highlight: '<path d="M14.5 4.5l5 5-8.5 8.5H6v-5zM4 21h9"/>',
  size: '<path d="M3 18l4.5-11L12 18M4.6 14.2h5.8M14 18l3-7.5 3 7.5M15 15.8h4"/>',
  align: '<path d="M4 6h16M7 10h10M4 14h16M7 18h10"/>',
  bullet: '<path d="M10 7h10M10 12h10M10 17h10"/><path d="M5 7v.01M5 12v.01M5 17v.01" stroke-width="2.6"/>',
  ordered: '<path d="M10 7h10M10 12h10M10 17h10M4.5 5.5h1V9M4 14.2a1 1 0 0 1 2 .3c0 .7-2 1.7-2 2.8h2"/>',
  task: '<rect x="3.5" y="4.5" width="6" height="6" rx="1.2"/><path d="M5 7.6l1.2 1.2L8.3 6.6M13 7.5h7.5"/><rect x="3.5" y="13.5" width="6" height="6" rx="1.2"/><path d="M13 16.5h7.5"/>',
  codeblock: '<rect x="3" y="4" width="18" height="16" rx="2"/><path d="M9.5 9.5L7 12l2.5 2.5M14.5 9.5L17 12l-2.5 2.5"/>',
  undo: '<path d="M9 14L4 9l5-5M4 9h10.5a5.5 5.5 0 0 1 0 11H11"/>',
  redo: '<path d="M15 14l5-5-5-5M20 9H9.5a5.5 5.5 0 0 0 0 11H13"/>',
  more: '<path d="M5.5 12v.01M12 12v.01M18.5 12v.01" stroke-width="2.8"/>',
  eye: '<path d="M2.5 12S6 5.5 12 5.5 21.5 12 21.5 12 18 18.5 12 18.5 2.5 12 2.5 12z"/><circle cx="12" cy="12" r="2.8"/>',
  spell: '<path d="M3.5 16l3.5-9 3.5 9M4.8 12.8h4.4M13.5 13.5l2.6 2.6 4.4-5.1"/>',
  caret: '<path d="M7 10l5 5 5-5"/>',
};

/** 이름에 맞는 아이콘 SVG 요소. 없는 이름이면 null. */
export function icon(name: string): SVGSVGElement | null {
  const body = PATHS[name];
  if (!body) return null;
  const tpl = document.createElement("template");
  // 위의 고정 문자열만 넣는다 (사용자 입력 없음).
  tpl.innerHTML = `<svg class="me-icon" viewBox="0 0 24 24" width="20" height="20" fill="none" stroke="currentColor" stroke-width="1.75" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true" focusable="false">${body}</svg>`;
  return tpl.content.firstElementChild as SVGSVGElement;
}
