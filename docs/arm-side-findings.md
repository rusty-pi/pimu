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
- Secondary cores wait on the spin table at `0xd8..0xf0`. All four cores
  come out of reset together and run the armstub (each sets up its own
  banked GIC state); Linux then releases 1..3 through the spin table
  (`smp: Brought up 1 node, 4 CPUs`). With only core 0 modelled, Linux gives
  up after its own 5 s timeout per core (`CPU1: failed to come online`).
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
  PL011 line (153) was not needed for console output, only for input
  (milestone 6). PCIe adds the endpoint's INTA (175) and the root complex's
  MSI block (180).
- A level line nobody on the ARM can clear stops the boot silently. The
  firmware's USB bring-up left an xHCI event pending in the VL805; when Linux
  retrained the link that came back as INTA, on the line only `pcieport`'s
  PME/AER handler listens to, and the kernel hung a few initcalls later with
  core 0 spinning. PERST# resets the endpoint on silicon, and now in the model.
  The run report prints each core's DAIF and GIC state (running priority,
  active, pending, lines held high) for this kind of hang.
- The firmware keeps serving property requests while Linux runs, and a lost
  VPU interrupt shows up there first. `reboot` used to end in `Firmware
  transaction 0x00038041 timeout`: the SD card power-off (`SET_GPIO_STATE`
  pin 134) sleeps 2 ms in the firmware, and its clock-service timer had
  stalled during Linux's boot, when a system-timer C2 match (VPU source 66)
  was dispatched as a mailbox interrupt. The model presented a device's
  source at CoreCtl when it was queued, not when it was vectored, so a source
  queued during the dispatcher's entry overwrote the one being taken.
  `RVF_DBG_MBOX` names each request's tag, which is how it was found.
