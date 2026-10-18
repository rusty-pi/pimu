import { data } from "./data.mjs";
import { expect, test } from "./fixtures.mjs";

test.describe("microSD", () => {
  test("official firmware is the default, and GitHub, files and images are the other cards", async ({ app }) => {
    await app.open("sd");
    await expect(app.drawer.getByRole("radio", { name: /Official firmware/ })).toBeChecked();

    await app.drawer.getByRole("radio", { name: /Boot files from a GitHub directory/ }).check();
    const repo = app.field("Repository").locator("input[type=text]").first();
    await expect(repo).toHaveValue("raspberrypi/firmware");
    await repo.fill("someone/firmware");
    await expect(app.page.locator('[data-sub="sd"]')).toHaveText("someone/firmware");
    const inputs = app.field("Repository").locator("input[type=text]");
    await inputs.nth(1).fill("stable");
    await inputs.nth(2).fill("boot");
    expect((await app.localState()).sd.github).toEqual({ repo: "someone/firmware", ref: "stable", path: "boot" });

    await app.drawer.getByRole("radio", { name: /Official firmware/ }).check();
    await expect(app.page.locator('[data-sub="sd"]')).toHaveText("official firmware");
  });

  test("boot files come from a folder or from files", async ({ app }) => {
    await app.open("sd");
    await app.drawer.getByRole("radio", { name: /Boot files from my computer/ }).check();
    await expect(app.field("What is on the card").locator("..")).toContainText("nothing chosen");
    await expect(app.part("sd")).toHaveAttribute("data-filled", "false");
    await expect(app.page.locator('[data-sub="sd"]')).toHaveText("no files");

    await app.drawer.locator("input[type=file][webkitdirectory]").setInputFiles(data("boot"));
    await expect(app.drawer).toContainText("4 files");
    await expect(app.drawer).toContainText("overlays/disable-bt.dtbo");
    await expect(app.page.locator('[data-sub="sd"]')).toHaveText("4 files");
    await expect(app.part("sd")).toHaveAttribute("data-filled", "true");

    await app.drawer.locator("input[type=file]:not([webkitdirectory])").setInputFiles([data("boot/start4.elf"), data("boot/fixup4.dat")]);
    await expect(app.drawer).toContainText("2 files: start4.elf, fixup4.dat");
  });

  test("a disk image is a local file or a URL, and takes no config.txt", async ({ app }) => {
    await app.open("sd");
    await expect(app.drawer).toContainText("config.txt and cmdline.txt");
    await app.drawer.getByRole("radio", { name: /A disk image/ }).check();
    await expect(app.drawer).not.toContainText("config.txt and cmdline.txt");
    await expect(app.page.locator('[data-sub="sd"]')).toHaveText("no URL");

    await app.drawer.getByRole("radio", { name: "URL" }).check();
    await app.drawer.getByLabel("a card image URL").fill("https://example.org/images/card.img");
    await expect(app.page.locator('[data-sub="sd"]')).toHaveText("card.img");
    await expect(app.part("sd")).toHaveAttribute("data-filled", "true");

    await app.drawer.getByRole("radio", { name: "Local file" }).check();
    await expect(app.drawer).toContainText("nothing chosen");
    await expect(app.page.locator('[data-sub="sd"]')).toHaveText("no file");
    await app.drawer.locator("input[type=file]").setInputFiles(data("disk.img"));
    await expect(app.drawer).toContainText("disk.img (4 KiB)");
    await expect(app.page.locator('[data-sub="sd"]')).toHaveText("disk.img");
  });

  test("config.txt lines and the command line are kept", async ({ app }) => {
    await app.open("sd");
    const config = app.drawer.locator("textarea");
    await expect(config).toHaveValue(/arm_64bit=1\nenable_uart=1/);
    await config.fill("arm_64bit=1\ngpu_mem=16");
    const cmdline = app.field("Kernel command line").locator("input");
    await expect(cmdline).toHaveValue("console=ttyAMA0,115200");
    await cmdline.fill("console=ttyAMA0,115200 quiet");
    const saved = await app.localState();
    expect(saved.configTxt).toBe("arm_64bit=1\ngpu_mem=16");
    expect(saved.cmdline).toBe("console=ttyAMA0,115200 quiet");
  });
});

