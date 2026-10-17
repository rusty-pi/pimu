// Files for the tests to hand to the page. They are made when the tests start and
// are not kept in the repository: nothing here is firmware, only bytes of the
// right size with names the page expects, and a disk image whose contents say
// where in it they are.

import { existsSync, mkdirSync, writeFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const root = fileURLToPath(new URL("./data/", import.meta.url));

export const data = (name) => join(root, name);

function counters(words) {
  const bytes = Buffer.alloc(words * 4);
  for (let i = 0; i < words; i++) bytes.writeUInt32LE(i, i * 4);
  return bytes;
}

const FILES = {
  "boot/config.txt": () => "arm_64bit=1\n",
  "boot/start4.elf": () => "ELF",
  "boot/fixup4.dat": () => "DAT",
  "boot/overlays/disable-bt.dtbo": () => "DTBO",
  "disk.img": () => counters(1024),
  "edid.bin": () => Buffer.alloc(128),
  "hat.eep": () => Buffer.alloc(256, "H"),
  "pubkey.bin": () => Buffer.alloc(264),
  // The magic of an xz stream, then padding: enough to be refused as one.
  "image.img.xz": () => Buffer.concat([Buffer.from([0xfd, 0x37, 0x7a, 0x58, 0x5a, 0x00]), Buffer.alloc(1000)]),
};

for (const [name, make] of Object.entries(FILES)) {
  const path = data(name);
  if (existsSync(path)) continue;
  mkdirSync(dirname(path), { recursive: true });
  writeFileSync(path, make());
}
