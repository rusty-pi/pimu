import { readFileSync } from "node:fs";

import { expect, test } from "./fixtures.mjs";

const info = (overrides = {}) => ({
  type: "info",
  deviceTree: "/dts-v1/;\n\n/ {\n\tmodel = \"Raspberry Pi 4 Model B Rev 1.4\";\n};\n",
  bootconf: ["[all] BOOT_UART=0", "[all] BOOT_ORDER=0xf41", "BOOT_ORDER: SD CARD -> USB-MSD -> RESTART"],
  otp: [
    { row: 28, value: 0x1aa2bb31, meaning: "serial number" },
    { row: 30, value: 0x00a03114, meaning: "revision code: board model, RAM size, maker" },
  ],
  state: {
    ended: false, ok: null, why: null, reboots: 1, armReleased: true, modelUs: 16_653_000, retired: 845_545_316, logDropped: 0,
    cores: {
      vpu: [{ state: "running", insns: 845_545_316 }, { state: "off", insns: 0 }],
      arm: [{ state: "sleeping", insns: 133_272_348 }, { state: "running", insns: 619_767_744 }, { state: "off", insns: 0 }, { state: "sleeping", insns: 2_020_379 }],
    },
  },
  ...overrides,
});

test.describe("the inspect tab", () => {
  test("it says to power on while the board is off", async ({ app }) => {
    await app.showTab("info");
    await expect(app.page.locator("#tab-info")).toHaveText("Power the board on to look inside it.");
  });

  test("it asks the board and shows that it is waiting", async ({ app }) => {
    await app.running();
    await app.showTab("info");
    await expect(app.page.locator("#tab-info")).toHaveText("Reading the board…");
    await expect.poll(async () => (await app.sent()).filter((m) => m.type === "info").length).toBeGreaterThan(0);
  });

  test("it shows the run, the cores, the bootloader configuration, the fuses and the device tree", async ({ app }) => {
    await app.running();
    await app.showTab("info");
    await app.emit(info());
    const pane = app.page.locator("#tab-info");
    await expect(pane.locator("section", { hasText: "Run" }).first()).toContainText("16.653 s");
    await expect(pane).toContainText("ARM started by the firmware");
    await expect(pane.locator("tr", { hasText: "Resets so far" })).toContainText("1");
    await expect(pane.locator("tr", { hasText: "Outcome" })).toContainText("still running");
    await expect(pane.locator("tr", { hasText: "VPU 0" })).toContainText("845,545,316 instructions");
    await expect(pane.locator("tr", { hasText: "ARM 1" })).toContainText("running");
    await expect(pane.locator("tr", { hasText: "ARM 2" })).toContainText("off");
    await expect(pane.locator("pre").nth(0)).toContainText("[all] BOOT_ORDER=0xf41");
    await expect(pane.locator("pre").nth(0)).toContainText("BOOT_ORDER: SD CARD -> USB-MSD -> RESTART");
    await expect(pane.locator("tr", { hasText: "row 28" })).toContainText("0x1aa2bb31");
    await expect(pane.locator("tr", { hasText: "row 30" })).toContainText("revision code");
    await expect(pane.locator("pre").nth(1)).toContainText('model = "Raspberry Pi 4 Model B Rev 1.4"');
  });

  test("what is missing says so", async ({ app }) => {
    await app.running();
    await app.showTab("info");
    await app.emit(info({ deviceTree: null, bootconf: [], otp: [], state: { ...info().state, ended: true, ok: false, why: "the VPU faulted", logDropped: 12 } }));
    const pane = app.page.locator("#tab-info");
    await expect(pane).toContainText("The firmware has not built it yet.");
    await expect(pane).toContainText("No bootconf.txt.");
    await expect(pane).toContainText("All blank.");
    await expect(pane.locator("tr", { hasText: "Outcome" })).toContainText("failed – the VPU faulted");
    await expect(pane.locator("tr", { hasText: "Log lines dropped" })).toContainText("12");
  });

  test("the flash and the device tree can be downloaded", async ({ app }) => {
    await app.running();
    await app.showTab("info");
    await app.emit(info());
    await app.page.getByRole("button", { name: "Download the flash as the firmware left it" }).click();
    expect((await app.sent()).at(-1)).toEqual({ type: "save", what: "eeprom" });
    await app.page.getByRole("button", { name: "Download .dtb" }).click();
    expect((await app.sent()).at(-1)).toEqual({ type: "save", what: "dtb" });

    const [flash] = await Promise.all([
      app.page.waitForEvent("download"),
      app.emit({ type: "file", what: "eeprom", bytes: new Uint8Array([1, 2, 3]) }),
    ]);
    expect(flash.suggestedFilename()).toBe("pieeprom-dump.bin");
    expect([...readFileSync(await flash.path())]).toEqual([1, 2, 3]);
    const [tree] = await Promise.all([
      app.page.waitForEvent("download"),
      app.emit({ type: "file", what: "dtb", bytes: new Uint8Array([0xd0, 0x0d, 0xfe, 0xed]) }),
    ]);
    expect(tree.suggestedFilename()).toBe("device-tree.dtb");
  });

  test("a download is given up to the browser and its link let go", async ({ app }) => {
    await app.running();
    await app.showTab("info");
    await app.emit(info());
    await app.page.clock.install();
    const [download] = await Promise.all([
      app.page.waitForEvent("download"),
      app.emit({ type: "file", what: "dtb", bytes: new Uint8Array([1]) }),
    ]);
    expect(download.suggestedFilename()).toBe("device-tree.dtb");
    await app.page.clock.runFor(6000);
  });

  test("it asks again every few seconds while it is open, and stops when it is not", async ({ app }) => {
    await app.running();
    await app.showTab("info");
    const asks = async () => (await app.sent()).filter((m) => m.type === "info").length;
    await expect.poll(asks, { timeout: 8000 }).toBeGreaterThanOrEqual(2);
    await app.showTab("console");
    const before = await asks();
    await app.page.waitForTimeout(3000);
    expect(await asks()).toBe(before);
  });

  test("it has no horizontal scrollbar, however long a line of the device tree is", async ({ app }) => {
    await app.running();
    await app.showTab("info");
    await app.emit(info({ deviceTree: `/ {\n\tbootargs = "${"console=ttyAMA0,115200 ".repeat(40)}";\n\tlong = <${"0x12345678 ".repeat(80)}>;\n};\n`, bootconf: ["[all] HTTP_CACERT_HASH=" + "ab".repeat(80)] }));
    const overflow = await app.page.locator("#tab-info").evaluate((pane) => ({
      pane: pane.scrollWidth > pane.clientWidth,
      pres: [...pane.querySelectorAll("pre")].map((p) => p.scrollWidth > p.clientWidth),
      page: document.documentElement.scrollWidth > innerWidth,
    }));
    expect(overflow).toEqual({ pane: false, pres: [false, false], page: false });
  });
});

test.describe("tabs", () => {
  test("one is selected at a time", async ({ app }) => {
    await expect(app.tab("console")).toHaveAttribute("aria-selected", "true");
    await app.showTab("log");
    await expect(app.tab("log")).toHaveAttribute("aria-selected", "true");
    await expect(app.tab("console")).toHaveAttribute("aria-selected", "false");
    await expect(app.page.locator("#tab-console")).toBeHidden();
    await app.showTab("info");
    await expect(app.page.locator("#tab-log")).toBeHidden();
    await app.showTab("console");
    await expect(app.page.locator("#tab-info")).toBeHidden();
  });
});
