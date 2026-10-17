// Nothing the person is working with jumps when a choice changes what the
// panel shows: a control at or above the one just clicked stays where it is,
// measured in the panel's own scrolled content, so scrolling is not a jump.

import { expect, test } from "./fixtures.mjs";

const PARTS = ["sd", "eeprom", "usb", "usbc", "eth", "hdmi", "gpio", "soc"];

function listControls(drawer) {
  const seen = new Map();
  const body = drawer.querySelector(".drawer-body") ?? drawer;
  const name = (el) =>
    el.getAttribute("aria-label") ?? el.closest("label")?.textContent.trim() ?? el.closest(".field")?.querySelector("label, .label")?.textContent.trim() ?? el.textContent.trim();
  return [...drawer.querySelectorAll("input, select, textarea, button")]
    .filter((el) => el.type !== "file" && el.checkVisibility())
    .map((el) => {
      const base = `${el.tagName}:${el.type}:${name(el)}`;
      const n = (seen.get(base) ?? 0) + 1;
      seen.set(base, n);
      const top = Math.round(el.getBoundingClientRect().top - body.getBoundingClientRect().top + body.scrollTop);
      return { key: `${base}#${n}`, top, toggle: el.type === "checkbox" || el.type === "radio", el };
    });
}

const controls = async (app) =>
  (await app.drawer.evaluate((drawer, source) => new Function(`return ${source}`)()(drawer).map(({ el, ...rest }) => rest), listControls.toString()));

const click = (app, key) =>
  app.drawer.evaluate((drawer, [source, wanted]) => new Function(`return ${source}`)()(drawer).find((c) => c.key === wanted)?.el.click(), [listControls.toString(), key]);

const positions = (list) => new Map(list.map((c) => [c.key, c.top]));

function jumped(before, after, limit) {
  const now = positions(after);
  return before.filter((c) => c.top <= limit && now.has(c.key) && now.get(c.key) !== c.top).map((c) => `${c.key}: ${c.top} -> ${now.get(c.key)}`);
}

for (const part of PARTS) {
  test(`${part}: no control moves when another is toggled`, async ({ app }) => {
    await app.open(part);
    const toggles = (await controls(app)).filter((c) => c.toggle);
    const shifts = [];
    for (const { key } of toggles) {
      const before = await controls(app);
      const target = before.find((c) => c.key === key);
      if (!target) continue;
      await click(app, key);
      shifts.push(...jumped(before, await controls(app), target.top).map((s) => `${key} => ${s}`));
    }
    expect(shifts).toEqual([]);
  });
}

test("plugging in the power does not move the controls of a panel", async ({ app }) => {
  await app.open("eth");
  await app.drawer.getByRole("checkbox", { name: "Plug the cable in" }).check();
  const before = await controls(app);
  await app.powerOn();
  await app.emit({ type: "phase", phase: "running" });
  expect(jumped(before, await controls(app), Infinity)).toEqual([]);
});
