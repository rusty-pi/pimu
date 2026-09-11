//! A read-only flattened-device-tree reader, just enough to look inside the
//! blob `start4.elf` hands to the ARM.
//!
//! The point of the whole bench is [rpi-mkosi#37]: a firmware bump must not
//! silently change `/chosen/rpi-machine-id`, because that string feeds the root
//! LUKS passphrase. To diff two firmware versions we have to get the *patched*
//! device tree back out of the model, so this module parses the blob well
//! enough to print the `/chosen` properties and to check the header before the
//! bytes are written to a file.
//!
//! This is deliberately not a general DTB library: no phandle resolution, no
//! memory-reservation walk, no writing. Spec: Devicetree Specification v0.4,
//! section 5 ("Flattened Devicetree Format").
//!
//! [rpi-mkosi#37]: https://github.com/valtzu/rpi-mkosi/issues/37

pub const FDT_MAGIC: u32 = 0xd00d_feed;

const FDT_BEGIN_NODE: u32 = 1;
const FDT_END_NODE: u32 = 2;
const FDT_PROP: u32 = 3;
const FDT_NOP: u32 = 4;
const FDT_END: u32 = 9;

/// The fixed-size header at the start of every `.dtb`.
#[derive(Debug, Clone, Copy)]
pub struct Header {
    pub totalsize: u32,
    pub off_dt_struct: u32,
    pub off_dt_strings: u32,
    pub size_dt_struct: u32,
    pub size_dt_strings: u32,
    pub version: u32,
}

/// One property, as found by [`Fdt::properties_of`].
#[derive(Debug, Clone)]
pub struct Property {
    pub name: String,
    pub value: Vec<u8>,
}

impl Property {
    /// The value rendered the way `fdtdump` would: a string if it looks like
    /// one, a list of big-endian words if the length is a multiple of four,
    /// raw hex otherwise.
    pub fn display(&self) -> String {
        if let Some(s) = self.as_str() {
            return format!("{s:?}");
        }
        if !self.value.is_empty() && self.value.len() % 4 == 0 && self.value.len() <= 16 {
            let words: Vec<String> = self
                .value
                .chunks_exact(4)
                .map(|c| format!("{:#010x}", be32(c)))
                .collect();
            return format!("<{}>", words.join(" "));
        }
        self.value.iter().map(|b| format!("{b:02x}")).collect()
    }

    /// The value as text, if every byte is printable (a trailing NUL or LF,
    /// which `start4` uses for both `rpi-serial64` and `rpi-machine-id`, is
    /// accepted and trimmed).
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

/// A parsed (borrowed) device tree blob.
pub struct Fdt<'a> {
    blob: &'a [u8],
    header: Header,
}

fn be32(b: &[u8]) -> u32 {
    u32::from_be_bytes([b[0], b[1], b[2], b[3]])
}

impl<'a> Fdt<'a> {
    /// Validate the header and wrap the blob.
    ///
    /// Errors carry enough detail to tell "this is not a device tree at all"
    /// apart from "the blob is truncated", because both are plausible when the
    /// address came out of a firmware log line.
    pub fn parse(blob: &'a [u8]) -> Result<Fdt<'a>, String> {
        if blob.len() < 40 {
            return Err(format!("too short for an FDT header ({} bytes)", blob.len()));
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

    /// The blob trimmed to the length its own header claims — what a `.dtb`
    /// file should contain.
    pub fn bytes(&self) -> &'a [u8] {
        &self.blob[..self.header.totalsize as usize]
    }

    fn string(&self, off: u32) -> String {
        let start = self.header.off_dt_strings as usize + off as usize;
        let rest = &self.blob[start.min(self.blob.len())..];
        let end = rest.iter().position(|&b| b == 0).unwrap_or(rest.len());
        String::from_utf8_lossy(&rest[..end]).into_owned()
    }

    /// Every property of one node, addressed by its full path (`/chosen`).
    /// An absent node gives `None`, which is different from a node with no
    /// properties.
    pub fn properties_of(&self, path: &str) -> Option<Vec<Property>> {
        let want: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();
        // The node names on the current branch. The root's own name is empty,
        // so `stack[1..] == want` means the tokens we are reading belong to the
        // node that was asked for.
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
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a tiny blob: `/ { chosen { bootargs = "hi"; rpi-machine-id = "ab\n"; } }`.
    fn sample() -> Vec<u8> {
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
        // The trailing LF start4 appends is trimmed for display.
        assert_eq!(props[1].as_str().as_deref(), Some("ab"));
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
