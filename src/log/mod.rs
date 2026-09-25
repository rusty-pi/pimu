//! What the machine did, by channel.
//!
//! `boot --log [text:|jsonl:]<channel>[,<channel>...]` turns channels on and
//! `--log-file <path>` sends them somewhere other than stderr. The machine is
//! handed one [`Log`] and clones it into each device that logs; a clone carries
//! the set of channels that are on, so asking is a bit test and [`crate::log!`]
//! formats a line only when it is. The channels share one output, so lines come
//! out in the order things happened, stamped with model time from the system
//! timer ([`Log::set_time`]).
//!
//! Values that would be secret on a real board are printed as they are: every
//! one is invented for the model (`src/periph/configotp.rs`). The channels
//! [`Channel::needs_diag`] names exist only in a `diag` build.

use std::cell::{Cell, RefCell};
use std::fmt;
use std::io::Write;
use std::rc::Rc;

pub mod fatmap;
mod io;

/// One line on a channel, formatted only when the channel is on.
#[macro_export]
macro_rules! log {
    ($log:expr, $channel:expr, $($arg:tt)+) => {{
        let log: &$crate::log::Log = &$log;
        let channel: $crate::log::Channel = $channel;
        if log.on(channel) {
            log.write(channel, format_args!($($arg)+));
        }
    }};
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Channel {
    /// Block runs with their files, OTP rows, and what the network peer did.
    Io,
    /// Synchronous ARM exceptions, with the `ESR`/`FAR` the handler sees.
    ArmExc,
    /// Misaligned scalar VPU accesses (`--check-alignment`).
    Alignment,
    /// Every system-timer compare arm.
    Cmp,
    /// Intervals `--jitter` has stretched, and by how much.
    Jitter,
    /// Reads that would be stale bytes on silicon (`--check-coherency`).
    Coherency,
    /// DWC2: every write, and every read that changed, so a poll shows once.
    Dwc2,
    /// EMMC2 and the legacy EMMC: commands, blocks and register accesses.
    Emmc,
    /// The FXL6408 GPIO expander's register traffic.
    Expander,
    /// Every pin the firmware changes, named the way the board wires it.
    Gpio,
    /// Interrupt enables, decoded back into the calls that wrote them.
    IrqEn,
    /// Every word across the ARM-VideoCore property mailbox, both directions.
    Mbox,
    /// The console UARTs: register writes, interrupt line, and the pins that
    /// pick which one the header carries.
    Uart,
    /// Every OTP row read and programmed, and the commands nothing models.
    Otp,
    /// The VL805's interrupt, `RC_BAR2` writes, and endpoint DMA outside it.
    Pcie,
    /// PMIC register traffic.
    Pmic,
    /// SPI0 transactions against the EEPROM flash.
    Spi,
    /// xHCI rings, TRBs and port state.
    Xhci,
    /// Execution leaving start4's code range, with the registers.
    Derail,
    /// Every DMA control block executed, and the legacy controller window.
    Dma,
    /// Every jump of the system timer through a firmware busy-wait.
    Ff,
    /// At exit, the firmware's handler table beside its vector table.
    IrqTbl,
    /// `sleep` instructions, and what woke the core.
    Sleep,
    /// Interrupts the firmware posts in software through CoreCtl.
    SwIrq,
    /// Tick deliveries and skips, and an `rti` outside start4's code.
    Tick,
    /// Interrupt vectoring: slot, vector base, handler.
    Vec,
}

impl Channel {
    pub const ALL: [Channel; 26] = [
        Channel::Io,
        Channel::ArmExc,
        Channel::Alignment,
        Channel::Cmp,
        Channel::Jitter,
        Channel::Coherency,
        Channel::Dwc2,
        Channel::Emmc,
        Channel::Expander,
        Channel::Gpio,
        Channel::IrqEn,
        Channel::Mbox,
        Channel::Uart,
        Channel::Otp,
        Channel::Pcie,
        Channel::Pmic,
        Channel::Spi,
        Channel::Xhci,
        Channel::Derail,
        Channel::Dma,
        Channel::Ff,
        Channel::IrqTbl,
        Channel::Sleep,
        Channel::SwIrq,
        Channel::Tick,
        Channel::Vec,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Channel::Io => "io",
            Channel::ArmExc => "arm-exc",
            Channel::Alignment => "alignment",
            Channel::Cmp => "cmp",
            Channel::Jitter => "jitter",
            Channel::Coherency => "coherency",
            Channel::Dwc2 => "dwc2",
            Channel::Emmc => "emmc",
            Channel::Expander => "expander",
            Channel::Gpio => "gpio",
            Channel::IrqEn => "irqen",
            Channel::Mbox => "mbox",
            Channel::Uart => "uart",
            Channel::Otp => "otp",
            Channel::Pcie => "pcie",
            Channel::Pmic => "pmic",
            Channel::Spi => "spi",
            Channel::Xhci => "xhci",
            Channel::Derail => "derail",
            Channel::Dma => "dma",
            Channel::Ff => "ff",
            Channel::IrqTbl => "irqtbl",
            Channel::Sleep => "sleep",
            Channel::SwIrq => "swirq",
            Channel::Tick => "tick",
            Channel::Vec => "vec",
        }
    }

    /// Only a `diag` build has it: a per-step check, or start4's layout.
    pub fn needs_diag(self) -> bool {
        matches!(
            self,
            Channel::Derail
                | Channel::Dma
                | Channel::Ff
                | Channel::IrqTbl
                | Channel::Sleep
                | Channel::SwIrq
                | Channel::Tick
                | Channel::Vec
        )
    }

    fn bit(self) -> u32 {
        1 << self as u32
    }
}