test.describe("EEPROM", () => {
  test("the form starts from the image's own settings", async ({ app }) => {
    await app.open("eeprom");
    const serial = async (key) => app.setting(key, "Serial and power");
    await expect((await serial("BOOT_UART")).locator("select")).toHaveValue("1");
    await expect(await serial("BOOT_UART")).toHaveClass(/changed/);
    await expect((await serial("WAKE_ON_GPIO")).locator("select")).toHaveValue("1");
    await expect((await app.setting("NET_INSTALL_AT_POWER_ON", "HTTP boot")).locator("select")).toHaveValue("1");
    const delay = await app.setting("HDMI_DELAY", "Display");
    await expect(delay.locator("input")).toHaveValue("5");
    await expect(delay).toContainText("(documented default)");
    await expect(await serial("BOOT_UART")).not.toContainText("(documented default)");
    await expect(await serial("WAKE_ON_GPIO")).not.toHaveClass(/changed/);
    await expect(app.drawer).toContainText("The image also sets SOMETHING_NEW");
    await expect(app.drawer).toContainText("conditional sections too ([gpio4=1])");
  });

  test("the bootloader's serial console is on unless you turn it off", async ({ app }) => {
    await app.open("eeprom");
    const row = await app.setting("BOOT_UART", "Serial and power");
    await expect(row.locator("select")).toHaveValue("1");
    await expect(row).toHaveClass(/changed/);
    await expect(app.page.locator('[data-sub="eeprom"]')).toHaveText("official · 1 edited");
    await row.locator("select").selectOption("0");
    await expect(row).not.toHaveClass(/changed/);
    expect((await app.localState()).bootconf).toEqual({});
    await expect(app.page.locator('[data-sub="eeprom"]')).toHaveText("official 2026-08-04");
  });

  test("a changed setting says so, counts on the chip, and resets", async ({ app }) => {
    await app.open("eeprom");
    const row = await app.setting("WAKE_ON_GPIO", "Serial and power");
    await row.locator("select").selectOption("0");
    await expect(row).toHaveClass(/changed/);
    await expect(app.page.locator('[data-sub="eeprom"]')).toHaveText("official · 2 edited");
    expect((await app.localState()).bootconf).toEqual({ BOOT_UART: "1", WAKE_ON_GPIO: "0" });
    await row.getByRole("button", { name: "Reset Wake on GPIO" }).click();
    await expect(row).not.toHaveClass(/changed/);
    expect((await app.localState()).bootconf).toEqual({ BOOT_UART: "1" });
    await expect(app.page.locator('[data-sub="eeprom"]')).toHaveText("official · 1 edited");
  });

  test("numbers and text are settings too, and an unset one can be set", async ({ app }) => {
    await app.open("eeprom");
    const delay = await app.setting("HDMI_DELAY", "Display");
    await delay.locator("input").fill("9");
    await (await app.setting("TFTP_IP", "Network boot")).locator("input").fill("10.0.0.2");
    await (await app.setting("WAIT_FOR_POWER_BUTTON", "Serial and power")).locator("select").selectOption("1");
    expect((await app.localState()).bootconf).toEqual({ BOOT_UART: "1", HDMI_DELAY: "9", TFTP_IP: "10.0.0.2", WAIT_FOR_POWER_BUTTON: "1" });
    await delay.locator("input").fill("5");
    expect((await app.localState()).bootconf).not.toHaveProperty("HDMI_DELAY");
  });

  test("the boot order is a list of steps to edit", async ({ app }) => {
    await app.open("eeprom");
    const order = app.drawer.locator(".field", { hasText: "Boot order" });
    const preset = order.getByLabel("Boot order preset");
    await expect(preset).toHaveValue("default");
    await expect(order.locator(".step")).toHaveCount(0);

    await preset.selectOption({ label: "SD card, then USB, then restart" });
    await expect(order.locator(".step")).toHaveCount(3);
    await expect(order).toContainText("BOOT_ORDER=0xf41");
    expect((await app.localState()).bootOrder).toEqual(["1", "4", "f"]);

    await order.getByLabel("Boot step 1").selectOption("2");
    await expect(order).toContainText("BOOT_ORDER=0xf42");
    await expect(preset).toHaveValue("custom");

    await order.getByRole("button", { name: "Move up" }).nth(1).click();
    await expect(order).toContainText("BOOT_ORDER=0xf24");

    await order.getByRole("button", { name: "Remove step" }).first().click();
    await expect(order.locator(".step")).toHaveCount(2);
    await expect(order).toContainText("BOOT_ORDER=0xf2");

    await order.getByRole("button", { name: "Add a step" }).click();
    await expect(order.locator(".step")).toHaveCount(3);
    await expect(order).toContainText("BOOT_ORDER=0xf12");
    await expect(app.page.locator('[data-sub="eeprom"]')).toHaveText("official · 2 edited");

    await preset.selectOption("default");
    await expect(order.locator(".step")).toHaveCount(0);
    expect((await app.localState()).bootOrder).toBeNull();
  });

  test("editing a step does not change the preset it came from", async ({ app }) => {
    await app.open("eeprom");
    const order = app.drawer.locator(".field", { hasText: "Boot order" });
    const preset = order.getByLabel("Boot order preset");
    await preset.selectOption({ label: "SD card, then USB, then restart" });
    await order.getByLabel("Boot step 1").selectOption("2");
    await preset.selectOption("default");
    await preset.selectOption({ label: "SD card, then USB, then restart" });
    await expect(order.getByLabel("Boot step 1")).toHaveValue("1");
    await expect(order).toContainText("BOOT_ORDER=0xf41");
  });

  test("signed boot, tryboot, extra lines and a public key", async ({ app }) => {
    await app.open("eeprom");
    await app.drawer.getByRole("checkbox", { name: /Skip the signature check/ }).check();
    await app.drawer.getByRole("checkbox", { name: /Tryboot/ }).check();
    await app.field("Other lines").locator("textarea").fill("HTTP_HOST=example.org\n\nNET_BOOT_MAX_RETRIES=3");
    await app.field("Public key").locator("input[type=file]").setInputFiles(data("pubkey.bin"));
    await expect(app.field("Public key")).toContainText("pubkey.bin");
    const saved = await app.localState();
    expect(saved.skipSignedBoot).toBe(true);
    expect(saved.tryboot).toBe(true);
    await expect(app.page.locator('[data-sub="eeprom"]')).toHaveText("official · 3 edited");
  });

  test("the image is official, a URL, or a file", async ({ app }) => {
    await app.open("eeprom");
    await expect(app.drawer.getByRole("radio", { name: /Official pieeprom/ })).toBeChecked();

    await app.drawer.getByRole("radio", { name: /An image from a URL/ }).check();
    await expect(app.page.locator('[data-sub="eeprom"]')).toHaveText("pieepro… · 1 edited");
    await app.page.route("https://example.org/mine.bin", (route) =>
      route.fulfill({ status: 200, headers: { "content-length": "2048" }, body: Buffer.alloc(2048) }));
    await app.drawer.getByLabel("EEPROM image URL").fill("https://example.org/mine.bin");
    await app.drawer.getByLabel("EEPROM image URL").press("Tab");
    await expect(app.page.locator('[data-sub="eeprom"]')).toHaveText("mine.bin · 1 edited");
    await expect(app.drawer).toContainText("Settings");

    await app.drawer.getByRole("radio", { name: /An image from my computer/ }).check();
    await expect(app.drawer).toContainText("nothing chosen");
    await app.drawer.locator("input[type=file][accept='.bin']").setInputFiles(data("pubkey.bin"));
    await expect(app.page.locator('[data-sub="eeprom"]')).toHaveText("pubkey.… · 1 edited");

    await app.drawer.getByRole("radio", { name: /Official pieeprom/ }).check();
    await expect(app.page.locator('[data-sub="eeprom"]')).toHaveText("official · 1 edited");
  });

  test("an image that cannot be fetched says so", async ({ app }) => {
    await app.open("eeprom");
    await app.page.route("https://example.org/gone.bin", (route) => route.fulfill({ status: 404, body: "no" }));
    await app.drawer.getByRole("radio", { name: /An image from a URL/ }).check();
    await app.drawer.getByLabel("EEPROM image URL").fill("https://example.org/gone.bin");
    await app.drawer.getByLabel("EEPROM image URL").press("Tab");
    await expect(app.drawer).toContainText("Could not read the image");
    await expect(app.drawer).toContainText("answered 404");
  });
});

