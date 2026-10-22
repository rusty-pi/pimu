import { data } from "./data.mjs";
import { EEPROM_URL, expect, test } from "./fixtures.mjs";


test.describe("powering on", () => {
  test("the default board asks for the official firmware, a 1 GiB C0 and the io log", async ({ app }) => {
    await app.powerOn();
    const request = await app.request();
    expect(request.eeprom).toEqual({ __bytes: 4096, values: undefined });
    expect(request.images).toEqual({});
    expect(request.boot.kind).toBe("download");
    expect(request.boot.files.map((f) => f.path)).toEqual([
      "start4.elf", "fixup4.dat", "bcm2711-rpi-4-b.dtb", "kernel8.img",
    ]);
    expect(request.boot.files[0].url).toBe("https://raw.githubusercontent.com/raspberrypi/firmware/refs/heads/master/boot/start4.elf");
    expect(request.options).toEqual({
      stepping: "c0",
      memoryMb: 1024,
      bootconf: ["BOOT_UART=1"],
      skipSignedBoot: false,
      tryboot: false,
      otpRows: [],
      log: "io",
      display: false,
      network: false,
      checkCoherency: false,
      checkAlignment: false,
      skipUnimpl: false,
      parallel: false,
      configTxt: ["arm_64bit=1", "enable_uart=1", "uart_2ndstage=1"],
      cmdline: "console=ttyAMA0,115200",
    });
  });

  test("every setting reaches the request", async ({ app }) => {
    await app.open("sd");
    await app.drawer.getByRole("radio", { name: /Boot files from my computer/ }).check();
    await app.drawer.locator("input[type=file][webkitdirectory]").setInputFiles(data("boot"));
    await app.drawer.locator("textarea").fill("arm_64bit=1\n\ngpu_mem=16\n");
    await app.field("Kernel command line").locator("input").fill("console=tty1");

    await app.open("eeprom");
    await (await app.setting("BOOT_UART", "Serial and power")).locator("select").selectOption("1");
    await (await app.setting("HDMI_DELAY", "Display")).locator("input").fill("9");
    await app.drawer.locator(".field", { hasText: "Boot order" }).getByLabel("Boot order preset").selectOption({ label: "USB, then SD card, then restart" });
    await app.drawer.getByRole("checkbox", { name: /Skip the signature check/ }).check();
    await app.drawer.getByRole("checkbox", { name: /Tryboot/ }).check();
    await app.field("Other lines").locator("textarea").fill("HTTP_HOST=example.org");
    await app.field("Public key").locator("input[type=file]").setInputFiles(data("pubkey.bin"));

    await app.open("usb");
    await app.drawer.getByRole("radio", { name: /A USB stick/ }).check();
    await app.drawer.locator("input[type=file]").setInputFiles(data("disk.img"));
    await app.field("Stick size").locator("input").fill("300");

    await app.open("usbc");
    await app.drawer.getByRole("radio", { name: "A USB stick" }).check();
    await app.drawer.getByRole("radio", { name: "URL" }).check();
    await app.drawer.getByLabel("a disk image URL").fill("https://example.org/otg.img");
    await app.drawer.getByRole("checkbox", { name: /Behind a USB-C dock/ }).check();

    await app.open("eth");
    await app.drawer.getByRole("checkbox", { name: "Plug the cable in" }).check();
    await app.drawer.getByRole("checkbox", { name: "Boot over TFTP" }).check();
    await app.drawer.getByRole("radio", { name: /Boot files from my computer/ }).check();
    await app.drawer.locator("input[type=file]:not([webkitdirectory])").setInputFiles([data("boot/start4.elf")]);

    await app.open("hdmi");
    await app.drawer.getByRole("checkbox", { name: /Plug in a monitor/ }).check();
    await app.field("EDID").locator("input[type=file]").setInputFiles(data("edid.bin"));

    await app.open("gpio");
    await app.drawer.locator("input[type=file]").setInputFiles(data("hat.eep"));

    await app.open("soc");
    await app.field("Silicon").locator("select").selectOption("b0");
    await app.field("Memory").locator("select").selectOption("2048");
    await app.drawer.locator("details summary").click();
    await app.field("OTP fuses").locator("textarea").fill("33=0\n17=0x3000000");
    await app.field("Jitter seed").locator("input").fill("7");
    await app.field("Faults").locator("input").fill("1000");
    await app.drawer.getByRole("checkbox", { name: /stale cache reads/ }).check();
    await app.drawer.getByRole("checkbox", { name: /misaligned VPU accesses/ }).check();
    await app.drawer.getByRole("checkbox", { name: /Step over instructions/ }).check();

    await app.powerOn();
    const request = await app.request();
    expect(request.options).toEqual({
      stepping: "b0",
      memoryMb: 2048,
      bootconf: ["BOOT_UART=1", "HDMI_DELAY=9", "HTTP_HOST=example.org"],
      bootOrder: "0xf14",
      skipSignedBoot: true,
      tryboot: true,
      otpRows: [[33, 0], [17, 0x3000000]],
      log: "io",
      display: true,
      network: true,
      usbMb: 300,
      otgDock: true,
      jitter: 7,
      faults: 1000,
      checkCoherency: true,
      checkAlignment: true,
      skipUnimpl: true,
      parallel: false,
      configTxt: ["arm_64bit=1", "gpu_mem=16"],
      cmdline: "console=tty1",
    });
    expect(request.boot.kind).toBe("files");
    expect([...request.boot.files].sort((a, b) => a.path.localeCompare(b.path))).toEqual([
      { path: "config.txt", file: { __file: "config.txt", size: 12 } },
      { path: "fixup4.dat", file: { __file: "fixup4.dat", size: 3 } },
      { path: "overlays/disable-bt.dtbo", file: { __file: "disable-bt.dtbo", size: 4 } },
      { path: "start4.elf", file: { __file: "start4.elf", size: 3 } },
    ]);
    expect(request.images).toEqual({
      usb: { file: { __file: "disk.img", size: 4096 } },
      otg: { url: "https://example.org/otg.img" },
    });
    expect(request.pubkey).toEqual({ __file: "pubkey.bin", size: 264 });
    expect(request.hat).toEqual({ __file: "hat.eep", size: 256 });
    expect(request.edid).toEqual({ __file: "edid.bin", size: 128 });
    expect(request.net).toEqual({ tftp: { kind: "files", files: [{ path: "start4.elf", file: { __file: "start4.elf", size: 3 } }] } });
  });

  test("a card that is an image asks for the image and no config.txt", async ({ app }) => {
    await app.open("sd");
    await app.drawer.getByRole("radio", { name: /A disk image/ }).check();
    await app.drawer.getByRole("radio", { name: "URL" }).check();
    await app.drawer.getByLabel("a card image URL").fill(" https://example.org/card.img ");
    await app.powerOn();
    const request = await app.request();
    expect(request.images).toEqual({ sd: { url: "https://example.org/card.img" } });
    expect(request.boot).toEqual({ kind: "none" });
    expect(request.options).not.toHaveProperty("configTxt");
    expect(request.options).not.toHaveProperty("cmdline");
  });

  test("a GitHub directory is listed by the worker", async ({ app }) => {
    await app.open("sd");
    await app.drawer.getByRole("radio", { name: /GitHub directory/ }).check();
    await app.powerOn();
    expect((await app.request()).boot).toEqual({ kind: "download", github: { repo: "raspberrypi/firmware", ref: "master", path: "boot" } });
  });

  test("a board revision code replaces the memory", async ({ app }) => {
    await app.open("soc");
    await app.drawer.locator("details summary").click();
    await app.field("Board revision code").locator("input").fill(" a03114 ");
    await app.powerOn();
    const { options } = await app.request();
    expect(options.boardRev).toBe("a03114");
    expect(options).not.toHaveProperty("memoryMb");
  });
});