impl std::str::FromStr for Channel {
    type Err = String;

    fn from_str(s: &str) -> Result<Channel, String> {
        Channel::ALL
            .into_iter()
            .find(|c| c.name() == s)
            .ok_or_else(|| {
                let names: Vec<&str> = Channel::ALL.into_iter().map(Channel::name).collect();
                format!("unknown log channel `{s}` (one of {})", names.join(", "))
            })
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Format {
    #[default]
    Text,
    Jsonl,
}

impl std::str::FromStr for Format {
    type Err = String;

    fn from_str(s: &str) -> Result<Format, String> {
        match s {
            "text" => Ok(Format::Text),
            "jsonl" | "json" => Ok(Format::Jsonl),
            other => Err(format!("unknown log format `{other}` (text or jsonl)")),
        }
    }
}

impl fmt::Display for Format {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Format::Text => "text",
            Format::Jsonl => "jsonl",
        })
    }
}

/// What `--log` asked for: the channels, and the format if it named one.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Spec {
    format: Option<Format>,
    mask: u32,
}

impl Spec {
    pub fn parse(s: &str) -> Result<Spec, String> {
        let (format, names) = match s.split_once(':') {
            Some((format, names)) => (Some(format.parse::<Format>()?), names),
            None => (None, s),
        };
        let mut mask = 0;
        for name in names.split(',').map(str::trim) {
            let channel: Channel = name.parse()?;
            if channel.needs_diag() && !crate::diag::ON {
                return Err(format!(
                    "log channel `{name}` needs a build with the `diag` feature: \
                     cargo build --release --features diag"
                ));
            }
            mask |= channel.bit();
        }
        Ok(Spec { format, mask })
    }

    /// A repeated `--log`: the channels add up, and the formats have to agree.
    pub fn add(&mut self, other: Spec) -> Result<(), String> {
        match (self.format, other.format) {
            (Some(a), Some(b)) if a != b => return Err(format!("--log asks for both {a} and {b}")),
            (None, format) => self.format = format,
            _ => {}
        }
        self.mask |= other.mask;
        Ok(())
    }

    pub fn is_empty(&self) -> bool {
        self.mask == 0
    }

    pub fn format(&self) -> Format {
        self.format.unwrap_or_default()
    }
}

/// Where a machine's channels go; every device that logs holds a clone.
#[derive(Clone, Default)]
pub struct Log {
    /// The channels that are on, a bit each, copied into every clone.
    mask: u32,
    shared: Option<Rc<Shared>>,
}

struct Shared {
    now_us: Cell<u64>,
    sink: RefCell<Sink>,
}

struct Event {
    text: String,
    json: String,
}

impl fmt::Debug for Log {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let on: Vec<&str> = Channel::ALL
            .into_iter()
            .filter(|&c| self.on(c))
            .map(Channel::name)
            .collect();
        f.debug_tuple("Log").field(&on).finish()
    }
}

