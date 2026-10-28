# USB mass storage

`--usb <img>` puts a SuperSpeed stick in blue socket A, on the VL805 (xHCI root
port 2). The model is in `src/periph/usb.rs`; the host controller side is
`src/periph/xhci.rs`.

| Option | Stick |
|---|---|
| `--usb <img>` | Bulk-Only Transport only, with the descriptors measured on a Raspberry Pi 4B d03115 and a Samsung "Flash Drive FIT". |
| `--usb <img> --usb-uas` | A UAS stick: one interface with Bulk-Only as alternate setting 0 and USB Attached SCSI as setting 1. |
| `--usb <img> --usb-uas-refuse` | The same, but the stick stalls `SET_INTERFACE` to setting 1, so a host has to stay on Bulk-Only. |

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
transfer. Several commands may be in flight under different tags. The SCSI
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

## Not reproduced

On a Raspberry Pi 4B d03115, Bulk-Only transfers from a Raspberry Pi flash
drive hang over the VL805, where the same drive works from a laptop. The cause
is unknown, so the model does not do it: `--usb` completes every transfer. UAS
sticks on the same board work.