test.describe("what stops a power-on", () => {
  const problems = [
    ["boot files with none picked", async (app) => {
      await app.open("sd");
      await app.drawer.getByRole("radio", { name: /Boot files from my computer/ }).check();
    }, "SD card: no boot files picked"],
    ["an image with no URL", async (app) => {
      await app.open("sd");
      await app.drawer.getByRole("radio", { name: /A disk image/ }).check();
      await app.drawer.getByRole("radio", { name: "URL" }).check();
    }, "SD card image: no URL given"],
    ["an image with no file", async (app) => {
      await app.open("sd");
      await app.drawer.getByRole("radio", { name: /A disk image/ }).check();
      await app.drawer.getByRole("radio", { name: "Local file" }).check();
    }, "SD card image: no file picked"],
    ["a USB stick with no file", async (app) => {
      await app.open("usb");
      await app.drawer.getByRole("radio", { name: /A USB stick/ }).check();
    }, "USB stick: no file picked"],
    ["an OTG stick with no URL", async (app) => {
      await app.open("usbc");
      await app.drawer.getByRole("radio", { name: "A USB stick" }).check();
      await app.drawer.getByRole("radio", { name: "URL" }).check();
    }, "USB-C stick: no URL given"],
    ["a fuse that is not ROW=VALUE", async (app) => {
      await app.open("soc");
      await app.drawer.locator("details summary").click();
      await app.field("OTP fuses").locator("textarea").fill("seventeen");
    }, 'OTP row "seventeen": expected ROW=VALUE'],
  ];
  for (const [name, setup, message] of problems) {
    test(name, async ({ app }) => {
      await setup(app);
      await app.powerButton.click();
      await expect(app.page.locator("#toast")).toHaveText(message);
      await expect(app.page.locator("#toast")).toHaveClass(/bad/);
      await expect(app.status).toHaveText("Off");
      await expect(app.page.locator("#downloads")).toBeHidden();
      expect(await app.workers()).toEqual([expect.objectContaining({ url: expect.stringContaining("worker.mjs") })]);
    });
  }

  test("an EEPROM image that cannot be fetched", async ({ app }) => {
    await app.page.route(EEPROM_URL, (route) => route.fulfill({ status: 503, body: "later" }));
    await app.powerButton.click();
    await expect(app.page.locator("#toast")).toContainText("answered 503");
    await expect(app.status).toHaveText("Off");
  });
});

