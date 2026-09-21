#!/usr/bin/env python3
"""Run a blob and say which of the firmware's own memory changed.

  sudo vpudiag.py <blob.b64> [@vectors.hex]

Snapshots the first SPAN of VC memory, runs the blob through EXECUTE_CODE with
a timeout, snapshots again and reports the pages that differ. A firmware that
dies right after an instruction is usually a firmware whose memory that
instruction wrote on, and this is what says whether that is what happened.
"""
import base64, ctypes, fcntl, mmap, os, struct, sys, threading

IOCTL_MBOX_PROPERTY = 0xC0086400
MEM_ALLOC, MEM_LOCK, MEM_UNLOCK, MEM_FREE = 0x3000C, 0x3000D, 0x3000E, 0x3000F
EXECUTE_CODE = 0x30010
IOC_BASE = 0x80047602
SIZE = 64 * 1024
PAGE = 4096
SPAN = 96 << 20  # the firmware's own region, generously
TIMEOUT = 5


class Mbox:
    def __init__(self):
        self.fd = os.open("/dev/vcio", os.O_RDWR)

    def call(self, tag, req, nresp):
        vals = list(req) + [0] * max(0, nresp - len(req))
        words = [0, 0, tag, len(vals) * 4, len(req) * 4] + vals + [0]
        words[0] = len(words) * 4
        buf = ctypes.create_string_buffer(
            struct.pack(f"<{len(words)}I", *words), len(words) * 4
        )
        fcntl.ioctl(self.fd, IOCTL_MBOX_PROPERTY, buf)
        out = struct.unpack(f"<{len(words)}I", buf.raw)
        if out[1] != 0x80000000:
            raise RuntimeError(f"mailbox refused the request: {out[1]:#x}")
        return out[5 : 5 + nresp]


vc = os.open("/dev/vc-mem", os.O_RDWR | os.O_SYNC)
b4 = bytearray(4)
fcntl.ioctl(vc, IOC_BASE, b4)
vcbase = struct.unpack("<I", bytes(b4))[0]


def read(phys, n=PAGE):
    m = mmap.mmap(vc, n, offset=phys)
    try:
        return bytes(m[:n])
    finally:
        m.close()


def write(phys, data):
    m = mmap.mmap(vc, PAGE, offset=phys)
    try:
        m[: len(data)] = data
    finally:
        m.close()


def snap(skip):
    out = {}
    for off in range(vcbase, vcbase + SPAN, PAGE):
        if skip[0] <= off < skip[1]:
            continue
        try:
            out[off] = read(off)
        except (OSError, ValueError):
            pass
    return out


blob = base64.b64decode(open(sys.argv[1], "rb").read())
data = b""
args = sys.argv[2:]
if args and args[0].startswith("@"):
    data = bytes.fromhex("".join(open(args[0][1:]).read().split()))
    args = args[1:]
args = [int(a, 0) for a in args] + [0] * 4

mb = Mbox()
handle = mb.call(MEM_ALLOC, [SIZE, PAGE, 0x4], 1)[0]
bus = mb.call(MEM_LOCK, [handle], 1)[0]
phys = bus & ~0xC0000000
print(f"buffer at {phys:#x}, vc-mem base {vcbase:#x}", flush=True)
write(phys, blob.ljust(PAGE, b"\0"))
write(phys + 0x1000, b"\0" * PAGE)
for off in range(0x2000, SIZE, PAGE):
    write(phys + off, bytes((i + 1) & 0xFF for i in range(PAGE)))
if data:
    write(phys + 0x3000, data.ljust(PAGE, b"\0"))

skip = (phys, phys + SIZE)
before = snap(skip)
print(f"snapshot: {len(before)} pages", flush=True)

result = {}


def run():
    try:
        result["r0"] = mb.call(
            EXECUTE_CODE, [bus, bus + 0x1000, bus + 0x2000, *args[:4]], 1
        )[0]
    except Exception as e:
        result["err"] = e


worker = threading.Thread(target=run, daemon=True)
worker.start()
worker.join(TIMEOUT)
print("STALLED" if worker.is_alive() else f"returned: {result}", flush=True)

after = snap(skip)
changed = [o for o in before if o in after and before[o] != after[o]]


def ndiff(o):
    a, b = before[o], after[o]
    return sum(1 for i in range(PAGE) if a[i] != b[i])


changed.sort(key=ndiff, reverse=True)
print(f"{len(changed)} pages changed, biggest first")
# Anything of ours that leaked into firmware memory would carry one of these.
sigs = {
    "probe vector data": bytes.fromhex("ffff008000f0f00f"),
    "marker page run": bytes(range(0x11, 0x21)),
    "blob first bytes": blob[:8],
}
for name, sig in sigs.items():
    for o in changed:
        if sig in after[o] and sig not in before[o]:
            print(f"  !! {name} appeared in page {o:#010x}")
def ascii_(b):
    return "".join(chr(c) if 32 <= c < 127 else "." for c in b)


for off in changed[:12]:
    a, b = before[off], after[off]
    idx = [i for i in range(PAGE) if a[i] != b[i]]
    print(f"  {off:#010x} {len(idx):5} bytes, first {idx[0]:#05x}")
    lo = max(0, (idx[0] // 16) * 16)
    for row in range(lo, min(PAGE, lo + 64), 16):
        print(f"      +{row:#05x} was {a[row:row+16].hex()} |{ascii_(a[row:row+16])}|")
        print(f"             now {b[row:row+16].hex()} |{ascii_(b[row:row+16])}|")
print(base64.b64encode(read(phys + 0x1000)).decode())
