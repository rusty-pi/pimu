// A card that keeps what the guest writes to it: the blocks written go to a
// file in the origin's private file system, with a bitmap of which they are,
// and the next power-on reads them back over the image. The image itself is
// never written.

const BLOCK = 512;

// What names a card's kept blocks: the image it overlays, by where it comes
// from.
export function keptKey({ file, url }) {
  return file ? `file:${file.name}:${file.size}:${file.lastModified}` : `url:${new URL(url, location.href)}`;
}

async function fileName(key) {
  const digest = await crypto.subtle.digest("SHA-256", new TextEncoder().encode(key));
  return [...new Uint8Array(digest)].map((b) => b.toString(16).padStart(2, "0")).join("");
}

async function cards() {
  const root = await navigator.storage.getDirectory();
  return root.getDirectoryHandle("cards", { create: true });
}

// The `{ data, map }` sync access handles for the card `key`, made if new.
export async function openKept(key) {
  const dir = await cards();
  const name = await fileName(key);
  const open = async (suffix) => (await dir.getFileHandle(`${name}.${suffix}`, { create: true })).createSyncAccessHandle();
  return { data: await open("data"), map: await open("map") };
}

// Drop what the guest wrote to the card `key`.
export async function forgetKept(key) {
  const dir = await cards();
  const name = await fileName(key);
  for (const suffix of ["data", "map"]) {
    await dir.removeEntry(`${name}.${suffix}`).catch(() => {});
  }
}

// `image` (`{ size, read }`) with the blocks written kept in `handles`, as
// `openKept` gives them: reads see them over the image, and `write` adds to
// them.
export function keptImage(image, { data, map: mapHandle }) {
  const blocks = Math.ceil(image.size / BLOCK);
  const map = new Uint8Array(Math.ceil(blocks / 8));
  mapHandle.read(map, { at: 0 });
  const kept = (b) => (map[b >> 3] >> (b & 7)) & 1;

  const read = (offset, length) => {
    const out = image.read(offset, length);
    const first = Math.floor(offset / BLOCK);
    const last = Math.floor((offset + length - 1) / BLOCK);
    for (let b = first; b <= last; ) {
      if (map[b >> 3] === 0) {
        b = (b | 7) + 1;
        continue;
      }
      if (!kept(b)) {
        b++;
        continue;
      }
      let end = b;
      while (end < last && kept(end + 1)) end++;
      const from = Math.max(b * BLOCK, offset);
      const to = Math.min((end + 1) * BLOCK, offset + length);
      data.read(out.subarray(from - offset, to - offset), { at: from });
      b = end + 1;
    }
    return out;
  };

  const write = (offset, bytes) => {
    data.write(bytes, { at: offset });
    const first = Math.floor(offset / BLOCK);
    const last = Math.floor((offset + bytes.length - 1) / BLOCK);
    for (let b = first; b <= last; b++) map[b >> 3] |= 1 << (b & 7);
    mapHandle.write(map.subarray(first >> 3, (last >> 3) + 1), { at: first >> 3 });
  };

  const close = () => {
    for (const h of [data, mapHandle]) {
      h.flush();
      h.close();
    }
  };

  return { size: image.size, read, write, close };
}
