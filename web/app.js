// The page: a picture of the board whose ports take what is plugged into them,
// the serial console, the log, and what the machine looks like from inside.
// Nothing here touches WebAssembly: the machine runs in worker.js.

import { download } from "./download.js";
import {
  BOOT_DEVICES, BOOT_ORDER_PRESETS, GROUPS, KNOWN_KEYS,
  conditionalSections, editedLines, formatBootOrder, imageSettings, parseBootOrder,
} from "./bootconf.js";

const FIRMWARE = "https://raw.githubusercontent.com/raspberrypi/firmware/refs/heads/master/boot/";
const OFFICIAL_FILES = [
  "start4.elf", "fixup4.dat", "bcm2711-rpi-4-b.dtb", "kernel8.img",
  "overlays/disable-bt.dtbo", "overlays/overlay_map.dtb",
].map((path) => ({ path, url: FIRMWARE + path }));
const OFFICIAL_EEPROM =
  "https://raw.githubusercontent.com/raspberrypi/rpi-eeprom/master/firmware-2711/latest/pieeprom-2026-08-04.bin";

const LOG_CHANNELS = [
  ["io", "Blocks read and written, OTP rows, and what the network peer did"],
  ["mbox", "ARM ↔ VideoCore property mailbox"],
  ["arm-exc", "Exceptions the ARM cores take"],
  ["uart", "The two console UARTs"],
  ["emmc", "The SD host: commands, responses, blocks"],
  ["gpio", "GPIO pins changing function or level"],
  ["otp", "OTP rows read and programmed"],
  ["spi", "SPI traffic to the EEPROM"],
  ["xhci", "xHCI rings and port state"],
  ["dwc2", "The USB-C OTG controller"],
  ["pcie", "PCIe interrupts and window writes"],
  ["pmic", "PMIC register traffic"],
  ["expander", "GPIO expander registers"],
  ["irqen", "Interrupt enables"],
  ["cmp", "System-timer compare arms"],
];

const DEFAULT_STATE = {
  sd: { mode: "official", image: { kind: "url", url: "" }, github: { repo: "raspberrypi/firmware", ref: "master", path: "boot" } },
  configTxt: "arm_64bit=1\nenable_uart=1\nuart_2ndstage=1\ndtoverlay=disable-bt",
  cmdline: "console=ttyAMA0,115200 earlycon",
  eeprom: { kind: "official", url: OFFICIAL_EEPROM },
  bootconf: {},
  bootOrder: null,
  extraBootconf: "",
  skipSignedBoot: false,
  tryboot: false,
  usb: { kind: "none", url: "", mb: "" },
  otg: { kind: "none", url: "", dock: false },
  net: false,
  display: false,
  board: { stepping: "c0", memoryMb: 1024, boardRev: "" },
  otpRows: "",
  advanced: { jitter: "", faults: "", checkCoherency: false, checkAlignment: false, skipUnimpl: false },
  log: ["io"],
};

// ---- small helpers -----------------------------------------------------------

const $ = (selector, root = document) => root.querySelector(selector);
const clean = (list) => list.flat().filter((child) => child != null && child !== false);

function h(tag, attrs = {}, ...children) {
  const node = document.createElement(tag);
  for (const [key, value] of Object.entries(attrs)) {
    if (value == null) continue;
    if (key.startsWith("aria-") || key.startsWith("data-")) node.setAttribute(key, String(value));
    else if (value === false) continue;
    else if (key === "class") node.className = value;
    else if (key.startsWith("on")) node.addEventListener(key.slice(2), value);
    else if (key === "value") node.value = value;
    else if (key === "checked" || key === "disabled" || key === "selected") node[key] = !!value;
    else node.setAttribute(key, value === true ? "" : value);
  }
  for (const child of children.flat()) {
    if (child == null || child === false) continue;
    node.append(child.nodeType ? child : document.createTextNode(String(child)));
  }
  return node;
}

function mergeDeep(base, extra) {
  if (Array.isArray(base) || typeof base !== "object" || base === null) return extra ?? base;
  const out = { ...base };
  for (const [key, value] of Object.entries(extra ?? {})) out[key] = key in base ? mergeDeep(base[key], value) : value;
  return out;
}

const state = (() => {
  try {
    return mergeDeep(DEFAULT_STATE, JSON.parse(localStorage.getItem("pimu-web") ?? "{}"));
  } catch {
    return structuredClone(DEFAULT_STATE);
  }
})();

let saveTimer = 0;
function changed() {
  refreshBoard();
  clearTimeout(saveTimer);
  saveTimer = setTimeout(() => {
    try {
      localStorage.setItem("pimu-web", JSON.stringify(state));
    } catch {
      // private window or blocked storage: the settings just do not outlive the tab
    }
  }, 200);
}

// Files the person picked. They cannot be stored, so they last as long as the tab.
const files = { boot: [], sdImage: null, eeprom: null, usb: null, otg: null, hat: null, edid: null, pubkey: null, net: [] };

function toast(message, bad = false) {
  const el = $("#toast");
  el.textContent = message;
  el.className = bad ? "toast bad" : "toast";
  el.hidden = false;
  clearTimeout(toast.timer);
  toast.timer = setTimeout(() => (el.hidden = true), bad ? 8000 : 3500);
}

const mib = (bytes) => (bytes >= 1 << 20 ? `${(bytes / (1 << 20)).toFixed(1)} MiB` : `${Math.max(1, Math.round(bytes / 1024))} KiB`);

// ---- power and the worker --------------------------------------------------------

let worker = null;
let phase = "off";
let powered = () => phase !== "off" && phase !== "error";
let lastState = null;
let lastIo = {};

function setPhase(next, detail) {
  phase = next;
  const labels = { off: "Off", downloading: "Downloading", starting: "Starting", running: "Running", paused: "Paused", ended: "Stopped", error: "Error" };
  const pill = $("#phase");
  pill.dataset.phase = next;
  pill.textContent = labels[next] + (detail ? ` – ${detail}` : "");
  const on = powered();
  $("#power").textContent = on ? "Power off" : "Power on";
  $("#power").dataset.on = on;
  $("#power-plug").dataset.on = on;
  $("#power-label").textContent = on ? "click to unplug" : "click to plug in";
  $("#led-power").dataset.on = on;
  $("#pause").style.visibility = next === "running" || next === "paused" ? "visible" : "hidden";
  $("#pause").textContent = next === "paused" ? "Resume" : "Pause";
  for (const part of document.querySelectorAll(".part")) part.dataset.locked = on && part.dataset.part !== "soc" ? "true" : "false";
  if (!$("#tab-log").hidden) renderLog();
  if (!on) {
    setCores(null);
    $("#led-act").dataset.on = false;
  }
  renderDrawer();
}

async function powerOn() {
  clearScreen();
  transfers.length = 0;
  setPhase("downloading", "preparing");
  let request;
  try {
    request = await buildRequest();
  } catch (e) {
    setPhase("off");
    $("#downloads").hidden = true;
    toast(e.message ?? String(e), true);
    return;
  }
  worker = new Worker("worker.js", { type: "module" });
  worker.onmessage = ({ data }) => onWorker(data);
  worker.onerror = (e) => fail(e.message || "the worker stopped");
  worker.postMessage(request);
  $("#term").focus();
}