- A level SPI held high does not show up as a storm of interrupts in the run
  report. Linux's `gic_handle_irq` reads `GICC_IAR` in a loop, and in split EOI
  mode each round is IAR, EOIR, the handlers, DIR, and IAR again returns the
  same ID: the core never leaves the exception. The report's per-core
  interrupt count stays small and the GIC line reads `rpr 0xff … active [n]
  pending [n] line [n]` — priority dropped, handler running, not a lost
  deactivation. With the PCIe endpoint's INTA (175) forced high, core 0 acked
  it 1.5 million times this way. What should end the loop is `note_interrupt`:
  after 100 000 rounds of every handler returning `IRQ_NONE` it prints `irq
  27: nobody cared` and disables the line. It did not, because `pcie_pme_irq`
  returned `IRQ_HANDLED` every time: `pcie_pme_probe` clears the root port's
  `PCI_EXP_RTSTA` PME status with `pcie_capability_set_dword`, a
  read-modify-write that relies on the bit being write-one-to-clear, and the
  model's root-port config past the header was plain storage, so the clear
  set it for good. With the RW1C status bits modelled the same run prints
  `nobody cared`, disables IRQ 27 and carries on. The GIC was right throughout
  (`level_spi_split_eoi_is_acknowledged_again_after_each_dir` in
  `src/periph/gic.rs`).

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
5. **PCIe root complex** — `PCIe RC controller misconfigured as Endpoint`.
   `pcie-brcmstb` checks the port-mode bit of `MISC_PCIE_STATUS` with PERST#
   still asserted, and the model only reported it with the link up; it is a
   strap. Beyond that Linux needed a real type-1 header for the root port
   (seeded from `rpi-dev`'s config dump: no BARs, writable bus numbers and
   windows, the PCIe capability with the trained link's status), the SerDes
   MDIO port for spread spectrum, and the 32-vector MSI block at `+0x4500`
   (`src/periph/pcie.rs`). It now enumerates the bridge and the VL805 the way
   the real board does, `link up, 5.0 GT/s PCIe x1 (SSC)` included.

The run ended with the kernel idling in `cpu_do_idle` at `Waiting for root
device`, because the test image had no ext4 `p2` and the card's CSD packing
hid block reads from Linux.

## Root filesystem and shell (in-house core, milestone 5)

`scripts/make-sd.sh` now adds a 32 MiB ext4 `p2` after the boot partition,
holding Debian's static busybox, and `testdata/boot/linux-boot.toml` boots
through to its prompt on all four cores. Things that mattered:

- The boot partition's geometry and the card's size are both in the firmware
  golden (FAT cluster count, CSD), so `p1` keeps its old size and the card
  grows instead; the bootloader then also lists `p2` in its MBR dump.
- The firmware's command line ends in `console=tty1`, which makes the
  framebuffer `/dev/console`; the inittab names `ttyAMA0` explicitly.
- `mke2fs -d` copies the builder's uid/gid and random inode generations, and
  e2fsprogs 1.47 enables `orphan_file` (a read-only mount then logs an orphan
  cleanup); the script fixes all of those so the image is byte-reproducible.

## Console input (milestone 6)

- The PL011 receives: host bytes go onto a modelled line and enter the 32-entry
  FIFO one character time apart (the `IBRD`/`FBRD` divisor of a 48 MHz
  `UARTCLK`), against the modelled clock. `amba-pl011` enables `RXIM` and
  `RTIM`; `RXIS` follows the `IFLS` trigger level and `RTIS` fires after 32
  idle bit periods. Its line is GIC SPI 121 (ID 153).
- The transmit interrupt is never raised: the transmit FIFO never fills, so the
  PIO path writes everything at once and never waits for it.
- Input waits on the line while the FIFO is full or the receiver is off, rather
  than overrunning.
- `recon --send-after <prompt> <text>` types a line once the console prints the
  prompt; the Linux scenario uses it (`[[boot.input]]`), keyed to the
  transcript so the golden stays deterministic. ash's line editor prints the
  prompt and then `ESC [6n` (a cursor-position query) once its tty is raw; text
  sent before that would be echoed twice, once by the cooked tty and once by the
  editor, so the scenario waits for the query.
- `recon --stdin` is the interactive console: raw terminal, `Ctrl-A x` quits.
  A real terminal answers the `ESC [6n` itself, through stdin.

## UEFI (the rpi-mkosi image)

The rpi-mkosi image's `config.txt` names `RPI_EFI.fd` (edk2 for the Pi 4) as
the armstub, so the ARM runs UEFI before any kernel. What it needed:

1. Its xHCI driver reads the VL805's BAR through the PCIe outbound window at
   `0x6_0000_0000`, which the ARM bus did not route. The synchronous external
   abort left UEFI in its exception handler until start4's watchdog reset the
   board (`2f548e1`, found with `RVF_DBG_ARM_EXC`).
2. `wfe` was a no-op, so TF-A's holding pen kept cores 1-3 spinning for the
   whole boot (`1c58887`).
3. `DwUsbHostDxe`, edk2's driver for the DWC2 controller behind the USB-C port,
   asks the firmware for USB power (`SET_POWER_STATE`, device 3, on and wait)
   soon after the banner. start4's handler asks the block at `0x7E80_8000` for
   power, waits for its acknowledge, then resets the DWC2 core, and none of
   those waits has a timeout. Both blocks were on the catch-all stub, so the
   handler never returned: `RVF_DBG_MBOX` showed the VPU read the request, never
   answer it and never read another. UEFI then gave each later property
   request a second and looked hung after its banner (#49, `src/periph/hd.rs`).
4. With power on, the driver brings the controller up as a host and halts all
   eight host channels, waiting up to ten seconds, polled, for each one to
   halt. The DWC2 model answers the configuration words measured on `rpi-dev`,
   halts a channel at once and reports an empty root port
   (`src/periph/dwc2.rs`).
5. systemd-boot writes its random seed back to the ESP before it starts an
   entry. After every write, edk2's `MmcDxe` asks the card how many blocks it
   took (CMD55 + ACMD22, a 4-byte read) and fails the write without an answer.
   The card model had no ACMD22, so the Arasan driver waited for Buffer Read
   Ready until `EFI_TIMEOUT`, and systemd-boot stopped at `Error opening root
   path: Time out` (#51, `src/periph/sdcard.rs`).
6. The entry's UKI (89.7 MB) then loads through the same driver in PIO mode.
   Every 512-byte block costs two `SET_GPIO_STATE` mailbox round trips (the
   activity LED, around each block in `MMCReadBlockData`) and the driver's
   `Stall`s, and core 0 busy-waits through all of it: on the mailbox status
   word in `RpiFirmwareDxe`, and on the counter in `NanoSecondDelay`. At one
   instruction per cycle that was most of the host time, so the ARM side now
   parks a core in such a loop instead of stepping it, and rebuilds its state
   when the loop ends or what it reads changes (#53, `src/arm/park.rs`,
   "Busy-wait loops" in `src/arm.rs`).
7. That made the load quick in host time, but it still crawled in guest time
   — about 230 blocks a second — so start4's 16 s early watchdog
   (`dtparam=watchdog=on`, armed at `arm_loader`; edk2 never touches it)
   reset the board half a megabyte in, every boot. The model was the cause:
   when the VPU `sleep`s, the model jumped the system timer to the next
   compare and then ran the ARM up to it with the VPU frozen, so every
   mailbox request UEFI made in that slice waited for the slice to end —
   1.9 ms on average, measured by logging both clocks at each request. The
   ARM now runs such a slice first and stops at its first write to a
   VPU-side peripheral, and the counter moves only that far (#53,
   `ArmSide::run_until_store`).

With those, UEFI prints its boot manager prompt (`ESC (setup), F1 (shell),
ENTER (boot)`) 0.7 s of guest time after its banner, and systemd-boot no
longer times out on the SD card.

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