impl Log {
    pub fn new(spec: Spec, out: Box<dyn Write>) -> Log {
        if spec.is_empty() {
            return Log::default();
        }
        let sink = Sink {
            out,
            format: spec.format(),
            io: io::Io::default(),
        };
        Log {
            mask: spec.mask,
            shared: Some(Rc::new(Shared {
                now_us: Cell::new(0),
                sink: RefCell::new(sink),
            })),
        }
    }

    #[inline]
    pub fn on(&self, channel: Channel) -> bool {
        self.mask & channel.bit() != 0
    }

    /// Model time for the lines from here on, as the system timer moves.
    #[inline]
    pub fn set_time(&self, us: u64) {
        if let Some(shared) = &self.shared {
            shared.now_us.set(us);
        }
    }

    /// One line on `channel`, if it is on; go through [`crate::log!`].
    pub fn write(&self, channel: Channel, args: fmt::Arguments<'_>) {
        if let Some(shared) = self.shared_for(channel) {
            let msg = args.to_string();
            let event = Event {
                json: fields(&[("msg", &msg)]),
                text: msg,
            };
            shared
                .sink
                .borrow_mut()
                .line(channel, shared.now_us.get(), &event);
        }
    }

    pub fn map_files(&self, dev: &'static str, read: &fatmap::ReadBlock) {
        if let Some(shared) = self.shared_for(Channel::Io) {
            shared.sink.borrow_mut().io.map_files(dev, read);
        }
    }

    /// `io`: blocks on `dev`; contiguous runs become one line, stamped with
    /// the time of their first block.
    pub fn blocks(&self, dev: &'static str, op: &'static str, lba: u64, count: u64) {
        if let Some(shared) = self.shared_for(Channel::Io) {
            shared
                .sink
                .borrow_mut()
                .blocks(shared.now_us.get(), dev, op, lba, count);
        }
    }

    /// `io`: an OTP row read; an unfused row reads 0, as on the hardware.
    pub fn otp_read(&self, row: u32, value: u32, fused: bool, meaning: &str) {
        if self.on(Channel::Io) {
            self.io_line(io::otp_read(row, value, fused, meaning));
        }
    }

    /// `io`: a row programmed, before and after; fuses only go 0 to 1.
    pub fn otp_write(&self, row: u32, value: u32, was: u32, meaning: &str) {
        if self.on(Channel::Io) {
            self.io_line(io::otp_write(row, value, was, meaning));
        }
    }

    /// `io`: something the network peer did, as the peer words it.
    pub fn net(&self, what: &str) {
        if self.on(Channel::Io) {
            self.io_line(io::net(what));
        }
    }

    pub fn flush(&self) {
        if let Some(shared) = &self.shared {
            shared.sink.borrow_mut().flush_run();
        }
    }

    fn io_line(&self, event: Event) {
        if let Some(shared) = &self.shared {
            shared
                .sink
                .borrow_mut()
                .line(Channel::Io, shared.now_us.get(), &event);
        }
    }

    fn shared_for(&self, channel: Channel) -> Option<&Rc<Shared>> {
        self.shared.as_ref().filter(|_| self.on(channel))
    }
}

struct Sink {
    out: Box<dyn Write>,
    format: Format,
    io: io::Io,
}

impl Sink {
    fn blocks(&mut self, us: u64, dev: &'static str, op: &'static str, lba: u64, count: u64) {
        if let Some(run) = self.io.blocks(us, dev, op, lba, count) {
            self.write_run(&run);
        }
    }

    /// One line, after the block run being merged, so lines stay in order.
    fn line(&mut self, channel: Channel, us: u64, event: &Event) {
        self.flush_run();
        self.write(channel, us, event);
    }

    fn flush_run(&mut self) {
        if let Some(run) = self.io.take_run() {
            self.write_run(&run);
        }
    }

    fn write_run(&mut self, run: &io::Run) {
        let event = self.io.run_event(run);
        self.write(Channel::Io, run.us, &event);
    }

    fn write(&mut self, channel: Channel, us: u64, event: &Event) {
        let _ = match self.format {
            Format::Text => writeln!(
                self.out,
                "{:4}.{:06} {}: {}",
                us / 1_000_000,
                us % 1_000_000,
                channel.name(),
                event.text
            ),
            Format::Jsonl => writeln!(
                self.out,
                "{{\"us\":{us},\"channel\":{},{}}}",
                quote(channel.name()),
                event.json
            ),
        };
        let _ = self.out.flush();
    }
}

