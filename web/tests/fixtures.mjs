// Fixtures for testing the page without the emulation.
//
// The page talks to a Web Worker that runs WebAssembly. Here `Worker` is
// replaced by a stand-in that records what the page sends and lets the test
// say what the worker answers, so no test boots anything, and every message the
// real worker.mjs would send is one a test can send at will. worker.spec.mjs
// runs the real worker.mjs against a stub of the WebAssembly module instead.

import { mkdirSync, writeFileSync } from "node:fs";
import { test as base, expect } from "@playwright/test";

export { expect };

export const EEPROM_URL =
  "https://raw.githubusercontent.com/raspberrypi/rpi-eeprom/master/firmware-2711/latest/pieeprom-2026-08-04.bin";

// What a pieeprom.bin's bootconf.txt says, as the page asks the worker for it.
export const BOOTCONF = [
  ["all", "BOOT_UART", "0"],
  ["all", "WAKE_ON_GPIO", "1"],
  ["all", "POWER_OFF_ON_HALT", "0"],
  ["all", "NET_INSTALL_AT_POWER_ON", "1"],
  ["all", "SOMETHING_NEW", "7"],
  ["gpio4=1", "BOOT_UART", "1"],
];

const FAKE_WORKER = String.raw`
(() => {
  window.__workers = [];
  window.__bootconf = null;
  const plain = (value) => {
    if (value instanceof File) return { __file: value.name, size: value.size };
    if (value instanceof Uint8Array) return { __bytes: value.length, values: value.length <= 64 ? Array.from(value) : undefined };
    if (Array.isArray(value)) return value.map(plain);
    if (value && typeof value === "object") return Object.fromEntries(Object.entries(value).map(([k, v]) => [k, plain(v)]));
    return value;
  };
  class FakeWorker {
    constructor(url, options) {
      this.url = String(url);
      this.options = options;
      this.sent = [];
      this.terminated = false;
      this.onmessage = null;
      this.onerror = null;
      window.__workers.push(this);
    }
    postMessage(data) {
      this.sent.push(data);
      if (data.type === "bootconf") {
        queueMicrotask(() => this.emit({ type: "bootconf", id: data.id, entries: window.__bootconf }));
      }
    }
    terminate() { this.terminated = true; }
    emit(data) { this.onmessage?.({ data }); }
  }
  window.Worker = FakeWorker;
  window.__machine = () => [...window.__workers].reverse().find((w) => w.sent.some((m) => m.type === "power-on"));
  window.__sent = () => (window.__machine()?.sent ?? []).map(plain);
  window.__plain = plain;
})();
`;

class App {
  constructor(page) {
    this.page = page;
    this.drawer = page.locator("#drawer");
    this.term = page.locator("#term");
    this.status = page.locator("#phase");
    this.powerButton = page.locator("#power");
  }

  part(name) {
    return this.page.locator(`#board .part[data-part="${name}"]`).first();
  }

  async open(name) {
    await this.part(name).click();
    await expect(this.part(name)).toHaveAttribute("data-open", "true");
  }

  // The form field whose label says `label`.
  field(label) {
    return this.drawer.locator(".field", { has: this.page.locator("label, .label", { hasText: label }) });
  }

  // A bootloader setting's row, with its group opened.
  async setting(key, group) {
    const details = this.drawer.locator("details", { has: this.page.locator("summary", { hasText: group }) });
    if (!(await details.evaluate((d) => d.open))) await details.locator("summary").click();
    return this.drawer.locator(".setting", { has: this.page.locator(`code:text-is("${key}")`) });
  }

  tab(name) {
    return this.page.locator(`.tabs [data-tab="${name}"]`);
  }

  async showTab(name) {
    await this.tab(name).click();
    await expect(this.page.locator(`#tab-${name}`)).toBeVisible();
  }

  // Plug the power in and wait for the page to start its worker.
  async powerOn() {
    await this.powerButton.click();
    await this.page.waitForFunction(() => window.__machine() !== undefined);
  }

  // What the page asked the machine's worker to do, with files and bytes summed up.
  async sent() {
    return this.page.evaluate(() => window.__sent());
  }

  async request() {
    return (await this.sent()).find((m) => m.type === "power-on");
  }

  async emit(message) {
    await this.page.evaluate((m) => window.__machine().emit(m), message);
  }

  async emitOutput(text) {
    await this.page.evaluate((t) => window.__machine().emit({ type: "output", bytes: new TextEncoder().encode(t) }), text);
  }

  async running() {
    await this.powerOn();
    await this.emit({ type: "phase", phase: "running" });
    await expect(this.status).toHaveText("Running");
  }

  async workers() {
    return this.page.evaluate(() => window.__workers.map((w) => ({ url: w.url, terminated: w.terminated, sent: window.__plain(w.sent) })));
  }

  // The settings the page has saved; it saves a moment after a change.
  async localState() {
    await this.page.waitForTimeout(300);
    return this.page.evaluate(() => JSON.parse(localStorage.getItem("pimu-web") ?? "{}"));
  }

  async setBootconf(entries) {
    await this.page.evaluate((e) => (window.__bootconf = e), entries);
  }

  async pickFiles(locator, files) {
    await locator.setInputFiles(files);
  }
}

export const test = base.extend({
  // The saved settings the page starts from; leave undefined for the defaults.
  state: [undefined, { option: true }],
  // A page that can have threads: cross-origin isolated, with shared memory.
  isolated: [false, { option: true }],

  app: async ({ page, state, isolated }, use, testInfo) => {
    await page.addInitScript(FAKE_WORKER);
    if (isolated) {
      await page.addInitScript(() => {
        Object.defineProperty(globalThis, "crossOriginIsolated", { value: true });
        globalThis.SharedArrayBuffer ??= class {};
      });
    }
    if (state) await page.addInitScript((s) => localStorage.setItem("pimu-web", JSON.stringify(s)), state);
    await page.addInitScript((bootconf) => (window.__bootconf = bootconf), BOOTCONF);
    await page.route(EEPROM_URL, (route) =>
      route.fulfill({ status: 200, contentType: "application/octet-stream", headers: { "content-length": "4096" }, body: Buffer.alloc(4096, 0xff) }));
    await page.coverage.startJSCoverage({ resetOnNavigation: false }).catch(() => {});
    await page.goto("/");
    await use(new App(page));
    const entries = await page.coverage.stopJSCoverage().catch(() => []);
    const functions = [];
    for (const entry of entries) {
      const name = new URL(entry.url).pathname;
      if (!["/app.mjs", "/bootconf.mjs", "/download.mjs"].includes(name)) continue;
      const lines = (offset) => entry.source.slice(0, offset).split("\n").length;
      for (const f of entry.functions) {
        functions.push({ file: name, name: f.functionName || "(anonymous)", line: lines(f.ranges[0].startOffset), count: f.ranges[0].count });
      }
    }
    mkdirSync("test-results/coverage", { recursive: true });
    writeFileSync(`test-results/coverage/${testInfo.workerIndex}-${Date.now()}-${Math.random().toString(36).slice(2)}.json`, JSON.stringify(functions));
  },
});
