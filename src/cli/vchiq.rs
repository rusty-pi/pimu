//! `boot --gencmd`: a `vcgencmd` round trip over VCHIQ, driven from here.
//!
//! The kernel's `bcm2835_vchiq` only *connects* once a userspace client opens
//! `/dev/vchiq`, and there is no userspace in this bench — so this harness is
//! that client: it lays out a slot area of its own as `vchiq_init_slots` does,
//! hands it over through the `VCHIQ_INIT` property tag, and speaks the protocol
//! (`CONNECT`, `OPEN` of `GCMD`, a `DATA` per command, `CLOSE`), polling the
//! shared area for answers rather than taking them off the doorbell.
//!
//! **Run it with the ARM parked** (`--sd firmware/sd-halt.img`, or a boot
//! stopped after a kernel panic): handing the area over posts a property
//! request, and a live kernel's `bcm2835-mbox` would take our reply as its own
//! and oops in `complete`. `--mbox-property` has the same hazard.
//!
//! Layout follows `vchiq_arm/vchiq_core.{c,h}` on `rpi-6.12.y`, the `GCMD`
//! message format `vmcs_host/vc_vchi_gencmd.c` in `raspberrypi/userland`.

use anyhow::{bail, Context, Result};

use pimu::bus::{Bus, Width};
use pimu::emulator::{Emulator, RunLimits};

use crate::mbox::{mbox_property_exchange, MboxRequest};

/// Clear of the kernel, the device tree, start4's image and the `--mbox-property` buffer.
const SLOT_AREA: u32 = 0x1010_0000;
/// The uncached alias `vchiq_platform_init` hands over as `channelbase`.
const BUS_ALIAS: u32 = 0xC000_0000;

const SLOT_SIZE: u32 = 4096;
const MAX_SLOTS_PER_SIDE: u32 = 64;
const ZERO_SLOTS: u32 = 1;
const TOTAL_SLOTS: u32 = ZERO_SLOTS + 2 * 32;

const MAGIC: u32 = 0x5643_4849;
const VERSION: u32 = 8;
const VERSION_MIN: u32 = 3;
const SLOT_ZERO_SIZE: u32 = 1288;

/// Offsets into `struct vchiq_slot_zero` and `struct vchiq_shared_state`.
const MASTER: u32 = 0x20;
const SLAVE: u32 = 0x194;
const INITIALISED: u32 = 0x00;
const SLOT_FIRST: u32 = 0x04;
const SLOT_LAST: u32 = 0x08;
const SLOT_SYNC: u32 = 0x0c;
const TRIGGER: u32 = 0x10;
const TX_POS: u32 = 0x1c;
const RECYCLE: u32 = 0x20;
const SLOT_QUEUE_RECYCLE: u32 = 0x2c;
const SYNC_TRIGGER: u32 = 0x30;
const SYNC_RELEASE: u32 = 0x3c;
const SLOT_QUEUE: u32 = 0x48;
const DEBUG: u32 = SLOT_QUEUE + 4 * MAX_SLOTS_PER_SIDE;
const DEBUG_MAX: u32 = 11;
const ARMED: u32 = 0;
const FIRED: u32 = 4;

const MSGID: u32 = 0;
const SIZE: u32 = 4;
const DATA: u32 = 8;
const HEADER_SIZE: u32 = 8;

const MSG_CONNECT: u32 = 1;
const MSG_OPEN: u32 = 2;
const MSG_OPENACK: u32 = 3;
const MSG_CLOSE: u32 = 4;
const MSG_DATA: u32 = 5;
const TYPE_SHIFT: u32 = 24;
const PORT_SHIFT: u32 = 12;
const PORT_MASK: u32 = 0xfff;

/// `VCHIQ_MAKE_FOURCC('G', 'C', 'M', 'D')` and `VC_GENCMD_VER`.
const FOURCC_GCMD: u32 = u32::from_be_bytes(*b"GCMD");
const GCMD_VERSION: u32 = 1;
/// The client's to choose; `vchiq_open_service_internal` passes the instance pointer as the id.
const LOCAL_PORT: u32 = 5;
const CLIENT_ID: u32 = 0x1234;

const BELL0: u32 = 0x7e00_b840;

