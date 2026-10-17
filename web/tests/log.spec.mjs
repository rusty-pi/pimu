import { expect, test } from "./fixtures.mjs";

const line = (row) => JSON.stringify(row);

test.describe("choosing what to log", () => {
  test("io is logged by default, and each channel is a toggle that says whether it is on", async ({ app }) => {
    await app.showTab("log");
    const chip = (name) => app.page.locator("#tab-log details .chip-btn", { hasText: new RegExp(`^${name}$`) });
    await expect(chip("io")).toHaveAttribute("aria-pressed", "true");
    await expect(chip("mbox")).toHaveAttribute("aria-pressed", "false");
    await expect(app.page.locator("#tab-log summary")).toHaveText("Channels to record (1 on)");

    await chip("mbox").click();
    await expect(chip("mbox")).toHaveAttribute("aria-pressed", "true");
    await expect(app.page.locator("#tab-log summary")).toHaveText("Channels to record (2 on)");
    await chip("io").click();
    await expect(chip("io")).toHaveAttribute("aria-pressed", "false");
    expect((await app.localState()).log).toEqual(["mbox"]);
  });

  test("None, Just I/O and All are presets", async ({ app }) => {
    await app.showTab("log");
    const summary = app.page.locator("#tab-log summary");
    await app.page.getByRole("button", { name: "All (slow)" }).click();
    await expect(summary).toHaveText("Channels to record (15 on)");
    await app.page.getByRole("button", { name: "None" }).click();
    await expect(summary).toHaveText("Channels to record (0 on)");
    await app.page.getByRole("button", { name: "Just I/O" }).click();
    await expect(summary).toHaveText("Channels to record (1 on)");
  });

  test("the channels are fixed once the board is powered", async ({ app }) => {
    await app.showTab("log");
    await app.powerOn();
    await expect(app.page.locator("#tab-log details .chip-btn").first()).toBeDisabled();
    await expect(app.page.getByRole("button", { name: "None" })).toBeDisabled();
    await expect(app.page.locator("#tab-log details")).toContainText("Power off to change the channels");
    await app.powerButton.click();
    await expect(app.page.locator("#tab-log details .chip-btn").first()).toBeEnabled();
  });

  test("the choice is asked of the machine at power-on", async ({ app }) => {
    await app.showTab("log");
    await app.page.locator("#tab-log details .chip-btn", { hasText: /^gpio$/ }).click();
    await app.powerOn();
    expect((await app.request()).options.log).toBe("io,gpio");
  });
});

