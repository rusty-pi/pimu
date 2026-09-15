//! The `io` channel (#35): what crossed the peripherals, apart from the
//! serial console.
//!
//! * block reads and writes on the SD card and the USB stick, contiguous runs
//!   merged, with the files they belong to ([`super::fatmap`])
//! * OTP rows the firmware read or programmed, with their values
//! * what the network peer did: DHCP, DNS, TFTP and HTTP
//!
//! Everything is captured where the data crosses a peripheral – the firmware
//! is a black box here as everywhere else.

use std::collections::HashMap;

use super::fatmap::{self, FileMap};
use super::{fields, quote, Event};

/// One merged run of block transfers, and the model time its first block
/// went at: the time its line carries.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Run {
    pub(super) us: u64,
    dev: &'static str,
    op: &'static str,
    lba: u64,
    count: u64,
}

/// What the channel keeps between events.
#[derive(Default)]
pub(super) struct Io {
    /// The file map of each block device, by name.
    files: HashMap<&'static str, FileMap>,
    /// The run being merged. It goes out when anything else is logged.
    pending: Option<Run>,
}

impl Io {
    /// Name the files on block device `dev`, once.
    pub(super) fn map_files(&mut self, dev: &'static str, read: &fatmap::ReadBlock) {
        self.files
            .entry(dev)
            .or_insert_with(|| FileMap::build(read));
    }

    /// `count` blocks from `lba` read (or written, erased) on `dev` at model
    /// time `us`. Joins the pending run when it carries on from it; otherwise
    /// the new run takes its place, and the one it ended comes back to be
    /// written out.
    pub(super) fn blocks(
        &mut self,
        us: u64,
        dev: &'static str,
        op: &'static str,
        lba: u64,
        count: u64,
    ) -> Option<Run> {
        if let Some(run) = &mut self.pending {
            if run.dev == dev && run.op == op && run.lba + run.count == lba {
                run.count += count;
                return None;
            }
        }
        self.pending.replace(Run {
            us,
            dev,
            op,
            lba,
            count,
        })
    }

    /// End the run being merged.
    pub(super) fn take_run(&mut self) -> Option<Run> {
        self.pending.take()
    }

    /// `run`, with the files its blocks belong to.
    pub(super) fn run_event(&self, run: &Run) -> Event {
        let names = self
            .files
            .get(run.dev)
            .map(|m| m.names(run.lba, run.count))
            .unwrap_or_default();
        let mut text = format!(
            "{:<4} {:<5} lba {:#x}+{}",
            run.dev, run.op, run.lba, run.count
        );
        if !names.is_empty() {
            text.push_str("  ");
            let shown: Vec<&str> = names.iter().take(4).copied().collect();
            text.push_str(&shown.join(", "));
            if names.len() > 4 {
                text.push_str(&format!(", ... ({} more)", names.len() - 4));
            }
        }
        let files: Vec<String> = names.iter().map(|n| quote(n)).collect();
        let json = format!(
            "\"dev\":{},\"op\":{},\"lba\":{},\"blocks\":{},\"files\":[{}]",
            quote(run.dev),
            quote(run.op),
            run.lba,
            run.count,
            files.join(",")
        );
        Event { text, json }
    }
}

/// An OTP row the firmware read. `fused`: the row is programmed on the
/// modelled board; a blank one reads 0, as on the hardware.
pub(super) fn otp_read(row: u32, value: u32, fused: bool) -> Event {
    let value = format!("{value:#010x}");
    Event {
        text: format!(
            "otp  read  row {row:<3} = {value}{}",
            if fused { "" } else { "  (blank)" }
        ),
        json: fields(&[
            ("dev", "otp"),
            ("op", "read"),
            ("row", &row.to_string()),
            ("value", &value),
        ]),
    }
}

/// An OTP row the firmware programmed: `value` is what the row holds now,
/// `was` what it held before.
pub(super) fn otp_write(row: u32, value: u32, was: u32) -> Event {
    let (value, was) = (format!("{value:#010x}"), format!("{was:#010x}"));
    Event {
        text: format!("otp  write row {row:<3} = {value}  (was {was})"),
        json: fields(&[
            ("dev", "otp"),
            ("op", "write"),
            ("row", &row.to_string()),
            ("value", &value),
            ("was", &was),
        ]),
    }
}

/// Something the network peer did, as the peer words it.
pub(super) fn net(what: &str) -> Event {
    Event {
        text: format!("net  {what}"),
        json: fields(&[("dev", "net"), ("event", what)]),
    }
}
