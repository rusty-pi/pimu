//! The I/O log: what the machine read and wrote, seen from the peripherals
//! (#35).
//!
//! `boot --io-log <path>` records, apart from the serial console:
//!
//! * block reads and writes on the SD card and the USB stick, contiguous runs
//!   merged, with the files they belong to ([`crate::fatmap`])
//! * OTP rows the firmware read, with their values
//! * what the network peer did: DHCP, DNS, TFTP and HTTP
//!
//! Everything is captured where the data crosses a peripheral – the firmware
//! is a black box here as everywhere else. Values that would be secret on a
//! real board are printed as they are: every one of them is invented for the
//! model (`src/periph/configotp.rs`).
//!
//! Two formats: `text`, one line per event for reading, and `jsonl`, one JSON
//! object per line for tools. Lines are written as they happen, so the log can
//! be followed while the run goes on.

use std::cell::RefCell;
use std::collections::HashMap;
use std::io::Write;
use std::rc::Rc;

use crate::fatmap::FileMap;

/// A shared handle: the log is written to from several peripherals.
pub type IoLogRef = Rc<RefCell<IoLog>>;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Format {
    Text,
    Jsonl,
}

impl std::str::FromStr for Format {
    type Err = String;

    fn from_str(s: &str) -> Result<Format, String> {
        match s {
            "text" => Ok(Format::Text),
            "jsonl" | "json" => Ok(Format::Jsonl),
            other => Err(format!("unknown I/O log format '{other}' (text or jsonl)")),
        }
    }
}

/// One merged run of block transfers.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Run {
    dev: &'static str,
    op: &'static str,
    lba: u64,
    count: u64,
}

pub struct IoLog {
    out: Box<dyn Write>,
    format: Format,
    /// The file map of each block device, by name.
    files: HashMap<&'static str, FileMap>,
    pending: Option<Run>,
}

impl IoLog {
    pub fn new(out: Box<dyn Write>, format: Format) -> IoLog {
        IoLog {
            out,
            format,
            files: HashMap::new(),
            pending: None,
        }
    }

    pub fn shared(self) -> IoLogRef {
        Rc::new(RefCell::new(self))
    }

    /// Name the files on block device `dev`, once.
    pub fn map_files(&mut self, dev: &'static str, read: &crate::fatmap::ReadBlock) {
        self.files
            .entry(dev)
            .or_insert_with(|| FileMap::build(read));
    }

    /// `count` blocks from `lba` read (or written, erased) on `dev`. Joins the
    /// previous run when it carries on from it.
    pub fn blocks(&mut self, dev: &'static str, op: &'static str, lba: u64, count: u64) {
        if let Some(run) = &mut self.pending {
            if run.dev == dev && run.op == op && run.lba + run.count == lba {
                run.count += count;
                return;
            }
        }
        self.flush_run();
        self.pending = Some(Run {
            dev,
            op,
            lba,
            count,
        });
    }

    /// An OTP row the firmware read. `fused`: the row is programmed on the
    /// modelled board; a blank one reads 0, as on the hardware.
    pub fn otp_read(&mut self, row: u32, value: u32, fused: bool) {
        self.flush_run();
        let (fmt, row_s, value_s) = (self.format, row.to_string(), format!("{value:#010x}"));
        let line = match fmt {
            Format::Text => format!(
                "otp  read  row {row:<3} = {value:#010x}{}",
                if fused { "" } else { "  (blank)" }
            ),
            Format::Jsonl => json(&[
                ("dev", "otp"),
                ("op", "read"),
                ("row", &row_s),
                ("value", &value_s),
            ]),
        };
        self.line(&line);
    }

    /// Something the network peer did, as the peer words it.
    pub fn net(&mut self, what: &str) {
        self.flush_run();
        let line = match self.format {
            Format::Text => format!("net  {what}"),
            Format::Jsonl => json(&[("dev", "net"), ("event", what)]),
        };
        self.line(&line);
    }

    /// Write out the run being merged.
    pub fn flush_run(&mut self) {
        let Some(run) = self.pending.take() else {
            return;
        };
        let names = self
            .files
            .get(run.dev)
            .map(|m| m.names(run.lba, run.count))
            .unwrap_or_default();
        let line = match self.format {
            Format::Text => {
                let mut l = format!(
                    "{:<4} {:<5} lba {:#x}+{}",
                    run.dev, run.op, run.lba, run.count
                );
                if !names.is_empty() {
                    l.push_str("  ");
                    let shown: Vec<&str> = names.iter().take(4).copied().collect();
                    l.push_str(&shown.join(", "));
                    if names.len() > 4 {
                        l.push_str(&format!(", ... ({} more)", names.len() - 4));
                    }
                }
                l
            }
            Format::Jsonl => {
                let files: Vec<String> = names.iter().map(|n| quote(n)).collect();
                format!(
                    "{{\"dev\":{},\"op\":{},\"lba\":{},\"blocks\":{},\"files\":[{}]}}",
                    quote(run.dev),
                    quote(run.op),
                    run.lba,
                    run.count,
                    files.join(",")
                )
            }
        };
        self.line(&line);
    }

    fn line(&mut self, line: &str) {
        let _ = writeln!(self.out, "{line}");
        let _ = self.out.flush();
    }
}

impl Drop for IoLog {
    fn drop(&mut self) {
        self.flush_run();
    }
}

/// A JSON object of string fields.
fn json(fields: &[(&str, &str)]) -> String {
    let body: Vec<String> = fields
        .iter()
        .map(|(k, v)| format!("{}:{}", quote(k), quote(v)))
        .collect();
    format!("{{{}}}", body.join(","))
}

fn quote(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A writer the test can read back.
    #[derive(Clone, Default)]
    struct Buf(Rc<RefCell<Vec<u8>>>);

    impl Write for Buf {
        fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
            self.0.borrow_mut().extend_from_slice(b);
            Ok(b.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    fn text(buf: &Buf) -> String {
        String::from_utf8(buf.0.borrow().clone()).unwrap()
    }

    #[test]
    fn contiguous_block_runs_merge_and_other_events_split_them() {
        let buf = Buf::default();
        let mut log = IoLog::new(Box::new(buf.clone()), Format::Text);
        log.blocks("sd", "read", 0x800, 1);
        log.blocks("sd", "read", 0x801, 7);
        log.otp_read(19, 0x8aa9_6d38, true);
        log.blocks("sd", "read", 0x808, 1);
        log.blocks("sd", "write", 0x809, 1);
        drop(log);
        assert_eq!(
            text(&buf),
            "sd   read  lba 0x800+8\n\
             otp  read  row 19  = 0x8aa96d38\n\
             sd   read  lba 0x808+1\n\
             sd   write lba 0x809+1\n"
        );
    }

    #[test]
    fn jsonl_lines_are_objects() {
        let buf = Buf::default();
        let mut log = IoLog::new(Box::new(buf.clone()), Format::Jsonl);
        log.net("tftp: RRQ \"start4.elf\"");
        log.blocks("usb", "read", 2, 3);
        drop(log);
        assert_eq!(
            text(&buf),
            "{\"dev\":\"net\",\"event\":\"tftp: RRQ \\\"start4.elf\\\"\"}\n\
             {\"dev\":\"usb\",\"op\":\"read\",\"lba\":2,\"blocks\":3,\"files\":[]}\n"
        );
    }
}
