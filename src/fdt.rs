//! A read-only flattened-device-tree reader, just enough to look inside the
//! blob `start4.elf` hands to the ARM, plus one narrow writer
//! ([`Fdt::with_property`], which replaces the value of a property that already
//! exists) for `earlycon` onto `/chosen/bootargs`.
//!
//! Getting the *patched* tree back out of the model is how two firmware
//! versions are diffed — `/chosen/rpi-machine-id` is the property that must not
//! move, but nothing here is specific to it. Not a
//! general DTB library: no phandle resolution, no memory-reservation walk.
//!
//! Spec: Devicetree Specification v0.4, section 5.

pub const FDT_MAGIC: u32 = 0xd00d_feed;

const FDT_BEGIN_NODE: u32 = 1;
const FDT_END_NODE: u32 = 2;
const FDT_PROP: u32 = 3;
const FDT_NOP: u32 = 4;
const FDT_END: u32 = 9;

#[derive(Debug, Clone, Copy)]
pub struct Header {
    pub totalsize: u32,
    pub off_dt_struct: u32,
    pub off_dt_strings: u32,
    pub size_dt_struct: u32,
    pub size_dt_strings: u32,
    pub version: u32,
}

#[derive(Debug, Clone)]
pub struct Property {
    pub name: String,
    pub value: Vec<u8>,
}

impl Property {
    /// The value rendered the way `fdtdump` would: string, big-endian cells or
    /// raw hex, whichever it looks like.
    pub fn display(&self) -> String {
        if let Some(s) = self.as_str() {
            return format!("{s:?}");
        }
        if !self.value.is_empty() && self.value.len().is_multiple_of(4) && self.value.len() <= 16 {
            let words: Vec<String> = self
                .value
                .as_chunks::<4>()
                .0
                .iter()
                .map(|c| format!("{:#010x}", be32(c)))
                .collect();
            return format!("<{}>", words.join(" "));
        }
        self.value.iter().map(|b| format!("{b:02x}")).collect()
    }

    /// The value as text, if every byte is printable; a trailing NUL or LF,
    /// which `start4` appends, is trimmed.
    pub fn as_str(&self) -> Option<String> {
        let mut v = self.value.as_slice();
        while let Some((&last, rest)) = v.split_last() {
            if last == 0 || last == b'\n' {
                v = rest;
            } else {
                break;
            }
        }
        if v.is_empty() || !v.iter().all(|&b| (0x20..0x7f).contains(&b)) {
            return None;
        }
        Some(String::from_utf8_lossy(v).into_owned())
    }
}

pub struct Fdt<'a> {
    blob: &'a [u8],
    header: Header,
}

fn be32(b: &[u8]) -> u32 {
    u32::from_be_bytes([b[0], b[1], b[2], b[3]])
}

impl<'a> Fdt<'a> {
    /// Validate the header and wrap the blob. Errors separate "not a device
    /// tree" from "truncated": both are plausible when the address came out of
    /// a firmware log line.
    pub fn parse(blob: &'a [u8]) -> Result<Fdt<'a>, String> {
        if blob.len() < 40 {
            return Err(format!(
                "too short for an FDT header ({} bytes)",
                blob.len()
            ));
        }
        let magic = be32(&blob[0..4]);
        if magic != FDT_MAGIC {
            return Err(format!("bad magic {magic:#010x} (want {FDT_MAGIC:#010x})"));
        }
        let header = Header {
            totalsize: be32(&blob[4..8]),
            off_dt_struct: be32(&blob[8..12]),
            off_dt_strings: be32(&blob[12..16]),
            version: be32(&blob[20..24]),
            size_dt_strings: be32(&blob[32..36]),
            size_dt_struct: be32(&blob[36..40]),
        };
        let end_struct = header.off_dt_struct as usize + header.size_dt_struct as usize;
        let end_strings = header.off_dt_strings as usize + header.size_dt_strings as usize;
        if header.totalsize as usize > blob.len()
            || end_struct > blob.len()
            || end_strings > blob.len()
        {
            return Err(format!(
                "truncated: totalsize {} struct end {} strings end {} but only {} bytes available",
                header.totalsize,
                end_struct,
                end_strings,
                blob.len()
            ));
        }
        Ok(Fdt { blob, header })
    }

    pub fn header(&self) -> Header {
        self.header
    }

    pub fn bytes(&self) -> &'a [u8] {
        &self.blob[..self.header.totalsize as usize]
    }

    fn string(&self, off: u32) -> String {
        let start = self.header.off_dt_strings as usize + off as usize;
        let rest = &self.blob[start.min(self.blob.len())..];
        let end = rest.iter().position(|&b| b == 0).unwrap_or(rest.len());
        String::from_utf8_lossy(&rest[..end]).into_owned()
    }

