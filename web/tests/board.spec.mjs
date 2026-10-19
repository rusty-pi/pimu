import { expect, test } from "./fixtures.mjs";

test("the board shows its parts, with nothing plugged in beyond the card", async ({ app }) => {
  await expect(app.status).toHaveText("Off");
  await expect(app.powerButton).toHaveAttribute("aria-checked", "false");
  for (const part of ["sd", "eeprom", "usb", "usbc", "eth", "hdmi", "gpio", "soc"]) {
    await expect(app.part(part)).toBeVisible();
  }
  const sub = (key) => app.page.locator(`[data-sub="${key}"]`);
  await expect(sub("sd")).toHaveText("official firmware");
  await expect(sub("eeprom")).toHaveText("official · 1 edited");
  await expect(sub("memory")).toHaveText("1 GiB");
  await expect(sub("usb")).toHaveText("empty");
  await expect(sub("usbc")).toHaveText("empty");
  await expect(sub("eth")).toHaveText("unplugged");
  await expect(sub("hdmi")).toHaveText("no monitor");
  await expect(sub("gpio")).toHaveText("no HAT");
  await expect(app.part("sd")).toHaveAttribute("data-filled", "true");
  await expect(app.part("usb")).toHaveAttribute("data-filled", "false");
  await expect(app.page.locator("#soc-title")).toHaveText("BCM2711 C0");
});

test("the six cores are tiles of one size", async ({ app }) => {
  const sizes = await app.page.locator("#board [data-core]").evaluateAll((tiles) =>
    tiles.map((t) => ({ core: t.dataset.core, w: t.getBoundingClientRect().width, h: t.getBoundingClientRect().height })));
  expect(sizes).toHaveLength(6);
  expect(sizes.map((s) => s.core).sort()).toEqual(["arm0", "arm1", "arm2", "arm3", "vpu0", "vpu1"]);
  for (const size of sizes) {
    expect(size.w).toBeCloseTo(sizes[0].w, 1);
    expect(size.h).toBeCloseTo(sizes[0].h, 1);
  }
});

test("a part opens its panel, and the same part closes it", async ({ app }) => {
  await expect(app.drawer).toHaveAttribute("data-empty", "true");
  await app.open("sd");
  await expect(app.drawer).toContainText("microSD card");
  await app.part("sd").click();
  await expect(app.part("sd")).toHaveAttribute("data-open", "false");
  await expect(app.drawer).toHaveAttribute("data-empty", "true");
  await expect(app.changes.locator(".changes-pane")).toBeVisible();
});

test("a part opens from the keyboard", async ({ app }) => {
  await app.part("usb").focus();
  await app.page.keyboard.press("Enter");
  await expect(app.drawer).toContainText("USB 3.0 socket");
  await app.part("eeprom").focus();
  await app.page.keyboard.press("Space");
  await expect(app.drawer).toContainText("Boot EEPROM");
  await expect(app.part("usb")).toHaveAttribute("data-open", "false");
});

test("the memory chip opens the board panel too", async ({ app }) => {
  await app.page.locator('#board .part[data-part="soc"]').nth(1).click();
  await expect(app.drawer).toContainText("BCM2711 and the board");
});

test("a panel's close button closes it", async ({ app }) => {
  await app.open("hdmi");
  await app.drawer.getByRole("button", { name: "Close" }).click();
  await expect(app.part("hdmi")).toHaveAttribute("data-open", "false");
  await expect(app.drawer).toContainText("Click a part of the board");
});

test("every panel is editable while the board is off", async ({ app }) => {
  for (const part of ["sd", "eeprom", "usb", "usbc", "eth", "hdmi", "gpio", "soc"]) {
    await app.open(part);
    await expect(app.drawer.locator("fieldset")).toBeEnabled();
  }
});

test("traffic on the io channel makes the part's wire wobble, and it settles once quiet", async ({ app }) => {
  await app.running();
  const rows = [
    { dev: "eeprom", op: "read", addr: 4096, bytes: 64 },
    { dev: "sd", op: "read", lba: 0, blocks: 1, files: [] },
    { dev: "usb", op: "read", lba: 0, blocks: 1, files: [] },
    { dev: "otg", op: "write", lba: 0, blocks: 1, files: [] },
    { dev: "net", event: "dhcp: DISCOVER" },
  ];
  await app.emit({ type: "log", text: rows.map((row) => JSON.stringify({ us: 1, channel: "io", ...row })).join("\n") + "\n" });
  for (const part of ["eeprom", "sd", "usb", "usbc", "eth"]) {
    await expect(app.part(part)).toHaveAttribute("data-busy", "true");
    await expect(app.part(part).locator(".wave")).toHaveAttribute("d", /^M/);
  }
  await expect(app.part("hdmi")).not.toHaveAttribute("data-busy", "true");
  for (const part of ["eeprom", "sd", "usb", "usbc", "eth"]) {
    await expect(app.part(part)).toHaveAttribute("data-busy", "false");
    await expect(app.part(part)).toHaveAttribute("data-wobbling", "false");
  }
});