test.describe("USB", () => {
  test("a stick is an image, local or by URL, of a size", async ({ app }) => {
    await app.open("usb");
    await expect(app.drawer.getByRole("radio", { name: "Nothing plugged in" })).toBeChecked();
    await app.drawer.getByRole("radio", { name: /A USB stick/ }).check();
    await expect(app.part("usb")).toHaveAttribute("data-filled", "true");
    await app.drawer.locator("input[type=file]").setInputFiles(data("disk.img"));
    await expect(app.page.locator('[data-sub="usb"]')).toHaveText("disk.img");
    await app.drawer.getByRole("radio", { name: "URL" }).check();
    await app.drawer.getByLabel("a disk image URL").fill("https://example.org/stick.img");
    await expect(app.page.locator('[data-sub="usb"]')).toHaveText("stick.img");
    await app.field("Stick size").locator("input").fill("300");
    expect((await app.localState()).usb).toMatchObject({ kind: "url", url: "https://example.org/stick.img", mb: "300" });
    await app.drawer.getByRole("radio", { name: "Nothing plugged in" }).check();
    await expect(app.part("usb")).toHaveAttribute("data-filled", "false");
    await expect(app.page.locator('[data-sub="usb"]')).toHaveText("empty");
  });
});

test.describe("USB-C", () => {
  test("it has the power, and an OTG stick behind a dock", async ({ app }) => {
    await app.open("usbc");
    await expect(app.drawer.getByRole("button", { name: "Plug in the power" })).toBeVisible();
    await app.drawer.getByRole("radio", { name: "A USB stick" }).check();
    await app.drawer.getByRole("radio", { name: "URL" }).check();
    await app.drawer.getByLabel("a disk image URL").fill("https://example.org/otg.img");
    await app.drawer.getByRole("checkbox", { name: /Behind a USB-C dock/ }).check();
    await expect(app.page.locator('[data-sub="usbc"]')).toHaveText("otg.img · dock");
    await expect(app.part("usbc")).toHaveAttribute("data-filled", "true");
    expect((await app.localState()).otg).toMatchObject({ kind: "url", dock: true });
  });

  test("its button plugs the power in and out", async ({ app }) => {
    await app.open("usbc");
    await app.drawer.getByRole("button", { name: "Plug in the power" }).click();
    await app.page.waitForFunction(() => window.__machine() !== undefined);
    await app.emit({ type: "phase", phase: "running" });
    await expect(app.drawer.getByRole("button", { name: "Unplug the power" })).toBeVisible();
    await app.drawer.getByRole("button", { name: "Unplug the power" }).click();
    await expect(app.pill).toHaveText("Off");
  });
});

