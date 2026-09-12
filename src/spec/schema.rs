//! Schema of the peripheral register specs in `specs/*.toml` (#39), and the two
//! things generated from them: the Rust constants in [`crate::spec`] and the
//! Markdown under `docs/periph/`.
//!
//! `build.rs` pulls this file in with `#[path]`, so it depends on nothing but
//! `std`, `serde` and `toml`. See `specs/README.md` for the format.

use std::collections::BTreeSet;
use std::fmt::Write as _;
use std::path::Path;

use serde::Deserialize;

/// One `specs/<block>.toml`.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Spec {
    pub block: Block,
    #[serde(default, rename = "register")]
    pub registers: Vec<Register>,
    /// Where the spec was read from, relative to the crate root. Not part of
    /// the file.
    #[serde(skip)]
    pub file: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Block {
    /// Name of the generated module; also the file stem.
    pub name: String,
    /// Bus address as the VPU sees it (`0x7E…`).
    pub base: u32,
    /// Size of the decoded window in bytes.
    pub size: u32,
    pub summary: String,
    #[serde(default)]
    pub notes: Option<String>,
    /// Number of identical register banks in the window (one per VPU core,
    /// say). Register offsets are relative to bank 0.
    #[serde(default = "one")]
    pub instances: u32,
    /// Distance between banks; required when `instances > 1`.
    #[serde(default)]
    pub instance_stride: u32,
    #[serde(default, rename = "source")]
    pub sources: Vec<Source>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Register {
    pub name: String,
    pub offset: u32,
    /// Array length; 1 for a plain register.
    #[serde(default = "one")]
    pub count: u32,
    /// Distance between array elements; only allowed with `count > 1`.
    #[serde(default)]
    pub stride: u32,
    /// Access width in bits.
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
    /// `"hi:lo"`, or `"n"` for a single bit.
    pub bits: String,
    /// Defaults to the register's access.
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
    /// Write 1 to clear.
    W1c,
    /// Read to clear.
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

/// Where a fact came from. Several sources for one fact is the point: a
/// register decoded from the firmware *and* measured on the reference board is
/// worth more than either alone.
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
    /// BCM2711 / BCM2835 ARM Peripherals.
    Datasheet,
    /// Upstream driver or DT binding.
    Linux,
    /// `firmware/source/*.c` or disassembly, with the address.
    Decompile,
    /// Read on the reference board, with how it was read.
    Measured,
    /// Observed in a `recon` run.
    Trace,
    /// A guess, with the reasoning.
    Inferred,
}

impl SourceKind {
    pub fn as_str(self) -> &'static str {
        match self {
            SourceKind::Datasheet => "datasheet",
            SourceKind::Linux => "linux",
            SourceKind::Decompile => "decompile",
            SourceKind::Measured => "measured",
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
    /// The register starting at `offset` (relative to bank 0), if any.
    pub fn register(&self, offset: u32) -> Option<&Register> {
        self.registers.iter().find(|r| r.offset == offset)
    }

    /// Offset of every bank within the block window.
    pub fn bank_offsets(&self) -> impl Iterator<Item = u32> + '_ {
        (0..self.block.instances).map(|i| i * self.block.instance_stride)
    }

    /// The window one bank's registers have to fit in.
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

    /// Offset of every element, relative to bank 0.
    pub fn element_offsets(&self) -> impl Iterator<Item = u32> + '_ {
        (0..self.count).map(|i| self.offset + i * self.stride)
    }
}

impl Field {
    /// `(hi, lo)`.
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

/// Mask of bits `hi..=lo`, in place. `hi` must be below 32.
fn mask(hi: u32, lo: u32) -> u32 {
    (((1u64 << (hi - lo + 1)) - 1) << lo) as u32
}

/// Parse and validate one spec. `file` is only used in messages.
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
        Ok(specs)
    } else {
        Err(errors.join("\n"))
    }
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

/// Everything wrong with `spec`, one message per problem.
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
    if u64::from(b.base) + u64::from(b.size) > 1 << 32 {
        errs.push("block window runs past the end of the address space".into());
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
        let bytes = r.bytes();
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
            n if n > 4096 => {
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

/// One generated `pub const`.
pub struct Const {
    pub name: String,
    pub value: u32,
    pub doc: String,
}

/// The constants generated for `spec`, in file order.
pub fn constants(spec: &Spec) -> Vec<Const> {
    let b = &spec.block;
    let mut out = Vec::new();
    let mut push = |name: String, value: u32, doc: String| out.push(Const { name, value, doc });

    push(
        "BASE".into(),
        b.base,
        "Bus address of the block (VPU view).".into(),
    );
    push(
        "SIZE".into(),
        b.size,
        "Size of the decoded window in bytes.".into(),
    );
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

/// The generated Rust module for `spec`.
pub fn rust_module(spec: &Spec) -> String {
    let mut s = String::new();
    writeln!(s, "// generated from {} – do not edit", spec.file).unwrap();
    writeln!(s, "#[doc = {:?}]", spec.block.summary).unwrap();
    writeln!(s, "pub mod {} {{", spec.block.name).unwrap();
    for c in constants(spec) {
        writeln!(s, "    #[doc = {:?}]", c.doc).unwrap();
        // Counts read better in decimal; addresses, offsets and masks in hex.
        if c.name == "INSTANCES" || c.name.ends_with("_COUNT") {
            writeln!(s, "    pub const {}: u32 = {};", c.name, c.value).unwrap();
        } else {
            writeln!(s, "    pub const {}: u32 = {:#X};", c.name, c.value).unwrap();
        }
    }
    writeln!(s, "}}").unwrap();
    s
}

/// Markdown table cell: one line, no bare pipes.
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

/// `docs/periph/<block>.md` for `spec`.
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
    writeln!(s, "- Base: `{:#010X}`", b.base).unwrap();
    writeln!(s, "- Size: `{:#X}`", b.size).unwrap();
    if b.instances > 1 {
        writeln!(
            s,
            "- Banks: {} × `{:#X}`; offsets below are for bank 0",
            b.instances, b.instance_stride
        )
        .unwrap();
    }
    if let Some(notes) = &b.notes {
        writeln!(s, "\n{}", notes.trim()).unwrap();
    }
    s.push_str("\nSources:\n\n");
    write_sources(&mut s, &b.sources);

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

/// `docs/periph/README.md`: one line per block.
pub fn index_markdown(specs: &[Spec]) -> String {
    let mut s = String::new();
    s.push_str("<!-- generated from specs/*.toml by `cargo run -- spec-docs --update` – do not edit -->\n\n");
    s.push_str("# Peripheral register specs\n\n");
    s.push_str(
        "Generated from the TOML specs in [`specs/`](../../specs/); see \
         [`specs/README.md`](../../specs/README.md) for the format and the rules.\n\n",
    );
    s.push_str("| Block | Base | Size | Registers | Summary |\n|---|---|---|---|---|\n");
    for spec in specs {
        let b = &spec.block;
        writeln!(
            s,
            "| [`{0}`]({0}.md) | `{1:#010X}` | `{2:#X}` | {3} | {4} |",
            b.name,
            b.base,
            b.size,
            spec.registers.len(),
            cell(&b.summary)
        )
        .unwrap();
    }
    s
}
