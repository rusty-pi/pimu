// Which functions of the page's own scripts did the tests run? Reads what the
// fixtures wrote (V8's coverage of every test's page), lists the functions
// that no test ran, and fails when there are any outside the allowlist.
//
// Function coverage is a coarse check, but it is the one that answers "does
// every feature of the page have a test that exercises it".

import { existsSync, readdirSync, readFileSync } from "node:fs";
import { join } from "node:path";

const dir = "test-results/coverage";
if (!existsSync(dir)) {
  console.error(`no coverage in ${dir}: run the tests first`);
  process.exit(2);
}

// Functions that cannot be run in a test, and why.
const ALLOW = new Set([
  // Runs in the worker, which this page-side coverage does not see; worker.spec.mjs tests it.
  "/download.mjs:githubListing",
]);

const seen = new Map();
for (const name of readdirSync(dir)) {
  for (const f of JSON.parse(readFileSync(join(dir, name), "utf8"))) {
    const key = `${f.file}:${f.line}:${f.name}`;
    seen.set(key, { ...f, count: (seen.get(key)?.count ?? 0) + f.count });
  }
}

const byFile = new Map();
for (const f of seen.values()) {
  const list = byFile.get(f.file) ?? [];
  list.push(f);
  byFile.set(f.file, list);
}

let missed = 0;
for (const [file, list] of [...byFile].sort()) {
  const ran = list.filter((f) => f.count > 0).length;
  console.log(`${file}  ${ran} of ${list.length} functions ran (${Math.round((100 * ran) / list.length)}%)`);
  for (const f of list.filter((f) => f.count === 0).sort((a, b) => a.line - b.line)) {
    const key = `${f.file}:${f.name}`;
    if (ALLOW.has(key)) continue;
    console.log(`    not run: ${f.name} (${f.file}:${f.line})`);
    missed += 1;
  }
}
if (missed) {
  console.error(`${missed} functions no test runs`);
  process.exit(1);
}
