//! The shape of `isa/vpu.toml`, and the Markdown it renders to.

use crate::spec::schema::Source;
use serde::Deserialize;
use std::fmt::Write;

/// One instruction-set reference: a header, then sections in file order.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Isa {
    pub title: String,
    /// Markdown printed under the title, before the first section.
    pub intro: String,
    #[serde(default, rename = "section")]
    pub sections: Vec<Section>,
}

/// A section of the reference. Its `body` is prose; a `table` or a `rows`
/// list carries the facts, each row with the source it rests on.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Section {
    pub title: String,
    /// Heading depth: 2 for `##`, 3 for `###`.
    #[serde(default = "two")]
    pub level: u8,
    #[serde(default)]
    pub body: Option<String>,
    /// Markdown printed after the table.
    #[serde(default)]
    pub after: Option<String>,
    #[serde(default)]
    pub columns: Vec<String>,
    /// One row per fact. The last column is filled in from the row's sources
    /// when `columns` names one more column than the row has cells.
    #[serde(default, rename = "row")]
    pub rows: Vec<Row>,
    #[serde(default, rename = "source")]
    pub sources: Vec<Source>,
}

/// A row of a section's table, with what it rests on.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Row {
    pub cells: Vec<String>,
    #[serde(default, rename = "source")]
    pub sources: Vec<Source>,
}

fn two() -> u8 {
    2
}

/// Load one spec file, checking that every table is rectangular.
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
    Ok(isa)
}

impl Section {
    /// Whether the table's last column is the generated source column.
    fn source_column(&self) -> bool {
        self.columns.last().map(String::as_str) == Some("Source")
    }
}

/// Render the whole reference.
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

/// Markdown table cell: one line, no bare pipes.
fn cell(s: &str) -> String {
    s.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .replace('|', "\\|")
}
