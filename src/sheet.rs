//! The board sheets under `docs/`: which files there are, and their two colour
//! ways.
//!
//! A sheet — `docs/board-sheet.svg`, and `docs/board-sheet-<board>.svg` per
//! board — is drawn by hand. What is generated is everything that could drift:
//! `tests/board_sheet.rs` checks each sheet against `specs/*.toml`, and the dark
//! twin `…-dark.svg` is the same drawing with the dark palette substituted into
//! its marked block, so a part cannot move on one and not the other.

use std::path::{Path, PathBuf};

/// The palette block's markers: what lies between them is generated for the
/// dark twin, everything outside is copied.
const PALETTE_START: &str = "    /* palette: light";
const PALETTE_END: &str = "    /* end palette */";

const PREFIX: &str = "board-sheet";
const DARK_SUFFIX: &str = "-dark.svg";

/// Every colour the sheets use, as `(custom property, light, dark)`. The light
/// values are checked against the drawings, so changing a colour without adding
/// it here fails rather than silently leaving the dark twin behind.
pub const PALETTE: &[(&str, &str, &str)] = &[
    ("paper", "#F7F8F6", "#0D1117"),
    ("sheet", "#FBFCFB", "#161B22"),
    ("soc", "#E7EEEB", "#172227"),
    ("grp", "#DCE6E2", "#1F2B31"),
    ("ink", "#16232B", "#E6EDF3"),
    ("muted", "#4C5C63", "#9FB0B8"),
    ("dim", "#6A787E", "#7D8B91"),
    ("wire", "#136B43", "#56D399"),
    ("part", "#8C2F1E", "#E8917A"),
    ("net", "#2B55A6", "#8DB0F4"),
    ("power", "#9C5200", "#E8A33D"),
];

fn root() -> &'static Path {
    Path::new(env!("CARGO_MANIFEST_DIR"))
}

pub fn dir() -> PathBuf {
    root().join("docs")
}

/// The sheet of the default board, the one `docs/README.md` shows first.
pub fn light_path() -> PathBuf {
    dir().join("board-sheet.svg")
}

pub fn light_sheets() -> Result<Vec<PathBuf>, String> {
    let dir = dir();
    let mut out: Vec<PathBuf> = std::fs::read_dir(&dir)
        .map_err(|e| format!("{}: {e}", dir.display()))?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| {
            let name = p.file_name().unwrap_or_default().to_string_lossy();
            name.starts_with(PREFIX) && name.ends_with(".svg") && !name.ends_with(DARK_SUFFIX)
        })
        .collect();
    out.sort();
    if out.is_empty() {
        return Err(format!("{}: no {PREFIX}*.svg", dir.display()));
    }
    Ok(out)
}

pub fn dark_path(light: &Path) -> PathBuf {
    let stem = light.file_stem().unwrap_or_default().to_string_lossy();
    light.with_file_name(format!("{stem}{DARK_SUFFIX}"))
}

/// The palette block a sheet should carry, for the light or the dark side.
fn palette_block(dark: bool) -> String {
    let mut s = String::new();
    if dark {
        s.push_str("    /* palette: dark. Generated from the light sheet beside it by\n");
        s.push_str("       `cargo run -- spec-docs --update` — edit that file, not this one. */\n");
    } else {
        s.push_str("    /* palette: light. The dark twin is generated from this block —\n");
        s.push_str("       `cargo run -- spec-docs --update` after changing a colour here. */\n");
    }
    s.push_str("    svg {\n");
    for (name, light, night) in PALETTE {
        let value = if dark { night } else { light };
        s.push_str(&format!("      --{name}: {value};\n"));
    }
    s.push_str("    }\n");
    s.push_str(PALETTE_END);
    s.push('\n');
    s
}

fn around_palette(svg: &str, what: &str) -> Result<(usize, usize), String> {
    let start = svg
        .find(PALETTE_START)
        .ok_or(format!("{what}: no `/* palette: light … */` block"))?;
    let end = svg[start..]
        .find(PALETTE_END)
        .map(|i| start + i + PALETTE_END.len() + 1)
        .ok_or(format!("{what}: no `/* end palette */`"))?;
    Ok((start, end))
}

/// The dark twin of the hand-drawn `light` sheet. Fails when the sheet's own
/// palette is not the one [`PALETTE`] records: the two have drifted.
pub fn dark_variant(light: &str, what: &str) -> Result<String, String> {
    let (start, end) = around_palette(light, what)?;
    let wanted = palette_block(false);
    let found = &light[start..end];
    if found != wanted {
        return Err(format!(
            "{what}: the palette is not the one `sheet::PALETTE` records. \
             Update the table (or the drawing) so they agree.\n\
             \nthe drawing has:\n{found}\n`PALETTE` says:\n{wanted}"
        ));
    }
    Ok(format!(
        "{}{}{}",
        &light[..start],
        palette_block(true),
        &light[end..]
    ))
}

/// Every dark twin that is out of date, plus twins whose sheet is gone; with
/// `update`, after rewriting them.
pub fn sync(update: bool) -> Result<Vec<PathBuf>, String> {
    let mut stale = Vec::new();
    let sheets = light_sheets()?;

    for light_path in &sheets {
        let what = display(light_path);
        let light =
            std::fs::read_to_string(light_path).map_err(|e| format!("{}: {e}", what.clone()))?;
        let dark = dark_variant(&light, &what)?;
        let path = dark_path(light_path);
        if std::fs::read_to_string(&path).ok().as_deref() == Some(dark.as_str()) {
            continue;
        }
        stale.push(path.clone());
        if update {
            std::fs::write(&path, &dark).map_err(|e| format!("{}: {e}", path.display()))?;
        }
    }

    // A twin whose sheet was renamed or removed is stale the other way.
    let wanted: Vec<PathBuf> = sheets.iter().map(|p| dark_path(p)).collect();
    if let Ok(entries) = std::fs::read_dir(dir()) {
        for entry in entries.flatten() {
            let path = entry.path();
            let name = entry.file_name().to_string_lossy().into_owned();
            if !name.starts_with(PREFIX) || !name.ends_with(DARK_SUFFIX) || wanted.contains(&path) {
                continue;
            }
            stale.push(path.clone());
            if update {
                std::fs::remove_file(&path).map_err(|e| format!("{}: {e}", path.display()))?;
            }
        }
    }

    stale.sort();
    Ok(stale)
}

pub fn display(path: &Path) -> String {
    path.strip_prefix(root())
        .unwrap_or(path)
        .display()
        .to_string()
}
