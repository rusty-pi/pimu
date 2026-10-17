// Everything that touches WebAssembly runs here, never on the page's thread, so
// a slow guest cannot make the page sluggish. One worker per power-on: a WebAssembly memory never shrinks,
// so powering off ends the worker instead of freeing the board's RAM inside it.

import init, { BootImage, PiBuilder, eeprom_bootconf } from "./pkg/pimu.js";
import { download as fetchWithProgress, githubListing } from "./download.mjs";
import { withCardEdits } from "./netboot.mjs";
import { signImage } from "./signing.mjs";

const SLICE_MS = 100;
const STATE_EVERY_MS = 400;
const XZ_MAGIC = [0xfd, 0x37, 0x7a, 0x58, 0x5a, 0x00];

let pi = null;
let paused = false;
let stats = {};
let lastState = 0;

const post = (message, transfer) => postMessage(message, transfer ?? []);

// ---- downloads -------------------------------------------------------------

const transfers = [];
let lastProgress = 0;

function reportProgress(force = false) {
  const now = performance.now();
  if (!force && now - lastProgress < 100) return;
  lastProgress = now;
  post({ type: "progress", transfers: transfers.map((t) => ({ ...t })) });
}

const downloads = new Map();

function download(name, url) {
  if (!downloads.has(url)) {
    let listed = false;
    downloads.set(url, fetchWithProgress(name, url, (transfer, force) => {
      if (!listed) {
        transfers.push(transfer);
        listed = true;
      }
      reportProgress(force);
    }));
  }
  return downloads.get(url);
}

async function readLocal(name, file) {
  const transfer = { name, url: null, loaded: file.size, total: file.size, done: true, local: true };
  transfers.push(transfer);
  reportProgress(true);
  return new Uint8Array(await file.arrayBuffer());
}

// ---- disk images -----------------------------------------------------------

function counted(slot, image) {
  stats[slot] = { read: 0, requests: 0 };
  return {
    size: image.size,
    read(offset, length) {
      const bytes = image.read(offset, length);
      stats[slot].read += bytes.length;
      stats[slot].requests += 1;
      return bytes;
    },
  };
}

function refuseXz(head, what) {
  if (XZ_MAGIC.every((b, i) => head[i] === b)) {
    throw new Error(`${what} is xz-compressed, which the browser build cannot read in place: decompress it first`);
  }
}

function fileImage(file) {
  const reader = new FileReaderSync();
  const read = (offset, length) =>
    new Uint8Array(reader.readAsArrayBuffer(file.slice(offset, offset + length)));
  refuseXz(read(0, XZ_MAGIC.length), file.name);
  return { size: file.size, read };
}

function urlImage(url) {
  const head = new XMLHttpRequest();
  head.open("HEAD", url, false);
  head.send();
  if (head.status !== 200) throw new Error(`${url} answered ${head.status}`);
  const size = Number(head.getResponseHeader("content-length"));
  if (!size) throw new Error(`${url} does not say how long it is`);
  const read = (offset, length) => {
    const request = new XMLHttpRequest();
    request.open("GET", url, false);
    request.responseType = "arraybuffer";
    request.setRequestHeader("Range", `bytes=${offset}-${offset + length - 1}`);
    request.send();
    if (request.status !== 206) {
      throw new Error(`${url} answered ${request.status} to a Range request, so it cannot be read in pieces`);
    }
    return new Uint8Array(request.response);
  };
  refuseXz(read(0, XZ_MAGIC.length), url);
  return { size, read };
}

function imageOf(descriptor) {
  return descriptor.file ? fileImage(descriptor.file) : urlImage(descriptor.url);
}

// ---- power on --------------------------------------------------------------

async function collect(source) {
  if (source.kind === "files") {
    return Promise.all(source.files.map(async ({ path, file }) => [path, await readLocal(path, file)]));
  }
  const wanted = source.files ?? (await githubListing(source.github));
  return Promise.all(wanted.map(async (f) => [f.path, await download(f.path, f.url)]));
}

const HTTP_BOOT_DIRECTORY = "net_install";

