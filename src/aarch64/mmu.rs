//! Stage 1 address translation (#40, milestone 3): the VMSAv8-64 table walk
//! for the EL1&0, EL2 and EL3 regimes, a TLB, and the faults Linux's memory
//! management depends on.
//!
//! Every data access and instruction fetch of the interpreter goes through
//! [`Cpu::read`] / [`Cpu::write`] / [`Cpu::fetch`], which translate when the
//! current regime has `SCTLR_ELx.M` set and pass the address through
//! unchanged otherwise. What is modelled, per ARMv8.0 (ARM ARM D5):
//!
//! - 4 KiB and 64 KiB granules (the A72's; `ID_AA64MMFR0_EL1.TGran16 = 0`, so
//!   a 16 KiB setting is treated as 4 KiB), `TxSZ` 16..=39, block and page
//!   descriptors, the two `EL1&0` ranges (`TTBR0`/`TTBR1`, `EPDx`), top-byte
//!   ignore, the output-size check against `IPS`/`PS` capped at the A72's
//!   44 bits.
//! - Permissions: `AP`, `UXN`/`PXN`/`XN`, the hierarchical table bits,
//!   `SCTLR_ELx.WXN`, and the rule that EL0-writable memory is never
//!   privileged-executable. `LDTR`/`STTR` check EL0 permissions from EL1.
//! - The access flag is never set by hardware (no `HAFDBS` before v8.1): a
//!   descriptor with `AF = 0` faults, and Linux sets it itself.
//! - Fault status codes with the level, `WnR`, and the virtual address in
//!   `FAR`.
//! - `AT` into `PAR_EL1`.
//!
//! The TLB is direct-mapped, one entry per 4 KiB page, and holds only
//! successful walks. It is flushed wholesale by every `TLBI` and whenever a
//! register that shapes translation changes (`SCTLR`, `TCR`, `TTBRx`,
//! `HCR_EL2`, `SCR_EL3`); dropping entries early is always allowed, so this
//! is exact for any guest that follows the architecture's maintenance rules.
//!
//! Not modelled: stage 2 (`HCR_EL2.VM` stops the run in [`Cpu::step_system`]),
//! memory types (so no alignment faults on Device memory, and `SCTLR.A` is
//! ignored), big-endian translation tables, and the contiguous hint, which
//! only affects TLB usage.

use super::cpu::{Cpu, Exception, Memory};
use super::exec::Stop;
use super::sysreg::{key, SysRegs, HCR_DC, HCR_TGE, SCR_NS, SCTLR_M};

/// Fault status codes (`ESR_ELx.{I,D}FSC`, `PAR_EL1.FST`). The first four
/// take the table level in the low two bits.
pub const FSC_ADDR_SIZE: u8 = 0x00;
pub const FSC_TRANSLATION: u8 = 0x04;
pub const FSC_ACCESS_FLAG: u8 = 0x08;
pub const FSC_PERMISSION: u8 = 0x0C;
pub const FSC_EXTERNAL: u8 = 0x10;
pub const FSC_WALK_EXTERNAL: u8 = 0x14;
pub const FSC_ALIGNMENT: u8 = 0x21;

/// `SCTLR_ELx.WXN`.
const SCTLR_WXN: u64 = 1 << 19;

/// The A72's physical address size (`ID_AA64MMFR0_EL1.PARange = 44 bits`).
const PA_BITS_MAX: u32 = 44;
const PA_MASK: u64 = (1 << 48) - 1;

/// What an access wants to do; the bit is its permission in [`Entry::perms`]
/// for EL0, shifted by 3 for the regime's own (privileged) EL.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Kind {
    Read = 1,
    Write = 2,
    Fetch = 4,
}

const PRIV: u32 = 3;

#[derive(Debug, Clone, Copy, Default)]
struct Entry {
    /// `(page << 2) | regime`; the regime is never 0, so a zeroed entry never
    /// matches.
    tag: u64,
    generation: u32,
    /// [`Kind`] bits: EL0's in 0..3, the regime EL's in 3..6.
    perms: u8,
    level: u8,
    /// The physical 4 KiB page.
    pa: u64,
}

