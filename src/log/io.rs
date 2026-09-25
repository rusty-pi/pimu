//! The `io` channel: what crossed the peripherals, apart from the
//! serial console.
//!
//! Block runs on the SD card and USB stick with the files they belong to
//! ([`super::fatmap`]), OTP rows read and programmed, and what the network peer
//! did — all captured where the data crosses a peripheral, so the firmware
//! stays a black box.

use std::collections::HashMap;

use super::fatmap::{self, FileMap};
use super::{fields, quote, Event};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Run {
    pub(super) us: u64,
    dev: &'static str,
    op: &'static str,
    lba: u64,
    count: u64,
}

#[derive(Default)]
pub(super) struct Io {
    files: HashMap<&'static str, FileMap>,
    pending: Option<Run>,
}

impl Io {
    pub(super) fn map_files(&mut self, dev: &'static str, read: &fatmap::ReadBlock) {
        self.files
            .entry(dev)
            .or_insert_with(|| FileMap::build(read));
    }

    /// `count` blocks from `lba` on `dev`: joins the pending run when it
    /// carries on from it, else replaces it and hands the old one back.
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

    pub(super) fn take_run(&mut self) -> Option<Run> {
        self.pending.take()
    }

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

/// An OTP row the firmware read; an unfused row reads 0, as on the hardware.
pub(super) fn otp_read(row: u32, value: u32, fused: bool, meaning: &str) -> Event {
    let value = format!("{value:#010x}");
    Event {
        text: format!(
            "otp  read  row {row:<3} = {value}  {meaning}{}",
            if fused { "" } else { " (blank)" }
        ),
        json: fields(&[
            ("dev", "otp"),
            ("op", "read"),
            ("row", &row.to_string()),
            ("value", &value),
            ("meaning", meaning),
        ]),
    }
}

/// An OTP row the firmware programmed, before and after.
pub(super) fn otp_write(row: u32, value: u32, was: u32, meaning: &str) -> Event {
    let (value, was) = (format!("{value:#010x}"), format!("{was:#010x}"));
    Event {
        text: format!("otp  write row {row:<3} = {value}  {meaning} (was {was})"),
        json: fields(&[
            ("dev", "otp"),
            ("op", "write"),
            ("row", &row.to_string()),
            ("value", &value),
            ("was", &was),
            ("meaning", meaning),
        ]),
    }
}

pub(super) fn net(what: &str) -> Event {
    Event {
        text: format!("net  {what}"),
        json: fields(&[("dev", "net"), ("event", what)]),
    }
}
