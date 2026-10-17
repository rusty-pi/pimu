// The real worker.mjs, run against a stub of the WebAssembly module (stub-pimu.mjs).
// What it fetches, reads and hands to the builder is checked here; that the
// machine boots is not, and nothing boots.

import { readFileSync } from "node:fs";

import { expect, test as base } from "@playwright/test";

import { data } from "./data.mjs";

const stub = readFileSync(new URL("./stub-pimu.mjs", import.meta.url), "utf8");

const PAGE_SIDE = String.raw`
  window.__log = [];
  window.startWorker = () => {
    const worker = new Worker("/worker.mjs", { type: "module" });
    worker.onmessage = (event) => window.__log.push(event.data);
    worker.onerror = (event) => window.__log.push({ type: "worker-error", message: event.message });
    window.__worker = worker;
  };
  // { __file: { name, text | bytes } } in a request becomes a File.
  const hydrate = (value) => {
    if (ArrayBuffer.isView(value)) return value;
    if (value && value.__file) return new File([value.__file.bytes ? new Uint8Array(value.__file.bytes) : value.__file.text], value.__file.name);
    if (Array.isArray(value)) return value.map(hydrate);
    if (value && typeof value === "object") return Object.fromEntries(Object.entries(value).map(([k, v]) => [k, hydrate(v)]));
    return value;
  };
  window.post = (message) => window.__worker.postMessage(hydrate(message));
  window.__origin = location.origin;
`;

const test = base.extend({
  talk: async ({ page }, use) => {
    await page.route("**/pkg/pimu.js", (route) => route.fulfill({ contentType: "text/javascript", body: stub }));
    await page.addInitScript(PAGE_SIDE);
    await page.goto("/style.css");
    await page.evaluate(() => window.startWorker());
    await use({
      post: (message) => page.evaluate((m) => window.post(m), message),
      log: () => page.evaluate(() => window.__log),
      until: (source) => page.waitForFunction(`window.__log.some((m) => (${source})(m))`),
      stubCalls: async () => (await page.evaluate(() => window.__log)).filter((m) => m.type === "stub").map((m) => m.call),
      messages: async (type) => (await page.evaluate(() => window.__log)).filter((m) => m.type === type),
      origin: () => page.evaluate(() => window.__origin),
    });
  },
});

const eeprom = () => Array(100).fill(0xff);
const request = (origin, overrides = {}) => ({
  type: "power-on",
  options: { stepping: "c0", ticks: 3 },
  eeprom: new Uint8Array(100),
  boot: { kind: "none" },
  images: {},
  ...overrides,
});

test("it parses an EEPROM image's settings when asked", async ({ talk }) => {
  await talk.post({ type: "bootconf", id: 7, bytes: new Uint8Array(10) });
  await talk.until("(m) => m.type === 'bootconf'");
  expect((await talk.messages("bootconf"))[0]).toEqual({ type: "bootconf", id: 7, entries: [["all", "BOOT_UART", "0"]] });
});

test("it downloads the boot files with progress, builds the machine and runs it to the end", async ({ talk }) => {
  const origin = await talk.origin();
  await talk.post(request(origin, {
    options: { stepping: "c0", ticks: 3, bootconf: ["BOOT_UART=1"] },
    boot: {
      kind: "download",
      files: [
        { path: "start4.elf", url: `${origin}/tests/data/boot/start4.elf` },
        { path: "overlays/disable-bt.dtbo", url: `${origin}/tests/data/boot/overlays/disable-bt.dtbo` },
      ],
    },
  }));
  await talk.until("(m) => m.type === 'phase' && m.phase === 'ended'");

  const phases = (await talk.messages("phase")).map((m) => m.phase);
  expect(phases).toEqual(["downloading", "starting", "running", "ended"]);

  const progress = await talk.messages("progress");
  const last = progress.at(-1).transfers;
  expect(last.map((t) => [t.name, t.loaded, t.total, t.done, !!t.local])).toEqual([
    ["start4.elf", 3, 3, true, false],
    ["overlays/disable-bt.dtbo", 4, 4, true, false],
  ]);
  expect(last[0].url).toBe(`${origin}/tests/data/boot/start4.elf`);

  const calls = await talk.stubCalls();
  expect(calls.slice(0, 2)).toEqual([["init"], ["builder", { stepping: "c0", ticks: 3, bootconf: ["BOOT_UART=1"] }, 100]]);
  const added = calls.filter((c) => c[0] === "add_file").map((c) => c.slice(1)).sort();
  expect(added).toEqual([["overlays/disable-bt.dtbo", 4], ["start4.elf", 3]]);

  const output = await talk.messages("output");
  expect(output.map((m) => new TextDecoder().decode(m.bytes))).toEqual(["tick 1\n", "tick 2\n", "tick 3\n"]);
  expect((await talk.messages("log")).map((m) => m.text)).toEqual(['{"us":1,"channel":"io","msg":"hello"}\n']);
  const states = await talk.messages("state");
  expect(states.at(-1).state).toMatchObject({ ended: true, ok: true, why: "done" });
  expect(states.at(-1).io).toEqual({});
});