const TLB_ENTRIES: usize = 1024;

/// A direct-mapped TLB (module docs).
#[derive(Clone)]
pub struct Tlb {
    generation: u32,
    entries: Box<[Entry]>,
    pub hits: u64,
    pub walks: u64,
    /// Set by a `TLBI`: the machine is to flush the other cores' TLBs too.
    pub broadcast: bool,
    /// The page the last instruction came from, as `(VA page, EL, PA page)`.
    ///
    /// Consecutive fetches are nearly always from the same page, and this
    /// lets [`Cpu::fetch`] skip the whole of `translate` for them (#43). It is
    /// only as good as the translation it was made from, so [`Tlb::flush`]
    /// drops it with everything else — which covers every register that
    /// shapes translation — and the EL is part of the key because it picks
    /// the regime and the permissions.
    fetch: Option<(u64, u32, u64)>,
}

impl Default for Tlb {
    fn default() -> Self {
        Self::new()
    }
}

impl Tlb {
    pub fn new() -> Tlb {
        Tlb {
            generation: 1,
            entries: vec![Entry::default(); TLB_ENTRIES].into_boxed_slice(),
            hits: 0,
            walks: 0,
            broadcast: false,
            fetch: None,
        }
    }

    /// Forget everything.
    pub fn flush(&mut self) {
        self.fetch = None;
        self.generation = self.generation.wrapping_add(1);
        if self.generation == 0 {
            self.entries.fill(Entry::default());
            self.generation = 1;
        }
    }

    fn slot(tag: u64) -> usize {
        (tag >> 2) as usize & (TLB_ENTRIES - 1)
    }

    fn get(&self, tag: u64) -> Option<Entry> {
        let e = self.entries[Self::slot(tag)];
        (e.tag == tag && e.generation == self.generation).then_some(e)
    }

    fn put(&mut self, e: Entry) {
        self.entries[Self::slot(e.tag)] = Entry {
            generation: self.generation,
            ..e
        };
    }
}

/// A successful walk.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Leaf {
    /// The physical address `va` translates to.
    oa: u64,
    perms: u8,
    level: u8,
    /// `AttrIndx`, `SH`, `NS` of the final descriptor.
    attr_index: u8,
    sh: u8,
    ns: bool,
}

/// Is stage 1 on for `regime` (1 = EL1&0, 2, 3)? With `HCR_EL2.{TGE,DC}` set
/// the non-secure EL1&0 regime runs with it off (D5.2.9).
fn stage1_on(s: &SysRegs, regime: u8) -> bool {
    if regime == 1 && s.scr_el3 & SCR_NS != 0 && s.hcr_el2 & (HCR_TGE | HCR_DC) != 0 {
        return false;
    }
    s.sctlr[regime as usize] & SCTLR_M != 0
}

/// `va` as the walk sees it: with top-byte-ignore on, bits 63..56 are
/// replaced by what the range needs (a copy of bit 55 for EL1&0, zeroes for
/// the single-range regimes).
fn untag(s: &SysRegs, regime: u8, va: u64) -> u64 {
    let tcr = s.tcr[regime as usize];
    if regime == 1 {
        let tbi = if va >> 55 & 1 == 0 { 37 } else { 38 };
        if tcr >> tbi & 1 != 0 {
            return ((va << 8) as i64 >> 8) as u64;
        }
    } else if tcr >> 20 & 1 != 0 {
        return va & 0x00FF_FFFF_FFFF_FFFF;
    }
    va
}

/// `TCR.{I}PS` to a number of bits, capped at what the A72 implements.
fn pa_bits(ps: u64) -> u32 {
    [32, 36, 40, 42, 44, 48, 48, 48][(ps & 7) as usize].min(PA_BITS_MAX)
}

