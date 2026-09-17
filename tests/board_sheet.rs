//! The board sheets under `docs/` against the register specs and the model
//! (#110).
//!
//! A sheet is drawn by hand: no generator makes a board diagram that puts a
//! controller on the edge of the die, carries the power rails and reads as a
//! schematic. What a generator *can* do is refuse to let the drawing drift, so
//! a sheet marks up what it draws —
//!
//! ```text
//!   <svg … data-board="d03115">
//!   <g data-block="emmc2" data-base="0x7E340000"> … </g>
//!   <line … data-edge="fxl6408->bsc.PMIC"/>
//!   <text … data-block-alt="pmic_1d">0x1D instead, on rev <= 1.4</text>
//! ```
//!
//! — and these tests check the marks against `specs/*.toml` and against the
//! board the sheet names: every spec that board has is on the sheet, every name
//! on the sheet is a spec, every `parent` is drawn, an address written on a
//! part is the one its spec gives, and a part the board does *not* have is
//! marked `data-block-alt` rather than drawn as fitted.
//!
//! Every `docs/board-sheet*.svg` is checked, so another board is another
//! drawing and nothing else: no test to add, no generator, no palette to write.

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

use rpi_virt_fw::periph::pmic::{Pmic, ADDR_1D, ADDR_CORE, ADDR_RAILS};
use rpi_virt_fw::sheet;
use rpi_virt_fw::soc::Board;
use rpi_virt_fw::spec::{self, schema::Bus, schema::Spec};

/// One hand-drawn sheet.
struct Sheet {
    /// The path as the messages write it.
    what: String,
    path: PathBuf,
    svg: String,
}

fn sheets() -> Vec<Sheet> {
    let paths = sheet::light_sheets().unwrap_or_else(|e| panic!("{e}"));
    paths
        .into_iter()
        .map(|path| Sheet {
            what: sheet::display(&path),
            svg: std::fs::read_to_string(&path)
                .unwrap_or_else(|e| panic!("{}: {e}", path.display())),
            path,
        })
        .collect()
}

fn specs() -> Vec<Spec> {
    spec::load().unwrap_or_else(|e| panic!("{e}"))
}

impl Sheet {
    /// The board the sheet draws, from the revision code in `data-board`.
    fn board(&self) -> Board {
        let code = attrs(&self.svg, "data-board").pop().unwrap_or_else(|| {
            panic!(
                "{}: the <svg> element needs a `data-board` revision code, so \
                 the checks know which board is drawn",
                self.what
            )
        });
        let revision = Board::parse_revision(&code)
            .unwrap_or_else(|e| panic!("{}: data-board {code:?}: {e}", self.what));
        Board {
            revision,
            ..Board::default()
        }
    }

    /// Every `data-block` name: the parts the sheet draws as fitted.
    fn fitted(&self) -> BTreeSet<String> {
        names(&self.svg, "data-block")
    }

    /// Every `data-block-alt` name: parts named for contrast, which this board
    /// does not have.
    fn alternatives(&self) -> BTreeSet<String> {
        names(&self.svg, "data-block-alt")
    }
}

/// The value of every `attr="…"` in `svg`, in document order.
fn attrs(svg: &str, attr: &str) -> Vec<String> {
    let needle = format!("{attr}=\"");
    let mut out = Vec::new();
    let mut rest = svg;
    while let Some(at) = rest.find(&needle) {
        // `data-block="…"` must not match the tail of `data-block-alt="…"`.
        let partial = rest[..at].ends_with(|c: char| c.is_ascii_alphanumeric() || c == '-');
        rest = &rest[at + needle.len()..];
        let Some(end) = rest.find('"') else { break };
        if !partial {
            out.push(rest[..end].to_string());
        }
        rest = &rest[end + 1..];
    }
    out
}

/// Every space-separated name in every `attr`.
fn names(svg: &str, attr: &str) -> BTreeSet<String> {
    attrs(svg, attr)
        .iter()
        .flat_map(|v| v.split_whitespace().map(str::to_string).collect::<Vec<_>>())
        .collect()
}