function powerOff() {
  worker?.terminate();
  worker = null;
  renderInspect(null);
  setPhase("off");
  $("#downloads").hidden = true;
  lastState = null;
  $("#clock").textContent = "";
}

function fail(message) {
  worker?.terminate();
  worker = null;
  setPhase("error");
  $("#downloads").hidden = true;
  toast(message, true);
}

function onWorker(data) {
  switch (data.type) {
    case "phase":
      setPhase(data.phase);
      if (data.phase === "running") setTimeout(() => (phase === "running" ? ($("#downloads").hidden = true) : 0), 1200);
      break;
    case "progress":
      renderDownloads([...transfers, ...data.transfers]);
      break;
    case "output":
      terminal.write(data.bytes);
      break;
    case "log":
      addLog(data.text);
      break;
    case "state":
      lastState = data.state;
      lastIo = data.io ?? {};
      showState();
      break;
    case "info":
      renderInspect(data);
      break;
    case "file":
      saveFile(data.what === "eeprom" ? "pieeprom-dump.bin" : "device-tree.dtb", data.bytes);
      break;
    case "wake-on-lan":
      toast(data.sent ? "Magic packet sent" : "No network peer to send it", !data.sent);
      break;
    case "error":
      fail(data.message);
      break;
  }
}

function saveFile(name, bytes) {
  const url = URL.createObjectURL(new Blob([bytes]));
  const a = h("a", { href: url, download: name });
  document.body.append(a);
  a.click();
  a.remove();
  setTimeout(() => URL.revokeObjectURL(url), 5000);
}

// ---- the request --------------------------------------------------------------------

const eepromCache = new Map();
const transfers = [];

function trackedDownload(name, url) {
  let listed = false;
  return download(name, url, (transfer) => {
    if (!listed) {
      transfers.push(transfer);
      listed = true;
    }
    renderDownloads(transfers);
  });
}

async function eepromBytes() {
  if (files.eeprom && state.eeprom.kind === "file") return new Uint8Array(await files.eeprom.arrayBuffer());
  const url = state.eeprom.url;
  if (!eepromCache.has(url)) {
    eepromCache.set(url, await trackedDownload("pieeprom.bin", url));
    if (phase === "off") setTimeout(() => (phase === "off" ? ($("#downloads").hidden = true) : 0), 1500);
  }
  return eepromCache.get(url);
}

function imageDescriptor(spec, file, what) {
  if (spec.kind === "file") {
    if (!file) throw new Error(`${what}: no file picked`);
    return { file };
  }
  if (!spec.url.trim()) throw new Error(`${what}: no URL given`);
  return { url: spec.url.trim() };
}

function parseOtpRows(text) {
  return text
    .split("\n")
    .map((line) => line.trim())
    .filter(Boolean)
    .map((line) => {
      const m = /^(\w+)\s*[=:\s]\s*(\w+)$/.exec(line);
      const row = m && Number(m[1]);
      const value = m && Number(m[2]);
      if (!m || !Number.isInteger(row) || !Number.isInteger(value)) throw new Error(`OTP row "${line}": expected ROW=VALUE`);
      return [row, value >>> 0];
    });
}

async function buildRequest() {
  const options = {};
  const s = state;
  options.stepping = s.board.stepping;
  if (s.board.boardRev.trim()) options.boardRev = s.board.boardRev.trim();
  else options.memoryMb = Number(s.board.memoryMb);

  const info = await eepromInfo();
  const lines = editedLines(new Map(Object.entries(s.bootconf)), info.settings, null);
  const extra = s.extraBootconf.split("\n").map((l) => l.trim()).filter(Boolean);
  options.bootconf = [...lines, ...extra];
  if (s.bootOrder) options.bootOrder = formatBootOrder(s.bootOrder);
  options.skipSignedBoot = s.skipSignedBoot;
  options.tryboot = s.tryboot;
  options.otpRows = parseOtpRows(s.otpRows);
  options.log = s.log.join(",");
  options.display = s.display;
  options.network = s.net;
  options.checkCoherency = s.advanced.checkCoherency;
  options.checkAlignment = s.advanced.checkAlignment;
  options.skipUnimpl = s.advanced.skipUnimpl;
  if (s.advanced.jitter.trim()) options.jitter = Number(s.advanced.jitter);
  if (s.advanced.faults.trim()) options.faults = Number(s.advanced.faults);

  const request = { type: "power-on", options, images: {}, boot: { kind: "none" } };
  request.eeprom = info.bytes;

  if (s.sd.mode === "image") {
    request.images.sd = imageDescriptor(s.sd.image, files.sdImage, "SD card image");
  } else {
    const cfg = s.configTxt.split("\n").map((l) => l.trim()).filter(Boolean);
    if (cfg.length) options.configTxt = cfg;
    if (s.cmdline.trim()) options.cmdline = s.cmdline.trim();
    if (s.sd.mode === "official") request.boot = { kind: "download", files: OFFICIAL_FILES };
    else if (s.sd.mode === "github") request.boot = { kind: "download", github: { ...s.sd.github } };
    else {
      if (!files.boot.length) throw new Error("SD card: no boot files picked");
      request.boot = { kind: "files", files: files.boot };
    }
  }
  if (s.usb.kind !== "none") {
    request.images.usb = imageDescriptor(s.usb, files.usb, "USB stick");
    if (s.usb.mb) options.usbMb = Number(s.usb.mb);
  }
  if (s.otg.kind !== "none") {
    request.images.otg = imageDescriptor(s.otg, files.otg, "USB-C stick");
    options.otgDock = s.otg.dock;
  }
  if (files.pubkey) request.pubkey = files.pubkey;
  if (files.hat) request.hat = files.hat;
  if (s.display && files.edid) request.edid = files.edid;
  if (s.net) request.netFiles = files.net;
  return request;
}

// ---- the EEPROM image, read for its settings ---------------------------------------

let tools = null;
const pending = new Map();
let nextId = 1;

function parseBootconf(bytes) {
  tools ??= (() => {
    const w = new Worker("worker.js", { type: "module" });
    w.onmessage = ({ data }) => {
      if (data.type === "bootconf") pending.get(data.id)?.(data.entries);
    };
    return w;
  })();
  return new Promise((resolve) => {
    const id = nextId++;
    pending.set(id, resolve);
    tools.postMessage({ type: "bootconf", id, bytes });
  });
}

let infoFor = null;
let infoPromise = null;
async function eepromInfo() {
  const key = state.eeprom.kind === "file" ? `file:${files.eeprom?.name}:${files.eeprom?.lastModified}` : state.eeprom.url;
  if (infoFor !== key) {
    infoFor = key;
    infoPromise = (async () => {
      const bytes = await eepromBytes();
      const entries = await parseBootconf(bytes.slice());
      return { bytes, entries, settings: imageSettings(entries) };
    })();
  }
  try {
    return await infoPromise;
  } catch (e) {
    infoFor = null;
    throw e;
  }
}

// ---- the downloads panel --------------------------------------------------------------

