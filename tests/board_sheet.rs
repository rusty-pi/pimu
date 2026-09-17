//! `docs/board-sheet.svg` against the register specs (#110).
//!
//! The sheet is drawn by hand: no generator makes a board diagram that puts a
//! controller on the edge of the die, carries the power rails and reads as a
//! schematic. What a generator *can* do is refuse to let the drawing drift, so
//! the sheet marks up what it draws —
//!
//! ```text
//!   <g data-block="emmc2" data-base="0x7E340000"> … </g>
//!   <line … data-edge="fxl6408->bsc.PMIC"/>
//! ```
//!
//! — and these tests check the marks against `specs/*.toml`: every spec is on
//! the sheet, every name on the sheet is a spec, every `parent` link is drawn,
//! and an address written on a part is the one its spec gives. Adding a spec
//! and forgetting the drawing is the failure this catches.

use std::collections::{BTreeMap, BTreeSet};

use rpi_virt_fw::spec::{self, schema::Spec};

const SHEET: &str = "docs/board-sheet.svg";

fn sheet() -> String {
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/docs/board-sheet.svg");
    std::fs::read_to_string(path).unwrap_or_else(|e| panic!("{SHEET}: {e}"))
}

fn specs() -> Vec<Spec> {
    spec::load().unwrap_or_else(|e| panic!("{e}"))
}

/// The value of every `attr="…"` in `svg`, in document order.
fn attrs(svg: &str, attr: &str) -> Vec<String> {
    let needle = format!("{attr}=\"");
    let mut out = Vec::new();
    let mut rest = svg;
    while let Some(at) = rest.find(&needle) {
        rest = &rest[at + needle.len()..];
        match rest.find('"') {
            Some(end) => {
                out.push(rest[..end].to_string());
                rest = &rest[end + 1..];
            }
            None => break,
        }
    }
    out
}

/// Every `data-block` name the sheet carries.
fn drawn_blocks(svg: &str) -> BTreeSet<String> {
    attrs(svg, "data-block")
        .iter()
        .flat_map(|v| v.split_whitespace().map(str::to_string).collect::<Vec<_>>())
        .collect()
}

/// Every `data-edge`, as `(child, parent)`. `&gt;` is how the arrow is spelt
/// in the attribute.
fn drawn_edges(svg: &str) -> BTreeSet<(String, String)> {
    let mut out = BTreeSet::new();
    for value in attrs(svg, "data-edge") {
        for edge in value.split_whitespace() {
            let edge = edge.replace("&gt;", ">");
            match edge.split_once("->") {
                Some((child, parent)) => {
                    out.insert((child.to_string(), parent.to_string()));
                }
                None => panic!("{SHEET}: data-edge {edge:?} is not `child->parent`"),
            }
        }
    }
    out
}

/// The text inside the element that starts at `from`, which must be a `<g>`:
/// everything between a `>` and the next `<`, up to the matching `</g>`.
fn group_text(svg: &str, from: usize) -> String {
    let open = svg[from..].find('>').map(|i| from + i + 1).unwrap();
    let close = svg[open..]
        .find("</g>")
        .map(|i| open + i)
        .unwrap_or(svg.len());
    let mut text = String::new();
    let mut rest = &svg[open..close];
    while let Some(at) = rest.find('>') {
        rest = &rest[at + 1..];
        let end = rest.find('<').unwrap_or(rest.len());
        text.push_str(&rest[..end]);
        text.push(' ');
        rest = &rest[end..];
    }
    text
}

/// Every `<g data-block=…>` on the sheet, as `(names, base attribute, text)`.
fn groups(svg: &str) -> Vec<(Vec<String>, Option<String>, String)> {
    let mut out = Vec::new();
    let mut at = 0;
    while let Some(i) = svg[at..].find("<g ") {
        let start = at + i;
        let tag_end = svg[start..].find('>').map(|e| start + e).unwrap();
        let tag = &svg[start..tag_end];
        at = tag_end;
        let Some(names) = attrs(tag, "data-block").pop() else {
            continue;
        };
        out.push((
            names.split_whitespace().map(str::to_string).collect(),
            attrs(tag, "data-base").pop(),
            group_text(svg, start),
        ));
    }
    out
}

#[test]
fn every_spec_is_on_the_sheet() {
    let svg = sheet();
    let drawn = drawn_blocks(&svg);
    for spec in specs() {
        assert!(
            drawn.contains(&spec.block.name),
            "{SHEET} does not name {}; draw it, or add its name to the \
             `data-block` of the part it lives in",
            spec.file
        );
    }
}

#[test]
fn every_name_on_the_sheet_is_a_spec() {
    let svg = sheet();
    let specs = specs();
    for name in drawn_blocks(&svg) {
        assert!(
            specs.iter().any(|s| s.block.name == name),
            "{SHEET}: data-block {name:?} is not a block in specs/"
        );
    }
}

#[test]
fn every_parent_link_is_drawn() {
    let svg = sheet();
    let drawn = drawn_edges(&svg);
    let specs = specs();

    let mut wanted = BTreeSet::new();
    for spec in &specs {
        if let Some(p) = &spec.block.parent {
            wanted.insert((spec.block.name.clone(), p.name.clone()));
        }
    }
    for edge in &wanted {
        assert!(
            drawn.contains(edge),
            "{SHEET} does not draw {} -> {}; the wire or connector needs \
             `data-edge=\"{}-&gt;{}\"`",
            edge.0,
            edge.1,
            edge.0,
            edge.1
        );
    }
    for edge in &drawn {
        assert!(
            wanted.contains(edge),
            "{SHEET}: data-edge {} -> {} is not a `parent` in specs/",
            edge.0,
            edge.1
        );
    }
}

/// A part that writes an address writes the one its spec gives, and writes it
/// where a reader can see it. `data-base` is what the drawing says; the test
/// ties it both to the spec and to the visible label.
#[test]
fn every_drawn_address_matches_its_spec() {
    let svg = sheet();
    let specs = specs();
    let by_name: BTreeMap<&str, &Spec> = specs.iter().map(|s| (s.block.name.as_str(), s)).collect();

    for (names, base, text) in groups(&svg) {
        let Some(base) = base else { continue };
        assert_eq!(
            names.len(),
            1,
            "{SHEET}: data-base {base:?} sits on a part that names \
             {names:?}; an address belongs to one block"
        );
        let spec = by_name[names[0].as_str()];
        let value = match base.strip_prefix("0x") {
            Some(hex) => u32::from_str_radix(hex, 16),
            None => base.parse(),
        }
        .unwrap_or_else(|e| panic!("{SHEET}: data-base {base:?} on {}: {e}", names[0]));
        assert!(
            spec.bases().any(|b| b == value),
            "{SHEET}: {} is drawn at {base}, which is neither its base nor a copy's",
            names[0]
        );
        assert!(
            text.contains(&base),
            "{SHEET}: {} carries data-base {base:?}, but the part's own \
             label does not show it: {text:?}",
            names[0]
        );
    }
}
