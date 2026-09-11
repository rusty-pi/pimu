//! Minimal reader for the VideoCore `dt-blob` (a small flattened device tree,
//! distinct from the ARM DT — see `scripts/make-dt-blob.py`).
//!
//! Only what the `gpioman` shim needs: the `pin_defines` map from the board's
//! `pins_*` section — pin name → `{ number, type }` as declared by each
//! `pin_define@<NAME>` node. This is not a general FDT library; it walks the
//! struct block once and pulls out that one subtree.

use std::collections::HashMap;

const FDT_MAGIC: u32 = 0xD00D_FEED;
const FDT_BEGIN_NODE: u32 = 1;
const FDT_END_NODE: u32 = 2;
const FDT_PROP: u32 = 3;
const FDT_NOP: u32 = 4;
const FDT_END: u32 = 9;

/// One `pin_define@<NAME>` entry.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PinDef {
    /// `number = <N>;` if the node carries one.
    pub number: Option<u32>,
    /// `type = "...";` — `"internal"`, `"external"`, `"absent"`, …
    pub kind: Option<String>,
}

impl PinDef {
    /// The pin is declared but explicitly not present on this board
    /// (`type = "absent"`), so `gpioman_get_pin_num` knows the name but has no
    /// number to hand back — it returns -1 *without* the "not defined" log.
    pub fn is_absent(&self) -> bool {
        self.kind.as_deref() == Some("absent")
    }
}

/// Pin name → definition, for one board section.
pub type PinMap = HashMap<String, PinDef>;

fn be32(b: &[u8], off: usize) -> Option<u32> {
    b.get(off..off + 4)
        .map(|s| u32::from_be_bytes([s[0], s[1], s[2], s[3]]))
}

fn cstr(b: &[u8], off: usize) -> &[u8] {
    let end = b[off..]
        .iter()
        .position(|&c| c == 0)
        .map_or(b.len(), |p| off + p);
    &b[off..end]
}

/// Parse `blob` and return the `pin_defines` map from the first board section
/// under `/videocore` whose name matches one of `prefer` (in order); if none
/// match, the first `pins_*` section found. `None` if the blob is not a
/// dt-blob or has no such section.
pub fn pin_map(blob: &[u8], prefer: &[&str]) -> Option<PinMap> {
    if be32(blob, 0)? != FDT_MAGIC {
        return None;
    }
    let off_struct = be32(blob, 8)? as usize;
    let off_strings = be32(blob, 12)? as usize;
    let size_struct = be32(blob, 36).unwrap_or(0) as usize;
    let strings = blob.get(off_strings..)?;
    let s = blob.get(off_struct..off_struct + size_struct.max(4))?;

    // Depth-tagged walk. `path` holds the node-name stack.
    let mut pos = 0usize;
    let mut path: Vec<String> = Vec::new();
    // (section name, its pin_defines map) for every board section seen.
    let mut sections: Vec<(String, PinMap)> = Vec::new();
    let mut cur_section: Option<usize> = None;

    while pos + 4 <= s.len() {
        let tok = be32(s, pos)?;
        pos += 4;
        match tok {
            FDT_BEGIN_NODE => {
                let name = String::from_utf8_lossy(cstr(s, pos)).into_owned();
                pos += name.len() + 1;
                pos = (pos + 3) & !3;

                let is_section = path.len() == 2
                    && path[0].is_empty()
                    && path[1] == "videocore"
                    && name.starts_with("pins");
                if is_section {
                    sections.push((name.clone(), PinMap::new()));
                    cur_section = Some(sections.len() - 1);
                }
                path.push(name);
            }
            FDT_END_NODE => {
                let leaving = path.pop();
                if let (Some(idx), Some(leaving)) = (cur_section, leaving) {
                    if sections[idx].0 == leaving {
                        cur_section = None;
                    }
                }
            }
            FDT_PROP => {
                let len = be32(s, pos)? as usize;
                let nameoff = be32(s, pos + 4)? as usize;
                pos += 8;
                let val = s.get(pos..pos + len)?;
                let pname = String::from_utf8_lossy(cstr(strings, nameoff)).into_owned();
                pos += len;
                pos = (pos + 3) & !3;

                // Inside `.../pins_XXX/pin_defines/pin_define@<NAME>`?
                if let Some(idx) = cur_section {
                    let depth = path.len();
                    if depth >= 2
                        && path[depth - 1].starts_with("pin_define@")
                        && path[depth - 2] == "pin_defines"
                    {
                        let key = path[depth - 1]["pin_define@".len()..].to_string();
                        let entry = sections[idx].1.entry(key).or_default();
                        match pname.as_str() {
                            "number" if len >= 4 => {
                                entry.number = be32(val, 0);
                            }
                            "type" => {
                                entry.kind =
                                    Some(String::from_utf8_lossy(cstr(val, 0)).into_owned());
                            }
                            _ => {}
                        }
                    }
                }
            }
            FDT_NOP => {}
            FDT_END => break,
            _ => return None,
        }
    }

    if sections.is_empty() {
        return None;
    }
    for want in prefer {
        if let Some((_, m)) = sections.iter().find(|(n, _)| n == want) {
            return Some(m.clone());
        }
    }
    Some(sections.remove(0).1)
}

