import { expect, test } from "./fixtures.mjs";

const changes = (app) => app.drawer.locator(".changes li");
const badge = (app, part) => app.part(part).locator(".badge");

test("a board as the page sets it up has nothing to list", async ({ app }) => {
  await expect(app.drawer.locator(".changes-pane")).toContainText("Nothing changed yet");
  await expect(changes(app)).toHaveCount(0);
  await expect(app.page.locator("#board .badge")).toHaveCount(0);
});

test("what you change is listed against the default, and counted on its part", async ({ app }) => {
  await app.open("sd");
  await app.field("Lines added to config.txt").locator("textarea").fill("arm_64bit=1\nenable_uart=1\ndtoverlay=disable-bt");
  await app.open("hdmi");
  await app.drawer.getByRole("checkbox", { name: "Plug in a monitor" }).check();
  await app.drawer.getByRole("button", { name: /Changes/ }).click();

  await expect(changes(app)).toHaveText([
    "microSDconfig.txtreset− uart_2ndstage=1+ dtoverlay=disable-bt",
    "HDMI 0Monitorresetnone → plugged in",
  ]);
  await expect(badge(app, "sd")).toHaveText("1");
  await expect(badge(app, "hdmi")).toHaveText("1");
  await expect(badge(app, "usb")).toHaveCount(0);
});

test("memory is counted on the memory chip, a bootloader setting on the EEPROM", async ({ app }) => {
  await app.open("soc");
  await app.field("Memory").locator("select").selectOption("2048");
  await app.open("eeprom");
  const row = await app.setting("WAKE_ON_GPIO", "Serial and power");
  await row.locator("select").selectOption("0");
  await app.drawer.getByRole("button", { name: /Changes/ }).click();

  await expect(changes(app)).toHaveText([
    "EEPROMWAKE_ON_GPIOresetthe image's → 0",
    "BCM2711Memoryreset1 GiB → 2 GiB",
  ]);
  await expect(app.page.locator('#board .part:has([data-sub="memory"]) .badge')).toHaveText("1");
  await expect(app.page.locator("#board .part[data-part=soc]").first().locator(".badge")).toHaveCount(0);
  await expect(badge(app, "eeprom")).toHaveText("1");
});

test("a change resets from the list, and the part opens from it", async ({ app }) => {
  await app.open("usb");
  await app.drawer.getByRole("radio", { name: /A USB stick/ }).check();
  await app.drawer.getByRole("button", { name: /Changes/ }).click();
  await expect(changes(app)).toHaveCount(1);

  await changes(app).getByRole("button", { name: "USB 3.0", exact: true }).click();
  await expect(app.part("usb")).toHaveAttribute("data-open", "true");
  await app.drawer.getByRole("button", { name: /Changes/ }).click();

  await app.drawer.getByRole("button", { name: "Reset USB 3.0: Stick" }).click();
  await expect(changes(app)).toHaveCount(0);
  await expect(badge(app, "usb")).toHaveCount(0);
  expect((await app.localState()).usb.kind).toBe("none");
});

test("everything resets at once, once you say so", async ({ app }) => {
  await app.open("eth");
  await app.drawer.getByRole("checkbox", { name: "Plug the cable in" }).check();
  await app.open("hdmi");
  await app.drawer.getByRole("checkbox", { name: "Plug in a monitor" }).check();
  await app.drawer.getByRole("button", { name: /Changes/ }).click();
  await expect(changes(app)).toHaveCount(2);

  app.page.once("dialog", (dialog) => dialog.dismiss());
  await app.drawer.getByRole("button", { name: "Reset everything" }).click();
  await expect(changes(app)).toHaveCount(2);

  app.page.once("dialog", (dialog) => dialog.accept());
  await app.drawer.getByRole("button", { name: "Reset everything" }).click();
  await expect(changes(app)).toHaveCount(0);
  const saved = await app.localState();
  expect([saved.net.plugged, saved.display]).toEqual([false, false]);
});

test("nothing resets while the board is powered", async ({ app }) => {
  await app.open("hdmi");
  await app.drawer.getByRole("checkbox", { name: "Plug in a monitor" }).check();
  await app.drawer.getByRole("button", { name: /Changes/ }).click();
  await app.running();
  await expect(app.drawer.getByRole("button", { name: "Reset HDMI 0: Monitor" })).toBeDisabled();
  await expect(app.drawer.getByRole("button", { name: "Reset everything" })).toBeDisabled();
});

test("a log channel is a change too, and opens the log", async ({ app }) => {
  await app.page.locator("#tab-log details .chip-btn", { hasText: /^mbox$/ }).click();
  await expect(changes(app)).toHaveText(["LogChannelsresetio → io, mbox"]);
  await app.showTab("info");
  await changes(app).getByRole("button", { name: "Log", exact: true }).click();
  await expect(app.page.locator("#tab-log")).toBeVisible();
});

test("the count on the way back follows what you change", async ({ app }) => {
  await app.open("hdmi");
  const back = app.drawer.getByRole("button", { name: /Changes/ });
  await expect(back).toHaveText("← Changes");
  await app.drawer.getByRole("checkbox", { name: "Plug in a monitor" }).check();
  await expect(back).toHaveText("← Changes (1)");
});

test("a bootloader setting resets from the list to what the page sets", async ({ app }) => {
  await app.open("eeprom");
  const row = await app.setting("BOOT_UART", "Serial and power");
  await row.getByRole("button", { name: /Reset/ }).click();
  await app.drawer.getByRole("button", { name: /Changes/ }).click();
  await expect(changes(app)).toHaveText(["EEPROMBOOT_UARTreset1 → the image's"]);
  await app.drawer.getByRole("button", { name: "Reset EEPROM: BOOT_UART" }).click();
  await expect(changes(app)).toHaveCount(0);
  expect((await app.localState()).bootconf).toEqual({ BOOT_UART: "1" });
});

test("a picked file is a change, and resets by letting go of it", async ({ app }) => {
  await app.open("gpio");
  await app.drawer.locator('input[type="file"]').setInputFiles({ name: "hat.eep", mimeType: "application/octet-stream", buffer: Buffer.alloc(64) });
  await app.drawer.getByRole("button", { name: /Changes/ }).click();
  await expect(changes(app)).toHaveText(["GPIOHAT ID EEPROMresetnone → hat.eep"]);
  await expect(badge(app, "gpio")).toHaveText("1");
  await app.drawer.getByRole("button", { name: "Reset GPIO: HAT ID EEPROM" }).click();
  await expect(changes(app)).toHaveCount(0);
  await expect(app.page.locator('[data-sub="gpio"]')).toHaveText("no HAT");
});