fn stride(size: u32) -> u32 {
    (size + HEADER_SIZE + HEADER_SIZE - 1) & !(HEADER_SIZE - 1)
}

fn make_msgid(ty: u32, src: u32, dst: u32) -> u32 {
    ty << TYPE_SHIFT | src << PORT_SHIFT | dst
}

struct Slave {
    /// Slot zero in the VPU's uncached view — the addresses the firmware uses.
    base: u32,
    tx_pos: u32,
    tx_slot: u32,
    tx_available: u32,
    rx_pos: u32,
    rx_slot: u32,
    rx_index: u32,
}

struct Message {
    msgid: u32,
    size: u32,
    payload: Vec<u8>,
}

impl Message {
    fn ty(&self) -> u32 {
        self.msgid >> TYPE_SHIFT
    }

    fn srcport(&self) -> u32 {
        self.msgid >> PORT_SHIFT & PORT_MASK
    }
}

impl Slave {
    /// Lay out slot zero as `vchiq_init_slots` does, leaving the master's side for the firmware.
    fn new(emu: &mut Emulator, base: u32) -> Result<Slave> {
        let mut w = |at: u32, v: u32| -> Result<()> {
            emu.machine
                .store(base + at, Width::Word, v)
                .map_err(|e| anyhow::anyhow!("staging slot zero at {:#x}: {e}", base + at))
        };
        for i in 0..SLOT_ZERO_SIZE / 4 {
            w(i * 4, 0)?;
        }
        w(0x00, MAGIC)?;
        w(0x04, VERSION | VERSION_MIN << 16)?;
        w(0x08, SLOT_ZERO_SIZE)?;
        w(0x0c, SLOT_SIZE)?;
        w(0x10, 128)?;
        w(0x14, MAX_SLOTS_PER_SIDE)?;

        let slots = TOTAL_SLOTS - ZERO_SLOTS;
        let first_data = ZERO_SLOTS;
        w(MASTER + SLOT_SYNC, first_data)?;
        w(MASTER + SLOT_FIRST, first_data + 1)?;
        w(MASTER + SLOT_LAST, first_data + slots / 2 - 1)?;
        w(SLAVE + SLOT_SYNC, first_data + slots / 2)?;
        w(SLAVE + SLOT_FIRST, first_data + slots / 2 + 1)?;
        w(SLAVE + SLOT_LAST, first_data + slots - 1)?;

        let (first, last) = (first_data + slots / 2 + 1, first_data + slots - 1);
        let mine = last + 1 - first;
        for i in 0..mine {
            w(SLAVE + SLOT_QUEUE + 4 * i, first + i)?;
        }
        w(SLAVE + TX_POS, 0)?;
        w(SLAVE + SLOT_QUEUE_RECYCLE, mine)?;
        for event in [TRIGGER, RECYCLE, SYNC_TRIGGER, SYNC_RELEASE] {
            w(SLAVE + event + ARMED, 0)?;
        }
        w(SLAVE + DEBUG, DEBUG_MAX)?;
        w((first_data + slots / 2) * SLOT_SIZE, 0)?;
        w(SLAVE + SYNC_RELEASE + FIRED, 1)?;
        w(SLAVE + INITIALISED, 1)?;

        Ok(Slave {
            base,
            tx_pos: 0,
            tx_slot: 0,
            tx_available: mine,
            rx_pos: 0,
            rx_slot: 0,
            rx_index: 0,
        })
    }

    fn read(&self, emu: &mut Emulator, at: u32) -> u32 {
        emu.machine.load(at, Width::Word).unwrap_or(0)
    }

    fn write(&self, emu: &mut Emulator, at: u32, value: u32) {
        let _ = emu.machine.store(at, Width::Word, value);
    }

