//! The register specs in `specs/*.toml` against the model: every spec has a
//! device, every register reaches it, `docs/periph/` is up to date. Stubbed
//! registers are reported, not failed (`--nocapture`).

use std::collections::BTreeSet;

use pimu::bus::Bus;
use pimu::periph::SPEC_COVERAGE;
use pimu::spec::{self, schema::Bus as SpecBus, schema::Spec};
use pimu::Machine;

fn specs() -> Vec<Spec> {
    spec::load().unwrap_or_else(|e| panic!("{e}"))
}

fn spec_for<'a>(specs: &'a [Spec], block: &str) -> &'a Spec {
    specs
        .iter()
        .find(|s| s.block.name == block)
        .unwrap_or_else(|| panic!("no specs/{block}.toml for a device that claims it"))
}

/// A spec with no device is a stale file; two devices on one spec is a mistake.
#[test]
fn every_spec_has_exactly_one_device_model() {
    let specs = specs();
    let mut claimed = BTreeSet::new();
    for cov in SPEC_COVERAGE {
        spec_for(&specs, cov.block);
        assert!(
            claimed.insert(cov.block),
            "{} is claimed by two device models",
            cov.block
        );
    }
    for spec in &specs {
        assert!(
            claimed.contains(spec.block.name.as_str()),
            "{} has no device model in periph::SPEC_COVERAGE",
            spec.file
        );
    }
}

#[test]
fn every_decoded_offset_is_in_the_spec() {
    let specs = specs();
    for cov in SPEC_COVERAGE {
        let spec = spec_for(&specs, cov.block);
        for (i, &off) in cov.decoded.iter().enumerate() {
            assert!(
                spec.register(off).is_some(),
                "{}: the model decodes +{off:#x}, which {} does not list",
                cov.block,
                spec.file
            );
            assert!(
                !cov.decoded[..i].contains(&off),
                "{}: +{off:#x} listed twice in the coverage",
                cov.block
            );
        }
    }
}

/// Every element of every register, in every bank of every copy, reaches the
/// device rather than the stub or DRAM. Only the VPU's bus goes through
/// [`Machine`]'s decoder.
#[test]
fn every_spec_register_reaches_its_device() {
    let specs = specs();
    for spec in specs.iter().filter(|s| s.block.bus == SpecBus::Vpu) {
        let name = &spec.block.name;
        let mut m = Machine::new(1024 * 1024);
        for base in spec.bases() {
            for bank in spec.bank_offsets() {
                for r in &spec.registers {
                    for off in r.element_offsets() {
                        let addr = base + bank + (off & !3);
                        let (stub, ram) = (m.stub_hits, m.ram_reads);
                        m.load32(addr)
                            .unwrap_or_else(|e| panic!("{name}.{} ({addr:#x}): {e}", r.name));
                        assert_eq!(
                            m.stub_hits, stub,
                            "{name}.{} ({addr:#x}) fell through to the peripheral stub",
                            r.name
                        );
                        assert_eq!(
                            m.ram_reads, ram,
                            "{name}.{} ({addr:#x}) folded onto DRAM",
                            r.name
                        );
                    }
                }
            }
        }
    }
}

#[test]
fn report_stubbed_registers() {
    let specs = specs();
    for cov in SPEC_COVERAGE {
        let spec = spec_for(&specs, cov.block);
        let stubbed: Vec<&str> = spec
            .registers
            .iter()
            .filter(|r| !cov.decoded.contains(&r.offset))
            .map(|r| r.name.as_str())
            .collect();
        if stubbed.is_empty() {
            eprintln!("{}: every register modelled", cov.block);
        } else {
            eprintln!("{}: stubbed {}", cov.block, stubbed.join(", "));
        }
    }
}

#[test]
fn generated_docs_are_up_to_date() {
    let stale = spec::sync_docs(false).unwrap_or_else(|e| panic!("{e}"));
    assert!(
        stale.is_empty(),
        "docs/periph is out of date ({}); run `cargo run -- spec-docs --update`",
        stale.join(", ")
    );
}

