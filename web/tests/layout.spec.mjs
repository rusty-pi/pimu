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

const panes = async (app) => ({
  changes: await box(app, "#changes"),
  board: await box(app, ".board-pane"),
  term: await box(app, ".console-pane"),
  part: await box(app, "#drawer"),
  log: await box(app, ".screen"),
});

const PARTS = ["sd", "eeprom", "usb", "usbc", "eth", "hdmi", "gpio", "soc"];

for (const viewport of [{ width: 1280, height: 800 }, { width: 1920, height: 1080 }]) {
  test.describe(`on a ${viewport.width} px screen`, () => {
    test.use({ viewport });

    test("the changes, the board and the open part are three columns, and the page does not scroll", async ({ app }) => {
      const { changes, board, term, part, log } = await panes(app);
      expect(changes.right).toBeLessThanOrEqual(board.left);
      expect(changes.top).toBe(board.top);
      expect(changes.bottom).toBe(term.bottom);
      expect(term.top).toBeGreaterThanOrEqual(board.bottom);
      expect(part.left).toBeGreaterThanOrEqual(board.right);
      expect(log.left).toBe(part.left);
      expect(log.top).toBeGreaterThanOrEqual(part.bottom);
      expect(log.bottom).toBe(term.bottom);
      const scrolls = await app.page.evaluate(() => ({ down: document.querySelector(".work").getBoundingClientRect().bottom > innerHeight, across: document.documentElement.scrollWidth > innerWidth }));
      expect(scrolls).toEqual({ down: false, across: false });
    });

    test("the open part is as tall as the board, whatever is in it", async ({ app }) => {
      for (const name of [null, "sd", "eeprom", "soc"]) {
        if (name) await app.open(name);
        const { board, part } = await panes(app);
        expect(part.top, String(name)).toBe(board.top);
        expect(part.height, String(name)).toBe(board.height);
      }
    });

    test("opening any part moves nothing", async ({ app }) => {
      const before = await geometry(app);
      for (const part of PARTS) {
        await app.open(part);
        await app.page.waitForTimeout(150);
        expect(await geometry(app), part).toEqual(before);
      }
    });
  });
}

test.describe("on a wide screen", () => {
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

test.describe("with a long list of changes", () => {
  test.use({ state: { configTxt: Array.from({ length: 80 }, (_, i) => `dtparam=x${i}=on`).join("\n") } });

  test("it scrolls inside its column, and the page still does not", async ({ app }) => {
    await expect(app.changes.locator(".changes li")).toHaveCount(1);
    const scroll = await app.changes.evaluate((el) => ({ inside: el.scrollHeight > el.clientHeight, page: document.querySelector(".work").getBoundingClientRect().bottom > innerHeight }));
    expect(scroll).toEqual({ inside: true, page: false });
  });
});

test("resizing across the breakpoint turns the column into a sheet and back", async ({ app }) => {
  await app.open("sd");
  await expect(app.drawer).toHaveCSS("position", "relative");
  await app.page.setViewportSize({ width: 900, height: 800 });
  await expect(app.drawer).toHaveCSS("position", "fixed");
  await app.part("sd").click();
  await expect(app.drawer).toBeHidden();
  await app.page.setViewportSize({ width: 1280, height: 900 });
  await expect(app.drawer).toBeVisible();
  await expect(app.drawer).toContainText("Click a part of the board");
});

test.describe("on a phone", () => {
  test.use({ viewport: { width: 375, height: 812 } });

  test("the changes stand between the board and the console", async ({ app }) => {
    const { changes, board, term } = await panes(app);
    expect(changes.top).toBeGreaterThanOrEqual(board.bottom);
    expect(term.top).toBeGreaterThanOrEqual(changes.bottom);
  });

  test("the open part is a sheet over the page, so opening a part moves nothing", async ({ app }) => {
    const before = await geometry(app);
    expect(before.pageOverflow).toBe(false);
    await expect(app.drawer).toBeHidden();
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

  test("the sheet closes", async ({ app }) => {
    await app.open("sd");
    await expect(app.drawer.getByRole("heading", { name: "microSD card" })).toBeInViewport();
    await app.drawer.getByRole("button", { name: "Close" }).click();
    await expect(app.drawer).toBeHidden();
  });
});
