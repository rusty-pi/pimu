<!-- generated from specs/otp.toml by `cargo run -- spec-docs --update` – do not edit -->

# `otp` – Always-on config / OTP engine: the fuse array, one row at a time – reads and programming

- Bus: `vpu` (VPU bus address)
- Base: `0x7E20F000`
- Size: `0x1000`

The fuse contents live in `src/periph/configotp.rs` and must never be a real board's (`CLAUDE.md`). Offsets without a known register keep the old 'always ready' status bits (17, 18, 7) so unrelated pollers progress. Programming follows start4's OTP driver.

Sources:

- decompile (high): EEPROM bootloader `getconfig(key)`: key -> `+0x1C`, 0 -> `+0x0C` / `+0x08`, `+0x08 |= 1`, poll `+0x10`, value <- `+0x18`
- decompile (high): start4's OTP driver (table `0x3EDFB5F8`): command `0x3ED3F24C`, read `0x3ED3FAA2`, enable programming `0x3ED3F9BE`, program `0x3ED3FC52`

## Register map

| Offset | Name | Access | Width | Sources |
|---|---|---|---|---|
| `0x000` | [`BOOTMODE`](#bootmode) | r | 32 | 3, best high |
| `0x004` | [`CLKMUX`](#clkmux) | rw | 32 | 1, best medium |
| `0x008` | [`PARAM_A`](#param_a) | rw | 32 | 2, best high |
| `0x00C` | [`PARAM_B`](#param_b) | rw | 32 | 1, best medium |
| `0x010` | [`STATUS`](#status) | rw | 32 | 1, best high |
| `0x018` | [`DATA`](#data) | rw | 32 | 2, best high |
| `0x01C` | [`KEY`](#key) | rw | 32 | 3, best high |

## `BOOTMODE`

Offset `0x000` · access `r` · 32 bits

`OTP_BOOTMODE_REG`: the bootmode row (17) as the fuse block presents it at power-on. The boot ROM picks its boot source from it (bit 14, bits 5:4, and a `bits[30:28] == ~bits[10:8]` check) before it reads anything else; with the old 'always ready' placeholder here it skipped SPI and waited in USB device mode forever. No later stage reads this register, but they all read row 17 through `DATA`: bit 14 with bits 18:15 non-zero is what makes the EEPROM stages take only signed files, along with rows 47 to 54 (the SHA-256 of the customer key, low word first) and the low byte of row 55 (that hash's count of 0 bits).

Sources:

- standard (medium): Broadcom OTP register map: `OTP_BOOTMODE_REG` at `0x7E20F000`, `OTP_CONFIG_REG` `+0x04`, `OTP_CTRL_LO`/`HI` `+0x08`/`+0x0C`, `OTP_STATUS` `+0x10`, `OTP_DATA` `+0x18`, `OTP_ADDR` `+0x1C`
- decompile (medium): C0 boot ROM `0x600009a6` reads it once; `0x6000038a` / `0x60000394` take bit 14 and bits 5:4
- decompile (high): pinned bootcode `0x80009010` and bootmain `0xA92A8` build the same flags of rows 17, 47-54 and 55; the key check is bootcode `0x800081AA` / bootmain `0xA7E84`

## `CLKMUX`

Offset `0x004` · access `rw` · 32 bits

Clock-mux pokes (3 / 0 / 2), absorbed.

Sources:

- decompile (medium): the bootloader writes 3, 0, 2 here

## `PARAM_A`

Offset `0x008` · access `rw` · 32 bits

The command, then bit 0 to start it. start4 reads it back once started and gives up when it differs.

| Bits | Field | Access | Notes |
|---|---|---|---|
| 0 | `GO` | rw | Start the command. |
| 5:1 | `CMD` | rw | 0: read row `KEY` into `DATA`. 2: one word of the programming key, from `DATA`. 3: disable programming. 10: program the bits of `DATA` into row `KEY`. The width is a guess; the highest command seen is 10. |

Sources:

- decompile (high): `getconfig`: writes 0, then `|= 1`
- decompile (high): start4 `0x3ED3F24C`: writes `cmd << 1`, then `cmd << 1 | 1`, and reads both parameters back

`GO` sources:

- decompile (high): `getconfig` sets bit 0 to trigger

`CMD` sources:

- decompile (high): start4: read `0x3ED3FAA2` (0), enable programming `0x3ED3F9BE` (2), program `0x3ED3FC52` (10, then 3)

## `PARAM_B`

Offset `0x00C` · access `rw` · 32 bits

Second transaction parameter, written 0.

Sources:

- decompile (medium): `getconfig` writes 0

## `STATUS`

Offset `0x010` · access `rw` · 32 bits

Transaction status.

| Bits | Field | Access | Notes |
|---|---|---|---|
| 1 | `DONE` | r | The block is idle: set with no command running, clear while one runs, set again when it finishes and, for a read, the row is in `DATA`. A write of 1 does not clear it. So a poll that only watches for it set returns on the previous command's flag and reads `DATA` before the row arrives. |
| 2 | `PROG_ENABLED` | r | Programming is enabled: the four key words went in, in order. Command 3 clears it. |

Sources:

- decompile (high): poll at `0x8000760e`: `btest [+0x10], #1`

`DONE` sources:

- decompile (high): `0x8000760e`
- decompile (high): start4 `0x3ED3F24C` polls bit 1 after every command
- measured (high): 4B rev 1.5: the flag read back set straight after a write of 1 (`before 0x200a`, `cleared 0x200a`), and a poll that only watched for it set read row 30 as 0 (`board: boardrev 0 otp 0`, then a Pi 3 device tree); watching it fall and rise read `0xd03115`

`PROG_ENABLED` sources:

- decompile (high): start4 `0x3ED3F9BE` polls bit 2 after the fourth key word

## `DATA`

Offset `0x018` · access `rw` · 32 bits

What a read found in the row. Written with a key word before each enable command, and with the bits to fuse before a program command.

Sources:

- decompile (high): `getconfig` reads the value from `+0x18`
- decompile (high): start4 `0x3ED3F9BE` writes the key words `0xf`, `0x4`, `0x8`, `0xd` (`.rdata` `0x3EDDE6E4`); `0x3ED3FC52` writes `old | new` before command 10

## `KEY`

Offset `0x01C` · access `rw` · 32 bits

OTP row number the next read or program command works on.

Sources:

- decompile (high): `getconfig` writes the key to `+0x1C`
- decompile (high): start4 `0x3ED3FC52` writes the row before command 10
- trace (high): `--log otp`: the keys are row numbers (19..26 for the board-identity check, 28 serial, 30 revision)
