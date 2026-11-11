//! The fit rule: does a module, from the needs inferred from its `.dkm` (`dkneeds`), fit the capacity of the device (docs/plans/what-fits.md)?
//!
//! The runtime's CAPACITY entries (`dkrt_capacity`) give limit, used and largest free piece per resource; `dkneeds::needs_of` (docs/design/digikit-dkm.md,
//! "Needs are inferred from the contents") gives what a load takes. `fits_now` compares them and names the first short resource; `take` takes a module's needs out of a capacity,
//! so a set is checked by loading its modules in turn (the sum rule: fragmentation is not modelled, so it is conservative). The rule exists only here:
//! the web app uses it through patchkit.wasm and the device tools through `patchkit fit` (packages/patchkit/src/fit.rs).
//! `modules/core/runtime/vectors/fit-vectors.json` holds its test vectors.
//!
//! The runtime's module set is fixed per session, so the menu checks a selection against the limits with the other ticked modules taken out
//! (the next boot loads them into an empty runtime), and a host checks against CAPACITY as the device reports it.
#![no_std]

use dkrt_sdk::proto::*;
use dkrt_sdk::{DkrtCap, DkrtFitOut, DkrtScan, SCAN_COMPAT, SCAN_MAX, SCAN_NEEDS};

pub use dkneeds::{Needs, ARENA_HDR};

/// The first short resource: its CAPACITY kind (`CAPK_*`), what the module needs and what is free (bytes, or a count).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Short {
    pub kind: u8,
    pub need: u32,
    pub free: u32,
}

/// The order the first short resource is looked for in: the loader's own order of refusals, with the slots first.
const ORDER: [u8; 10] = [
    CAPK_STAGE,
    CAPK_MODULES,
    CAPK_SELECTION,
    CAPK_DSP_AREA,
    CAPK_ARENA,
    CAPK_VOICE_ROWS,
    CAPK_MACHINE_ROWS,
    CAPK_PARAM_ROWS,
    CAPK_PAGE_ROWS,
    CAPK_FX_SLOTS,
];

/// The entry of `kind`; None when the capacity has none (an older runtime without that kind): it is not judged.
fn cap(caps: &[DkrtCap], kind: u8) -> Option<DkrtCap> {
    for c in caps {
        if c.kind == kind as u32 {
            return Some(*c);
        }
    }
    None
}

fn free(c: &DkrtCap) -> u32 {
    c.limit.saturating_sub(c.used)
}

/// None when the module fits `caps`, else the first short resource.
pub fn fits_now(n: &Needs, caps: &[DkrtCap]) -> Option<Short> {
    for kind in ORDER {
        let Some(c) = cap(caps, kind) else {
            continue;
        };
        // an `if` chain, not a `match`: dense arms become a PC-relative jump table, which is not ColdFire code
        let (need, room) = if kind == CAPK_STAGE {
            (n.file_size, c.largest)
        } else if kind == CAPK_MODULES || kind == CAPK_SELECTION {
            (1, free(&c))
        } else if kind == CAPK_DSP_AREA {
            (n.dsp_need(), free(&c).min(c.largest))
        } else if kind == CAPK_ARENA {
            // the module's block and the block of its DSP image are in the arena together until the transfer ends
            let total = n.arena.saturating_add(n.arena_xfer);
            if total > free(&c) {
                return Some(Short {
                    kind,
                    need: total,
                    free: free(&c),
                });
            }
            (n.arena.max(n.arena_xfer), c.largest + ARENA_HDR)
        } else if kind == CAPK_VOICE_ROWS {
            (n.voice_rows, free(&c))
        } else if kind == CAPK_MACHINE_ROWS {
            (n.machine_rows, free(&c))
        } else if kind == CAPK_PARAM_ROWS {
            (n.param_rows, free(&c))
        } else if kind == CAPK_PAGE_ROWS {
            (n.page_rows, free(&c))
        } else {
            (n.fx_slots, free(&c))
        };
        if need > room {
            return Some(Short {
                kind,
                need,
                free: room,
            });
        }
    }
    None
}

/// Make `caps` the capacity of a runtime that has loaded nothing: every resource unused and its whole limit free, as the next boot sees it (the
/// module set is fixed per session). The arena starts one block header short of its limit and the DSP area 8 bytes short (its first range starts at
/// an offset that is not 8-aligned); the staging buffer is not a resource that a load uses up.
pub fn empty(caps: &mut [DkrtCap]) {
    for c in caps.iter_mut() {
        if c.kind == CAPK_STAGE as u32 {
            continue;
        }
        c.used = 0;
        c.largest = if c.kind == CAPK_ARENA as u32 {
            c.limit.saturating_sub(ARENA_HDR)
        } else if c.kind == CAPK_DSP_AREA as u32 {
            c.limit.saturating_sub(dkneeds::DSP_SLACK)
        } else {
            c.limit
        };
    }
}

