// 태그 칸의 값 다듬기. 화면 없이 테스트한다 (test/roundtrip.ts).
// 최종 정리(같은 주소 합치기·처음 이름으로 맞추기)는 서버(core `tags::normalize`)가 한다.
// 여기서는 칩이 겹쳐 보이지 않을 만큼만 맞춘다.

export const TAG_MAX = 20;
export const TAG_CHARS = 30;

/** 앞 `#`·앞뒤 공백을 떼고 안쪽 공백을 하나로. */
export function cleanTag(raw: string): string {
  return raw.trim().replace(/^#+/, "").trim().replace(/\s+/g, " ");
}

/** 칩이 같은 태그인지 볼 때 쓰는 열쇠. 대소문자만 무시한다. */
export function tagKey(name: string): string {
  return name.toLowerCase();
}

/** 쉼표로 이은 글을 태그 목록으로. 빈 것과 겹치는 것은 뺀다(처음 것이 남는다). */
export function splitTags(text: string): string[] {
  const out: string[] = [];
  for (const part of text.split(",")) {
    const t = cleanTag(part);
    if (t && !out.some((x) => tagKey(x) === tagKey(t))) out.push(t);
  }
  return out;
}