function renderDownloads(list) {
  const panel = $("#downloads");
  panel.hidden = false;
  const remote = list.filter((t) => !t.local);
  const known = remote.every((t) => t.total > 0);
  const loaded = remote.reduce((n, t) => n + t.loaded, 0);
  const total = remote.reduce((n, t) => n + t.total, 0);
  const rows = list.map((t) =>
    h("div", { class: "dl" },
      h("span", { class: "name", title: t.url ?? t.name }, t.name),
      h("div", { class: `bar${t.total || t.done ? "" : " indeterminate"}` }, h("i", { style: `width:${t.total ? Math.min(100, (100 * t.loaded) / t.total) : t.done ? 100 : 0}%` })),
      h("span", { class: "size" }, t.local ? `${mib(t.loaded)} (read from disk)` : t.done ? `${mib(t.loaded)} ✓` : t.total ? `${mib(t.loaded)} / ${mib(t.total)}` : mib(t.loaded)),
    ));
  const doneCount = list.filter((t) => t.done).length;
  panel.replaceChildren(...clean([
    h("h2", {}, phase === "downloading" || phase === "off" ? "Downloading firmware" : "Firmware"),
    ...rows,
    remote.length > 1
      ? h("div", { class: "dl total" },
          h("span", {}, `${doneCount} of ${list.length} files`),
          h("div", { class: `bar${known ? "" : " indeterminate"}` }, h("i", { style: `width:${known && total ? (100 * loaded) / total : 0}%` })),
          h("span", { class: "size" }, known ? `${mib(loaded)} / ${mib(total)}` : mib(loaded)))
      : null,
  ]));
}

// ---- cores, clock, LEDs ---------------------------------------------------------------

const CORE_WORDS = { off: "off", sleeping: "asleep", running: "running" };

function setCores(cores) {
  const set = (id, c) => {
    const state = c?.state ?? "off";
    $(`[data-core="${id}"]`).dataset.state = state;
    $(`[data-core-state="${id}"]`).textContent = CORE_WORDS[state];
  };
  for (let i = 0; i < 4; i++) set(`arm${i}`, cores?.arm[i]);
  for (let i = 0; i < 2; i++) set(`vpu${i}`, cores?.vpu[i]);
}

function showState() {
  const st = lastState;
  if (!st) return;
  setCores(st.cores);
  $("#led-act").dataset.on = st.armReleased;
  const seconds = (st.modelUs / 1e6).toFixed(1);
  const arm = st.cores.arm.reduce((n, c) => n + c.insns, 0);
  const millions = (n) => `${(n / 1e6).toFixed(0)} M`;
  $("#clock").textContent = `model time ${seconds} s · VPU ${millions(st.retired)} instructions · ARM ${millions(arm)}`;
  const result = $("#result");
  if (st.ended) {
    result.textContent = `${st.ok ? "ok" : "failed"} – ${st.why}`;
    result.title = result.textContent;
    result.dataset.ok = st.ok;
  } else {
    result.textContent = st.reboots ? `${st.reboots} reset${st.reboots > 1 ? "s" : ""}` : "";
    result.dataset.ok = "";
  }
  const reading = Object.entries(lastIo).filter(([, v]) => v.requests).map(([slot, v]) => `${slot}: ${mib(v.read)} read`);
  $("#clock").title = reading.join("\n");
  if (reading.length) $("#clock").textContent += ` · ${reading.join(", ")}`;
}

// ---- the serial console -----------------------------------------------------------------

const terminal = (() => {
  const el = $("#term");
  const decoder = new TextDecoder();
  let text = "";
  let escape = "";
  let queued = false;
  const MAX = 400_000;

  function feed(chunk) {
    for (const ch of chunk) {
      if (escape) {
        escape += ch;
        if (escape[1] === "]") {
          // An operating-system command (a window title): up to BEL or ESC \.
          if (ch === "\x07" || escape.endsWith("\x1b\\")) escape = "";
        } else if (escape[1] === "[") {
          if (escape.length > 2 && /[@-~]/.test(ch)) escape = "";
        } else {
          escape = "";
        }
        continue;
      }
      if (ch === "\x1b") escape = ch;
      else if (ch === "\r") continue;
      else if (ch === "\b") text = text.endsWith("\n") ? text : text.slice(0, -1);
      else text += ch;
    }
    if (text.length > MAX) text = text.slice(text.length - MAX / 2);
  }

  function paint() {
    queued = false;
    const stick = el.scrollTop + el.clientHeight >= el.scrollHeight - 6;
    el.textContent = text;
    if (stick) el.scrollTop = el.scrollHeight;
  }

  return {
    write(bytes) {
      feed(decoder.decode(bytes, { stream: true }));
      if (!queued) {
        queued = true;
        requestAnimationFrame(paint);
      }
    },
    clear() {
      text = "";
      escape = "";
      el.textContent = "";
    },
  };
})();

function clearScreen() {
  terminal.clear();
  renderInspect(null);
  logRows.length = 0;
  logCounts.clear();
  $("#result").textContent = "";
  $("#log-count").textContent = "";
  renderLog();
}

$("#term").addEventListener("keydown", (event) => {
  if (!worker) return;
  let keys = null;
  if (event.ctrlKey && event.key.length === 1 && /[a-z]/i.test(event.key)) keys = Uint8Array.of(event.key.toLowerCase().charCodeAt(0) - 96);
  else if (event.metaKey || event.altKey) return;
  else if (event.key === "Enter") keys = Uint8Array.of(13);
  else if (event.key === "Backspace") keys = Uint8Array.of(127);
  else if (event.key === "Tab") keys = Uint8Array.of(9);
  else if (event.key === "Escape") keys = Uint8Array.of(27);
  else if (event.key.length === 1) keys = new TextEncoder().encode(event.key);
  if (keys) {
    event.preventDefault();
    worker.postMessage({ type: "keys", keys });
  }
});
$("#term").addEventListener("paste", (event) => {
  if (!worker) return;
  event.preventDefault();
  worker.postMessage({ type: "keys", keys: new TextEncoder().encode(event.clipboardData.getData("text").replace(/\r?\n/g, "\r")) });
});

// ---- the log ---------------------------------------------------------------------------

const logRows = [];
const logCounts = new Map();
const logOff = new Set();
let logFilter = "";
let logTimer = 0;
const LOG_KEPT = 20_000;

function addLog(text) {
  for (const line of text.split("\n")) {
    if (!line) continue;
    let row;
    try {
      row = JSON.parse(line);
    } catch {
      continue;
    }
    logRows.push(row);
    logCounts.set(row.channel, (logCounts.get(row.channel) ?? 0) + 1);
  }
  if (logRows.length > LOG_KEPT) logRows.splice(0, logRows.length - LOG_KEPT);
  $("#log-count").textContent = logRows.length ? `(${logRows.length})` : "";
  if (!logTimer && !$("#tab-log").hidden) logTimer = setTimeout(() => {
    logTimer = 0;
    renderFilters();
    renderLogRows();
  }, 250);
}

function describeRow(row) {
  if (typeof row.msg === "string") return row.msg;
  return Object.entries(row)
    .filter(([k]) => k !== "us" && k !== "channel")
    .map(([k, v]) => `${k}=${Array.isArray(v) ? v.join(",") : v}`)
    .join("  ");
}

