//! Front-panel LED state decoded from the UART8 TX stream (the firmware's own panel records).
//!
//! Framing is the one in `tools/emulator/panel_tx.py`: `0x10..=0x17 col d0..d7` (OLED page row, 9 data bytes),
//! `b8` (frame present), `b9 idx val` (LED set), `b5 idx r g b` (RGB LED), `b7 x`, `bb a b c`, `bc x`,
//! `0x40..=0x4f 0`; any other byte is skipped alone. A record may be split across drains, so the decoder
//! keeps the partly received record between calls. Decoding has no effect on emulation.

pub const LED_COUNT: usize = 64;

#[derive(Clone, Debug)]
pub struct PanelLeds {
    pub leds: [u8; LED_COUNT],
    pub rgb: [[u8; 3]; LED_COUNT],
    /// Bumped on each b9/b5 record that changes a value.
    pub revision: u32,
    opcode: u8,
    need: u8,
    len: u8,
    data: [u8; 9],
}

impl Default for PanelLeds {
    fn default() -> Self {
        Self {
            leds: [0; LED_COUNT],
            rgb: [[0; 3]; LED_COUNT],
            revision: 0,
            opcode: 0,
            need: 0,
            len: 0,
            data: [0; 9],
        }
    }
}

fn data_len(opcode: u8) -> Option<u8> {
    match opcode {
        0x10..=0x17 => Some(9),
        0xb8 => Some(0),
        0xb9 => Some(2),
        0xb5 => Some(4),
        0xb7 | 0xbc => Some(1),
        0xbb => Some(3),
        0x40..=0x4f => Some(1),
        _ => None,
    }
}

impl PanelLeds {
    pub fn feed(&mut self, bytes: &[u8]) {
        for &byte in bytes {
            if self.need == 0 {
                match data_len(byte) {
                    None => {}
                    Some(0) => {}
                    Some(n) => {
                        self.opcode = byte;
                        self.need = n;
                        self.len = 0;
                    }
                }
                continue;
            }
            self.data[self.len as usize] = byte;
            self.len += 1;
            self.need -= 1;
            if self.need == 0 {
                self.apply();
            }
        }
    }

    fn apply(&mut self) {
        let d = self.data;
        let idx = d[0] as usize;
        match self.opcode {
            0xb9 if idx < LED_COUNT => {
                if self.leds[idx] != d[1] {
                    self.leds[idx] = d[1];
                    self.revision = self.revision.wrapping_add(1);
                }
            }
            0xb5 if idx < LED_COUNT => {
                let rgb = [d[1], d[2], d[3]];
                if self.rgb[idx] != rgb {
                    self.rgb[idx] = rgb;
                    self.revision = self.revision.wrapping_add(1);
                }
            }
            _ => {}
        }
    }

    /// `leds` as 128 lowercase hex digits.
    pub fn leds_hex(&self) -> String {
        hex(self.leds.iter().copied())
    }

    /// `rgb` as 384 hex digits, or `None` while no RGB LED has been set.
    pub fn rgb_hex(&self) -> Option<String> {
        self.rgb
            .iter()
            .any(|c| *c != [0; 3])
            .then(|| hex(self.rgb.iter().flatten().copied()))
    }
}

fn hex(bytes: impl Iterator<Item = u8>) -> String {
    bytes.map(|b| format!("{b:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn applies_b9_and_b5() {
        let mut p = PanelLeds::default();
        p.feed(&[0xb9, 0x19, 0x71, 0xb5, 0x05, 1, 2, 3]);
        assert_eq!(p.leds[0x19], 0x71);
        assert_eq!(p.rgb[5], [1, 2, 3]);
        assert_eq!(p.revision, 2);
        p.feed(&[0xb9, 0x19, 0x71]);
        assert_eq!(p.revision, 2, "unchanged value does not bump");
    }

    #[test]
    fn records_split_across_drains() {
        let mut p = PanelLeds::default();
        p.feed(&[0xb9]);
        p.feed(&[0x1a]);
        assert_eq!(p.revision, 0);
        p.feed(&[0x11, 0xb5, 0x02]);
        p.feed(&[9, 8]);
        p.feed(&[7]);
        assert_eq!(p.leds[0x1a], 0x11);
        assert_eq!(p.rgb[2], [9, 8, 7]);
    }

    #[test]
    fn oled_rows_and_other_records_are_skipped() {
        let mut p = PanelLeds::default();
        p.feed(&[0x13, 0x00, 0xb9, 0x03, 0xff, 0xb9, 0x04, 0xb9, 0x05, 0x00]);
        p.feed(&[
            0xb8, 0xb7, 0xb9, 0xbb, 0xb9, 0x01, 0xb9, 0xbc, 0xb9, 0x43, 0x00,
        ]);
        p.feed(&[0x00, 0xb9, 0xff, 0x05, 0xb9, 0x07, 0x21]);
        assert_eq!(p.leds[0x07], 0x21);
        assert_eq!(p.leds[3], 0);
        assert_eq!(p.leds[4], 0);
        assert_eq!(p.revision, 1);
    }

    #[test]
    fn hex_forms() {
        let mut p = PanelLeds::default();
        assert_eq!(p.leds_hex().len(), 128);
        assert!(p.rgb_hex().is_none());
        p.feed(&[0xb5, 0, 1, 0, 0]);
        assert_eq!(p.rgb_hex().unwrap().len(), 384);
    }
}