/// The translation table walk (ARM ARM `AArch64.TranslationTableWalk` and
/// `AArch64.CheckPermission`, v8.0). `va` is already untagged. Errors are
/// fault status codes.
#[inline(never)]
fn walk<M: Memory + ?Sized>(s: &SysRegs, mem: &mut M, regime: u8, va: u64) -> Result<Leaf, u8> {
    let r = regime as usize;
    let tcr = s.tcr[r];
    let upper = regime == 1 && va >> 55 & 1 != 0;
    // (TTBR, TxSZ, granule bits, EPDx, output size)
    let (ttbr, tsz, granule, disabled, ps) = if regime != 1 {
        let g = if tcr >> 14 & 3 == 1 { 16 } else { 12 };
        (s.ttbr0[r], tcr & 63, g, false, tcr >> 16 & 7)
    } else if !upper {
        let g = if tcr >> 14 & 3 == 1 { 16 } else { 12 };
        (s.ttbr0[1], tcr & 63, g, tcr >> 7 & 1 != 0, tcr >> 32 & 7)
    } else {
        let g = if tcr >> 30 & 3 == 3 { 16 } else { 12 };
        (
            s.ttbr1_el1,
            tcr >> 16 & 63,
            g,
            tcr >> 23 & 1 != 0,
            tcr >> 32 & 7,
        )
    };
    let inputsize = 64 - tsz.clamp(16, 39) as u32;
    let outputsize = pa_bits(ps);
    let high = va >> inputsize;
    let in_range = if upper {
        high == u64::MAX >> inputsize
    } else {
        high == 0
    };
    if !in_range || disabled {
        return Err(FSC_TRANSLATION);
    }

    let stride = granule - 3;
    let start = 4 - (inputsize - granule).div_ceil(stride);
    let mut level = start;
    let lsb_of = |level: u32| granule + stride * (3 - level);
    let first_bits = inputsize - lsb_of(start);
    let mut table = ttbr & PA_MASK & !((8u64 << first_bits) - 1);
    if table >> outputsize != 0 {
        return Err(FSC_ADDR_SIZE);
    }

    // Hierarchical attributes: no EL0 access, read-only, UXN (XN), PXN.
    let (mut no_el0, mut ro, mut uxn, mut pxn) = (false, false, false, false);
    let two_ranges = regime == 1;
    loop {
        let lsb = lsb_of(level);
        let width = if level == start { first_bits } else { stride };
        let index = (va >> lsb) & ((1 << width) - 1);
        let lvl = level as u8;
        let desc = mem
            .read(table + index * 8, 8)
            .map_err(|_| FSC_WALK_EXTERNAL | lvl)?;
        if desc & 1 == 0 {
            return Err(FSC_TRANSLATION | lvl);
        }
        if level < 3 && desc & 2 != 0 {
            table = desc & PA_MASK & !((1 << granule) - 1);
            if table >> outputsize != 0 {
                return Err(FSC_ADDR_SIZE | lvl);
            }
            ro |= desc >> 62 & 1 != 0;
            uxn |= desc >> 60 & 1 != 0;
            if two_ranges {
                no_el0 |= desc >> 61 & 1 != 0;
                pxn |= desc >> 59 & 1 != 0;
            }
            level += 1;
            continue;
        }
        // A page at level 3, or a block where the granule allows one.
        let block_ok = match granule {
            12 => level == 1 || level == 2,
            _ => level == 2,
        };
        if (level == 3) != (desc & 2 != 0) || (level < 3 && !block_ok) {
            return Err(FSC_TRANSLATION | lvl);
        }
        let oa = (desc & PA_MASK & !((1 << lsb) - 1)) | (va & ((1 << lsb) - 1));
        if oa >> outputsize != 0 {
            return Err(FSC_ADDR_SIZE | lvl);
        }
        if desc >> 10 & 1 == 0 {
            return Err(FSC_ACCESS_FLAG | lvl);
        }
        let ap2 = desc >> 7 & 1 != 0 || ro;
        let wxn = s.sctlr[r] & SCTLR_WXN != 0;
        let perms = if two_ranges {
            let el0 = desc >> 6 & 1 != 0 && !no_el0;
            let uxn = uxn || desc >> 54 & 1 != 0;
            let pxn = pxn || desc >> 53 & 1 != 0;
            let user_w = el0 && !ap2;
            let priv_w = !ap2;
            // v8.0: EL0 may execute what it cannot read (execute-only).
            let user_x = !(uxn || (user_w && wxn));
            let priv_x = !(pxn || (priv_w && wxn) || user_w);
            perm_bits(el0, user_w, user_x) | perm_bits(true, priv_w, priv_x) << PRIV
        } else {
            let w = !ap2;
            let x = !(uxn || desc >> 54 & 1 != 0 || (w && wxn));
            perm_bits(true, w, x) << PRIV
        };
        return Ok(Leaf {
            oa,
            perms,
            level: lvl,
            attr_index: (desc >> 2 & 7) as u8,
            sh: (desc >> 8 & 3) as u8,
            ns: desc >> 5 & 1 != 0,
        });
    }
}

