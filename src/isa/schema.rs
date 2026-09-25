//! The shape of `isa/vpu.toml`, and the Markdown it renders to.

use crate::spec::schema::Source;
use serde::Deserialize;
use std::fmt::Write;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Isa {
    pub title: String,
    pub intro: String,
    #[serde(default, rename = "section")]
    pub sections: Vec<Section>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Section {
    pub title: String,
    #[serde(default = "two")]
    pub level: u8,
    #[serde(default)]
    pub body: Option<String>,
    #[serde(default)]
    pub after: Option<String>,
    /// `"alu"` or `"mem"`: the rows of this section are the sub-op table of
    /// that class, and the build reads the mnemonics out of them.
    #[serde(default)]
    pub ops: Option<OpClass>,
    #[serde(default)]
    pub columns: Vec<String>,
    #[serde(default, rename = "row")]
    pub rows: Vec<Row>,
    #[serde(default, rename = "source")]
    pub sources: Vec<Source>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Row {
    pub cells: Vec<String>,
    #[serde(default)]
    pub subop: Option<u8>,
    /// In an op-table section: the mnemonic as `binutils-vc4` spells it, less
    /// the `v<w>` prefix; the name the model prints.
    #[serde(default)]
    pub mnemonic: Option<String>,
    /// In an op-table section: whether the model can carry the op out. The
    /// build turns this into a table `tests/vpu_isa.rs` checks, so the page and
    /// the model cannot drift apart.
    #[serde(default)]
    pub status: Option<Status>,
    #[serde(default, rename = "source")]
    pub sources: Vec<Source>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum OpClass {
    Alu,
    Mem,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Status {
    /// Measured, and the model carries it out.
    Executes,
    /// The encoding has a name and nothing more; the model faults.
    Unknown,
}

fn two() -> u8 {
    2
}

pub fn load(path: &std::path::Path) -> Result<Isa, String> {
    let text = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let isa: Isa = toml::from_str(&text).map_err(|e| format!("{}: {e}", path.display()))?;
    for section in &isa.sections {
        let want = section.columns.len();
        if want == 0 && !section.rows.is_empty() {
            return Err(format!("{}: rows without columns", section.title));
        }
        for row in &section.rows {
            let have = row.cells.len() + usize::from(section.source_column());
            if want != have {
                return Err(format!(
                    "{}: row has {} cells, the table has {want} columns: {:?}",
                    section.title,
                    row.cells.len(),
                    row.cells
                ));
            }
        }
        if section.source_column() && section.rows.iter().any(|r| r.sources.is_empty()) {
            return Err(format!("{}: a row has no source", section.title));
        }
    }
    for class in [OpClass::Alu, OpClass::Mem] {
        let rows: Vec<&Row> = isa
            .sections
            .iter()
            .filter(|s| s.ops == Some(class))
            .flat_map(|s| s.rows.iter())
            .collect();
        if rows.is_empty() {
            return Err(format!("no {} sub-op table", class.as_str()));
        }
        let mut seen = vec![false; class.count()];
        for row in rows {
            let (Some(subop), Some(mnemonic), Some(_)) =
                (row.subop, row.mnemonic.as_ref(), row.status)
            else {
                return Err(format!(
                    "{} table: a row is missing `subop`, `mnemonic` or `status`: {:?}",
                    class.as_str(),
                    row.cells
                ));
            };
            let slot = seen
                .get_mut(subop as usize)
                .ok_or_else(|| format!("{} sub-op {subop} is out of range", class.as_str()))?;
            if std::mem::replace(slot, true) {
                return Err(format!("{} sub-op {subop} listed twice", class.as_str()));
            }
            if mnemonic.is_empty() {
                return Err(format!(
                    "{} sub-op {subop} has an empty mnemonic",
                    class.as_str()
                ));
            }
        }
        if let Some(missing) = seen.iter().position(|seen| !seen) {
            return Err(format!(
                "{} sub-op {missing} is not in the table",
                class.as_str()
            ));
        }
    }
    Ok(isa)
}

impl OpClass {
    pub fn as_str(self) -> &'static str {
        match self {
            OpClass::Alu => "ALU",
            OpClass::Mem => "memory",
        }
    }

    pub fn count(self) -> usize {
        match self {
            OpClass::Alu => 64,
            OpClass::Mem => 32,
        }
    }
}

pub fn op_table(isa: &Isa, class: OpClass) -> Vec<(String, Status)> {
    let mut out = vec![(String::new(), Status::Unknown); class.count()];
    for row in isa
        .sections
        .iter()
        .filter(|s| s.ops == Some(class))
        .flat_map(|s| s.rows.iter())
    {
        let (Some(subop), Some(mnemonic), Some(status)) =
            (row.subop, row.mnemonic.as_ref(), row.status)
        else {
            continue;
        };
        out[subop as usize] = (mnemonic.clone(), status);
    }
    out
}

/// The Rust the build writes beside the model: the mnemonic tables the
/// disassembler prints, and what the page claims the model can carry out.
pub fn rust_module(isa: &Isa) -> String {
    let mut s = String::new();
    writeln!(
        s,
        "// Generated from isa/vpu.toml by build.rs – do not edit.\n"
    )
    .unwrap();
    for (class, name, len) in [
        (OpClass::Alu, "VEC_ALU_OPS", 64),
        (OpClass::Mem, "VEC_MEM_OPS", 32),
    ] {
        let table = op_table(isa, class);
        writeln!(
            s,
            "/// Sub-op mnemonics of the {} class, as `binutils-vc4` spells them.",
            class.as_str()
        )
        .unwrap();
        writeln!(s, "pub const {name}: [&str; {len}] = [").unwrap();
        for (mnemonic, _) in &table {
            writeln!(s, "    \"{mnemonic}\",").unwrap();
        }
        writeln!(s, "];\n").unwrap();
        writeln!(
            s,
            "/// Which of them `isa/vpu.toml` says the model carries out."
        )
        .unwrap();
        writeln!(s, "pub const {name}_EXECUTE: [bool; {len}] = [").unwrap();
        for (_, status) in &table {
            writeln!(s, "    {},", *status == Status::Executes).unwrap();
        }
        writeln!(s, "];\n").unwrap();
    }
    s
}

impl Section {
    fn source_column(&self) -> bool {
        self.columns.last().map(String::as_str) == Some("Source")
    }
}

pub fn markdown(isa: &Isa) -> String {
    let mut s = String::new();
    writeln!(
        s,
        "<!-- generated from isa/vpu.toml by `cargo run -- spec-docs --update` – do not edit -->\n"
    )
    .unwrap();
    writeln!(s, "# {}\n", isa.title.trim()).unwrap();
    writeln!(s, "{}", isa.intro.trim()).unwrap();
    for section in &isa.sections {
        while s.ends_with("\n\n") {
            s.pop();
        }
        let hashes = "#".repeat(section.level.clamp(2, 4) as usize);
        writeln!(s, "\n{hashes} {}\n", section.title.trim()).unwrap();
        if let Some(body) = &section.body {
            writeln!(s, "{}\n", body.trim()).unwrap();
        }
        if !section.columns.is_empty() {
            writeln!(s, "| {} |", section.columns.join(" | ")).unwrap();
            writeln!(
                s,
                "|{}",
                section.columns.iter().map(|_| "---|").collect::<String>()
            )
            .unwrap();
            for row in &section.rows {
                let mut cells: Vec<String> = row.cells.iter().map(|c| cell(c)).collect();
                if section.source_column() {
                    cells.push(row_source(&row.sources));
                }
                writeln!(s, "| {} |", cells.join(" | ")).unwrap();
            }
            s.push('\n');
        }
        if let Some(after) = &section.after {
            writeln!(s, "{}\n", after.trim()).unwrap();
        }
        if !section.sources.is_empty() {
            writeln!(s, "Sources:\n").unwrap();
            for src in &section.sources {
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
            s.push('\n');
        }
    }
    while s.ends_with("\n\n") {
        s.pop();
    }
    s
}

/// A row's sources, as the one cell the table shows: each kind once, with the
/// references it names, and any note in italics after them.
fn row_source(sources: &[Source]) -> String {
    let mut out: Vec<(&str, Vec<String>)> = Vec::new();
    for src in sources {
        let mut text = src.reference.trim().to_string();
        if let Some(note) = &src.note {
            text = format!("{text} — _{}_", note.trim());
        }
        match out.iter_mut().find(|(kind, _)| *kind == src.kind.as_str()) {
            Some((_, refs)) => refs.push(text),
            None => out.push((src.kind.as_str(), vec![text])),
        }
    }
    out.iter()
        .map(|(kind, refs)| format!("{kind}: {}", refs.join(", ")))
        .collect::<Vec<_>>()
        .join("; ")
}

fn cell(s: &str) -> String {
    s.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .replace('|', "\\|")
}