test.describe("reading the log", () => {
  test("lines come in, in order, with the model's time and a colour per channel", async ({ app }) => {
    await app.running();
    await app.showTab("log");
    await app.emit({
      type: "log",
      text: [
        line({ us: 1500, channel: "mbox", msg: "VPU read request 0x1" }),
        line({ us: 2_000_000, channel: "io", dev: "sd", op: "read", lba: "0x0", blocks: 1, files: ["(partition table)"] }),
        "not json at all",
        "",
        line({ us: 2_000_100, channel: "otp", msg: "row 30" }),
      ].join("\n") + "\n",
    });
    const rows = app.page.locator(".logrow");
    await expect(rows).toHaveCount(3);
    await expect(rows.nth(0)).toHaveText("0.001500mboxVPU read request 0x1");
    await expect(rows.nth(1)).toHaveText("2.000000iodev=sd  op=read  lba=0x0  blocks=1  files=(partition table)");
    await expect(rows.nth(2)).toHaveText("2.000100otprow 30");
    await expect(app.page.locator("#log-count")).toHaveText("(3)");
    const colours = await rows.locator(".c").evaluateAll((els) => els.map((e) => e.style.color));
    expect(new Set(colours).size).toBe(3);
  });

  test("the lines are there when the log tab is opened later", async ({ app }) => {
    await app.running();
    await app.emit({ type: "log", text: line({ us: 1, channel: "io", msg: "early" }) + "\n" });
    await expect(app.page.locator("#log-count")).toHaveText("(1)");
    await app.showTab("log");
    await expect(app.page.locator(".logrow")).toHaveCount(1);
  });

  test("a channel can be hidden, and shows how many lines it has", async ({ app }) => {
    await app.running();
    await app.showTab("log");
    await app.emit({
      type: "log",
      text: [line({ us: 1, channel: "io", msg: "a" }), line({ us: 2, channel: "io", msg: "b" }), line({ us: 3, channel: "mbox", msg: "c" })].join("\n"),
    });
    const filters = app.page.locator("#tab-log > .channels .chip-btn");
    await expect(filters).toHaveText(["io2", "mbox1"]);
    await filters.first().click();
    await expect(filters.first()).toHaveAttribute("aria-pressed", "false");
    await expect(app.page.locator(".logrow")).toHaveText(["0.000003mboxc"]);
    await filters.first().click();
    await expect(app.page.locator(".logrow")).toHaveCount(3);
  });

  test("lines can be searched, by text and by channel", async ({ app }) => {
    await app.running();
    await app.showTab("log");
    await app.emit({
      type: "log",
      text: [line({ us: 1, channel: "io", msg: "read start4.elf" }), line({ us: 2, channel: "mbox", msg: "tag 0x1" }), line({ us: 3, channel: "io", msg: "read fixup4.dat" })].join("\n"),
    });
    const search = app.page.getByLabel("Filter log lines");
    await search.fill("FIXUP");
    await expect(app.page.locator(".logrow")).toHaveCount(1);
    await search.fill("mbox");
    await expect(app.page.locator(".logrow")).toHaveText(["0.000002mboxtag 0x1"]);
    await search.fill("nothing like it");
    await expect(app.page.locator(".logrow")).toHaveCount(0);
    await search.fill("");
    await expect(app.page.locator(".logrow")).toHaveCount(3);
  });

  test("only the newest lines are shown, and the oldest are dropped past a limit", async ({ app }) => {
    await app.running();
    await app.showTab("log");
    await app.page.evaluate(() => {
      const text = Array.from({ length: 20_500 }, (_, i) => JSON.stringify({ us: i, channel: "io", msg: `row ${i}` })).join("\n");
      window.__machine().emit({ type: "log", text });
    });
    await expect(app.page.locator("#log-count")).toHaveText("(20000)");
    await expect(app.page.locator(".logrow")).toHaveCount(600);
    await expect(app.page.locator(".logrow").last()).toContainText("row 20499");
    await expect(app.page.locator(".logrow").first()).toContainText("row 19900");
  });

  test("it can be saved as jsonl", async ({ app }) => {
    await app.running();
    await app.showTab("log");
    const rows = [{ us: 1, channel: "io", msg: "a" }, { us: 2, channel: "mbox", msg: "b" }];
    await app.emit({ type: "log", text: rows.map(line).join("\n") });
    const [download] = await Promise.all([app.page.waitForEvent("download"), app.page.getByRole("button", { name: "Save .jsonl" }).click()]);
    expect(download.suggestedFilename()).toBe("pimu-log.jsonl");
    const path = await download.path();
    const { readFileSync } = await import("node:fs");
    expect(readFileSync(path, "utf8").split("\n").map((l) => JSON.parse(l))).toEqual(rows);
  });

  test("a new power-on starts a new log", async ({ app }) => {
    await app.running();
    await app.emit({ type: "log", text: line({ us: 1, channel: "io", msg: "old" }) });
    await expect(app.page.locator("#log-count")).toHaveText("(1)");
    await app.powerButton.click();
    await app.powerButton.click();
    await expect(app.page.locator("#log-count")).toHaveText("");
    await app.showTab("log");
    await expect(app.page.locator(".logrow")).toHaveCount(0);
    await expect(app.page.locator("#logbox")).toContainText("Nothing logged yet.");
  });
});
