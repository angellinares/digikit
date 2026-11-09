//! The last fault of the device (docs/findings/ the fault trace): a CPU crash or a DSP stop, kept in the settings store as the key `dk/fault` so that it
//! survives power cycles, and the ABOUT rows made from it. Pure code: the record format and the row text, no hardware.
//!
//! The settings value is 40 bytes, big-endian words:
//!   0 `FLT1`   1 kind (1 CPU crash, 2 DSP stopped) | site << 8 | core step << 16   2 id (DSP: machine type, or the row's byte offset in the effect table; 0 for a CPU crash)
//!   3 module step (the number the module stored with `DK_STEP`; 0 none)   4 the program counter (CPU crash), or the raw `last` word of the DSP site
//!   5 runtime version   6..9 the name of the module in which it happened (16 bytes, NUL padded; empty: none)
//! The core step is the DSP table's stage: 4 the core is about to call the module (CALL), 5 it returned (RET).

pub const KEY: &[u8] = b"dk/fault";
pub const MAGIC: u32 = 0x464c_5431; // "FLT1"
pub const BYTES: usize = 40;

pub const KIND_CPU: u32 = 1;
pub const KIND_DSP: u32 = 2;
pub const KIND_PANIC: u32 = 3;

pub const SITE_NONE: u32 = 0;
pub const SITE_VOICE: u32 = 1;
pub const SITE_FX_MASTER: u32 = 2;
pub const SITE_FX_SEND: u32 = 3;

pub const STAGE_CALL: u32 = 4;
pub const STAGE_RET: u32 = 5;

/// Characters of an ABOUT row (the menu shows 22).
pub const ROW: usize = 18;
pub const ROWS: usize = 4;
/// Bytes of a row in `DkrtDiag.fault_text`: the text and a NUL.
pub const ROW_BYTES: usize = 24;

#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Fault {
    pub kind: u32,
    pub site: u32,
    pub stage: u32,
    pub id: u32,
    pub step: u32,
    /// The PC of a CPU crash; the raw `last` word of the site for a DSP stop.
    pub at: u32,
    pub runtime: u32,
    pub name: [u8; 16],
}

impl Fault {
    pub const fn empty() -> Fault {
        Fault {
            kind: 0,
            site: 0,
            stage: 0,
            id: 0,
            step: 0,
            at: 0,
            runtime: 0,
            name: [0; 16],
        }
    }

    pub fn encode(&self) -> [u8; BYTES] {
        let mut b = [0u8; BYTES];
        let w = [
            MAGIC,
            self.kind & 0xff | (self.site & 0xff) << 8 | (self.stage & 0xff) << 16,
            self.id,
            self.step,
            self.at,
            self.runtime,
        ];
        let mut i = 0;
        while i < 6 {
            b[4 * i..4 * i + 4].copy_from_slice(&w[i].to_be_bytes());
            i += 1;
        }
        b[24..40].copy_from_slice(&self.name);
        b
    }

    pub fn decode(b: &[u8]) -> Option<Fault> {
        if b.len() < BYTES {
            return None;
        }
        let w = |i: usize| u32::from_be_bytes([b[4 * i], b[4 * i + 1], b[4 * i + 2], b[4 * i + 3]]);
        if w(0) != MAGIC {
            return None;
        }
        let k = w(1);
        let kind = k & 0xff;
        if kind != KIND_CPU && kind != KIND_DSP && kind != KIND_PANIC {
            return None;
        }
        let mut name = [0u8; 16];
        name.copy_from_slice(&b[24..40]);
        Some(Fault {
            kind,
            site: (k >> 8) & 0xff,
            stage: (k >> 16) & 0xff,
            id: w(2),
            step: w(3),
            at: w(4),
            runtime: w(5),
            name,
        })
    }
}

/// The module name as shown: after the last `/`, upper case, at most `max` characters.
fn name_text(name: &[u8; 16], out: &mut Row, max: usize) {
    let mut n = 0;
    while n < 16 && name[n] != 0 {
        n += 1;
    }
    let mut start = 0;
    let mut i = 0;
    while i < n {
        if name[i] == b'/' {
            start = i + 1;
        }
        i += 1;
    }
    let mut k = start;
    while k < n && k - start < max {
        out.c(name[k].to_ascii_uppercase());
        k += 1;
    }
}

/// A row being built: at most `ROW` characters, the rest is dropped.
pub struct Row {
    pub b: [u8; ROW_BYTES],
    pub n: usize,
}