const hue = (name) => [...name].reduce((n, c) => (n * 31 + c.charCodeAt(0)) % 360, 7);

function renderLog() {
  const pane = $("#tab-log");
  const channelButtons = LOG_CHANNELS.map(([name, help]) =>
    h("button", {
      class: "chip-btn", "aria-pressed": state.log.includes(name), title: help, disabled: powered(),
      onclick: () => {
        state.log = state.log.includes(name) ? state.log.filter((c) => c !== name) : [...state.log, name];
        changed();
        renderLog();
      },
    }, name));
  pane.replaceChildren(
    h("details", { open: !logRows.length }, h("summary", {}, `Channels to record (${state.log.length} on)`),
      h("p", { class: "hint" }, powered() ? "Power off to change the channels: they are chosen when the board starts." : "Pick what the machine should log at the next power on. Lines come out in the order things happened, stamped with the model's own clock."),
      h("div", { class: "channels" }, channelButtons),
      h("div", { class: "toolbar" },
        h("button", { class: "small", disabled: powered(), onclick: () => { state.log = []; changed(); renderLog(); } }, "None"),
        h("button", { class: "small", disabled: powered(), onclick: () => { state.log = ["io"]; changed(); renderLog(); } }, "Just I/O"),
        h("button", { class: "small", disabled: powered(), onclick: () => { state.log = LOG_CHANNELS.map((c) => c[0]); changed(); renderLog(); } }, "All (slow)"))),
    h("div", { class: "toolbar" },
      h("input", { type: "search", placeholder: "Filter lines", value: logFilter, "aria-label": "Filter log lines", oninput: (e) => { logFilter = e.target.value; renderLogRows(); } }),
      h("button", { class: "small", onclick: () => { const text = logRows.map((r) => JSON.stringify(r)).join("\n"); saveFile("pimu-log.jsonl", new TextEncoder().encode(text)); } }, "Save .jsonl")),
    h("div", { class: "channels", id: "log-filters" }),
    h("div", { class: "logbox", id: "logbox", role: "log" }),
  );
  renderFilters();
  renderLogRows();
}

// One chip a channel that has logged, with its line count, to hide or show it.
function renderFilters() {
  const box = $("#log-filters");
  if (!box) return;
  box.replaceChildren(...[...logCounts.keys()].sort().map((name) =>
    h("button", {
      class: "chip-btn", "aria-pressed": !logOff.has(name),
      onclick: () => {
        logOff.has(name) ? logOff.delete(name) : logOff.add(name);
        renderFilters();
        renderLogRows();
      },
    }, name, h("span", { class: "n" }, logCounts.get(name)))));
}

function renderLogRows() {
  const box = $("#logbox");
  if (!box) return;
  const needle = logFilter.toLowerCase();
  const stick = box.scrollTop + box.clientHeight >= box.scrollHeight - 8;
  const shown = [];
  for (let i = logRows.length - 1; i >= 0 && shown.length < 600; i--) {
    const row = logRows[i];
    if (logOff.has(row.channel)) continue;
    const text = describeRow(row);
    if (needle && !text.toLowerCase().includes(needle) && !row.channel.includes(needle)) continue;
    shown.push(h("div", { class: "logrow" },
      h("span", { class: "t" }, (row.us / 1e6).toFixed(6)),
      h("span", { class: "c", style: `color:hsl(${hue(row.channel)} 55% var(--ch-l))` }, row.channel),
      h("span", {}, text)));
  }
  box.replaceChildren(...shown.reverse());
  if (!logRows.length) box.append(h("p", { class: "hint", style: "padding:8px" }, "Nothing logged yet."));
  if (stick) box.scrollTop = box.scrollHeight;
}

// ---- inspect ---------------------------------------------------------------------------

let inspectTimer = 0;
let inspectShown = false;

function renderInspect(info) {
  const pane = $("#tab-info");
  inspectShown = !!info;
  if (!info) {
    pane.replaceChildren(h("p", { class: "hint" }, worker ? "Reading the board…" : "Power the board on to look inside it."));
    return;
  }
  const st = info.state;
  const cores = st.cores;
  pane.replaceChildren(
    h("div", { class: "inspect" },
      h("section", {}, h("h3", {}, "Run"),
        h("table", { class: "kv" },
          h("tr", {}, h("th", {}, "Model time"), h("td", { class: "num" }, `${(st.modelUs / 1e6).toFixed(3)} s`)),
          h("tr", {}, h("th", {}, "ARM started by the firmware"), h("td", {}, st.armReleased ? "yes" : "not yet")),
          h("tr", {}, h("th", {}, "Resets so far"), h("td", { class: "num" }, st.reboots)),
          h("tr", {}, h("th", {}, "Outcome"), h("td", {}, st.ended ? `${st.ok ? "ok" : "failed"} – ${st.why}` : "still running")),
          st.logDropped ? h("tr", {}, h("th", {}, "Log lines dropped"), h("td", { class: "num" }, st.logDropped)) : null)),
      h("section", {}, h("h3", {}, "Cores"),
        h("table", { class: "kv" },
          ...cores.vpu.map((c, i) => h("tr", {}, h("th", {}, `VPU ${i}`), h("td", {}, c.state), h("td", { class: "num" }, `${c.insns.toLocaleString()} instructions`))),
          ...cores.arm.map((c, i) => h("tr", {}, h("th", {}, `ARM ${i}`), h("td", {}, c.state), h("td", { class: "num" }, `${c.insns.toLocaleString()} instructions`))))),
      h("section", {}, h("h3", {}, "EEPROM bootloader configuration"),
        info.bootconf.length ? h("pre", {}, info.bootconf.join("\n")) : h("p", { class: "hint" }, "No bootconf.txt."),
        h("div", { class: "toolbar" }, h("button", { class: "small", onclick: () => worker?.postMessage({ type: "save", what: "eeprom" }) }, "Download the flash as the firmware left it"))),
      h("section", {}, h("h3", {}, "Fuses (OTP) that are not blank"),
        info.otp.length
          ? h("table", { class: "kv" }, ...info.otp.map((r) => h("tr", {}, h("th", {}, `row ${r.row}`), h("td", { class: "num" }, "0x" + r.value.toString(16).padStart(8, "0")), h("td", {}, r.meaning))))
          : h("p", { class: "hint" }, "All blank.")),
      h("section", {}, h("h3", {}, "Device tree handed to the ARM"),
        info.deviceTree
          ? [h("pre", {}, info.deviceTree), h("div", { class: "toolbar" }, h("button", { class: "small", onclick: () => worker?.postMessage({ type: "save", what: "dtb" }) }, "Download .dtb"))]
          : h("p", { class: "hint" }, "The firmware has not built it yet."))));
}

function refreshInspect() {
  clearTimeout(inspectTimer);
  if ($("#tab-info").hidden) return;
  if (worker && phase !== "downloading") {
    if (!inspectShown) renderInspect(null);
    worker.postMessage({ type: "info" });
  } else renderInspect(null);
  inspectTimer = setTimeout(refreshInspect, 2500);
}

// ---- tabs -------------------------------------------------------------------------------