    /// Every property of one node by full path; an absent node gives `None`,
    /// which differs from a node with no properties.
    pub fn properties_of(&self, path: &str) -> Option<Vec<Property>> {
        let want: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();
        let mut stack: Vec<String> = Vec::new();
        let matches = |stack: &[String], want: &[&str]| -> bool {
            stack.len() == want.len() + 1 && stack[1..].iter().zip(want).all(|(a, b)| a == b)
        };
        let mut props = Vec::new();
        let mut found = false;

        let mut p = self.header.off_dt_struct as usize;
        let end = (p + self.header.size_dt_struct as usize).min(self.blob.len());
        while p + 4 <= end {
            let token = be32(&self.blob[p..p + 4]);
            p += 4;
            match token {
                FDT_BEGIN_NODE => {
                    let rest = &self.blob[p..end];
                    let nlen = rest.iter().position(|&b| b == 0).unwrap_or(0);
                    let name = String::from_utf8_lossy(&rest[..nlen]).into_owned();
                    p += (nlen + 4) & !3;
                    stack.push(name);
                    if matches(&stack, &want) {
                        found = true;
                    }
                }
                FDT_END_NODE => {
                    if matches(&stack, &want) {
                        return Some(props);
                    }
                    stack.pop();
                }
                FDT_PROP => {
                    if p + 8 > end {
                        break;
                    }
                    let len = be32(&self.blob[p..p + 4]) as usize;
                    let nameoff = be32(&self.blob[p + 4..p + 8]);
                    p += 8;
                    let data_end = (p + len).min(self.blob.len());
                    if matches(&stack, &want) {
                        props.push(Property {
                            name: self.string(nameoff),
                            value: self.blob[p..data_end].to_vec(),
                        });
                    }
                    p += (len + 3) & !3;
                }
                FDT_NOP => {}
                FDT_END => break,
                _ => break,
            }
        }
        if found {
            Some(props)
        } else {
            None
        }
    }
    /// Every node in the tree, depth first, as `(depth, path, properties)`;
    /// the root is depth 0 at `/`. General on purpose: a firmware bump may add
    /// nodes as readily as it changes a property inside one.
    pub fn nodes(&self) -> Vec<(usize, String, Vec<Property>)> {
        let mut out: Vec<(usize, String, Vec<Property>)> = Vec::new();
        let mut open: Vec<usize> = Vec::new();
        let mut names: Vec<String> = Vec::new();

        let mut p = self.header.off_dt_struct as usize;
        let end = (p + self.header.size_dt_struct as usize).min(self.blob.len());
        while p + 4 <= end {
            let token = be32(&self.blob[p..p + 4]);
            p += 4;
            match token {
                FDT_BEGIN_NODE => {
                    let rest = &self.blob[p..end];
                    let nlen = rest.iter().position(|&b| b == 0).unwrap_or(0);
                    let name = String::from_utf8_lossy(&rest[..nlen]).into_owned();
                    p += (nlen + 4) & !3;
                    names.push(name);
                    let path = if names.len() == 1 {
                        "/".to_string()
                    } else {
                        format!("/{}", names[1..].join("/"))
                    };
                    open.push(out.len());
                    out.push((names.len() - 1, path, Vec::new()));
                }
                FDT_END_NODE => {
                    open.pop();
                    names.pop();
                }
                FDT_PROP => {
                    if p + 8 > end {
                        break;
                    }
                    let len = be32(&self.blob[p..p + 4]) as usize;
                    let nameoff = be32(&self.blob[p + 4..p + 8]);
                    p += 8;
                    let data_end = (p + len).min(self.blob.len());
                    if let Some(&idx) = open.last() {
                        out[idx].2.push(Property {
                            name: self.string(nameoff),
                            value: self.blob[p..data_end].to_vec(),
                        });
                    }
                    p += (len + 3) & !3;
                }
                FDT_NOP => {}
                FDT_END => break,
                _ => break,
            }
        }
        out
    }