/// See [`Cpu::translate_access`].
type Translated = (u64, Option<(u64, u64)>);

fn perm_bits(r: bool, w: bool, x: bool) -> u8 {
    let bit = |on: bool, k: Kind| if on { k as u8 } else { 0 };
    bit(r, Kind::Read) | bit(w, Kind::Write) | bit(x, Kind::Fetch)
}

impl Cpu {
    /// The translation regime the core's accesses use now, if stage 1 is on
    /// for it.
    pub(super) fn regime(&self) -> Option<u8> {
        let r = self.el.max(1) as u8;
        stage1_on(&self.sys, r).then_some(r)
    }

    /// Translate `va` for an access of `kind`; errors are fault status codes.
    #[inline(always)]
    fn translate<M: Memory + ?Sized>(
        &mut self,
        mem: &mut M,
        va: u64,
        kind: Kind,
    ) -> Result<u64, u8> {
        let Some(regime) = self.regime() else {
            return Ok(va);
        };
        let user = regime == 1 && (self.el == 0 || (self.unprivileged && kind != Kind::Fetch));
        let va_t = untag(&self.sys, regime, va);
        let tag = (va_t >> 12) << 2 | u64::from(regime);
        let e = match self.tlb.get(tag) {
            Some(e) => {
                self.tlb.hits += 1;
                e
            }
            None => {
                self.tlb.walks += 1;
                let leaf = walk(&self.sys, mem, regime, va_t)?;
                let e = Entry {
                    tag,
                    generation: 0,
                    perms: leaf.perms,
                    level: leaf.level,
                    pa: leaf.oa & !0xFFF,
                };
                self.tlb.put(e);
                e
            }
        };
        let need = (kind as u8) << if user { 0 } else { PRIV };
        if e.perms & need == 0 {
            return Err(FSC_PERMISSION | e.level);
        }
        Ok(e.pa | (va & 0xFFF))
    }

    /// Translate both pages of an access, before touching either: the first
    /// physical address, and for an access that crosses into the next page,
    /// how many bytes are in the first and where the rest go. Errors are
    /// `(faulting VA, fault status code)`.
    #[inline(always)]
    fn translate_access<M: Memory + ?Sized>(
        &mut self,
        mem: &mut M,
        va: u64,
        size: u32,
        kind: Kind,
    ) -> Result<Translated, (u64, u8)> {
        let first = self.translate(mem, va, kind).map_err(|f| (va, f))?;
        let in_page = 0x1000 - (va & 0xFFF);
        if u64::from(size) <= in_page || self.regime().is_none() {
            return Ok((first, None));
        }
        let next = va.wrapping_add(in_page);
        let second = self.translate(mem, next, kind).map_err(|f| (next, f))?;
        Ok((first, Some((in_page, second))))
    }

