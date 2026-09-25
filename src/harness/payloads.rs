//! Hand-assembled VPU test payloads.
//!
//! Tiny programs written directly in machine code, so the bench has
//! deterministic, self-contained inputs that need no firmware. Each one doubles
//! as a worked example of the instruction encoding.

use crate::soc::bcm2711 as map;

const MU_IO: u32 = map::AUX_BASE + crate::spec::aux::MU_IO;

fn emit16(out: &mut Vec<u8>, h: u16) {
    out.extend_from_slice(&h.to_le_bytes());
}

fn emit32(out: &mut Vec<u8>, w: u32) {
    emit16(out, (w >> 16) as u16);
    emit16(out, w as u16);
}

fn mov_imm(out: &mut Vec<u8>, rd: u32, imm: u16) {
    emit32(out, 0xB000_0000 | (rd << 16) | imm as u32);
}
fn shl_imm(out: &mut Vec<u8>, rd: u32, imm: u16) {
    emit32(out, 0xB000_0000 | (28 << 21) | (rd << 16) | imm as u32);
}
fn or_imm(out: &mut Vec<u8>, rd: u32, imm: u16) {
    emit32(out, 0xB000_0000 | (13 << 21) | (rd << 16) | imm as u32);
}
/// `stb rs_val, (rbase)` — 16-bit `0000 1ww1 ssss dddd`, ww = byte = 2.
fn stb(out: &mut Vec<u8>, base: u16, val: u16) {
    emit16(
        out,
        0x0800 | (2 << 9) | 0x0100 | ((base & 0xF) << 4) | (val & 0xF),
    );
}
fn swi0(out: &mut Vec<u8>) {
    emit16(out, 0x01C0);
}
fn load_u32(out: &mut Vec<u8>, rd: u32, v: u32) {
    mov_imm(out, rd, (v >> 16) as u16);
    shl_imm(out, rd, 16);
    or_imm(out, rd, (v & 0xFFFF) as u16);
}

/// Emit `text` to the mini-UART a byte at a time, then `swi`.
pub fn print_and_halt(text: &str) -> Vec<u8> {
    let mut out = Vec::new();
    load_u32(&mut out, 0, MU_IO); // r0 = &MU_IO
    for &b in text.as_bytes() {
        mov_imm(&mut out, 1, b as u16); // r1 = byte
        stb(&mut out, 0, 1); // *(u8*)r0 = r1
    }
    swi0(&mut out);
    out
}

/// The canonical smoke payload.
pub fn hello_world() -> Vec<u8> {
    print_and_halt("hello from the vpu\n")
}

pub fn count_dots() -> Vec<u8> {
    let mut out = Vec::new();
    load_u32(&mut out, 0, MU_IO); // r0 = &MU_IO
    mov_imm(&mut out, 2, 5); // r2 = remaining
    mov_imm(&mut out, 3, 0); // r3 = 0 (for cmp)
    mov_imm(&mut out, 1, b'.' as u16); // r1 = '.'

    let loop_start = out.len();
    stb(&mut out, 0, 1); // print '.'
                         // sub r2, #1   -> AluImm p = sub = 6
    emit32(&mut out, 0xB000_0000 | (6 << 21) | (2 << 16) | 1);
    // cmp r2, r3   -> 16-bit Alu2 p = cmp = 10 : 010 01010 ssss dddd  (rd=r2, rs=r3)
    emit16(&mut out, 0x4000 | (10 << 8) | (3 << 4) | 2);
    // b<ne> loop_start  -> 16-bit 0001 1ccc cooo oooo, cond = ne = 1
    let insn_addr = out.len();
    let disp = (loop_start as i32 - insn_addr as i32) / 2; // in halfwords, from insn addr
    emit16(&mut out, 0x1800 | (1 << 7) | ((disp as u32) & 0x7F) as u16);

    swi0(&mut out);
    out
}

/// A builtin payload by the name a scenario file uses (`builtin:<name>`).
pub fn by_name(name: &str) -> Option<Vec<u8>> {
    match name {
        "hello" => Some(hello_world()),
        "count-dots" => Some(count_dots()),
        _ => None,
    }
}
