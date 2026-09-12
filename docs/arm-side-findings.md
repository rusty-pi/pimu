# The ARM side: what running Linux against the live firmware taught us

Before #40, PR #36 (`arm-unicorn` branch) put an in-process Unicorn 2 (QEMU TCG)
Cortex-A72 next to the VPU model and released it into what `arm_loader` leaves
behind. It got Linux to `Waiting for root device /dev/mmcblk0p2...` with the
real firmware answering the mailbox. Unicorn is not the way forward (#40 wants
our own interpreter), but most of what that run needed is architecture and
device knowledge that carries over. The code that does not depend on Unicorn
is on `main`:

| What | Where |
|---|---|
| GIC-400 distributor + CPU interface, with the board's ID registers | `src/periph/gic.rs` |
| ARM-local block (the two registers the armstub writes) | `src/periph/armlocal.rs` |
| Generic timer on modelled cycles, driving the GIC's PPIs | `src/periph/gentimer.rs` |
| The ARM's mailbox interrupt (INTID 65) | `Mbox::arm_irq_asserted` |
| eMMC2 interrupt (INTID 158), ADMA2/SDMA, writes, 1.8 V | `src/periph/emmc2.rs` (`gic::ID_EMMC2`) |
| armstub hand-off words, image check, bootargs patch | `src/armstub.rs` |
| Exception entry, IRQ routing, system registers | `src/aarch64/` (`cpu.rs`, `sysreg.rs`) |
| `/memory` ranges, replacing a property in a DTB | `Fdt::memory_ranges`, `Fdt::with_property` |

## Entry

- The firmware lets the ARM out of reset by writing the ARM control block:
  `0x7E00_B000 <- 0x1000`, right after it prints `arm_loader: Starting ARM
  with 948MB` (traced with `RVF_TRACE_MMIO`, see `src/periph/armctrl.rs`).
  The #36 and #37 drafts keyed on that log line instead.
- Start CPU 0 at physical 0 in EL3 with `DAIF` masked. The firmware's armstub
  is there (`src/armstub.rs` has its instruction-level summary). It writes the
  ARM-local prescaler and `CNTFRQ_EL0 = 54 MHz`, puts every GIC interrupt in
  group 1 through the secure view, and drops to non-secure EL2 with `x0` = the
  dtb. The pinned firmware leaves `[0xfc] = 0x00200000` (kernel) and
  `[0xf8] = 0x2eff1e00` (dtb).
- The armstub touches two IMPLEMENTATION DEFINED registers (`L2CTLR_EL1`,
  `CPUECTLR_EL1`); plain storage is enough.
- Secondary cores wait on the spin table at `0xd8..0xf0`. With one core
  modelled, Linux gives up after its own 5 s timeout per core
  (`CPU1: failed to come online`) and carries on with one.
- RAM for the ARM is the handed-over tree's `/memory` (948 MiB with the pinned
  firmware), not the whole SDRAM.

## Kernel command line

Add `earlycon keep_bootcon kvm-arm.mode=none` to `/chosen/bootargs` after the
firmware is done (`armstub::add_bootargs`), not in `cmdline.txt`: the firmware
echoes `cmdline.txt` into the golden transcript. `keep_bootcon` is needed
because the firmware's `console=tty1` otherwise takes the console away from the
PL011; `kvm-arm.mode=none` because the GIC's virtualisation interface is not
modelled.

## Interrupts and time

- Linux enters at EL2 without VHE, binds the non-secure physical timer
  (INTID 30) and uses the GIC's split EOI mode (`GICC_DIR` at `+0x1000`).
- GIC accesses carry the security state: the armstub programs the secure view
  from EL3, Linux the non-secure one. A model without the distinction enables
  only group 0 when Linux writes `GICD_CTLR = 1`.
- The generic timer must not follow the host clock, or timestamps differ
  between runs. Counting one instruction as one cycle at a nominal 1.5 GHz and
  letting `wfi` skip to the next wake-up gave byte-identical consoles,
  timestamps included, across five runs.
- A pending interrupt has to be taken within a few instructions of the unmask:
  the idle loop only unmasks briefly after each `wfi`.
- Wired lines were the timers (PPIs), the mailbox (65) and eMMC2 (158). The
  PL011 line (153) was not needed for console output.

## Devices Linux touched, and what they needed

Each of these was found by the Linux run and fixed in the device model on
`main`:

1. **GPIO expander** — `Failed to get GPIO 4 config (0 ffffffff)` stalled
   the SD regulator. The firmware reaches expander pins 128..135 through an
   FXL6408 at `0x43` on the PMIC bus (`src/periph/fxl6408.rs`).
2. **RNG** — `hwrng_fillfn` spun in `bcm2711_rng200_read`. The block is an
   rng200, and start4 picks between two drivers by a bit in `+0x14`
   (`src/periph/rng.rs`).
3. **GENET** — probed as v1 with all-zero registers. Now a v5 MAC with the
   UniMAC MDIO bus and a BCM54213PE (`src/periph/genet.rs`, #38).
4. **eMMC2** — Linux uses ADMA, DDR50 at 1.8 V, and needs the interrupt line
   and writes. The CSD, SCR and present-state values the firmware prints are
   golden-bound; see the eMMC2 commits.

The run ended with the kernel idling in `cpu_do_idle` at `Waiting for root
device`, because the test image had no ext4 `p2` and the card's CSD packing
hid block reads from Linux.

## Unicorn-specific lessons (for differential testing, if it is ever used)

- Its C `UC_HOOK_INSN` system-register hook returns `uint32_t`, but the Rust
  binding's trampoline returns `bool`: passed-through `MSR`s were silently
  dropped until the hook was registered through the C API directly.
- It forces EL1 at reset; getting to EL3 needs a cached-flags rebuild.
- It never delivers an exception or an interrupt to the guest; both have to be
  entered by hand (the rules are in `src/aarch64/`).
- Its counter follows the host clock.
- A DMA write into RAM behind its back leaves stale translated code; the
  translation cache has to be flushed.