/// Take the module's needs out of `caps` (what is left once it is loaded). An entry the capacity lacks is ignored.
pub fn take(n: &Needs, caps: &mut [DkrtCap]) {
    let mut use_ = |kind: u8, amount: u32| {
        for c in caps.iter_mut() {
            if c.kind == kind as u32 {
                c.used = c.used.saturating_add(amount);
                c.largest = c.largest.saturating_sub(amount);
            }
        }
    };
    use_(CAPK_MODULES, 1);
    use_(CAPK_SELECTION, 1);
    use_(CAPK_ARENA, n.arena.saturating_add(n.arena_xfer));
    use_(CAPK_DSP_AREA, n.dsp_need());
    use_(CAPK_VOICE_ROWS, n.voice_rows);
    use_(CAPK_MACHINE_ROWS, n.machine_rows);
    use_(CAPK_PARAM_ROWS, n.param_rows);
    use_(CAPK_PAGE_ROWS, n.page_rows);
    use_(CAPK_FX_SLOTS, n.fx_slots);
}

/// Load the modules of a set in order: None when all fit, else the index of the first that does not and what it is short of. `caps` is left as
/// the capacity after the modules that fit.
pub fn fits_set(set: &[Needs], caps: &mut [DkrtCap]) -> Option<(usize, Short)> {
    for (i, n) in set.iter().enumerate() {
        if let Some(s) = fits_now(n, caps) {
            return Some((i, s));
        }
        take(n, caps);
    }
    None
}

/// The resources the load line shows, in this order, with the label the device shows: the two a user can act on (DSP memory, CPU memory).
/// The module records, the row tables, the staging buffer, the selection paths, the voice rows and the effect slots are not shown.
const LINE: [(u8, &[u8]); 2] = [(CAPK_DSP_AREA, b"DSP"), (CAPK_ARENA, b"CPU")];
/// The longest load line: two labels of three letters, two percents of four characters and a space between.
pub const LINE_MAX: usize = 17;

/// `used` as a percent of `limit`, rounded up: a set that takes anything never reads 0 %, and a resource reads 100 % only when it is full.
/// (u32 arithmetic only: the limits of these resources are at most 2^19, so `used * 100` fits; the module has no 64-bit helpers.)
fn percent(used: u32, limit: u32) -> u32 {
    used.min(limit)
        .saturating_mul(100)
        .saturating_add(limit - 1)
        / limit
}

/// The load line of a selection (the MODULES page, above APPLY & RESTART): `DSP 52% CPU 1%`, always the DSP memory and then the CPU memory of
/// `caps`, which is the capacity once the ticked modules are taken out of an empty one (`empty`, `take`). A resource the capacity lacks or that has
/// no limit is left out. Writes into `out` and returns the length (0 when the capacity has neither).
pub fn load_line(caps: &[DkrtCap], out: &mut [u8; LINE_MAX]) -> usize {
    let mut n = 0;
    let mut put = |bytes: &[u8]| {
        for &b in bytes {
            out[n] = b;
            n += 1;
        }
    };
    let mut first = true;
    for &(kind, label) in LINE.iter() {
        let Some(c) = cap(caps, kind) else {
            continue;
        };
        if c.limit == 0 {
            continue;
        }
        if !first {
            put(b" ");
        }
        first = false;
        put(label);
        put(b" ");
        let p = percent(c.used, c.limit);
        if p >= 100 {
            put(b"100");
        } else {
            if p >= 10 {
                put(&[b'0' + (p / 10) as u8]);
            }
            put(&[b'0' + (p % 10) as u8]);
        }
        put(b"%");
    }
    n
}

// ---- the verdict of a scanned list (`dkrt_fit`) and its texts ---------------------------------------------------------------

/// The needs a `DkrtScan` entry carries (valid when `SCAN_NEEDS` is set).
pub fn scan_needs(e: &DkrtScan) -> Needs {
    Needs {
        file_size: e.need_file_size,
        arena: e.need_arena,
        arena_xfer: e.need_arena_xfer,
        dsp_area: e.need_dsp_area,
        dsp_align: e.need_dsp_align,
        machine_rows: e.need_machine_rows,
        voice_rows: e.need_voice_rows,
        param_rows: e.need_param_rows,
        page_rows: e.need_page_rows,
        fx_slots: e.need_fx_slots,
    }
}

/// Put `n` into the `need_*` fields of `e` and set `SCAN_NEEDS`.
pub fn set_scan_needs(e: &mut DkrtScan, n: &Needs) {
    e.need_file_size = n.file_size;
    e.need_arena = n.arena;
    e.need_arena_xfer = n.arena_xfer;
    e.need_dsp_area = n.dsp_area;
    e.need_dsp_align = n.dsp_align;
    e.need_machine_rows = n.machine_rows;
    e.need_voice_rows = n.voice_rows;
    e.need_param_rows = n.param_rows;
    e.need_page_rows = n.page_rows;
    e.need_fx_slots = n.fx_slots;
    e.flags |= SCAN_NEEDS;
}

/// A text written into a zero-filled buffer; it keeps the last byte 0 and drops what does not fit.
struct Out<'a> {
    b: &'a mut [u8],
    n: usize,
}

