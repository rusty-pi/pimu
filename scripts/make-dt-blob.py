#!/usr/bin/env python3
"""Compile a Raspberry Pi ``dt-blob`` source into a flattened device tree.

The VideoCore boot firmware in ``start4.elf`` configures its GPIO manager
(``gpioman``) from a ``dt-blob`` — a small FDT with a ``/videocore`` schema,
distinct from the ARM device tree. When ``/mfs/sd/dt-blob.bin`` is absent the
firmware falls back to a built-in default; the model does not reproduce that
fallback yet (the pin-provider list at ``[gp+807672]`` never gets populated),
so ``gpioman`` never becomes ready and the boot wedges retrying it forever.

Shipping a real ``dt-blob.bin`` on the SD sidesteps that: the firmware reads
the file, parses it through the same path, and ``gpioman`` comes up.

``dtc`` is not always installed, and ``dt-blob.dts`` only uses a tiny subset of
DTS syntax (string / u32-cell / empty properties, unit-address node names, no
includes / phandles / labels / byte-strings), so this is a self-contained
compiler for exactly that subset. Source: ``raspberrypi/firmware`` →
``extra/dt-blob.dts`` (vendored as ``firmware/dt-blob.dts``).
"""

from __future__ import annotations

import struct
import sys
from pathlib import Path

FDT_MAGIC = 0xD00DFEED
FDT_BEGIN_NODE = 0x1
FDT_END_NODE = 0x2
FDT_PROP = 0x3
FDT_NOP = 0x4
FDT_END = 0x9


class Node:
    def __init__(self, name: str) -> None:
        self.name = name
        self.props: list[tuple[str, bytes]] = []
        self.children: list[Node] = []


def strip_comments(text: str) -> str:
    out: list[str] = []
    i, n = 0, len(text)
    while i < n:
        c = text[i]
        if c == "/" and i + 1 < n and text[i + 1] == "*":
            j = text.find("*/", i + 2)
            i = n if j < 0 else j + 2
        elif c == "/" and i + 1 < n and text[i + 1] == "/":
            j = text.find("\n", i + 2)
            i = n if j < 0 else j
        elif c == '"':
            j = i + 1
            while j < n and text[j] != '"':
                j += 2 if text[j] == "\\" else 1
            out.append(text[i : j + 1])
            i = j + 1
        else:
            out.append(c)
            i += 1
    return "".join(out)


def tokenize(text: str) -> list[str]:
    toks: list[str] = []
    i, n = 0, len(text)
    while i < n:
        c = text[i]
        if c.isspace():
            i += 1
        elif c in "{};=<>,":
            toks.append(c)
            i += 1
        elif c == '"':
            j = i + 1
            buf = []
            while j < n and text[j] != '"':
                if text[j] == "\\":
                    buf.append(text[j + 1])
                    j += 2
                else:
                    buf.append(text[j])
                    j += 1
            toks.append('"' + "".join(buf) + '"')
            i = j + 1
        else:
            j = i
            while j < n and not text[j].isspace() and text[j] not in "{};=<>,":
                j += 1
            toks.append(text[i:j])
            i = j
    return toks


def parse_u32(tok: str) -> int:
    v = int(tok, 0)
    if not 0 <= v <= 0xFFFFFFFF:
        raise ValueError(f"cell out of range: {tok}")
    return v


def encode_value(toks: list[str], pos: int) -> tuple[bytes, int]:
    """Parse a property value starting at ``toks[pos]`` (just past ``=``)."""
    parts: list[bytes] = []
    while True:
        t = toks[pos]
        if t.startswith('"'):
            parts.append(t[1:-1].encode() + b"\0")
            pos += 1
        elif t == "<":
            pos += 1
            cells: list[int] = []
            while toks[pos] != ">":
                cells.append(parse_u32(toks[pos]))
                pos += 1
            pos += 1  # ">"
            parts.append(b"".join(struct.pack(">I", c) for c in cells))
        else:
            raise SyntaxError(f"unexpected value token {t!r}")
        if toks[pos] == ",":
            pos += 1
            continue
        if toks[pos] == ";":
            return b"".join(parts), pos + 1
        raise SyntaxError(f"expected ',' or ';' after value, got {toks[pos]!r}")


