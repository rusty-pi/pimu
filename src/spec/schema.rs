//! Schema of the peripheral register specs in `specs/*.toml`, and the two
//! things generated from them: the Rust constants in [`crate::spec`] and the
//! Markdown under `docs/periph/`.
//!
//! `build.rs` pulls this file in with `#[path]`, so it depends on nothing but
//! `std`, `serde` and `toml`. See `specs/README.md` for the format.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::path::Path;

use serde::Deserialize;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Spec {
    pub block: Block,
    #[serde(default, rename = "register")]
    pub registers: Vec<Register>,
    #[serde(skip)]
    pub file: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Block {
    pub name: String,
    #[serde(default)]
    pub bus: Bus,
    pub base: u32,
    pub size: u32,
    pub summary: String,
    #[serde(default)]
    pub notes: Option<String>,
    /// Identical register banks in the window; offsets are relative to bank 0.
    #[serde(default = "one")]
    pub instances: u32,
    #[serde(default)]
    pub instance_stride: u32,
    /// Required on a bus that is not memory-mapped.
    #[serde(default)]
    pub parent: Option<Parent>,
    #[serde(default)]
    pub irq: Option<Irq>,
    #[serde(default, rename = "source")]
    pub sources: Vec<Source>,
    #[serde(default, rename = "copy")]
    pub copies: Vec<BlockCopy>,
}

/// What a block hangs off: another spec's block name, optionally with the copy
/// that carries it (`"bsc"` or `"bsc.PMIC"`). One key for two relations,
/// because both say who decodes an address: the master of an indexed or PCI
/// bus, and the window a block is carved out of and decoded ahead of.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Parent {
    pub name: String,
    #[serde(default)]
    pub notes: Option<String>,
    #[serde(default, rename = "source")]
    pub sources: Vec<Source>,
}

impl Parent {
    pub fn split(&self) -> (&str, Option<&str>) {
        match self.name.split_once('.') {
            Some((block, copy)) => (block, Some(copy)),
            None => (self.name.as_str(), None),
        }
    }
}

/// The interrupt lines a block drives: a VPU source, a GIC-400 SPI, or both.
/// A lone line stays unnamed; named ones put the name in the constant.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Irq {
    #[serde(default)]
    pub vpu: Option<Lines>,
    /// GIC-400 SPI as the device tree writes it; Linux's id is 32 higher.
    #[serde(default)]
    pub gic_spi: Option<Lines>,
    #[serde(default)]
    pub notes: Option<String>,
    #[serde(default, rename = "source")]
    pub sources: Vec<Source>,
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
pub enum Lines {
    One(u32),
    Named(BTreeMap<String, u32>),
}

