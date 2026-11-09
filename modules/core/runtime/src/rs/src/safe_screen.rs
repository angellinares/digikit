//! The safe-mode screen: three lines of text in a built-in 5x7 pixel font, drawn into the native 128 x 64 bitmap (column-major, the byte
//! of pixel (x, y), y = 0 at the top, is `8*x + (63-y)/8`, the bit is `7 - (63-y)%8`, as in docs/design/dkbs.md). Pure: no firmware call and no allocator. `dkrt_boot`
//! renders it once into a static buffer, and the intro task only blits that buffer (`intro::show_safe`).
use dkrt_sdk::*;

pub const FRAME_BYTES: usize = 1024;
const WIDTH: usize = 128;
const GLYPH_W: usize = 5;
/// Pixels from one character to the next at scale 1 (the glyph and one blank column).
const ADVANCE: usize = GLYPH_W + 1;

/// 5 columns per glyph, bit 0 is the top row, in the order of `GLYPH_ORDER`. A table and not a `match`: the compiler may turn a `match` into an
/// inline jump table in the code section, which the ISA check of the build cannot decode.
static GLYPHS: [[u8; GLYPH_W]; 38] = [
    [0x3E, 0x51, 0x49, 0x45, 0x3E], // 0
    [0x00, 0x42, 0x7F, 0x40, 0x00], // 1
    [0x42, 0x61, 0x51, 0x49, 0x46], // 2
    [0x21, 0x41, 0x45, 0x4B, 0x31], // 3
    [0x18, 0x14, 0x12, 0x7F, 0x10], // 4
    [0x27, 0x45, 0x45, 0x45, 0x39], // 5
    [0x3C, 0x4A, 0x49, 0x49, 0x30], // 6
    [0x01, 0x71, 0x09, 0x05, 0x03], // 7
    [0x36, 0x49, 0x49, 0x49, 0x36], // 8
    [0x06, 0x49, 0x49, 0x29, 0x1E], // 9
    [0x00, 0x36, 0x36, 0x00, 0x00], // :
    [0x08, 0x08, 0x08, 0x08, 0x08], // -
    [0x7E, 0x11, 0x11, 0x11, 0x7E], // A
    [0x7F, 0x49, 0x49, 0x49, 0x36], // B
    [0x3E, 0x41, 0x41, 0x41, 0x22], // C
    [0x7F, 0x41, 0x41, 0x41, 0x3E], // D
    [0x7F, 0x49, 0x49, 0x49, 0x41], // E
    [0x7F, 0x09, 0x09, 0x09, 0x01], // F
    [0x3E, 0x41, 0x49, 0x49, 0x7A], // G
    [0x7F, 0x08, 0x08, 0x08, 0x7F], // H
    [0x00, 0x41, 0x7F, 0x41, 0x00], // I
    [0x20, 0x40, 0x41, 0x3F, 0x01], // J
    [0x7F, 0x08, 0x14, 0x22, 0x41], // K
    [0x7F, 0x40, 0x40, 0x40, 0x40], // L
    [0x7F, 0x02, 0x0C, 0x02, 0x7F], // M
    [0x7F, 0x04, 0x08, 0x10, 0x7F], // N
    [0x3E, 0x41, 0x41, 0x41, 0x3E], // O
    [0x7F, 0x09, 0x09, 0x09, 0x06], // P
    [0x3E, 0x41, 0x51, 0x21, 0x5E], // Q
    [0x7F, 0x09, 0x19, 0x29, 0x46], // R
    [0x46, 0x49, 0x49, 0x49, 0x31], // S
    [0x01, 0x01, 0x7F, 0x01, 0x01], // T
    [0x3F, 0x40, 0x40, 0x40, 0x3F], // U
    [0x1F, 0x20, 0x40, 0x20, 0x1F], // V
    [0x3F, 0x40, 0x38, 0x40, 0x3F], // W
    [0x63, 0x14, 0x08, 0x14, 0x63], // X
    [0x07, 0x08, 0x70, 0x08, 0x07], // Y
    [0x61, 0x51, 0x49, 0x45, 0x43], // Z
];

fn glyph(c: u8) -> [u8; GLYPH_W] {
    let i = match c {
        b'0'..=b'9' => c - b'0',
        b':' => 10,
        b'-' => 11,
        b'A'..=b'Z' => c - b'A' + 12,
        _ => return [0; GLYPH_W], // space and anything else
    };
    GLYPHS[i as usize]
}

/// Width in pixels of `text` at `scale` (no blank column after the last character).
pub fn text_width(text: &str, scale: usize) -> usize {
    let n = text.len();
    if n == 0 {
        0
    } else {
        n * ADVANCE * scale - scale
    }
}

fn set(buf: &mut [u8; FRAME_BYTES], x: usize, y: usize) {
    if x < WIDTH && y < 64 {
        let v = 63 - y;
        buf[8 * x + v / 8] |= 1 << (7 - v % 8);
    }
}

/// `text` at row `y`, centred, at `scale`.
fn line(buf: &mut [u8; FRAME_BYTES], text: &str, y: usize, scale: usize) {
    let mut x0 = WIDTH.saturating_sub(text_width(text, scale)) / 2;
    for &c in text.as_bytes() {
        let g = glyph(c);
        for (col, bits) in g.iter().enumerate() {
            for row in 0..7 {
                if bits >> row & 1 != 0 {
                    for dx in 0..scale {
                        for dy in 0..scale {
                            set(buf, x0 + col * scale + dx, y + row * scale + dy);
                        }
                    }
                }
            }
        }
        x0 += ADVANCE * scale;
    }
}