impl Out<'_> {
    fn s(&mut self, t: &[u8]) -> &mut Self {
        for &c in t {
            if self.n + 1 < self.b.len() {
                self.b[self.n] = c;
                self.n += 1;
            }
        }
        self
    }
    fn num(&mut self, mut v: u32) -> &mut Self {
        let mut tmp = [0u8; 10];
        let mut k = 0;
        loop {
            tmp[k] = b'0' + (v % 10) as u8;
            v /= 10;
            k += 1;
            if v == 0 {
                break;
            }
        }
        while k > 0 {
            k -= 1;
            self.s(&tmp[k..k + 1]);
        }
        self
    }
}

/// `n` bytes in K for a row: `<1K`, `259K`, `1.2M`.
pub fn size_k(out: &mut [u8], n: u32) {
    let mut t = Out { b: out, n: 0 };
    if n < 1024 {
        t.s(b"<1K");
    } else if n.div_ceil(1024) < 1000 {
        t.num(n.div_ceil(1024)).s(b"K");
    } else {
        let tenth = (n / 1024 * 10) / 1024;
        t.num(tenth / 10).s(b".").num(tenth % 10).s(b"M");
    }
}

/// The names of the CAPACITY kinds a fit can be short of, by kind number (0 and the kinds that cannot be short: empty).
const SHORT_NAME: [&str; 13] = [
    "", "MODULES", "SELECTED", "CPU", "FILE", "DSP", "MACHINES", "VOICES", "PARAMS", "PAGES", "FX",
    "", "",
];

/// What is short, in K for bytes and as numbers for rows and slots (the need rounded up, what is free rounded down): `DSP 400K > 254K FREE`, in a
/// popup; `compact` for a list row: `DSP 400K>254K`. `out` is zero-filled first.
pub fn short_text(out: &mut [u8], s: &Short, compact: bool) {
    for b in out.iter_mut() {
        *b = 0;
    }
    let mut t = Out { b: out, n: 0 };
    let k = s.kind as usize;
    t.s(if k < 13 {
        SHORT_NAME[k].as_bytes()
    } else {
        b""
    })
    .s(b" ");
    let bytes = s.kind == CAPK_ARENA || s.kind == CAPK_DSP_AREA || s.kind == CAPK_STAGE;
    let gt: &[u8] = if compact { b">" } else { b" > " };
    if bytes {
        t.num(s.need.div_ceil(1024))
            .s(b"K")
            .s(gt)
            .num(s.free / 1024)
            .s(b"K");
    } else {
        t.num(s.need).s(gt).num(s.free);
    }
    if !compact {
        t.s(if s.kind == CAPK_STAGE {
            b" MAX"
        } else {
            b" FREE"
        });
    }
}

/// Which entries fit. A module set is fixed per session, so the ticked set loads at the next boot into an empty runtime: `caps` becomes the limits
/// (nothing used), the ticked entries are taken in list order (one that no longer fits is flagged and not taken), then each unticked entry is
/// checked against what the ticked ones leave. An entry without `SCAN_COMPAT` or `SCAN_NEEDS` has no verdict. At most `SCAN_MAX` entries.
pub fn fit_set(caps: &mut [DkrtCap], entries: &[DkrtScan], ticked: &[u8], out: &mut DkrtFitOut) {
    // SAFETY: `DkrtFitOut` is plain integers; all zero is a valid value
    unsafe { core::ptr::write_bytes(out as *mut DkrtFitOut, 0, 1) };
    let n = if entries.len() > SCAN_MAX {
        SCAN_MAX
    } else {
        entries.len()
    };
    out.count = n as u32;
    empty(caps);
    let tick = |i: usize| i < ticked.len() && ticked[i] != 0;
    for i in 0..n {
        if entries[i].flags & SCAN_NEEDS != 0 {
            let nd = scan_needs(&entries[i]);
            size_k(&mut out.rows[i].mem, nd.arena + nd.dsp_area);
        }
    }
    for pass in 0..2 {
        if pass == 1 {
            // the ticked modules that fit are taken: the DSP and CPU memory of the limits they leave
            let mut any = false;
            for i in 0..n {
                any |= tick(i);
            }
            if any {
                let mut buf = [0u8; LINE_MAX];
                let len = load_line(caps, &mut buf);
                out.line[..len].copy_from_slice(&buf[..len]);
            }
        }
        for i in 0..n {
            let e = &entries[i];
            if e.flags & SCAN_NEEDS == 0 || e.flags & SCAN_COMPAT == 0 || tick(i) != (pass == 0) {
                continue;
            }
            let nd = scan_needs(e);
            match fits_now(&nd, caps) {
                None => {
                    if pass == 0 {
                        take(&nd, caps);
                    }
                }
                Some(s) => {
                    let r = &mut out.rows[i];
                    r.wont = 1;
                    r.short_kind = s.kind as u32;
                    r.short_need = s.need;
                    r.short_free = s.free;
                    short_text(&mut r.reason, &s, false);
                    short_text(&mut r.brief, &s, true);
                }
            }
        }
    }
}

#[cfg(test)]
mod scan_tests;
#[cfg(test)]
mod tests;
