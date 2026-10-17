import assert from "node:assert/strict";
import test from "node:test";

import { netBootOrder, withCardEdits } from "./netboot.mjs";

const text = (s) => new TextEncoder().encode(s);
const read = (list, path) => new TextDecoder().decode(list.find(([name]) => name === path)?.[1]);

test("config.txt lines go under [all], after what the file has", () => {
  const list = [["config.txt", text("arm_64bit=0")], ["start4.elf", text("x")]];
  const edited = withCardEdits(list, { config: ["enable_uart=1", "uart_2ndstage=1"] });
  assert.equal(read(edited, "config.txt"), "arm_64bit=0\n[all]\nenable_uart=1\nuart_2ndstage=1\n");
  assert.equal(read(list, "config.txt"), "arm_64bit=0");
  assert.equal(edited.length, 2);
});

test("a config.txt is made when the files have none", () => {
  const edited = withCardEdits([["start4.elf", text("x")]], { config: ["enable_uart=1"] });
  assert.equal(read(edited, "config.txt"), "[all]\nenable_uart=1\n");
});

test("cmdline.txt is replaced, not extended", () => {
  const edited = withCardEdits([["cmdline.txt", text("root=/dev/sda1")]], { cmdline: "console=ttyAMA0,115200" });
  assert.equal(read(edited, "cmdline.txt"), "console=ttyAMA0,115200\n");
});

test("nothing to apply leaves the files as they are", () => {
  const list = [["kernel8.img", text("k")]];
  assert.deepEqual(withCardEdits(list, {}), list);
});

test("the network comes before the card, TFTP before HTTP", () => {
  assert.deepEqual(netBootOrder({ tftp: true, http: true, sd: true }), ["2", "7", "1", "f"]);
  assert.deepEqual(netBootOrder({ http: true, usb: true, otg: true }), ["7", "4", "5", "f"]);
});
