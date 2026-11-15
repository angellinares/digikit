//! Effect pages: the FX view page and the knobs of a run-time effect (docs/design/digikit-registry.md "Effect pages", docs/findings/61-fx-pages.md).
//!
//! A `.dkm` that carries an EFFECT record (`register` kind 4) for one of its effects gets a page after CHO in the FX view of the Digitakt II. The page is built
//! from rows that `core/fxpages` reserved at patch time and that nothing here names a device for: `fxpages_info` is the table of their addresses, the kit's value
//! array and the device's free value indices. For effect row k (the k-th free row of `core/dynfx`) the record makes
//!
//! - the page row k of `fxpages`: tab label, title and the nine parameter codes (the knobs at their positions, MIX on the level slot),
//! - eight parameter rows (`params.fp<8k + j>`): knob j (the effect's parameter word p_j) and MIX (j = 7), each pointing at the kit's value array index
//!   `value_indices[8k + j]` as its mirror index, so the firmware's own page code shows and edits the value there,
//! - the list of pages of the FX view (the stock three, then the pages of the loaded effects in row order).
//!
//! `tick` (once per UI-loop pass) reads the values of the loaded effects from the kit's array and sends a changed value to the effect's row in the DSP, one word
//! per transfer through window 2 of `core/dsp-link`: knob j as a float in [min, max] into p_j (min + (max - min) * raw / 0x7f00), MIX as a float 0 .. 1 into the row's
//! mix word (raw / 0x7f00), which the adapter `core/fx` applies as the dry/wet blend. A value is the raw s16 of the array, 0 to 0x7f00 (the display shows raw / 256).
//! The values are saved with the kit (persistence, finding 61): the hooks `fxp_load` and `fxp_save` of `core/fxpages` run in the firmware's kit loader and saver, put the
//! values of the loaded effects into the kit's spare storage halfwords and take them out again, together with the effect's ID (the EFFECT record's `id`). A load
//! gives an effect row the stored values only when the stored ID is the one of the effect now in the row, and the effect's defaults otherwise, before the UI loop
//! looks at the array again, so no zero ever reaches the DSP. A third hook, `fxp_init` (the firmware's initialiser of the array: a new kit, a fresh kit that Load Kit fills),
//! gives each loaded effect its defaults. Both put a mark in a spare index of the array; a kit object without the mark (another pattern's kit, made before the effect was loaded) gets the
//! defaults the first time `tick` finds it live, before any value is read from it.
//! The array is the live kit's (`[fx] kit_ptr` of the device file): the kit accessor of the firmware (`fw_kit_of`) answers another object, the project's.
//!
//! Everything runs in the UI task. No float arithmetic runs in an interrupt; the arithmetic is the compiler's soft float.
use crate::dsp as fxdsp;
use crate::fxsend;
use dkrt_abi::core_api::{CoreApi, StaticCore};
use dkrt_abi::mem::{cstr_len, iw, rd, rd16, rd32, wr, wr16};
use core::ptr::{addr_of_mut, read_volatile, write_volatile};
use dkrt_sdk::*;

pub use dkrt_sdk::records::{KIND_EFFECT, LEN_EFFECT};
const INFO_MAGIC: u32 = 0x4658_5047; // "FXPG"
use dkrt_abi::dsp::MAX_FX;
const KNOBS: usize = 7;
const WORDS: usize = 8; // the knobs and MIX
const TEXT: usize = 24;
const REP_SIZE: usize = 48;
const KNOB_REC: usize = 52;
const KNOB_AT: usize = 48;
const RAW_MAX: i32 = 0x7f00;
const RAW_F: f32 = 32512.0; // 0x7f00

extern "C" {
    /// Data symbol of core/fxpages; resolves to 0 when the module is not in the image (`[object] optional`).
    fn fxpages_info();
}