for (const button of document.querySelectorAll(".tabs [data-tab]")) {
  button.addEventListener("click", () => {
    for (const other of document.querySelectorAll(".tabs [data-tab]")) {
      const on = other === button;
      other.setAttribute("aria-selected", on);
      $(`#tab-${other.dataset.tab}`).hidden = !on;
    }
    if (button.dataset.tab === "log") renderLog();
    if (button.dataset.tab === "info") refreshInspect();
  });
}

// ---- the drawer: what each part of the board takes -----------------------------------------

let openPart = null;

function filePicker(label, { accept, multiple, directory, onPick, current }) {
  const input = h("input", { type: "file", hidden: true, accept, multiple, ...(directory ? { webkitdirectory: "" } : {}) });
  input.addEventListener("change", () => {
    if (input.files.length) onPick([...input.files]);
  });
  return h("span", { class: "filebtn" },
    input,
    h("button", { class: "small", type: "button", onclick: () => input.click() }, label),
    current ? h("span", { class: "filename" }, current) : null);
}

function radio(name, value, checked, label, help, onChange) {
  return h("label", { class: "choice" },
    h("input", { type: "radio", name, checked, onchange: onChange }),
    h("span", {}, label, help ? h("small", {}, help) : null));
}

function imagePicker(slot, spec, what) {
  const file = files[slot === "sd" ? "sdImage" : slot];
  return h("div", {},
    h("div", { class: "row" },
      h("label", {}, h("input", { type: "radio", name: `${slot}-kind`, checked: spec.kind === "file", onchange: () => { spec.kind = "file"; changed(); renderDrawer(); } }), " Local file"),
      h("label", {}, h("input", { type: "radio", name: `${slot}-kind`, checked: spec.kind === "url", onchange: () => { spec.kind = "url"; changed(); renderDrawer(); } }), " URL")),
    spec.kind === "file"
      ? h("div", { class: "field" }, filePicker(`Choose ${what}…`, { accept: ".img,.iso,.bin,.raw", onPick: ([f]) => { files[slot === "sd" ? "sdImage" : slot] = f; renderDrawer(); }, current: file ? `${file.name} (${mib(file.size)})` : "nothing chosen" }))
      : h("div", { class: "field" }, h("input", { type: "url", placeholder: "https://example.org/disk.img", value: spec.url, "aria-label": `${what} URL`, oninput: (e) => { spec.url = e.target.value; changed(); } })),
    h("p", { class: "help hint" }, "Read in pieces as the guest asks for blocks – never loaded whole. What the guest writes stays in memory. A URL needs a server that allows cross-origin Range requests. xz-compressed images must be decompressed first."));
}

function drawerFrame(title, ...body) {
  return [
    h("header", {}, h("h2", {}, title), h("button", { class: "small", onclick: () => { openPart = null; renderDrawer(); } }, "Close")),
    powered() ? h("p", { class: "lock" }, "The board is powered: power off to change what is plugged in.") : null,
    h("fieldset", { style: "border:0;padding:0;margin:0", disabled: powered() }, ...body),
  ];
}