test.describe("while the board is powered", () => {
  test("the phases show in the status and the buttons follow", async ({ app }) => {
    await expect(app.page.locator("#pause")).toHaveCSS("visibility", "hidden");
    await app.powerOn();
    await expect(app.powerButton).toHaveAttribute("aria-checked", "true");
    await app.emit({ type: "phase", phase: "downloading" });
    await expect(app.status).toHaveText("Downloading");
    await app.emit({ type: "phase", phase: "starting" });
    await expect(app.status).toHaveText("Starting");
    await expect(app.page.locator("#pause")).toHaveCSS("visibility", "hidden");
    await app.emit({ type: "phase", phase: "running" });
    await expect(app.status).toHaveText("Running");
    await expect(app.page.locator("#pause")).toHaveCSS("visibility", "visible");

    await app.page.locator("#pause").click();
    expect((await app.sent()).at(-1)).toEqual({ type: "pause" });
    await app.emit({ type: "phase", phase: "paused" });
    await expect(app.status).toHaveText("Paused");
    await expect(app.page.locator("#pause")).toHaveText("Resume");
    await app.page.locator("#pause").click();
    expect((await app.sent()).at(-1)).toEqual({ type: "resume" });
    await app.emit({ type: "phase", phase: "running" });
    await expect(app.page.locator("#pause")).toHaveText("Pause");

    await app.emit({ type: "phase", phase: "ended" });
    await expect(app.status).toHaveText("Stopped");
    await expect(app.page.locator("#pause")).toHaveCSS("visibility", "hidden");
  });

  test("the power switch, its label and the LED follow the power", async ({ app }) => {
    const plug = app.page.locator("#power-plug");
    await expect(plug).toHaveAttribute("data-on", "false");
    await expect(app.page.locator("#power-label")).toHaveText("click to plug in");
    await expect(app.page.locator("#led-power")).toHaveAttribute("data-on", "false");
    await app.running();
    await expect(plug).toHaveAttribute("data-on", "true");
    await expect(app.page.locator("#power-label")).toHaveText("click to unplug");
    await expect(app.page.locator("#led-power")).toHaveAttribute("data-on", "true");
  });

  test("the switch on the board powers it on and off, by mouse and by keyboard", async ({ app }) => {
    await app.page.locator("#power-plug").click();
    await app.page.waitForFunction(() => window.__machine() !== undefined);
    await expect(app.status).not.toHaveText("Off");
    await app.page.locator("#power-plug").focus();
    await app.page.keyboard.press("Enter");
    await expect(app.status).toHaveText("Off");
    await app.page.keyboard.press("Space");
    await expect(app.status).not.toHaveText("Off");
  });

  test("powering off ends the worker and clears the cores and the clock", async ({ app }) => {
    await app.running();
    await app.emit({ type: "state", io: {}, state: stateOf({ arm: ["running", "sleeping", "off", "off"] }) });
    await expect(app.page.locator("#clock")).not.toBeEmpty();
    await app.powerButton.click();
    await expect(app.status).toHaveText("Off");
    const [, machine] = [null, ...(await app.workers()).filter((w) => w.sent.some((m) => m.type === "power-on"))];
    expect(machine.terminated).toBe(true);
    await expect(app.page.locator("#clock")).toBeEmpty();
    await expect(app.page.locator('[data-core="arm0"]')).toHaveAttribute("data-state", "off");
    await expect(app.page.locator("#led-act")).toHaveAttribute("data-on", "false");
  });

  test("the parts are locked, but still show what is in them", async ({ app }) => {
    await app.open("sd");
    await app.running();
    for (const part of ["sd", "eeprom", "usb", "usbc", "eth", "hdmi", "gpio"]) {
      await expect(app.part(part)).toHaveAttribute("data-locked", "true");
    }
    await expect(app.part("soc")).toHaveAttribute("data-locked", "false");
    await expect(app.drawer).toContainText("Locked – power off to change");
    await expect(app.drawer.locator("fieldset")).toHaveJSProperty("disabled", true);
    await expect(app.drawer.locator("fieldset input").first()).toBeDisabled();
    await app.powerButton.click();
    await expect(app.drawer.locator("fieldset")).toHaveJSProperty("disabled", false);
    await expect(app.part("sd")).toHaveAttribute("data-locked", "false");
  });

  test("the empty panel and the changes say what is true of a powered board", async ({ app }) => {
    await expect(app.drawer).toContainText("Click a part of the board to change what is plugged into it.");
    await app.running();
    await expect(app.drawer).toContainText("Click a part of the board to see what is plugged into it.");
    await expect(app.changes).toContainText("The board is powered, so its parts are locked");
    await app.powerButton.click();
    await expect(app.drawer).toContainText("Click a part of the board to change what is plugged into it.");
    await expect(app.changes).not.toContainText("The board is powered");
  });

  test("the Power button in the USB-C panel works while the rest is locked", async ({ app }) => {
    await app.open("usbc");
    await app.running();
    await expect(app.drawer.getByRole("radio", { name: "A USB stick" })).toBeDisabled();
    await expect(app.drawer.getByRole("button", { name: "Unplug the power" })).toBeEnabled();
  });

  test("a worker that reports an error ends the run", async ({ app }) => {
    await app.running();
    await app.emit({ type: "error", message: "unreachable executed" });
    await expect(app.status).toHaveText("Error");
    await expect(app.page.locator("#toast")).toHaveText("unreachable executed");
    await expect(app.powerButton).toHaveAttribute("aria-checked", "false");
    await app.powerOn();
    expect((await app.workers()).filter((w) => w.sent.some((m) => m.type === "power-on"))).toHaveLength(2);
  });

  test("a worker that dies is reported", async ({ app }) => {
    await app.running();
    await app.page.evaluate(() => window.__machine().onerror({ message: "out of memory" }));
    await expect(app.status).toHaveText("Error");
    await expect(app.page.locator("#toast")).toHaveText("out of memory");
    await app.powerOn();
    await app.page.evaluate(() => window.__machine().onerror({ message: "" }));
    await expect(app.page.locator("#toast")).toHaveText("the worker stopped");
  });
});

