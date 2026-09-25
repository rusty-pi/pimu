<!-- generated from specs/pm.toml by `cargo run -- spec-docs --update` – do not edit -->

# `pm` – Power management: reset control, reset status, watchdog, power-domain registers

- Bus: `vpu` (VPU bus address)
- Base: `0x7E100000`
- Size: `0x1000`

Writes carry the `0x5A` password in the top byte, which reads back masked. The watchdog counts down at 65536 Hz from the last `WDOG` value once `RSTC` arms a full reset, and a later `WDOG` write reloads the countdown without touching `RSTC` — `WDOG` is the counter, not a shadow of it.

That reload is the whole of the firmware's **boot watchdog**, which `bootconf.txt`'s `BOOT_WATCHDOG_TIMEOUT` (seconds) turns on and `BOOT_WATCHDOG_PARTITION` aims: a countdown that covers the entire boot, so a board that hangs anywhere between the bootcode and the kernel resets instead of sitting there. The budget lives in software, as a deadline on the 1 MHz system timer that the bootcode writes into the `BPWR` block of the bootloader state (`+124` the boot's start and `+132` the deadline, 64-bit microseconds each; `+140` the timeout in seconds, `+144` the partition). The hardware is only ever told how much of it is left:

1. the bootcode arms it once — `WDOG = PASSWORD | min((end_at - now) >> 4, 0xFFFFF)` then `RSTC = PASSWORD | 0x20` (`0x80007980` / `0x800079EC`), and prints `Boot watchdog init. max duration %us part %u started_at %u end_at %u`;
2. `bootmain` and `start4.elf` feed it with bare `WDOG` writes and never touch `RSTC` (`0x000A75E4`, `0x3ED47B98`);
3. whoever finishes with it puts `RSTS`'s partition field back to the one the board actually booted, reports `boot watchdog stop: remaining %u` (microseconds, `(WDOG & 0xFFFFF) << 4`) and clears `WRCFG`.

Because of (2), a model that only reloaded the countdown on an `RSTC` write reset every such boot 16 s in, whatever the budget was.

Sources:

- linux (high): `drivers/watchdog/bcm2835_wdt.c` and `drivers/pmdomain/bcm/bcm2835-power.c` map this block
- decompile (high): bootloader self-update reset (`WDOG = PASSWORD | 10`, `RSTC = PASSWORD | full reset`); start4 arms the dog at `0x3ED62334` / `0x3ED62342` with `dtparam=watchdog=on`
- trace (high): `--trace-mmio` of the pinned firmware (`pieeprom.bin` and `start4.elf` as `firmware.sha256` lists them) booted through the ARM release; every PC cited below is in those images

## Register map

| Offset | Name | Access | Width | Sources |
|---|---|---|---|---|
| `0x01C` | [`RSTC`](#rstc) | rw | 32 | 3, best high |
| `0x020` | [`RSTS`](#rsts) | rw | 32 | 6, best high |
| `0x024` | [`WDOG`](#wdog) | rw | 32 | 3, best high |
| `0x028` | [`PADS0`](#pads0) | rw | 32 | 2, best high |
| `0x02C` | [`PADS2`](#pads2) | rw | 32 | 4, best high |
| `0x030` | [`PADS3`](#pads3) | rw | 32 | 3, best high |
| `0x034` | [`PADS4`](#pads4) | rw | 32 | 3, best high |
| `0x038` | [`PADS5`](#pads5) | rw | 32 | 2, best high |
| `0x040`–`0x05C` (8 × 0x4) | [`DOMAIN_STATUS`](#domain_status) | rw | 32 | 1, best low |
| `0x074` | [`SPAREW`](#sparew) | rw | 32 | 2, best high |
| `0x078` | [`SPARER`](#sparer) | r | 32 | 3, best high |
| `0x07C` | [`AVS_RSTDR`](#avs_rstdr) | rw | 32 | 2, best high |
| `0x080` | [`AVS_STAT`](#avs_stat) | rw | 32 | 2, best high |
| `0x084` | [`AVS_EVENT`](#avs_event) | rw | 32 | 2, best high |
| `0x088` | [`AVS_INTEN`](#avs_inten) | rw | 32 | 2, best high |
| `0x0FC` | [`REG_FC`](#reg_fc) | r | 32 | 1, best high |
| `0x108` | [`IMAGE`](#image) | rw | 32 | 3, best high |
| `0x10C` | [`GRAFX`](#grafx) | rw | 32 | 2, best high |
| `0x110` | [`PROC`](#proc) | rw | 32 | 2, best high |

## `RSTC`

Offset `0x01C` · access `rw` · 32 bits

Reset control. Arming a full reset starts the watchdog; clearing `WRCFG` stops it. As it starts, start4 ORs `0x3000` into it (bits 13:12, meaning unknown) just before the PLL power words, then does the same watchdog set-up as the bootcode: `WRCFG` cleared, bits 9:8 set to 2, bits 1:0 set to 2 (`0x3202` in the trace), and `RSTS` cleared if `HADWRF` is set. It skips the set-up when the bootloader's `BPWR` tag says a boot watchdog is running (its word at `+140`, `BOOT_WATCHDOG_TIMEOUT`), because clearing `WRCFG` would take that dog off. See the block note.

| Bits | Field | Access | Notes |
|---|---|---|---|
| 5:4 | `WRCFG` | rw | 2 = full reset when the watchdog expires. |
| 31:24 | `PASSWD` | rw | Must be `0x5A` for the write to count. |

Sources:

- linux (high): `bcm2835_wdt_start` writes `PM_PASSWORD | 0x3222` style values, `bcm2835_wdt_stop` writes `0x102`
- decompile (high): start4: `_DAT_7e10001c |= 0x5a003000` in `0x3ED55D20`; watchdog set-up `0x3ED6226E`: `(v & ~0x30)`, `(v & ~0x300) | 0x200`, `(v & ~3) | 2`, unless the `PWRB` tag's word at `+140` is non-zero
- trace (high): start4: reads `0x202` and writes `0x5A003202` at `0x3ED55D38`, then `0x5A003202` at `0x3ED6229E`, `0x3ED622AC` and `0x3ED622BC`

`WRCFG` sources:

- linux (high): `bcm2835_wdt.c`: `PM_RSTC_WRCFG_FULL_RESET = 0x20`

`PASSWD` sources:

- linux (high): `bcm2835_wdt.c`: `PM_PASSWORD = 0x5a000000`

## `RSTS`

Offset `0x020` · access `rw` · 32 bits · reset `0x20`

Which reset source fired last. The bootloader also packs the partition to boot into the even bits 0..10, which the odd `HADWRF` bit does not disturb, and a watchdog reset keeps them: that is how Linux names the partition to boot next, and partition 63 (`0x555`) is its power-off asking for a halt. The bootloader's halt (`0x800005AC`, on a 4B whenever the partition is 63) writes the password alone here, blinks the activity LED ten times, disables every interrupt source on both VPU cores and sleeps until GPIO 3 falls (`GPFEN0` bit 3, source 116, whose handler writes `0xA` to `GPEDS0`), then boots as usual. start4's watchdog set-up writes the password alone (0) here when `HADWRF` is set.

| Bits | Field | Access | Notes |
|---|---|---|---|
| 1 | `TRYBOOT` | rw | A one-shot request to boot with `tryboot.txt`: start4 sets or clears it for the `SET_REBOOT_FLAGS` property tag (value 1), and answers `GET_REBOOT_FLAGS` from it. It has to survive the watchdog reset Linux reboots with, so the model keeps it with the partition field. The bootcode reads the register twice as it starts, writes it back without this bit, logs `TRYBOOT` after `VC-JTAG`, and passes the request on in bit 0 of the `BVER` block's reset-info word, which the bootloader logs as `reset_info 00000001`. |
| 5 | `HADWRF` | rw | Last reset was a full watchdog reset: what power-on sequencing leaves. |

Sources:

- measured (high): the reference board's bootloader prints `PM_RSTS 00000020` after power-on
- trace (high): start4: reads `0x20` twice at `0x3ED622BE` / `0x3ED622C0`, writes `0x5A000000` at `0x3ED622CC`
- decompile (high): partition field in the even bits (mask `0x555`): `FUN_00000578` / `FUN_00000666` in `firmware/source/pieeprom.bin.c`
- linux (high): `bcm2835_wdt.c`: `__bcm2835_restart` ORs the partition into `RSTS` before arming a 10-tick full reset, and `bcm2835_power_off` asks for partition 63
- trace (high): `boot --send-after '/ # ' 'poweroff -f\n'` on the Linux card: after the reset the bootloader prints `partition 63`, `PM_RSTS 00000575` and `Halt: wake: 1 power_off: 0`; `PIMU_MMIO_FROM=0x800005AC` shows `RSTS <- 0x5A000000`, ten `GPSET1`/`GPCLR1` bit 10 pairs, zeros to `0x7E002010..2C` and `0x7E002810..2C`, `VBASE <- 0x80000000`, `GPFEN0 <- 8`, `+0x28 <- 0x10000`, `sleep`, and all three undone
- decompile (high): call site `0x80000DFE`: the halt runs with `WAKE_ON_GPIO` (`FCEB+0x18`) and `POWER_OFF_ON_HALT` (`+0x1C`) unless the board is a Pi 400 (`0x8000884A`) and `WAKE_ON_GPIO` is not 2; `0x80008490` powers off through the board's PMIC op `+0x78` only when wake is 0 and power-off is set; wake GPIO from `0x80000A20` (3)

`TRYBOOT` sources:

- trace (high): `--mbox-property 0x00038064:4=1`: start4 reads `0x20` twice and writes `0x5A000002` (`0x3ED622F4`), then `GET_REBOOT_FLAGS` answers 1. With `--patch 0x7E100020=0x5A000022`: the bootcode reads `0x22` at `0x80007EFE` / `0x80007F06` and writes `0x5A000020` at `0x80007F12`

`HADWRF` sources:

- linux (high): `bcm2835_wdt.c`: `PM_RSTS_HADWRF_SET`

## `WDOG`

Offset `0x024` · access `rw` · 32 bits

Watchdog timeout on write, ticks left on read (1 tick = 1/65536 s). The register is the counter itself, so writing it reloads a countdown already running — the firmware's boot watchdog is fed by bare `WDOG` writes and never re-arms `RSTC`.

| Bits | Field | Access | Notes |
|---|---|---|---|
| 19:0 | `TIME` | rw | Timeout in watchdog ticks; `0xFFFFF` is 16 s. |
| 31:24 | `PASSWD` | rw | Must be `0x5A`. |

Sources:

- linux (high): `bcm2835_wdt.c`: `PM_WDOG_TIME_SET = 0x000fffff`, `get_timeleft` reads it back
- trace (high): `--bootconf BOOT_WATCHDOG_TIMEOUT=600` on the pinned firmware: one `RSTC` arm in the bootcode (`0x800079EC`) and then nothing but `WDOG` writes — `0x000A75E4` in `bootmain` (86254 of them in one boot) and `0x3ED47B98` in start4 — all the way to `arm_loader: Starting ARM`
- decompile (high): the same feed routine in all three stages — bootcode `0x8000791E`, `bootmain` `0x000A7582`, start4 `0x0ED47B32`: `(end_at - now) >> 4` clamped to `0xFFFFF`, stored to `WDOG` alone

`TIME` sources:

- linux (high): `bcm2835_wdt.c`: `PM_WDOG_TIME_SET`

`PASSWD` sources:

- linux (high): `bcm2835_wdt.c`: `PM_PASSWORD`

## `PADS0`

Offset `0x028` · access `rw` · 32 bits

Linux's first pad register. start4's pad writer can address it (index 2), with the same doubled drive code as `PADS2` and `PADS3`; the pinned firmware does not write it on the Pi 4B.

Sources:

- linux (high): `bcm2835-power.c`: `PM_PADS0`
- decompile (high): pad writer `0x3ED55F20`: switch at `0x3ED55F6E`, index 2 writes `0x7E100028` at `0x3ED55F7E`

## `PADS2`

Offset `0x02C` · access `rw` · 32 bits · reset `0x1B`

Pad control for GPIO 0-27. start4 applies `dt-blob.bin`'s `pin_config` last pin first; for each pin it sets the function if it differs, drives an output to its startup level, writes the pull, then rewrites `PADS2`, `PADS3` and `PADS4` from the drive strengths the pins so far have named. On the Pi 4B this register stays `0x19` until GPIO 15 (8 mA) and ends on `0x1F`.

| Bits | Field | Access | Notes |
|---|---|---|---|
| 2:0 | `DRIVE` | rw | Drive strength: 0 is 2 mA up to 7 for 16 mA, with the current halved on 4-series boards. start4's pad writer writes `min(2n + 1, 7)` to `PADS0`, `PADS2` and `PADS3` when the flag at `gp+5476` is set, and n unchanged to `PADS4` and `PADS5`. With `n = mA / 2 - 1` of `dt-blob.bin`'s `drive_strength_mA`, the Pi 4B's numbers all fit: 8 mA and 16 mA give 7 in `PADS2` / `PADS3`, 14 mA gives 6 in `PADS4`, and a bank no pin names a strength for gets 1. |
| 3 | `HYST` | rw | Input hysteresis on. start4 sets it in every value it writes. |
| 4 | `SLEW` | rw | 1 turns slew rate limiting off. start4 sets it in every value it writes. |
| 31:24 | `PASSWD` | rw | Must be `0x5A`. |

Sources:

- linux (high): `drivers/pmdomain/bcm/bcm2835-power.c`: `PM_PADS2`
- datasheet (high): raspberrypi/documentation b01a5c1, `documentation/asciidoc/computers/raspberry-pi/gpio-pad-controls.adoc`: `0x7E10002C` is the pads of GPIO 0-27; `PASSWRD`, `SLEW`, `HYST`, `DRIVE` and their reset values
- trace (high): start4 `0x3ED55F88`, after each pin's function (`0x3ECC9548` / `0x3ECC9562`), level (`0x3ECC8018`) and pull (`0x3ECC8414`) writes: GPIO 46, 45 .. 40, 39 .. 34, 15, 14; again for GPIO 30 and 31 just before the board clock set-up
- decompile (high): pad writer `0x3ED55F20`: `PASSWD | (r2 & 1) << 4 | (r3 & 1) << 3 | drive`, index 3 written at `0x3ED55F88`

`DRIVE` sources:

- datasheet (high): raspberrypi/documentation b01a5c1, `documentation/asciidoc/computers/raspberry-pi/gpio-pad-controls.adoc`: drive strength list, and 'On 4-series devices, the current level is half the value shown'
- decompile (high): `0x3ED55F2E..0x3ED55F44`: for index 2..4, `r4 = min(2 * r4 + 1, 7)`
- inferred (medium): `n = mA / 2 - 1`: fitted to the trace against `pins_4b` (GPIO 15 at 8 mA, 40/41 at 16 mA, 46 at 14 mA)

`HYST` sources:

- datasheet (high): raspberrypi/documentation b01a5c1, `documentation/asciidoc/computers/raspberry-pi/gpio-pad-controls.adoc`: `HYST`

`SLEW` sources:

- datasheet (high): raspberrypi/documentation b01a5c1, `documentation/asciidoc/computers/raspberry-pi/gpio-pad-controls.adoc`: `SLEW`, 0 = slew rate limited, 1 = not limited

`PASSWD` sources:

- linux (high): `bcm2835-power.c`: `PM_PASSWORD`

## `PADS3`

Offset `0x030` · access `rw` · 32 bits · reset `0x1B`

Pad control for GPIO 28-45, laid out as `PADS2`. On the Pi 4B start4 writes `0x19` until GPIO 41 (16 mA) and `0x1F` from then on.

Sources:

- linux (high): `bcm2835-power.c`: `PM_PADS3`
- datasheet (high): raspberrypi/documentation b01a5c1, `documentation/asciidoc/computers/raspberry-pi/gpio-pad-controls.adoc`: `0x7E100030` is the pads of GPIO 28-45
- trace (high): start4 `0x3ED55F92`, with `PADS2`

## `PADS4`

Offset `0x034` · access `rw` · 32 bits · reset `0x1B`

Pad control for GPIO 46 up, laid out as `PADS2` but with the drive code written as it is. On the Pi 4B start4 writes `0x1E` from the first pin on, the 14 mA `pins_4b` gives GPIO 46 ('Dummy pin for HSTL drive on bank 2').

Sources:

- linux (high): `bcm2835-power.c`: `PM_PADS4`
- datasheet (high): raspberrypi/documentation b01a5c1, `documentation/asciidoc/computers/raspberry-pi/gpio-pad-controls.adoc`: `0x7E100034` is the pads of GPIO 46-53
- trace (high): start4 `0x3ED55F9C`, with `PADS2`

## `PADS5`

Offset `0x038` · access `rw` · 32 bits

Linux's next pad register. start4's pad writer can address it (index 6) and keeps its bit 6 when it does; the pinned firmware does not write it on the Pi 4B.

Sources:

- linux (high): `bcm2835-power.c`: `PM_PADS5`
- decompile (high): pad writer `0x3ED55F20`: index 6 reads `0x7E100038`, keeps bit 6 and writes at `0x3ED55FAE`

## `DOMAIN_STATUS`

Offset `0x040`, 8 elements 0x4 apart · access `rw` · 32 bits · reset `0x7040`

Power-domain words the firmware reads for 'powered, clocks stable'. Which domain each word is was not decoded; the model answers powered so the powered-up branch runs.

Sources:

- inferred (low): the model's choice: status reads here gate a branch the boot has to take

## `SPAREW`

Offset `0x074` · access `rw` · 32 bits

Linux's spare writable word, read back through `SPARER`. bootmain writes it late, right after it puts the PCIe bridge back into reset (`RGR1_SW_INIT_1` `0x3`) before start4 is loaded: bit 22 set and the partition it booted in the low bits, `0x400001` for the first, `0x400002` when `PARTITION=2` chose the second.

Sources:

- linux (high): `bcm2835-power.c`: `PM_SPAREW`
- trace (high): bootmain: `0x5A400001` at `0x000804AE`, `0x5A400002` with `--bootconf PARTITION=2` and a second FAT32 partition

## `SPARER`

Offset `0x078` · access `r` · 32 bits

Reads what was last written to `SPAREW`, without the password byte. This is how start4 learns the partition the bootloader booted (`0x3ECC44E8`): with bit 22 set, the low 22 bits are the partition; otherwise it decodes the partition field of the `RSTS` it saved as it started. It logs the result as `boot-part: N` and reports it as `/chosen/bootloader/partition`.

Sources:

- linux (high): `bcm2835-power.c`: `PM_SPARER`
- measured (high): `/dev/mem` on a Raspberry Pi 4B d03115 after a stock boot from the first partition: `SPAREW` and `SPARER` both `0x00400001`, `/chosen/bootloader/partition` 1, and the board's log prints `boot-part: 1`
- decompile (high): start4 `0x3ECC44E8`: PM op `+0x1C` (`0x3ED62214`, reads `+0x78`), bit 22 tested; else PM op `+0x18` (`0x3ED62204`, the saved `RSTS`) through the partition decoder `0x3EC723F2`

## `AVS_RSTDR`

Offset `0x07C` · access `rw` · 32 bits

Linux's BCM2835 name. start4 writes `0x4` to it and the next three words before it touches its first PLL, which on this SoC looks more like PLL power than AVS.

Sources:

- linux (medium): `bcm2835-power.c`: `PM_AVS_RSTDR` — _a BCM2835 define; the BCM2711 firmware's use does not fit the name_
- trace (high): start4: `0x5A000004` at `0x3ED55D4E`

## `AVS_STAT`

Offset `0x080` · access `rw` · 32 bits

Linux's BCM2835 name. start4 writes `0x4` to it with `AVS_RSTDR`.

Sources:

- linux (medium): `bcm2835-power.c`: `PM_AVS_STAT` — _a BCM2835 define; the BCM2711 firmware's use does not fit the name_
- trace (high): start4: `0x5A000004` at `0x3ED55D52`

## `AVS_EVENT`

Offset `0x084` · access `rw` · 32 bits

Linux's BCM2835 name. The bootcode pulses it (`0x800004`, then `0x4`) right before it brings PLLC up, and start4 writes `0x4` with `AVS_RSTDR`.

Sources:

- linux (medium): `bcm2835-power.c`: `PM_AVS_EVENT` — _a BCM2835 define; the BCM2711 firmware uses it as PLLC's power control_
- trace (high): bootcode `0x8000AB8A` / `0x8000AB8E`, and bootmain again at `0x000AE4EE` / `0x000AE4F2`: `0x5A800004`, `0x5A000004`; start4: `0x5A000004` at `0x3ED55D54`

## `AVS_INTEN`

Offset `0x088` · access `rw` · 32 bits

Linux's BCM2835 name. The bootcode pulses it (`0x800004`, then `0x4`) right before it brings PLLD up, and start4 writes `0x4` with `AVS_RSTDR`.

Sources:

- linux (medium): `bcm2835-power.c`: `PM_AVS_INTEN` — _a BCM2835 define; the BCM2711 firmware uses it as PLLD's power control_
- trace (high): bootcode `0x8000AB8A` / `0x8000AB8E`, and bootmain again at `0x000AE4EE` / `0x000AE4F2`: `0x5A800004`, `0x5A000004`; start4: `0x5A000004` at `0x3ED55D56`

## `REG_FC`

Offset `0x0FC` · access `r` · 32 bits

Read twice by each pass of start4's `PROC` sequence, once before it clears the low bits of `PROC` and once after it has restored `PLLB_ARM`, and never written. Reads 0 in the model, and nothing acts on the value. Meaning unknown; Linux names no register here.

Sources:

- trace (high): start4 `0x3EC82008` and `0x3EC8211C`, twice each over a boot, reading `0x00000000`; the `PROC` routine is `0x3EC81FC0`

## `IMAGE`

Offset `0x108` · access `rw` · 32 bits

Image power domain: ISP and H264 resets among others. start4 writes `0x3C0` outright as it applies `config.txt`: `PERIRSTN`, `H264RSTN` and `ISPRSTN`, plus bit 9, which Linux does not name, with `POWUP` and the rest clear.

Sources:

- linux (high): `bcm2835-power.c`: `PM_IMAGE`, `PM_ISPRSTN = BIT(8)`, `PM_H264RSTN = BIT(7)`, `PM_PERIRSTN = BIT(6)`
- decompile (high): power-domain switch `0x3ED54E40` writes `0x7E100108 & ~BIT(8)` / `& ~BIT(7)`
- trace (high): start4: `0x5A0003C0` at `0x3ED48734`, in the routine at `0x3ED48528` that applies `config.txt` (the write is skipped when the flag at `gp+5476` is clear)

## `GRAFX`

Offset `0x10C` · access `rw` · 32 bits

Graphics power domain: the V3D reset.

Sources:

- linux (high): `bcm2835-power.c`: `PM_GRAFX`, `PM_V3DRSTN = BIT(6)`
- decompile (high): power-domain switch `0x3ED54E40` writes `0x7E10010C & ~BIT(6)`

## `PROC`

Offset `0x110` · access `rw` · 32 bits

Linux's name for the processor power domain. The pinned start4 runs a sequence on it twice after it has released the ARM and slowed it to 600 MHz. It sets `PLLB_ARM`'s divider to PLLB's `NDIV` and reads PM `0xFC`. It clears the low 12 bits and waits a `0x10` `DELAY`. Then it sets `0x400`, `0x800` and `0x3FF` in turn; each is set with PLLB's `HOLDARM` on and an 8-tick `DELAY` before, after and after the release. Last it restores `PLLB_ARM` and reads `0xFC` again. The writes are 0, `0x400`, `0xC00`, `0xFFF` each time. Its wrapper for mode 1 then clears bit 9 of the ARM control block's `0x440`; modes 0 and 2 set that bit, wait for bit 31 of `0x444` and clear the low 12 bits here instead. What the bits do here is not known.

Sources:

- linux (high): `bcm2835-power.c`: `PM_PROC`
- trace (high): start4: `0x5A000000` at `0x3EC82024`, `0x5A000400` at `0x3EC82066`, `0x5A000C00` at `0x3EC820AA`, `0x5A000FFF` at `0x3EC820EE`; routine `0x3EC81FC0`, wrappers `0x3EC81F44` and `0x3EC81FA2`
