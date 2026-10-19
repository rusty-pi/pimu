import { expect, test } from "./fixtures.mjs";

const changes = (app) => app.changes.locator(".changes li");
const badge = (app, part) => app.part(part).locator(".badge");

test("a board as the page sets it up has nothing to list", async ({ app }) => {
  await expect(app.changes).toContainText("Nothing changed yet");
  await expect(changes(app)).toHaveCount(0);
  await expect(app.page.locator("#board .badge")).toHaveCount(0);
});

test("what you change is listed against the default, and counted on its part", async ({ app }) => {
  await app.open("sd");
  await app.field("Lines added to config.txt").locator("textarea").fill("arm_64bit=1\nenable_uart=1\ndtoverlay=disable-bt");
  await app.open("hdmi");
  await app.drawer.getByRole("checkbox", { name: "Plug in a monitor" }).check();

  await expect(changes(app)).toHaveText([
    "microSDconfig.txtreset− uart_2ndstage=1+ dtoverlay=disable-bt",
    "HDMI 0Monitorresetnone → plugged in",
  ]);
  await expect(badge(app, "sd")).toHaveText("1");
  await expect(badge(app, "hdmi")).toHaveText("1");
  await expect(badge(app, "usb")).toHaveCount(0);
  expect(await app.part("sd").evaluate((g) => g.lastElementChild.classList.contains("badge")), "drawn over the outline").toBe(true);
});

test("memory is counted on the memory chip, a bootloader setting on the EEPROM", async ({ app }) => {
  await app.open("soc");
  await app.field("Memory").locator("select").selectOption("2048");
  await app.open("eeprom");
  const row = await app.setting("WAKE_ON_GPIO", "Serial and power");
  await row.locator("select").selectOption("0");

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
  await expect(changes(app)).toHaveCount(1);

  await changes(app).getByRole("button", { name: "USB 3.0", exact: true }).click();
  await expect(app.part("usb")).toHaveAttribute("data-open", "true");

  await app.changes.getByRole("button", { name: "Reset USB 3.0: Stick" }).click();
  await expect(changes(app)).toHaveCount(0);
  await expect(badge(app, "usb")).toHaveCount(0);
  expect((await app.localState()).usb.kind).toBe("none");
});

test("no single change resets while the board is powered", async ({ app }) => {
  await app.open("hdmi");
  await app.drawer.getByRole("checkbox", { name: "Plug in a monitor" }).check();
  await app.running();
  await expect(app.changes.getByRole("button", { name: "Reset HDMI 0: Monitor" })).toBeDisabled();
});

test("a log channel is a change too, and opens the log", async ({ app }) => {
  await app.showTab("log");
  await app.page.locator("#tab-log details .chip-btn", { hasText: /^mbox$/ }).click();
  await expect(changes(app)).toHaveText(["LogChannelsresetio → io, mbox"]);
  await app.showTab("info");
  await changes(app).getByRole("button", { name: "Log", exact: true }).click();
  await expect(app.page.locator("#tab-log")).toBeVisible();
});

test("text that only gains blank lines or spaces is no change, lines moved are", async ({ app }) => {
  await app.open("sd");
  const box = app.field("Lines added to config.txt").locator("textarea");
  await box.fill("arm_64bit=1\n\nenable_uart=1  \nuart_2ndstage=1\n");
  await expect(changes(app)).toHaveCount(0);
  await expect(badge(app, "sd")).toHaveCount(0);
  await box.fill("enable_uart=1\narm_64bit=1\nuart_2ndstage=1");
  await expect(changes(app)).toHaveText(["microSDconfig.txtreset− arm_64bit=1− enable_uart=1− uart_2ndstage=1+ enable_uart=1+ arm_64bit=1+ uart_2ndstage=1"]);
});

test("the list follows what you change while the part stays open", async ({ app }) => {
  await app.open("hdmi");
  await expect(changes(app)).toHaveCount(0);
  await app.drawer.getByRole("checkbox", { name: "Plug in a monitor" }).check();
  await expect(changes(app)).toHaveCount(1);
  await expect(app.part("hdmi")).toHaveAttribute("data-open", "true");
  await app.changes.getByRole("button", { name: "Reset HDMI 0: Monitor" }).click();
  await expect(app.drawer.getByRole("checkbox", { name: "Plug in a monitor" })).not.toBeChecked();
});

test("a bootloader setting resets from the list to what the page sets", async ({ app }) => {
  await app.open("eeprom");
  const row = await app.setting("BOOT_UART", "Serial and power");
  await row.getByRole("button", { name: /Reset/ }).click();
  await expect(changes(app)).toHaveText(["EEPROMBOOT_UARTreset1 → the image's"]);
  await app.changes.getByRole("button", { name: "Reset EEPROM: BOOT_UART" }).click();
  await expect(changes(app)).toHaveCount(0);
  expect((await app.localState()).bootconf).toEqual({ BOOT_UART: "1" });
});

test("a picked file is a change, and resets by letting go of it", async ({ app }) => {
  await app.open("gpio");
  await app.drawer.locator('input[type="file"]').setInputFiles({ name: "hat.eep", mimeType: "application/octet-stream", buffer: Buffer.alloc(64) });
  await expect(changes(app)).toHaveText(["GPIOHAT ID EEPROMresetnone → hat.eep"]);
  await expect(badge(app, "gpio")).toHaveText("1");
  await app.changes.getByRole("button", { name: "Reset GPIO: HAT ID EEPROM" }).click();
  await expect(changes(app)).toHaveCount(0);
  await expect(app.page.locator('[data-sub="gpio"]')).toHaveText("no HAT");
});