impl Row {
    pub const fn new() -> Row {
        Row {
            b: [0; ROW_BYTES],
            n: 0,
        }
    }
    pub fn c(&mut self, c: u8) {
        if self.n < ROW {
            self.b[self.n] = c;
            self.n += 1;
        }
    }
    pub fn s(&mut self, s: &str) {
        for c in s.bytes() {
            self.c(c);
        }
    }
    pub fn num(&mut self, mut v: u32) {
        let mut d = [0u8; 10];
        let mut n = 0;
        loop {
            d[n] = b'0' + (v % 10) as u8;
            n += 1;
            v /= 10;
            if v == 0 {
                break;
            }
        }
        while n > 0 {
            n -= 1;
            self.c(d[n]);
        }
    }
    pub fn hex8(&mut self, v: u32) {
        let mut i = 8;
        while i > 0 {
            i -= 1;
            let nib = ((v >> (4 * i)) & 15) as u8;
            self.c(if nib < 10 {
                b'0' + nib
            } else {
                b'A' + nib - 10
            });
        }
    }
}

/// The text of the core step when the module stored no step of its own.
fn core_step(stage: u32) -> &'static str {
    match stage {
        STAGE_CALL => "CALL",
        STAGE_RET => "RET",
        1 => "ENTER",
        _ => "?",
    }
}

/// The ABOUT rows of a fault (`FAULT: ...`, `IN <MODULE>`, and the place: `PC` of a CPU crash, or the site and the step of a DSP stop).
/// Every row is at most `ROW` (18) characters: ABOUT rows are cut at about 18 characters on DT2.
/// Returns the rows made (0 to 3; a panic makes 2).
pub fn rows(f: &Fault, out: &mut [Row; ROWS]) -> usize {
    let mut r0 = Row::new();
    r0.s(match f.kind {
        KIND_CPU => "FAULT: CPU CRASH",
        KIND_PANIC => "FAULT: CPU PANIC",
        _ => "FAULT: DSP STOP",
    });
    out[0] = r0;
    let mut r1 = Row::new();
    if f.name[0] != 0 {
        r1.s("IN ");
        name_text(&f.name, &mut r1, ROW - 3);
    } else if f.kind == KIND_CPU {
        r1.s("IN FIRMWARE");
    } else {
        r1.s("IN UNKNOWN");
    }
    out[1] = r1;
    if f.kind == KIND_PANIC {
        // the panic has no PC: the stack is in the log
        return 2;
    }
    let mut r2 = Row::new();
    if f.kind == KIND_CPU {
        r2.s("PC ");
        r2.hex8(f.at);
    } else {
        match f.site {
            SITE_VOICE => r2.s("VOICE"),
            SITE_FX_MASTER => r2.s("FX MASTER"),
            SITE_FX_SEND => r2.s("FX SEND"),
            _ => r2.s("SITE UNKNOWN"),
        }
        if f.site != SITE_NONE {
            r2.s(" ");
            if f.step != 0 {
                r2.s("STEP ");
                r2.num(f.step);
            } else {
                r2.s(core_step(f.stage));
            }
        }
    }
    out[2] = r2;
    3
}