/// Words of `fxpages_info` (byte offsets / 4); the value indices are bytes from byte 76.
mod w {
    pub const MAGIC: usize = 0;
    pub const DEVICE: usize = 2;
    pub const ROWS: usize = 3;
    pub const ROW_SIZE: usize = 4;
    pub const FIRST: usize = 5;
    pub const PAGES: usize = 6;
    pub const LIST: usize = 7;
    pub const LIST_CAP: usize = 8;
    pub const COUNT: usize = 9;
    pub const STOCK: usize = 10;
    pub const STOCK_N: usize = 11;
    pub const P_FIRST: usize = 12;
    pub const PER: usize = 13;
    pub const ARRAY_OFF: usize = 14;
    pub const VALUES_N: usize = 15;
    pub const TPL_PERCENT: usize = 16;
    pub const TPL_BIPOLAR: usize = 17;
    pub const TPL_MIX: usize = 18;
    pub const KIT_PTR: usize = 19;
    pub const INDICES: usize = 80; // bytes
    pub const IDS: usize = 28; // byte 112: address of the effect IDs of the rows (halfwords)
    pub const DEFS: usize = 29; // the default raw values (halfwords, one for each value index)
    pub const MARK_INDEX: usize = 37; // byte 148: the array index of the mark, and its value (word 38)
    pub const MARK_VALUE: usize = 38;
}

/// The effect pages' state: who owns each page (module record + 1), the values in force, and the strings the rows point to.
#[repr(C, align(4))]
pub struct FxpState {
    pub magic: u32,
    /// The kit object whose array the values were last put in or read from (0: none seen yet).
    pub kit: u32,
    pub polls: u32,
    pub changes: u32,
    pub sent: u32,
    pub errors: u32,
    pub seeds: u32,
    pub owner: [u8; MAX_FX],
    /// Knobs of the page (the words p0 .. p(n-1)); MIX is word 7.
    pub nknob: [u8; MAX_FX],
    /// The raw value (0 .. 0x7f00, as the kit's array holds it) in force, the one handed to the link as the word's live value (`want`) and the one the DSP has
    /// taken (`dsp`; -1: none).
    pub cur: [[i32; WORDS]; MAX_FX],
    pub want: [[i32; WORDS]; MAX_FX],
    pub dsp: [[i32; WORDS]; MAX_FX],
    /// The record's default raw values (knob j, MIX): what a kit gets that holds no values of this effect.
    pub def: [[i32; WORDS]; MAX_FX],
    /// The effect ID of each row (the record's; 0: no effect), as `fxp_ids` of core/fxpages has it for the kit hooks.
    pub id: [u32; MAX_FX],
    /// The live kit pointer changed to a kit object that had the mark (a pattern change back to a kit we know).
    pub adopts: u32,
    /// The float range of knob j: its minimum and its span max - min (f32 bits).
    pub min: [[u32; KNOBS]; MAX_FX],
    pub span: [[u32; KNOBS]; MAX_FX],
    tab: [[u8; TEXT]; MAX_FX],
    title: [[u8; TEXT]; MAX_FX],
    name: [[u8; 32]; MAX_FX * WORDS],
    short: [[u8; 16]; MAX_FX * WORDS],
    rep_tab: [[u8; REP_SIZE]; MAX_FX],
    rep_title: [[u8; REP_SIZE]; MAX_FX],
}

#[no_mangle]
pub static mut DKRT_FXP: FxpState = unsafe { core::mem::zeroed() };

fn fs() -> &'static mut FxpState {
    unsafe { &mut *addr_of_mut!(DKRT_FXP) }
}

pub(crate) unsafe fn info<C: StaticCore>() -> *const u32 {
    let a = core::hint::black_box(fxpages_info as usize);
    if a == 0 {
        return core::ptr::null();
    }
    let p = a as *const u32;
    if read_volatile(p.add(w::MAGIC)) != INFO_MAGIC || read_volatile(p.add(w::DEVICE)) != C::CORE.device_id() {
        return core::ptr::null();
    }
    p
}

