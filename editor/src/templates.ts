// 글 틀. 서버(`services::templates`)의 기본 틀 + 저장한 틀을 불러오고, 지금 글을 틀로 저장한다.

export interface Template {
  id: string;
  name: string;
  body_md: string;
  builtin: boolean;
}

async function call<T>(method: "GET" | "POST", csrf: string, body?: unknown): Promise<T> {
  let res: Response;
  try {
    res = await fetch("/admin/templates", {
      method,
      credentials: "same-origin",
      headers: { "X-CSRF-Token": csrf, ...(body ? { "Content-Type": "application/json" } : {}) },
      body: body ? JSON.stringify(body) : undefined,
    });
  } catch {
    throw new Error("서버에 연결하지 못했습니다.");
  }
  let json: { ok?: boolean; data?: T; error?: { message?: string } } = {};
  try {
    json = await res.json();
  } catch {
    // 본문이 JSON 이 아닌 응답
  }
  if (!res.ok || !json.ok || json.data === undefined) {
    if (res.status === 401) throw new Error("로그인이 끝났습니다. 새 탭에서 다시 로그인한 뒤 시도하세요.");
    throw new Error(json.error?.message || `글 틀을 처리하지 못했습니다 (${res.status}).`);
  }
  return json.data;
}

export function fetchTemplates(csrf: string): Promise<Template[]> {
  return call<Template[]>("GET", csrf);
}

export function saveTemplate(name: string, bodyMd: string, csrf: string): Promise<Template> {
  return call<Template>("POST", csrf, { name, body_md: bodyMd });
}
