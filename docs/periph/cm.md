<!-- generated from specs/cm.toml by `cargo run -- spec-docs --update` – do not edit -->

# `cm` – Clock manager, with the A2W PLL control in the same window

- Bus: `vpu` (VPU bus address)
- Base: `0x7E101000`
- Size: `0x2000`

The analogue PLLs are not modelled: every PLL reads locked, every *_CTL register reads BUSY clear, and everything else is stored with the password byte masked on read-back. The A2W half (from 0x1000) holds each PLL's analogue words, control word, fraction and output channels. The BCM2711 bootcode programs PLLC and PLLD through a second copy of that half 0x800 higher (the *R registers); start4 reprograms PLLA, PLLC, PLLD and PLLB, and writes PLLH's analogue words, through the base copy.

Sources:

- datasheet (high): BCM2711 ARM Peripherals, General Purpose GPIO Clocks: the CM_*CTL / CM_*DIV layout and the 0x5A password
- decompile (high): EEPROM bootloader programs a PLL and polls for lock before trusting the SPI clock
- linux (high): drivers/clk/bcm/clk-bcm2835.c: the CM_* and A2W_* offsets, the generator control bits, the PLL hold and reset bits, the lock bits and the A2W control fields — _written for the BCM2835; every offset below that the BCM2711 firmware was seen to use lines up with it_
- trace (high): --trace-mmio of the pinned firmware (pieeprom.bin and start4.elf as firmware.sha256 lists them) booted through the ARM release; every PC cited below is in those images

## Register map