/// The second line of the screen: why this boot is safe.
pub fn reason_text(reason: u32) -> &'static str {
    // a list and not a `match` (see `GLYPHS`)
    static REASONS: [(u32, &str); 6] = [
        (SAFE_KEY, "REC HELD"),
        (SAFE_COUNTER, "2 FAILED BOOTS"),
        (SAFE_FLAG, "SAFE ONCE"),
        (SAFE_DSP, "DSP FAULT"),
        (SAFE_PANIC, "CRASH"),
        (SAFE_READ, "SELECTION UNREADABLE"),
    ];
    let mut i = 0;
    while i < REASONS.len() {
        if REASONS[i].0 == reason {
            return REASONS[i].1;
        }
        i += 1;
    }
    ""
}

/// The whole frame for `reason`: black, `SAFE MODE` at 2x, the reason, `NO MODULES LOADED`.
pub fn render(reason: u32, buf: &mut [u8; FRAME_BYTES]) {
    *buf = [0; FRAME_BYTES];
    line(buf, "SAFE MODE", 10, 2);
    line(buf, reason_text(reason), 32, 1);
    line(buf, "NO MODULES LOADED", 46, 1);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lit(buf: &[u8; FRAME_BYTES], x: usize, y: usize) -> bool {
        buf[8 * x + (63 - y) / 8] >> (7 - (63 - y) % 8) & 1 != 0
    }

    fn frame(reason: u32) -> [u8; FRAME_BYTES] {
        let mut b = [0; FRAME_BYTES];
        render(reason, &mut b);
        b
    }

    #[test]
    fn width_is_six_columns_a_character_less_one_gap() {
        assert_eq!(text_width("", 1), 0);
        assert_eq!(text_width("A", 1), 5);
        assert_eq!(text_width("SAFE MODE", 2), 106);
        assert_eq!(text_width("SELECTION UNREADABLE", 1), 119);
        assert_eq!(text_width("NO MODULES LOADED", 1), 101);
    }

    #[test]
    fn every_reason_fits_on_the_screen() {
        for r in [SAFE_KEY, SAFE_COUNTER, SAFE_FLAG, SAFE_DSP, SAFE_PANIC, SAFE_READ] {
            assert!(!reason_text(r).is_empty());
            assert!(text_width(reason_text(r), 1) <= WIDTH);
        }
        assert_eq!(reason_text(SAFE_NONE), "");
    }

    #[test]
    fn known_pixels_of_the_title() {
        let b = frame(SAFE_KEY);
        // "SAFE MODE" at 2x: x0 = (128 - 106) / 2 = 11, y0 = 10. 'S' column 0 is 0x46: rows 1, 2 and 6 are lit.
        assert!(!lit(&b, 11, 10) && !lit(&b, 12, 11)); // row 0 of the glyph is dark
        assert!(lit(&b, 11, 12) && lit(&b, 12, 13) && lit(&b, 11, 13)); // row 1 at 2x covers y 12 and 13
        assert!(lit(&b, 11, 14) && lit(&b, 11, 22));
        assert!(!lit(&b, 11, 16)); // row 3 of column 0 is dark
        // the blank column after the glyph (x = 11 + 10 and 11)
        assert!(!lit(&b, 21, 14) && !lit(&b, 22, 14));
        // nothing outside the text rows: the top rows and the bottom rows are black
        for x in 0..WIDTH {
            assert!(!lit(&b, x, 0) && !lit(&b, x, 63));
        }
    }

    #[test]
    fn bit_layout_is_the_panels_vertical_order() {
        // measured on hardware: y = 0 is the top row, stored in the last byte of the column, bit 0
        let b = frame(SAFE_KEY);
        // first glyph column of the title, pixel (11, 12): byte 94, mask 0x10; pixel (11, 10) in the same byte is dark
        assert_ne!(b[94] & 0x10, 0);
        assert_eq!(b[94] & 0x04, 0);
        // the top-left corner (0, 0) is byte 7 mask 0x01 and the bottom-left (0, 63) is byte 0 mask 0x80: both dark
        assert_eq!(b[7] & 0x01, 0);
        assert_eq!(b[0] & 0x80, 0);
        // the lit title lies in the top-left area: some byte of columns 11..22, upper half of the screen
        assert!(b[8 * 11 + 4..8 * 11 + 8].iter().any(|&v| v != 0));
        assert!(b[8 * 11..8 * 11 + 4].iter().all(|&v| v == 0));
    }

    #[test]
    fn the_reason_changes_only_line_two() {
        let key = frame(SAFE_KEY);
        let counter = frame(SAFE_COUNTER);
        assert_ne!(key, counter);
        for x in 0..WIDTH {
            for y in (0..32).chain(40..64) {
                assert_eq!(lit(&key, x, y), lit(&counter, x, y), "({x},{y})");
            }
        }
        assert_ne!(frame(SAFE_PANIC), frame(SAFE_READ));
    }

    /// The device tests compare the intro framebuffer with these files (`tests/fixtures/safe-*.bin`). `DK_BLESS=1` rewrites them.
    #[test]
    fn fixtures_match_the_renderer() {
        for (name, reason) in [("safe-key.bin", SAFE_KEY), ("safe-counter.bin", SAFE_COUNTER)] {
            let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../../tests/fixtures/").to_string() + name;
            let want = frame(reason);
            if std::env::var("DK_BLESS").is_ok() {
                std::fs::write(&path, want).unwrap();
            }
            let got = std::fs::read(&path).unwrap_or_else(|_| panic!("{path} missing: run with DK_BLESS=1"));
            assert!(got == want, "{name} differs from the renderer: run with DK_BLESS=1");
        }
    }
}