    /// A data read of `size` bytes at virtual address `va`.
    #[inline(always)]
    pub(super) fn read<M: Memory + ?Sized>(
        &mut self,
        mem: &mut M,
        va: u64,
        size: u32,
    ) -> Result<u64, Stop> {
        let abort = |addr: u64, fsc: u8| {
            Stop::Exception(Exception::DataAbort {
                addr,
                write: false,
                fsc,
            })
        };
        let (pa, split) = self
            .translate_access(mem, va, size, Kind::Read)
            .map_err(|(a, f)| abort(a, f))?;
        self.last_pa = pa;
        let Some((in_page, pa2)) = split else {
            return mem.read(pa, size).map_err(|_| {
                self.abort_pa = pa;
                abort(va, FSC_EXTERNAL)
            });
        };
        let mut v = 0;
        for i in 0..u64::from(size) {
            let p = if i < in_page {
                pa + i
            } else {
                pa2 + i - in_page
            };
            let b = mem.read(p, 1).map_err(|_| {
                self.abort_pa = p;
                abort(va + i, FSC_EXTERNAL)
            })?;
            v |= b << (8 * i);
        }
        Ok(v)
    }

    /// A data write of the low `size` bytes of `value` at `va`.
    #[inline(always)]
    pub(super) fn write<M: Memory + ?Sized>(
        &mut self,
        mem: &mut M,
        va: u64,
        size: u32,
        value: u64,
    ) -> Result<(), Stop> {
        let abort = |addr: u64, fsc: u8| {
            Stop::Exception(Exception::DataAbort {
                addr,
                write: true,
                fsc,
            })
        };
        let (pa, split) = self
            .translate_access(mem, va, size, Kind::Write)
            .map_err(|(a, f)| abort(a, f))?;
        let Some((in_page, pa2)) = split else {
            return mem.write(pa, size, value).map_err(|_| {
                self.abort_pa = pa;
                abort(va, FSC_EXTERNAL)
            });
        };
        for i in 0..u64::from(size) {
            let p = if i < in_page {
                pa + i
            } else {
                pa2 + i - in_page
            };
            mem.write(p, 1, value >> (8 * i)).map_err(|_| {
                self.abort_pa = p;
                abort(va + i, FSC_EXTERNAL)
            })?;
        }
        Ok(())
    }

    /// Fetch the instruction at `va` (4-byte aligned, so within one page).
    pub(super) fn fetch<M: Memory + ?Sized>(
        &mut self,
        mem: &mut M,
        va: u64,
    ) -> Result<u32, Exception> {
        let abort = |fsc| Exception::InsnAbort { addr: va, fsc };
        let page = va & !0xFFF;
        let pa = match self.tlb.fetch {
            Some((v, el, pa)) if v == page && el == self.el => pa | (va & 0xFFF),
            _ => {
                let pa = self.translate(mem, va, Kind::Fetch).map_err(abort)?;
                self.tlb.fetch = Some((page, self.el, pa & !0xFFF));
                pa
            }
        };
        mem.fetch(pa).map_err(|_| {
            self.abort_pa = pa;
            abort(FSC_EXTERNAL)
        })
    }
}

/// `AT S1Ex{R,W}` / `AT S12Ex{R,W}` (stage 2 is not modelled, so the latter
/// are stage 1 only): walk `va` in `regime` as EL0 (`user`) or the regime's
/// own EL, and report in `PAR_EL1` (ARM ARM D17.2.113). Never uses or fills
/// the TLB.
pub(super) fn at<M: Memory + ?Sized>(
    cpu: &mut Cpu,
    mem: &mut M,
    regime: u8,
    user: bool,
    write: bool,
    va: u64,
) {
    let s = &cpu.sys;
    let ns = s.scr_el3 & SCR_NS != 0 && regime != 3;
    // Bit 11 is RES1 in both formats.
    let par = if !stage1_on(s, regime) {
        (va & PA_MASK & !0xFFF) | (ns as u64) << 9 | 1 << 11
    } else {
        let va_t = untag(s, regime, va);
        let kind = if write { Kind::Write } else { Kind::Read };
        match walk(s, mem, regime, va_t) {
            Ok(leaf) if leaf.perms & (kind as u8) << if user { 0 } else { PRIV } != 0 => {
                let attr = s.mair[regime as usize] >> (8 * leaf.attr_index) & 0xFF;
                (leaf.oa & PA_MASK & !0xFFF)
                    | attr << 56
                    | u64::from(leaf.sh) << 7
                    | ((ns || leaf.ns) as u64) << 9
                    | 1 << 11
            }
            Ok(leaf) => fault_par(FSC_PERMISSION | leaf.level),
            Err(fsc) => fault_par(fsc),
        }
    };
    cpu.sys.plain.insert(key(3, 0, 7, 4, 0), par);
}