/// The kit's value array index of word `j` of effect row `k`.
#[inline]
unsafe fn value_index(inf: *const u32, k: usize, j: usize) -> u32 {
    read_volatile((inf as *const u8).add(w::INDICES + k * WORDS + j)) as u32
}

unsafe fn text_ok(p: *const u8, field: usize, max: usize) -> bool {
    let n = cstr_len(p, field);
    if n == 0 || n > max || n == field {
        return false;
    }
    let mut i = 0;
    while i < field {
        let c = *p.add(i);
        if i < n {
            if !(0x20..0x7f).contains(&c) {
                return false;
            }
        } else if c != 0 {
            return false;
        }
        i += 1;
    }
    true
}

unsafe fn copy_text(dst: *mut u8, src: *const u8, n: usize) {
    let mut i = 0;
    while i < n {
        *dst.add(i) = *src.add(i);
        i += 1;
    }
}

/// Whether this image can show effect pages at all (`core/fxpages` and `core/dynreg` are in it).
pub unsafe fn available<C: StaticCore>() -> bool {
    !info::<C>().is_null()
}

/// The fixed-size EFFECT record at `b` (its body), checked before anything is written. Pure: the page slots are checked by `apply`.
pub unsafe fn check_record<C: StaticCore>(b: *const u8) -> i32 {
    let inf = info::<C>();
    if inf.is_null() {
        return E_NO_REGISTRY;
    }
    let (fx, mix, n) = (rd32(b), rd32(b.add(4)), rd32(b.add(8)) as usize);
    let id = rd32(b.add(12));
    if fx as usize >= MAX_FX || mix > 127 || n > KNOBS || id == 0 || id > 0xffff {
        return E_FORMAT;
    }
    if !text_ok(b.add(16), 8, 3) || !text_ok(b.add(24), TEXT, TEXT - 1) {
        return E_FORMAT;
    }
    let mut seen = 0u32;
    let mut j = 0;
    while j < KNOBS {
        let k = b.add(KNOB_AT + j * KNOB_REC);
        if j < n {
            let (pos, disp) = (rd32(k), rd32(k.add(4)));
            let (lo, hi, d) = (f32::from_bits(rd32(k.add(8))), f32::from_bits(rd32(k.add(12))), f32::from_bits(rd32(k.add(16))));
            if pos >= 8 || seen & (1 << pos) != 0 || disp > 1 {
                return E_FORMAT;
            }
            seen |= 1 << pos;
            // finite, min < max, the default inside (the comparisons are false for a NaN)
            if !(lo < hi) || !(d >= lo && d <= hi) || (hi - lo) == f32::INFINITY {
                return E_FORMAT;
            }
            if !text_ok(k.add(20), TEXT, TEXT - 1) || !text_ok(k.add(20 + TEXT), 8, 6) {
                return E_FORMAT;
            }
        }
        j += 1;
    }
    OK
}

/// The raw value of a float default: the nearest of 0 .. 0x7f00 over [min, max].
fn raw_of(def: f32, lo: f32, hi: f32) -> i32 {
    let f = (def - lo) / (hi - lo) * RAW_F + 0.5;
    let n = f as i32;
    if n < 0 {
        0
    } else if n > RAW_MAX {
        RAW_MAX
    } else {
        n
    }
}

/// The float a raw value stands for: min + span * (raw / 0x7f00); the maximum is exact.
#[inline]
fn knob_value(min: u32, span: u32, raw: i32) -> u32 {
    (f32::from_bits(min) + f32::from_bits(span) * (raw as f32 / RAW_F)).to_bits()
}

/// MIX as a float: raw / 0x7f00 (0x7f00 is exactly 1.0, 0 exactly 0).
#[inline]
fn mix_value(raw: i32) -> u32 {
    (raw as f32 / RAW_F).to_bits()
}