/// A device matching on `spec::<block>::*` really is matching on the spec.
#[test]
fn generated_constants_match_the_toml() {
    let specs = specs();
    let corectl = spec_for(&specs, "corectl");
    assert_eq!(spec::corectl::BASE, corectl.block.base);
    assert_eq!(
        spec::corectl::INSTANCE_STRIDE,
        corectl.block.instance_stride
    );
    assert_eq!(spec::mcsync::SEMA_COUNT, 32);
    assert_eq!(spec::systimer::C, 0x0C);
    assert_eq!(spec::systimer::CS_M3_MASK, 1 << 3);
    assert_eq!(spec::corectl::IRQ_PENDING_SOURCE_MASK, 0x7F);
    assert_eq!(spec::corectl::IRQ_PENDING_PRIO_SHIFT, 8);

    let bsc = spec_for(&specs, "bsc");
    assert_eq!(bsc.block.copies[0].name, "PMIC");
    assert_eq!(spec::bsc::PMIC_BASE, bsc.block.copies[0].base);
    assert_eq!(spec::fxl6408::BASE, 0x43);
    assert_eq!(spec::bcm54213pe::PHYSID2, 3);
    assert_eq!(spec::xhci::HCIVERSION, 0x02);
    assert_eq!(spec::xhci::HCIVERSION_RESET, 0x0100);
}

/// No provenance or overlapping registers is refused, by the code `build.rs` runs.
#[test]
fn malformed_specs_are_refused() {
    let head = r#"
        [block]
        name = "x"
        base = 0x7E000000
        size = 0x100
        summary = "test"
        [[block.source]]
        kind = "inferred"
        ref = "test"
        confidence = "low"
    "#;
    let src = r#"
        [[register.source]]
        kind = "inferred"
        ref = "test"
        confidence = "low"
    "#;
    let i2c = r#"
        [block]
        name = "x"
        bus = "i2c"
        base = 0x80
        size = 0x100
        summary = "test"
        [[block.source]]
        kind = "inferred"
        ref = "test"
        confidence = "low"
    "#;
    let cases = [
        (
            "no source",
            format!("{head}\n[[register]]\nname = \"A\"\noffset = 0\naccess = \"r\"\n"),
            "no [[register.source]]",
        ),
        (
            "overlap",
            format!(
                "{head}\n[[register]]\nname = \"A\"\noffset = 0\ncount = 2\nstride = 4\naccess = \"r\"\n{src}\
                 [[register]]\nname = \"B\"\noffset = 4\naccess = \"r\"\n{src}"
            ),
            "overlaps",
        ),
        (
            "field outside the register",
            format!(
                "{head}\n[[register]]\nname = \"A\"\noffset = 0\nwidth = 16\naccess = \"r\"\n{src}\
                 [[register.field]]\nname = \"F\"\nbits = \"17:16\"\n\
                 [[register.field.source]]\nkind = \"inferred\"\nref = \"t\"\nconfidence = \"low\"\n"
            ),
            "outside the 16-bit register",
        ),
        (
            "past the window",
            format!("{head}\n[[register]]\nname = \"A\"\noffset = 0x100\naccess = \"r\"\n{src}"),
            "past the",
        ),
        (
            "unknown key",
            format!("{head}\n[[register]]\nname = \"A\"\noffset = 0\naccess = \"r\"\ncolour = 1\n{src}"),
            "unknown field",
        ),
        (
            "copy with no source",
            format!("{head}\n[[block.copy]]\nname = \"B\"\nbase = 0x7E001000\n"),
            "no [[block.copy.source]]",
        ),
        (
            "I²C address past 7 bits",
            format!("{i2c}\n[[register]]\nname = \"A\"\noffset = 1\nwidth = 8\naccess = \"r\"\n{src}"),
            "does not fit the i2c bus",
        ),
    ];
    for (what, text, expect) in cases {
        let err = spec::schema::parse("x.toml", &text)
            .err()
            .unwrap_or_else(|| panic!("{what}: accepted"));
        assert!(err.contains(expect), "{what}: unexpected error {err}");
    }
}

/// `docs/vpu-isa.md` comes from `isa/vpu.toml` the same way.
#[test]
fn isa_doc_is_current() {
    let stale = pimu::isa::sync_docs(false).unwrap_or_else(|e| panic!("{e}"));
    assert!(
        stale.is_empty(),
        "docs/vpu-isa.md is out of date; run `cargo run -- spec-docs --update`"
    );
}
