// Stands in for pkg/pimu.js, the WebAssembly build, when the real worker.mjs is
// tested: it does what the builder and the machine are asked and reports it,
// and runs nothing. Every call reaches the page as a { type: "stub" } message.

const record = (...call) => postMessage({ type: "stub", call });

export default async function init() {
  record("init");
}

export class BootImage {
  constructor() {
    this.files = [];
  }
  add_file(path, bytes) { this.files.push([path, bytes.length]); }
  build() {
    record("boot_image", this.files);
    return new Uint8Array(1024);
  }
}

export class PiBuilder {
  constructor(options, eeprom) {
    this.options = JSON.parse(options);
    record("builder", this.options, eeprom.length);
  }
  add_file(path, bytes) { record("add_file", path, bytes.length); }
  add_tftp_file(path, bytes) { record("add_tftp_file", path, bytes.length); }
  add_http_file(path, bytes) { record("add_http_file", path, bytes.length); }
  set_pubkey(bytes) { record("set_pubkey", bytes.length); }
  set_hat(bytes) { record("set_hat", bytes.length); }
  set_edid(bytes) { record("set_edid", bytes.length); }
  attach_image(slot, image) {
    const head = Array.from(image.read(0, 8));
    const tail = Array.from(image.read(image.size - 4, 4));
    record("attach_image", slot, image.size, head, tail);
  }
  build() {
    return new Pi(this.options.ticks ?? 3);
  }
}

class Pi {
  constructor(ticks) {
    this.ticks = ticks;
    this.n = 0;
  }
  run() {
    this.n += 1;
    return new TextEncoder().encode(`tick ${this.n}\n`);
  }
  take_log() {
    return this.n === 1 ? '{"us":1,"channel":"io","msg":"hello"}\n' : "";
  }
  status() {
    const ended = this.n >= this.ticks;
    return JSON.stringify({
      ended, ok: ended ? true : null, why: ended ? "done" : null, reboots: 0, armReleased: false,
      modelUs: this.n * 1000, retired: this.n, logDropped: 0,
      cores: { arm: Array(4).fill({ state: "off", insns: 0 }), vpu: [{ state: "running", insns: this.n }, { state: "off", insns: 0 }] },
    });
  }
  send(bytes) { record("send", Array.from(bytes)); }
  send_magic_packet() { record("send_magic_packet"); return true; }
  device_tree() { return "/dts-v1/;"; }
  device_tree_blob() { return new Uint8Array([1, 2, 3]); }
  eeprom() { return new Uint8Array([9, 9]); }
  bootconf() { return ["[all] A=1"]; }
  otp() { return "[]"; }
}

export function eeprom_bootconf(bytes) {
  record("eeprom_bootconf", bytes.length);
  return JSON.stringify([["all", "BOOT_UART", "0"]]);
}
