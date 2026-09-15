// The SHA-256 block loops `src/arm.rs`'s tests run (`SHA_MEM`, `SHA_REGS`):
// plain Rust, compiled with
//   rustc --edition 2021 --target aarch64-unknown-none -C opt-level=2 \
//     -C panic=abort --crate-type=lib --emit=obj sha256_blocks.rs
// and each function's section taken out as it is, e.g.
//   aarch64-linux-gnu-objcopy -O binary --only-section=.text.sha256_blocks \
//     sha256_blocks.o sha256_blocks.bin
// `k` comes in as an argument so the code has no relocations to resolve.
#![no_std]

#[panic_handler]
fn panic(_: &core::panic::PanicInfo) -> ! {
    loop {}
}

/// `state` = h0..h7, `data` = `blocks` 64-byte blocks, `k` = the 64 round
/// constants. Stores the state after every block, like OpenSSL's C.
#[no_mangle]
pub unsafe extern "C" fn sha256_blocks(state: *mut u32, mut data: *const u8, blocks: usize, k: *const u32) {
    let end = data.add(blocks * 64);
    while data != end {
        let mut h = [0u32; 8];
        for i in 0..8 {
            h[i] = core::ptr::read_volatile(state.add(i));
        }
        let mut w = [0u32; 16];
        let [mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut hh] = h;
        for i in 0..64 {
            let wi = if i < 16 {
                let p = data.add(4 * i);
                let v = u32::from_be_bytes([*p, *p.add(1), *p.add(2), *p.add(3)]);
                w[i] = v;
                v
            } else {
                let s0 = w[(i + 1) & 15].rotate_right(7) ^ w[(i + 1) & 15].rotate_right(18) ^ (w[(i + 1) & 15] >> 3);
                let s1 = w[(i + 14) & 15].rotate_right(17) ^ w[(i + 14) & 15].rotate_right(19) ^ (w[(i + 14) & 15] >> 10);
                let v = w[i & 15]
                    .wrapping_add(s0)
                    .wrapping_add(w[(i + 9) & 15])
                    .wrapping_add(s1);
                w[i & 15] = v;
                v
            };
            let s1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
            let ch = (e & f) ^ (!e & g);
            let t1 = hh
                .wrapping_add(s1)
                .wrapping_add(ch)
                .wrapping_add(*k.add(i))
                .wrapping_add(wi);
            let s0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
            let maj = (a & b) ^ (a & c) ^ (b & c);
            let t2 = s0.wrapping_add(maj);
            hh = g;
            g = f;
            f = e;
            e = d.wrapping_add(t1);
            d = c;
            c = b;
            b = a;
            a = t1.wrapping_add(t2);
        }
        let out = [a, b, c, d, e, f, g, hh];
        for i in 0..8 {
            core::ptr::write_volatile(state.add(i), h[i].wrapping_add(out[i]));
        }
        data = data.add(64);
    }
}

#[no_mangle]
pub unsafe extern "C" fn sha256_blocks_regs(state: *mut u32, mut data: *const u8, blocks: usize, k: *const u32) {
    let end = data.add(blocks * 64);
    let mut h = [0u32; 8];
    for i in 0..8 {
        h[i] = core::ptr::read_volatile(state.add(i));
    }
    while data != end {
        let mut w = [0u32; 16];
        let [mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut hh] = h;
        for i in 0..64 {
            let wi = if i < 16 {
                let p = data.add(4 * i);
                let v = u32::from_be_bytes([*p, *p.add(1), *p.add(2), *p.add(3)]);
                w[i] = v;
                v
            } else {
                let s0 = w[(i + 1) & 15].rotate_right(7) ^ w[(i + 1) & 15].rotate_right(18) ^ (w[(i + 1) & 15] >> 3);
                let s1 = w[(i + 14) & 15].rotate_right(17) ^ w[(i + 14) & 15].rotate_right(19) ^ (w[(i + 14) & 15] >> 10);
                let v = w[i & 15]
                    .wrapping_add(s0)
                    .wrapping_add(w[(i + 9) & 15])
                    .wrapping_add(s1);
                w[i & 15] = v;
                v
            };
            let s1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
            let ch = (e & f) ^ (!e & g);
            let t1 = hh
                .wrapping_add(s1)
                .wrapping_add(ch)
                .wrapping_add(*k.add(i))
                .wrapping_add(wi);
            let s0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
            let maj = (a & b) ^ (a & c) ^ (b & c);
            let t2 = s0.wrapping_add(maj);
            hh = g;
            g = f;
            f = e;
            e = d.wrapping_add(t1);
            d = c;
            c = b;
            b = a;
            a = t1.wrapping_add(t2);
        }
        let out = [a, b, c, d, e, f, g, hh];
        for i in 0..8 {
            h[i] = h[i].wrapping_add(out[i]);
            core::ptr::write_volatile(state.add(i), h[i]);
        }
        data = data.add(64);
    }
}
