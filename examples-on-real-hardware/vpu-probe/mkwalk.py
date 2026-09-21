#!/usr/bin/env python3
"""mkwalk.py <name> <tag:insn> ... — a blob that says where it stopped.

Every step is preceded by a scalar store of its tag to the page's word at
4088, so a blob that stalls still names the instruction it stalled on: the
poll in `vpuprobe5.py` reads the page whether or not the blob comes back.
"""
import subprocess, sys

name, steps = sys.argv[1], sys.argv[2:]
body = ["\tmov r3,#64", "\tmov r4,r1", "\tadd r4,#4096", "\tv32mov HY(0++,0),0 REP64"]
for i in range(6):
    body.append(f"\tv32mov HY({i},0),-1\t\t; a destination preset, so a zero shows")
for s in steps:
    tag, ins = s.split(":", 1)
    body += [f"\tmov r2,#{tag}", "\tst r2,(r0+4088)", f"\t{ins}"]
body += ["\tmov r2,#99", "\tst r2,(r0+4088)",
         "\tv32st HY(0++,0),(r0+=r3) REP64",
         "\tmov r2,#0x5a5aa5a5", "\tst r2,(r0+4092)", "\trts"]
open(f"{name}.s", "w").write("\t.text\n\t.global _start\n_start:\n" + "\n".join(body) + "\n")
for cmd in ([f"vc4-elf-as", "-o", f"{name}.o", f"{name}.s"],
            ["vc4-elf-objcopy", "-O", "binary", f"{name}.o", f"{name}.bin"]):
    subprocess.run(cmd, check=True)
open(f"{name}.b64", "w").write(subprocess.run(["base64", "-w0", f"{name}.bin"],
                                              capture_output=True, text=True).stdout)
print(f"{name} ok")