const PANELS = {
  sd() {
    const sd = state.sd;
    const group = "sd-mode";
    return drawerFrame("microSD card",
      h("div", { class: "field" }, h("span", { class: "label" }, "What is on the card"),
        radio(group, "official", sd.mode === "official", "Official firmware", "start4.elf, fixup4.dat, the device tree and kernel from raspberrypi/firmware on GitHub", () => { sd.mode = "official"; changed(); renderDrawer(); }),
        radio(group, "github", sd.mode === "github", "Boot files from a GitHub directory", "Any repository: its files become the card's root", () => { sd.mode = "github"; changed(); renderDrawer(); }),
        radio(group, "files", sd.mode === "files", "Boot files from my computer", "Your own firmware: pick a folder, or the files themselves", () => { sd.mode = "files"; changed(); renderDrawer(); }),
        radio(group, "image", sd.mode === "image", "A disk image", "A card image, read as the guest asks for blocks", () => { sd.mode = "image"; changed(); renderDrawer(); })),
      sd.mode === "github" ? h("div", { class: "field" },
        h("label", {}, "Repository"), h("input", { type: "text", value: sd.github.repo, oninput: (e) => { sd.github.repo = e.target.value.trim(); changed(); } }),
        h("div", { class: "row" },
          h("div", { style: "flex:1" }, h("label", {}, "Branch or tag"), h("input", { type: "text", value: sd.github.ref, oninput: (e) => { sd.github.ref = e.target.value.trim(); changed(); } })),
          h("div", { style: "flex:1" }, h("label", {}, "Directory"), h("input", { type: "text", value: sd.github.path, oninput: (e) => { sd.github.path = e.target.value.trim(); changed(); } }))),
        h("p", { class: "help hint" }, "The files directly in that directory. Listed through the GitHub API, which allows 60 requests an hour.")) : null,
      sd.mode === "files" ? h("div", { class: "field" },
        h("div", { class: "row" },
          filePicker("Choose a folder…", { directory: true, onPick: (list) => { files.boot = list.map((f) => ({ path: f.webkitRelativePath ? f.webkitRelativePath.split("/").slice(1).join("/") : f.name, file: f })); renderDrawer(); } }),
          filePicker("Choose files…", { multiple: true, onPick: (list) => { files.boot = list.map((f) => ({ path: f.name, file: f })); renderDrawer(); } })),
        files.boot.length
          ? h("p", { class: "filename" }, `${files.boot.length} files: ${files.boot.slice(0, 8).map((f) => f.path).join(", ")}${files.boot.length > 8 ? ", …" : ""}`)
          : h("p", { class: "filename" }, "nothing chosen"),
        h("p", { class: "help hint" }, "Read into memory when the board is powered on. Needs start4.elf, fixup4.dat, a kernel and its device tree.")) : null,
      sd.mode === "image" ? imagePicker("sd", sd.image, "a card image") : null,
      sd.mode !== "image" ? [
        h("h3", {}, "config.txt and cmdline.txt"),
        h("div", { class: "field" },
          h("label", {}, "Lines added to config.txt"),
          h("textarea", { rows: 5, spellcheck: false, oninput: (e) => { state.configTxt = e.target.value; changed(); } }, state.configTxt),
          h("p", { class: "help" }, "Added under [all], after whatever config.txt the files carry. enable_uart=1 is what makes the firmware print on the serial console; uart_2ndstage=1 adds start4.elf's own log.")),
        h("div", { class: "field" },
          h("label", {}, "Kernel command line (cmdline.txt)"),
          h("input", { type: "text", value: state.cmdline, spellcheck: false, oninput: (e) => { state.cmdline = e.target.value; changed(); } })),
      ] : null);
  },

  eeprom() {
    const eeprom = state.eeprom;
    const body = h("div", {});
    const form = drawerFrame("Boot EEPROM", body);
    body.append(...clean([
      h("div", { class: "field" }, h("span", { class: "label" }, "Bootloader image"),
        radio("eeprom-kind", "official", eeprom.kind === "official", "Official pieeprom-2026-08-04", "From raspberrypi/rpi-eeprom on GitHub", () => { eeprom.kind = "official"; eeprom.url = OFFICIAL_EEPROM; changed(); renderDrawer(); }),
        radio("eeprom-kind", "url", eeprom.kind === "url", "An image from a URL", null, () => { eeprom.kind = "url"; changed(); renderDrawer(); }),
        radio("eeprom-kind", "file", eeprom.kind === "file", "An image from my computer", "A pieeprom.bin, signed or not", () => { eeprom.kind = "file"; changed(); renderDrawer(); })),
      eeprom.kind === "url" ? h("div", { class: "field" }, h("input", { type: "url", value: eeprom.url, "aria-label": "EEPROM image URL", onchange: (e) => { eeprom.url = e.target.value.trim(); changed(); renderDrawer(); } })) : null,
      eeprom.kind === "file" ? h("div", { class: "field" }, filePicker("Choose pieeprom.bin…", { accept: ".bin", onPick: ([f]) => { files.eeprom = f; renderDrawer(); }, current: files.eeprom ? `${files.eeprom.name} (${mib(files.eeprom.size)})` : "nothing chosen" })) : null,
      h("p", { class: "hint", id: "eeprom-loading" }, "Reading the image’s settings…"),
    ]));
    eepromInfo().then((info) => fillBootconf(body, info), (e) => {
      const el = $("#eeprom-loading");
      if (el) el.textContent = `Could not read the image: ${e.message ?? e}`;
    });
    return form;
  },

  usb() {
    return drawerFrame("USB 3.0 socket (blue)",
      h("div", { class: "field" }, h("span", { class: "label" }, "Stick"),
        radio("usb-none", "none", state.usb.kind === "none", "Nothing plugged in", null, () => { state.usb.kind = "none"; changed(); renderDrawer(); }),
        radio("usb-none", "file", state.usb.kind !== "none", "A USB stick (disk image)", "Boots when the boot order includes USB mass storage; with a stick the order defaults to SD card, then USB", () => { state.usb.kind = state.usb.kind === "none" ? "file" : state.usb.kind; changed(); renderDrawer(); })),
      state.usb.kind !== "none" ? [
        imagePicker("usb", state.usb, "a disk image"),
        h("div", { class: "field" }, h("label", {}, "Stick size (MiB, optional)"), h("input", { type: "number", min: 1, value: state.usb.mb, oninput: (e) => { state.usb.mb = e.target.value; changed(); } }),
          h("p", { class: "help" }, "Bigger than the image: the rest reads as zeros, as on a stick the image was written to the start of.")),
      ] : null);
  },

  usbc() {
    const on = powered();
    const power = h("div", { class: "field" }, h("span", { class: "label" }, "Power"),
      h("button", { class: "primary", type: "button", "data-on": on, onclick: () => (on ? powerOff() : powerOn()) }, on ? "Unplug the power" : "Plug in the power"),
      h("p", { class: "help" }, "The board starts the moment power arrives: the boot ROM reads the EEPROM, the bootloader starts start4.elf, which starts the ARM cores."));
    const frame = drawerFrame("USB-C",
      h("h3", {}, "A stick on the USB-C port (OTG)"),
      h("p", { class: "hint" }, "The BCM2711’s own USB controller. Boot order digit 5 boots from it; Linux gets it when the firmware booted from it, or with otg_mode=1 in config.txt."),
      h("div", { class: "field" },
        radio("otg-none", "none", state.otg.kind === "none", "Nothing", null, () => { state.otg.kind = "none"; changed(); renderDrawer(); }),
        radio("otg-none", "file", state.otg.kind !== "none", "A USB stick", null, () => { state.otg.kind = state.otg.kind === "none" ? "file" : state.otg.kind; changed(); renderDrawer(); })),
      state.otg.kind !== "none" ? [
        imagePicker("otg", state.otg, "a disk image"),
        h("label", { class: "choice" }, h("input", { type: "checkbox", checked: state.otg.dock, onchange: (e) => { state.otg.dock = e.target.checked; changed(); } }), h("span", {}, "Behind a USB-C dock", h("small", {}, "Two hubs, an Ethernet adapter and an empty card reader besides the stick"))),
      ] : null);
    // The power button is not part of what is locked while the board runs.
    frame.splice(frame.length - 1, 0, power);
    return frame;
  },

  eth() {
    const wake = h("div", { class: "field" }, h("span", { class: "label" }, "Wake-on-LAN"),
      h("button", { class: "small", type: "button", disabled: !worker || phase !== "running", onclick: () => worker?.postMessage({ type: "wake-on-lan" }) }, "Send a magic packet"),
      h("p", { class: "help" }, "Works while the board runs, and wakes it when the OS armed it."));
    const frame = drawerFrame("Ethernet",
      h("label", { class: "choice" }, h("input", { type: "checkbox", checked: state.net, onchange: (e) => { state.net = e.target.checked; changed(); renderDrawer(); } }),
        h("span", {}, "Plug the cable into the built-in network peer", h("small", {}, "It answers DHCP and DNS, and serves the files below over TFTP and HTTP. It never reaches the real internet."))),
      state.net ? [
        h("div", { class: "field" }, h("span", { class: "label" }, "Files to serve (TFTP and HTTP)"),
          h("div", { class: "row" },
            filePicker("Choose a folder…", { directory: true, onPick: (list) => { files.net = list.map((f) => ({ path: f.webkitRelativePath.split("/").slice(1).join("/"), file: f })); renderDrawer(); } }),
            filePicker("Choose files…", { multiple: true, onPick: (list) => { files.net = list.map((f) => ({ path: f.name, file: f })); renderDrawer(); } })),
          h("p", { class: "filename" }, files.net.length ? `${files.net.length} files` : "nothing chosen"),
          h("p", { class: "help" }, "For a network boot, set the boot order to Network (or HTTP, with HTTP_HOST) in the EEPROM settings.")),
      ] : null);
    // Waking the board only makes sense while it runs, so it is not part of what is locked.
    if (state.net) frame.splice(frame.length - 1, 0, wake);
    return frame;
  },

  hdmi() {
    return drawerFrame("HDMI 0",
      h("label", { class: "choice" }, h("input", { type: "checkbox", checked: state.display, onchange: (e) => { state.display = e.target.checked; changed(); renderDrawer(); } }),
        h("span", {}, "Plug in a monitor", h("small", {}, "HOTPLUG reports connected and a built-in EDID answers on the DDC bus. There is no picture: the display output is not rendered."))),
      state.display ? h("div", { class: "field" }, h("span", { class: "label" }, "EDID (128 or 256 bytes, optional)"),
        filePicker("Choose an EDID…", { onPick: ([f]) => { files.edid = f; renderDrawer(); }, current: files.edid?.name ?? "the built-in one" })) : null);
  },

  gpio() {
    return drawerFrame("GPIO header",
      h("div", { class: "field" }, h("span", { class: "label" }, "HAT"),
        filePicker("Choose a HAT ID EEPROM…", { onPick: ([f]) => { files.hat = f; renderDrawer(); }, current: files.hat ? files.hat.name : "no HAT" }),
        files.hat ? h("button", { class: "small", type: "button", onclick: () => { files.hat = null; renderDrawer(); } }, "Remove the HAT") : null,
        h("p", { class: "help" }, "The ID EEPROM image (`eepmake` output) sits at 0x50 on I²C0. The firmware reads it where it probes the header and applies the device-tree overlay in it.")));
  },

  soc() {
    const b = state.board;
    const a = state.advanced;
    return drawerFrame("BCM2711 and the board",
      h("div", { class: "field" }, h("label", {}, "Silicon"),
        h("select", { onchange: (e) => { b.stepping = e.target.value; changed(); } },
          h("option", { value: "c0", selected: b.stepping === "c0" }, "C0 – current boards"),
          h("option", { value: "b0", selected: b.stepping === "b0" }, "B0 – early boards (rev 1.1, 1.2)"))),
      h("div", { class: "field" }, h("label", {}, "Memory"),
        h("select", { onchange: (e) => { b.memoryMb = Number(e.target.value); changed(); } },
          h("option", { value: 1024, selected: b.memoryMb === 1024 }, "1 GiB"),
          h("option", { value: 2048, selected: b.memoryMb === 2048 }, "2 GiB")),
        h("p", { class: "help" }, "The firmware trains and publishes this size. A tab cannot be given more than 2 GiB of WebAssembly memory to count on, so 4 and 8 GiB boards are out.")),
      h("details", {}, h("summary", {}, "Advanced"),
        h("div", { class: "field" }, h("label", {}, "Board revision code (hex, overrides the two above)"), h("input", { type: "text", placeholder: "a03114", value: b.boardRev, oninput: (e) => { b.boardRev = e.target.value; changed(); } })),
        h("div", { class: "field" }, h("label", {}, "OTP fuses to program before boot"), h("textarea", { rows: 3, spellcheck: false, placeholder: "ROW=VALUE, one a line, e.g. 17=0x3000000", oninput: (e) => { state.otpRows = e.target.value; changed(); } }, state.otpRows),
          h("p", { class: "help" }, "Rows the board would have fused by its owner, such as the device key in 56–63. Row 30 is the revision code.")),
        h("div", { class: "field" }, h("label", {}, "Jitter seed"), h("input", { type: "number", value: a.jitter, oninput: (e) => { a.jitter = e.target.value; changed(); } }),
          h("p", { class: "help" }, "Stretches the intervals the model takes exactly (a card block, a link training) by a factor drawn from this seed: reproducible, but not even.")),
        h("div", { class: "field" }, h("label", {}, "Faults: one chance in"), h("input", { type: "number", value: a.faults, oninput: (e) => { a.faults = e.target.value; changed(); } }),
          h("p", { class: "help" }, "With a jitter seed, let about one chance in this many go wrong the way a board does: a frame that never arrives.")),
        checkbox("Report stale cache reads (coherency log)", a.checkCoherency, (v) => (a.checkCoherency = v)),
        checkbox("Report misaligned VPU accesses (alignment log)", a.checkAlignment, (v) => (a.checkAlignment = v)),
        checkbox("Step over instructions the decoder lacks", a.skipUnimpl, (v) => (a.skipUnimpl = v), "Instead of stopping the run.")));
  },
};