#[cfg(test)]
mod tests {
    use super::*;

    // A tiny hand-built dt-blob: /videocore/pins_4b/pin_defines/{FOO=<7>,
    // BAR type="absent"}.
    fn sample() -> Vec<u8> {
        fn be(v: u32) -> [u8; 4] {
            v.to_be_bytes()
        }
        let mut strings = Vec::new();
        let soff = |strings: &mut Vec<u8>, s: &str| -> u32 {
            let o = strings.len() as u32;
            strings.extend_from_slice(s.as_bytes());
            strings.push(0);
            o
        };
        let s_number = soff(&mut strings, "number");
        let s_type = soff(&mut strings, "type");

        let mut st = Vec::new();
        let node = |st: &mut Vec<u8>, name: &str| {
            st.extend_from_slice(&be(FDT_BEGIN_NODE));
            st.extend_from_slice(name.as_bytes());
            st.push(0);
            while !st.len().is_multiple_of(4) {
                st.push(0);
            }
        };
        let end = |st: &mut Vec<u8>| st.extend_from_slice(&be(FDT_END_NODE));
        let prop_u32 = |st: &mut Vec<u8>, noff: u32, v: u32| {
            st.extend_from_slice(&be(FDT_PROP));
            st.extend_from_slice(&be(4));
            st.extend_from_slice(&be(noff));
            st.extend_from_slice(&be(v));
        };
        let prop_str = |st: &mut Vec<u8>, noff: u32, v: &str| {
            let mut b = v.as_bytes().to_vec();
            b.push(0);
            st.extend_from_slice(&be(FDT_PROP));
            st.extend_from_slice(&be(b.len() as u32));
            st.extend_from_slice(&be(noff));
            st.extend_from_slice(&b);
            while !st.len().is_multiple_of(4) {
                st.push(0);
            }
        };

        node(&mut st, ""); // root
        node(&mut st, "videocore");
        node(&mut st, "pins_4b");
        node(&mut st, "pin_defines");
        node(&mut st, "pin_define@FOO");
        prop_u32(&mut st, s_number, 7);
        end(&mut st);
        node(&mut st, "pin_define@BAR");
        prop_str(&mut st, s_type, "absent");
        end(&mut st);
        end(&mut st); // pin_defines
        end(&mut st); // pins_4b
        end(&mut st); // videocore
        end(&mut st); // root
        st.extend_from_slice(&be(FDT_END));

        let header_size = 40u32;
        let rsvmap = [0u8; 16];
        let off_struct = header_size + rsvmap.len() as u32;
        let off_strings = off_struct + st.len() as u32;
        let total = off_strings + strings.len() as u32;
        let mut out = Vec::new();
        out.extend_from_slice(&be(FDT_MAGIC));
        out.extend_from_slice(&be(total));
        out.extend_from_slice(&be(off_struct));
        out.extend_from_slice(&be(off_strings));
        out.extend_from_slice(&be(header_size)); // off_rsvmap
        out.extend_from_slice(&be(17));
        out.extend_from_slice(&be(16));
        out.extend_from_slice(&be(0));
        out.extend_from_slice(&be(strings.len() as u32));
        out.extend_from_slice(&be(st.len() as u32));
        out.extend_from_slice(&rsvmap);
        out.extend_from_slice(&st);
        out.extend_from_slice(&strings);
        out
    }

    #[test]
    fn reads_pin_defines() {
        let m = pin_map(&sample(), &["pins_4b"]).expect("map");
        assert_eq!(m["FOO"].number, Some(7));
        assert!(m["BAR"].is_absent());
        assert_eq!(m["BAR"].number, None);
        assert!(!m.contains_key("MISSING"));
    }

    #[test]
    fn rejects_non_dtblob() {
        assert!(pin_map(b"not a blob", &[]).is_none());
    }
}