| Offset | Name | Access | Width | Sources |
|---|---|---|---|---|
| `0x008` | [`VPUCTL`](#vpuctl) | rw | 32 | 2, best high |
| `0x00C` | [`VPUDIV`](#vpudiv) | rw | 32 | 2, best high |
| `0x0E8` | [`TIMERCTL`](#timerctl) | rw | 32 | 2, best high |
| `0x0EC` | [`TIMERDIV`](#timerdiv) | rw | 32 | 2, best high |
| `0x0F0` | [`UARTCTL`](#uartctl) | rw | 32 | 3, best high |
| `0x0F4` | [`UARTDIV`](#uartdiv) | rw | 32 | 3, best high |
| `0x100` | [`DELAY`](#delay) | rw | 32 | 3, best high |
| `0x104` | [`PLLA`](#plla) | rw | 32 | 2, best high |
| `0x108` | [`PLLC`](#pllc) | rw | 32 | 2, best high |
| `0x10C` | [`PLLD`](#plld) | rw | 32 | 2, best high |
| `0x114` | [`LOCK`](#lock) | r | 32 | 3, best high |
| `0x160` | [`DSI1PCTL`](#dsi1pctl) | rw | 32 | 3, best high |
| `0x170` | [`PLLB`](#pllb) | rw | 32 | 2, best high |
| `0x1C0` | [`EMMCCTL`](#emmcctl) | rw | 32 | 2, best high |
| `0x1C4` | [`EMMCDIV`](#emmcdiv) | rw | 32 | 2, best high |
| `0x1D0` | [`EMMC2CTL`](#emmc2ctl) | rw | 32 | 2, best high |
| `0x1D4` | [`EMMC2DIV`](#emmc2div) | rw | 32 | 3, best high |
| `0x1E0` | [`GEN_1E0_CTL`](#gen_1e0_ctl) | rw | 32 | 2, best high |
| `0x1E4` | [`GEN_1E0_DIV`](#gen_1e0_div) | rw | 32 | 1, best high |
| `0x1E8` | [`GEN_1E8_CTL`](#gen_1e8_ctl) | rw | 32 | 2, best high |
| `0x1EC` | [`GEN_1E8_DIV`](#gen_1e8_div) | rw | 32 | 1, best high |
| `0x210` | [`GEN_210_CTL`](#gen_210_ctl) | rw | 32 | 2, best high |
| `0x214` | [`GEN_210_DIV`](#gen_210_div) | rw | 32 | 1, best high |
| `0x23C` | [`GEN_23C_CTL`](#gen_23c_ctl) | rw | 32 | 2, best high |
| `0x240` | [`GEN_23C_DIV`](#gen_23c_div) | rw | 32 | 1, best high |
| `0x1010`–`0x101C` (4 × 0x4) | [`PLLA_ANA`](#plla_ana) | rw | 32 | 2, best high |
| `0x1030`–`0x103C` (4 × 0x4) | [`PLLC_ANA`](#pllc_ana) | rw | 32 | 2, best high |
| `0x1050`–`0x105C` (4 × 0x4) | [`PLLD_ANA`](#plld_ana) | rw | 32 | 2, best high |
| `0x1070`–`0x107C` (4 × 0x4) | [`PLLH_ANA`](#pllh_ana) | rw | 32 | 2, best high |
| `0x10F0`–`0x10FC` (4 × 0x4) | [`PLLB_ANA`](#pllb_ana) | rw | 32 | 2, best high |
| `0x1100` | [`PLLA_CTRL`](#plla_ctrl) | rw | 32 | 2, best high |
| `0x1120` | [`PLLC_CTRL`](#pllc_ctrl) | rw | 32 | 2, best high |
| `0x1140` | [`PLLD_CTRL`](#plld_ctrl) | rw | 32 | 2, best high |
| `0x11E0` | [`PLLB_CTRL`](#pllb_ctrl) | rw | 32 | 2, best high |
| `0x1200` | [`PLLA_FRAC`](#plla_frac) | rw | 32 | 2, best high |
| `0x1220` | [`PLLC_FRAC`](#pllc_frac) | rw | 32 | 2, best high |
| `0x1240` | [`PLLD_FRAC`](#plld_frac) | rw | 32 | 2, best high |
| `0x12E0` | [`PLLB_FRAC`](#pllb_frac) | rw | 32 | 2, best high |
| `0x1320` | [`PLLC_CORE2`](#pllc_core2) | rw | 32 | 2, best high |
| `0x1330` | [`A2W_1330`](#a2w_1330) | rw | 32 | 1, best high |
| `0x1350` | [`A2W_1350`](#a2w_1350) | rw | 32 | 1, best high |
| `0x1390` | [`A2W_1390`](#a2w_1390) | rw | 32 | 2, best high |
| `0x13E0` | [`PLLB_ARM`](#pllb_arm) | rw | 32 | 2, best high |
| `0x1400` | [`PLLA_CORE`](#plla_core) | rw | 32 | 2, best high |
| `0x1420` | [`PLLC_CORE1`](#pllc_core1) | rw | 32 | 2, best high |
| `0x1440` | [`PLLD_CORE`](#plld_core) | rw | 32 | 2, best high |
| `0x1520` | [`PLLC_PER`](#pllc_per) | rw | 32 | 2, best high |
| `0x1540` | [`PLLD_PER`](#plld_per) | rw | 32 | 2, best high |
| `0x1620` | [`PLLC_CORE0`](#pllc_core0) | rw | 32 | 2, best high |
| `0x1920` | [`PLLC_CTRLR`](#pllc_ctrlr) | rw | 32 | 2, best high |
| `0x1940` | [`PLLD_CTRLR`](#plld_ctrlr) | rw | 32 | 1, best high |
| `0x1A20` | [`PLLC_FRACR`](#pllc_fracr) | rw | 32 | 1, best high |
| `0x1A40` | [`PLLD_FRACR`](#plld_fracr) | rw | 32 | 1, best high |
| `0x1B20` | [`PLLC_CORE2R`](#pllc_core2r) | rw | 32 | 1, best high |
| `0x1B40` | [`PLLD_DSI0R`](#plld_dsi0r) | rw | 32 | 1, best high |
| `0x1C20` | [`PLLC_CORE1R`](#pllc_core1r) | rw | 32 | 1, best high |
| `0x1C40` | [`PLLD_CORER`](#plld_corer) | rw | 32 | 1, best high |
| `0x1D20` | [`PLLC_PERR`](#pllc_perr) | rw | 32 | 1, best high |
| `0x1D40` | [`PLLD_PERR`](#plld_perr) | rw | 32 | 1, best high |
| `0x1E20` | [`PLLC_CORE0R`](#pllc_core0r) | rw | 32 | 1, best high |
| `0x1E40` | [`PLLD_DSI1R`](#plld_dsi1r) | rw | 32 | 1, best high |
| `0x1F20` | [`A2W_1F20`](#a2w_1f20) | rw | 32 | 1, best high |
| `0x1F40` | [`A2W_1F40`](#a2w_1f40) | rw | 32 | 1, best high |

## `VPUCTL`

Offset `0x008` · access `rw` · 32 bits

The VPU's own clock generator. start4 writes 0x71 early, and SRC 4 (PLLA's core channel) with ENAB and GATE once PLLA runs.

Sources:

- linux (high): clk-bcm2835.c: CM_VPUCTL
- trace (high): start4: 0x5A000071 at 0x3EC7C434, later 0x5A000054 at 0x3EC7DE96

## `VPUDIV`

Offset `0x00C` · access `rw` · 32 bits

VPU clock divider, 12 fractional bits. start4 writes 0x1000, divide by one.

Sources:

- linux (high): clk-bcm2835.c: CM_VPUDIV, CM_DIV_FRAC_BITS = 12
- trace (high): start4: 0x5A001000 at 0x3EC7C448

## `TIMERCTL`

Offset `0x0E8` · access `rw` · 32 bits

The timer's clock generator, off the oscillator. The bootcode enables it on SRC 1; start4 writes it again with GATE set.

Sources:

- linux (high): clk-bcm2835.c: CM_TIMERCTL
- trace (high): bootcode: 0x5A000000 at 0x8000A7B2, 0x5A000011 at 0x8000A7CC; start4: 0x5A000051 at 0x3EC7DE96

## `TIMERDIV`

Offset `0x0EC` · access `rw` · 32 bits

Timer clock divider: 0x36000 is 54, a 1 MHz tick off the 54 MHz oscillator.

Sources:

- linux (high): clk-bcm2835.c: CM_TIMERDIV
- trace (high): bootcode 0x8000A7CA and start4 0x3EC7DE34 write 0x5A036000; bootmain reads it back at 0x0008EB2A

## `UARTCTL`

Offset `0x0F0` · access `rw` · 32 bits

UART clock control. The bootloader runs the UART off the oscillator (SRC 1); just before the ARM starts, start4 stops it on SRC 1 and moves it to SRC 6, PLLD's peripheral channel, which is where Linux finds it (with MASH 1).

| Bits | Field | Access | Notes |
|---|---|---|---|
| 3:0 | `SRC` | rw | Clock source: 0 ground, 1 oscillator, 4 PLLA, 5 PLLC, 6 PLLD, 7 PLLH's aux channel. For the VPU generator 8 and 9 are PLLC's other core channels. start4's generator helper changes the source only with the generator stopped: it clears ENAB, waits for BUSY, writes SRC \| GATE, and then ENAB as well. The new divider goes in before the source when it is larger than the old one (a slower clock), and after the source and a second BUSY wait when it is not, so the output never runs faster than its old or new rate. A running generator that keeps its source gets the new divider first, with no stop. |
| 4 | `ENAB` | rw | Generator on. |
| 5 | `KILL` | rw | Stop the generator immediately. start4 sets it when a generator's BUSY (or BIT8) fails to clear. |
| 6 | `GATE` | rw | start4 sets it together with the source, one write before ENAB, on every generator it starts or moves, oscillator-fed ones (TIMERCTL, GEN_23C_CTL) included. |
| 7 | `BUSY` | r | Generator running. Real silicon holds it at 1 while running; the model always reads 0, because the shutdown path 0x3EC7F0BA spins on it waiting for clocks the model stops instantly. Same position in every *_CTL register. |
| 8 | `BIT8` | r | No Linux name. start4 waits for it to clear after it changes the divider of a generator that is running (ENAB set), and sets KILL if it does not. Reads 0 in the model. |
| 10:9 | `MASH` | rw | MASH noise-shaping stages. |
| 31:24 | `PASSWD` | rw | 0x5A on write; reads back masked. |

Sources:

- measured (high): /dev/mem read of 0xFE1010F0 on rpi-dev, Linux idle: 0x00000296
- decompile (high): console writer 0x3ED85E9C and clock-change callback 0x3EC799BC gate on ENAB
- trace (high): start4 0x3EC7DDBA..0x3EC7DEC4: 0x5A000001, UARTDIV 0x5A00FA00, 0x5A000046, 0x5A000056, with UART0 CR cleared before and set to 0x301 after — _an earlier note had start4 writing SRC 1 only; SRC 1 is what it stops the generator on before it moves it_

`SRC` sources:

- datasheet (high): BCM2711 ARM Peripherals, CM_GPxCTL: SRC
- linux (high): clk-bcm2835.c: CM_SRC_OSC, CM_SRC_PLLA_PER, CM_SRC_PLLC_PER, CM_SRC_PLLD_PER, CM_SRC_PLLH_AUX, CM_SRC_PLLC_CORE1 / CORE2
- decompile (high): start4's generator helper: ENAB cleared at 0x3EC7DDBA, old divider read at 0x3EC7DE1A and compared at 0x3EC7DE1C, divider first at 0x3EC7DE34, SRC | GATE at 0x3EC7DE96, divider after at 0x3EC7DEAA, ENAB at 0x3EC7DEC4
- trace (high): start4 moves UARTCTL (0x5DC0 to 0xFA00: divider first) and EMMCCTL (0x3C00 to 0x3000: divider after); it changes EMMC2DIV live (0x3C00 to 0x7800) on an unchanged source

`ENAB` sources:

- datasheet (high): BCM2711 ARM Peripherals, CM_GPxCTL: ENAB

`KILL` sources:

- linux (high): clk-bcm2835.c: CM_KILL
- decompile (high): start4 0x3EC7F908 polls BUSY up to 2000 times, then ORs in 0x20 at 0x3EC7F928; 0x3EC7F956 does the same for bit 8 at 0x3EC7F97C

`GATE` sources:

- linux (high): clk-bcm2835.c: CM_GATE_BIT
- trace (high): start4's generator helper: SRC | 0x40 at 0x3EC7DE96, then SRC | 0x50 at 0x3EC7DEC4

`BUSY` sources:

- datasheet (high): BCM2711 ARM Peripherals, CM_GPxCTL: BUSY
- decompile (high): shutdown path 0x3EC7F0BA polls BUSY with a 1000-iteration escape

`BIT8` sources:

- decompile (high): start4 0x3EC7F956: only with ENAB set, polls bit 8 up to 2000 times at 0x3EC7F962; called right after the divider write at 0x3EC7DE34

`MASH` sources:

- datasheet (high): BCM2711 ARM Peripherals, CM_GPxCTL: MASH

`PASSWD` sources:

- datasheet (high): BCM2711 ARM Peripherals, CM_GPxCTL: PASSWD

## `UARTDIV`

Offset `0x0F4` · access `rw` · 32 bits

UART clock divider, 12 fractional bits: the bootloader's 0x5DC0 off the oscillator, then start4's 0xFA00 (15.625) off PLLD's 750 MHz peripheral channel, the 48 MHz the ARM's PL011 driver expects.

Sources:

- measured (high): /dev/mem read of 0xFE1010F4 on rpi-dev, Linux idle: 0x0000fa00
- trace (high): bootcode 0x8000A7CA and bootmain 0x000AE0C4: 0x5A005DC0; start4: 0x5A00FA00 at 0x3EC7DE34
- linux (high): clk-bcm2835.c: CM_DIV_FRAC_BITS = 12

## `DELAY`

Offset `0x100` · access `rw` · 32 bits

Self-clearing countdown: written password | count, polled until it reads 0. Always reads 0. The bootloader's SD host helper waits 108000000 / card clock on it after every host register write (2160 before a clock is set, 276 at 390625 Hz, 2 at 50 MHz); start4 waits 0x36 in its PLL bring-up, and 8 and 0x21C around the PLLB change after the ARM release.

Sources:

- decompile (high): the bootloader's SDHCI register-write helper 0x00081dc0 writes it and spins at 0x00081de2
- linux (high): clk-bcm2835.c names the offset CM_OSCCOUNT — _a count of oscillator cycles fits the numbers: 108000000 / f ticks of the 54 MHz crystal is two periods of the card clock_
- trace (high): bootmain after EMMC2 writes: 0x5A000870, 0x5A000114, 0x5A000002; start4 0x3EC7EE22: 0x5A000036; after the release 0x3EC82082: 0x5A000008 and 0x3EC7E940: 0x5A00021C

## `PLLA`

Offset `0x104` · access `rw` · 32 bits

PLLA's analogue reset and the hold bits of its output channels. start4 writes 0x6AA, then 0x72A, programs the A2W words, releases ANARST with 0x62A, waits for lock and clears the lot. Bits 9 and 10 are set in all three values; what they do is not known.

| Bits | Field | Access | Notes |
|---|---|---|---|
| 8 | `ANARST` | rw | Hold the PLL's analogue side in reset. |
| 7 | `HOLDPER` | rw | Hold the peripheral channel. |
| 5 | `HOLDCORE` | rw | Hold the core channel. |
| 3 | `HOLDCCP2` | rw | Hold the CCP2 channel. |
| 1 | `HOLDDSI0` | rw | Hold the DSI0 channel. |

Sources:

- linux (high): clk-bcm2835.c: CM_PLLA, CM_PLL_ANARST, CM_PLLA_HOLD*
- trace (high): start4: 0x5A0006AA at 0x3EC7EDFA, 0x5A00072A at 0x3EC7EE38, 0x5A00062A at 0x3EC7EED2, 0x5A000000 at 0x3EC7EEF2

`ANARST` sources:

- linux (high): clk-bcm2835.c: CM_PLL_ANARST

`HOLDPER` sources:

- linux (high): clk-bcm2835.c: CM_PLLA_HOLDPER

`HOLDCORE` sources:

- linux (high): clk-bcm2835.c: CM_PLLA_HOLDCORE

`HOLDCCP2` sources:

- linux (high): clk-bcm2835.c: CM_PLLA_HOLDCCP2

`HOLDDSI0` sources:

- linux (high): clk-bcm2835.c: CM_PLLA_HOLDDSI0

## `PLLC`

Offset `0x108` · access `rw` · 32 bits

PLLC's analogue reset and channel holds. start4 first puts it in reset (0x100), then brings it up as PLLA (0x7AA, 0x72A, 0x62A, 0); the bootcode's bring-up through the *R registers holds it with 0x6AA and clears it after lock.

| Bits | Field | Access | Notes |
|---|---|---|---|
| 8 | `ANARST` | rw | Hold the PLL's analogue side in reset. |
| 7 | `HOLDPER` | rw | Hold the peripheral channel. |
| 5 | `HOLDCORE2` | rw | Hold core channel 2. |
| 3 | `HOLDCORE1` | rw | Hold core channel 1. |
| 1 | `HOLDCORE0` | rw | Hold core channel 0. |

Sources:

- linux (high): clk-bcm2835.c: CM_PLLC, CM_PLLC_HOLD*
- trace (high): start4: 0x5A000100 at 0x3EC7C452, 0x5A0007AA / 0x5A00072A / 0x5A00062A / 0x5A000000 from 0x3EC7EDFA; bootcode 0x8000ABFA / 0x8000AC66 and bootmain 0x000AE564 / 0x000AE5DC: 0x5A0006AA, then 0x5A000000

`ANARST` sources:

- linux (high): clk-bcm2835.c: CM_PLL_ANARST

`HOLDPER` sources:

- linux (high): clk-bcm2835.c: CM_PLLC_HOLDPER

`HOLDCORE2` sources:

- linux (high): clk-bcm2835.c: CM_PLLC_HOLDCORE2

`HOLDCORE1` sources:

- linux (high): clk-bcm2835.c: CM_PLLC_HOLDCORE1

`HOLDCORE0` sources:

- linux (high): clk-bcm2835.c: CM_PLLC_HOLDCORE0

## `PLLD`

Offset `0x10C` · access `rw` · 32 bits

PLLD's analogue reset and channel holds, brought up the same way as PLLC by both the bootcode and start4.

| Bits | Field | Access | Notes |
|---|---|---|---|
| 8 | `ANARST` | rw | Hold the PLL's analogue side in reset. |
| 7 | `HOLDPER` | rw | Hold the peripheral channel. |
| 5 | `HOLDCORE` | rw | Hold the core channel. |
| 3 | `HOLDDSI1` | rw | Hold the DSI1 channel. |
| 1 | `HOLDDSI0` | rw | Hold the DSI0 channel. |

Sources:

- linux (high): clk-bcm2835.c: CM_PLLD, CM_PLLD_HOLD*
- trace (high): bootcode 0x8000ABFA / 0x8000AC66 and bootmain 0x000AE564 / 0x000AE5DC: 0x5A0006AA, then 0x5A000000; start4: 0x5A0006AA / 0x5A00072A / 0x5A00062A / 0x5A000000 from 0x3EC7EDFA

`ANARST` sources:

- linux (high): clk-bcm2835.c: CM_PLL_ANARST

`HOLDPER` sources:

- linux (high): clk-bcm2835.c: CM_PLLD_HOLDPER

`HOLDCORE` sources:

- linux (high): clk-bcm2835.c: CM_PLLD_HOLDCORE

`HOLDDSI1` sources:

- linux (high): clk-bcm2835.c: CM_PLLD_HOLDDSI1

`HOLDDSI0` sources:

- linux (high): clk-bcm2835.c: CM_PLLD_HOLDDSI0

## `LOCK`

Offset `0x114` · access `r` · 32 bits

One bit per PLL, set once it has locked. Always all ones. start4 polls it up to 2000 times per PLL and carries on either way.

| Bits | Field | Access | Notes |
|---|---|---|---|
| 8 | `FLOCKA` | r | PLLA locked. |
| 9 | `FLOCKB` | r | PLLB locked. |
| 10 | `FLOCKC` | r | PLLC locked. |
| 11 | `FLOCKD` | r | PLLD locked. |
| 12 | `FLOCKH` | r | PLLH locked. |

Sources:

- decompile (medium): the bootloader polls it after programming a PLL
- decompile (high): start4's PLL enable at 0x3EC7EED4..0x3EC7EEE4: 2000 reads, masked with a per-PLL lock bit from its table
- linux (high): clk-bcm2835.c: CM_LOCK_FLOCKA..FLOCKH

`FLOCKA` sources:

- linux (high): clk-bcm2835.c: CM_LOCK_FLOCKA

`FLOCKB` sources:

- linux (high): clk-bcm2835.c: CM_LOCK_FLOCKB

`FLOCKC` sources:

- linux (high): clk-bcm2835.c: CM_LOCK_FLOCKC

`FLOCKD` sources:

- linux (high): clk-bcm2835.c: CM_LOCK_FLOCKD

`FLOCKH` sources:

- linux (high): clk-bcm2835.c: CM_LOCK_FLOCKH

## `DSI1PCTL`

Offset `0x160` · access `rw` · 32 bits

Linux's BCM2835 name. start4 treats it as a clock select: in its board set-up it asks for clock 8 to be fed from 0x13, which ORs 0x18 into the cleared low nibble.

Sources:

- linux (medium): clk-bcm2835.c: CM_DSI1PCTL — _a BCM2835 define; source 8 is outside the DSI1 pixel clock's parents there_
- decompile (high): start4's clock-select routine: select 0x13 at 0x3EC7E478 writes (value & ~0xF) | 0x18 at 0x3EC7E4C6..0x3EC7E4DC; the board set-up calls it with (8, 0x13) at 0x3ED4A0D4
- trace (high): start4: 0x5A000018 at 0x3EC7E4DC

## `PLLB`

Offset `0x170` · access `rw` · 32 bits

PLLB, the ARM cores' clock: analogue reset and the ARM channel's hold. start4 brings it up as the last clock before the console handover (0x2, 0x102, then 0x2 after the A2W words, 0 after lock) and toggles HOLDARM again around PROC after the ARM release.

| Bits | Field | Access | Notes |
|---|---|---|---|
| 8 | `ANARST` | rw | Hold the PLL's analogue side in reset. |
| 1 | `HOLDARM` | rw | Hold the ARM channel. |
| 0 | `LOADARM` | rw | Load the ARM channel's divider. |

Sources:

- linux (high): clk-bcm2835.c: CM_PLLB, CM_PLLB_HOLDARM, CM_PLLB_LOADARM
- trace (high): start4: 0x5A000002 at 0x3EC7EDFA, 0x5A000102 at 0x3EC7EE38, 0x5A000002 at 0x3EC7EED2, 0x5A000000 at 0x3EC7EEF2 and 0x3EC7E964; after the release 0x5A000002 / 0x5A000000 from 0x3EC82096

`ANARST` sources:

- linux (high): clk-bcm2835.c: CM_PLL_ANARST

`HOLDARM` sources:

- linux (high): clk-bcm2835.c: CM_PLLB_HOLDARM

`LOADARM` sources:

- linux (high): clk-bcm2835.c: CM_PLLB_LOADARM

## `EMMCCTL`

Offset `0x1C0` · access `rw` · 32 bits

The legacy EMMC block's clock generator. bootmain starts it on SRC 5 (PLLC) before it touches an SD host; start4 moves it to SRC 6 (PLLD) before its own SD access.

Sources:

- linux (high): clk-bcm2835.c: CM_EMMCCTL
- trace (high): bootmain: 0x5A000000 at 0x000AE0B2, 0x5A000015 at 0x000AE0CE; start4: 0x5A000005, 0x5A000046, 0x5A000056 from 0x3EC7DDBA

## `EMMCDIV`

Offset `0x1C4` · access `rw` · 32 bits

Legacy EMMC clock divider: 0x3C00 from bootmain, 0x3000 from start4.

Sources:

- linux (high): clk-bcm2835.c: CM_EMMCDIV
- trace (high): bootmain: 0x5A003C00 at 0x000AE0C4; start4: 0x5A003000 at 0x3EC7DEAA

## `EMMC2CTL`

Offset `0x1D0` · access `rw` · 32 bits

EMMC2's clock generator, the SD card's host. bootmain starts it on SRC 5 (PLLC) before its first host register write; start4 moves it to SRC 6 (PLLD) before its own.

Sources:

- linux (high): clk-bcm2835.c: CM_EMMC2CTL
- trace (high): bootmain: 0x5A000000 at 0x000AE0B2, 0x5A000015 at 0x000AE0CE; start4: 0x5A000005, 0x5A000046, 0x5A000056 from 0x3EC7DDBA

## `EMMC2DIV`

Offset `0x1D4` · access `rw` · 32 bits

EMMC2 clock divider. bootmain writes 0x3C00; start4 writes 0x3C00 off PLLD's 750 MHz channel (200 MHz, the rate its driver logs), then 0x7800 (100 MHz) in the board clock set-up it runs once config.txt is read. It keeps reading the card after that, with the card clock divider unchanged.

Sources:

- linux (high): clk-bcm2835.c: CM_EMMC2DIV
- trace (high): bootmain: 0x5A003C00 at 0x000AE0C4; start4: 0x5A003C00 at 0x3EC7DEAA, later 0x5A007800 at 0x3EC7DE34
- decompile (high): the board clock set-up asks for clock 51 at 100000000 Hz (0x3ED4A110..0x3ED4A122), between the 'ETH_CLK' / 'WL_LPO_CLK' clocks and the EMMC2 REG_154 / REG_100 writes

## `GEN_1E0_CTL`

Offset `0x1E0` · access `rw` · 32 bits

A clock generator Linux's clk-bcm2835 does not list, laid out as UARTCTL. The bootcode starts it on SRC 6 (0x16, no GATE) at 250 MHz; start4's board set-up asks for 250 MHz again and, the source unchanged, rewrites it with GATE (0x56) without stopping it.

Sources:

- trace (high): bootcode: 0x5A000000 at 0x8000A7B2, 0x5A000016 at 0x8000A7CC; start4: 0x5A000056 at 0x3EC7DE96 and 0x3EC7DEC4
- decompile (high): start4's board set-up asks for clock 71 at 250000000 Hz (0x3ED4A13C..0x3ED4A14A)

## `GEN_1E0_DIV`

Offset `0x1E4` · access `rw` · 32 bits

Divider for GEN_1E0_CTL, 12 fractional bits. 0x3000 (3) off PLLD's 750 MHz channel: 250 MHz.

Sources:

- trace (high): bootcode: 0x5A003000 at 0x8000A7CA; start4: 0x5A003000 at 0x3EC7DE34

## `GEN_1E8_CTL`

Offset `0x1E8` · access `rw` · 32 bits

A clock generator Linux's clk-bcm2835 does not list, laid out as UARTCTL. start4's board set-up starts it on SRC 6 at 250 MHz.

Sources:

- trace (high): start4: 0x5A000000 at 0x3EC7DDBA, 0x5A000046 at 0x3EC7DE96, 0x5A000056 at 0x3EC7DEC4
- decompile (high): start4's board set-up asks for clock 0x1E at 250000000 Hz (0x3ED4A0EC..0x3ED4A0F8)

## `GEN_1E8_DIV`

Offset `0x1EC` · access `rw` · 32 bits

Divider for GEN_1E8_CTL, 12 fractional bits. 0x3000 (3) off PLLD's 750 MHz channel: 250 MHz.

Sources:

- trace (high): start4: 0x5A003000 at 0x3EC7DE34

## `GEN_210_CTL`

Offset `0x210` · access `rw` · 32 bits

A clock generator Linux's clk-bcm2835 does not list, laid out as UARTCTL. start4's board set-up starts it on SRC 6 at 125 MHz.

Sources:

- trace (high): start4: 0x5A000000 at 0x3EC7DDBA, 0x5A000046 at 0x3EC7DE96, 0x5A000056 at 0x3EC7DEC4
- decompile (high): start4's board set-up asks for clock 0x1F at 125000000 Hz (0x3ED4A0FC..0x3ED4A10C)

## `GEN_210_DIV`

Offset `0x214` · access `rw` · 32 bits

Divider for GEN_210_CTL, 12 fractional bits. 0x6000 (6) off PLLD's 750 MHz channel: 125 MHz.

Sources:

- trace (high): start4: 0x5A006000 at 0x3EC7DE34

## `GEN_23C_CTL`

Offset `0x23C` · access `rw` · 32 bits

A clock generator Linux's clk-bcm2835 does not list, laid out as UARTCTL. start4's board set-up starts it on SRC 1, the oscillator, at 27 MHz.

Sources:

- trace (high): start4: 0x5A000000 at 0x3EC7DDBA, 0x5A000041 at 0x3EC7DE96, 0x5A000051 at 0x3EC7DEC4
- decompile (high): start4's board set-up asks for clock 52 at 27000000 Hz (0x3ED4A126..0x3ED4A138)

## `GEN_23C_DIV`

Offset `0x240` · access `rw` · 32 bits

Divider for GEN_23C_CTL, 12 fractional bits. 0x2000 (2) off the 54 MHz oscillator: 27 MHz.

Sources:

- trace (high): start4: 0x5A002000 at 0x3EC7DE34

## `PLLA_ANA`

Offset `0x1010`, 4 elements 0x4 apart · access `rw` · 32 bits

PLLA's four analogue words, written from the last to the first: 0, 0x4000, 0x11C000, 0.

Sources:

- linux (high): clk-bcm2835.c: A2W_PLLA_ANA0
- trace (high): start4 0x3EC7EE3C, 0x3EC7EE4C, 0x3EC7EE56, 0x3EC7EE6C

## `PLLC_ANA`

Offset `0x1030`, 4 elements 0x4 apart · access `rw` · 32 bits

PLLC's four analogue words. start4 writes 0x180, 0, 0x1D0000, 0 while it holds PLLC in reset, and 0, 0x4000, 0x11C000 when it brings it up.

Sources:

- linux (high): clk-bcm2835.c: A2W_PLLC_ANA0
- trace (high): start4 0x3EC7C480..0x3EC7C48E, then 0x3EC7EE3C..0x3EC7EE56

## `PLLD_ANA`

Offset `0x1050`, 4 elements 0x4 apart · access `rw` · 32 bits

PLLD's four analogue words, as PLLA's.

Sources:

- linux (high): clk-bcm2835.c: A2W_PLLD_ANA0
- trace (high): start4 0x3EC7EE3C..0x3EC7EE6C

## `PLLH_ANA`

Offset `0x1070`, 4 elements 0x4 apart · access `rw` · 32 bits

PLLH's four analogue words. start4 writes 0, 0x4C, 0x2C00, 0 just before it brings PLLB up, without enabling PLLH.

Sources:

- linux (high): clk-bcm2835.c: A2W_PLLH_ANA0
- trace (high): start4 0x3EC7CCDA..0x3EC7CCEA

## `PLLB_ANA`

Offset `0x10F0`, 4 elements 0x4 apart · access `rw` · 32 bits

PLLB's four analogue words, as PLLA's.

Sources:

- linux (high): clk-bcm2835.c: A2W_PLLB_ANA0
- trace (high): start4 0x3EC7EE3C..0x3EC7EE6C

## `PLLA_CTRL`

Offset `0x1100` · access `rw` · 32 bits

PLLA's multiplier. start4 writes 0x1037, then sets PRST_DISABLE after lock: 55 plus FRAC 0x8E38E, 3000 MHz off the 54 MHz oscillator.

| Bits | Field | Access | Notes |
|---|---|---|---|
| 9:0 | `NDIV` | rw | Integer part of the multiplier. |
| 14:12 | `PDIV` | rw | Pre-divider. |
| 16 | `PWRDN` | rw | PLL powered down. |
| 17 | `PRST_DISABLE` | rw | Set once the PLL has locked. |

Sources:

- linux (high): clk-bcm2835.c: A2W_PLLA_CTRL and its fields
- trace (high): start4: 0x5A001037 at 0x3EC7EEC6, 0x5A021037 at 0x3EC7EF28

`NDIV` sources:

- linux (high): clk-bcm2835.c: A2W_PLL_CTRL_NDIV_MASK

`PDIV` sources:

- linux (high): clk-bcm2835.c: A2W_PLL_CTRL_PDIV_MASK

`PWRDN` sources:

- linux (high): clk-bcm2835.c: A2W_PLL_CTRL_PWRDN

`PRST_DISABLE` sources:

- linux (high): clk-bcm2835.c: A2W_PLL_CTRL_PRST_DISABLE

## `PLLC_CTRL`

Offset `0x1120` · access `rw` · 32 bits

PLLC's multiplier, laid out as PLLA_CTRL. start4 powers PLLC down (0x10000) early, then writes 0x11030 and 0x31030: 48, no fraction.

Sources:

- linux (high): clk-bcm2835.c: A2W_PLLC_CTRL
- trace (high): start4: 0x5A010000 at 0x3EC7C466, 0x5A011030 at 0x3EC7EEC6, 0x5A031030 at 0x3EC7EF28

## `PLLD_CTRL`

Offset `0x1140` · access `rw` · 32 bits

PLLD's multiplier, laid out as PLLA_CTRL: start4 writes 0x1037 and 0x21037, 3000 MHz with FRAC 0x8E38E.

Sources:

- linux (high): clk-bcm2835.c: A2W_PLLD_CTRL
- trace (high): start4: 0x5A001037 at 0x3EC7EEC6, 0x5A021037 at 0x3EC7EF28

## `PLLB_CTRL`

Offset `0x11E0` · access `rw` · 32 bits

PLLB's multiplier, laid out as PLLA_CTRL: start4 writes 0x1042 and 0x21042, 66 plus FRAC 0xAAAAB, 3600 MHz. After the ARM release it steps NDIV up from 0x22 to 0x42 one write at a time.

Sources:

- linux (high): clk-bcm2835.c: A2W_PLLB_CTRL
- trace (high): start4: 0x5A001042 at 0x3EC7EEC6, 0x5A021042 at 0x3EC7EF28; after the release 0x5A021022..0x5A021042 at 0x3EC7EEA0

## `PLLA_FRAC`

Offset `0x1200` · access `rw` · 32 bits

Fractional part of PLLA's multiplier, 20 bits: start4 writes 0x8E38E.

Sources:

- linux (high): clk-bcm2835.c: A2W_PLLA_FRAC, A2W_PLL_FRAC_BITS = 20
- trace (high): start4 0x3EC7EEB0

## `PLLC_FRAC`

Offset `0x1220` · access `rw` · 32 bits

Fractional part of PLLC's multiplier: start4 clears it while PLLC is held in reset.

Sources:

- linux (high): clk-bcm2835.c: A2W_PLLC_FRAC
- trace (high): start4 0x3EC7C45C

## `PLLD_FRAC`

Offset `0x1240` · access `rw` · 32 bits

Fractional part of PLLD's multiplier: start4 writes 0x8E38E.

Sources:

- linux (high): clk-bcm2835.c: A2W_PLLD_FRAC
- trace (high): start4 0x3EC7EEB0

## `PLLB_FRAC`

Offset `0x12E0` · access `rw` · 32 bits

Fractional part of PLLB's multiplier: start4 writes 0xAAAAB, before and again during the ramp after the ARM release.

Sources:

- linux (high): clk-bcm2835.c: A2W_PLLB_FRAC
- trace (high): start4 0x3EC7EEB0 and 0x3EC7EE7E

## `PLLC_CORE2`

Offset `0x1320` · access `rw` · 32 bits

PLLC core channel 2. start4 disables it while PLLC is held in reset.

| Bits | Field | Access | Notes |
|---|---|---|---|
| 7:0 | `DIV` | rw | Channel divider. |
| 8 | `DISABLE` | rw | Channel off. |

Sources:

- linux (high): clk-bcm2835.c: A2W_PLLC_CORE2, A2W_PLL_CHANNEL_DISABLE, A2W_PLL_DIV_BITS
- trace (high): start4: 0x5A000100 at 0x3EC7C478

`DIV` sources:

- linux (high): clk-bcm2835.c: A2W_PLL_DIV_BITS = 8

`DISABLE` sources:

- linux (high): clk-bcm2835.c: A2W_PLL_CHANNEL_DISABLE

## `A2W_1330`

Offset `0x1330` · access `rw` · 32 bits

The bootcode writes 0x23 here before it programs PLLC through the *R registers. Not named by Linux; meaning unknown.

Sources:

- trace (high): bootcode 0x8000ABBE, and bootmain again at 0x000AE528: 0x5A000023

## `A2W_1350`

Offset `0x1350` · access `rw` · 32 bits

The bootcode writes 0x23 here before it programs PLLD through the *R registers. Not named by Linux; meaning unknown.

Sources:

- trace (high): bootcode 0x8000ABBE, and bootmain again at 0x000AE528: 0x5A000023

## `A2W_1390`

Offset `0x1390` · access `rw` · 32 bits

The bootcode writes 1 here early, later reads it back and ORs in 0x300020 (writing 0x300021) before it starts the PLLs, right before it sets bit 9 of 0x7E500220; bootmain and start4 write the same value. Meaning unknown.

Sources:

- decompile (high): bootcode 0x8000A7F6..0x8000A804: Ld r0, [0x7E102390]; Or r0, 0x5A300020; St r0
- trace (high): bootcode: 0x5A000001 at 0x80002118, 0x5A300021 at 0x8000A804; bootmain 0x0008BE5C / 0x000AE118 and start4 0x3ED494C4: 0x5A300021

## `PLLB_ARM`

Offset `0x13E0` · access `rw` · 32 bits

PLLB's ARM channel, the clock the ARM cores run from: start4 sets the divider to 2 (1800 MHz from 3600) before the ARM starts, then 3 and back to 2 around the ramp after it. Laid out as PLLC_CORE2.

Sources:

- linux (high): clk-bcm2835.c: A2W_PLLB_ARM
- trace (high): start4: 0x5A000002 at 0x3EC7E954; after the release 0x5A000003 at 0x3EC8211A, 0x5A000002 at 0x3EC7E920 / 0x3EC7E954

## `PLLA_CORE`

Offset `0x1400` · access `rw` · 32 bits

PLLA's core channel, the VPU's clock once VPUCTL selects it: start4 sets the divider to 6, 500 MHz.

Sources:

- linux (high): clk-bcm2835.c: A2W_PLLA_CORE
- trace (high): start4: 0x5A000006 at 0x3EC7E954

## `PLLC_CORE1`

Offset `0x1420` · access `rw` · 32 bits

PLLC core channel 1, laid out as PLLC_CORE2: start4 disables it while PLLC is held in reset.

Sources:

- linux (high): clk-bcm2835.c: A2W_PLLC_CORE1
- trace (high): start4: 0x5A000100 at 0x3EC7C474

## `PLLD_CORE`

Offset `0x1440` · access `rw` · 32 bits

PLLD's core channel: start4 sets the divider to 5.

Sources:

- linux (high): clk-bcm2835.c: A2W_PLLD_CORE
- trace (high): start4: 0x5A000005 at 0x3EC7E954

## `PLLC_PER`

Offset `0x1520` · access `rw` · 32 bits

PLLC's peripheral channel: start4 disables it while PLLC is held in reset and sets the divider to 4 once PLLC is up.

Sources:

- linux (high): clk-bcm2835.c: A2W_PLLC_PER
- trace (high): start4: 0x5A000100 at 0x3EC7C47C, 0x5A000004 at 0x3EC7E954

## `PLLD_PER`

Offset `0x1540` · access `rw` · 32 bits

PLLD's peripheral channel: start4 sets the divider to 4, 750 MHz, which the UART and both SD hosts then run from.

Sources:

- linux (high): clk-bcm2835.c: A2W_PLLD_PER
- trace (high): start4: 0x5A000004 at 0x3EC7E954

## `PLLC_CORE0`

Offset `0x1620` · access `rw` · 32 bits

PLLC core channel 0: start4 disables it while PLLC is held in reset.

Sources:

- linux (high): clk-bcm2835.c: A2W_PLLC_CORE0
- trace (high): start4: 0x5A000100 at 0x3EC7C470

## `PLLC_CTRLR`

Offset `0x1920` · access `rw` · 32 bits

PLLC_CTRL's copy 0x800 up, laid out the same. The bootcode programs PLLC through it: 0x1037, then 0x21037 after lock.

Sources:

- linux (medium): clk-bcm2835.c defines the same copy for PLLH only: A2W_PLLH_CTRLR = A2W_PLLH_CTRL + 0x800, and A2W_PLLH_*R for its other registers — _the name follows Linux's PLLH one; that PLLC and PLLD have the copy too is from the firmware's use_
- trace (high): bootcode 0x8000ABE0 / 0x8000AC3C, and bootmain again at 0x000AE54C / 0x000AE5B0: 0x5A001037, then 0x5A021037

## `PLLD_CTRLR`

Offset `0x1940` · access `rw` · 32 bits

PLLD_CTRL's copy 0x800 up: the bootcode programs PLLD through it, 0x1037 then 0x21037.

Sources:

- trace (high): bootcode 0x8000ABE0 / 0x8000AC3C, and bootmain again at 0x000AE54C / 0x000AE5B0: 0x5A001037, then 0x5A021037

## `PLLC_FRACR`

Offset `0x1A20` · access `rw` · 32 bits

PLLC_FRAC's copy 0x800 up: the bootcode writes 0x8E390.

Sources:

- trace (high): bootcode 0x8000ABCE, and bootmain again at 0x000AE53E: 0x5A08E390

## `PLLD_FRACR`

Offset `0x1A40` · access `rw` · 32 bits

PLLD_FRAC's copy 0x800 up: the bootcode writes 0x8E390.

Sources:

- trace (high): bootcode 0x8000ABCE, and bootmain again at 0x000AE53E: 0x5A08E390

## `PLLC_CORE2R`

Offset `0x1B20` · access `rw` · 32 bits

PLLC_CORE2's copy 0x800 up: the bootcode sets the divider to 6 and disables the channel after lock.

Sources:

- trace (high): bootcode 0x8000AC32 / 0x8000AC5A, and bootmain again at 0x000AE586 / 0x000AE5D2: 0x5A000006, later 0x5A000100

## `PLLD_DSI0R`

Offset `0x1B40` · access `rw` · 32 bits

PLLD_DSI0's copy 0x800 up: the bootcode sets the divider to 0x10 and disables the channel after lock.

Sources:

- trace (high): bootcode 0x8000AC32 / 0x8000AC5A, and bootmain again at 0x000AE586 / 0x000AE5D2: 0x5A000010, later 0x5A000100

## `PLLC_CORE1R`

Offset `0x1C20` · access `rw` · 32 bits

PLLC_CORE1's copy 0x800 up: the bootcode sets the divider to 6 and disables the channel after lock.

Sources:

- trace (high): bootcode 0x8000AC32 / 0x8000AC5A, and bootmain again at 0x000AE586 / 0x000AE5D2: 0x5A000006, later 0x5A000100

## `PLLD_CORER`

Offset `0x1C40` · access `rw` · 32 bits

PLLD_CORE's copy 0x800 up: the bootcode sets the divider to 5.

Sources:

- trace (high): bootcode 0x8000AC32, and bootmain again at 0x000AE586: 0x5A000005

## `PLLC_PERR`

Offset `0x1D20` · access `rw` · 32 bits

PLLC_PER's copy 0x800 up: the bootcode sets the divider to 5, and bootmain's SD host clocks run from this channel.

Sources:

- trace (high): bootcode 0x8000AC32, and bootmain again at 0x000AE586: 0x5A000005

## `PLLD_PERR`

Offset `0x1D40` · access `rw` · 32 bits

PLLD_PER's copy 0x800 up: the bootcode sets the divider to 4.

Sources:

- trace (high): bootcode 0x8000AC32, and bootmain again at 0x000AE586: 0x5A000004

## `PLLC_CORE0R`

Offset `0x1E20` · access `rw` · 32 bits

PLLC_CORE0's copy 0x800 up: the bootcode sets the divider to 6.

Sources:

- trace (high): bootcode 0x8000AC32, and bootmain again at 0x000AE586: 0x5A000006

## `PLLD_DSI1R`

Offset `0x1E40` · access `rw` · 32 bits

PLLD_DSI1's copy 0x800 up: the bootcode sets the divider to 0x10 and disables the channel after lock.

Sources:

- trace (high): bootcode 0x8000AC32 / 0x8000AC5A, and bootmain again at 0x000AE586 / 0x000AE5D2: 0x5A000010, later 0x5A000100

## `A2W_1F20`

Offset `0x1F20` · access `rw` · 32 bits

The bootcode clears it while it brings PLLC up through the *R registers, before and after lock. Not named by Linux.

Sources:

- trace (high): bootcode 0x8000ABE4 / 0x8000AC40 / 0x8000AC62, and bootmain again at 0x000AE550 / 0x000AE5B4 / 0x000AE5D8: 0x5A000000

## `A2W_1F40`

Offset `0x1F40` · access `rw` · 32 bits

The bootcode clears it while it brings PLLD up through the *R registers, before and after lock. Not named by Linux.

Sources:

- trace (high): bootcode 0x8000ABE4 / 0x8000AC40 / 0x8000AC62, and bootmain again at 0x000AE550 / 0x000AE5B4 / 0x000AE5D8: 0x5A000000
