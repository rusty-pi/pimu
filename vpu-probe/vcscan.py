#!/usr/bin/env python3
"""Scan VC memory for the firmware's log text, without the mailbox.

A wedged firmware answers nothing, but `/dev/vc-mem` still reads its RAM. The
ioctls give the window; this walks it a page at a time and prints runs of
printable text, which is where the log ring and any assert message live.
"""
import fcntl, mmap, os, re, struct, sys

PAGE = 4096
IOC_PHYS = 0x80087600  # _IOR('v', 0, unsigned long)
IOC_SIZE = 0x80047601  # _IOR('v', 1, unsigned int)
IOC_BASE = 0x80047602  # _IOR('v', 2, unsigned int)

fd = os.open("/dev/vc-mem", os.O_RDWR | os.O_SYNC)
buf = bytearray(8)
fcntl.ioctl(fd, IOC_PHYS, buf)
phys = struct.unpack("<Q", bytes(buf))[0]
b4 = bytearray(4)
fcntl.ioctl(fd, IOC_SIZE, b4)
size = struct.unpack("<I", bytes(b4))[0]
fcntl.ioctl(fd, IOC_BASE, b4)
base = struct.unpack("<I", bytes(b4))[0]
print(f"vc-mem phys {phys:#x} base {base:#x} size {size:#x}", flush=True)

want = re.compile(rb"[ -~]{24,}")
pat = sys.argv[1].encode() if len(sys.argv) > 1 else None
start, end = base, base + size
hits = 0
for off in range(start, end, PAGE):
    try:
        m = mmap.mmap(fd, PAGE, offset=off)
    except (OSError, ValueError):
        continue
    try:
        page = bytes(m[:PAGE])
    finally:
        m.close()
    for s in want.findall(page):
        if pat and pat.lower() not in s.lower():
            continue
        print(f"{off:#010x} {s.decode(errors='replace')[:160]}")
        hits += 1
        if hits > 400:
            sys.exit(0)
