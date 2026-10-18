import { expect, test } from "./fixtures.mjs";

const io = (rows) => ({ type: "log", text: rows.map((row) => JSON.stringify({ channel: "io", ...row })).join("\n") + "\n" });
const read = (us, lba, blocks, files, held) => ({ us, dev: "sd", op: "read", lba, blocks, files, ...(held ? { file_blocks: held } : {}) });

test.describe("the I/O tab", () => {
  test("is the first tab, and says what it waits for", async ({ app }) => {
    await expect(app.tab("io")).toHaveAttribute("aria-selected", "true");
    await expect(app.page.locator("#tab-io")).toHaveText("Power the board on to see the files it reads and the fuses.");
    await app.running();
    await expect(app.page.locator("#tab-io")).toHaveText("Nothing read yet.");
  });

  test("says so when the io channel is off", async ({ app }) => {
    await app.showTab("log");
    await app.page.locator("#tab-log details .chip-btn", { hasText: /^io$/ }).click();
    await app.showTab("io");
    await expect(app.page.locator("#tab-io")).toContainText("The io channel is off");
  });

  test("lists the files each disk read, in the order it first read them, with how much of each", async ({ app }) => {
    await app.running();
    await app.emit(io([
      read(1_000_000, 0, 1, ["(partition table)"]),
      read(1_100_000, 8192, 21, ["p1:(boot sector)", "p1:(FAT)"], [1, 20]),
      read(2_000_000, 9000, 4, ["p1:/config.txt"]),
      read(2_500_000, 9100, 4400, ["p1:/start4.elf", "p1:/fixup4.dat"], [4390, 10]),
      read(3_000_000, 20000, 2, ["p1:/overlays/"]),
      read(4_000_000, 30000, 4, ["p1:/config.txt"]),
      { us: 4_500_000, dev: "usb", op: "write", lba: 64, blocks: 8, files: [] },
    ]));
    const sd = app.page.locator(".io-disk").first();
    await expect(sd.locator("h3")).toHaveText("microSD card3 files · 2.2 MiB read");
    await expect(sd.locator("li .name")).toHaveText(["config.txt", "start4.elf", "fixup4.dat", "(partition table, boot sector, FAT, 1 directory)"]);
    await expect(sd.locator("li").nth(0).locator(".num")).toHaveText(["4.0 KiB", "2.00 s"]);
    await expect(sd.locator("li").nth(1).locator(".num")).toHaveText(["2.1 MiB", "2.50 s"]);
    await expect(sd.locator("li").nth(2).locator(".num")).toHaveText(["5.0 KiB", "2.50 s"]);
    await expect(sd.locator("li.meta")).toHaveCount(1);
    await expect(sd.locator("li").nth(0)).toHaveAttribute("title", "p1:/config.txt\nfirst read at 2.00 s, last at 4.00 s, 2 runs of blocks");
    const strip = await sd.locator("li").nth(0).locator(".strip i").evaluateAll((bins) => bins.map((b) => b.style.opacity !== "0"));
    expect(strip.filter(Boolean)).toHaveLength(2);

    const usb = app.page.locator(".io-disk").nth(1);
    await expect(usb.locator("h3")).toHaveText("USB 3.0 stick0 files · 0 B read · 4.0 KiB written");
    await expect(usb.locator("li .name")).toHaveText(["(blocks no file holds)"]);
    await expect(usb.locator(".wrote")).toHaveCount(1);
  });

  test("a run without the blocks of each file shares them out evenly", async ({ app }) => {
    await app.running();
    await app.emit(io([read(1, 0, 4, ["p1:/a", "p1:/b"])]));
    await expect(app.page.locator(".io-disk li .num:not(.when)")).toHaveText(["1.0 KiB", "1.0 KiB"]);
  });

  test("a file opens the log, filtered to it", async ({ app }) => {
    await app.running();
    await app.emit(io([read(1, 9000, 4, ["p1:/config.txt"]), read(2, 9100, 8, ["p1:/start4.elf"])]));
    await app.page.locator(".io-disk li .name", { hasText: "config.txt" }).click();
    await expect(app.page.locator("#tab-log")).toBeVisible();
    await expect(app.page.getByRole("searchbox", { name: "Filter log lines" })).toHaveValue("config.txt");
    await expect(app.page.locator(".logrow")).toHaveText(["0.000001iomicroSD card: read 4 blocks at 9000 – config.txt"]);
  });

  test("shows the fuses that say something, and folds away the blank and undocumented ones", async ({ app }) => {
    await app.running();
    await app.emit(io([
      { us: 1, dev: "otp", op: "read", row: 17, value: "0x000008b0", meaning: "boot mode" },
      { us: 2, dev: "otp", op: "read", row: 17, value: "0x000008b0", meaning: "boot mode" },
      { us: 3, dev: "otp", op: "read", row: 2, value: "0xfa1e0002", meaning: "not documented" },
      { us: 4, dev: "otp", op: "read", row: 45, value: "0x00000000", meaning: "codec licence" },
      { us: 5, dev: "otp", op: "write", row: 36, value: "0x11111111", was: "0x00000000", meaning: "customer OTP, word 1 of 8" },
    ]));
    const section = app.page.locator(".io section", { has: app.page.locator("h3", { hasText: "Fuses" }) });
    await expect(section.locator("h3")).toHaveText("Fuses (OTP)4 rows read · 1 programmed");
    await expect(section.locator(":scope > table tr")).toHaveText(["row 170x000008b0boot mode", "row 360x11111111 was 0x00000000customer OTP, word 1 of 8"]);
    await expect(section.locator("tr.wrote")).toHaveCount(1);
    await expect(section.locator("details summary")).toHaveText("2 more rows: 1 blank, 1 undocumented");
    await section.locator("details summary").click();
    await expect(section.locator("details tr")).toHaveText(["row 20xfa1e0002not documented", "row 450x00000000codec licence"]);
  });

  test("follows new lines while it is open, and starts over at the next power-on", async ({ app }) => {
    await app.running();
    await app.emit(io([read(1, 9000, 4, ["p1:/config.txt"])]));
    await expect(app.page.locator(".io-disk li")).toHaveCount(1);
    await app.emit(io([read(2, 9100, 8, ["p1:/start4.elf"])]));
    await expect(app.page.locator(".io-disk li")).toHaveCount(2);
    await app.powerButton.click();
    await app.powerOn();
    await expect(app.page.locator(".io-disk")).toHaveCount(0);
  });
});