async function httpBootFiles(files) {
  const volume = new BootImage();
  for (const [path, bytes] of files) volume.add_file(path, bytes);
  const image = volume.build();
  const { sig, pubkey } = await signImage(image);
  return {
    pubkey,
    served: [[`${HTTP_BOOT_DIRECTORY}/boot.img`, image], [`${HTTP_BOOT_DIRECTORY}/boot.sig`, sig]],
  };
}

async function powerOn(request) {
  stats = {};
  transfers.length = 0;
  downloads.clear();
  post({ type: "phase", phase: "downloading" });
  await init();

  const eeprom = request.eeprom;

  const bootFiles = request.boot.kind === "none" ? [] : await collect(request.boot);

  const extras = {};
  for (const key of ["pubkey", "hat", "edid"]) {
    if (request[key]) extras[key] = await readLocal(request[key].name, request[key]);
  }

  const edits = { config: request.options.configTxt, cmdline: request.options.cmdline };
  const tftpFiles = request.net?.tftp ? withCardEdits(await collect(request.net.tftp), edits) : [];
  let httpFiles = [];
  if (request.net?.http) {
    const boot = await httpBootFiles(withCardEdits(await collect(request.net.http), edits));
    httpFiles = boot.served;
    extras.pubkey = boot.pubkey;
  }

  post({ type: "phase", phase: "starting" });
  const builder = new PiBuilder(JSON.stringify(request.options), eeprom);
  for (const [path, bytes] of bootFiles) builder.add_file(path, bytes);
  for (const [path, bytes] of tftpFiles) builder.add_tftp_file(path, bytes);
  for (const [path, bytes] of httpFiles) builder.add_http_file(path, bytes);
  if (extras.pubkey) builder.set_pubkey(extras.pubkey);
  if (extras.hat) builder.set_hat(extras.hat);
  if (extras.edid) builder.set_edid(extras.edid);
  for (const [slot, descriptor] of Object.entries(request.images)) {
    builder.attach_image(slot, counted(slot, imageOf(descriptor)));
  }
  pi = builder.build();
  post({ type: "phase", phase: "running" });
  run();
}

function run() {
  try {
    step();
  } catch (e) {
    post({ type: "error", message: e.message ?? String(e) });
  }
}

function step() {
  if (!pi || paused) return;
  const output = pi.run(SLICE_MS);
  if (output.length) post({ type: "output", bytes: output }, [output.buffer]);
  const log = pi.take_log();
  if (log) post({ type: "log", text: log });
  const now = performance.now();
  const state = JSON.parse(pi.status());
  if (state.ended || now - lastState > STATE_EVERY_MS) {
    lastState = now;
    post({ type: "state", state, io: stats });
  }
  if (state.ended) {
    post({ type: "phase", phase: "ended" });
    return;
  }
  setTimeout(run, 0);
}

function info() {
  if (!pi) return;
  post({
    type: "info",
    deviceTree: pi.device_tree() ?? null,
    bootconf: pi.bootconf(),
    otp: JSON.parse(pi.otp()),
    state: JSON.parse(pi.status()),
  });
}

function save(what) {
  const bytes = what === "eeprom" ? pi.eeprom() : pi.device_tree_blob();
  if (bytes) post({ type: "file", what, bytes }, [bytes.buffer]);
}

onmessage = ({ data }) => {
  switch (data.type) {
    case "power-on":
      powerOn(data).catch((e) => post({ type: "error", message: e.message ?? String(e) }));
      break;
    case "bootconf":
      init().then(() => post({ type: "bootconf", id: data.id, entries: JSON.parse(eeprom_bootconf(data.bytes)) }));
      break;
    case "keys":
      pi?.send(data.keys);
      break;
    case "wake-on-lan":
      if (pi) post({ type: "wake-on-lan", sent: pi.send_magic_packet() });
      break;
    case "pause":
      paused = true;
      post({ type: "phase", phase: "paused" });
      break;
    case "resume":
      paused = false;
      post({ type: "phase", phase: "running" });
      run();
      break;
    case "info":
      info();
      break;
    case "save":
      save(data.what);
      break;
  }
};