    /// `queue_message` without the quotas. The firmware polls, so no doorbell is rung.
    fn send(&mut self, emu: &mut Emulator, msgid: u32, payload: &[u8]) -> Result<()> {
        let space = stride(payload.len() as u32);
        let left = SLOT_SIZE - (self.tx_pos & (SLOT_SIZE - 1));
        if space > left {
            let header = self.tx_slot + (self.tx_pos & (SLOT_SIZE - 1));
            self.write(emu, header + MSGID, 0);
            self.write(emu, header + SIZE, left - HEADER_SIZE);
            self.tx_pos += left;
        }
        if self.tx_pos & (SLOT_SIZE - 1) == 0 {
            if self.tx_pos / SLOT_SIZE == self.tx_available {
                bail!("the firmware has not recycled a slot; nowhere to put the message");
            }
            let queued = SLOT_QUEUE + 4 * ((self.tx_pos / SLOT_SIZE) & (MAX_SLOTS_PER_SIDE - 1));
            let index = self.read(emu, self.base + SLAVE + queued);
            self.tx_slot = self.base + index * SLOT_SIZE;
        }
        let header = self.tx_slot + (self.tx_pos & (SLOT_SIZE - 1));
        self.write(emu, header + MSGID, msgid);
        self.write(emu, header + SIZE, payload.len() as u32);
        for (i, byte) in payload.iter().enumerate() {
            let _ = emu
                .machine
                .store(header + DATA + i as u32, Width::Byte, *byte as u32);
        }
        self.tx_pos += space;
        self.write(emu, self.base + SLAVE + TX_POS, self.tx_pos);
        // `remote_event_signal`. The firmware never arms, so check rather than ring blind.
        let event = self.base + MASTER + TRIGGER;
        self.write(emu, event + FIRED, 1);
        if self.read(emu, event + ARMED) != 0 {
            let _ = emu.machine.store(0x7e00_b848, Width::Word, 0);
        }
        Ok(())
    }

    fn receive(&mut self, emu: &mut Emulator) -> Option<Message> {
        let tx_pos = self.read(emu, self.base + MASTER + TX_POS);
        while self.rx_pos != tx_pos {
            if self.rx_pos & (SLOT_SIZE - 1) == 0 {
                let queued =
                    SLOT_QUEUE + 4 * ((self.rx_pos / SLOT_SIZE) & (MAX_SLOTS_PER_SIDE - 1));
                self.rx_index = self.read(emu, self.base + MASTER + queued);
                self.rx_slot = self.base + self.rx_index * SLOT_SIZE;
            }
            let header = self.rx_slot + (self.rx_pos & (SLOT_SIZE - 1));
            let msgid = self.read(emu, header + MSGID);
            let size = self.read(emu, header + SIZE);
            let mut payload = Vec::new();
            for i in 0..size.min(SLOT_SIZE) {
                payload.push(
                    emu.machine
                        .load(header + DATA + i, Width::Byte)
                        .unwrap_or(0) as u8,
                );
            }
            self.rx_pos += stride(size);
            if self.rx_pos & (SLOT_SIZE - 1) == 0 {
                self.release(emu, self.rx_index);
            }
            if msgid >> TYPE_SHIFT != 0 {
                return Some(Message {
                    msgid,
                    size,
                    payload,
                });
            }
        }
        None
    }

    fn release(&mut self, emu: &mut Emulator, index: u32) {
        let master = self.base + MASTER;
        let recycle = self.read(emu, master + SLOT_QUEUE_RECYCLE);
        let at = SLOT_QUEUE + 4 * (recycle & (MAX_SLOTS_PER_SIDE - 1));
        self.write(emu, master + at, index);
        self.write(emu, master + SLOT_QUEUE_RECYCLE, recycle.wrapping_add(1));
        let event = master + RECYCLE;
        self.write(emu, event + FIRED, 1);
    }

    /// Wait for a message of one type, running the machine in slices. `armed` is
    /// set first so the firmware's `signal` takes its "ring the bell" branch.
    fn wait(
        &mut self,
        emu: &mut Emulator,
        limits: &RunLimits,
        ty: u32,
        what: &str,
    ) -> Result<Message> {
        let event = self.base + SLAVE + TRIGGER;
        let slice = RunLimits {
            max_steps: None,
            max_wall: Some(std::time::Duration::from_millis(10)),
            idle_spin_limit: 0,
            silent_us: u64::MAX,
            ..limits.clone()
        };
        let budget = std::time::Duration::from_secs(30);
        let started = std::time::Instant::now();
        loop {
            self.write(emu, event + FIRED, 0);
            self.write(emu, event + ARMED, 1);
            if let Some(message) = self.receive(emu) {
                self.write(emu, event + ARMED, 0);
                let rung = emu.machine.bell.take(0);
                if message.ty() != ty {
                    bail!(
                        "waiting for {what}: got message type {} ({:#010x})",
                        message.ty(),
                        message.msgid
                    );
                }
                let _ = rung;
                return Ok(message);
            }
            if started.elapsed() >= budget {
                self.write(emu, event + ARMED, 0);
                bail!("waiting for {what}: nothing came back in {budget:?}");
            }
            emu.run(&slice);
            // Reading the bell clears it and drops the line, as the ARM handler does.
            let _ = emu.machine.load(BELL0, Width::Word);
        }
    }
}