    /// The RAM the tree describes: `reg` of every `memory` node under the
    /// root, decoded with the root's `#address-cells`/`#size-cells`.
    pub fn memory_ranges(&self) -> Result<Vec<(u64, u64)>, String> {
        let nodes = self.nodes();
        let root = nodes.first().ok_or("empty tree")?;
        let cells = |name: &str, default: usize| {
            root.2
                .iter()
                .find(|p| p.name == name && p.value.len() == 4)
                .map_or(default, |p| be32(&p.value) as usize)
        };
        let (ac, sc) = (cells("#address-cells", 2), cells("#size-cells", 1));
        if !(1..=2).contains(&ac) || !(1..=2).contains(&sc) {
            return Err(format!(
                "unsupported #address-cells {ac} / #size-cells {sc}"
            ));
        }
        let num = |b: &[u8]| {
            b.chunks(4)
                .fold(0u64, |acc, c| (acc << 32) | u64::from(be32(c)))
        };
        let mut out = Vec::new();
        for (depth, path, props) in &nodes {
            let name = path.rsplit('/').next().unwrap_or("");
            if *depth != 1 || !(name == "memory" || name.starts_with("memory@")) {
                continue;
            }
            let Some(reg) = props.iter().find(|p| p.name == "reg") else {
                continue;
            };
            let stride = 4 * (ac + sc);
            if reg.value.is_empty() || !reg.value.len().is_multiple_of(stride) {
                return Err(format!("{path}/reg is {} bytes", reg.value.len()));
            }
            for entry in reg.value.chunks(stride) {
                out.push((num(&entry[..4 * ac]), num(&entry[4 * ac..])));
            }
        }
        if out.is_empty() {
            return Err("no /memory node with a reg".into());
        }
        Ok(out)
    }

    /// A copy of the blob with one existing property's value replaced. The
    /// value may change length, so the structure block is re-emitted and the
    /// header rewritten; the blob's layout (header, reservations, structure,
    /// strings) is required rather than assumed, and the result is exactly as
    /// long as its own `totalsize`. Only replaces: adding a property would also
    /// need a new entry in the strings block.
    pub fn with_property(&self, path: &str, name: &str, value: &[u8]) -> Result<Vec<u8>, String> {
        let h = self.header;
        let (s_off, s_len) = (h.off_dt_struct as usize, h.size_dt_struct as usize);
        let (t_off, t_len) = (h.off_dt_strings as usize, h.size_dt_strings as usize);
        if s_off < 40 || t_off < s_off + s_len {
            return Err(format!(
                "unexpected block order: struct at {s_off:#x}+{s_len:#x}, strings at {t_off:#x}"
            ));
        }
        let want: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();
        let at_path = |stack: &[String]| {
            stack.len() == want.len() + 1 && stack[1..].iter().zip(&want).all(|(a, b)| a == b)
        };

        let blob = &self.blob[s_off..s_off + s_len];
        let mut out: Vec<u8> = Vec::with_capacity(s_len + value.len() + 8);
        let mut stack: Vec<String> = Vec::new();
        let mut replaced = false;
        let mut p = 0usize;
        while p + 4 <= blob.len() {
            let token = be32(&blob[p..p + 4]);
            let start = p;
            p += 4;
            match token {
                FDT_BEGIN_NODE => {
                    let rest = &blob[p..];
                    let nlen = rest.iter().position(|&b| b == 0).unwrap_or(rest.len());
                    stack.push(String::from_utf8_lossy(&rest[..nlen]).into_owned());
                    p += (nlen + 4) & !3;
                }
                FDT_END_NODE => {
                    stack.pop();
                }
                FDT_PROP => {
                    if p + 8 > blob.len() {
                        return Err("truncated property".into());
                    }
                    let len = be32(&blob[p..p + 4]) as usize;
                    let nameoff = be32(&blob[p + 4..p + 8]);
                    p += 8 + ((len + 3) & !3);
                    if !replaced && at_path(&stack) && self.string(nameoff) == name {
                        out.extend_from_slice(&FDT_PROP.to_be_bytes());
                        out.extend_from_slice(&(value.len() as u32).to_be_bytes());
                        out.extend_from_slice(&nameoff.to_be_bytes());
                        out.extend_from_slice(value);
                        out.resize(out.len().next_multiple_of(4), 0);
                        replaced = true;
                        continue;
                    }
                }
                FDT_NOP => {}
                FDT_END => {
                    out.extend_from_slice(&blob[start..p]);
                    break;
                }
                other => return Err(format!("unknown token {other:#x} at +{start:#x}")),
            }
            out.extend_from_slice(&blob[start..p.min(blob.len())]);
        }
        if !replaced {
            return Err(format!("no property {path}/{name}"));
        }

        let new_strings = s_off + out.len();
        let total = new_strings + t_len;
        let mut b = Vec::with_capacity(total);
        b.extend_from_slice(&self.blob[..s_off]);
        b.extend_from_slice(&out);
        b.extend_from_slice(&self.blob[t_off..t_off + t_len]);
        b[4..8].copy_from_slice(&(total as u32).to_be_bytes());
        b[12..16].copy_from_slice(&(new_strings as u32).to_be_bytes());
        b[36..40].copy_from_slice(&(out.len() as u32).to_be_bytes());
        Ok(b)
    }

