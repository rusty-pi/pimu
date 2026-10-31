//! What a reset leaves for the next boot: the card keeps what the guest wrote
//! to it, in the command line and in the browser build alike, since both carry
//! a machine over with [`Carry`].

use pimu::boot::{Carry, FlashEdits};
use pimu::machine::Machine;
use pimu::periph::disk::Disk;

fn machine_with_card(image: Vec<u8>) -> Machine {
    let mut machine = Machine::new(1 << 20);
    machine.emmc2.insert_disk(Disk::from_vec(image));
    machine
}

fn block(machine: &mut Machine, lba: u32) -> [u8; 512] {
    let mut out = [0u8; 512];
    machine
        .emmc2
        .card_slot()
        .as_ref()
        .unwrap()
        .read_block(lba, &mut out);
    out
}

#[test]
fn the_card_keeps_what_the_guest_wrote_through_a_reset() {
    let edits = FlashEdits::new(false, false, None, &[], false, None).unwrap();
    let mut first = machine_with_card(vec![0xaa; 4096]);
    first
        .emmc2
        .card_slot()
        .as_mut()
        .unwrap()
        .write_block(3, &[0x55; 512]);

    let mut carry = Carry::leave(&mut first, &edits);
    let mut second = machine_with_card(vec![0xaa; 4096]);
    assert_eq!(
        block(&mut second, 3),
        [0xaa; 512],
        "a card built for the boot has not been written to"
    );
    carry.restore(&mut second, &[]);

    assert_eq!(block(&mut second, 3), [0x55; 512], "the write is there");
    assert_eq!(block(&mut second, 2), [0xaa; 512], "the rest is the image");
}

#[test]
fn the_card_keeps_it_through_more_than_one_reset() {
    let edits = FlashEdits::new(false, false, None, &[], false, None).unwrap();
    let mut machine = machine_with_card(vec![0; 4096]);
    for boot in 1..=3u8 {
        machine
            .emmc2
            .card_slot()
            .as_mut()
            .unwrap()
            .write_block(u32::from(boot), &[boot; 512]);
        let mut carry = Carry::leave(&mut machine, &edits);
        machine = machine_with_card(vec![0; 4096]);
        carry.restore(&mut machine, &[]);
    }
    for boot in 1..=3u8 {
        assert_eq!(block(&mut machine, u32::from(boot)), [boot; 512]);
    }
}

#[test]
fn a_machine_without_a_card_gets_none() {
    let edits = FlashEdits::new(false, false, None, &[], false, None).unwrap();
    let mut first = Machine::new(1 << 20);
    let mut carry = Carry::leave(&mut first, &edits);
    let mut second = Machine::new(1 << 20);
    carry.restore(&mut second, &[]);
    assert!(second.emmc2.card_slot().is_none());
}