/// Run every `--gencmd` command against the firmware's `GCMD` service.
pub fn gencmd_exchange(emu: &mut Emulator, limits: &RunLimits, commands: &[String]) -> Result<()> {
    println!("\n--- VCHIQ gencmd (slot area at {SLOT_AREA:#010x}) ---");
    let base = BUS_ALIAS | SLOT_AREA;
    let mut slave = Slave::new(emu, base).context("laying out slot zero")?;

    // Hand the area over before anything is queued.
    let tag = (0x0004_8010, Some(4), vec![base]);
    mbox_property_exchange(emu, limits, &MboxRequest::Tags(vec![tag]))?;
    if emu
        .machine
        .load(base + MASTER + INITIALISED, Width::Word)
        .unwrap_or(0)
        == 0
    {
        bail!("the firmware did not bring its side up: master.initialised is still 0");
    }
    println!(
        "  master up: slots {}..{}, tx_pos {}",
        emu.machine
            .load(base + MASTER + SLOT_FIRST, Width::Word)
            .unwrap_or(0),
        emu.machine
            .load(base + MASTER + SLOT_LAST, Width::Word)
            .unwrap_or(0),
        emu.machine
            .load(base + MASTER + TX_POS, Width::Word)
            .unwrap_or(0),
    );

    slave.send(emu, make_msgid(MSG_CONNECT, 0, 0), &[])?;
    slave.wait(emu, limits, MSG_CONNECT, "CONNECT")?;
    println!("  CONNECT acknowledged");

    let mut open = Vec::new();
    open.extend_from_slice(&FOURCC_GCMD.to_le_bytes());
    open.extend_from_slice(&CLIENT_ID.to_le_bytes());
    open.extend_from_slice(&(GCMD_VERSION as u16).to_le_bytes());
    open.extend_from_slice(&(GCMD_VERSION as u16).to_le_bytes());
    slave.send(emu, make_msgid(MSG_OPEN, LOCAL_PORT, 0), &open)?;
    let ack = slave.wait(emu, limits, MSG_OPENACK, "OPENACK")?;
    let remote = ack.srcport();
    let peer = u16::from_le_bytes([
        ack.payload.first().copied().unwrap_or(0),
        ack.payload.get(1).copied().unwrap_or(0),
    ]);
    println!("  GCMD open on firmware port {remote}, peer version {peer}");

    for command in commands {
        let mut payload = command.as_bytes().to_vec();
        payload.push(0);
        slave.send(emu, make_msgid(MSG_DATA, LOCAL_PORT, remote), &payload)?;
        let reply = slave.wait(emu, limits, MSG_DATA, "the gencmd answer")?;
        let status = u32::from_le_bytes(std::array::from_fn(|i| {
            reply.payload.get(i).copied().unwrap_or(0)
        }));
        let text = reply.payload.get(4..).unwrap_or(&[]);
        let text = String::from_utf8_lossy(text.split(|&b| b == 0).next().unwrap_or(&[]));
        println!("  {command}  ->  status {status}, {} bytes", reply.size);
        for line in text.lines() {
            println!("      {line}");
        }
    }

    slave.send(emu, make_msgid(MSG_CLOSE, LOCAL_PORT, remote), &[])?;
    slave.wait(emu, limits, MSG_CLOSE, "CLOSE")?;
    println!(
        "  GCMD closed; doorbell 0 rung {} times",
        emu.machine.bell.rings[0]
    );
    Ok(())
}
