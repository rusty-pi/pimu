import init, { PiBuilder } from "./pkg/pimu.js";

const FIRMWARE =
  "https://raw.githubusercontent.com/raspberrypi/firmware/refs/heads/master/boot/";
const EEPROM =
  "https://raw.githubusercontent.com/raspberrypi/rpi-eeprom/master/firmware-2711/latest/pieeprom-2026-08-04.bin";
const BOOT_FILES = ["start4.elf", "fixup4.dat", "bcm2711-rpi-4-b.dtb", "kernel8.img"];
const CONFIG_TXT = "arm_64bit=1\nenable_uart=1\nuart_2ndstage=1\ndtoverlay=disable-bt\n";
const CMDLINE_TXT = "console=ttyAMA0,115200 earlycon\n";
const SLICE_MS = 100;

let pi = null;

async function fetchBytes(url) {
  const response = await fetch(url);
  if (!response.ok) throw new Error(`${url}: ${response.status}`);
  return new Uint8Array(await response.arrayBuffer());
}

async function start() {
  postMessage({ status: "downloading firmware" });
  const [eeprom, ...files] = await Promise.all([
    fetchBytes(EEPROM),
    ...BOOT_FILES.map((name) => fetchBytes(FIRMWARE + name)),
    init(),
  ]);
  const builder = new PiBuilder(eeprom);
  BOOT_FILES.forEach((name, i) => builder.add_file(name, files[i]));
  const text = new TextEncoder();
  builder.add_file("config.txt", text.encode(CONFIG_TXT));
  builder.add_file("cmdline.txt", text.encode(CMDLINE_TXT));
  pi = builder.build();
  postMessage({ status: "running" });
  run();
}

function run() {
  const out = pi.run(SLICE_MS);
  if (out.length) postMessage({ output: out }, [out.buffer]);
  const ended = pi.ended();
  if (ended) {
    postMessage({ status: `stopped: ${ended}` });
    return;
  }
  setTimeout(run, 0);
}

onmessage = (event) => {
  if (event.data.start) {
    start().catch((e) => postMessage({ status: `failed: ${e.message ?? e}` }));
  } else if (event.data.keys && pi) {
    pi.send(event.data.keys);
  }
};
