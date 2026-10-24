import assert from "node:assert/strict";
import test from "node:test";

import { keptImage } from "./kept.mjs";

// A sync access handle on memory, as far as `keptImage` uses one.
function handle(bytes = new Uint8Array(0)) {
  let file = bytes;
  return {
    read(buf, { at }) {
      const n = Math.max(0, Math.min(buf.length, file.length - at));
      buf.set(file.subarray(at, at + n));
      return n;
    },
    write(buf, { at }) {
      if (at + buf.length > file.length) {
        const grown = new Uint8Array(at + buf.length);
        grown.set(file);
        file = grown;
      }
      file.set(buf, at);
      return buf.length;
    },
    flush() {},
    close() {},
    get bytes() {
      return file;
    },
  };
}

const image = (size) => ({
  size,
  read: (offset, length) => new Uint8Array(length).fill(0xee),
});

test("reads show the blocks written over the image, and only those", () => {
  const card = keptImage(image(64 * 512), { data: handle(), map: handle() });
  card.write(3 * 512, new Uint8Array(2 * 512).fill(0x33));
  const got = card.read(0, 8 * 512);
  assert.equal(got[3 * 512 - 1], 0xee);
  assert.equal(got[3 * 512], 0x33);
  assert.equal(got[5 * 512 - 1], 0x33);
  assert.equal(got[5 * 512], 0xee);
});

test("what one power-on wrote, the next one reads", () => {
  const data = handle();
  const map = handle();
  keptImage(image(64 * 512), { data, map }).write(40 * 512, new Uint8Array(512).fill(7));
  const again = keptImage(image(64 * 512), { data: handle(data.bytes), map: handle(map.bytes) });
  assert.equal(again.read(40 * 512, 512)[0], 7);
  assert.equal(again.read(39 * 512, 512)[0], 0xee);
});

test("a read that starts inside a block written gets the rest of it", () => {
  const card = keptImage(image(16 * 512), { data: handle(), map: handle() });
  card.write(512, new Uint8Array(512).map((_, i) => i & 0xff));
  const got = card.read(512 + 100, 20);
  assert.deepEqual([...got], [...Array(20).keys()].map((i) => (100 + i) & 0xff));
});