unsafe fn rebuild_list(inf: *const u32) {
    let s = fs();
    let list = iw(inf, w::LIST);
    let stock = iw(inf, w::STOCK);
    // the stock count is the byte of `moveq N,D1` in the view builder
    let n_stock = read_volatile((iw(inf, w::STOCK_N) + 1) as *const u8) as u32;
    let cap = iw(inf, w::LIST_CAP);
    let mut n = 0;
    while n < n_stock && n < cap {
        wr(list + 4 * n, rd(stock + 4 * n));
        n += 1;
    }
    // the send effects first, then the master effects, each in row order (FX routing: the send effects are the ones the track FX key has a page for)
    let mut pass = 0;
    while pass < 2 {
        let mut k = 0;
        while k < iw(inf, w::PAGES) as usize && k < MAX_FX {
            if s.owner[k] != 0 && n < cap && fxsend::is_send(k) == (pass == 0) {
                wr(list + 4 * n, iw(inf, w::FIRST) + k as u32);
                n += 1;
            }
            k += 1;
        }
        pass += 1;
    }
    // no page of ours: the count 0 keeps the stock list (the hook in the builder)
    wr(iw(inf, w::COUNT), if n == n_stock { 0 } else { n });
}

/// Build the page list again (a route changed the order).
pub unsafe fn relist<C: StaticCore>() {
    let inf = info::<C>();
    if !inf.is_null() {
        rebuild_list(inf);
    }
}

/// The module record + 1 that owns the page of effect row `k` (0: no page).
#[inline]
pub fn page_owner(k: usize) -> u8 {
    if k < MAX_FX {
        fs().owner[k]
    } else {
        0
    }
}

/// The tab label of the page of effect row `k` (up to 3 characters) into `out`.
#[inline]
pub fn tab_of(k: usize, out: &mut [u8; 8]) {
    if k < MAX_FX {
        let t = &fs().tab[k];
        let mut i = 0;
        while i < 8 && t[i] != 0 {
            out[i] = t[i];
            i += 1;
        }
    }
}