test("it reads local files, and hands the extras to the builder", async ({ talk }) => {
  const origin = await talk.origin();
  await talk.post(request(origin, {
    boot: { kind: "files", files: [{ path: "config.txt", file: { __file: { name: "config.txt", text: "arm_64bit=1\n" } } }] },
    pubkey: { __file: { name: "pubkey.bin", text: "k".repeat(264) } },
    hat: { __file: { name: "hat.eep", text: "h".repeat(256) } },
    edid: { __file: { name: "edid.bin", text: "e".repeat(128) } },
    netFiles: [{ path: "boot/start4.elf", file: { __file: { name: "start4.elf", text: "abc" } } }],
  }));
  await talk.until("(m) => m.type === 'phase' && m.phase === 'running'");
  const calls = await talk.stubCalls();
  expect(calls).toContainEqual(["add_file", "config.txt", 12]);
  expect(calls).toContainEqual(["add_net_file", "boot/start4.elf", 3]);
  expect(calls).toContainEqual(["set_pubkey", 264]);
  expect(calls).toContainEqual(["set_hat", 256]);
  expect(calls).toContainEqual(["set_edid", 128]);
  const transfers = (await talk.messages("progress")).at(-1).transfers;
  expect(transfers.map((t) => [t.name, t.local])).toEqual([["config.txt", true], ["pubkey.bin", true], ["hat.eep", true], ["edid.bin", true], ["boot/start4.elf", true]]);
});

test("a disk image is read in ranges, from a file or from a URL", async ({ talk }) => {
  const origin = await talk.origin();
  const image = readFileSync(data("disk.img"));
  await talk.post(request(origin, {
    images: {
      sd: { url: `${origin}/tests/data/disk.img` },
      usb: { file: { __file: { name: "stick.img", bytes: [...image] } } },
    },
  }));
  await talk.until("(m) => m.type === 'phase' && m.phase === 'running'");
  const attach = (await talk.stubCalls()).filter((c) => c[0] === "attach_image");
  const bySlot = Object.fromEntries(attach.map((c) => [c[1], c]));
  // the image is a run of 32-bit counters, so a ranged read shows where it came from
  expect(bySlot.sd.slice(2)).toEqual([4096, [0, 0, 0, 0, 1, 0, 0, 0], [255, 3, 0, 0]]);
  expect(bySlot.usb[2]).toBe(4096);
  expect(bySlot.usb[3]).toEqual([0, 0, 0, 0, 1, 0, 0, 0]);
  const io = (await talk.messages("state")).at(-1).io;
  // the stub reads 8 bytes at the start and 4 at the end; sniffing for xz is not counted
  expect(io.sd).toEqual({ read: 12, requests: 2 });
  expect(io.usb).toEqual({ read: 12, requests: 2 });
});

test("a compressed image is refused with the reason", async ({ talk }) => {
  const origin = await talk.origin();
  await talk.post(request(origin, { images: { sd: { url: `${origin}/tests/data/image.img.xz` } } }));
  await talk.until("(m) => m.type === 'error'");
  expect((await talk.messages("error"))[0].message).toContain("is xz-compressed");
});

test("a server that does not do ranges is refused with the reason", async ({ talk }) => {
  const origin = await talk.origin();
  await talk.post(request(origin, { images: { sd: { url: `${origin}/tests/data/disk.img?norange=1` } } }));
  await talk.until("(m) => m.type === 'error'");
  expect((await talk.messages("error"))[0].message).toContain("cannot be read in pieces");
});

test("an image that is not there is reported", async ({ talk }) => {
  const origin = await talk.origin();
  await talk.post(request(origin, { images: { sd: { url: `${origin}/tests/data/missing.img` } } }));
  await talk.until("(m) => m.type === 'error'");
  expect((await talk.messages("error"))[0].message).toContain("answered 404");
});