test.describe("Ethernet", () => {
  test("the cable goes into the peer, which boots the board over TFTP, HTTP or both", async ({ app }) => {
    await app.open("eth");
    await expect(app.drawer.getByRole("button", { name: "Send a magic packet" })).toHaveCount(0);
    await expect(app.drawer.getByRole("checkbox", { name: "Boot over TFTP" })).toHaveCount(0);
    await app.drawer.getByRole("checkbox", { name: "Plug the cable in" }).check();
    await expect(app.part("eth")).toHaveAttribute("data-filled", "true");
    await expect(app.page.locator('[data-sub="eth"]')).toHaveText("built-in peer");
    await expect(app.drawer.getByRole("button", { name: "Send a magic packet" })).toBeDisabled();

    await app.drawer.getByRole("checkbox", { name: "Boot over TFTP" }).check();
    await expect(app.page.locator('[data-sub="eth"]')).toHaveText("TFTP");
    await expect(app.drawer.getByRole("radio", { name: /Official firmware/ })).toBeChecked();
    await app.drawer.getByRole("checkbox", { name: "Boot over HTTP" }).check();
    await expect(app.page.locator('[data-sub="eth"]')).toHaveText("TFTP · HTTP");
    await expect(app.drawer).toContainText("HTTP boot is slow");

    await app.drawer.getByRole("checkbox", { name: "Boot over TFTP" }).uncheck();
    await expect(app.page.locator('[data-sub="eth"]')).toHaveText("HTTP");
    await app.drawer.getByRole("checkbox", { name: "Plug the cable in" }).uncheck();
    await expect(app.page.locator('[data-sub="eth"]')).toHaveText("unplugged");
  });

  test("each protocol takes its own files", async ({ app }) => {
    await app.open("eth");
    await app.drawer.getByRole("checkbox", { name: "Plug the cable in" }).check();
    await app.drawer.getByRole("checkbox", { name: "Boot over TFTP" }).check();
    await app.drawer.getByRole("radio", { name: /Boot files from my computer/ }).check();
    await expect(app.drawer).toContainText("nothing chosen");
    await app.drawer.locator("input[type=file]:not([webkitdirectory])").setInputFiles([data("boot/start4.elf"), data("boot/fixup4.dat")]);
    await expect(app.drawer).toContainText("2 files");
    await app.drawer.locator("input[type=file][webkitdirectory]").setInputFiles(data("boot"));
    await expect(app.drawer).toContainText("4 files");

    await app.drawer.getByRole("checkbox", { name: "Boot over HTTP" }).check();
    await app.drawer.getByRole("radio", { name: /Boot files from a GitHub directory/ }).nth(1).check();
    await app.drawer.getByLabel("http repository").fill("someone/firmware");
    await app.drawer.getByLabel("http branch or tag").fill("stable");
    await app.drawer.getByLabel("http directory").fill("boot");
    await expect(app.drawer.getByLabel("tftp repository")).toHaveCount(0);

    await app.powerOn();
    const request = await app.request();
    expect(request.net.tftp.kind).toBe("files");
    expect(request.net.tftp.files).toHaveLength(4);
    expect(request.net.http).toEqual({ kind: "download", github: { repo: "someone/firmware", ref: "stable", path: "boot" } });
    expect(request.options.network).toBe(true);
  });

  test("the network comes first in the boot order, and HTTP comes with a host", async ({ app }) => {
    await app.open("eth");
    await app.drawer.getByRole("checkbox", { name: "Plug the cable in" }).check();
    await app.drawer.getByRole("checkbox", { name: "Boot over TFTP" }).check();
    await app.drawer.getByRole("checkbox", { name: "Boot over HTTP" }).check();
    await app.powerOn();
    const request = await app.request();
    expect(request.options.bootOrder).toBe("0xf172");
    expect(request.options.bootconf).toEqual(expect.arrayContaining(["HTTP_HOST=netboot.test", "HTTP_PORT=80"]));
    expect(request.net.tftp.files.map((f) => f.path)).toEqual(["start4.elf", "fixup4.dat", "bcm2711-rpi-4-b.dtb", "kernel8.img"]);
    expect(request.net.http.files).toEqual(request.net.tftp.files);
  });

  test("a boot order or HTTP host of one's own is left alone", async ({ app }) => {
    await app.open("eeprom");
    await app.field("Other lines").locator("textarea").fill("HTTP_HOST=example.org");
    await app.open("eth");
    await app.drawer.getByRole("checkbox", { name: "Plug the cable in" }).check();
    await app.drawer.getByRole("checkbox", { name: "Boot over HTTP" }).check();
    await app.powerOn();
    const bootconf = (await app.request()).options.bootconf;
    expect(bootconf).toContain("HTTP_HOST=example.org");
    expect(bootconf).not.toContain("HTTP_HOST=netboot.test");
    expect(bootconf).toContain("HTTP_PORT=80");
  });

  test("without a card, nothing is on the slot and the network boots alone", async ({ app }) => {
    await app.open("sd");
    await app.drawer.getByRole("radio", { name: /No card/ }).check();
    await expect(app.page.locator('[data-sub="sd"]')).toHaveText("empty");
    await expect(app.part("sd")).toHaveAttribute("data-filled", "false");
    await expect(app.field("Kernel command line")).toHaveCount(0);
    await app.open("eth");
    await app.drawer.getByRole("checkbox", { name: "Plug the cable in" }).check();
    await app.drawer.getByRole("checkbox", { name: "Boot over TFTP" }).check();
    await app.open("sd");
    await expect(app.field("Kernel command line")).toBeVisible();
    await app.powerOn();
    const request = await app.request();
    expect(request.options.noSdCard).toBe(true);
    expect(request.options.bootOrder).toBe("0xf2");
    expect(request.boot).toEqual({ kind: "none" });
    expect(request.images).toEqual({});
  });
});

