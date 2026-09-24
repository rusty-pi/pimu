//! `boot --mbox-property`: a property-interface request to the booted
//! firmware, posted the way a Linux client does.

use anyhow::{bail, Result};

use pimu::emulator::{Emulator, RunLimits};

/// Where the request buffer is built. Well clear of everything `--dram-map`
/// reports dirty at `arm_loader` — the kernel ends below `0x0280_0000`, the
/// device tree sits at `0x2eff_1e00`, and start4's own image is above
/// `0x3ebe_4000`.
const MBOX_BUFFER: u32 = 0x1000_0000;

/// One tag in a `--mbox-property` request: the tag, an optional override of the
/// value-buffer size, and optional request words (a `key_id`, most often).
pub type MboxTag = (u32, Option<u32>, Vec<u32>);

/// What one exchange posts.
pub enum MboxRequest {
    /// `--mbox-property`: tags, each staged in a word-aligned slot with an end
    /// marker and slack, the way a well-behaved client lays a request out.
    Tags(Vec<MboxTag>),
    /// `--mbox-raw`: an exact byte image of somebody else's request. A client
    /// whose `sizeof` is wrong lays the buffer out in ways this one never
    /// would — an unaligned declared total, an end tag at an odd offset, stale
    /// bytes past the total — and that is what decides whether the firmware
    /// accepts it. Staging it word by word cannot reproduce any of those.
    Raw(Vec<u8>),
}

