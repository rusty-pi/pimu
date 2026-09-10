#!/usr/bin/env python3
"""Find every `bl`/`b` that targets a given address in a VC4 ELF.

Ghidra loses cross-references in start4.elf wherever its linear disassembly
desynced on inline data, and the built-in disassembler cannot xref for the same
reason. This decodes the two branch encodings that carry a full 32-bit target at
*every* 2-byte offset in the loadable segments, so it finds call sites the
decompilation does not list.

    scripts/vc4-xref.py 0x3ed64bde [0x...]

Addresses may be given either as the runtime address (`0x3E......`) or as the
ELF's own (`0x0E......`); both are matched, and printed in the ELF's form (the
`FUN_0e......` names Ghidra uses in `firmware/source/start4.elf.c`).
"""

import struct
import sys


def sext(v, bits):
    return v - (1 << bits) if v & (1 << (bits - 1)) else v


def segments(path):
    f = open(path, "rb").read()
    (e_phoff,) = struct.unpack_from("<I", f, 28)
    e_phentsize, e_phnum = struct.unpack_from("<HH", f, 42)
    for i in range(e_phnum):
        p = e_phoff + i * e_phentsize
        t, off, va, _pa, fs, _ms, _fl, _al = struct.unpack_from("<8I", f, p)
        if t == 1 and fs:
            yield va, f[off : off + fs]


def branches(va, b):
    """Yield (site, target, is_link) for every branch-with-32-bit-target."""
    for j in range(0, len(b) - 6, 2):
        pc = va + j
        hw0 = struct.unpack_from("<H", b, j)[0]
        # 1110 00/01/10/11 00 <imm32> : b / bl, absolute or pc-relative
        if hw0 in (0xE000, 0xE100, 0xE200, 0xE300):
            imm = struct.unpack_from("<H", b, j + 2)[0] | (
                struct.unpack_from("<H", b, j + 4)[0] << 16
            )
            target = imm if hw0 in (0xE000, 0xE200) else (pc + imm) & 0xFFFFFFFF
            yield pc, target, hw0 in (0xE200, 0xE300)
            continue
        # 1001 cccc 0 <off23> : b<cond>   |   1001 <hi4> 1 <off23> : bl (27-bit)
        if (hw0 & 0xF000) == 0x9000:
            w = (hw0 << 16) | struct.unpack_from("<H", b, j + 2)[0]
            if w & 0x0080_0000:
                o = (((w >> 24) & 0xF) << 23) | (w & 0x007F_FFFF)
                yield pc, (pc + sext(o, 27) * 2) & 0xFFFFFFFF, True
            else:
                yield pc, (pc + sext(w & 0x007F_FFFF, 23) * 2) & 0xFFFFFFFF, False


def main():
    if len(sys.argv) < 2:
        sys.exit(__doc__)
    path = "firmware/start4.elf"
    wanted = set()
    for a in sys.argv[1:]:
        if a.endswith(".elf"):
            path = a
            continue
        v = int(a, 16)
        wanted.add(v & 0x0FFF_FFFF)
    for va, b in segments(path):
        for site, target, link in branches(va, b):
            if (target & 0x0FFF_FFFF) in wanted:
                kind = "bl" if link else "b "
                print(f"0x{site & 0x0FFF_FFFF:08x}  {kind} 0x{target & 0x0FFF_FFFF:08x}")


if __name__ == "__main__":
    main()