test.describe("HDMI 0 and GPIO", () => {
  test("a monitor has an EDID", async ({ app }) => {
    await app.open("hdmi");
    await expect(app.drawer.locator(".field", { hasText: "EDID" })).toHaveCount(0);
    await app.drawer.getByRole("checkbox", { name: /Plug in a monitor/ }).check();
    await expect(app.page.locator('[data-sub="hdmi"]')).toHaveText("monitor");
    await expect(app.part("hdmi")).toHaveAttribute("data-filled", "true");
    await expect(app.field("EDID")).toContainText("the built-in one");
    await app.field("EDID").locator("input[type=file]").setInputFiles(data("edid.bin"));
    await expect(app.field("EDID")).toContainText("edid.bin");
  });

  test("a HAT can be fitted and removed", async ({ app }) => {
    await app.open("gpio");
    await expect(app.drawer).toContainText("no HAT");
    await app.drawer.locator("input[type=file]").setInputFiles(data("hat.eep"));
    await expect(app.page.locator('[data-sub="gpio"]')).toHaveText("hat.eep");
    await expect(app.part("gpio")).toHaveAttribute("data-filled", "true");
    await app.drawer.getByRole("button", { name: "Remove the HAT" }).click();
    await expect(app.page.locator('[data-sub="gpio"]')).toHaveText("no HAT");
  });
});