impl Lines {
    pub fn each(&self) -> Vec<(&str, u32)> {
        match self {
            Lines::One(n) => vec![("", *n)],
            Lines::Named(map) => map.iter().map(|(k, v)| (k.as_str(), *v)).collect(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Bus {
    /// VPU bus address (`0x7E…`, or the `0x7C…`/`0x7D…` blocks below it).
    #[default]
    Vpu,
    /// ARM physical address, for blocks the VPU has no view of.
    Arm,
    /// Offset into one PCI function's configuration space or BAR; `base` is 0.
    Pci,
    /// I²C slave: `base` is the 7-bit address, offsets are register numbers.
    I2c,
    /// MDIO clause-22 PHY: `base` is the PHY address, offsets register numbers.
    Mdio,
}

impl Bus {
    pub fn as_str(self) -> &'static str {
        match self {
            Bus::Vpu => "vpu",
            Bus::Arm => "arm",
            Bus::Pci => "pci",
            Bus::I2c => "i2c",
            Bus::Mdio => "mdio",
        }
    }

    /// Offsets are register numbers rather than byte addresses.
    pub fn indexed(self) -> bool {
        matches!(self, Bus::I2c | Bus::Mdio)
    }

    /// The SoC decodes this bus itself; everything else goes through a master,
    /// which is what [`Block::parent`] names.
    pub fn memory_mapped(self) -> bool {
        matches!(self, Bus::Vpu | Bus::Arm)
    }

    fn base_meaning(self) -> &'static str {
        match self {
            Bus::Vpu => "VPU bus address",
            Bus::Arm => "ARM physical address, low-peripheral mode",
            Bus::Pci => "offset in the PCI function",
            Bus::I2c => "7-bit I²C address",
            Bus::Mdio => "MDIO PHY address",
        }
    }

    /// On an indexed bus: past the highest device address, and the most
    /// register numbers one device can have.
    fn indexed_limits(self) -> Option<(u32, u32)> {
        match self {
            Bus::I2c => Some((0x80, 0x100)),
            Bus::Mdio => Some((0x20, 0x20)),
            Bus::Vpu | Bus::Arm | Bus::Pci => None,
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BlockCopy {
    pub name: String,
    pub base: u32,
    #[serde(default)]
    pub notes: Option<String>,
    #[serde(default, rename = "source")]
    pub sources: Vec<Source>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Register {
    pub name: String,
    pub offset: u32,
    #[serde(default = "one")]
    pub count: u32,
    #[serde(default)]
    pub stride: u32,
    #[serde(default = "thirty_two")]
    pub width: u32,
    pub access: Access,
    #[serde(default)]
    pub reset: Option<u32>,
    #[serde(default)]
    pub notes: Option<String>,
    #[serde(default, rename = "source")]
    pub sources: Vec<Source>,
    #[serde(default, rename = "field")]
    pub fields: Vec<Field>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Field {
    pub name: String,
    pub bits: String,
    #[serde(default)]
    pub access: Option<Access>,
    #[serde(default)]
    pub notes: Option<String>,
    #[serde(default, rename = "source")]
    pub sources: Vec<Source>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Access {
    R,
    W,
    Rw,
    W1c,
    Rc,
}

impl Access {
    pub fn as_str(self) -> &'static str {
        match self {
            Access::R => "r",
            Access::W => "w",
            Access::Rw => "rw",
            Access::W1c => "w1c",
            Access::Rc => "rc",
        }
    }
}

/// Where a fact came from. Several sources for one fact is the point: decoded
/// from the firmware *and* measured on a board beats either alone.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Source {
    pub kind: SourceKind,
    #[serde(rename = "ref")]
    pub reference: String,
    pub confidence: Confidence,
    #[serde(default)]
    pub note: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SourceKind {
    /// BCM2711 / BCM2835 ARM Peripherals, or a third-party part's datasheet.
    Datasheet,
    /// A published industry or architecture specification, with the section.
    Standard,
    Linux,
    /// `firmware/source/*.c` or disassembly, with the address.
    Decompile,
    /// Read on the reference board, with how it was read.
    Measured,
    /// A third-party reverse-engineering document, naming what a measurement
    /// then confirmed. Never on its own: it says where to look, not what is so.
    Manual,
    Trace,
    Inferred,
}

impl SourceKind {
    pub fn as_str(self) -> &'static str {
        match self {
            SourceKind::Datasheet => "datasheet",
            SourceKind::Standard => "standard",
            SourceKind::Linux => "linux",
            SourceKind::Decompile => "decompile",
            SourceKind::Measured => "measured",
            SourceKind::Manual => "manual",
            SourceKind::Trace => "trace",
            SourceKind::Inferred => "inferred",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Confidence {
    Low,
    Medium,
    High,
}

impl Confidence {
    pub fn as_str(self) -> &'static str {
        match self {
            Confidence::Low => "low",
            Confidence::Medium => "medium",
            Confidence::High => "high",
        }
    }
}

fn one() -> u32 {
    1
}

fn thirty_two() -> u32 {
    32
}

impl Spec {
    pub fn register(&self, offset: u32) -> Option<&Register> {
        self.registers.iter().find(|r| r.offset == offset)
    }

    pub fn bank_offsets(&self) -> impl Iterator<Item = u32> + '_ {
        (0..self.block.instances).map(|i| i * self.block.instance_stride)
    }

    pub fn bases(&self) -> impl Iterator<Item = u32> + '_ {
        std::iter::once(self.block.base).chain(self.block.copies.iter().map(|c| c.base))
    }

    /// Address units one element takes: bytes, or one register number.
    pub fn span(&self, r: &Register) -> u32 {
        if self.block.bus.indexed() {
            1
        } else {
            r.bytes()
        }
    }

    fn bank_size(&self) -> u32 {
        if self.block.instances > 1 {
            self.block.instance_stride
        } else {
            self.block.size
        }
    }
}

impl Register {
    pub fn bytes(&self) -> u32 {
        self.width / 8
    }

    pub fn element_offsets(&self) -> impl Iterator<Item = u32> + '_ {
        (0..self.count).map(|i| self.offset + i * self.stride)
    }
}

impl Field {
    pub fn range(&self) -> Result<(u32, u32), String> {
        let bit = |s: &str| {
            s.trim()
                .parse::<u32>()
                .map_err(|_| format!("bad bit number {s:?} in bits = {:?}", self.bits))
        };
        match self.bits.split_once(':') {
            Some((hi, lo)) => Ok((bit(hi)?, bit(lo)?)),
            None => bit(&self.bits).map(|b| (b, b)),
        }
    }
}

fn mask(hi: u32, lo: u32) -> u32 {
    (((1u64 << (hi - lo + 1)) - 1) << lo) as u32
}

pub fn parse(file: &str, text: &str) -> Result<Spec, String> {
    let mut spec: Spec = toml::from_str(text).map_err(|e| format!("{file}: {e}"))?;
    spec.file = file.to_string();
    let errors = validate(&spec);
    if errors.is_empty() {
        Ok(spec)
    } else {
        Err(errors
            .iter()
            .map(|e| format!("{file}: {e}"))
            .collect::<Vec<_>>()
            .join("\n"))
    }
}

/// Load every `specs/*.toml` under the crate root `root`, sorted by file name.
/// All problems in all files are reported together.
pub fn load_dir(root: &Path) -> Result<Vec<Spec>, String> {
    let dir = root.join("specs");
    let mut paths: Vec<_> = std::fs::read_dir(&dir)
        .map_err(|e| format!("{}: {e}", dir.display()))?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|x| x == "toml"))
        .collect();
    paths.sort();

    let mut specs = Vec::new();
    let mut errors = Vec::new();
    for path in paths {
        let stem = path.file_stem().unwrap_or_default().to_string_lossy();
        let file = format!("specs/{stem}.toml");
        let text = match std::fs::read_to_string(&path) {
            Ok(t) => t,
            Err(e) => {
                errors.push(format!("{file}: {e}"));
                continue;
            }
        };
        match parse(&file, &text) {
            Ok(spec) if spec.block.name != stem => errors.push(format!(
                "{file}: block name {:?} does not match the file name",
                spec.block.name
            )),
            Ok(spec) => specs.push(spec),
            Err(e) => errors.push(e),
        }
    }
    if errors.is_empty() {
        errors = validate_all(&specs);
    }
    if errors.is_empty() {
        Ok(specs)
    } else {
        Err(errors.join("\n"))
    }
}

/// Everything wrong across the whole set: the `parent` links, resolvable only
/// once every spec is loaded. One message per problem.
pub fn validate_all(specs: &[Spec]) -> Vec<String> {
    let mut errs = Vec::new();
    let by_name: BTreeMap<&str, &Spec> = specs.iter().map(|s| (s.block.name.as_str(), s)).collect();

    for spec in specs {
        let Some(p) = &spec.block.parent else {
            continue;
        };
        let (name, copy) = p.split();
        let Some(target) = by_name.get(name) else {
            errs.push(format!(
                "{}: parent {:?}: there is no specs/{name}.toml",
                spec.file, p.name
            ));
            continue;
        };
        if let Some(copy) = copy {
            if !target.block.copies.iter().any(|c| c.name == copy) {
                errs.push(format!(
                    "{}: parent {:?}: {name} has no {copy} copy",
                    spec.file, p.name
                ));
            }
        }
        // A block is carried either by a window the SoC decodes, or — for the
        // registers behind a PCI function's BAR — by another PCI function.
        let (child, parent) = (spec.block.bus, target.block.bus);
        if !parent.memory_mapped() && !(child == Bus::Pci && parent == Bus::Pci) {
            errs.push(format!(
                "{}: parent {:?}: a {} block cannot carry a {} one",
                spec.file,
                p.name,
                parent.as_str(),
                child.as_str()
            ));
        }
    }

    // Walking up from every block has to end at one without a parent.
    for spec in specs {
        let mut seen = BTreeSet::from([spec.block.name.as_str()]);
        let mut at = spec;
        while let Some(p) = &at.block.parent {
            let Some(next) = by_name.get(p.split().0) else {
                break;
            };
            if !seen.insert(next.block.name.as_str()) {
                errs.push(format!(
                    "{}: parent {:?} closes a cycle",
                    at.file,
                    at.block.parent.as_ref().unwrap().name
                ));
                break;
            }
            at = next;
        }
    }

    errs.sort();
    errs.dedup();
    errs
}

fn is_module_name(s: &str) -> bool {
    s.starts_with(|c: char| c.is_ascii_lowercase())
        && s.chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
}

fn is_const_name(s: &str) -> bool {
    s.starts_with(|c: char| c.is_ascii_uppercase())
        && s.chars()
            .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_')
}

pub fn validate(spec: &Spec) -> Vec<String> {
    let mut errs = Vec::new();
    let b = &spec.block;

    if !is_module_name(&b.name) {
        errs.push(format!("block name {:?} is not lower_snake_case", b.name));
    }
    if b.sources.is_empty() {
        errs.push("block has no [[block.source]]".into());
    }
    if b.size == 0 {
        errs.push("block size is 0".into());
    }
    match b.bus.indexed_limits() {
        Some((addrs, regs)) => {
            for (name, base) in std::iter::once(("block", b.base))
                .chain(b.copies.iter().map(|c| (c.name.as_str(), c.base)))
            {
                if base >= addrs {
                    errs.push(format!(
                        "{name}: address {base:#x} does not fit the {} bus",
                        b.bus.as_str()
                    ));
                }
            }
            if b.size > regs {
                errs.push(format!(
                    "size {:#x}: a {} device has at most {regs:#x} registers",
                    b.size,
                    b.bus.as_str()
                ));
            }
        }
        None => {
            for (name, base) in std::iter::once(("block", b.base))
                .chain(b.copies.iter().map(|c| (c.name.as_str(), c.base)))
            {
                if u64::from(base) + u64::from(b.size) > 1 << 32 {
                    errs.push(format!(
                        "{name}: window runs past the end of the address space"
                    ));
                }
            }
        }
    }
    let mut copy_names = BTreeSet::new();
    for c in &b.copies {
        let what = format!("copy {}", c.name);
        if !is_const_name(&c.name) {
            errs.push(format!("{what}: name is not UPPER_SNAKE_CASE"));
        }
        if !copy_names.insert(c.name.as_str()) {
            errs.push(format!("{what}: defined twice"));
        }
        if c.sources.is_empty() {
            errs.push(format!("{what}: no [[block.copy.source]]"));
        }
        if c.base == b.base {
            errs.push(format!("{what}: same base as the block"));
        }
        check_sources(&mut errs, &what, &c.sources);
    }
    match b.instances {
        0 => errs.push("instances = 0".into()),
        1 if b.instance_stride != 0 => errs.push("instance_stride set with one instance".into()),
        1 => {}
        n if b.instance_stride == 0 => errs.push(format!("{n} instances but no instance_stride")),
        n if u64::from(n) * u64::from(b.instance_stride) > u64::from(b.size) => errs.push(format!(
            "{n} banks of {:#x} do not fit the {:#x}-byte window",
            b.instance_stride, b.size
        )),
        _ => {}
    }
    check_sources(&mut errs, "block", &b.sources);

    match &b.parent {
        Some(p) => {
            let (block, copy) = p.split();
            if !is_module_name(block) {
                errs.push(format!("parent {:?}: not a block name", p.name));
            }
            if copy.is_some_and(|c| !is_const_name(c)) {
                errs.push(format!("parent {:?}: copy is not UPPER_SNAKE_CASE", p.name));
            }
            if block == b.name {
                errs.push(format!("parent {:?}: a block cannot carry itself", p.name));
            }
            if p.sources.is_empty() {
                errs.push("parent has no [[block.parent.source]]".into());
            }
            check_sources(&mut errs, "parent", &p.sources);
        }
        None if !b.bus.memory_mapped() => errs.push(format!(
            "a block on the {} bus needs a [block.parent] saying what carries it",
            b.bus.as_str()
        )),
        None => {}
    }

    if let Some(irq) = &b.irq {
        if irq.vpu.is_none() && irq.gic_spi.is_none() {
            errs.push("[block.irq] names no line".into());
        }
        if irq.sources.is_empty() {
            errs.push("irq has no [[block.irq.source]]".into());
        }
        check_sources(&mut errs, "irq", &irq.sources);
        // A VPU core vectors its 64 sources as interrupt numbers 64..=127,
        // which is what every spec and firmware log calls them; the GIC-400's
        // 0..=31 are SGIs and PPIs rather than peripheral lines.
        for (what, lines, range) in [
            ("vpu", irq.vpu.as_ref(), 64..128),
            ("gic_spi", irq.gic_spi.as_ref(), 0..224),
        ] {
            let Some(lines) = lines else { continue };
            let mut seen = BTreeSet::new();
            for (name, n) in lines.each() {
                if !name.is_empty() && !is_const_name(name) {
                    errs.push(format!("irq {what}.{name}: name is not UPPER_SNAKE_CASE"));
                }
                if !range.contains(&n) {
                    errs.push(format!(
                        "irq {what}: {n} is outside {}..{}, the lines that controller has",
                        range.start, range.end
                    ));
                }
                if !seen.insert(n) {
                    errs.push(format!("irq {what}: line {n} is listed twice"));
                }
            }
        }
    }

    let bank = u64::from(spec.bank_size());
    let mut names = BTreeSet::new();
    let mut spans: Vec<(u64, u64, &str)> = Vec::new();
    for r in &spec.registers {
        let what = format!("register {}", r.name);
        if !is_const_name(&r.name) {
            errs.push(format!("{what}: name is not UPPER_SNAKE_CASE"));
        }
        if !names.insert(r.name.as_str()) {
            errs.push(format!("{what}: defined twice"));
        }
        if r.sources.is_empty() {
            errs.push(format!("{what}: no [[register.source]]"));
        }
        check_sources(&mut errs, &what, &r.sources);
        if !matches!(r.width, 8 | 16 | 32) {
            errs.push(format!("{what}: width {} is not 8, 16 or 32", r.width));
            continue;
        }
        let bytes = spec.span(r);
        if r.offset % bytes != 0 {
            errs.push(format!(
                "{what}: offset {:#x} is not {bytes}-byte aligned",
                r.offset
            ));
        }
        if let Some(reset) = r.reset {
            if r.width < 32 && reset >> r.width != 0 {
                errs.push(format!(
                    "{what}: reset {reset:#x} is wider than {} bits",
                    r.width
                ));
            }
        }
        match r.count {
            0 => {
                errs.push(format!("{what}: count = 0"));
                continue;
            }
            1 if r.stride != 0 => errs.push(format!("{what}: stride without count")),
            1 => {}
            n if n > 0x1_0000 => {
                errs.push(format!("{what}: count {n} is implausibly large"));
                continue;
            }
            _ if r.stride < bytes || r.stride % bytes != 0 => {
                errs.push(format!(
                    "{what}: stride {:#x} does not fit {bytes}-byte elements",
                    r.stride
                ));
                continue;
            }
            _ => {}
        }
        let end =
            u64::from(r.offset) + u64::from(r.count - 1) * u64::from(r.stride) + u64::from(bytes);
        if end > bank {
            errs.push(format!(
                "{what}: ends at {end:#x}, past the {bank:#x}-byte window"
            ));
        }
        for off in r.element_offsets() {
            spans.push((
                u64::from(off),
                u64::from(off) + u64::from(bytes),
                r.name.as_str(),
            ));
        }

        let mut used = 0u32;
        let mut field_names = BTreeSet::new();
        for f in &r.fields {
            let what = format!("field {}.{}", r.name, f.name);
            if !is_const_name(&f.name) {
                errs.push(format!("{what}: name is not UPPER_SNAKE_CASE"));
            }
            if !field_names.insert(f.name.as_str()) {
                errs.push(format!("{what}: defined twice"));
            }
            if f.sources.is_empty() {
                errs.push(format!("{what}: no [[register.field.source]]"));
            }
            check_sources(&mut errs, &what, &f.sources);
            let (hi, lo) = match f.range() {
                Ok(r) => r,
                Err(e) => {
                    errs.push(format!("{what}: {e}"));
                    continue;
                }
            };
            if lo > hi {
                errs.push(format!("{what}: bits {:?} are the wrong way round", f.bits));
                continue;
            }
            if hi >= r.width {
                errs.push(format!(
                    "{what}: bit {hi} is outside the {}-bit register",
                    r.width
                ));
                continue;
            }
            let m = mask(hi, lo);
            if used & m != 0 {
                errs.push(format!("{what}: overlaps another field"));
            }
            used |= m;
        }
    }

    spans.sort();
    let mut reported = BTreeSet::new();
    for w in spans.windows(2) {
        let ((_, a_end, a), (b_start, _, b)) = (w[0], w[1]);
        if b_start < a_end && reported.insert((a, b)) {
            errs.push(format!("register {a} overlaps {b} at {b_start:#x}"));
        }
    }

    let mut consts = BTreeSet::new();
    for c in constants(spec) {
        if !consts.insert(c.name.clone()) {
            errs.push(format!("generated constant {} is defined twice", c.name));
        }
    }
    errs
}

fn check_sources(errs: &mut Vec<String>, what: &str, sources: &[Source]) {
    for s in sources {
        if s.reference.trim().is_empty() {
            errs.push(format!(
                "{what}: a {} source has an empty ref",
                s.kind.as_str()
            ));
        }
    }
}

pub struct Const {
    pub name: String,
    pub value: u32,
    pub doc: String,
}

pub fn constants(spec: &Spec) -> Vec<Const> {
    let b = &spec.block;
    let mut out = Vec::new();
    let mut push = |name: String, value: u32, doc: String| out.push(Const { name, value, doc });

    push(
        "BASE".into(),
        b.base,
        format!("Where the block sits: {}.", b.bus.base_meaning()),
    );
    push(
        "SIZE".into(),
        b.size,
        if b.bus.indexed() {
            "Number of register numbers the device decodes.".into()
        } else {
            "Size of the decoded window in bytes.".into()
        },
    );
    for c in &b.copies {
        push(
            format!("{}_BASE", c.name),
            c.base,
            format!("Where the `{}` copy of the block sits.", c.name),
        );
    }
    if b.instances > 1 {
        push(
            "INSTANCES".into(),
            b.instances,
            "Number of register banks in the window.".into(),
        );
        push(
            "INSTANCE_STRIDE".into(),
            b.instance_stride,
            "Distance between register banks; offsets below are for bank 0.".into(),
        );
    }
    if let Some(irq) = &b.irq {
        for (prefix, lines, bias) in [
            ("IRQ_VPU", irq.vpu.as_ref(), 0u32),
            ("IRQ_GIC", irq.gic_spi.as_ref(), 32),
        ] {
            let Some(lines) = lines else { continue };
            for (name, n) in lines.each() {
                let const_name = if name.is_empty() {
                    prefix.to_string()
                } else {
                    format!("{prefix}_{name}")
                };
                let line = if name.is_empty() {
                    String::new()
                } else {
                    format!(" `{name}`")
                };
                let doc = if bias == 0 {
                    format!("VPU interrupt source the block raises{line}.")
                } else {
                    format!("GIC-400 interrupt id of the block's{line} line (`GIC_SPI {n}`).")
                };
                push(const_name, n + bias, doc);
            }
        }
    }
    for r in &spec.registers {
        let mut doc = format!("`{}`", r.access.as_str());
        if let Some(notes) = &r.notes {
            write!(doc, " — {}", notes.trim()).unwrap();
        }
        push(r.name.clone(), r.offset, doc);
        if r.count > 1 {
            push(
                format!("{}_COUNT", r.name),
                r.count,
                format!("Number of `{}` elements.", r.name),
            );
            push(
                format!("{}_STRIDE", r.name),
                r.stride,
                format!("Distance between `{}` elements.", r.name),
            );
        }
        if let Some(reset) = r.reset {
            push(
                format!("{}_RESET", r.name),
                reset,
                format!("Reset value of `{}`.", r.name),
            );
        }
        for f in &r.fields {
            let Ok((hi, lo)) = f.range() else { continue };
            if lo > hi || hi >= 32 {
                continue;
            }
            push(
                format!("{}_{}_SHIFT", r.name, f.name),
                lo,
                format!("Lowest bit of `{}.{}` (bits {}).", r.name, f.name, f.bits),
            );
            push(
                format!("{}_{}_MASK", r.name, f.name),
                mask(hi, lo),
                format!("`{}.{}`, in place.", r.name, f.name),
            );
        }
    }
    out
}

pub fn rust_module(spec: &Spec) -> String {
    let mut s = String::new();
    writeln!(s, "// generated from {} – do not edit", spec.file).unwrap();
    writeln!(s, "#[doc = {:?}]", spec.block.summary).unwrap();
    writeln!(s, "pub mod {} {{", spec.block.name).unwrap();
    for c in constants(spec) {
        writeln!(s, "    #[doc = {:?}]", c.doc).unwrap();
        if c.name == "INSTANCES" || c.name.ends_with("_COUNT") || c.name.starts_with("IRQ_") {
            writeln!(s, "    pub const {}: u32 = {};", c.name, c.value).unwrap();
        } else {
            writeln!(s, "    pub const {}: u32 = {:#X};", c.name, c.value).unwrap();
        }
    }
    writeln!(s, "}}").unwrap();
    s
}

fn cell(s: &str) -> String {
    s.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .replace('|', "\\|")
}

fn write_sources(s: &mut String, sources: &[Source]) {
    for src in sources {
        write!(
            s,
            "- {} ({}): {}",
            src.kind.as_str(),
            src.confidence.as_str(),
            src.reference.trim()
        )
        .unwrap();
        if let Some(note) = &src.note {
            write!(s, " — _{}_", note.trim()).unwrap();
        }
        s.push('\n');
    }
}

fn source_summary(sources: &[Source]) -> String {
    match sources.iter().map(|s| s.confidence).max() {
        Some(best) => format!("{}, best {}", sources.len(), best.as_str()),
        None => "none".into(),
    }
}

/// A `parent` as the Markdown shows it: a link to the block's page, and the
/// copy that carries the device where it is one.
fn parent_link(p: &Parent) -> String {
    match p.split() {
        (name, Some(copy)) => format!("[`{name}`]({name}.md), `{copy}` copy"),
        (name, None) => format!("[`{name}`]({name}.md)"),
    }
}

/// The lines an `irq` names, as one line of prose.
fn irq_summary(irq: &Irq) -> String {
    let mut parts = Vec::new();
    if let Some(lines) = &irq.vpu {
        for (name, n) in lines.each() {
            let named = if name.is_empty() {
                String::new()
            } else {
                format!("`{name}` ")
            };
            parts.push(format!("{named}VPU source {n}"));
        }
    }
    if let Some(lines) = &irq.gic_spi {
        for (name, n) in lines.each() {
            let named = if name.is_empty() {
                String::new()
            } else {
                format!("`{name}` ")
            };
            parts.push(format!("{named}GIC id {} (`GIC_SPI {n}`)", n + 32));
        }
    }
    parts.join(" · ")
}

pub fn markdown(spec: &Spec) -> String {
    let b = &spec.block;
    let mut s = String::new();
    writeln!(
        s,
        "<!-- generated from {} by `cargo run -- spec-docs --update` – do not edit -->\n",
        spec.file
    )
    .unwrap();
    writeln!(s, "# `{}` – {}\n", b.name, b.summary.trim()).unwrap();
    writeln!(s, "- Bus: `{}` ({})", b.bus.as_str(), b.bus.base_meaning()).unwrap();
    writeln!(s, "- Base: `{}`", base_str(b.bus, b.base)).unwrap();
    for c in &b.copies {
        writeln!(s, "- `{}` copy: `{}`", c.name, base_str(b.bus, c.base)).unwrap();
    }
    writeln!(s, "- Size: `{:#X}`", b.size).unwrap();
    if b.instances > 1 {
        writeln!(
            s,
            "- Banks: {} × `{:#X}`; offsets below are for bank 0",
            b.instances, b.instance_stride
        )
        .unwrap();
    }
    if let Some(p) = &b.parent {
        writeln!(s, "- Carried by: {}", parent_link(p)).unwrap();
    }
    if let Some(irq) = &b.irq {
        writeln!(s, "- Interrupts: {}", irq_summary(irq)).unwrap();
    }
    if let Some(notes) = &b.notes {
        writeln!(s, "\n{}", notes.trim()).unwrap();
    }
    s.push_str("\nSources:\n\n");
    write_sources(&mut s, &b.sources);
    for c in &b.copies {
        writeln!(s, "\n`{}` copy:\n", c.name).unwrap();
        if let Some(notes) = &c.notes {
            writeln!(s, "{}\n", notes.trim()).unwrap();
        }
        write_sources(&mut s, &c.sources);
    }
    if let Some(p) = &b.parent {
        writeln!(s, "\nCarried by {}:\n", parent_link(p)).unwrap();
        if let Some(notes) = &p.notes {
            writeln!(s, "{}\n", notes.trim()).unwrap();
        }
        write_sources(&mut s, &p.sources);
    }
    if let Some(irq) = &b.irq {
        writeln!(s, "\nInterrupts ({}):\n", irq_summary(irq)).unwrap();
        if let Some(notes) = &irq.notes {
            writeln!(s, "{}\n", notes.trim()).unwrap();
        }
        write_sources(&mut s, &irq.sources);
    }

    s.push_str("\n## Register map\n\n");
    s.push_str("| Offset | Name | Access | Width | Sources |\n");
    s.push_str("|---|---|---|---|---|\n");
    for r in &spec.registers {
        let offset = if r.count > 1 {
            let last = r.offset + (r.count - 1) * r.stride;
            format!(
                "`0x{:03X}`–`0x{last:03X}` ({} × {:#X})",
                r.offset, r.count, r.stride
            )
        } else {
            format!("`0x{:03X}`", r.offset)
        };
        writeln!(
            s,
            "| {offset} | [`{}`](#{}) | {} | {} | {} |",
            r.name,
            r.name.to_lowercase(),
            r.access.as_str(),
            r.width,
            source_summary(&r.sources)
        )
        .unwrap();
    }

    for r in &spec.registers {
        writeln!(s, "\n## `{}`\n", r.name).unwrap();
        write!(s, "Offset `0x{:03X}`", r.offset).unwrap();
        if r.count > 1 {
            write!(s, ", {} elements {:#X} apart", r.count, r.stride).unwrap();
        }
        write!(s, " · access `{}` · {} bits", r.access.as_str(), r.width).unwrap();
        if let Some(reset) = r.reset {
            write!(s, " · reset `{reset:#X}`").unwrap();
        }
        s.push('\n');
        if let Some(notes) = &r.notes {
            writeln!(s, "\n{}", notes.trim()).unwrap();
        }
        if !r.fields.is_empty() {
            s.push_str("\n| Bits | Field | Access | Notes |\n|---|---|---|---|\n");
            for f in &r.fields {
                writeln!(
                    s,
                    "| {} | `{}` | {} | {} |",
                    f.bits,
                    f.name,
                    f.access.unwrap_or(r.access).as_str(),
                    cell(f.notes.as_deref().unwrap_or(""))
                )
                .unwrap();
            }
        }
        s.push_str("\nSources:\n\n");
        write_sources(&mut s, &r.sources);
        for f in &r.fields {
            writeln!(s, "\n`{}` sources:\n", f.name).unwrap();
            write_sources(&mut s, &f.sources);
        }
    }
    s
}

pub fn index_markdown(specs: &[Spec]) -> String {
    let mut s = String::new();
    s.push_str("<!-- generated from specs/*.toml by `cargo run -- spec-docs --update` – do not edit -->\n\n");
    s.push_str("# Peripheral register specs\n\n");
    s.push_str(
        "Generated from the TOML specs in [`specs/`](../../specs/); see \
         [`specs/README.md`](../../specs/README.md) for the format and the rules.\n\n",
    );
    s.push_str("| Block | Bus | Base | Size | Registers | Summary |\n|---|---|---|---|---|---|\n");
    for spec in specs {
        let b = &spec.block;
        writeln!(
            s,
            "| [`{0}`]({0}.md) | {1} | `{2}` | `{3:#X}` | {4} | {5} |",
            b.name,
            b.bus.as_str(),
            base_str(b.bus, b.base),
            b.size,
            spec.registers.len(),
            cell(&b.summary)
        )
        .unwrap();
    }

    write_tree(&mut s, specs);
    write_irq_table(&mut s, specs);
    s
}

/// The `parent` links as a nested list: only the blocks that carry something.
fn write_tree(s: &mut String, specs: &[Spec]) {
    let children = |name: &str| -> Vec<&Spec> {
        specs
            .iter()
            .filter(|s| s.block.parent.as_ref().is_some_and(|p| p.split().0 == name))
            .collect()
    };

    s.push_str("\n## What carries what\n\n");
    s.push_str(
        "The `parent` of a block: the master of its bus, or the window it is carved out of \
         and decoded ahead of. Drawn by hand in [`board-sheet.svg`](../board-sheet.svg), \
         which `tests/board_sheet.rs` checks against these specs.\n\n",
    );
    for spec in specs {
        if spec.block.parent.is_some() || children(&spec.block.name).is_empty() {
            continue;
        }
        writeln!(s, "- [`{0}`]({0}.md)", spec.block.name).unwrap();
        let mut stack: Vec<(usize, &Spec)> = children(&spec.block.name)
            .into_iter()
            .rev()
            .map(|c| (1, c))
            .collect();
        while let Some((depth, child)) = stack.pop() {
            let b = &child.block;
            let copy = match b.parent.as_ref().and_then(|p| p.split().1) {
                Some(copy) => format!(", `{copy}` copy"),
                None => String::new(),
            };
            writeln!(
                s,
                "{}- [`{}`]({}.md) — {} `{}`{copy}",
                "  ".repeat(depth),
                b.name,
                b.name,
                b.bus.as_str(),
                base_str(b.bus, b.base),
            )
            .unwrap();
            stack.extend(children(&b.name).into_iter().rev().map(|c| (depth + 1, c)));
        }
    }
}

fn write_irq_table(s: &mut String, specs: &[Spec]) {
    let mut rows: Vec<((u32, u32), String)> = Vec::new();
    for spec in specs {
        let Some(irq) = &spec.block.irq else { continue };
        for (which, lines, bias) in [
            ("VPU source", irq.vpu.as_ref(), 0u32),
            ("GIC id", irq.gic_spi.as_ref(), 32),
        ] {
            let Some(lines) = lines else { continue };
            for (name, n) in lines.each() {
                let line = if name.is_empty() {
                    format!("[`{0}`]({0}.md)", spec.block.name)
                } else {
                    format!("[`{0}`]({0}.md) `{name}`", spec.block.name)
                };
                let spi = if bias == 0 {
                    "—".to_string()
                } else {
                    format!("`GIC_SPI {n}`")
                };
                rows.push((
                    (bias, n + bias),
                    format!("| {which} | {} | {line} | {spi} |", n + bias),
                ));
            }
        }
    }
    rows.sort();

    s.push_str("\n## Interrupt lines\n\n");
    s.push_str("| Controller | Number | Block | Device tree |\n|---|---|---|---|\n");
    for (_, row) in rows {
        writeln!(s, "{row}").unwrap();
    }
}

/// A base as the Markdown shows it: a full 32-bit address on a memory bus, a
/// short device address on an indexed one.
fn base_str(bus: Bus, base: u32) -> String {
    if bus.indexed() {
        format!("{base:#04X}")
    } else {
        format!("{base:#010X}")
    }
}