    /// The whole tree as source, near enough to `dtc -O dts` to diff two
    /// firmware versions by eye. Not a faithful `.dts` — values are guessed by
    /// [`Property::display`] — so use `--dump-fdt` when exactness matters.
    pub fn to_dts(&self) -> String {
        let mut s = String::from("/dts-v1/;\n\n");
        let nodes = self.nodes();
        let mut depth_of_last = 0usize;
        for (i, (depth, path, props)) in nodes.iter().enumerate() {
            while depth_of_last > *depth {
                depth_of_last -= 1;
                s.push_str(&format!("{}}};\n", "\t".repeat(depth_of_last)));
            }
            let name = if *depth == 0 {
                "/"
            } else {
                path.rsplit('/').next().unwrap_or(path)
            };
            let indent = "\t".repeat(*depth);
            s.push_str(&format!("{indent}{name} {{\n"));
            for prop in props {
                s.push_str(&format!("{indent}\t{} = {};\n", prop.name, prop.display()));
            }
            let next_depth = nodes.get(i + 1).map(|n| n.0).unwrap_or(0);
            if next_depth <= *depth {
                s.push_str(&format!("{indent}}};\n"));
                depth_of_last = *depth;
            } else {
                depth_of_last = *depth + 1;
            }
        }
        while depth_of_last > 0 {
            depth_of_last -= 1;
            s.push_str(&format!("{}}};\n", "\t".repeat(depth_of_last)));
        }
        s
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    #[test]
    fn memory_ranges_decode_reg_with_the_root_cells() {
        // The shape start4 hands over on a 1 GB Pi 4: cells 2 and 1.
        let cells = |v: &[u32]| v.iter().flat_map(|w| w.to_be_bytes()).collect::<Vec<u8>>();
        let blob = build(
            &[
                ("#address-cells", cells(&[2])),
                ("#size-cells", cells(&[1])),
            ],
            &[(
                "memory@0",
                vec![
                    ("device_type", b"memory\0".to_vec()),
                    ("reg", cells(&[0, 0, 0x3b40_0000])),
                ],
            )],
        );
        let fdt = Fdt::parse(&blob).unwrap();
        assert_eq!(fdt.memory_ranges().unwrap(), vec![(0, 0x3b40_0000)]);
        let none = build(&[], &[("chosen", vec![])]);
        assert!(Fdt::parse(&none).unwrap().memory_ranges().is_err());
    }

    #[allow(clippy::type_complexity)]
    fn build(root: &[(&str, Vec<u8>)], children: &[(&str, Vec<(&str, Vec<u8>)>)]) -> Vec<u8> {
        fn tok(s: &mut Vec<u8>, v: u32) {
            s.extend_from_slice(&v.to_be_bytes());
        }
        fn pad(s: &mut Vec<u8>) {
            s.resize(s.len().next_multiple_of(4), 0);
        }
        fn node(s: &mut Vec<u8>, name: &str) {
            tok(s, FDT_BEGIN_NODE);
            s.extend_from_slice(name.as_bytes());
            s.push(0);
            pad(s);
        }
        fn prop(s: &mut Vec<u8>, strings: &mut Vec<u8>, name: &str, v: &[u8]) {
            let off = strings.len() as u32;
            strings.extend_from_slice(name.as_bytes());
            strings.push(0);
            tok(s, FDT_PROP);
            tok(s, v.len() as u32);
            tok(s, off);
            s.extend_from_slice(v);
            pad(s);
        }
        let (mut s, mut strings) = (Vec::new(), Vec::new());
        node(&mut s, "");
        for (n, v) in root {
            prop(&mut s, &mut strings, n, v);
        }
        for (name, props) in children {
            node(&mut s, name);
            for (n, v) in props {
                prop(&mut s, &mut strings, n, v);
            }
            tok(&mut s, FDT_END_NODE);
        }
        tok(&mut s, FDT_END_NODE);
        tok(&mut s, FDT_END);
        let off_struct = 40u32;
        let off_strings = off_struct + s.len() as u32;
        let total = off_strings + strings.len() as u32;
        let mut b = Vec::new();
        for w in [
            FDT_MAGIC,
            total,
            off_struct,
            off_strings,
            0,
            17,
            16,
            0,
            strings.len() as u32,
            s.len() as u32,
        ] {
            b.extend_from_slice(&w.to_be_bytes());
        }
        b.extend_from_slice(&s);
        b.extend_from_slice(&strings);
        b
    }

    #[test]
    fn nodes_walks_the_whole_tree_not_just_one_path() {
        let blob = sample();
        let fdt = Fdt::parse(&blob).unwrap();
        let nodes = fdt.nodes();
        let paths: Vec<&str> = nodes.iter().map(|(_, p, _)| p.as_str()).collect();
        assert_eq!(paths, vec!["/", "/chosen"]);
        assert_eq!(nodes[0].0, 0);
        assert_eq!(nodes[1].0, 1);
        assert!(nodes[0].2.is_empty());
        let names: Vec<&str> = nodes[1].2.iter().map(|p| p.name.as_str()).collect();
        assert_eq!(names, vec!["bootargs", "rpi-machine-id"]);
    }

    #[test]
    fn to_dts_renders_every_node() {
        let blob = sample();
        let dts = Fdt::parse(&blob).unwrap().to_dts();
        assert!(dts.starts_with("/dts-v1/;"), "{dts}");
        assert!(dts.contains("chosen {"), "{dts}");
        assert!(dts.contains("bootargs = \"hi\";"), "{dts}");
        assert_eq!(dts.matches('{').count(), dts.matches("};").count());
    }

    pub(crate) fn sample() -> Vec<u8> {
        let strings = b"bootargs\0rpi-machine-id\0".to_vec();
        let mut s: Vec<u8> = Vec::new();
        fn tok(s: &mut Vec<u8>, v: u32) {
            s.extend_from_slice(&v.to_be_bytes());
        }
        tok(&mut s, FDT_BEGIN_NODE);
        s.extend_from_slice(b"\0\0\0\0"); // root name "" padded
        tok(&mut s, FDT_BEGIN_NODE);
        s.extend_from_slice(b"chosen\0\0");
        tok(&mut s, FDT_PROP);
        tok(&mut s, 3);
        tok(&mut s, 0);
        s.extend_from_slice(b"hi\0\0");
        tok(&mut s, FDT_PROP);
        tok(&mut s, 3);
        tok(&mut s, 9);
        s.extend_from_slice(b"ab\n\0");
        tok(&mut s, FDT_END_NODE);
        tok(&mut s, FDT_END_NODE);
        tok(&mut s, FDT_END);

        let off_struct = 40u32;
        let off_strings = off_struct + s.len() as u32;
        let total = off_strings + strings.len() as u32;
        let mut b: Vec<u8> = Vec::new();
        for w in [
            FDT_MAGIC,
            total,
            off_struct,
            off_strings,
            0,
            17,
            16,
            0,
            strings.len() as u32,
            s.len() as u32,
        ] {
            b.extend_from_slice(&w.to_be_bytes());
        }
        b.extend_from_slice(&s);
        b.extend_from_slice(&strings);
        b
    }

    #[test]
    fn reads_chosen_properties() {
        let blob = sample();
        let fdt = Fdt::parse(&blob).expect("parses");
        assert_eq!(fdt.header().totalsize as usize, blob.len());
        let props = fdt.properties_of("/chosen").expect("has /chosen");
        assert_eq!(props.len(), 2);
        assert_eq!(props[0].name, "bootargs");
        assert_eq!(props[0].as_str().as_deref(), Some("hi"));
        assert_eq!(props[1].name, "rpi-machine-id");
        assert_eq!(props[1].as_str().as_deref(), Some("ab"));
    }

    #[test]
    fn with_property_grows_a_value_and_keeps_the_rest() {
        let blob = sample();
        let fdt = Fdt::parse(&blob).unwrap();
        let patched = fdt
            .with_property("/chosen", "bootargs", b"earlycon hi\0")
            .unwrap();
        let back = Fdt::parse(&patched).expect("still a valid blob");
        assert_eq!(back.header().totalsize as usize, patched.len());
        let props = back.properties_of("/chosen").unwrap();
        assert_eq!(props[0].as_str().as_deref(), Some("earlycon hi"));
        assert_eq!(props[1].name, "rpi-machine-id");
        assert_eq!(props[1].as_str().as_deref(), Some("ab"));
        assert!(fdt.with_property("/chosen", "nope", b"x").is_err());
        assert!(fdt.with_property("/nope", "bootargs", b"x").is_err());
    }

    #[test]
    fn absent_node_is_none() {
        let blob = sample();
        let fdt = Fdt::parse(&blob).unwrap();
        assert!(fdt.properties_of("/nope").is_none());
    }

    #[test]
    fn rejects_a_non_fdt() {
        let junk = vec![0u8; 64];
        assert!(Fdt::parse(&junk).is_err());
    }
}
