import { expect, test } from "./fixtures.mjs";

// The page must not jump: opening a part, switching a tab, starting a download
// or a run changes nothing about where the rest of the page is.
const geometry = (app) =>
  app.page.evaluate(() => ({
    document: document.documentElement.scrollHeight,
    board: Math.round(document.getElementById("board").getBoundingClientRect().height),
    console: Math.round(document.getElementById("term").getBoundingClientRect().top + scrollY),
    screen: Math.round(document.querySelector(".screen").getBoundingClientRect().height),
    pageOverflow: document.documentElement.scrollWidth > innerWidth,
  }));

const PARTS = ["sd", "eeprom", "usb", "usbc", "eth", "hdmi", "gpio", "soc"];

test.describe("on a wide screen", () => {
  test("opening any part moves nothing", async ({ app }) => {
    const before = await geometry(app);
    for (const part of PARTS) {
      await app.open(part);
      await app.page.waitForTimeout(150);
      expect(await geometry(app), part).toEqual(before);
    }
  });

  test("the panel is a column as tall as the console, whatever is in it", async ({ app }) => {
    const heights = [];
    for (const part of [null, "sd", "eeprom", "soc"]) {
      if (part) await app.open(part);
      heights.push(await app.page.evaluate(() => [document.querySelector(".screen").offsetHeight, document.getElementById("drawer").offsetHeight]));
    }
    for (const [screen, drawer] of heights) expect(drawer).toBe(screen);
  });

  test("a long panel scrolls inside itself", async ({ app }) => {
    await app.open("eeprom");
    await expect(app.drawer).toContainText("Settings");
    const body = app.drawer.locator(".drawer-body");
    expect(await body.evaluate((el) => el.scrollHeight > el.clientHeight)).toBe(true);
  });

  test("switching tabs moves nothing", async ({ app }) => {
    const before = await geometry(app);
    for (const tab of ["log", "info", "console"]) {
      await app.showTab(tab);
      const now = await geometry(app);
      expect({ ...now, console: before.console }).toEqual({ ...before, console: before.console });
    }
  });

  test("a download, a run and the clock moving move nothing", async ({ app }) => {
    const before = await geometry(app);
    await app.powerOn();
    const seen = [];
    const snap = async () => seen.push(await geometry(app));
    await app.emit({ type: "phase", phase: "downloading" });
    await app.emit({ type: "progress", transfers: [1, 2, 3, 4, 5, 6].map((i) => ({ name: `file${i}`, url: "u", loaded: i, total: 10, done: false })) });
    await snap();
    await app.emit({ type: "phase", phase: "running" });
    await app.emit({
      type: "state",
      io: { sd: { read: 1 << 20, requests: 1 } },
      state: {
        ended: false, ok: null, why: null, reboots: 1, armReleased: true, modelUs: 99_000_000, retired: 5_000_000_000, logDropped: 0,
        cores: { arm: Array(4).fill({ state: "running", insns: 7_000_000_000 }), vpu: Array(2).fill({ state: "running", insns: 1 }) },
      },
    });
    await snap();
    await app.emit({
      type: "state",
      io: {},
      state: {
        ended: true, ok: false, why: "a rather long reason for the run to have stopped so that the result line is long", reboots: 0, armReleased: true, modelUs: 1, retired: 1, logDropped: 0,
        cores: { arm: Array(4).fill({ state: "off", insns: 0 }), vpu: Array(2).fill({ state: "off", insns: 0 }) },
      },
    });
    await snap();
    for (const g of seen) expect(g).toEqual(before);
  });
});

test("resizing across the breakpoint turns the column into a sheet and back", async ({ app }) => {
  await app.open("sd");
  await expect(app.drawer).toHaveCSS("position", "relative");
  await app.page.setViewportSize({ width: 600, height: 800 });
  await expect(app.drawer).toHaveCSS("position", "fixed");
  await app.part("sd").click();
  await expect(app.drawer).toBeHidden();
  await app.page.setViewportSize({ width: 1280, height: 900 });
  await expect(app.drawer).toBeVisible();
  await expect(app.drawer).toContainText("Plug something in");
});

test.describe("on a phone", () => {
  test.use({ viewport: { width: 375, height: 812 } });

  test("the panel is a sheet over the page, so opening a part moves nothing", async ({ app }) => {
    const before = await geometry(app);
    expect(before.pageOverflow).toBe(false);
    for (const part of PARTS) {
      await app.open(part);
      await expect(app.drawer).toHaveCSS("position", "fixed");
      expect(await geometry(app), part).toEqual(before);
    }
  });

  test("the board scrolls sideways inside its panel instead of shrinking to nothing", async ({ app }) => {
    const board = await app.page.locator("#board").evaluate((svg) => svg.getBoundingClientRect().width);
    expect(board).toBeGreaterThanOrEqual(720);
    const stage = await app.page.locator(".stage").evaluate((el) => ({ scrolls: el.scrollWidth > el.clientWidth, overflow: getComputedStyle(el).overflowX }));
    expect(stage).toEqual({ scrolls: true, overflow: "auto" });
    expect((await geometry(app)).pageOverflow).toBe(false);
  });

  test("the panel can be closed", async ({ app }) => {
    await app.open("sd");
    await expect(app.drawer).toBeVisible();
    await app.drawer.getByRole("button", { name: "Close" }).click();
    await expect(app.drawer).toBeHidden();
  });
});