test.describe("what the machine reports", () => {
  test("the cores light up and say what they are doing", async ({ app }) => {
    await app.running();
    await app.emit({ type: "state", io: {}, state: stateOf({ arm: ["running", "sleeping", "off", "running"], vpu: ["running", "off"] }) });
    const state = (core) => app.page.locator(`[data-core="${core}"]`);
    const word = (core) => app.page.locator(`[data-core-state="${core}"]`);
    await expect(state("arm0")).toHaveAttribute("data-state", "running");
    await expect(word("arm0")).toHaveText("running");
    await expect(state("arm1")).toHaveAttribute("data-state", "sleeping");
    await expect(word("arm1")).toHaveText("asleep");
    await expect(word("arm2")).toHaveText("off");
    await expect(state("vpu0")).toHaveAttribute("data-state", "running");
    await expect(state("vpu1")).toHaveAttribute("data-state", "off");
    await expect(app.page.locator("#led-act")).toHaveAttribute("data-on", "true");
  });

  test("the clock counts VPU and ARM instructions and the disk it has read", async ({ app }) => {
    await app.page.clock.install({ time: 0 });
    await app.page.clock.pauseAt(1000);
    await app.running();
    await app.page.clock.runFor(2500);
    await app.emit({
      type: "state",
      io: { sd: { read: 17 << 20, requests: 17 }, usb: { read: 0, requests: 0 } },
      state: stateOf({ arm: ["running", "running", "sleeping", "running"], retired: 845_000_000, insns: 480_000_000, modelUs: 16_300_000 }),
    });
    await expect(app.page.locator("#clock")).toHaveText("model time 16.3 s · real 2.5 s · VPU 845 M instructions · ARM 480 M · sd: 17.0 MiB read");
  });

  test("the real time moves on while the worker is too busy to send its state", async ({ app }) => {
    await app.page.clock.install({ time: 0 });
    await app.page.clock.pauseAt(1000);
    await app.running();
    await app.emit({ type: "state", io: {}, state: stateOf({ modelUs: 1_000_000 }) });
    await app.page.clock.runFor(2300);
    await expect(app.page.locator("#clock")).toContainText("model time 1.0 s · real 2.3 s ·");
  });

  test("the real time stands still while paused and after the run ends", async ({ app }) => {
    await app.page.clock.install({ time: 0 });
    await app.page.clock.pauseAt(1000);
    await app.running();
    await app.page.clock.runFor(3000);
    await app.emit({ type: "phase", phase: "paused" });
    await app.page.clock.runFor(5000);
    await app.emit({ type: "phase", phase: "running" });
    await app.page.clock.runFor(1000);
    await app.emit({ type: "state", io: {}, state: stateOf({ ended: true, ok: true, why: "done" }) });
    await app.page.clock.runFor(5000);
    await app.emit({ type: "state", io: {}, state: stateOf({ ended: true, ok: true, why: "done" }) });
    await expect(app.page.locator("#clock")).toContainText("· real 4.0 s ·");
  });

  test("the result says how the run ended, and how many resets it had", async ({ app }) => {
    await app.running();
    await app.emit({ type: "state", io: {}, state: stateOf({ reboots: 2 }) });
    await expect(app.page.locator("#result")).toHaveText("2 resets");
    await app.emit({ type: "state", io: {}, state: stateOf({ reboots: 1 }) });
    await expect(app.page.locator("#result")).toHaveText("1 reset");
    await app.emit({ type: "state", io: {}, state: stateOf({ ended: true, ok: true, why: "the firmware started the ARM" }) });
    await expect(app.page.locator("#result")).toHaveText("ok – the firmware started the ARM");
    await expect(app.page.locator("#result")).toHaveAttribute("data-ok", "true");
    await app.emit({ type: "state", io: {}, state: stateOf({ ended: true, ok: false, why: "the VPU faulted" }) });
    await expect(app.page.locator("#result")).toHaveText("failed – the VPU faulted");
    await expect(app.page.locator("#result")).toHaveAttribute("data-ok", "false");
  });

  test("the Wake-on-LAN button works while the board runs with the peer", async ({ app }) => {
    await app.open("eth");
    await app.drawer.getByRole("checkbox", { name: "Plug the cable in" }).check();
    await app.powerOn();
    await app.emit({ type: "phase", phase: "running" });
    const send = app.drawer.getByRole("button", { name: "Send a magic packet" });
    await expect(send).toBeEnabled();
    await send.click();
    expect((await app.sent()).at(-1)).toEqual({ type: "wake-on-lan" });
    await app.emit({ type: "wake-on-lan", sent: true });
    await expect(app.page.locator("#toast")).toHaveText("Magic packet sent");
    await app.emit({ type: "wake-on-lan", sent: false });
    await expect(app.page.locator("#toast")).toHaveText("No network peer to send it");
    await expect(app.page.locator("#toast")).toHaveClass(/bad/);
  });
});