/// Every `data-edge`, as `(child, parent)`. `&gt;` is how the arrow is spelt
/// in an attribute.
fn drawn_edges(sheet: &Sheet) -> BTreeSet<(String, String)> {
    let mut out = BTreeSet::new();
    for value in attrs(&sheet.svg, "data-edge") {
        for edge in value.split_whitespace() {
            let edge = edge.replace("&gt;", ">");
            match edge.split_once("->") {
                Some((child, parent)) => {
                    out.insert((child.to_string(), parent.to_string()));
                }
                None => panic!("{}: data-edge {edge:?} is not `child->parent`", sheet.what),
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

/// Every `<g data-block=…>` on a sheet, as `(names, base attribute, text)`.
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

/// Whether `board` has this block fitted, for the blocks whose presence depends
/// on the board — today the three PMICs, which `Pmic::for_board` chooses
/// between. `None` is a block every board has.
///
/// The model is the authority here on purpose: another board's sheet gets the
/// answer from the same code the boot does, not from a table in a test.
fn fitted_on(board: Board, spec: &Spec) -> Option<bool> {
    let addr = u8::try_from(spec.block.base).ok()?;
    if spec.block.bus != Bus::I2c || ![ADDR_CORE, ADDR_RAILS, ADDR_1D].contains(&addr) {
        return None;
    }
    Some(Pmic::for_board(board).responds_to(addr))
}

#[test]
fn every_spec_is_on_its_board_sheet() {
    let specs = specs();
    for sheet in sheets() {
        let board = sheet.board();
        let fitted = sheet.fitted();
        let alternatives = sheet.alternatives();
        for spec in &specs {
            let name = &spec.block.name;
            if fitted_on(board, spec) == Some(false) {
                assert!(
                    !fitted.contains(name),
                    "{}: {name} is drawn as fitted, but a {:06x} board does not \
                     have it; mark it `data-block-alt` or leave it out",
                    sheet.what,
                    board.revision
                );
                continue;
            }
            assert!(
                fitted.contains(name),
                "{} does not name {}; draw it, or add its name to the \
                 `data-block` of the part it lives in",
                sheet.what,
                spec.file
            );
            assert!(
                !alternatives.contains(name),
                "{}: {name} is marked `data-block-alt`, but a {:06x} board has it",
                sheet.what,
                board.revision
            );
        }
    }
}

#[test]
fn every_name_on_a_sheet_is_a_spec() {
    let specs = specs();
    for sheet in sheets() {
        for name in sheet.fitted().union(&sheet.alternatives()) {
            assert!(
                specs.iter().any(|s| s.block.name == *name),
                "{}: {name:?} is not a block in specs/",
                sheet.what
            );
        }
    }
}

#[test]
fn every_parent_link_is_drawn() {
    let specs = specs();
    let mut wanted = BTreeSet::new();
    for spec in &specs {
        if let Some(p) = &spec.block.parent {
            wanted.insert((spec.block.name.clone(), p.name.clone()));
        }
    }

    for sheet in sheets() {
        let drawn = drawn_edges(&sheet);
        let named = sheet.fitted();
        for edge in &wanted {
            if !named.contains(&edge.0) {
                continue; // a part this board does not have
            }
            assert!(
                drawn.contains(edge),
                "{} does not draw {} -> {}; the wire or connector needs \
                 `data-edge=\"{}-&gt;{}\"`",
                sheet.what,
                edge.0,
                edge.1,
                edge.0,
                edge.1
            );
        }
        for edge in &drawn {
            assert!(
                wanted.contains(edge),
                "{}: data-edge {} -> {} is not a `parent` in specs/",
                sheet.what,
                edge.0,
                edge.1
            );
        }
    }
}

/// A part that writes an address writes the one its spec gives, and writes it
/// where a reader can see it. `data-base` is what the drawing says; the test
/// ties it both to the spec and to the visible label.
#[test]
fn every_drawn_address_matches_its_spec() {
    let specs = specs();
    let by_name: BTreeMap<&str, &Spec> = specs.iter().map(|s| (s.block.name.as_str(), s)).collect();

    for sheet in sheets() {
        for (names, base, text) in groups(&sheet.svg) {
            let Some(base) = base else { continue };
            assert_eq!(
                names.len(),
                1,
                "{}: data-base {base:?} sits on a part that names {names:?}; \
                 an address belongs to one block",
                sheet.what
            );
            let spec = by_name[names[0].as_str()];
            let value = match base.strip_prefix("0x") {
                Some(hex) => u32::from_str_radix(hex, 16),
                None => base.parse(),
            }
            .unwrap_or_else(|e| panic!("{}: data-base {base:?} on {}: {e}", sheet.what, names[0]));
            assert!(
                spec.bases().any(|b| b == value),
                "{}: {} is drawn at {base}, which is neither its base nor a copy's",
                sheet.what,
                names[0]
            );
            assert!(
                text.contains(&base),
                "{}: {} carries data-base {base:?}, but the part's own label \
                 does not show it: {text:?}",
                sheet.what,
                names[0]
            );
        }
    }
}

/// A dark sheet is not drawn, it is generated: the same drawing with the dark
/// palette. A part moved on one and not the other is exactly the drift nobody
/// notices by eye.
#[test]
fn every_dark_sheet_matches_its_hand_drawn_one() {
    for sheet in sheets() {
        let wanted = sheet::dark_variant(&sheet.svg, &sheet.what).unwrap_or_else(|e| panic!("{e}"));
        let path = sheet::dark_path(&sheet.path);
        let found = std::fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("{}: {e}", sheet::display(&path)));
        if found == wanted {
            continue;
        }
        panic!(
            "{} is not {} with the dark palette; run \
             `cargo run -- spec-docs --update`\n\n{}",
            sheet::display(&path),
            sheet.what,
            first_difference(&found, &wanted)
        );
    }
}

/// Where two sheets first differ, for a message that fits on a screen.
fn first_difference(found: &str, wanted: &str) -> String {
    match found.lines().zip(wanted.lines()).position(|(a, b)| a != b) {
        Some(i) => format!(
            "line {}:\n  the file has: {}\n  should be:   {}",
            i + 1,
            found.lines().nth(i).unwrap_or(""),
            wanted.lines().nth(i).unwrap_or("")
        ),
        None => format!(
            "{} lines vs {}",
            found.lines().count(),
            wanted.lines().count()
        ),
    }
}

/// A sheet without a twin, or a twin whose sheet is gone, is what
/// `sheet::sync` reports — the same check `spec-docs` runs.
#[test]
fn the_sheets_and_their_twins_are_in_step() {
    let stale = sheet::sync(false).unwrap_or_else(|e| panic!("{e}"));
    assert!(
        stale.is_empty(),
        "out of date: {}; run `cargo run -- spec-docs --update`",
        stale
            .iter()
            .map(|p| sheet::display(p))
            .collect::<Vec<_>>()
            .join(", ")
    );
}