/// Fill the page, the parameter rows and the list for the EFFECT record at `b` of module `owner` (module record + 1). The effect's row in the chain has been
/// claimed by `dsp::reserve`; the page and parameter rows belong to that row's number.
pub unsafe fn apply<C: StaticCore>(owner: u8, b: *const u8) -> i32 {
    let inf = info::<C>();
    if inf.is_null() {
        return E_NO_REGISTRY;
    }
    let s = fs();
    let Some(k) = fxdsp::fx_row_of::<C>(owner, rd32(b)) else {
        return E_FORMAT; // no such effect in the module's DSP section
    };
    if k >= iw(inf, w::PAGES) as usize || k >= MAX_FX || (iw(inf, w::VALUES_N) as usize) < (k + 1) * WORDS {
        return E_PAGE_FULL;
    }
    if s.owner[k] != 0 {
        return E_PAGE_FULL;
    }
    let n = rd32(b.add(8)) as usize;
    let first = iw(inf, w::P_FIRST) + (k * WORDS) as u32;
    let row = iw(inf, w::ROWS) + k as u32 * iw(inf, w::ROW_SIZE);
    // strings
    copy_text(s.tab[k].as_mut_ptr(), b.add(16), 8);
    copy_text(s.title[k].as_mut_ptr(), b.add(24), TEXT);
    let tab = C::CORE.make_rep(s.rep_tab[k].as_mut_ptr(), s.tab[k].as_ptr(), false);
    let title = C::CORE.make_rep(s.rep_title[k].as_mut_ptr(), s.title[k].as_ptr(), false);
    // the page's nine codes: 0 where no knob stands
    let mut codes = [0u32; 9];
    let mut j = 0;
    while j < WORDS {
        let wi = k * WORDS + j;
        let (code, tpl, def_n) = if j < n {
            let kr = b.add(KNOB_AT + j * KNOB_REC);
            let (lo, hi) = (f32::from_bits(rd32(kr.add(8))), f32::from_bits(rd32(kr.add(12))));
            copy_text(s.name[wi].as_mut_ptr(), kr.add(20), TEXT);
            copy_text(s.short[wi].as_mut_ptr(), kr.add(20 + TEXT), 8);
            let pos = rd32(kr) as usize;
            let tpl = if rd32(kr.add(4)) == 1 { iw(inf, w::TPL_BIPOLAR) } else { iw(inf, w::TPL_PERCENT) };
            let d = raw_of(f32::from_bits(rd32(kr.add(16))), lo, hi);
            s.min[k][j] = lo.to_bits();
            s.span[k][j] = (hi - lo).to_bits();
            codes[pos] = first + j as u32;
            (first + j as u32, tpl, d)
        } else if j == WORDS - 1 {
            copy_text(s.name[wi].as_mut_ptr(), b"Mix\0".as_ptr(), 4);
            copy_text(s.short[wi].as_mut_ptr(), b"MIX\0".as_ptr(), 4);
            codes[8] = first + j as u32;
            (first + j as u32, iw(inf, w::TPL_MIX), rd32(b.add(4)) as i32 * 256)
        } else {
            j += 1;
            continue; // a word without a knob
        };
        let idx = value_index(inf, k, j);
        write_knob_row(&C::CORE, s, k, j, code, tpl, idx, def_n);
        j += 1;
    }
    s.nknob[k] = n as u8;
    publish_row(inf, k, rd32(b.add(12)));
    let live = kit(inf);
    if live != 0 {
        // a kit object is live already and holds nothing of this effect (the init hook ran before the effect was here): its defaults now
        seed_row(inf, k, live + iw(inf, w::ARRAY_OFF));
        mark(inf, live + iw(inf, w::ARRAY_OFF));
        s.seeds += 1;
    }
    fxdsp::fx_set_mix::<C>(k, mix_value(rd32(b.add(4)) as i32 * 256));
    // the page row: codes, then the strings; the owner and the list publish it
    let mut c = 0;
    while c < 9 {
        wr(row + 8 + 4 * c as u32, codes[c]);
        c += 1;
    }
    wr(row, tab);
    wr(row + 4, title);
    s.owner[k] = owner;
    rebuild_list(inf);
    fxsend::apply::<C>(k, owner, s.tab[k].as_ptr());
    OK
}

/// Give back the pages of module `owner`: out of the list first, then the rows back to the reserved state.
pub unsafe fn remove<C: StaticCore>(owner: u8) {
    let inf = info::<C>();
    if inf.is_null() {
        return;
    }
    let s = fs();
    let mut gone = 0u32;
    let mut k = 0;
    while k < MAX_FX {
        if s.owner[k] == owner {
            s.owner[k] = 0;
            gone |= 1 << k;
        }
        k += 1;
    }
    if gone == 0 {
        return;
    }
    rebuild_list(inf);
    k = 0;
    while k < MAX_FX {
        if gone & (1 << k) != 0 {
            let row = iw(inf, w::ROWS) + k as u32 * iw(inf, w::ROW_SIZE);
            let mut c = 0;
            while c < 9 {
                wr(row + 8 + 4 * c as u32, 0);
                c += 1;
            }
            let mut j = 0;
            publish_row(inf, k, 0);
            while j < WORDS {
                C::CORE.reset_param_row(iw(inf, w::P_FIRST) + (k * WORDS + j) as u32);
                s.cur[k][j] = 0;
                s.def[k][j] = 0;
                s.dsp[k][j] = -1;
                s.want[k][j] = -1;
                j += 1;
            }
            s.nknob[k] = 0;
            fxdsp::fx_forget(&C::CORE, k);
            fxsend::remove::<C>(k);
        }
        k += 1;
    }
}