test.describe("messages", () => {
  test("a message goes away by itself, sooner than an error", async ({ app }) => {
    await app.page.clock.install();
    await app.open("sd");
    await app.drawer.getByRole("radio", { name: /Boot files from my computer/ }).check();
    await app.powerButton.click();
    await expect(app.page.locator("#toast")).toBeVisible();
    await app.page.clock.runFor(7000);
    await expect(app.page.locator("#toast")).toBeVisible();
    await app.page.clock.runFor(1500);
    await expect(app.page.locator("#toast")).toBeHidden();
  });

  test("a good message goes sooner", async ({ app }) => {
    await app.open("eth");
    await app.drawer.getByRole("checkbox", { name: "Plug the cable in" }).check();
    await app.powerOn();
    await app.emit({ type: "phase", phase: "running" });
    await app.page.clock.install();
    await app.emit({ type: "wake-on-lan", sent: true });
    await expect(app.page.locator("#toast")).toBeVisible();
    await app.page.clock.runFor(3600);
    await expect(app.page.locator("#toast")).toBeHidden();
  });
});

test.describe("downloads", () => {
  test("the EEPROM image shows its progress while the request is made", async ({ app }) => {
    let release;
    const gate = new Promise((resolve) => (release = resolve));
    await app.page.route(EEPROM_URL, async (route) => {
      await gate;
      await route.fulfill({ status: 200, headers: { "content-length": "4096" }, body: Buffer.alloc(4096) });
    });
    await app.powerButton.click();
    const panel = app.page.locator("#downloads");
    await expect(panel).toBeVisible();
    await expect(panel).toContainText("Downloading firmware");
    await expect(panel.locator(".dl .name")).toHaveText("pieeprom.bin");
    await expect(panel.locator(".bar")).toHaveClass(/indeterminate/);
    expect(await app.page.evaluate(() => window.__machine())).toBeUndefined();
    release();
    await app.page.waitForFunction(() => window.__machine() !== undefined);
    await expect(panel.locator(".size")).toHaveText("4 KiB ✓");
  });

  test("the worker's downloads are bars with names and sizes, and a total", async ({ app }) => {
    await app.powerOn();
    await app.emit({ type: "phase", phase: "downloading" });
    const MiB = 1 << 20;
    await app.emit({
      type: "progress",
      transfers: [
        { name: "start4.elf", url: "https://x/start4.elf", loaded: 2.2 * MiB, total: 2.2 * MiB, done: true },
        { name: "kernel8.img", url: "https://x/kernel8.img", loaded: 5 * MiB, total: 10 * MiB, done: false },
        { name: "fixup4.dat", url: "https://x/fixup4.dat", loaded: 100, total: 0, done: false },
        { name: "config.txt", url: null, loaded: 63, total: 63, done: true, local: true },
      ],
    });
    const panel = app.page.locator("#downloads");
    await expect(panel.locator(".dl:not(.total) .name")).toHaveText(["pieeprom.bin", "start4.elf", "kernel8.img", "fixup4.dat", "config.txt"]);
    await expect(panel.locator(".dl:not(.total) .size")).toHaveText(["4 KiB ✓", "2.2 MiB ✓", "5.0 MiB / 10.0 MiB", "1 KiB", "1 KiB (read from disk)"]);
    const widths = await panel.locator(".dl:not(.total) .bar i").evaluateAll((bars) => bars.map((b) => b.style.width));
    expect(widths).toEqual(["100%", "100%", "50%", "0%", "100%"]);
    await expect(panel.locator(".dl:not(.total) .bar").nth(3)).toHaveClass(/indeterminate/);
    await expect(panel.locator(".dl.total")).toContainText("3 of 5 files");
  });

  test("the panel goes away once the board runs", async ({ app }) => {
    await app.powerOn();
    await app.emit({ type: "progress", transfers: [{ name: "a", url: "u", loaded: 1, total: 1, done: true }] });
    await expect(app.page.locator("#downloads")).toBeVisible();
    await app.emit({ type: "phase", phase: "running" });
    await expect(app.page.locator("#downloads")).toBeHidden({ timeout: 3000 });
  });

  test("the EEPROM image downloaded to read its settings is shown too, and then goes", async ({ app }) => {
    await app.open("eeprom");
    await expect(app.page.locator("#downloads .name")).toHaveText("pieeprom.bin");
    await expect(app.page.locator("#downloads")).toBeHidden({ timeout: 4000 });
  });
});

