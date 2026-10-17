import assert from "node:assert/strict";
import test from "node:test";

import { BOOT_ORDER_PRESETS, GROUPS, KNOWN_KEYS, editedLines, formatBootOrder, imageSettings, parseBootOrder } from "./bootconf.js";

test("a boot order reads right to left", () => {
  assert.deepEqual(parseBootOrder("0xf41"), ["1", "4", "f"]);
  assert.equal(formatBootOrder(["1", "4", "f"]), "0xf41");
  assert.equal(parseBootOrder("nonsense"), null);
});

test("every preset survives a round trip", () => {
  for (const [, steps] of BOOT_ORDER_PRESETS) {
    assert.deepEqual(parseBootOrder(formatBootOrder(steps)), steps);
  }
});

test("only what changed leaves the form", () => {
  const image = imageSettings([["all", "BOOT_UART", "0"], ["gpio4=1", "BOOT_UART", "1"]]);
  const values = new Map([["BOOT_UART", "0"], ["WAKE_ON_GPIO", "0"], ["HDMI_DELAY", "5"], ["TFTP_IP", ""]]);
  assert.deepEqual(editedLines(values, image, null), ["WAKE_ON_GPIO=0"]);
  assert.deepEqual(editedLines(new Map(), image, ["1", "f"]), ["BOOT_ORDER=0xf1"]);
});

test("a setting is listed once, and a select offers its own default", () => {
  const keys = GROUPS.flatMap((g) => g.settings.map((s) => s.key));
  assert.equal(new Set(keys).size, keys.length);
  assert.ok(KNOWN_KEYS.has("BOOT_ORDER"));
  for (const setting of GROUPS.flatMap((g) => g.settings)) {
    if (setting.type === "select" && setting.default) {
      assert.ok(setting.options.some(([value]) => value === setting.default), setting.key);
    }
  }
});