/// Hand the kit hooks of `core/fxpages` the effect ID of row `k` (0: none) and its defaults. An effect's defaults go first and its ID after them, so a kit
/// load between the two stores cannot restore values against defaults that are not there yet; a row's ID goes to 0 first for the same reason.
unsafe fn publish_row(inf: *const u32, k: usize, id: u32) {
    let s = fs();
    let (ids, defs) = (iw(inf, w::IDS), iw(inf, w::DEFS));
    if id == 0 {
        write_volatile((ids + 2 * k as u32) as *mut u16, 0);
    }
    let mut j = 0;
    while j < WORDS {
        write_volatile((defs + 2 * (k * WORDS + j) as u32) as *mut u16, s.def[k][j] as u16);
        j += 1;
    }
    if id != 0 {
        write_volatile((ids + 2 * k as u32) as *mut u16, id as u16);
    }
    s.id[k] = id;
}

/// Put the mark of `core/fxpages` into the kit array `arr`: its effect rows hold values.
#[inline]
unsafe fn mark(inf: *const u32, arr: u32) {
    wr16(arr + 2 * iw(inf, w::MARK_INDEX), iw(inf, w::MARK_VALUE) as i32);
}

/// The defaults of effect row `k` into the kit array `arr`.
unsafe fn seed_row(inf: *const u32, k: usize, arr: u32) {
    let s = fs();
    let mut j = 0;
    while j < WORDS {
        if has_word(s, k, j) {
            wr16(arr + 2 * value_index(inf, k, j), s.def[k][j]);
            s.cur[k][j] = s.def[k][j];
        }
        j += 1;
    }
}

/// The live kit object (0 while the firmware has none); a pointer in a DDR cell, checked to lie in DDR. Any alignment: the Digitone II's kit objects are allocated at odd
/// addresses (its sound arrays have an odd stride), so the array's halfwords are read and written a byte at a time.
pub(crate) unsafe fn kit(inf: *const u32) -> u32 {
    let k = rd(iw(inf, w::KIT_PTR));
    let ddr = dkrt_sdk::device::DDR_CACHED_BASE..dkrt_sdk::device::DDR_CACHED_BASE + dkrt_sdk::device::DDR_BYTES;
    if !ddr.contains(&k) {
        return 0;
    }
    k
}

/// Words of effect row `k` that exist: its knobs and MIX.
#[inline(never)]
fn has_word(s: &FxpState, k: usize, j: usize) -> bool {
    j == WORDS - 1 || j < s.nknob[k] as usize
}

/// Once per UI-loop pass: note which values the DSP has taken, put the values in a new kit's array, and hand the changed values to the link; then the send effects' amounts.
pub unsafe fn tick<C: StaticCore>() {
    let s = fs();
    let mut any = false;
    let mut k = 0;
    while k < MAX_FX {
        any |= s.owner[k] != 0;
        k += 1;
    }
    if !any {
        return;
    }
    let inf = info::<C>();
    if inf.is_null() {
        return;
    }
    confirm::<C>();
    values::<C>(inf);
    fxsend::tick::<C>();
}

