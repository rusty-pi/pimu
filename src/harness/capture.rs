//! Turn captured UART bytes into a stable, diffable transcript.

/// Normalise a raw console byte stream into text suitable for golden comparison:
///
/// - `\r\n` and lone `\r` become `\n`
/// - printable ASCII and `\n`/`\t` pass through
/// - any other byte becomes `\xNN`
/// - a trailing newline is ensured (so goldens are POSIX text files)
pub fn transcript(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() + 1);
    let mut i = 0;
    while i < bytes.len() {
        let b = bytes[i];
        match b {
            b'\r' => {
                if bytes.get(i + 1) == Some(&b'\n') {
                    i += 1;
                }
                s.push('\n');
            }
            b'\n' | b'\t' => s.push(b as char),
            0x20..=0x7E => s.push(b as char),
            _ => s.push_str(&format!("\\x{b:02x}")),
        }
        i += 1;
    }
    if !s.ends_with('\n') {
        s.push('\n');
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crlf_and_nonprintable() {
        assert_eq!(transcript(b"a\r\nb\rc"), "a\nb\nc\n");
        assert_eq!(transcript(&[0x1b, b'[', b'0', b'm']), "\\x1b[0m\n");
        assert_eq!(transcript(b"done\n"), "done\n");
    }
}
