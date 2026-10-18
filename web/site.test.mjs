import assert from "node:assert/strict";
import { execFileSync } from "node:child_process";
import { existsSync, mkdtempSync, readFileSync, readdirSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import test from "node:test";

const script = new URL("../scripts/assemble-site.sh", import.meta.url).pathname;
const BUILT_BY_WASM_BINDGEN = /^pkg(-mt)?\//;

const referencesOf = (file, text) => {
  const patterns = file.endsWith(".html")
    ? [/(?:src|href)="([^"#:]+)"/g]
    : [
        /\bfrom\s+"(\.\/[^"]+)"/g,
        /\bimport\("(\.\/[^"]+)"\)/g,
        /\bimport\s+"(\.\/[^"]+)"/g,
        /new Worker\("([^"]+)"/g,
        /new URL\("(\.\/[^"]+)", import\.meta\.url\)/g,
      ];
  return patterns.flatMap((pattern) => [...text.matchAll(pattern)].map((m) => m[1].replace(/^\.\//, "")));
};

test("every file the published page loads is published", () => {
  const site = mkdtempSync(join(tmpdir(), "pimu-site-"));
  try {
    execFileSync(script, [site]);
    const published = readdirSync(site).filter((name) => /\.(html|mjs)$/.test(name));
    assert.ok(published.includes("index.html") && published.includes("app.mjs"));
    for (const file of published) {
      for (const reference of referencesOf(file, readFileSync(join(site, file), "utf8"))) {
        if (BUILT_BY_WASM_BINDGEN.test(reference)) continue;
        assert.ok(existsSync(join(site, reference)), `${file} loads ${reference}, which the site does not have`);
      }
    }
    assert.ok(!published.some((name) => name.endsWith(".test.mjs") || name === "playwright.config.mjs"));
  } finally {
    rmSync(site, { recursive: true, force: true });
  }
});