test.describe("sharing", () => {
  const shareLink = async (app) => {
    await app.page.context().grantPermissions(["clipboard-read", "clipboard-write"]);
    await app.changes.getByRole("button", { name: "Share as a link" }).click();
    await expect(app.page.locator("#toast")).toContainText("Link copied.");
    expect(await app.page.evaluate(() => location.hash)).toBe("");
    return app.page.evaluate(() => navigator.clipboard.readText());
  };

  // A fresh visit to the link: no saved settings of its own.
  const visit = async (app, link) => {
    const blank = new URL("/blank", app.page.url()).href;
    await app.page.route(blank, (route) => route.fulfill({ contentType: "text/html", body: "" }));
    await app.page.goto(blank);
    await app.page.evaluate(() => localStorage.clear());
    await app.page.goto(link);
  };

  test("a link carries the changes to another visit, and the fragment is cleared", async ({ app }) => {
    await app.open("sd");
    await app.field("Lines added to config.txt").locator("textarea").fill("arm_64bit=1\ndtoverlay=disable-bt");
    await app.open("soc");
    await app.field("Memory").locator("select").selectOption("2048");
    await app.open("eeprom");
    await (await app.setting("WAKE_ON_GPIO", "Serial and power")).locator("select").selectOption("0");
    await app.drawer.getByRole("combobox", { name: "Boot order preset" }).selectOption({ index: 1 });
    await app.showTab("log");
    await app.page.locator("#tab-log details .chip-btn", { hasText: /^mbox$/ }).click();
    const before = await changes(app).allTextContents();

    const link = await shareLink(app);
    expect(new URL(link).hash).toMatch(/^#board=[\w-]+$/);
    await visit(app, link);
    await expect(app.page.locator("#toast")).toHaveText("Opened the board from the link.");
    await expect(changes(app)).toHaveText(before);
    expect(await app.page.evaluate(() => location.hash)).toBe("");
    expect((await app.localState()).board.memoryMb).toBe(2048);
  });

  test("without the clipboard the link goes to the address bar", async ({ app }) => {
    await app.open("hdmi");
    await app.drawer.getByRole("checkbox", { name: "Plug in a monitor" }).check();
    await app.page.evaluate(() => Object.defineProperty(navigator, "clipboard", { value: { writeText: () => Promise.reject(new Error("denied")) } }));
    await app.changes.getByRole("button", { name: "Share as a link" }).click();
    await expect(app.page.locator("#toast")).toHaveText("The link is in the address bar.");
    expect(await app.page.evaluate(() => location.hash)).toMatch(/^#board=/);
  });

  test("a link says when the files you picked stay behind", async ({ app }) => {
    await app.open("gpio");
    await app.drawer.locator('input[type="file"]').setInputFiles({ name: "hat.eep", mimeType: "application/octet-stream", buffer: Buffer.alloc(64) });
    await app.changes.getByRole("button", { name: "Share as a link" }).click();
    await expect(app.page.locator("#toast")).toContainText("The files you picked are not in it");
  });

  test("a link does not replace your own changes unless you say so", async ({ app }) => {
    await app.open("hdmi");
    await app.drawer.getByRole("checkbox", { name: "Plug in a monitor" }).check();
    const link = await shareLink(app);
    await app.drawer.getByRole("checkbox", { name: "Plug in a monitor" }).uncheck();
    await app.open("eth");
    await app.drawer.getByRole("checkbox", { name: "Plug the cable in" }).check();

    app.page.once("dialog", (dialog) => dialog.dismiss());
    await app.page.evaluate((url) => (location.href = url), link);
    await expect(changes(app)).toHaveText(["EthernetCableresetunplugged → plugged in"]);

    app.page.once("dialog", (dialog) => dialog.accept());
    await app.page.evaluate((url) => (location.href = url), link);
    await expect(changes(app)).toHaveText(["HDMI 0Monitorresetnone → plugged in"]);
  });

  test("a link that cannot be read says so, and what it cannot use is left alone", async ({ app }) => {
    await visit(app, "/#board=not-deflate");
    await expect(app.page.locator("#toast")).toHaveText("The board in the link could not be read.");
    await expect(changes(app)).toHaveCount(0);
  });

  test("what a link sets outside the page's choices keeps the default", async ({ app }) => {
    const shared = { sd: { mode: "x" }, board: { stepping: "z9", memoryMb: 99999 }, log: ["io", "nope"], bootOrder: ["z"], usb: { kind: "file" } };
    const link = await app.page.evaluate(async (json) => {
      const packed = await new Response(new Blob([new TextEncoder().encode(json)]).stream().pipeThrough(new CompressionStream("deflate-raw"))).arrayBuffer();
      return `#board=${btoa(String.fromCharCode(...new Uint8Array(packed))).replace(/\+/g, "-").replace(/\//g, "_").replace(/=+$/, "")}`;
    }, JSON.stringify(shared));
    await visit(app, `/${link}`);
    await expect(app.page.locator("#toast")).toHaveText("Opened the board from the link.");
    await expect(changes(app)).toHaveText(["USB 3.0Stickresetnothing → a local file"]);
    await expect(app.page.locator('[data-sub="sd"]')).toHaveText("official firmware");
    const saved = await app.localState();
    expect([saved.board, saved.log, saved.bootOrder]).toEqual([{ stepping: "c0", memoryMb: 1024, boardRev: "" }, ["io"], null]);
  });

  test("a link opens nothing while the board is powered", async ({ app }) => {
    await app.running();
    await app.page.evaluate(() => (location.hash = "board=x"));
    await expect(app.page.locator("#toast")).toHaveText("Power the board off to open the one in the link.");
  });
});