impl Drop for Sink {
    fn drop(&mut self) {
        self.flush_run();
    }
}

/// Warn about a replaced environment variable that is still set: an old recipe
/// would otherwise do nothing, silently.
pub fn warn_replaced_env() {
    for (key, _) in std::env::vars_os() {
        let Some(key) = key.to_str() else { continue };
        let now = match key {
            "EMMC_DBG" => "--log emmc".to_string(),
            "PIMU_DBG_TCB" | "RVF_DBG_TCB" => "PIMU_TCB".to_string(),
            _ => match key
                .strip_prefix("PIMU_DBG_")
                .or_else(|| key.strip_prefix("RVF_DBG_"))
            {
                Some(name) => format!("--log {}", name.to_lowercase().replace('_', "-")),
                // Every other `RVF_*` switch kept its name and changed prefix.
                None => match key.strip_prefix("RVF_") {
                    Some(rest) => format!("PIMU_{rest}"),
                    None => continue,
                },
            },
        };
        eprintln!("warning: {key} is gone, use {now}");
    }
}

/// String fields for a `jsonl` line: `"key":"value"` pairs, without the braces.
fn fields(pairs: &[(&str, &str)]) -> String {
    let body: Vec<String> = pairs
        .iter()
        .map(|(k, v)| format!("{}:{}", quote(k), quote(v)))
        .collect();
    body.join(",")
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

    fn open(spec: &str) -> (Log, Buf) {
        let buf = Buf::default();
        let log = Log::new(Spec::parse(spec).unwrap(), Box::new(buf.clone()));
        (log, buf)
    }

    #[test]
    fn a_spec_is_channels_after_an_optional_format() {
        let spec = Spec::parse("jsonl:io,pcie").unwrap();
        assert_eq!(spec.format(), Format::Jsonl);
        let log = Log::new(spec, Box::new(std::io::sink()));
        assert!(log.on(Channel::Io) && log.on(Channel::Pcie) && !log.on(Channel::Spi));
        assert_eq!(Spec::parse("spi").unwrap().format(), Format::Text);
        assert_eq!(Spec::parse("json:spi").unwrap().format(), Format::Jsonl);

        assert!(Spec::parse("xml:io").unwrap_err().contains("text or jsonl"));
        // An unknown name lists the ones there are.
        assert!(Spec::parse("pci").unwrap_err().contains("pcie"));
        assert!(Spec::parse("jsonl:").is_err());
    }

    #[test]
    fn repeated_specs_add_up_and_their_formats_have_to_agree() {
        let mut spec = Spec::parse("pcie").unwrap();
        spec.add(Spec::parse("jsonl:io").unwrap()).unwrap();
        assert_eq!(spec.format(), Format::Jsonl);
        assert!(spec.add(Spec::parse("text:spi").unwrap()).is_err());
        let log = Log::new(spec, Box::new(std::io::sink()));
        assert!(log.on(Channel::Pcie) && log.on(Channel::Io));
    }

    #[test]
    fn the_per_step_channels_need_a_diag_build() {
        assert_eq!(Spec::parse("tick").is_ok(), crate::diag::ON);
        assert!(Channel::ALL
            .into_iter()
            .filter(|c| !c.needs_diag())
            .all(|c| Spec::parse(c.name()).is_ok()));
    }

    #[test]
    fn a_line_is_only_formatted_when_its_channel_is_on() {
        struct Boom;
        impl fmt::Display for Boom {
            fn fmt(&self, _: &mut fmt::Formatter<'_>) -> fmt::Result {
                panic!("formatted a line nobody asked for")
            }
        }
        let (log, buf) = open("spi");
        crate::log!(log, Channel::Pcie, "{}", Boom);
        crate::log!(Log::default(), Channel::Spi, "{}", Boom);
        assert_eq!(text(&buf), "");
    }

    #[test]
    fn a_line_is_the_time_its_channel_and_message() {
        let (log, buf) = open("pcie");
        let bus = 0x1000;
        crate::log!(log, Channel::Pcie, "endpoint read at bus {bus:#x}");
        log.set_time(9_002_222);
        crate::log!(log, Channel::Pcie, "inbound window");
        log.set_time(12_345_000_001);
        crate::log!(log, Channel::Pcie, "much later");
        assert_eq!(
            text(&buf),
            "   0.000000 pcie: endpoint read at bus 0x1000\n   \
             9.002222 pcie: inbound window\n\
             12345.000001 pcie: much later\n"
        );

        let (log, buf) = open("jsonl:arm-exc");
        log.set_time(9_002_222);
        crate::log!(log, Channel::ArmExc, "pc \"here\"");
        assert_eq!(
            text(&buf),
            "{\"us\":9002222,\"channel\":\"arm-exc\",\"msg\":\"pc \\\"here\\\"\"}\n"
        );
    }

    #[test]
    fn a_block_run_carries_the_time_of_its_first_block() {
        let (log, buf) = open("io,pcie");
        log.set_time(1_000_000);
        log.blocks("sd", "read", 0x800, 1);
        log.set_time(2_000_000);
        log.blocks("sd", "read", 0x801, 7);
        log.set_time(3_000_000);
        crate::log!(log, Channel::Pcie, "endpoint irq");
        assert_eq!(
            text(&buf),
            "   1.000000 io: sd   read  lba 0x800+8\n   3.000000 pcie: endpoint irq\n"
        );
    }

    #[test]
    fn contiguous_block_runs_merge_and_other_lines_split_them() {
        let (log, buf) = open("io,pcie");
        log.blocks("sd", "read", 0x800, 1);
        log.blocks("sd", "read", 0x801, 7);
        log.otp_read(19, 0x8aa9_6d38, true, "board identity, word 1 of 4");
        log.blocks("sd", "read", 0x808, 1);
        crate::log!(log, Channel::Pcie, "endpoint irq");
        log.blocks("sd", "read", 0x809, 1);
        log.blocks("sd", "write", 0x80a, 1);
        drop(log);
        assert_eq!(
            text(&buf),
            "   0.000000 io: sd   read  lba 0x800+8\n   \
             0.000000 io: otp  read  row 19  = 0x8aa96d38  board identity, word 1 of 4\n   \
             0.000000 io: sd   read  lba 0x808+1\n   \
             0.000000 pcie: endpoint irq\n   \
             0.000000 io: sd   read  lba 0x809+1\n   \
             0.000000 io: sd   write lba 0x80a+1\n"
        );
    }

    #[test]
    fn io_lines_in_jsonl_keep_their_fields() {
        let (log, buf) = open("jsonl:io");
        log.net("tftp: RRQ \"start4.elf\"");
        log.blocks("usb", "read", 2, 3);
        drop(log);
        assert_eq!(
            text(&buf),
            "{\"us\":0,\"channel\":\"io\",\"dev\":\"net\",\"event\":\"tftp: RRQ \\\"start4.elf\\\"\"}\n\
             {\"us\":0,\"channel\":\"io\",\"dev\":\"usb\",\"op\":\"read\",\"lba\":2,\"blocks\":3,\"files\":[]}\n"
        );
    }

    #[test]
    fn otp_writes_say_what_the_row_held_before() {
        let (log, buf) = open("io");
        log.otp_write(36, 0x1111_1111, 0, "customer OTP, word 1 of 8");
        assert_eq!(
            text(&buf),
            "   0.000000 io: otp  write row 36  = 0x11111111  customer OTP, word 1 of 8 \
             (was 0x00000000)\n"
        );

        let (log, buf) = open("jsonl:io");
        log.otp_write(36, 0x1111_1111, 0, "customer OTP, word 1 of 8");
        assert_eq!(
            text(&buf),
            "{\"us\":0,\"channel\":\"io\",\"dev\":\"otp\",\"op\":\"write\",\"row\":\"36\",\
             \"value\":\"0x11111111\",\"was\":\"0x00000000\",\
             \"meaning\":\"customer OTP, word 1 of 8\"}\n"
        );
    }

    #[test]
    fn io_events_need_the_io_channel() {
        let (log, buf) = open("pcie");
        log.blocks("sd", "read", 0, 1);
        log.otp_read(19, 1, true, "board identity, word 1 of 4");
        log.net("dhcp");
        drop(log);
        assert_eq!(text(&buf), "");
    }
}
