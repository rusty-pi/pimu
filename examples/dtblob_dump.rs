fn main() {
    let b = std::fs::read("firmware/dt-blob.bin").unwrap();
    let m = rpi_virt_fw::firmware::dtblob::pin_map(&b, &["pins_4b"]).unwrap();
    let mut v: Vec<_> = m.iter().collect();
    v.sort();
    for (k, d) in v { println!("{k:30} {:?} {:?}", d.number, d.kind); }
    for n in ["DISPLAY_DSI_PORT","SDCARD_CONTROL_POWER","LEDS_PWR_OK","LEDS_DISK_ACTIVITY","BT_ON","LAN_RUN"] {
        println!("lookup {n:24} -> {:?}", m.get(n));
    }
}