test("a download that fails is reported with its name", async ({ talk }) => {
  const origin = await talk.origin();
  await talk.post(request(origin, { boot: { kind: "download", files: [{ path: "start4.elf", url: `${origin}/tests/data/boot/missing.elf` }] } }));
  await talk.until("(m) => m.type === 'error'");
  expect((await talk.messages("error"))[0].message).toContain("start4.elf");
  expect((await talk.messages("error"))[0].message).toContain("answered 404");
});

test("a GitHub directory is listed, and its files are the card's", async ({ talk, page }) => {
  const origin = await talk.origin();
  await page.context().route("https://api.github.com/repos/someone/firmware/contents/boot?ref=stable", (route) =>
    route.fulfill({
      status: 200,
      contentType: "application/json",
      headers: { "access-control-allow-origin": "*" },
      body: JSON.stringify([
        { type: "file", name: "start4.elf", download_url: `${origin}/tests/data/boot/start4.elf` },
        { type: "dir", name: "overlays", download_url: null },
        { type: "file", name: "fixup4.dat", download_url: `${origin}/tests/data/boot/fixup4.dat` },
      ]),
    }));
  await talk.post(request(origin, { boot: { kind: "download", github: { repo: "someone/firmware", ref: "stable", path: "boot" } } }));
  await talk.until("(m) => m.type === 'phase' && m.phase === 'running'");
  const added = (await talk.stubCalls()).filter((c) => c[0] === "add_file").map((c) => c.slice(1)).sort();
  expect(added).toEqual([["fixup4.dat", 3], ["start4.elf", 3]]);
});

test("a GitHub directory that cannot be listed is reported", async ({ talk, page }) => {
  const origin = await talk.origin();
  await page.context().route("https://api.github.com/**", (route) =>
    route.fulfill({ status: 403, headers: { "access-control-allow-origin": "*" }, body: "{}" }));
  await talk.post(request(origin, { boot: { kind: "download", github: { repo: "a/b", ref: "main", path: "boot" } } }));
  await talk.until("(m) => m.type === 'error'");
  expect((await talk.messages("error"))[0].message).toContain("GitHub answered 403");
});

test.describe("a machine that keeps running", () => {
  test.beforeEach(async ({ talk }) => {
    const origin = await talk.origin();
    await talk.post(request(origin, { options: { stepping: "c0", ticks: 100000 } }));
    await talk.until("(m) => m.type === 'phase' && m.phase === 'running'");
  });

  test("keys and the magic packet reach it, and it can be paused and resumed", async ({ talk, page }) => {
    await talk.post({ type: "keys", keys: new Uint8Array([108, 115, 13]) });
    await talk.until("(m) => m.type === 'stub' && m.call[0] === 'send'");
    expect((await talk.stubCalls()).find((c) => c[0] === "send")).toEqual(["send", [108, 115, 13]]);

    await talk.post({ type: "wake-on-lan" });
    await talk.until("(m) => m.type === 'wake-on-lan'");
    expect((await talk.messages("wake-on-lan"))[0]).toEqual({ type: "wake-on-lan", sent: true });

    await talk.post({ type: "pause" });
    await talk.until("(m) => m.type === 'phase' && m.phase === 'paused'");
    await page.waitForTimeout(100);
    const ticks = async () => (await talk.messages("output")).length;
    const paused = await ticks();
    await page.waitForTimeout(300);
    expect(await ticks()).toBe(paused);
    await talk.post({ type: "resume" });
    await expect.poll(ticks).toBeGreaterThan(paused);
  });

  test("it answers for the inspect tab, and hands over the flash and the device tree", async ({ talk }) => {
    await talk.post({ type: "info" });
    await talk.until("(m) => m.type === 'info'");
    const [info] = await talk.messages("info");
    expect(info).toMatchObject({ deviceTree: "/dts-v1/;", bootconf: ["[all] A=1"], otp: [] });
    expect(info.state.cores.arm).toHaveLength(4);

    await talk.post({ type: "save", what: "eeprom" });
    await talk.post({ type: "save", what: "dtb" });
    await talk.until("(m) => m.type === 'file' && m.what === 'dtb'");
    const files = await talk.messages("file");
    expect(files.map((f) => [f.what, [...f.bytes]])).toEqual([["eeprom", [9, 9]], ["dtb", [1, 2, 3]]]);
  });
});
