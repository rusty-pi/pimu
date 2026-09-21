#[test]
fn dbg() {
    use rpi_virt_fw::vpu::decode::decode;
    use rpi_virt_fw::vpu::insn::Op;
    let raw: u128 = 264569989104650;
    let bytes: Vec<u8> = (0..6).map(|i| (raw >> (8 * i)) as u8).collect();
    println!("bytes {bytes:02x?}");
    let i = decode(&bytes, 0);
    if let Op::Vector(v) = &i.op {
        println!(
            "len {} mem {} subop {} d_dash {} a_dash {} b {:?} addr {:?}",
            i.len,
            v.mem,
            v.subop,
            v.d.is_dash(),
            v.a.is_dash(),
            v.b,
            v.addr
        );
    }
}