function checkbox(label, checked, set, help) {
  return h("label", { class: "choice" }, h("input", { type: "checkbox", checked, onchange: (e) => { set(e.target.checked); changed(); } }), h("span", {}, label, help ? h("small", {}, help) : null));
}

// The EEPROM form: boot order, then every setting the bootloader reads.
function fillBootconf(body, info) {
  $("#eeprom-loading")?.remove();
  const image = info.settings;
  const edits = state.bootconf;
  const sections = conditionalSections(info.entries);

  const orderBox = h("div", { class: "field" });
  const renderOrder = () => {
    const steps = state.bootOrder;
    orderBox.replaceChildren(...clean([
      h("span", { class: "label" }, "Boot order"),
      h("p", { class: "help" }, "Tried from the top. The bootloader falls to the next step when one fails; Restart starts over."),
      h("select", { "aria-label": "Boot order preset", onchange: (e) => { state.bootOrder = e.target.value === "default" ? null : [...BOOT_ORDER_PRESETS[Number(e.target.value)][1]]; changed(); renderOrder(); } },
        h("option", { value: "default", selected: !steps }, `Bootloader default${image.has("BOOT_ORDER") ? ` (${image.get("BOOT_ORDER")} in the image)` : ""}`),
        BOOT_ORDER_PRESETS.map(([label, preset], i) => h("option", { value: i, selected: steps && steps.join() === preset.join() }, label)),
        steps && !BOOT_ORDER_PRESETS.some(([, p]) => p.join() === steps.join()) ? h("option", { value: "custom", selected: true }, "Custom") : null),
      steps ? h("div", { class: "steps" },
        steps.map((code, i) => h("div", { class: "step" },
          h("span", { class: "n" }, `${i + 1}.`),
          h("select", { "aria-label": `Boot step ${i + 1}`, onchange: (e) => { steps[i] = e.target.value; changed(); renderOrder(); } },
            BOOT_DEVICES.map(([digit, name]) => h("option", { value: digit, selected: digit === code }, name))),
          h("span", { class: "row" },
            h("button", { class: "small", type: "button", disabled: i === 0, "aria-label": "Move up", onclick: () => { [steps[i - 1], steps[i]] = [steps[i], steps[i - 1]]; changed(); renderOrder(); } }, "↑"),
            h("button", { class: "small", type: "button", disabled: steps.length === 1, "aria-label": "Remove step", onclick: () => { steps.splice(i, 1); changed(); renderOrder(); } }, "✕")))),
        steps.length < 8 ? h("div", {}, h("button", { class: "small", type: "button", onclick: () => { steps.splice(steps.length - 1 > 0 ? steps.length - 1 : 0, 0, "1"); changed(); renderOrder(); } }, "Add a step")) : null,
        h("p", { class: "help" }, `BOOT_ORDER=${formatBootOrder(steps)}`)) : null,
    ]));
  };
  state.bootOrder = state.bootOrder && state.bootOrder.every((d) => parseBootOrder(d)) ? state.bootOrder : null;
  renderOrder();

  const groups = GROUPS.map((group) =>
    h("details", {}, h("summary", {}, group.title),
      group.settings.map((setting) => settingRow(setting, image, edits))));

  const other = [...image.keys()].filter((k) => !KNOWN_KEYS.has(k));
  body.append(...clean([
    orderBox,
    h("div", { class: "field" },
      checkbox("Skip the signature check of boot.img", state.skipSignedBoot, (v) => (state.skipSignedBoot = v), "Turns SIGNED_BOOT off: the check is about half a billion interpreted instructions, which is slow here."),
      checkbox("Tryboot", state.tryboot, (v) => (state.tryboot = v), "As `reboot '0 tryboot'` does: the bootloader reads tryboot.txt in place of config.txt.")),
    h("div", { class: "field" }, h("span", { class: "label" }, "Public key for signed boot (264 bytes)"),
      filePicker("Choose pubkey.bin…", { onPick: ([f]) => { files.pubkey = f; renderDrawer(); }, current: files.pubkey?.name ?? "the image's own" })),
    h("h3", {}, "Settings"),
    h("p", { class: "hint" }, "Values come from the image; where it says nothing, they are Raspberry Pi’s documented defaults. Only what you change is added to the image’s bootconf.txt."),
    groups,
    h("div", { class: "field" }, h("label", {}, "Other lines"),
      h("textarea", { rows: 3, spellcheck: false, placeholder: "KEY=VALUE, one a line", oninput: (e) => { state.extraBootconf = e.target.value; changed(); } }, state.extraBootconf),
      h("p", { class: "help" }, `Added last, so they win. ${other.length ? `The image also sets ${other.join(", ")}. ` : ""}${sections.length ? `The image has conditional sections too (${sections.map((s) => `[${s}]`).join(" ")}); they stay as they are.` : ""}`)),
  ]));
}

