# USB mass storage

`--usb <img>` puts a SuperSpeed stick in blue socket A, on the VL805 (xHCI root
port 2). The model is in `src/periph/usb.rs`; the host controller side is
`src/periph/xhci.rs`.

| Option | Stick |
|---|---|
| `--usb <img>` | Bulk-Only Transport only, with the descriptors measured on a Raspberry Pi 4B d03115 and a Samsung "Flash Drive FIT". |
| `--usb <img> --usb-uas` | A UAS stick: one interface with Bulk-Only as alternate setting 0 and USB Attached SCSI as setting 1. |
| `--usb <img> --usb-uas-refuse` | The same, but the stick stalls `SET_INTERFACE` to setting 1, so a host has to stay on Bulk-Only. |
| `--usb <img> --usb-wedge host-silent:<seconds>` | Any of the above, but it refuses an address when the host first runs later than that after reset. See [A stick that wedges](#a-stick-that-wedges). |

`--otg` is always Bulk-Only.

## UAS

`UasStorage` is a SuperSpeed device whose one interface has two alternate
settings, as every UAS stick does:

* setting 0, `bInterfaceProtocol` `0x50`: bulk OUT `0x01`, bulk IN `0x82`,
  exactly the Bulk-Only stick (it is a `MassStorage` underneath).
* setting 1, `0x62`: command pipe (OUT `0x01`, pipe ID 1), status pipe (IN
  `0x82`, 2), data-in (IN `0x83`, 3) and data-out (OUT `0x04`, 4). Each endpoint
  has a SuperSpeed companion descriptor (burst 16; `MaxStreams` 0 on the command
  pipe, 4 on the other three, so 16 stream IDs) and a pipe usage descriptor.

A command is a 32-byte Command IU (`0x01`, tag at bytes 2..4 big-endian, CDB at
16) on the command pipe. The device answers on the status pipe with a Read
Ready (`0x06`) or Write Ready (`0x07`) IU when data has to move and, once it
has, a Sense IU (`0x03`, SCSI status at byte 6, sense data after the 16-byte
header on CHECK CONDITION). The tag is also the stream ID of everything that
belongs to the command: the status IUs, the data-in transfer and the data-out
transfer. Several commands may be in flight under different tags.

A WRITE gets its Write Ready IU only once a transfer is queued on the data-out
stream of its tag, as on a UAS stick (`2e8a:0030`) behind the VL805 of a
Raspberry Pi 4B d03115, which sends none until the host has the data queued. A
host that waits for the Write Ready IU before it queues the data hears nothing,
and a data-out transfer queued before the command is NAKed until the command
arrives. A read's data-in transfer is queued with the command in any case. The
device does not model the rest of what that stick did after such a timeout
(later reads went unanswered too): how its recovery failed is not known. The SCSI
answers are `MassStorage`'s: TEST UNIT READY, INQUIRY, READ CAPACITY(10), the
READ and WRITE families on the `Disk`, and the rest fail with CHECK CONDITION.

`SET_INTERFACE` back to setting 0, `SET_CONFIGURATION` and a bus reset all drop
back to Bulk-Only and forget any commands.

The descriptors are **not measured**: no UAS stick has been captured here. They
follow USB 3.2 section 9.6 and the UAS specification, with an invented identity
(`1209:0001`, "pimu" / "UAS Flash Drive").

## Streams in the xHCI model

An endpoint context with `MaxPStreams` (dword 0, bits 14:10) non-zero points at
an array of 2^(`MaxPStreams`+1) 16-byte stream contexts instead of at a
transfer ring. A doorbell's stream ID (bits 31:16) picks the context, whose
first two dwords are that stream's dequeue pointer, cycle bit and type; the ring
engine runs that ring, and writes the dequeue pointer back into the stream
context. Set TR Dequeue Pointer takes the stream ID from bits 31:16 of its third
dword. A doorbell for stream 0, or for a stream whose context is not a Primary
Transfer Ring (type 1), posts a Transfer Event with Invalid Stream ID Error.
Secondary stream arrays are not modelled, and `LSA` is not looked at: nuboot
leaves it clear and puts type-1 contexts straight into the array, and the VL805
on a Raspberry Pi 4B d03115 runs it that way. An endpoint without streams
ignores the stream ID, as before.

Configure Endpoint with the Deconfigure bit drops every endpoint but the
default one and returns the slot to Addressed; dropping an endpoint with the
Drop flags clears its whole context.

## Checked with nuboot

`rusty-pi/nuboot`'s UEFI firmware, branch `uefi-usb-uas` (UAS plus its fall back
to Bulk-Only), with its `scenarios/usb-hello.yaml` and the options above (the
scenario runner passes them on):