fn fault_par(fsc: u8) -> u64 {
    1 | u64::from(fsc) << 1 | 1 << 11
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::aarch64::cpu::Abort;
    use crate::aarch64::Step;

    /// 1 MiB of flat physical memory.
    struct Phys(Vec<u8>);
    impl Memory for Phys {
        fn read(&mut self, addr: u64, size: u32) -> Result<u64, Abort> {
            let a = addr as usize;
            let s = self
                .0
                .get(a..a + size as usize)
                .ok_or(Abort { addr, write: false })?;
            let mut b = [0u8; 8];
            b[..s.len()].copy_from_slice(s);
            Ok(u64::from_le_bytes(b))
        }
        fn write(&mut self, addr: u64, size: u32, value: u64) -> Result<(), Abort> {
            let a = addr as usize;
            let d = self
                .0
                .get_mut(a..a + size as usize)
                .ok_or(Abort { addr, write: true })?;
            d.copy_from_slice(&value.to_le_bytes()[..size as usize]);
            Ok(())
        }
    }

    const L1: u64 = 0x1_0000;
    const L2: u64 = 0x2_0000;
    const L3: u64 = 0x3_0000;
    const L1_HIGH: u64 = 0x4_0000;
    const AF: u64 = 1 << 10;
    const PAGE: u64 = 3 | AF;
    const AP_EL0: u64 = 1 << 6;
    const AP_RO: u64 = 1 << 7;
    const PXN: u64 = 1 << 53;
    const UXN: u64 = 1 << 54;

    /// EL1 with a 39-bit, 4 KiB-granule EL1&0 regime (start at level 1):
    /// VA 0..2 MiB through L2/L3 page tables, VA 2..4 MiB a block onto PA 0,
    /// and the TTBR1 range's first GiB through the same L2 table.
    fn setup() -> (Cpu, Phys) {
        let mut m = Phys(vec![0; 1 << 20]);
        let mut put = |a: u64, v: u64| m.write(a, 8, v).unwrap();
        put(L1, L2 | 3);
        put(L2, L3 | 3);
        put(L2 + 8, 1 | AF | UXN); // 2 MiB block: VA 0x20_0000 -> PA 0
        put(L3 + 8, 0x5000 | PAGE); // VA 0x1000: EL1 RW, PXN off
        put(L3 + 8 * 2, 0x6000 | PAGE | AP_RO | UXN); // VA 0x2000: EL1 RO
        put(L3 + 8 * 3, 0x7000 | PAGE | AP_EL0 | PXN); // VA 0x3000: EL0 RW
        put(L3 + 8 * 4, 0x8000 | 3); // VA 0x4000: AF = 0
        put(L3 + 8 * 5, 0x9000 | PAGE); // VA 0x5000: contiguous with 0x6000
        put(L3 + 8 * 6, 0xA000 | PAGE);
        put(L1_HIGH, L2 | 3); // TTBR1: same L2 for the first GiB
        let mut c = Cpu::new();
        c.el = 1;
        c.sys.scr_el3 = SCR_NS;
        c.sys.tcr[1] = 25 | 25 << 16 | 2 << 30 | 2 << 32; // T0SZ = T1SZ = 25, TG1 = 4K, 40-bit IPS
        c.sys.ttbr0[1] = L1;
        c.sys.ttbr1_el1 = L1_HIGH;
        c.sys.sctlr[1] |= SCTLR_M;
        c.sys.vbar[1] = 0x800;
        (c, m)
    }

    fn fault(r: Result<u64, Stop>) -> Option<(u64, bool, u8)> {
        match r {
            Err(Stop::Exception(Exception::DataAbort { addr, write, fsc })) => {
                Some((addr, write, fsc))
            }
            _ => None,
        }
    }

    #[test]
    fn pages_blocks_and_both_ranges_translate() {
        let (mut c, mut m) = setup();
        m.write(0x5123, 4, 0xAABB_CCDD).unwrap();
        m.write(0x8_0010, 8, 0x1122).unwrap();
        assert_eq!(c.read(&mut m, 0x1123, 4).ok(), Some(0xAABB_CCDD));
        assert_eq!(c.read(&mut m, 0x28_0010, 8).ok(), Some(0x1122));
        // TTBR1: the top 25 bits all ones, then the same tables.
        assert_eq!(
            c.read(&mut m, 0xFFFF_FF80_0000_1123, 4).ok(),
            Some(0xAABB_CCDD)
        );
        // Neither range: bits 63..39 mixed.
        assert_eq!(
            fault(c.read(&mut m, 0x0000_0080_0000_0000, 4)),
            Some((0x80_0000_0000, false, FSC_TRANSLATION))
        );
        // Top-byte ignore, when on.
        assert!(fault(c.read(&mut m, 0x5600_0000_0000_1123, 4)).is_some());
        c.sys.tcr[1] |= 1 << 37;
        c.tlb.flush();
        assert_eq!(
            c.read(&mut m, 0x5600_0000_0000_1123, 4).ok(),
            Some(0xAABB_CCDD)
        );
    }

    #[test]
    fn faults_carry_level_and_kind() {
        let (mut c, mut m) = setup();
        // Unmapped L3 entry, level 3; unmapped L2 entry, level 2.
        assert_eq!(
            fault(c.read(&mut m, 0x7000, 1)),
            Some((0x7000, false, FSC_TRANSLATION | 3))
        );
        assert_eq!(
            fault(c.read(&mut m, 0x60_0000, 1)),
            Some((0x60_0000, false, FSC_TRANSLATION | 2))
        );
        assert_eq!(
            fault(c.read(&mut m, 0x4008, 1)),
            Some((0x4008, false, FSC_ACCESS_FLAG | 3))
        );
        // Read-only: reads fine, writes fault with WnR.
        assert!(c.read(&mut m, 0x2000, 8).is_ok());
        assert_eq!(
            fault(c.write(&mut m, 0x2000, 8, 0).map(|_| 0)),
            Some((0x2000, true, FSC_PERMISSION | 3))
        );
        // A write crossing from a writable page into a read-only one faults
        // on the second page and leaves the first untouched.
        m.write(0x9FFC, 4, 0x1234_5678).unwrap();
        assert_eq!(
            fault(c.write(&mut m, 0x1FFC, 8, 0).map(|_| 0)),
            Some((0x2000, true, FSC_PERMISSION | 3))
        );
        assert_eq!(m.read(0x5FFC, 4).ok(), Some(0));
        // A read across two writable pages stitches them.
        m.write(0x9FFC, 4, 0x1234_5678).unwrap();
        m.write(0xA000, 4, 0x9ABC_DEF0).unwrap();
        assert_eq!(c.read(&mut m, 0x5FFC, 8).ok(), Some(0x9ABC_DEF0_1234_5678));
    }

    #[test]
    fn el0_permissions_and_ldtr() {
        let (mut c, mut m) = setup();
        // EL1 may not execute EL0-writable memory; EL0 can use it.
        assert_eq!(
            c.fetch(&mut m, 0x3000),
            Err(Exception::InsnAbort {
                addr: 0x3000,
                fsc: FSC_PERMISSION | 3
            })
        );
        c.el = 0;
        assert!(c.fetch(&mut m, 0x3000).is_ok());
        assert!(c.write(&mut m, 0x3000, 4, 1).is_ok());
        assert_eq!(
            fault(c.read(&mut m, 0x1000, 4)),
            Some((0x1000, false, FSC_PERMISSION | 3))
        );
        // LDTR at EL1 = EL0's view.
        c.el = 1;
        assert!(c.read(&mut m, 0x1000, 4).is_ok());
        c.unprivileged = true;
        assert!(fault(c.read(&mut m, 0x1000, 4)).is_some());
        assert!(c.read(&mut m, 0x3000, 4).is_ok());
    }

    #[test]
    fn fault_is_taken_with_esr_and_far() {
        let (mut c, mut m) = setup();
        // Code at VA 0x1000 (PA 0x5000): ldr x0, [x1]
        m.write(0x5000, 4, 0xF940_0020).unwrap();
        c.pc = 0x1000;
        c.x[1] = 0x7008;
        c.sys.vbar[1] = 0x1000; // vectors in the mapped page
        assert!(matches!(c.step_system(&mut m), Step::Took(_)));
        assert_eq!(
            c.sys.esr[1],
            0x25 << 26 | 1 << 25 | u64::from(FSC_TRANSLATION | 3)
        );
        assert_eq!(c.sys.far[1], 0x7008);
        assert_eq!(c.sys.elr[1], 0x1000);
        assert_eq!(c.pc, 0x1200);
    }

    #[test]
    fn tlb_holds_until_flushed() {
        let (mut c, mut m) = setup();
        assert!(c.read(&mut m, 0x1000, 4).is_ok());
        m.write(L3 + 8, 8, 0).unwrap(); // unmap without TLBI
        assert!(c.read(&mut m, 0x1000, 4).is_ok());
        c.tlb.flush();
        assert!(fault(c.read(&mut m, 0x1000, 4)).is_some());
    }

    /// The fetch hint (#43) skips `translate` for the page the last
    /// instruction came from, so it has to go wherever the translation goes:
    /// with a flush, and when the EL (so the regime and permissions) changes.
    #[test]
    fn fetch_hint_follows_flushes_and_the_el() {
        let (mut c, mut m) = setup();
        m.write(0x5000, 4, 0xD503_201F).unwrap(); // nop, at the PA VA 0x1000 maps to
        m.write(0x9000, 4, 0xD503_203F).unwrap(); // yield, at PA 0x9000
        assert_eq!(c.fetch(&mut m, 0x1000), Ok(0xD503_201F));
        m.write(L3 + 8, 8, 0x9000 | PAGE).unwrap(); // remap without TLBI
        assert_eq!(c.fetch(&mut m, 0x1004), Ok(0));
        c.tlb.flush();
        assert_eq!(c.fetch(&mut m, 0x1000), Ok(0xD503_203F));

        // VA 0x3000 is EL0-executable but PXN: a hint made at EL0 must not
        // let EL1 fetch from it.
        c.el = 0;
        assert_eq!(c.fetch(&mut m, 0x3000), Ok(0));
        c.el = 1;
        assert!(matches!(
            c.fetch(&mut m, 0x3000),
            Err(Exception::InsnAbort { addr: 0x3000, .. })
        ));
    }

    #[test]
    fn at_reports_into_par() {
        let (mut c, mut m) = setup();
        c.sys.mair[1] = 0xFF;
        at(&mut c, &mut m, 1, false, false, 0x1234);
        let par = c.sys.plain[&key(3, 0, 7, 4, 0)];
        assert_eq!(par & 1, 0);
        assert_eq!(par & 0xF_FFFF_F000, 0x5000);
        assert_eq!(par >> 56, 0xFF);
        at(&mut c, &mut m, 1, false, true, 0x2000);
        let par = c.sys.plain[&key(3, 0, 7, 4, 0)];
        assert_eq!(par & 0x7F, 1 | u64::from(FSC_PERMISSION | 3) << 1);
    }
}