function stateOf({ arm = ["off", "off", "off", "off"], vpu = ["running", "off"], retired = 1_000_000, insns = 0, modelUs = 1_000_000, reboots = 0, ended = false, ok = null, why = null } = {}) {
  const per = Math.round(insns / 4);
  return {
    ended,
    ok,
    why,
    reboots,
    armReleased: arm.some((s) => s !== "off"),
    modelUs,
    retired,
    logDropped: 0,
    cores: {
      arm: arm.map((state) => ({ state, insns: state === "off" ? 0 : per })),
      vpu: vpu.map((state) => ({ state, insns: state === "off" ? 0 : retired })),
    },
  };
}

test.describe("a snapshot at the ARM's release", () => {
  test("is asked for once, kept, and the next boot starts from it", async ({ app }) => {
    await app.open("soc");
    await app.drawer.locator("details summary").click();
    await app.drawer.getByRole("checkbox", { name: /Skip the firmware's boot with a snapshot/ }).check();
    await app.powerOn();
    const first = await app.request();
    expect(first.options.snapshot).toBe(true);
    expect(first.snapshot).toBeUndefined();
    await app.page.evaluate((key) => window.__machine().emit({ type: "snapshot", key, bytes: new Uint8Array([1, 2, 3]) }), first.snapshotKey);
    await expect(app.page.locator("#toast")).toHaveText(/^Snapshot kept/);
    await app.powerButton.click();
    await app.powerOn();
    const workers = await app.workers();
    const second = workers.at(-1).sent.find((m) => m.type === "power-on");
    expect(second.snapshotKey).toBe(first.snapshotKey);
    expect(second.snapshot).toBeDefined();
    expect(second.options.snapshot).toBeUndefined();
  });
});
