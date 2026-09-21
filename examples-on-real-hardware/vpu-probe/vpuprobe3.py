#!/usr/bin/env python3
"""Run a VPU code blob, and say which of the 64 KiB changed while it ran.

Same as `vpuprobe2.py`, but every page of the allocation is read back before
and after the call and compared, so an instruction that writes somewhere other
than its stated destination is caught wherever it lands inside the buffer.

  sudo vpuprobe2.py <blob.b64> [@vectors.hex] [r2 r3 r4 r5]

Same as `vpuprobe.py`, but the mailbox call runs in a thread with a timeout:
an instruction that stalls the VPU no longer takes the output with it. When
the call does not come back the page is read and printed anyway, so a probe
can still say how far the blob got — which is the only way to learn anything
about an encoding that hangs the processor.

64 KiB of VC memory is allocated; the blob goes at offset 0 and is entered with
r0 = bus address of offset 0x1000. Offsets 0x1000..0x2000 are zeroed and come
back base64-encoded; 0x2000 upwards is filled with a per-word marker
(`0xA0000000 | offset`) for load probes. `/dev/vc-mem` maps one 4 KiB window at
a time, which is all it allows.
"""
import base64
import ctypes
import threading
import fcntl
import mmap
import os
import struct
import sys

IOCTL_MBOX_PROPERTY = 0xC0086400  # _IOWR(100, 0, char *), 8-byte pointer
MEM_ALLOC, MEM_LOCK, MEM_UNLOCK, MEM_FREE = 0x3000C, 0x3000D, 0x3000E, 0x3000F
EXECUTE_CODE = 0x30010
SIZE = 64 * 1024
TIMEOUT = 5  # seconds to wait for the blob before reading the page anyway
PAGE = 4096


class Mbox:
    def __init__(self):
        self.fd = os.open("/dev/vcio", os.O_RDWR)

    def call(self, tag, req, nresp):
        vals = list(req) + [0] * max(0, nresp - len(req))
        words = [0, 0, tag, len(vals) * 4, len(req) * 4] + vals + [0]
        words[0] = len(words) * 4
        buf = ctypes.create_string_buffer(struct.pack(f"<{len(words)}I", *words), len(words) * 4)
        fcntl.ioctl(self.fd, IOCTL_MBOX_PROPERTY, buf)
        out = struct.unpack(f"<{len(words)}I", buf.raw)
        if out[1] != 0x80000000:
            raise RuntimeError(f"mailbox refused the request: code {out[1]:#x}")
        if not out[4] & 0x80000000:
            raise RuntimeError(f"firmware did not answer tag {tag:#x} (len word {out[4]:#x})")
        return out[5 : 5 + nresp]


class VcMem:
    """One 4 KiB window at a time — more than that and the driver faults."""

    def __init__(self):
        self.fd = os.open("/dev/vc-mem", os.O_RDWR | os.O_SYNC)

    def read(self, phys):
        m = mmap.mmap(self.fd, PAGE, offset=phys)
        try:
            return bytes(m[:PAGE])
        finally:
            m.close()

    def write(self, phys, data):
        assert len(data) <= PAGE
        m = mmap.mmap(self.fd, PAGE, offset=phys)
        try:
            m[: len(data)] = data
        finally:
            m.close()


def main():
    blob = base64.b64decode(open(sys.argv[1], "rb").read())
    data = b""
    args = sys.argv[2:]
    if args and args[0].startswith("@"):
        data = bytes.fromhex(open(args[0][1:]).read().split())if False else bytes.fromhex("".join(open(args[0][1:]).read().split()))
        args = args[1:]
    args = [int(a, 0) for a in args]
    args += [0] * (5 - len(args))

    mb, vc = Mbox(), VcMem()
    handle = mb.call(MEM_ALLOC, [SIZE, PAGE, 0x4], 1)[0]
    if handle == 0:
        raise RuntimeError("mem_alloc failed")
    try:
        bus = mb.call(MEM_LOCK, [handle], 1)[0]
        phys = bus & ~0xC0000000
        print(f"handle {handle:#x} bus {bus:#010x} phys {phys:#010x}", flush=True)
        vc.write(phys, blob.ljust(PAGE, b"\0"))
        vc.write(phys + 0x1000, b"\0" * PAGE)
        for off in range(0x2000, SIZE, PAGE):
            page = bytes((i + 1) & 0xFF for i in range(PAGE))
            vc.write(phys + off, page)
        if data:
            vc.write(phys + 0x3000, data.ljust(PAGE, b"\0"))
        before = {off: vc.read(phys + off) for off in range(0, SIZE, PAGE)}
        result = {}

        def run():
            try:
                result["r0"] = mb.call(
                    EXECUTE_CODE, [bus, bus + 0x1000, bus + 0x2000, *args[:4]], 1
                )[0]
            except Exception as e:  # the mailbox refused it
                result["err"] = e

        worker = threading.Thread(target=run, daemon=True)
        worker.start()
        worker.join(TIMEOUT)
        stalled = worker.is_alive()
        # The page is ours either way: `/dev/vc-mem` reads it without asking
        # the firmware, which is the point.
        out = vc.read(phys + 0x1000)
        for off in range(0, SIZE, PAGE):
            if off == 0x1000:
                continue  # the dump page, which the probe writes on purpose
            now = vc.read(phys + off)
            if now != before[off]:
                diffs = [i for i in range(PAGE) if now[i] != before[off][i]]
                print(f"CHANGED page {off:#06x}: {len(diffs)} bytes, first at {diffs[0]:#x}")
                for i in diffs[:16]:
                    print(f"  {off + i:#07x}: {before[off][i]:02x} -> {now[i]:02x}")
        if stalled:
            print("STALLED: the blob did not return")
        elif "err" in result:
            print(f"mailbox error: {result['err']}")
        else:
            print(f"r0 {result['r0']:#010x}")
        print(base64.b64encode(out).decode())
        if stalled:
            # Freeing it would need the mailbox, which is gone with the VPU.
            os._exit(2)
    finally:
        mb.call(MEM_UNLOCK, [handle], 1)
        mb.call(MEM_FREE, [handle], 1)


main()