/// Post a property-interface request to the still-running firmware, the way a
/// booted Linux does through `/dev/vcio`, and report what comes back.
///
/// This stands in for that Linux client: build the buffer, ring the doorbell,
/// keep the machine running, and read the reply. The address on the wire is
/// `0xC000_0000 | phys` because Linux allocates the buffer coherently and
/// `/soc` carries `dma-ranges = <0xc0000000 0x0 0x0 0x40000000>` — the uncached
/// alias, which the model already maps to the same DRAM.
pub fn mbox_property_exchange(
    emu: &mut Emulator,
    limits: &RunLimits,
    request: &MboxRequest,
) -> Result<()> {
    use pimu::bus::{Bus, Width};

    println!("\n--- ARM property mailbox (0x7e00_b880) ---");
    let tags: &[MboxTag] = match request {
        MboxRequest::Tags(t) => t,
        MboxRequest::Raw(_) => &[],
    };

    // Each tag names its own value-buffer size, and the firmware walks the
    // request by those sizes — so one wrong size desynchronises every tag after
    // it and the whole buffer comes back `0x80000001` (parse error). A fixed
    // 64-byte slot for everything did exactly that.
    //
    // Sizes and request payloads follow raspberrypi/utils `rpifwcrypto.c`,
    // which is the Linux-side client of the same interface. The service itself
    // lives in `start4.elf` (`arm_crypto_*`, with its own mbedTLS) — this is
    // only the caller, standing in for the Linux client.
    let spec = |tag: u32| -> (u32, Vec<u32>) {
        match tag {
            // `flags, key_id` in; `status, length, key[]` back. The buffer has
            // to hold the key, so it is sized by the client's maxima:
            // 512 bytes of public key, 1024 of private key.
            0x0003_0093 => (8 + 512, vec![0, 0]),
            0x0003_0094 => (8 + 1024, vec![0, 0]),
            // `flags, key_id` in, nothing back.
            0x0003_0095 => (8, vec![0, 0]),
            // `key_id, status` / `key_id, usage` in.
            0x0003_8090 | 0x0003_809c => (8, vec![0, 0]),
            // `key_id` in, one word back.
            0x0003_0090 | 0x0003_009c => (4, vec![0]),
            // `flags, key_id, length, hash[32]` in; `status, length, sig[]`
            // back, so the buffer has to be the larger of the two.
            0x0003_0091 => (128, vec![0, 0, 32]),
            // `flags, key_id, length, message[]` in; `status, length,
            // hmac[32]` back. A fixed short message keeps the result stable
            // across runs, which is what makes it a regression.
            0x0003_0092 => {
                let mut v = vec![0, 0, 16];
                v.extend_from_slice(&[0x6c6c6548, 0x77202c6f, 0x646c726f, 0x00000021]);
                (128, v)
            }
            // Everything else: one word in, one word back.
            _ => (4, vec![0]),
        }
    };

    let mut words: Vec<u32> = vec![0, 0];
    for (tag, override_size, override_req) in tags {
        let (tag, override_size) = (*tag, *override_size);
        let (size, payload) = spec(tag);
        let size = override_size.unwrap_or(size);
        let payload = if override_req.is_empty() {
            payload
        } else {
            override_req.clone()
        };
        words.push(tag);
        words.push(size);
        words.push(0);
        // Rounded up: the firmware writes a 6-byte answer into a 6-byte slot,
        // and a slot of one word would put the end marker under the last two
        // bytes of it.
        let slot = size.div_ceil(4) as usize;
        for i in 0..slot {
            words.push(payload.get(i).copied().unwrap_or(0));
        }
    }
    // End marker, then slack. The firmware rejects a buffer whose declared
    // total ends exactly at the marker: the last tag comes back unhandled and
    // the whole buffer gets `0x80000001`, whichever tag is last. `rpifwcrypto.c`
    // never hits this because it declares `sizeof(msg)` — its value arrays are
    // bigger than the `tag_buf_size` it asks for, so its total always carries
    // spare room past the marker.
    words.push(0);
    words.extend_from_slice(&[0; 4]);
    words[0] = (words.len() as u32) * 4;

    let staged = match request {
        MboxRequest::Tags(_) => {
            for (i, w) in words.iter().enumerate() {
                emu.machine
                    .store(MBOX_BUFFER + (i as u32) * 4, Width::Word, *w)
                    .map_err(|e| anyhow::anyhow!("staging the request buffer: {e}"))?;
            }
            words.len() * 4
        }
        MboxRequest::Raw(bytes) => {
            // Byte by byte, and nothing else touched: what lies past the image
            // is part of the test, so the caller's bytes are the whole buffer.
            for (i, b) in bytes.iter().enumerate() {
                emu.machine
                    .store(MBOX_BUFFER + i as u32, Width::Byte, *b as u32)
                    .map_err(|e| anyhow::anyhow!("staging the raw request buffer: {e}"))?;
            }
            bytes.len()
        }
    };

    let bus_addr = 0xC000_0000 | MBOX_BUFFER;
    let message = (bus_addr & !0xF) | pimu::periph::mbox::CHANNEL_PROPERTY;
    match request {
        MboxRequest::Tags(_) => println!(
            "  posting {message:#010x}  ({} tags, {staged} byte buffer at {MBOX_BUFFER:#010x})",
            tags.len()
        ),
        MboxRequest::Raw(bytes) => {
            // The image's own header, which is the point: a client that
            // declares a total its layout does not match is exactly the case
            // worth replaying.
            let declared =
                u32::from_le_bytes(std::array::from_fn(|i| bytes.get(i).copied().unwrap_or(0)));
            println!(
                "  posting {message:#010x}  (raw {staged} byte image at \
                 {MBOX_BUFFER:#010x}, declared total {declared})"
            );
        }
    }
    if !emu.machine.mbox.post_from_arm(message) {
        bail!("the mailbox is full — the firmware has not drained earlier requests");
    }

    // The firmware is parked in the ThreadX idle loop by now, so the two stop
    // conditions that end a *boot* would end this instantly and wrongly: the
    // idle-spin detector fires on the idle loop itself, and the silence
    // watchdog fires because a serviced mailbox request prints nothing.
    //
    // Nothing in `RunLimits` can say "stop when the reply lands", so run in
    // short slices and check between them. The answer takes a few million
    // instructions once the interrupt gets through; the budget is there for
    // the case where it does not.
    // Short slices, because the check between them is also what dates the
    // reply: the firmware idles through `sleep`, so half a second of wall
    // clock is ten of modelled time — ten times what a Linux client waits.
    let slice = RunLimits {
        max_steps: None,
        max_wall: Some(std::time::Duration::from_millis(10)),
        idle_spin_limit: 0,
        silent_us: u64::MAX,
        ..limits.clone()
    };
    let budget = std::time::Duration::from_secs(20);
    let started = std::time::Instant::now();
    let retired_before = emu.cpu.retired;
    let us_before = emu.machine.systimer.now_us();
    let replies_before = emu.machine.mbox.writes;
    let mut console = Vec::new();
    let mut report = emu.run(&slice);
    loop {
        console.extend_from_slice(&report.console);
        let answered =
            !emu.machine.mbox.request_outstanding() && emu.machine.mbox.writes > replies_before;
        if answered || started.elapsed() >= budget {
            break;
        }
        report = emu.run(&slice);
    }
    // Modelled time is what a real client's timeout counts (Linux's
    // `raspberrypi-firmware` gives up after one second); the wall clock only
    // says how long the interpreter took.
    println!(
        "  resumed: {} instructions, {} us modelled, over {:.1?}, ended {:?} at {:#010x}",
        report.retired.saturating_sub(retired_before),
        emu.machine.systimer.now_us().saturating_sub(us_before),
        started.elapsed(),
        report.end,
        report.pc
    );
    println!(
        "  mailbox: config1 {:#x}, {} requests taken, {} replies written",
        emu.machine.mbox.interrupt_armed(),
        emu.machine.mbox.reads,
        emu.machine.mbox.writes
    );

    match emu.machine.mbox.take_reply() {
        Some(reply) => println!("  reply {reply:#010x}"),
        None if emu.machine.mbox.request_outstanding() => {
            println!("  no reply: the firmware never read the request off MAIL1");
            println!("  (the firmware's mailbox reader waits for the mailbox interrupt, so");
            println!("   that wake never arrived: check that the config word above carries");
            println!("   the pending bit 4)");
            return Ok(());
        }
        None => println!("  the request was read, but no reply was written to MAIL0"),
    }

    let code = emu.machine.load(MBOX_BUFFER + 4, Width::Word).unwrap_or(0);
    println!(
        "  response code {code:#010x} ({})",
        match code {
            0x8000_0000 => "success",
            0x8000_0001 => "parse error",
            _ => "not a response",
        }
    );
    let total = emu.machine.load(MBOX_BUFFER, Width::Word).unwrap_or(0);
    if code != 0x8000_0000 {
        // The tag walk below trusts the sizes it staged. When the firmware
        // disagrees about them that walk is exactly what cannot be trusted, so
        // print the buffer as the firmware left it and decode by hand.
        println!("  raw reply buffer ({total} bytes by its own header):");
        let n = (total.min(1024) / 4).max(4);
        for row in 0..n.div_ceil(4) {
            let mut line = format!("  {:#010x} ", MBOX_BUFFER + row * 16);
            for col in 0..4 {
                let i = row * 4 + col;
                if i < n {
                    let w = emu
                        .machine
                        .load(MBOX_BUFFER + i * 4, Width::Word)
                        .unwrap_or(0);
                    line.push_str(&format!(" {w:08x}"));
                }
            }
            println!("{line}");
        }
    }
    let mut off = 8;
    while off + 12 <= total.min(4096) {
        let tag = emu
            .machine
            .load(MBOX_BUFFER + off, Width::Word)
            .unwrap_or(0);
        if tag == 0 {
            break;
        }
        // Bit 31 of the third word is the firmware's "I handled this" mark. A
        // tag it does not know is left exactly as it was staged, so the word
        // reads back 0 — which is how an unknown tag is told apart from a
        // handler that answered with nothing.
        let resp = emu
            .machine
            .load(MBOX_BUFFER + off + 8, Width::Word)
            .unwrap_or(0);
        let len = resp & 0x7FFF_FFFF;
        let mut vals = Vec::new();
        // Enough for the longest answer worth reading inline: a
        // 32-byte HMAC plus its status and length words.
        for i in 0..len.div_ceil(4).min(16) {
            vals.push(format!(
                "{:#010x}",
                emu.machine
                    .load(MBOX_BUFFER + off + 12 + i * 4, Width::Word)
                    .unwrap_or(0)
            ));
        }
        let mark = if resp & 0x8000_0000 != 0 {
            "answered"
        } else {
            "not handled"
        };
        println!(
            "  tag {tag:#010x}  {mark:>11}  {len:>3} bytes  {}",
            vals.join(" ")
        );
        let slot = emu
            .machine
            .load(MBOX_BUFFER + off + 4, Width::Word)
            .unwrap_or(0);
        off += 12 + ((slot.max(len) + 3) & !3);
    }
    // A trace armed by `PIMU_TRACE_ON_PC` inside the exchange is collected here,
    // after the run report that normally prints one has already run — so
    // print it, or investigating a tag handler silently produces nothing.
    if !emu.cpu.trace_log.is_empty() {
        println!(
            "\n--- instruction trace while servicing the request ({} entries) ---",
            emu.cpu.trace_log.len()
        );
        for l in &emu.cpu.trace_log {
            println!("{l}");
        }
    }
    if !console.is_empty() {
        // Anything the firmware printed while servicing the request.
        let tail = String::from_utf8_lossy(&console);
        for line in tail.lines().filter(|l| !l.is_empty()) {
            println!("  console: {line}");
        }
    }
    Ok(())
}