/// The effect pages' values: the kit's array against the DSP rows.
unsafe fn values<C: StaticCore>(inf: *const u32) {
    let s = fs();
    let mut k;
    let kit = kit(inf);
    if kit == 0 {
        return;
    }
    let arr = kit + iw(inf, w::ARRAY_OFF);
    if rd16(arr + 2 * iw(inf, w::MARK_INDEX)) != iw(inf, w::MARK_VALUE) as i32 {
        // no mark: the hooks of core/fxpages have not given this kit object its rows (a kit that was there before the effect was loaded: another pattern's, or one the
        // firmware cleared without them): the defaults, and the mark, before anything is read from it (a zero is never sent)
        k = 0;
        while k < MAX_FX {
            if s.owner[k] != 0 {
                seed_row(inf, k, arr);
            }
            k += 1;
        }
        mark(inf, arr);
        s.seeds += 1;
        s.kit = kit;
        return;
    }
    if kit != s.kit {
        s.adopts += 1; // a kit object with its rows: the live kit changed (a pattern change); the poll sends what differs from the DSP
        s.kit = kit;
        return;
    }
    s.polls = s.polls.wrapping_add(1);
    k = 0;
    while k < MAX_FX {
        if s.owner[k] != 0 {
            let mut j = 0;
            while j < WORDS {
                if has_word(s, k, j) {
                    let raw = rd16(arr + 2 * value_index(inf, k, j));
                    let n = if raw < 0 { 0 } else if raw > RAW_MAX { RAW_MAX } else { raw };
                    if n != s.cur[k][j] {
                        s.cur[k][j] = n;
                        s.changes += 1;
                    }
                    // only when the row is in the DSP and no load is going on; the link sends the newest value of the word when it is free
                    if n != s.want[k][j] && fxdsp::fx_live::<C>(k) {
                        set_knob(&C::CORE, s, k, j, n);
                    }
                }
                j += 1;
            }
        }
        k += 1;
    }
}

/// The parameter row of word `j` of effect row `k` (the code is the row's), and the record's default as the value in force, in the kit and in the DSP row.
unsafe fn write_knob_row<C: CoreApi>(c: &C, s: &mut FxpState, k: usize, j: usize, code: u32, tpl: u32, idx: u32, def_n: i32) {
    let wi = k * WORDS + j;
    c.write_fx_param_row(code, tpl, idx, 0, RAW_MAX as u32, def_n as u32, s.name[wi].as_ptr() as u32, s.short[wi].as_ptr() as u32);
    s.cur[k][j] = def_n;
    s.def[k][j] = def_n;
    s.dsp[k][j] = def_n; // the DSP row gets the record's defaults (p words) and the mix below
    s.want[k][j] = def_n;
}

/// The link slot and the DSP word of word `j` of effect row `k` for the raw value `n`: knob j is word 4 + j (a float in [min, max]), MIX is word 13.
unsafe fn knob_word<C: CoreApi>(c: &C, s: &FxpState, k: usize, j: usize, n: i32) -> Option<(dkrt_abi::link::Slot, u32)> {
    let (word, bits) = if j == WORDS - 1 {
        (dkrt_abi::dsp::FXW_MIX, mix_value(n))
    } else {
        (4 + j as u32, knob_value(s.min[k][j], s.span[k][j], n))
    };
    fxdsp::fx_slot(c, k, word).map(|slot| (slot, bits))
}

/// Hand the raw value `n` of word `j` of effect row `k` to the link as the word's live value.
unsafe fn set_knob<C: CoreApi>(c: &C, s: &mut FxpState, k: usize, j: usize, n: i32) -> bool {
    match knob_word(c, s, k, j, n) {
        Some((slot, bits)) => {
            c.link_set(slot, bits);
            s.want[k][j] = n;
            true
        }
        None => false,
    }
}

/// Note the values the DSP has taken: a word whose live value the link confirms is the DSP's.
unsafe fn confirm<C: StaticCore>() {
    let s = fs();
    let mut k = 0;
    while k < MAX_FX {
        if s.owner[k] != 0 {
            let mut j = 0;
            while j < WORDS {
                let n = s.want[k][j];
                if has_word(s, k, j) && n >= 0 && n != s.dsp[k][j] {
                    if let Some((slot, bits)) = knob_word(&C::CORE, s, k, j, n) {
                        if C::CORE.link_confirmed(slot) == Some(bits) {
                            s.dsp[k][j] = n;
                            s.sent += 1;
                        }
                    }
                }
                j += 1;
            }
        }
        k += 1;
    }
    s.errors = C::CORE.link_errors();
}

/// Pages in use and the pages the image reserved (for CAPACITY-like reports and tests): (reserved, used).
pub unsafe fn counts<C: StaticCore>() -> (u32, u32) {
    let inf = info::<C>();
    if inf.is_null() {
        return (0, 0);
    }
    let s = fs();
    let mut used = 0;
    let mut k = 0;
    while k < MAX_FX {
        if s.owner[k] != 0 {
            used += 1;
        }
        k += 1;
    }
    (iw(inf, w::PAGES), used)
}

