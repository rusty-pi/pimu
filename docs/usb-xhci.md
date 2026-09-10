# PCIe + VL805 xHCI: what it would take, and whether it is worth it

Scoping study for modelling the Pi 4's USB 3.0 host so that USB boot becomes
reachable — tracked as [issue #18]. The Pi 4B's xHCI controller is a **VIA
VL805** (`1106:3483`) sitting behind the BCM2711's PCIe root complex, not on
the peripheral bus, and it needs a firmware blob uploaded to it at boot.

Every address, register offset and log line below was measured — either from a
`recon` run of this bench, from a static disassembly of the real blobs, or from
`ssh rpi-dev` (a real Pi 4B with a VL805). Provenance is given inline. Nothing
here is recalled from memory.

**Bottom line up front:** the boot loses nothing to `USB xHC init failed`
today. Stages 0 and 1 are done — the root complex is decoded, the link trains,
and the config-space router finds the VL805 — but stage 1 ships with the
endpoint **detached** (`RVF_PCIE_DEVICE=1` to attach it), because with a live
link the bootloader starts a USB bring-up that the model cannot finish and the
boot stalls. What turns it on is stage 3, the controller the *bootloader*
needs. Stage 2 (start4's `XHCI_RESET`, `MCU FW`, `VLI firmware load`) is a
separate errand: it is about handing Linux a working controller and lands
after `arm_loader` in the reference log, so it is not on the USB-boot path at
all. See [Recommendation](#7-recommendation).

---

## 1. The two stages, and the code behind each log line

The strings are split across the two blobs, and the code is completely
different in each.

### Where the strings live

`pieeprom.bin` is mostly LZ-packed sections, so `grep` finds nothing in it. The
second-stage bootloader's strings are visible in its uncompressed sibling
`recovery.bin`, and at runtime in the unpacked image the model builds in RAM:

| string | blob | address |
|---|---|---|
| `PCI%d init` | 2nd-stage bootloader | `0x000A6F5C` (unpacked, runtime) |
| `PCIe timeout: 0x%08x` | 2nd-stage bootloader | `0x000A6F68` |
| `PCI%d reset` | 2nd-stage bootloader | `0x000A7084` |
| `PCIe scan %08x:%08x` | 2nd-stage bootloader | `0x000A7118` |
| `USB xHC init failed` | 2nd-stage bootloader | `0x000B62AC` |
| `HUB2.0 fail` | 2nd-stage bootloader | `0x000B6DF8` |
| `xHC%d ver: %d HCS: %08x %08x %08x HCC: %08x` | 2nd-stage bootloader | `0x000BBF20` |
| `xHC HCRST timeout %x` | 2nd-stage bootloader | `0x000BB0A4` |
| `xHC%d ports %d slots %d intrs %d` | 2nd-stage bootloader | `0x000BB0BC` |
| `USB%d root HUB port %d init` | 2nd-stage bootloader | `0x000B9ACC` |
| `HUB init %s` / `HUB %s port %d st: %x ch: %x dev: %p` | 2nd-stage bootloader | `0x000BB454` / `0x000BB6F0` |
| `PCIe: xHC failed` / `PCIe: xHC initialised` | `start4.elf` | `0x3EC61368` / `0x3EC6137C` |
| `XHCI_RESET: vendor: %x device: %x pcie-base: %08x` | `start4.elf` | `0x3EDC6358` |
| `VL805 device not recognized` | `start4.elf` | `0x3EDC633C` |
| `VL805 FW missing %p %p` | `start4.elf` | `0x3EDC6324` |
| `MCU FW: %x %x` | `start4.elf` | `0x3EDC5FB8` |
| `hub2 mismatch at %02x %02x %02x` | `start4.elf` | `0x3EDC5FC8` |
| `VLI firmware load complete status %d` | `start4.elf` | `0x3EDC5FE8` |
| `Using bootloader MCU %p %d` | `start4.elf` | `0x3EDC6158` |

The bootloader's unpacked image was recovered with
`recon ... --dump 0xa0000:0x60000` and re-disassembled with the `disasm`
subcommand; `start4.elf` addresses are runtime (`0x3E…` = ELF `0x0E…`
+ `0x3000_0000`).

One gotcha when re-walking this: on a *flat* binary `disasm --vaddr` only
relabels the output, it does not seek — the bytes still come from file offset
0. Slice the dump to the address you want (`dd skip=$((addr - 0xa0000))`) and
pass `--base <addr>`, or the listing is a linear sweep from the wrong place
that decodes into plausible-looking nonsense.

Neither blob's string references are plain 32-bit immediates, so
`scripts/vc4-xref.py` cannot find them. They are all `lea rd, (pc ± imm)`. A
scan of `.text` for both encodings (`rd, 0xE5, imm32` and `0xE0|rd, 0xBF,
imm16`) is what located the call sites listed below.

### Stage 1 — the EEPROM bootloader

The bootloader addresses everything through a single base register held at
`0x7C00_0000`; the system timer, for example, is `base + 0x0200_3004` =
`0x7E00_3004`. PCIe registers are `base + 0x0150_xxxx` = `0x7D50_xxxx`.

`pcie_reset` (`0x000A7034`), which prints `PCI%d reset`:

```
0x000A7034  push {r6,r7,lr}
0x000A703C  lea  r0, "PCI%d reset"
0x000A7042  bl   printf
0x000A7046  mov  r4, #0x7C000000
0x000A7052  ld   r3, [r4 + 0x1509210]   ; 0x7D509210  RGR1_SW_INIT_1
0x000A705A  or   r3, #2                 ; bridge reset
0x000A705E  st   r3, [r4 + 0x1509210]
0x000A7064  ld   r0, [r4 + 0x1509210]
0x000A706A  or   r0, #1                 ; PERST#
0x000A706E  st   r0, [r4 + 0x1509210]
0x000A7074  mov  r0, #10000
0x000A7078  bl   udelay                 ; 10 ms in reset
```

`0x7D50_9210` is `PCIE_RGR1_SW_INIT_1` — the same register Linux's
`pcie-brcmstb` uses for BCM2711, bit 0 = PERST#, bit 1 = bridge soft-reset.
Confirmed live with `RVF_WATCH=0x7d509210`:

```
[watch] pc=0x8000ab4a store32 0x7d509210 <- 0x2      # bootcode, before stage 2
[watch] pc=0x8000ab50 store32 0x7d509210 <- 0x3
  2.34 PCI0 init
  2.34 PCI0 reset
[watch] pc=0x000a705e store32 0x7d509210 <- 0x3      # pcie_reset: assert
[watch] pc=0x000a706e store32 0x7d509210 <- 0x3
[watch] pc=0x000a6ca2 store32 0x7d509210 <- 0x1      # release bridge reset
[watch] pc=0x000a6dcc store32 0x7d509210 <- 0x0      # de-assert PERST#
  3.66 PCIe timeout: 0x00000000
```

Note the boot ROM / bootcode stage already parks the block in reset at
`0x8000AB4A` before the second stage runs.

`pcie_init` continues at `0x000A6C92` (the return site of the `pcie_reset`
call at `0x000A6C8E`). Its full register conversation, in order:

| pc | op | address | what |
|---|---|---|---|
| `A6C92`/`A6CA2` | rmw | `0x7D509210` | clear bit 1 — release bridge reset |
| `A6CA8`/`A6CB0` | rmw | `0x7D504204` | `MISC_HARD_PCIE_HARD_DEBUG` — clears `~0x08000000` (SERDES IDDQ) |
| `A6CB6` | — | — | `udelay(10000)` |
| `A6CC2`… | rmw ×4 | `0x7D504008` | `MISC_MISC_CTRL` — burst size / SCB sizes |
| `A6D1C`/`A6D22` | write | `0x7D504034`, `0x7D504038` | `MISC_RC_BAR2_CONFIG_LO/HI` — the inbound (DMA) window |
| `A6D3C`…`A6D5A` | write | `0x7D504044/48/4C` | `MISC_MSI_BAR_CONFIG_LO/HI` and `MISC_MSI_DATA_CONFIG` — `0xFFFF_FFFD`, `0xF`, `0xFFE0_6540`, matching the MSI address/data `lspci` reports on the endpoint |
| `A6D60`/`A6D6A` | rmw | `0x7D50402C` | `MISC_RC_BAR1_CONFIG_LO` — disabled (`& ~0x1F`) |
| `A6D70`/`A6D7A` | rmw | `0x7D50403C` | `MISC_RC_BAR3_CONFIG_LO` — disabled |
| `A6D92`/`A6D98` | write | `0x7D504310`, `0x7D504308` | MSI/interrupt-mask registers, set to `~0` |
| `A6D9E`/`A6DBC` | rmw | `0x7D5000B8`, `0x7D5000DC` | root-port config space (link control / capability) |
| `A6DC2`/`A6DCC` | rmw | `0x7D509210` | clear bit 0 — **de-assert PERST#** |
| `A6DDE` | call | — | `wait_until(cb, arg, timeout, 1000 µs)` at `0x000A9038` |
| `A6DF2` | — | — | on failure `udelay(100000)` |
| `A6DFC`/`A6E20` | rmw | `0x7D50043C` | `RC_CFG_PRIV1_ID_VAL3` ← `0x060400` (class = PCI-to-PCI bridge) |
| `A6E26`…`A6E60` | rmw | `0x7D5000B4`, `0x7D5000C8`, `0x7D500188` | more root-port config space |
| `A6E66`/`A6E74` | rmw | `0x7D504204` | re-enable the SERDES clock (`or 0x40000000`) |
| `A6E80`…`A6F24` | writes | bridge config header | offsets `0x04`←`0x146`, `0x0C`←`16`, `0x22`←`0x8000`, `0x24`←`0xBFF0`, `0x26`←`0xFFF0`, `0x28`, `0x2C`, `0x30`, `0x32`, `0x3C`←`53`, `0x3E`←`1` |
| `A6F3C` | read | `0x7D504068` | `MISC_PCIE_STATUS` — printed by `PCIe timeout: 0x%08x` |

The `0x060400` at `0x000A6E08` is a nice cross-check: it is exactly the class
code Linux reports for the root port (`pci 0000:00:00.0: [14e4:2711] type 01
class 0x060400`, `dmesg` on `rpi-dev`), and `0x146` at `0x000A6F1C` is exactly
the command register `lspci` reports on the endpoint (`Mem+ BusMaster+ ParErr+
SERR+`).

The link wait is the generic helper at `0x000A9038`:

```
0x000A9038  push
0x000A903A  mov  r8, #0x7C000000
0x000A9040  ld   r6, [r8 + 0x2003004]   ; CLO snapshot = deadline base
            loop: udelay(r7=poll interval)
                  r0 = (*r11)(r10)      ; predicate callback
                  if r0 == 0 -> return 0          (success)
                  if (CLO - r6) >= r9  -> return ~0 (timeout)
```

On timeout `pcie_init` falls through to `0x000A6F3C`, reads
`MISC_PCIE_STATUS`, prints it, and returns failure. **A failed link is not
fatal**: control returns to the USB bring-up, which prints `USB xHC init
failed` (`0x000B62AC`, next to `USB delay %u` and `Reset USB port-power %d
ms`) and the bootloader carries on to the next `BOOT_ORDER` entry.

What the model prints today:

```
  2.34 PCI0 init
  2.34 PCI0 reset
  3.66 PCIe timeout: 0x00000000
  3.66 USB xHC init failed
  4.59 SD_OC: 0
  4.92 Boot mode: SD (01) order f4
```

`0x00000000` is the value `MISC_PCIE_STATUS` reads back because nothing
answers — see [§4](#4-what-the-model-does-with-these-addresses-today).

### Stage 2 — `start4.elf`

On real hardware this runs **after the ARM has already been released**
(`examples-on-real-hardware/vc4-boot.log`):

```
9569.959: arm_loader: Starting ARM with 948MB
9681.099: sdram: sdram refresh 1562->3124 (2)
10664.803: XHCI_RESET: vendor: 1106 device: 3483 pcie-base: 00004000
10755.480: MCU FW: 10ffc800 3ff20000
10755.505: VLI firmware load complete status 0
```

`XHCI_RESET` is printed by the function at **`0x3EDC61F4`**:

```
0x3EDC61F4  push {r6..r11,lr}
0x3EDC61F6  mov   r10, r1                  ; arg1 -> printed as "pcie-base"
0x3EDC61FC  mov   r4,  #0x7D4F8000
0x3EDC6204  ld.h  r7,  [r4 + 0x10000]      ; 0x7D508000  vendor id
0x3EDC620A  ld.h  r6,  [r4 + 0x10002]      ; 0x7D508002  device id
0x3EDC621E  lea   r2,  "XHCI_RESET: vendor: %x device: %x pcie-base: %08x"
0x3EDC622E  cmp   r7,  #0x1106             ; VIA
0x3EDC6232  bne   -> "VL805 device not recognized"
0x3EDC6234  cmp   r6,  #0x3483             ; VL805
0x3EDC6238  beq   -> continue
```

`0x7D50_8000` is `PCIE_EXT_CFG_DATA` — the 4 KiB window through which the root
complex exposes the configuration space of whatever
`PCIE_EXT_CFG_INDEX` (`0x7D50_9000`) selects. So start4 identifies the endpoint
by reading its config-space vendor/device pair through the bridge, and refuses
to touch anything that is not `1106:3483`. That matches `lspci -nn` on
`rpi-dev`:

```
00:00.0 PCI bridge [0604]: Broadcom Inc. BCM2711 PCIe Bridge [14e4:2711] (rev 20)
01:00.0 USB controller [0c03]: VIA VL805/806 xHCI USB 3.0 Controller [1106:3483] (rev 01)
```

The firmware-upload function is just below it. The `MCU FW: %x %x` print at
`0x3EDC5EF8` is preceded by:

```
0x3EDC5ED0  mov  r4, #0x7D504000
0x3EDC5ED6  bmask r8, #30                  ; r8 &= 0x3FFFFFFF  (strip the cache alias)
0x3EDC5ED8  ld   r1, [r4 + 0x38]           ; MISC_RC_BAR2_CONFIG_HI
0x3EDC5EDA  ld   r2, [r4 + 0x34]           ; MISC_RC_BAR2_CONFIG_LO
0x3EDC5EDC  shl  r1, #26
0x3EDC5EDE  and  r2, #~0xFFF
0x3EDC5EE2  or   r10, r8, r2
0x3EDC5EE6  lsr  r10, #6
0x3EDC5EE8  or   r10, r1
0x3EDC5EF8  lea  r2, "MCU FW: %x %x"
0x3EDC5EFE  mov  r3, r10                   ; first  %x
0x3EDC5F00  mov  r4, r8                    ; second %x
```

So the two words in `MCU FW: 10ffc800 3ff20000` are the **same buffer in two
address spaces**:

- `0x3FF20000` — the VPU/ARM-physical address of the firmware image in SDRAM
  (`r8`, with the cache-alias bits masked off);
- `0x10FFC800` — the address the VL805 must DMA from, derived from the inbound
  window: `(0x3FF20000 >> 6) | (RC_BAR2_CONFIG_HI << 26)` = `0x00FFC800 |
  0x10000000`. `RC_BAR2_CONFIG_HI = 4` puts the inbound window at PCI address
  `0x4_0000_0000`, which is exactly what the device tree says (see §2) and what
  Linux reports: `IB MEM 0x0000000000..0x01ffffffff -> 0x0400000000`.

Around the print, start4 pokes VL805 vendor registers via a 4-byte
index/data pair in the endpoint's config space at `0x7D50_8078` /
`0x7D50_807C` (an indirect port), walks the image writing register indices
`0x7C`/`0x7D`/`0x7E` with values `1`, `0`, `0x1800`, `0x500`, then reads the
image back for the `hub2 mismatch at %02x %02x %02x` verify pass, and finally
prints `VLI firmware load complete status %d`.

The `pcie-base: 00004000` argument is passed in by the caller. It is *not* the
BAR value Linux ends up with (`0x6_0000_0000` / bus `0xC000_0000`), so the
model does not have to reproduce Linux's assignment — it has to reproduce
whatever start4's own PCIe allocator hands out, which is a separate function
not yet traced.

---

## 2. The hardware blocks, and where they live

All from `rpi-dev` (`/proc/device-tree`, `lspci`, `dmesg`) — not guessed.

**PCIe root complex** — `pcie@7d500000`, `compatible = "brcm,bcm2711-pcie",
"brcm,bcm7445-pcie"`:

```
reg        = <0x0 0x7d500000  0x0 0x9310>
ranges     = <0x02000000 0x0 0xc0000000   0x6 0x00000000   0x0 0x40000000>
dma-ranges = <0x02000000 0x4 0x00000000   0x0 0x00000000   0x2 0x00000000>
```

- registers: `0x7D50_0000`, **0x9310 bytes** — so the window the model has to
  claim is `0x7D50_0000 .. 0x7D50_9310`.
- outbound: SoC physical `0x6_0000_0000`, 1 GiB, appearing on the PCI bus at
  `0xC000_0000`. Confirmed by `dmesg`: `MEM 0x0600000000..0x063fffffff ->
  0x00c0000000`.
- inbound: PCI `0x4_0000_0000`, 8 GiB, mapped to SoC physical `0x0` — the
  window `RC_BAR2_CONFIG_LO/HI` programs and the one the `MCU FW` address
  arithmetic above depends on.

Register offsets the firmware actually touches, all within that window:

| offset | Linux `pcie-brcmstb` name | used by |
|---|---|---|
| `0x0000`–`0x0FFF` | root-port own config space | bootloader (`0x0B4/0x0B8/0x0C8/0x188`), class code at `0x043C` |
| `0x4008` | `MISC_MISC_CTRL` | bootloader |
| `0x402C` | `MISC_RC_BAR1_CONFIG_LO` | bootloader |
| `0x4034`/`0x4038` | `MISC_RC_BAR2_CONFIG_LO/HI` | bootloader (write), start4 (read) |
| `0x403C` | `MISC_RC_BAR3_CONFIG_LO` | bootloader |
| `0x4044`/`0x4048`/`0x404C` | MSI BAR lo/hi + MSI data | bootloader |
| `0x400C`/`0x4010`/`0x4070`/`0x4080`/`0x4084` | `CPU_2_PCIE_MEM_WIN0` outbound window | bootloader, after the scan (see §5.1) |
| `0x4068` | `MISC_PCIE_STATUS` | bootloader — **the link-up poll** |
| `0x4204` | `MISC_HARD_PCIE_HARD_DEBUG` | bootloader (SERDES IDDQ / clock) |
| `0x4308`/`0x4310`/`0x4314` | MSI / interrupt masks | bootloader, start4 |
| `0x8000`–`0x8FFF` | `EXT_CFG_DATA` | start4 (endpoint config space) |
| `0x9000` | `EXT_CFG_INDEX` | (bus/dev/fn selector for the above) |
| `0x9210` | `RGR1_SW_INIT_1` | bootcode, bootloader `pcie_reset`, `pcie_init` |

**VL805 configuration space** — `lspci -vvv -s 01:00.0` plus the raw config
dump on `rpi-dev`:

```
06 11 83 34 46 05 10 00 01 30 03 0c 10 00 00 00
04 00 00 c0 00 00 00 00 00 00 00 00 00 00 00 00
```

- vendor `1106`, device `3483`, class `0c0330` (xHCI), revision `01`
- BAR0 = `0xC000_0004` → 64-bit memory, **4 KiB**, bus address `0xC000_0000`
  (SoC `0x6_0000_0000`); no other BARs
- capabilities: PM at `0x80`, MSI (4 vectors, 64-bit) at `0x90`, PCIe endpoint
  at `0xC4`, AER at `0x100`
- link: 5 GT/s x1

**xHCI MMIO** — 4 KiB at BAR0. An earlier draft of this document said
`/dev/mem` was locked on `rpi-dev` and that these registers could not be read.
**That was wrong** and got repeated for several sessions: the kernel there has
`# CONFIG_STRICT_DEVMEM is not set`, so `sudo` plus `mmap` reads any physical
address, MMIO included. BAR0 is at physical `0x6_0000_0000`
(`/sys/bus/pci/devices/0000:01:00.0/resource`):

```python
m = mmap.mmap(os.open("/dev/mem", os.O_RDONLY | os.O_SYNC), 0x1000,
              mmap.MAP_SHARED, mmap.PROT_READ, offset=0x600000000)
struct.unpack_from("<I", m, off)[0]
```

One trap: a bulk byte slice such as `m[:64]` goes through `memcpy`, which
bursts and hands back each dword aliased four times. It looks like plausible
data and is not — only 32-bit `unpack_from` reads are valid on an MMIO
mapping. Read-only; never write to it.

Measured that way, with Linux driving the controller:

```text
CAPLENGTH  0x20        HCIVERSION 0x0100
HCSPARAMS1 0x05000420  MaxSlots=32  MaxIntrs=4  MaxPorts=5
HCSPARAMS2 0xfc000031  IST=1  ERSTMax=3  MaxScratchpad=31
HCSPARAMS3 0x00e70004  U1ExitLat=4  U2ExitLat=231
HCCPARAMS1 0x002841eb  AC64=1 CSZ=0 PPC=1  xECP=0x0028 -> caps at BAR0 + 0xA0
DBOFF      0x00000100  RTSOFF 0x00000200   HCCPARAMS2 0x00000000
USBCMD 0x5  USBSTS 0  PAGESIZE 1  DNCTRL 2  CONFIG 0x20
```

`HCCPARAMS1` cross-checks against `dmesg`'s `hcc params 0x002841eb hci version
0x100`, which is how we know the read path is sound. Note `MaxScratchpad = 31`
— the controller wants 31 scratchpad pages from the host, which a model has to
account for.

Extended capability list, walked from `0xA0`:

```text
+0x0a0  id=1   USB legacy support
+0x0b0  id=2   "USB " rev 2.0   portoff=1  portcount=1
+0x0d0  id=2   "USB " rev 3.0   portoff=2  portcount=4
+0x300  id=10
```

So port 1 is USB2 and ports 2-5 are USB3 — see [§5.2](#52-live-ground-truth-for-stage-3)
for how those map onto the board's sockets.

---

## 3. Where the VL805 firmware blob comes from

Not from `start4.elf`, and not from the SD card. It is **in the SPI EEPROM
image**, as two packed sections, and the bootloader hands the decompressed
result to start4 in RAM.

`firmware/vl805-000138c0.bin` (99352 bytes, fetched by
`scripts/fetch-firmware.sh` from `rpi-eeprom`) does not appear byte-for-byte in
`start4.elf`, `pieeprom.bin`, `recovery.bin` or `fixup4.dat` — it is the
*uncompressed* VL805 SPI image, kept alongside for reference. Inside
`pieeprom.bin` the same content is present compressed, in two named sections:

```
0x05F080:  ... 55 AA F3 3F 00 00 1B 74  "vl805hub.bin\0..."
0x060C00:  ... 55 AA F3 3F 00 00 EC 23  "vl805mcu.bin\0..."
```

`55 AA` is the EEPROM section magic, `F3 3F` the packed-file section type, then
a big-endian length (`0x1B74` = 7028 bytes for the hub image, `0xEC23` = 60451
for the MCU image) and the file name. The bench's own section walk shows these
as the two `packed 0xf33f` entries in the `recon --eeprom` header:

```
0x05f090..0x060c0c  packed 0xf33f
0x060c10..0x06f83b  packed 0xf33f
```

(every other packed section is `0xf44f` — the bootloader's own code.)

`start4.elf` knows both names (`vl805hub.bin`, `vl805mcu.bin` at file offsets
411480/411496) and, decisively, carries the string **`Using bootloader MCU %p
%d`** at `0x3EDC6158`: it takes a pointer and a length from the bootloader
hand-off rather than loading a file. `VL805 FW missing %p %p` is the path taken
when that hand-off is empty. The address printed in the reference log,
`0x3FF20000`, is a plain SDRAM address just under the 1 GiB VPU limit — a
buffer the bootloader decompressed into and left behind.

`recovery.bin` additionally knows `vl805.bin` / `vl805.sig` and has the whole
`Updating VL805` / `Verify VL805 EEPROM` / `VLI EEPROM is write protected
(%02x)` flow: that is the path that flashes the VL805's *own* SPI EEPROM. It is
not used in a normal boot, where the firmware is uploaded to RAM on the device
each time.

**Consequence for a model:** the firmware content never has to be interpreted.
A model can accept the upload, checksum-compare it against what it was given
(so the `hub2 mismatch` verify pass passes), and report `status 0`. Retaining
it only matters if the model ever wants to claim a *specific* VL805 firmware
revision back to start4.

---

## 4. What the model does with these addresses today

Nothing — and worse than nothing.

`Machine::in_mmio` (`src/machine.rs`) only recognises three regions:

```rust
(map::PERIPH_BASE..map::PERIPH_BASE + map::PERIPH_SIZE).contains(&addr)   // 0x7E00_0000
    || (map::SDRAMC_BASE..).contains(&addr)                               // 0x7DC0_0000
    || (map::CLKMON_BASE..).contains(&addr)                               // 0x7D5D_0000
```

`0x7D50_0000` is in none of them, so every PCIe register access falls through
to `fold_ram_addr(addr) = addr & 0x3FFF_FFFF` and lands in **DRAM** at
`0x3D50_0000`–`0x3D50_9310`. That is why:

- `RGR1_SW_INIT_1` reads back whatever was last written to it (the `RVF_WATCH`
  trace in §1 shows the read-modify-writes composing correctly — off DRAM);
- `MISC_PCIE_STATUS` reads `0`, so the link never comes up and
  `PCIe timeout: 0x00000000` is printed;
- the firmware is silently scribbling ~37 KiB into a region of modelled DRAM
  that nothing currently guards. Anything the boot later places at
  `0x3D50_0000` would be corrupted, and vice versa.

It also meant the PCIe traffic was invisible: a RAM access is not an MMIO
access, so `RVF_TRACE_MMIO` showed nothing for it and it never reached the run
report's peripheral-window stub table. During the whole 1.32 s link poll the
only traced access was the system timer:

```
966329  0x7e003004
```

**Cost of the timeout, measured.** `PCI0 init` at `2.34` to `PCIe timeout` at
`3.66` in the firmware's own timeline = **1.32 s of modelled time**. The poll
body is three instructions per `CLO` read, so ~2.9 M VPU instructions — about
**0.4 %** of the 715 M a 150 s `recon` run retires. It costs a little over a
second of the transcript's timestamps and essentially no wall clock.

---

## 5. What USB boot would additionally need

Bringing the link up is the small half. USB boot needs a full stack, layered
like this — each layer only exists because the one above it asks for it:

1. **PCIe root complex** (`src/periph/pcie.rs`). Register file at
   `0x7D50_0000`, `RGR1_SW_INIT_1` reset semantics, `MISC_PCIE_STATUS`
   reporting link-up once PERST# is released, and — the part that makes it more
   than a register file — a **config-space router**: writes to `EXT_CFG_INDEX`
   select a `(bus, dev, fn)`, and accesses to the `EXT_CFG_DATA` window at
   `+0x8000` are forwarded to that function's config space.
2. **A PCI function model for the VL805.** Its config space is only 256 bytes
   plus the AER extended block, and `rpi-dev` gives the exact bytes. BAR0 must
   be sizeable (write all-ones, read back `0xFFFFF000`-style) and, once
   programmed, must make a 4 KiB MMIO region appear in the outbound window. On
   a 32-bit VPU the outbound aperture is not `0x6_0000_0000`, so the model has
   to place it wherever start4's own allocator puts it — `pcie-base: 00004000`
   in the reference log is the clue, and pinning down that allocator is part of
   stage 2.
3. **The VL805 vendor layer.** The indirect index/data port at config offsets
   `0x78`/`0x7C`, enough state for the register writes at indices
   `0x7C`/`0x7D`/`0x7E`, and a firmware sink that stores the uploaded image so
   the `hub2 mismatch` verify pass reads back what was written. This is what
   makes `VLI firmware load complete status 0` appear.
4. **xHCI operational registers.** `CAPLENGTH`/`HCIVERSION`/`HCSPARAMS`/
   `HCCPARAMS1` (measured values in §2), `USBCMD`/`USBSTS` reset and run
   semantics, `CRCR`, `DCBAAP`, `CONFIG`, and the port registers `PORTSC` for
   1 USB2 + 4 USB3 ports.
5. **Command and event rings.** The command ring (doorbell 0), the event ring
   with its ERST, and enough TRB handling to answer `Enable Slot`,
   `Address Device`, `Configure Endpoint`, and `Evaluate Context`. The model
   already has a DMA-capable bus, so ring memory is just RAM reads.
6. **A USB device model.** A mass-storage device behind the root hub:
   descriptors (device/config/interface/endpoint), the standard control
   requests, and two bulk endpoints.
7. **BOT/SCSI.** CBW/CSW framing, then `INQUIRY`, `READ CAPACITY(10)`,
   `TEST UNIT READY`, `READ(10)`. The last one is where a disk image gets
   plugged in.
8. **The boot-media abstraction.** This is the part that already exists:
   `--sd` is backed by `src/periph/sdcard.rs` behind the Arasan eMMC model. A
   USB disk would be a *second* block backend behind the SCSI layer, sharing
   the same FAT/GPT walk the bootloader performs itself. The bench-level change
   is a `--usb <img>` flag alongside `--sd`, plus a `BOOT_ORDER` override so the
   USB entry is actually reached.

Layers 1–4 are a week-ish of careful work with good ground truth. Layers 5–7
are where the size is: an xHCI ring implementation plus a USB device plus
BOT/SCSI is a genuine subsystem, comparable in scope to the whole
`src/periph/` directory as it stands. This is a multi-session project, and
should be tracked as an epic with independently verifiable stages.

### 5.1 What the bootloader does once the link is up

Measured, with stage 1 landed and `RVF_PCIE_DEVICE=1`:

```text
  2.14 PCI0 init
  2.14 PCI0 reset
  2.75 PCIe scan 000014e4:00002711
  2.75 PCIe scan 00001106:00003483
  3.31 XHCI-STOP
  3.31 xHC0 ver: 0 HCS: 00000000 00000000 00000000 HCC: 00000000
  3.31 USBSTS 0
```

…and then nothing. `end Stuck { pc: 0x000AA3C0 }` — a `udelay` poll, 60 s of
modelled silence, no `SD_OC`, no `Boot mode`, no `arm_loader`. `boot-check.sh`
fails with the whole file-loading phase missing. This is #18's warning made
concrete, and the reason the flag defaults off.

Five things worth knowing before stage 3 starts.

**The scan works and is exhaustive.** `pcie_scan` writes
`bus << 20` into `EXT_CFG_INDEX` (`0x7D50_9000`) for all 256 buses and reads
vendor/device/class/BAR0/header-type at `EXT_CFG_DATA + 0x00/0x08/0x10/0x0E`.
It finds the root port at bus 0 and the VL805 at bus 1, and every other bus
answers `0xFFFF`. So the `pcie-brcmstb` index encoding
(`bus << 20 | slot << 15 | fn << 12`) is confirmed against the firmware, not
just against Linux.

**The outbound window is out of the VPU's reach.** After the scan the
bootloader programs, at `0x000A725C`–`0x000A72F0`:

| register | value | meaning |
|---|---|---|
| `0x7D50_400C` `MEM_WIN0_LO` | `0x8000_0000` | PCI bus address the window maps to |
| `0x7D50_4010` `MEM_WIN0_HI` | `0` | |
| `0x7D50_4070` `MEM_WIN0_BASE_LIMIT` | `0x3FF0_0000` | base `[31:20]`, limit `[15:4]`, in MiB |
| `0x7D50_4080` `MEM_WIN0_BASE_HI` | `6` | |
| `0x7D50_4084` `MEM_WIN0_LIMIT_HI` | `6` | |

Decoding the way `brcm_pcie_set_outbound_win()` encodes it, the CPU-side base
is `(6 << 12 | 0x3FF) << 20` = `0x6_3FF0_0000` — a 35-bit address the 32-bit
VPU cannot form. So whatever aperture the VPU uses to reach xHCI MMIO, it is
**not** this window directly, and finding it is a prerequisite for stage 3.
(The `0x000A6D3C`–`0x000A6D48` writes the earlier draft of this doc called
"outbound window base/limit" are not that: `0x4044`/`0x4048`/`0x404C` are
`MSI_BAR_CONFIG_LO`/`_HI` and `MSI_DATA_CONFIG`, and the values written —
`0xFFFF_FFFD`, `0xF`, `0xFFE0_6540` — match the MSI address `0xfffffffc` and
data `0x6540` `lspci` reports on the endpoint.)

**The first thing the bootloader does with the endpoint is a firmware
upload, not xHCI** — and a sticky index/data port is enough for it. The loop
at `0x000B6CE0` walks the hub image byte by byte: for each byte `i` it calls a
write helper (`0x000B6E64`) with address `0x5_2000 + i` and the byte
replicated into all four lanes, then reads the same address back
(`0x000B6F10`) and compares the low byte, bailing out with `HUB2.0 fail`
(`0x000B6DF8`) on the first mismatch. Around the loop it pokes
`0x5_1000`/`0x5_1004` and `0x3_0000`/`0x3_0004`/`0x3_0008`/`0x3_000C` with
`1`, `0`, `0x1800`, `0x500` — the same constants start4's `0x3EDC5EF8` path
uses. Those are addresses *inside the VL805*, reached indirectly through
`0x400D0`/`0x400E0`/`0x400F0`; the exact framing has not been decoded, but a
`BTreeMap` behind config `0x78`/`0x7C` that returns what was written is
already enough to get past the verify pass. Which is how the run above reaches
xHCI at all.

**It assigns BAR0 itself, and where the VPU reaches it is still open.** After
the scan, `0x000A6918` sizes BAR0 (write all-ones, read back `0xFFFF_F004` —
4 KiB), assigns it the PCI bus address **`0x8200_0000`**, writes zero to the
upper half, programs the MSI capability at `0x92`/`0x94`/`0x98`/`0x9C` with
`0x0085` / `0xFFFF_FFFC` / `0xF` / `0x6540` — the same MSI address and data
`lspci` reports on the running board — and finally writes `0x0146` to the
command register, again matching `lspci`'s `Mem+ BusMaster+ ParErr+ SERR+`.

**Where it actually stops is the xHCI capability registers.** `xHC0 ver: 0
HCS: 0 0 0 HCC: 0` (`0x000BBF20`) is the bootloader reading its idea of BAR0
and getting zeros, then hanging in a `udelay` poll at `0x000AA3C0`. Finding
what VPU address that is, is the first thing stage 3 has to do, and it is not
obvious:

* the `CPU_2_PCIE_MEM_WIN0` window the bootloader programs lands at
  CPU-physical `0x6_3FF0_0000`, which a 32-bit VPU cannot form;
* `r6` in the poll loop at `0x000BBF70` holds `0xFFF4_0000`, which *looks*
  like a candidate and is **not** one — `0xFFF4_0000 & 0x3FFF_FFFF` is
  `0x3FF4_0000`, uncached DRAM near the top of the VPU's 1 GiB window, and the
  firmware `memset`s it to zero at `0x000A0864` before use. It is a driver
  buffer, not a register window. (Decoding it as MMIO was tried and corrupts
  the buffer; the capability words still read zero, which is the proof.)

So the aperture is genuinely unidentified. The place to look next is whatever
computes the base the xHCI driver is handed — start4 prints its own version of
the same number as `pcie-base: 00004000`, so both stages get it from an
allocator that is worth tracing directly rather than inferring from a register
snapshot.

### 5.2 Live ground truth for stage 3

Captured on `rpi-dev` with a Samsung "Flash Drive FIT" plugged in, so stage 3
does not have to invent descriptor bytes.

**Topology, and how the sockets are wired.** The VL805 has five xHCI root
ports; the board routes them like this, pinned by moving the stick between
sockets and re-reading `PORTSC` each time:

```text
xHCI port 1 -> VIA Labs 2109:3431 hub, 4 ports
                 hub port 1 -> blue socket A, USB2 half     [inferred]
                 hub port 2 -> blue socket B, USB2 half     [inferred]
                 hub port 3 -> black socket, upper          [measured]
                 hub port 4 -> black socket, lower          [measured]
xHCI port 2 -> blue socket A, SuperSpeed                    [measured]
xHCI port 3 -> blue socket B, SuperSpeed                    [measured]
xHCI ports 4, 5 -> no connector                             [measured]
```

The bracketed labels are confidence levels, not decoration. Hub ports 3 and 4
were pinned by moving a stick between the two black sockets; hub ports 1 and 2
being the blue sockets' USB 2 halves is the obvious reason a four-port hub sits
behind two sockets, but it is unverified and the test stick cannot verify it —
being USB 3.10, it always negotiates SuperSpeed onto a root port when it is in
a blue socket. Confirming would need a USB2-only device.

(The USB-C connector is power-only on a Pi 4B, so it is not an xHCI port at
all.) Watch the off-by-one against `lsusb`: USB3 root-hub port *n* is xHCI
port *n + 1*.

`PORTSC` lives at `CAPLENGTH + 0x400 + (n - 1) * 0x10`. Measured values:

```text
0x00001203   SuperSpeed device attached and enabled (CCS=1 PED=1 PLS=0 speed=4)
0x000002a0   empty but powered                      (CCS=0 PED=0 PLS=5 PP=1)
0x4c000e63   the VIA hub, high-speed, on port 1
0x40000e03   the VIA hub with a device below it
```

Ports 2-5 sometimes read `0x0a0002a0` instead of `0x000002a0`: bits 25 and 27
are `WCE`/`WOE`, wake-on-connect and wake-on-over-current, which Linux sets
when it idles a port. They are power management, not presence — model
`0x000002a0` as the resting value. An unrouted port is observationally
identical to an empty one, so ports 4 and 5 need no special case.

**This forks stage 3 into two targets, and they are not the same size.**

1. **Blue socket** — the device sits directly on an xHCI root port. Needs the
   ring engine and a device model, and nothing else. This is the cheaper
   target and the one the first `--usb <img>` fixture should aim at.
2. **Black socket** — the device sits behind the VIA hub (as `1-1.3` or
   `1-1.4`), so it additionally needs a **USB hub model**: hub descriptor,
   per-port status, `GetPortStatus` / `SetPortFeature`, and the interrupt-IN
   status-change endpoint. Real extra work; worth doing only deliberately.

The firmware boots from whichever port reports a device, so this is a choice
about what the fixture is pretending to be, not about what the firmware
requires.

The hub, if it is ever needed: `2109:3431`, `bcdUSB 2.10`, `bDeviceClass 9` /
`bDeviceProtocol 1` (single TT), `bcdDevice 4.21`, one configuration
(`wTotalLength 0x19`, self-powered, remote wakeup, `MaxPower 100mA`), one
interface with a single interrupt IN endpoint `0x81`, `wMaxPacketSize 1`,
`bInterval 12`.

**The device's raw descriptors** (`/sys/bus/usb/devices/2-2/descriptors`,
device + config + interface + both endpoints, verbatim):

```text
12 01 10 03 00 00 00 09 0c 09 00 10 00 11 01 02
03 01 09 02 2c 00 01 01 00 80 26 09 04 00 00 02
08 06 50 00 07 05 01 02 00 04 00 06 30 08 00 00
00 07 05 82 02 00 04 00 06 30 08 00 00 00
```

i.e. `bcdUSB 3.10`, `bMaxPacketSize0 9` (2^9 = 512), `090c:1000`,
`bcdDevice 11.00`, strings 1/2/3 = `Samsung` / `Flash Drive FIT` /
`0374122050000640`; one configuration, `wTotalLength 0x2C`, bus-powered,
304 mA; one interface, class 8 subclass 6 protocol 80 (Mass Storage / SCSI /
Bulk-Only); bulk OUT `0x01` and bulk IN `0x82`, `wMaxPacketSize 0x400`, each
followed by a SuperSpeed endpoint companion (`06 30 08 00 00 00`,
`bMaxBurst 8`).

The two root hubs' own descriptors, for completeness:

```text
usb2 (SuperSpeed): 12 01 00 03 09 00 03 09 6b 1d 03 00 12 06 03 02 01 01
                   09 02 1f 00 01 01 00 e0 00 09 04 00 00 01 09 00 00 00
                   07 05 81 03 02 00 0c 06 30 00 00 02 00
usb1 (high speed): 12 01 00 02 09 00 01 40 6b 1d 02 00 12 06 03 02 01 01
                   09 02 19 00 01 01 00 e0 00 09 04 00 00 01 09 00 00 00
                   07 05 81 03 04 00 0c
```

**SCSI.** `sg3-utils` is not installed on `rpi-dev` and was not installed for
this, so the `INQUIRY` and `READ CAPACITY(10)` payloads were not read as raw
bytes; the fields the kernel parsed out of them are:

```text
/sys/block/sda/device/vendor      "Samsung "
/sys/block/sda/device/model       "Flash Drive FIT "
/sys/block/sda/device/rev         "1100"
/sys/block/sda/device/type        0        (direct access)
/sys/block/sda/device/scsi_level  7        (SPC-5, so INQUIRY version byte 0x06)
/sys/block/sda/size               125313283  512-byte sectors
logical/physical_block_size       512 / 512
```

so `READ CAPACITY(10)` returns last-LBA `125313282` (`0x0778_2102`) and block
length `512`. A model's own disk image will have its own capacity; what
matters is the field layout and the vendor/model/rev padding to 8, 16 and 4
bytes.

**Sector 0** is a classic MBR — `55 aa` at `0x1FE` and two entries:

```text
p1  type 0x0c (FAT32 LBA)  start LBA 0x0000_4000  0x0010_0000 sectors (512 MiB)
p2  type 0x83 (Linux)      start LBA 0x0010_4000  0x0767_E103 sectors
```

Neither entry has the boot flag set. This is the same shape the bootloader
already parses off SD, which is the point: the FAT/GPT walk above the block
layer is code the firmware already runs, so a `--usb <img>` fixture can be
built the way `scripts/make-sd.sh` builds the SD one.

### Staged plan

**Stage 0 — stop the aliasing. Done** (`src/periph/pcie.rs`).
A sticky register file over `0x7D50_0000..0x7D50_9310`, decoded in
`Machine::in_mmio` / `Machine::device_for`. Nothing else changes:
`MISC_PCIE_STATUS` still reads `0`, the timeout still happens, `USB xHC init
failed` still prints, the boot still takes the SD path. What *does* change is
that ~37 KiB of DRAM stops being scribbled on, and the whole PCIe conversation
becomes visible to `RVF_TRACE_MMIO` for the next stage to read:

```
mmio 0x000a7052  R4  0x7d509210 <- 0x00000003
mmio 0x000a705e  W4  0x7d509210 <- 0x00000003
mmio 0x000a706e  W4  0x7d509210 <- 0x00000003
mmio 0x000a6ca2  W4  0x7d509210 <- 0x00000001
mmio 0x000a6ca8  R4  0x7d504204 <- 0x00000000
mmio 0x000a6cf6  W4  0x7d504008 <- 0x90003000
mmio 0x000a6d1c  W4  0x7d504034 <- 0x00000012
mmio 0x000a6d22  W4  0x7d504038 <- 0x00000000
mmio 0x000a6d3c  W4  0x7d504044 <- 0xfffffffd
mmio 0x000a6d48  W4  0x7d50404c <- 0xffe06540
```

(`RVF_TRACE_ON_CONSOLE="PCI0 init" RVF_TRACE_MMIO=1 RVF_TRACE_CF=1`, which
also confirms the register table in §1 line by line.) Note
`RC_BAR2_CONFIG_HI` is written `0`, not the `4` the reference log's `MCU FW`
arithmetic implies — the inbound window is only widened later, presumably by
start4, and stage 2 will need to know where.

**Verified by:** `scripts/boot-check.sh` passing unchanged, and the transcript
being byte-identical across the change (`PCI0 init` / `PCI0 reset` / `PCIe
timeout: 0x00000000` / `USB xHC init failed` / `Boot mode: SD (01) order f4`,
same timestamps).

**Stage 1 — make the link come up. Done, behind a flag**
(`src/periph/pcie.rs`, `src/periph/vl805.rs`). `RGR1_SW_INIT_1` reset
semantics, `MISC_PCIE_STATUS` reporting link-up + RC mode once PERST# is
de-asserted, the `EXT_CFG_INDEX`/`EXT_CFG_DATA` router, and a VL805
config-space model seeded from the `rpi-dev` dump — including BAR sizing, so
writing all-ones to `0x10` reads back `0xFFFF_F004`.

The endpoint is **detached by default**; set `RVF_PCIE_DEVICE=1` to attach it.
That is a deliberate choice, not a hardware claim — a real Pi 4B has the VL805
soldered on. With the flag set the boot **stalls**: see
[§5.1](#51-what-the-bootloader-does-once-the-link-is-up). With it clear the
transcript is byte-identical to stage 0 (`PCIe timeout: 0x00000000` at `3.66`,
`Boot mode: SD (01) order f4` at `4.92`) and `boot-check.sh` passes.

**Verified by:** `boot-check.sh` passing on the default, `cargo test` (five
cases in `src/periph/pcie.rs`, two in `tests/peripherals.rs`), and, with
`RVF_PCIE_DEVICE=1`, `PCIe timeout` being replaced by

```text
  2.75 PCIe scan 000014e4:00002711
  2.75 PCIe scan 00001106:00003483
```

which is the firmware itself confirming the reset semantics, the link-up bits
and the config router. `PCIe: xHC initialised` does **not** appear — that
needs stage 3.

**Stage 2 — make start4's `XHCI_RESET` recognise the device.** Trace start4's
PCIe allocator to find out what `pcie-base` is and where it maps BAR0, then
expose 4 KiB of xHCI MMIO there. Accept the VL805 firmware upload and answer
the verify pass. **Verified by:** the three reference-log lines appearing in
the transcript with the right values —
`XHCI_RESET: vendor: 1106 device: 3483`, `MCU FW: <bus addr> <phys addr>`,
`VLI firmware load complete status 0` — which is a new `want` in
`boot-check.sh`. Note this requires the boot to reach `arm_loader` first.

**This stage is not on the USB-boot path**, and it is worth being explicit
about why, because the ordering is easy to get backwards. USB *boot* is the
EEPROM bootloader reading `start4.elf` and the kernel off a stick; that is
stages 1 and 3, and it all happens around `Boot mode: SD (01) order f4`, long
before `arm_loader`. Stage 2 is start4 handing an already-working controller
over to Linux, which is why the reference log puts those three lines *after*
`arm_loader: Starting ARM with 948MB`. The pairing that matters is **1 + 3**.

**Stage 3 — xHCI rings + a USB mass-storage device.** Everything in layers
5–7. The one thing this doc did not originally call out, found by running
stage 1 with the endpoint attached (§5.1), has to come first:

**Find the aperture the VPU reaches BAR0 through.** The bootloader assigns
BAR0 the PCI bus address `0x8200_0000` and programs an outbound window whose
CPU-side base is `0x6_3FF0_0000` — an address a 32-bit VPU cannot form. So
there is an alias or a second window still to identify, and nothing else in
stage 3 can be tested until it is. `0xFFF4_0000`, the base the xHCI poll loop
at `0x000BBF70` carries in `r6`, is *not* it — that folds to `0x3FF4_0000`,
uncached DRAM, and is a driver buffer. Trace the allocator that produces the
base instead; start4 prints its own copy of the same number as
`pcie-base: 00004000`.

After that, in order: the VL805's own hub-firmware upload (a sticky index/data
port behind config `0x78`/`0x7C` is enough — see §5.1), then the capability
registers, then the command and event rings, then the device.

Nothing in stage 3 needs a value invented. The xHCI registers are measured
(§2), and §5.2 has descriptors, BOT/SCSI identity, geometry and the MBR off a
live stick. Decide deliberately between the two device placements §5.2
describes: a blue socket needs only a root-port device, a black one needs a
hub model as well.

**Verified by:** `recon ... --usb <img>` with `BOOT_ORDER` forced to
USB-MSD reaching `Read start4.elf bytes …` off the USB image instead of the SD
one.

---

## 6. Does the boot lose anything today?

No.

- **The bootloader falls through cleanly.** After `USB xHC init failed` at
  `3.66` the transcript goes straight on to `SD_OC: 0` and
  `Boot mode: SD (01) order f4`. `order f4` is the *remaining* `BOOT_ORDER`
  digits after the leading `1` (SD) was consumed — i.e. the default `0xf41`:
  SD first, then `4` = USB-MSD, then `f` = restart. Because the SD image
  mounts, the USB entry is never reached.
- **On real hardware the xHCI bring-up is post-hand-off.** In
  `examples-on-real-hardware/vc4-boot.log` the `XHCI_RESET` / `MCU FW` /
  `VLI firmware load` triple lands at `10664`–`10755`, *after*
  `arm_loader: Starting ARM with 948MB` at `9569`. Nothing between the config
  read and the ARM release depends on it.
- **The cost is 1.32 s of modelled time and ~0.4 % of the instructions** (§4).
- **The one real harm is the DRAM aliasing** (§4), and that is fixed by stage 0
  alone.

The only thing the model currently *diverges* on is that a real board prints
`PCIe scan` / `PCIe: xHC initialised` here and the model prints
`PCIe timeout` / `USB xHC init failed`. Since the reference log
(`vc4-boot.log`) starts after that point, the bench has no golden line to
regress against for this phase either way.

With stage 1 landed and `RVF_PCIE_DEVICE=1`, half of that divergence closes:
`PCIe scan` appears with the right values and the timeout is gone. The other
half — `PCIe: xHC initialised` — needs stage 3.

---

## 7. Recommendation

**Stages 0 and 1 are done. Stage 1 ships detached; stage 3 is what turns it
on.**

Stage 0 was a contained fix for a genuine correctness bug — the firmware was
writing PCIe registers into modelled DRAM — and it made all further PCIe work
observable through the tooling that already exists.

Stage 1 is now implemented and unit-tested, but `RVF_PCIE_DEVICE` defaults to
off, so by default the transcript is unchanged and `boot-check.sh` cannot
regress. That default is not caution for its own sake — it was measured.
Attaching the endpoint gets the bootloader through the bus scan and through the
VL805 hub-firmware upload, and then hangs it in the xHCI bring-up against
capability registers that read zero, with no SD fall-through at all (§5.1).
This section's earlier prediction — "trading a clean, harmless 1.3-second
timeout for a new and much less clean stall" — turned out to be exactly right.

The honest ordering from here is: **stage 3, then flip the default.** Stage 3
is the real work and the real payoff — it is what USB *boot* needs, it is
exercisable today (the bootloader's USB bring-up happens around
`Boot mode: SD (01) order f4`, nowhere near `arm_loader`), and §5.2 now has
live device ground truth for all of it. Stage 2 is a separate errand for a
separate day: it is about handing Linux a working controller, it sits after
`arm_loader` in the reference log, and nothing on the USB-boot path waits on
it.

[issue #5]: https://github.com/valtzu/rpi-virt-fw/issues/5
[issue #18]: https://github.com/valtzu/rpi-virt-fw/issues/18
[issue #17]: https://github.com/valtzu/rpi-virt-fw/issues/17
