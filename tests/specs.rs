//! The register specs in `specs/*.toml` against the model (#39).
//!
//! `build.rs` already refuses a malformed spec. These tests check the other
//! direction: that what a device model claims to decode is in its spec, that
//! every register the spec lists actually reaches the device, and that the
//! generated `docs/periph/` is up to date. Registers a model leaves stubbed are
//! reported, not failed — run with `--nocapture` to see them.

use rpi_virt_fw::bus::Bus;
use rpi_virt_fw::periph::SPEC_COVERAGE;
use rpi_virt_fw::spec::{self, schema::Spec};
use rpi_virt_fw::Machine;

fn specs() -> Vec<Spec> {
    spec::load().unwrap_or_else(|e| panic!("{e}"))
}

fn spec_for<'a>(specs: &'a [Spec], block: &str) -> &'a Spec {
    specs
        .iter()
        .find(|s| s.block.name == block)
        .unwrap_or_else(|| panic!("no specs/{block}.toml for a device that claims it"))
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

/// Every element of every register, in every bank, has to reach the device
/// rather than the catch-all stub — a window mapped too small is exactly the
/// bug `tests/memory_map.rs` was written for, and the spec knows the extent.
#[test]
fn every_spec_register_reaches_its_device() {
    let specs = specs();
    for cov in SPEC_COVERAGE {
        let spec = spec_for(&specs, cov.block);
        let mut m = Machine::new(1024 * 1024);
        for bank in spec.bank_offsets() {
            for r in &spec.registers {
                for off in r.element_offsets() {
                    let addr = spec.block.base + bank + (off & !3);
                    let before = m.stub_hits;
                    m.load32(addr)
                        .unwrap_or_else(|e| panic!("{}.{} ({addr:#x}): {e}", cov.block, r.name));
                    assert_eq!(
                        m.stub_hits, before,
                        "{}.{} ({addr:#x}) fell through to the peripheral stub",
                        cov.block, r.name
                    );
                }
            }
        }
    }
}

/// Not a failure: the list of what is still stubbed, per block, and the specs
/// no device model claims yet.
#[test]
fn report_stubbed_registers() {
    let specs = specs();
    for spec in &specs {
        let Some(cov) = SPEC_COVERAGE.iter().find(|c| c.block == spec.block.name) else {
            eprintln!("{}: no device model", spec.block.name);
            continue;
        };
        let stubbed: Vec<&str> = spec
            .registers
            .iter()
            .filter(|r| !cov.decoded.contains(&r.offset))
            .map(|r| r.name.as_str())
            .collect();
        if stubbed.is_empty() {
            eprintln!("{}: every register modelled", spec.block.name);
        } else {
            eprintln!("{}: stubbed {}", spec.block.name, stubbed.join(", "));
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

/// The generated constants and the TOML agree, so a device matching on
/// `spec::<block>::*` really is matching on the spec.
#[test]
fn generated_constants_match_the_toml() {
    let specs = specs();
    let corectl = spec_for(&specs, "corectl");
    assert_eq!(spec::corectl::BASE, corectl.block.base);
    assert_eq!(
        spec::corectl::INSTANCE_STRIDE,
        corectl.block.instance_stride
    );
    assert_eq!(spec::mcsync::DOORBELL_COUNT, 32);
    assert_eq!(spec::systimer::C, 0x0C);
    assert_eq!(spec::systimer::CS_M3_MASK, 1 << 3);
    assert_eq!(spec::corectl::IRQ_PENDING_SOURCE_MASK, 0x3F);
    assert_eq!(spec::corectl::IRQ_PENDING_VALID_SHIFT, 8);
}

/// A spec with no provenance, or with overlapping registers, is refused. This
/// is what `build.rs` runs, so it is also what fails the build.
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
    ];
    for (what, text, expect) in cases {
        let err = spec::schema::parse("x.toml", &text)
            .err()
            .unwrap_or_else(|| panic!("{what}: accepted"));
        assert!(err.contains(expect), "{what}: unexpected error {err}");
    }
}
