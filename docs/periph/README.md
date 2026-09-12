<!-- generated from specs/*.toml by `cargo run -- spec-docs --update` – do not edit -->

# Peripheral register specs

Generated from the TOML specs in [`specs/`](../../specs/); see [`specs/README.md`](../../specs/README.md) for the format and the rules.

| Block | Base | Size | Registers | Summary |
|---|---|---|---|---|
| [`corectl`](corectl.md) | `0x7E002000` | `0x1000` | 4 | VPU core control: per-core boot handshake and interrupt controller |
| [`mcsync`](mcsync.md) | `0x7E000000` | `0x1000` | 4 | Doorbells / semaphores between the two VPU cores |
| [`systimer`](systimer.md) | `0x7E003000` | `0x1000` | 4 | System timer: 64-bit free-running 1 MHz counter with four compare channels |
