// 번들(editor.js)에 들어간 패키지의 라이선스 전문을 editor.licenses.txt 로 모은다.
// MIT·BSD 는 복사본에 저작권 고지를 함께 두라고 한다. `npm run build` 가 번들을 만든 뒤 부른다.
// 포함 여부는 esbuild 메타파일(입력 파일 목록)로 정한다. 손으로 고친 목록은 빠지기 쉽다.

import { readFileSync, readdirSync, writeFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const root = join(dirname(fileURLToPath(import.meta.url)), "..");
const meta = JSON.parse(readFileSync(join(root, "node_modules", ".editor-meta.json"), "utf8"));
const out = join(root, "..", "crates", "themes", "admin", "static", "editor.licenses.txt");

const names = new Set();
for (const input of Object.keys(meta.inputs)) {
  const m = input.match(/node_modules\/((?:@[^/]+\/)?[^/]+)\//);
  if (m) names.add(m[1]);
}

const parts = [
  "Maru 글 에디터(editor.js)에 들어간 오픈소스 패키지와 라이선스",
  "에디터 자체 코드: MIT OR Apache-2.0 (Maru)",
  "",
];
for (const name of [...names].sort()) {
  const dir = join(root, "node_modules", name);
  const pkg = JSON.parse(readFileSync(join(dir, "package.json"), "utf8"));
  const file = readdirSync(dir).find((f) => /^(licen[cs]e|copying)/i.test(f));
  if (!file) throw new Error(`${name}: 라이선스 파일이 없습니다`);
  parts.push("=".repeat(72), `${pkg.name} ${pkg.version} (${pkg.license})`, "=".repeat(72));
  parts.push(readFileSync(join(dir, file), "utf8").trim(), "");
}
writeFileSync(out, parts.join("\n"));
console.log(`라이선스 ${names.size}개 → ${out}`);
