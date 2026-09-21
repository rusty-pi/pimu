use rpi_virt_fw::vpu::decode::decode;
use rpi_virt_fw::vpu::insn::{Op, VecExec};

#[test]
fn census() {
    let elf = std::fs::read("firmware/start4.elf").unwrap();
    let shoff = u32::from_le_bytes(elf[32..36].try_into().unwrap()) as usize;
    let shentsize = u16::from_le_bytes(elf[46..48].try_into().unwrap()) as usize;
    let shnum = u16::from_le_bytes(elf[48..50].try_into().unwrap()) as usize;
    let shstrndx = u16::from_le_bytes(elf[50..52].try_into().unwrap()) as usize;
    let strp = shoff + shstrndx * shentsize;
    let stroff = u32::from_le_bytes(elf[strp + 16..strp + 20].try_into().unwrap()) as usize;
    let (mut total, mut ok) = (0usize, 0usize);
    let mut dump = String::new();
    for i in 0..shnum {
        let p = shoff + i * shentsize;
        let nameoff = u32::from_le_bytes(elf[p..p + 4].try_into().unwrap()) as usize;
        let b = &elf[stroff + nameoff..];
        let n = b.iter().position(|c| *c == 0).unwrap();
        if std::str::from_utf8(&b[..n]).unwrap() != ".text" {
            continue;
        }
        let vaddr = u32::from_le_bytes(elf[p + 12..p + 16].try_into().unwrap());
        let off = u32::from_le_bytes(elf[p + 16..p + 20].try_into().unwrap()) as usize;
        let size = u32::from_le_bytes(elf[p + 20..p + 24].try_into().unwrap()) as usize;
        let seg = &elf[off..off + size];
        let mut a = 0usize;
        while a + 2 <= seg.len() {
            let addr = vaddr.wrapping_add(a as u32);
            let insn = decode(&seg[a..], addr);
            if let Op::Vector(v) = &insn.op {
                total += 1;
                if matches!(v.executable(), VecExec::NeedsVrf) {
                    let class = if v.mem { "mem" } else { "alu" };
                    dump.push_str(&format!("{addr:x} {class} sub-op {}\n", v.subop));
                } else {
                    ok += 1;
                }
            }
            a += (insn.len as usize).max(2);
        }
    }
    std::fs::write("/tmp/unresolved.txt", dump).unwrap();
    println!("total {total} executes {ok} unresolved {}", total - ok);
}
