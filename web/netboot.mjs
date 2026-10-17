// What a network boot serves: the card's files with the page's config.txt and
// cmdline.txt lines applied, the same way the card's own files get them.

const decoder = new TextDecoder();
const encoder = new TextEncoder();

function put(list, path, bytes) {
  const at = list.findIndex(([name]) => name === path);
  if (at >= 0) list[at] = [path, bytes];
  else list.push([path, bytes]);
  return list;
}

// `list` is `[[path, bytes], ...]`; a new list comes back and `list` is left alone.
export function withCardEdits(list, { config = [], cmdline = "" } = {}) {
  const out = list.map(([path, bytes]) => [path, bytes]);
  if (config.length) {
    let text = decoder.decode(out.find(([path]) => path === "config.txt")?.[1] ?? new Uint8Array());
    if (text && !text.endsWith("\n")) text += "\n";
    put(out, "config.txt", encoder.encode(`${text}[all]\n${config.join("\n")}\n`));
  }
  if (cmdline) put(out, "cmdline.txt", encoder.encode(`${cmdline}\n`));
  return out;
}

// The boot order for the protocols that are on, ahead of the other boot media.
export function netBootOrder({ tftp, http, sd, usb, otg }) {
  return [tftp && "2", http && "7", sd && "1", usb && "4", otg && "5", "f"].filter(Boolean);
}