/// Whether an event of the log goes into the tail of the crash record (the last events before a crash or a stop). The periodic info events
/// `HEALTH` and `DSP_DIAG` do not: a burst of them can fill the whole tail and push out the boot's `MOD_LOAD` and `MOD_DSP` results. An error
/// flagged event of these kinds stays, and so does every other kind.
pub fn tail_keeps(kind: u16, flags: u16) -> bool {
    use crate::gen_sdk::logk;
    (kind != logk::HEALTH && kind != logk::DSP_DIAG) || flags & logk::F_ERROR != 0
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text(r: &Row) -> &str {
        core::str::from_utf8(&r.b[..r.n]).unwrap()
    }

    fn name(s: &str) -> [u8; 16] {
        let mut n = [0u8; 16];
        n[..s.len()].copy_from_slice(s.as_bytes());
        n
    }

    fn rows_of(f: &Fault) -> [Row; ROWS] {
        let mut out = [Row::new(), Row::new(), Row::new(), Row::new()];
        assert_eq!(rows(f, &mut out), 3);
        out
    }

    #[test]
    fn a_record_decodes_to_what_was_encoded() {
        let f = Fault {
            kind: KIND_DSP,
            site: SITE_FX_SEND,
            stage: STAGE_CALL,
            id: 96,
            step: 3,
            at: 0x0002_0004,
            runtime: 0x0005_0300,
            name: name("test/dyn-fx-hang"),
        };
        assert_eq!(Fault::decode(&f.encode()), Some(f));
    }

    #[test]
    fn a_value_that_is_not_a_fault_record_is_refused() {
        let mut b = Fault {
            kind: KIND_CPU,
            ..Fault::empty()
        }
        .encode();
        assert!(Fault::decode(&b[..BYTES - 1]).is_none());
        b[0] ^= 1;
        assert!(Fault::decode(&b).is_none());
        // a kind that is neither
        assert!(Fault::decode(
            &Fault {
                kind: 7,
                ..Fault::empty()
            }
            .encode()
        )
        .is_none());
    }

    #[test]
    fn a_dsp_stop_in_a_send_effect_names_the_module_site_and_step() {
        let f = Fault {
            kind: KIND_DSP,
            site: SITE_FX_SEND,
            stage: STAGE_CALL,
            step: 3,
            name: name("test/dyn-fx-hang"),
            ..Fault::empty()
        };
        let r = rows_of(&f);
        assert_eq!(
            (text(&r[0]), text(&r[1]), text(&r[2])),
            ("FAULT: DSP STOP", "IN DYN-FX-HANG", "FX SEND STEP 3")
        );
    }

    #[test]
    fn without_a_module_step_the_row_shows_the_core_step() {
        let f = Fault {
            kind: KIND_DSP,
            site: SITE_FX_MASTER,
            stage: STAGE_CALL,
            name: name("fx/gain"),
            ..Fault::empty()
        };
        assert_eq!(text(&rows_of(&f)[2]), "FX MASTER CALL");
        let f = Fault {
            kind: KIND_DSP,
            site: SITE_VOICE,
            stage: STAGE_RET,
            step: 0,
            name: name("test/hang-d"),
            ..Fault::empty()
        };
        assert_eq!(text(&rows_of(&f)[2]), "VOICE RET");
        let f = Fault {
            kind: KIND_DSP,
            site: SITE_VOICE,
            stage: STAGE_CALL,
            step: 12,
            ..Fault::empty()
        };
        let r = rows_of(&f);
        assert_eq!((text(&r[1]), text(&r[2])), ("IN UNKNOWN", "VOICE STEP 12"));
    }

    #[test]
    fn a_cpu_crash_shows_the_pc_and_a_long_name_is_cut_to_the_row() {
        let f = Fault {
            kind: KIND_CPU,
            at: 0x0012_a4f0,
            name: name("author/abcdefghi"),
            ..Fault::empty()
        };
        let r = rows_of(&f);
        assert_eq!(text(&r[0]), "FAULT: CPU CRASH");
        assert_eq!(text(&r[2]), "PC 0012A4F0");
        assert_eq!(text(&r[1]), "IN ABCDEFGHI");
        let f = Fault {
            kind: KIND_CPU,
            at: 0x4001_0000,
            ..Fault::empty()
        };
        assert_eq!(text(&rows_of(&f)[1]), "IN FIRMWARE");
    }

    #[test]
    fn a_panic_names_the_runtime_and_has_no_pc_row() {
        let f = Fault {
            kind: KIND_PANIC,
            name: name("runtime"),
            ..Fault::empty()
        };
        assert_eq!(Fault::decode(&f.encode()), Some(f));
        let mut out = [Row::new(), Row::new(), Row::new(), Row::new()];
        assert_eq!(rows(&f, &mut out), 2);
        assert_eq!(
            (text(&out[0]), text(&out[1])),
            ("FAULT: CPU PANIC", "IN RUNTIME")
        );
    }

    #[test]
    fn no_row_is_longer_than_a_menu_row() {
        let f = Fault {
            kind: KIND_DSP,
            site: SITE_FX_MASTER,
            stage: STAGE_CALL,
            step: 4_000_000_000,
            name: name("aaaaaaaaaaaaaaaa"),
            ..Fault::empty()
        };
        for r in rows_of(&f).iter().take(3) {
            assert!(r.n <= ROW);
        }
    }

    #[test]
    fn the_longest_row_fits_the_about_page_of_the_dt2() {
        let f = Fault {
            kind: KIND_DSP,
            site: SITE_FX_MASTER,
            stage: STAGE_CALL,
            step: 255,
            name: name("aaaaaaaaaaaaaaaa"),
            ..Fault::empty()
        };
        let r = rows_of(&f);
        assert_eq!(text(&r[2]), "FX MASTER STEP 255");
        for row in r.iter().take(3) {
            assert!(row.n <= 18);
        }
    }

    #[test]
    fn the_tail_leaves_out_periodic_info_events() {
        use crate::gen_sdk::logk;
        assert!(!tail_keeps(logk::DSP_DIAG, 0));
        assert!(!tail_keeps(logk::HEALTH, 0));
        assert!(tail_keeps(logk::DSP_DIAG, logk::F_ERROR));
        assert!(tail_keeps(logk::HEALTH, logk::F_ERROR));
        assert!(tail_keeps(logk::MOD_LOAD, 0));
        assert!(tail_keeps(logk::MOD_DSP, 0));
    }
}