#[cfg(test)]
mod tests {
    use super::*;
    use dkrt_test_support::{Call, FakeCore};

    /// The optional data symbol of `core/fxpages`: the runtime's own host fixture defines it for its tests; this crate's test binary has none.
    #[no_mangle]
    pub extern "C" fn fxpages_info() {}

    /// A knob of effect row 2 is declared with a default, then its value changes: the parameter row is written first, then the new value is the word's live value.
    #[test]
    fn a_knob_gets_its_row_then_a_change_is_set_on_the_link() {
        let mut s: std::boxed::Box<FxpState> = unsafe { std::boxed::Box::new(core::mem::zeroed()) };
        let core = FakeCore::new();
        unsafe {
            s.min[2][1] = 0f32.to_bits();
            s.span[2][1] = 1f32.to_bits();
            write_knob_row(&core, &mut s, 2, 1, 120, 7, 33, 0x4000);
            assert_eq!((s.cur[2][1], s.def[2][1], s.dsp[2][1], s.want[2][1]), (0x4000, 0x4000, 0x4000, 0x4000));
            assert!(set_knob(&core, &mut s, 2, 1, 0x7f00));
            assert_eq!(s.want[2][1], 0x7f00);
            // a knob is word 4 + j of its row, MIX word 13, whatever its place among the knobs; the rows are 24 words apart
            set_knob(&core, &mut s, 3, WORDS - 1, 0x7f00);
        }
        let calls = core.calls();
        assert_eq!(calls[0], Call::FxRow { code: 120, mirror: 33, def: 0x4000 });
        assert!(matches!(calls[1], Call::Set { slot: dkrt_abi::link::Slot { win: 2, word: 53 }, .. }));
        assert!(matches!(calls[2], Call::Set { slot: dkrt_abi::link::Slot { win: 2, word: 85 }, .. }));
    }

    /// The offsets that the device tests read from `DKRT_FXP` (`tests/regression/runtime/fx-pages-ui.ts`).
    #[test]
    fn the_layout_the_device_tests_read_is_unchanged() {
        assert_eq!(core::mem::offset_of!(FxpState, sent), 16);
        assert_eq!(core::mem::offset_of!(FxpState, errors), 20);
        assert_eq!(core::mem::offset_of!(FxpState, seeds), 24);
        assert_eq!(core::mem::offset_of!(FxpState, owner), 28);
        assert_eq!(core::mem::offset_of!(FxpState, cur), 36);
        assert_eq!(core::mem::offset_of!(FxpState, dsp), 292);
    }

    /// Several turns before the link is free end as one write of the newest value, and the word counts as the DSP's when the link confirms it.
    #[test]
    fn a_fast_turn_sends_the_last_value_and_the_dsp_value_follows_the_confirmation() {
        let mut s: std::boxed::Box<FxpState> = unsafe { std::boxed::Box::new(core::mem::zeroed()) };
        let core = FakeCore::new();
        unsafe {
            s.min[2][1] = 0f32.to_bits();
            s.span[2][1] = 1f32.to_bits();
            write_knob_row(&core, &mut s, 2, 1, 120, 7, 33, 0x4000);
            for n in [0x100, 0x200, 0x300] {
                set_knob(&core, &mut s, 2, 1, n);
            }
            for _ in 0..50 {
                core.link.borrow_mut().pump();
            }
            let slot = fxdsp::fx_slot(&core, 2, 5).unwrap();
            let wire = core.link.borrow().t.words();
            assert_eq!(wire.len(), 1);
            assert_eq!(core.link_confirmed(slot), Some(knob_value(s.min[2][1], s.span[2][1], 0x300)));
        }
    }
}