def parse_node(toks: list[str], pos: int, name: str) -> tuple[Node, int]:
    node = Node(name)
    assert toks[pos] == "{"
    pos += 1
    while toks[pos] != "}":
        ident = toks[pos]
        pos += 1
        if toks[pos] == "{":
            child, pos = parse_node(toks, pos, ident)
            node.children.append(child)
            assert toks[pos] == ";"
            pos += 1
        elif toks[pos] == ";":
            node.props.append((ident, b""))
            pos += 1
        elif toks[pos] == "=":
            value, pos = encode_value(toks, pos + 1)
            node.props.append((ident, value))
        else:
            raise SyntaxError(f"unexpected token after {ident!r}: {toks[pos]!r}")
    return node, pos + 1  # past "}"


def parse_dts(text: str) -> Node:
    toks = tokenize(strip_comments(text))
    pos = 0
    while pos < len(toks) and toks[pos] != "/":
        pos += 1
    # skip "/dts-v1/;" and any other "/ ... ;" version/keyword lines
    while toks[pos] == "/" and toks[pos + 1] != "{":
        while toks[pos] != ";":
            pos += 1
        pos += 1
    assert toks[pos] == "/" and toks[pos + 1] == "{", "no root node"
    root, pos = parse_node(toks, pos + 1, "")
    return root


class StringTable:
    def __init__(self) -> None:
        self.blob = bytearray()
        self.offsets: dict[str, int] = {}

    def add(self, s: str) -> int:
        if s not in self.offsets:
            self.offsets[s] = len(self.blob)
            self.blob += s.encode() + b"\0"
        return self.offsets[s]


def pad4(b: bytes | bytearray) -> bytes:
    return bytes(b) + b"\0" * (-len(b) % 4)


def flatten(root: Node) -> bytes:
    strings = StringTable()
    struct_block = bytearray()

    def emit(node: Node) -> None:
        struct_block.extend(struct.pack(">I", FDT_BEGIN_NODE))
        struct_block.extend(pad4(node.name.encode() + b"\0"))
        for pname, pval in node.props:
            struct_block.extend(
                struct.pack(">III", FDT_PROP, len(pval), strings.add(pname))
            )
            struct_block.extend(pad4(pval))
        for child in node.children:
            emit(child)
        struct_block.extend(struct.pack(">I", FDT_END_NODE))

    emit(root)
    struct_block.extend(struct.pack(">I", FDT_END))

    rsvmap = struct.pack(">QQ", 0, 0)  # single terminator entry
    header_size = 40
    off_rsvmap = header_size
    off_struct = off_rsvmap + len(rsvmap)
    off_strings = off_struct + len(struct_block)
    total = off_strings + len(strings.blob)

    header = struct.pack(
        ">IIIIIIIIII",
        FDT_MAGIC,
        total,
        off_struct,
        off_strings,
        off_rsvmap,
        17,  # version
        16,  # last_comp_version
        0,  # boot_cpuid_phys
        len(strings.blob),
        len(struct_block),
    )
    return header + rsvmap + bytes(struct_block) + bytes(strings.blob)


def main(argv: list[str]) -> int:
    if len(argv) != 3:
        print(f"usage: {argv[0]} <dt-blob.dts> <dt-blob.bin>", file=sys.stderr)
        return 2
    src = Path(argv[1]).read_text()
    blob = flatten(parse_dts(src))
    Path(argv[2]).write_bytes(blob)
    print(f"wrote {argv[2]}  ({len(blob)} bytes)")
    return 0


if __name__ == "__main__":
    raise SystemExit(main(sys.argv))