test.describe("BCM2711", () => {
  test("the silicon and the memory", async ({ app }) => {
    await app.open("soc");
    await app.field("Silicon").locator("select").selectOption("b0");
    await expect(app.page.locator("#soc-title")).toHaveText("BCM2711 B0");
    await app.field("Memory").locator("select").selectOption("2048");
    await expect(app.page.locator('[data-sub="memory"]')).toHaveText("2 GiB");
    expect((await app.localState()).board).toMatchObject({ stepping: "b0", memoryMb: 2048 });
  });

  test("the advanced settings", async ({ app }) => {
    await app.open("soc");
    await app.drawer.locator("details summary").click();
    await app.field("Board revision code").locator("input").fill("a03114");
    await expect(app.page.locator('[data-sub="memory"]')).toHaveText("1 GiB · a03114");
    await app.field("OTP fuses").locator("textarea").fill("33=0\n17=0x3000000");
    await app.field("Jitter seed").locator("input").fill("7");
    await app.field("Faults").locator("input").fill("1000");
    await app.drawer.getByRole("checkbox", { name: /stale cache reads/ }).check();
    await app.drawer.getByRole("checkbox", { name: /misaligned VPU accesses/ }).check();
    await app.drawer.getByRole("checkbox", { name: /Step over instructions/ }).check();
    const saved = await app.localState();
    expect(saved.board.boardRev).toBe("a03114");
    expect(saved.otpRows).toBe("33=0\n17=0x3000000");
    expect(saved.advanced).toEqual({ jitter: "7", faults: "1000", checkCoherency: true, checkAlignment: true, skipUnimpl: true, parallel: false });
  });
});

test("the buttons open the file chooser", async ({ app }) => {
  await app.open("sd");
  await app.drawer.getByRole("radio", { name: /Boot files from my computer/ }).check();
  for (const [name, directory] of [["Choose a folder…", true], ["Choose files…", false]]) {
    const [chooser] = await Promise.all([app.page.waitForEvent("filechooser"), app.drawer.getByRole("button", { name }).click()]);
    expect(chooser.isMultiple()).toBe(!directory);
    expect(await chooser.element().evaluate((input) => input.hasAttribute("webkitdirectory"))).toBe(directory);
  }
});

test("the OTG stick can be taken out again", async ({ app }) => {
  await app.open("usbc");
  await app.drawer.getByRole("radio", { name: "A USB stick" }).check();
  await expect(app.part("usbc")).toHaveAttribute("data-filled", "true");
  await app.drawer.getByRole("radio", { name: "Nothing" }).check();
  await expect(app.part("usbc")).toHaveAttribute("data-filled", "false");
  await expect(app.page.locator('[data-sub="usbc"]')).toHaveText("empty");
});

test("what is chosen is still there after a reload", async ({ app }) => {
  await app.open("hdmi");
  await app.drawer.getByRole("checkbox", { name: /Plug in a monitor/ }).check();
  await app.open("soc");
  await app.field("Memory").locator("select").selectOption("2048");
  await app.page.waitForTimeout(300);
  await app.page.reload();
  await expect(app.page.locator('[data-sub="hdmi"]')).toHaveText("monitor");
  await expect(app.page.locator('[data-sub="memory"]')).toHaveText("2 GiB");
});

test("Reset all settings puts every setting back to its default", async ({ app }) => {
  await app.open("soc");
  await app.field("Memory").locator("select").selectOption("2048");
  expect(await app.localState()).toMatchObject({ board: { memoryMb: 2048 } });
  app.page.once("dialog", (dialog) => dialog.accept());
  await app.page.getByRole("button", { name: "Reset all settings" }).click();
  await expect(app.page.locator('[data-sub="memory"]')).toHaveText("1 GiB");
  expect(await app.page.evaluate(() => localStorage.getItem("pimu-web"))).toBeNull();
});

test("Reset all settings changes nothing when cancelled", async ({ app }) => {
  await app.open("soc");
  await app.field("Memory").locator("select").selectOption("2048");
  app.page.once("dialog", (dialog) => dialog.dismiss());
  await app.page.getByRole("button", { name: "Reset all settings" }).click();
  await expect(app.page.locator('[data-sub="memory"]')).toHaveText("2 GiB");
  expect(await app.localState()).toMatchObject({ board: { memoryMb: 2048 } });
});