test.describe("the Network tab", () => {
  test("says why it is empty", async ({ app }) => {
    await app.showTab("net");
    await expect(app.page.locator("#tab-net")).toContainText("The Ethernet cable is unplugged");
    await app.open("eth");
    await app.drawer.getByRole("checkbox", { name: "Plug the cable in" }).check();
    await expect(app.page.locator("#tab-net")).toHaveText("Power the board on to see what it asks the network for.");
    await app.running();
    await expect(app.page.locator("#tab-net")).toHaveText("Nothing on the network yet.");
    await app.powerButton.click();
    await app.showTab("log");
    await app.page.locator("#tab-log details .chip-btn", { hasText: /^io$/ }).click();
    await app.showTab("net");
    await expect(app.page.locator("#tab-net")).toContainText("The io channel is off");
  });

  test("lists what the network peer did, by protocol, with what came of it", async ({ app }) => {
    await app.running();
    await app.showTab("net");
    await app.emit(io([
      { us: 1_200_000, dev: "net", event: "dhcp: DISCOVER from aa:bb -> OFFER 192.168.1.2" },
      { us: 1_300_000, dev: "net", event: "http: GET /boot.img -> 200, 100 bytes" },
      { us: 1_400_000, dev: "net", event: "http: GET /boot.sig -> 404" },
      { us: 1_500_000, dev: "net", event: "something odd" },
      { us: 1_600_000, dev: "net", event: "http: incomplete request, connection reset" },
    ]));
    await expect(app.page.locator(".io-net li")).toHaveText([
      "1.20 sdhcpDISCOVER from aa:bb → OFFER 192.168.1.2",
      "1.30 shttpGET /boot.img → 200, 100 bytes",
      "1.40 shttpGET /boot.sig → 404",
      "1.50 ssomething odd",
      "1.60 shttpincomplete request, connection reset",
    ]);
    await expect(app.page.locator(".io-net li.missed")).toHaveText(["1.40 shttpGET /boot.sig → 404"]);
    await expect(app.page.locator(".io-net li.bad")).toHaveText(["1.60 shttpincomplete request, connection reset"]);
    await expect(app.tab("net")).toHaveText("Network (5)");
  });
});

test("io lines read as sentences in the log", async ({ app }) => {
  await app.running();
  await app.showTab("log");
  await app.emit(io([
    { us: 1, dev: "otp", op: "read", row: 17, value: "0x000008b0", meaning: "boot mode" },
    { us: 2, dev: "otp", op: "write", row: 36, value: "0x11111111", was: "0x00000000", meaning: "" },
    { us: 3, dev: "net", event: "tftp: RRQ start4.elf -> 2253280 bytes" },
    { us: 4, dev: "usb", op: "write", lba: 64, blocks: 1, files: [] },
    { us: 5, dev: "mystery", what: "else" },
  ]));
  await expect(app.page.locator(".logrow span:last-child")).toHaveText([
    "OTP row 17 read 0x000008b0 · boot mode",
    "OTP row 36 programmed to 0x11111111 (was 0x00000000)",
    "tftp: RRQ start4.elf -> 2253280 bytes",
    "USB 3.0 stick: write 1 block at 64",
    "dev=mystery  what=else",
  ]);
});