function settingRow(setting, image, edits) {
  const base = image.get(setting.key) ?? setting.default ?? "";
  const current = () => edits[setting.key] ?? base;
  const row = h("div", { class: "setting" });
  const control = () => {
    if (setting.type === "select") {
      const known = setting.options.some(([value]) => value === current());
      return h("select", { "aria-label": setting.label, onchange: (e) => set(e.target.value) },
        current() === "" ? h("option", { value: "", selected: true }, "(not set)") : null,
        !known && current() !== "" ? h("option", { value: current(), selected: true }, current()) : null,
        setting.options.map(([value, label]) => h("option", { value, selected: value === current() }, label)));
    }
    return h("input", { type: setting.type === "int" ? "number" : "text", "aria-label": setting.label, value: current(), placeholder: base === "" ? "not set" : base, spellcheck: false, oninput: (e) => set(e.target.value, false) });
  };
  const set = (value, rerender = true) => {
    if (value === base) delete edits[setting.key];
    else edits[setting.key] = value;
    changed();
    row.classList.toggle("changed", setting.key in edits);
    if (rerender) paint();
  };
  const paint = () => {
    row.classList.toggle("changed", setting.key in edits);
    row.replaceChildren(...clean([
      h("div", { class: "name" }, setting.label, h("code", {}, setting.key)),
      h("div", { class: "ctl" }, control(),
        setting.key in edits ? h("button", { class: "small", type: "button", title: `Back to ${base === "" ? "not set" : base}`, "aria-label": `Reset ${setting.label}`, onclick: () => { delete edits[setting.key]; changed(); paint(); } }, "↺") : null),
      setting.help ? h("p", { class: "help" }, setting.help + (image.has(setting.key) ? "" : base === "" ? "" : " (documented default)")) : null,
    ]));
  };
  paint();
  return row;
}

const wide = matchMedia("(min-width: 980px)");
wide.addEventListener("change", () => renderDrawer());

function renderDrawer() {
  refreshBoard();
  const drawer = $("#drawer");
  for (const part of document.querySelectorAll(".part")) part.dataset.open = part.dataset.part === openPart;
  if (!openPart) {
    drawer.hidden = !wide.matches;
    drawer.replaceChildren(h("div", { class: "drawer-body empty" }, ...(powered()
      ? [
          h("h2", {}, "The board is powered"),
          h("p", {}, "Click a part of the board to see what is plugged into it. The parts are locked while the board runs: switch the power off to change them."),
          h("p", { class: "hint" }, "The Power switch at the top of the board switches it off."),
        ]
      : [
          h("h2", {}, "Plug something in"),
          h("p", {}, "Click a part of the board to change what is plugged into it: the microSD slot, the EEPROM, the USB sockets, the Ethernet jack, the monitor, the GPIO header, or the processor for the board itself."),
          h("p", { class: "hint" }, "The Power switch at the top of the board starts it."),
        ])));
    return;
  }
  drawer.hidden = false;
  drawer.replaceChildren(h("div", { class: "drawer-body" }, ...PANELS[openPart]().filter(Boolean)));
}

const short = (text, n = 18) => (text.length > n ? `${text.slice(0, n - 1)}…` : text);

function tail(url) {
  try {
    const u = new URL(url);
    return u.pathname.split("/").filter(Boolean).pop() ?? u.host;
  } catch {
    return url;
  }
}

function describeImage(spec, file) {
  if (spec.kind === "file") return file ? file.name : "no file";
  return spec.url.trim() ? tail(spec.url.trim()) : "no URL";
}

function refreshBoard() {
  const s = state;
  const sd = {
    official: ["official firmware", true],
    github: [s.sd.github.repo, true],
    files: [files.boot.length ? `${files.boot.length} files` : "no files", files.boot.length > 0],
    image: [describeImage(s.sd.image, files.sdImage), s.sd.image.kind === "url" ? !!s.sd.image.url.trim() : !!files.sdImage],
  }[s.sd.mode];
  const edits = Object.keys(s.bootconf).length + (s.bootOrder ? 1 : 0) + s.extraBootconf.split("\n").filter((l) => l.trim()).length;
  const eepromName = s.eeprom.kind === "official" ? "official 2026-08-04" : s.eeprom.kind === "file" ? (files.eeprom?.name ?? "no file") : tail(s.eeprom.url);
  const parts = {
    sd: { sub: sd[0], filled: sd[1] },
    eeprom: { sub: edits ? `${short(eepromName, 12)} +${edits}` : eepromName, filled: true },
    memory: { sub: `${s.board.memoryMb / 1024} GiB${s.board.boardRev.trim() ? ` · ${s.board.boardRev.trim()}` : ""}` },
    usb: { sub: s.usb.kind === "none" ? "empty" : describeImage(s.usb, files.usb), filled: s.usb.kind !== "none" },
    usbc: { sub: s.otg.kind === "none" ? "empty" : `${describeImage(s.otg, files.otg)}${s.otg.dock ? " · dock" : ""}`, filled: s.otg.kind !== "none" },
    eth: { sub: s.net ? (files.net.length ? `peer · ${files.net.length} files` : "built-in peer") : "unplugged", filled: s.net },
    hdmi: { sub: s.display ? "monitor" : "no monitor", filled: s.display },
    gpio: { sub: files.hat ? files.hat.name : "no HAT", filled: !!files.hat },
  };
  for (const [key, { sub, filled }] of Object.entries(parts)) {
    const text = $(`[data-sub="${key}"]`);
    text.textContent = short(sub, 19);
    if (filled !== undefined) text.closest(".part").dataset.filled = filled;
  }
  $("#soc-title").textContent = `BCM2711 ${s.board.stepping.toUpperCase()}`;
  $("#eth-led").dataset.on = s.net && powered();
}

for (const part of document.querySelectorAll(".part")) {
  const choose = () => {
    if (!PANELS[part.dataset.part]) return;
    openPart = openPart === part.dataset.part ? null : part.dataset.part;
    renderDrawer();
  };
  part.addEventListener("click", choose);
  part.addEventListener("keydown", (e) => {
    if (e.key === "Enter" || e.key === " ") {
      e.preventDefault();
      choose();
    }
  });
}

const plug = $("#power-plug");
const togglePower = () => (powered() ? powerOff() : powerOn());
plug.addEventListener("click", togglePower);
plug.addEventListener("keydown", (e) => {
  if (e.key === "Enter" || e.key === " ") {
    e.preventDefault();
    togglePower();
  }
});
$("#power").addEventListener("click", togglePower);
$("#pause").addEventListener("click", () => worker?.postMessage({ type: phase === "paused" ? "resume" : "pause" }));

setPhase("off");
refreshBoard();
renderInspect(null);
