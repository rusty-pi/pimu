import { expect, test } from "./fixtures.mjs";

// The page must not jump: opening a part, switching a tab, starting a download
// or a run changes nothing about where the rest of the page is.
const geometry = (app) =>
  app.page.evaluate(() => ({
    document: document.documentElement.scrollHeight,
    board: Math.round(document.getElementById("board").getBoundingClientRect().height),
    console: Math.round(document.getElementById("term").getBoundingClientRect().top + scrollY),
    screen: Math.round(document.querySelector(".console-pane").getBoundingClientRect().height),
    pageOverflow: document.documentElement.scrollWidth > innerWidth,
  }));

const box = (app, selector) => app.page.locator(selector).evaluate((el) => {
  const r = el.getBoundingClientRect();
  return { left: Math.round(r.left), top: Math.round(r.top), right: Math.round(r.right), bottom: Math.round(r.bottom), height: Math.round(r.height) };
});

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

  test("the panel stands beside the board, as tall as it, whatever is in it", async ({ app }) => {
    for (const part of [null, "sd", "eeprom", "soc"]) {
      if (part) await app.open(part);
      const [board, drawer] = [await box(app, ".board-pane"), await box(app, "#drawer")];
      expect(drawer.left, String(part)).toBeGreaterThanOrEqual(board.right);
      expect(drawer.top, String(part)).toBe(board.top);
      expect(drawer.height, String(part)).toBe(board.height);
    }
  });

  test("the console and the log share the row under the board", async ({ app }) => {
    const [board, term, screen] = [await box(app, ".board-pane"), await box(app, ".console-pane"), await box(app, ".screen")];
    expect(term.top).toBeGreaterThan(board.top);
    expect(screen.top).toBe(term.top);
    expect(screen.left).toBeGreaterThanOrEqual(term.right);
  });

  test("a long panel scrolls inside itself", async ({ app }) => {
    await app.open("eeprom");
    await expect(app.drawer).toContainText("Settings");
    const body = app.drawer.locator(".drawer-body");
    expect(await body.evaluate((el) => el.scrollHeight > el.clientHeight)).toBe(true);
  });

  test("switching tabs moves nothing", async ({ app }) => {
    const before = await geometry(app);
    for (const tab of ["info", "log"]) {
      await app.showTab(tab);
      expect(await geometry(app), tab).toEqual(before);
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

test.describe("on a very wide screen", () => {
  test.use({ viewport: { width: 1920, height: 1000 } });

  test("the board, the panel and the log are three columns, and the page does not scroll", async ({ app }) => {
    const [board, term, drawer, screen] = [await box(app, ".board-pane"), await box(app, ".console-pane"), await box(app, "#drawer"), await box(app, ".screen")];
    expect(term.top).toBeGreaterThanOrEqual(board.bottom);
    expect(term.right).toBe(board.right);
    expect(drawer.left).toBeGreaterThanOrEqual(board.right);
    expect(screen.left).toBeGreaterThanOrEqual(drawer.right);
    expect(screen.height).toBe(drawer.height);
    const scrolls = await app.page.evaluate(() => ({ down: document.querySelector(".work").getBoundingClientRect().bottom > innerHeight, across: document.documentElement.scrollWidth > innerWidth }));
    expect(scrolls).toEqual({ down: false, across: false });
  });

  test("opening a part moves nothing", async ({ app }) => {
    const before = await geometry(app);
    for (const part of PARTS) {
      await app.open(part);
      expect(await geometry(app), part).toEqual(before);
    }
  });
});

test("resizing across the breakpoint turns the column into a sheet and back", async ({ app }) => {
  await app.open("sd");
  await expect(app.drawer).toHaveCSS("position", "relative");
  await app.page.setViewportSize({ width: 900, height: 800 });
  await expect(app.drawer).toHaveCSS("position", "fixed");
  await expect(app.drawer).toHaveAttribute("data-sheet", "open");
  await app.part("sd").click();
  await expect(app.drawer).toHaveAttribute("data-sheet", "closed");
  await app.page.setViewportSize({ width: 1280, height: 900 });
  await expect(app.drawer).toHaveCSS("position", "relative");
  await expect(app.drawer.locator(".changes-pane")).toBeVisible();
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

  test("the board stands upright and fits, with the parts in two columns under the SoC", async ({ app }) => {
    await expect(app.page.locator("#board")).toHaveAttribute("viewBox", /^0 12 400 /);
    const rect = (sel) => app.page.locator(sel).first().evaluate((el) => el.getBoundingClientRect().toJSON());
    const board = await rect("#board");
    expect(board.right).toBeLessThanOrEqual(375);
    const soc = await rect("#board .soc");
    const columns = new Set();
    for (const part of ["sd", "eeprom", "usb", "usbc", "eth", "hdmi", "gpio"]) {
      const r = await rect(`#board .part[data-part="${part}"] .box`);
      expect(r.top, part).toBeGreaterThan(soc.bottom);
      expect(r.right, part).toBeLessThanOrEqual(board.right + 1);
      columns.add(Math.round(r.left));
    }
    expect(columns.size).toBe(2);
    const core = await rect('#board [data-core="arm0"]');
    expect(core.width).toBeGreaterThan(40);
    expect((await geometry(app)).pageOverflow).toBe(false);
  });

  test("the sheet shows its handle, and opens and closes from it", async ({ app }) => {
    const handle = app.drawer.locator(".sheet-handle");
    await expect(handle).toBeInViewport();
    await expect(app.drawer).toHaveAttribute("data-sheet", "closed");
    await handle.click();
    await expect(app.drawer).toHaveAttribute("data-sheet", "open");
    await expect(app.drawer.locator(".changes-pane")).toBeInViewport();
    await handle.click();
    await expect(app.drawer).toHaveAttribute("data-sheet", "closed");
  });

  test("a part opens the sheet, and the same part closes it", async ({ app }) => {
    await app.open("sd");
    await expect(app.drawer).toHaveAttribute("data-sheet", "open");
    await expect(app.drawer.getByRole("heading", { name: "microSD card" })).toBeInViewport();
    await app.part("sd").click();
    await expect(app.drawer).toHaveAttribute("data-sheet", "closed");
  });
});
