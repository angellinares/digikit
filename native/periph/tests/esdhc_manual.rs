//! eSDHC early register tests against MCF5441xRM chapter 25.

use periph::esdhc::{self, CardPort, Esdhc, RegisterPolicy};

#[derive(Default)]
struct Card {
    last: Option<(u8, u32)>,
}
impl CardPort for Card {
    fn command(&mut self, idx: u8, arg: u32) -> [u32; 4] {
        self.last = Some((idx, arg));
        [idx as u32, arg, 3, 4]
    }
    fn read_word(&mut self, _idx: u8, pattern: u32) -> u32 {
        !pattern
    }
    fn data_for(&mut self, _idx: u8, _arg: u32, _len: usize) -> Option<Vec<u8>> {
        None
    }
    fn write_data(&mut self, _idx: u8, _arg: u32, _payload: &[u8]) {}
}

#[test]
fn reset_values_table_25_2_and_inserted_card() {
    let mut h = Esdhc::new(Card::default());
    assert_eq!(h.read(esdhc::BASE + esdhc::PRSSTAT, 4), Some(0xFF89_00F8));
    assert_eq!(h.read(esdhc::BASE + esdhc::SYSCTL, 4), Some(0x0000_8008));
    assert_eq!(h.read(esdhc::BASE + 0x34, 4), Some(0x117F_013F));
}

#[test]
fn sysctl_resets_self_clear() {
    let mut h = Esdhc::new(Card::default());
    h.write(esdhc::BASE + esdhc::SYSCTL, 4, 0x0F00_1058);
    assert_eq!(h.read(esdhc::BASE + esdhc::SYSCTL, 4), Some(0x0000_1058));
}

#[test]
fn oracle_irqstat_guest_write_round_trips() {
    // `emu.esdhc.Esdhc` installs no IRQSTAT write hook, so its guest store
    // remains plain register-file data despite the device's W1C behavior.
    let mut h = Esdhc::new(Card::default());
    h.write(esdhc::BASE + esdhc::IRQSTAT, 4, 0xA5A5_0003);
    assert_eq!(h.read(esdhc::BASE + esdhc::IRQSTAT, 4), Some(0xA5A5_0003));
}

#[test]
fn device_irqstat_guest_write_is_w1c() {
    let mut h = Esdhc::with_policy(Card::default(), RegisterPolicy::Device);
    h.write(esdhc::BASE + esdhc::XFERTYP, 4, 0); // command completion sets CC|TC
    h.write(esdhc::BASE + esdhc::IRQSTAT, 4, 1);
    assert_eq!(h.read(esdhc::BASE + esdhc::IRQSTAT, 4), Some(2));
}

#[test]
fn xfertyp_write_issues_command_and_marks_data_ready() {
    let mut h = Esdhc::new(Card::default());
    h.write(esdhc::BASE + esdhc::CMDARG, 4, 0x1234_0000);
    // CMD14, data present and card-to-host (RM XFERTYP command index/data bits).
    h.write(esdhc::BASE + esdhc::XFERTYP, 4, 0x0E3A_0010);
    assert_eq!(h.read(esdhc::BASE + esdhc::CMDRSP0, 4), Some(14));
    assert_eq!(h.read(esdhc::BASE + esdhc::CMDRSP1, 4), Some(0x1234_0000));
    assert_eq!(h.read(esdhc::BASE + esdhc::DATPORT, 4), Some(0xFFFF_FFFF));
    assert_eq!(
        h.read(esdhc::BASE + esdhc::PRSSTAT, 4).unwrap() & (1 << 11),
        1 << 11
    );
    assert_eq!(
        h.read(esdhc::BASE + esdhc::IRQSTAT, 4).unwrap() & 0x23,
        0x23
    );
}
