// 이모지·특수문자 고르기 창. 그림 파일 없이 유니코드 글자만 쓴다(스티커 이미지는 따로).

export const CHAR_GROUPS: [string, string][] = [
  ["표정", "😀😃😄😁😆😅🤣😂🙂😉😊😇🥰😍🤩😘😋😛😜🤪🤗🤭🤔🤨😐😑😶🙄😏😴😪😮😯😲🥱😳🥺😢😭😤😠😡🤯😱😨😰😥🤒🤕🤧😎🤓🧐😕😟🙁☹️😬🫠🫡🫢"],
  ["손·사람", "👍👎👏🙌👐🤲🙏🤝✌️🤞🤟🤘👌🤌👈👉👆👇☝️✋🤚🖐️🖖👋🤙💪✍️🙋🙆🙅🤷🤦💁🙇👶👧🧒👦👩🧑👨👵🧓👴"],
  ["마음·기호", "❤️🧡💛💚💙💜🤎🖤🤍💔❣️💕💞💓💗💖💘💝✨⭐🌟💫🔥💯✅❌⭕❗❓‼️⁉️💡📌📍🔔🎉🎊🎁🏆🥇🥈🥉"],
  ["자연·음식", "☀️🌤️⛅🌧️⛈️🌈❄️☃️🌸🌷🌹🌻🌼🍀🍁🍂🌲🌳🌴🌵🐶🐱🐰🐻🐼🐨🐯🦁🐮🐷🐸🐵🐔🐧🐦🦋🐟🍎🍊🍋🍉🍇🍓🍑🍒🥝🍅🥑🍞🍳🍔🍕🍜🍣🍱🍙🍚🍰🎂🍪🍫☕🍵🍺🍷"],
  ["사물·여행", "📷📸🎥📺💻⌨️🖥️📱☎️📞📚📖📝✏️🖊️📎✂️📅📆🗓️⏰⌛💰💳🎵🎶🎧🎮🎨🏠🏢🏫🏥⛪🚗🚌🚇🚄✈️🚢🚲⛰️🏖️🏕️🗺️🧳"],
  ["특수문자", "※★☆○●◎◇◆□■△▲▽▼◁◀▷▶♤♠♡♥♧♣⊙◈▣◐◑▒▤▥▨▧▦▩☏☎☜☞¶†‡↕↗↙↖↘♭♩♪♬㉿㈜№㏇™㏂㏘℡®ª"],
  ["화살표·괄호", "→←↑↓↔⇒⇔⇐⇑⇓「」『』《》〈〉【】〔〕（）［］｛｝‘’“”·…‥¨〃―∥＼∼"],
  ["숫자·단위", "①②③④⑤⑥⑦⑧⑨⑩⑪⑫⑬⑭⑮ⅠⅡⅢⅣⅤⅥⅦⅧⅨⅩ½⅓⅔¼¾±×÷≠≤≥∞∴∵°′″℃℉Å￠￡￥€₩㎜㎝㎞㎎㎏㎖㎗㎘㏄㎡㎥%‰"],
];

/** 글자 단위로 나눈다. 이모지는 여러 코드포인트(변형 선택자·ZWJ)로 된 것이 있어 Intl.Segmenter 로 자른다. */
export function splitChars(s: string): string[] {
  const Seg = (Intl as unknown as { Segmenter?: new (l: string, o: object) => { segment(s: string): Iterable<{ segment: string }> } }).Segmenter;
  if (Seg) return Array.from(new Seg("ko", { granularity: "grapheme" }).segment(s), (x) => x.segment);
  return Array.from(s);
}

const RECENT_KEY = "maru:editor-recent-chars";

export function recentChars(): string[] {
  try {
    const v = JSON.parse(localStorage.getItem(RECENT_KEY) || "[]");
    return Array.isArray(v) ? v.filter((x) => typeof x === "string").slice(0, 24) : [];
  } catch {
    return [];
  }
}

export function rememberChar(c: string) {
  try {
    const next = [c, ...recentChars().filter((x) => x !== c)].slice(0, 24);
    localStorage.setItem(RECENT_KEY, JSON.stringify(next));
  } catch {
    // 저장소를 못 쓰면 최근 목록 없이
  }
}