```bash
pimu boot --scenario scenarios/usb-hello.yaml --usb-uas          # driven through UAS
pimu boot --scenario scenarios/usb-hello.yaml --usb-uas-refuse   # falls back to Bulk-Only
pimu boot --scenario scenarios/usb-hello.yaml                    # Bulk-Only stick, as before
```

All three pass. With `--usb-uas` the firmware logs `drive driven through UAS`,
finds the disk, reads and verifies the application off it through UAS and runs
it. With `--usb-uas-refuse` it logs `UAS interface not selected` (the request
stalled) and `UAS did not work, using bulk-only`, and boots the same.

This needs a nuboot build, so it is not part of `cargo test`; the unit tests in
`usb.rs` and `xhci.rs` cover the IUs, the streams, `Set TR Dequeue Pointer` and
the deconfigure.

## A stick that wedges

A Raspberry Pi flash drive (`2e8a:0030`, UAS) behind the VL805 of a Raspberry
Pi 4B d03115 works after a power cycle and, after a warm reboot, sometimes
fails `Address Device` (Linux: `device not accepting address 2, error -62`).
It fails when the first host contact after the reset comes late. With signed
boot (`SIGNED_BOOT=1`, SD first) the bootloader makes no contact, so the first
one is Linux's xHCI probe; with `SIGNED_BOOT=0`, or USB first in `BOOT_ORDER`,
the bootloader enumerates the stick about 3 s after reset and the stick keeps
working however late Linux comes. On a Raspberry Pi 4B d03115 with a signed
16 MiB `boot.img` and `boot_delay` in `config.txt`, Linux's first xHCI access
is at 13.2 s (`boot_delay=1.5` and `1.75`, the stick works) and 13.5 s
(`boot_delay=2`, it wedges): the limit is about 13.4 s from the warm reset.
The same happens with every `start4.elf` and every bootloader tried that can
load a signed image. Why the stick does it is not known.

`--usb-wedge host-silent:<seconds>` models that and nothing else: the first time
the xHCI controller runs (`USBCMD.RS`) after the reset of the SoC is the first
the stick hears of a host, and when that is later than `<seconds>` every
`Address Device` for a SuperSpeed device fails with USB Transaction Error. A
controller run before the limit, or a later one, changes nothing, and the BCM2711's own xHCI
and USB 2.0 devices are not touched. It needs `--usb`, works with `--usb-uas`,
and is a command line option only.

The limit is model time, and the model's time is not the board's: the
bootloader's first stage takes about the same, but a 30 MiB `boot.img` takes 41 s
to verify (5.6 s on the board) and Linux reaches its first xHCI access 2.7 times
later. So the board's 13.4 s cannot be used; calibrate it per build. For the
shortest signed config, a 14 MiB `boot.img` of the official `start4.elf`,
`fixup4.dat`, device tree and `kernel8.img` signed with the test key
(`testdata/netboot/test-signing-key.pem`), SD first, Linux's first xHCI access
is at 36.2 s with `boot_delay=2` in `config.txt` and 2.0 s earlier with
`boot_delay=0`: a limit of `35.2` wedges the first and not the second. The same
card with `SIGNED_BOOT=0` runs the controller at 1.7 s, in the bootloader, and
never wedges, as the stick on the board does not. A signed boot only skips the
bootloader's early USB bring-up when `MBOX5` keeps the secure-boot mark; see
[`periph/mcsync.md`](periph/mcsync.md).

```bash
pimu boot --eeprom pieeprom-signed.bin --bootconf SIGNED_BOOT=1 \
  --sd card.img --usb stick.img --usb-wedge host-silent:35.2
```

`pieeprom-signed.bin` is the official EEPROM image with the test key's public
half in its `pubkey.bin` slot (`--eeprom-pubkey`), and `card.img` holds the
files as they are (read when `SIGNED_BOOT=0`) beside `boot.img` and `boot.sig`.

Unit tests in `xhci.rs` check the limit: the first run at the limit works, a
microsecond later wedges, and a later controller reset does not start the clock
again.

## Not reproduced

On a Raspberry Pi 4B d03115, Bulk-Only transfers from a Raspberry Pi flash
drive hang over the VL805, where the same drive works from a laptop. The cause
is unknown, so the model does not do it: `--usb` completes every transfer. UAS
sticks on the same board work.
