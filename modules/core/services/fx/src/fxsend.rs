//! Send effects: the route of a run-time effect and its send amounts, one for each track (docs/findings/64-fx-send-page.md; the DSP side is finding 62).
//!
//! A digikit effect runs on the master chain or on a send bus. The route is a setting (`fx/route/<module id>`, one byte, 0 master, 1 send; step 2's store) and is read when the
//! effect is registered, at the boot. The effect's record says which routes are allowed and which is the default (`route`, `routes` of the DK_FX record; flags word of the
//! record): the default is used when the setting is not there or names a route the effect does not allow. DIGIKIT > FX ROUTES (`core/digikit-menu`) writes the setting through
//! `dkrt_settings_put`, and lists the effects with `dkrt_fx`; the device limits the send effects (`[limits] send_slots`, `fxpages_info` word 39: the cells of the send page); an effect past the limit
//! stays on the master. A send effect has a cell on the send page (the second page of the track FX key, `core/fxpages`): one knob row (`params.fs<c>`) whose value is the halfword
//! `Sound[send_indices[c]]` of the current track's Sound object, so the firmware's own page code shows, edits and draws it (`FSH Send=40`). The row has the value class of the
//! kit's Delay rows (no p-lock) and no CC.
//!
//! The amount of the effect for track t is the byte `Sound_t[idx] >> 8` (0 .. 127). It is kept in the settings (`fx/send/<module id>`, 16 bytes) and in the DSP row (words 19 to
//! 22, finding 62), never in an Elektron file: the sound loader zeroes the index (Load Kit, a pattern's kit, a project: the hook of `core/fxpages` counts the loads) and the
//! saver does not write it. `tick` (every UI-loop pass):
//!
//! - after the loader counter changed and stayed still for `SETTLE` passes, and when the live kit object changed (a pattern's kit), the stored amounts are written back
//!   into the Sound arrays of the 16 tracks first (so the zeroes of a load are never taken as a turn),
//! - then each Sound array is read: a value that is not the one last written or seen is a turn of the knob: the effect's amount for that track follows (a put into the
//!   settings, written to the card 2 s after the last change, and the DSP words, live values on the link through window 2 like the effect pages' values).
//!
//! Everything runs in the UI task (the settings store is not for other tasks).
use crate::dsp as fxdsp;
use crate::fxpage;
use core::ptr::addr_of_mut;
use dkrt_abi::core_api::StaticCore;
use dkrt_abi::dsp;
use dkrt_abi::mem::{cstr_len, iw, rd, rd16, wr, wr16};
use dkrt_sdk::*;

/// The routes of an effect (`dkrt_sdk::DkrtFx`), and the bits of the flags word of its record (docs/design/digikit-dkm.md).
const ROUTE_MASTER: u8 = dkneeds::page::DK_ROUTE_MASTER as u8;
const ROUTE_SEND: u8 = dkneeds::page::DK_ROUTE_SEND as u8;
use dkneeds::{FXF_DEFAULT_SEND, FXF_NOMASTER, FXF_SEND};

/// Cells of the send page the image can have at most (`[limits.max] send_slots`).
pub const SEND_MAX: usize = 2;
const TRACKS: usize = 16;
/// UI-loop passes the loader counter must be still before the amounts are written back.
const SETTLE: u32 = 4;
/// UI-loop passes a 0 must stay in a Sound array before it counts as a turn to 0: the sound loader zeroes the array at the start of a load and the loader counter moves at its
/// end (a pass in between sees the zero; the restore after the count overwrites it before it is taken).
const ZERO_PASSES: u8 = 3;
/// Offset of the parameter array in a Sound object.
const SOUND_ARRAY: u32 = 0x14;
const KEY_MAX: usize = 48;

/// Words of `fxpages_info` (byte offset / 4) from version 2.
mod w {
    pub const VERSION: usize = 1;
    pub const TPL_PERCENT: usize = 16;
    pub const CELLS: usize = 39;
    pub const ROW: usize = 40;
    pub const COUNT: usize = 41;
    pub const P_SEND: usize = 42;
    pub const IDX0: usize = 43;
    pub const TEMPLATE: usize = 45;
    pub const SOUND_OFF: usize = 46;
    pub const SOUND_STRIDE: usize = 47;
    pub const NSOUND: usize = 48;
}

#[repr(C, align(4))]
pub struct SendState {
    pub magic: u32,
    /// Send cells in use, loader count last seen, passes left until it counts as still, restores done, turns found, DSP words sent, failed, puts.
    pub used: u32,
    pub nsound: u32,
    pub settle: u32,
    pub restores: u32,
    pub turns: u32,
    pub sent: u32,
    pub errors: u32,
    pub puts: u32,
    /// The tracks (bit mask) whose Sound array gets the stored amounts before it is read.
    pub restore: u32,
    /// Per effect row: the routes the effect allows (`ROUTE_*` bits) and its default route (`ROUTE_*`), from its record.
    pub allow: [u8; dsp::MAX_FX],
    pub dflt: [u8; dsp::MAX_FX],
    /// Per effect row: 1 when it runs on a send bus, its cell + 1, the length of its module id and the id.
    pub route: [u8; dsp::MAX_FX],
    pub cell: [u8; dsp::MAX_FX],
    pub idlen: [u8; dsp::MAX_FX],
    /// Per cell: its effect row + 1.
    pub row: [u8; SEND_MAX],
    pub id: [[u8; 32]; dsp::MAX_FX],
    /// The amounts, one byte for each track.
    pub amt: [[u8; TRACKS]; dsp::MAX_FX],
    /// The amount words the DSP row has taken, and the ones handed to the link as live values.
    pub dsp: [[u32; 4]; dsp::MAX_FX],
    pub want: [[u32; 4]; dsp::MAX_FX],
    /// The live kit object last seen (the 16 Sound objects lie in it), and for each cell and track the raw value last written to or read from `Sound[idx]` (-1: none).
    pub kit: u32,
    pub shown: [[i32; TRACKS]; SEND_MAX],
    pub zero_wait: [[u8; TRACKS]; SEND_MAX],
    name: [[u8; 16]; SEND_MAX],
    short: [[u8; 8]; SEND_MAX],
}

#[no_mangle]
pub static mut DKRT_FXS: SendState = unsafe { core::mem::zeroed() };

fn ss() -> &'static mut SendState {
    unsafe { &mut *addr_of_mut!(DKRT_FXS) }
}

/// The info table when this image has send cells (a `core/fxpages` object of version 2 with at least one), else null.
unsafe fn info<C: StaticCore>() -> *const u32 {
    let inf = fxpage::info::<C>();
    if inf.is_null() || iw(inf, w::VERSION) < 2 || iw(inf, w::CELLS) == 0 {
        return core::ptr::null();
    }
    inf
}

/// The cells of the send page this image has (the CAPACITY limit of `SEND_SLOTS`): 0 without `core/fxpages` or on a device with no send page.
pub unsafe fn slots<C: StaticCore>() -> u32 {
    let inf = info::<C>();
    if inf.is_null() {
        return 0;
    }
    iw(inf, w::CELLS).min(SEND_MAX as u32)
}

/// The routes of a record's flags word: (allowed `ROUTE_*` bits, default `ROUTE_*`). The loader has checked the word.
fn routes_of(flags: u32) -> (u8, u8) {
    let mut allow = 0;
    if flags & FXF_NOMASTER == 0 {
        allow |= ROUTE_MASTER;
    }
    if flags & FXF_SEND != 0 {
        allow |= ROUTE_SEND;
    }
    (
        allow,
        if flags & FXF_DEFAULT_SEND != 0 {
            ROUTE_SEND
        } else {
            ROUTE_MASTER
        },
    )
}

/// The route effect row `k` has in the settings, as `ROUTE_*`: the stored one when the effect allows it, else its default. UI task.
unsafe fn saved_route<C: StaticCore>(k: usize) -> u8 {
    let s = ss();
    let mut key = [0u8; KEY_MAX];
    let kn = key_of(&mut key, b"fx/route/", &s.id[k], s.idlen[k] as usize);
    let mut v = [0u8; 4];
    if C::CORE.settings_get(&key[..kn], &mut v) == 1 {
        let r = if v[0] == 1 {
            ROUTE_SEND
        } else if v[0] == 0 {
            ROUTE_MASTER
        } else {
            0
        };
        if r != 0 && s.allow[k] & r != 0 {
            return r;
        }
    }
    s.dflt[k]
}

/// Effect `index` (0 ..) in row order, among the rows that have a page: the entry for DIGIKIT > FX ROUTES. 0, or `E_NOT_FOUND` past the end. UI task.
pub unsafe fn fx_entry<C: StaticCore>(index: u32, out: &mut DkrtFx) -> i32 {
    let s = ss();
    let mut seen = 0;
    let mut k = 0;
    while k < dsp::MAX_FX {
        if fxpage::page_owner(k) != 0 {
            if seen == index {
                out.row = k as u32;
                out.allowed = s.allow[k] as u32;
                out.dflt = s.dflt[k] as u32;
                out.running = if s.route[k] != 0 {
                    ROUTE_SEND as u32
                } else {
                    ROUTE_MASTER as u32
                };
                out.saved = saved_route::<C>(k) as u32;
                out.tab = [0; 8];
                fxpage::tab_of(k, &mut out.tab);
                out.id = s.id[k];
                return OK;
            }
            seen += 1;
        }
        k += 1;
    }
    E_NOT_FOUND
}

/// Does effect row `k` run on a send bus (the FX view lists the send effects before the master effects).
#[inline]
pub fn is_send(k: usize) -> bool {
    k < dsp::MAX_FX && ss().route[k] != 0
}

/// Send effects loaded (CAPACITY `used`).
#[inline]
pub fn used() -> u32 {
    ss().used
}

fn words_of(amt: &[u8; TRACKS]) -> [u32; 4] {
    let mut out = [0u32; 4];
    let mut t = 0;
    while t < TRACKS {
        out[t / 4] |= (amt[t] as u32) << (8 * (t % 4));
        t += 1;
    }
    out
}

/// `prefix` and the module id of effect row `k` into `buf`: the key's length.
fn key_of(buf: &mut [u8; KEY_MAX], prefix: &[u8], id: &[u8; 32], idlen: usize) -> usize {
    let mut n = 0;
    while n < prefix.len() {
        buf[n] = prefix[n];
        n += 1;
    }
    let mut i = 0;
    while i < idlen {
        buf[n + i] = id[i];
        i += 1;
    }
    n + idlen
}

unsafe fn copy(dst: &mut [u8], src: *const u8, max: usize) {
    let mut i = 0;
    while i < dst.len() {
        dst[i] = 0;
        i += 1;
    }
    i = 0;
    while i < max && i + 1 < dst.len() && *src.add(i) != 0 {
        dst[i] = *src.add(i);
        i += 1;
    }
}

/// The count cell of the track FX key's list: 2 (the send page follows the stock page) while a cell is in use, else 0 (the stock list).
#[inline]
unsafe fn publish_count(inf: *const u32) {
    wr(iw(inf, w::COUNT), if ss().used != 0 { 2 } else { 0 });
}

/// Effect row `k` (module record `owner - 1`, tab string `tab`) has been registered: read its route and amounts from the settings and, for a send effect within the limit, give it a
/// cell of the send page. The route and the four amount words go to the DSP with the row (`dsp::fx_set_route`).
pub unsafe fn apply<C: StaticCore>(k: usize, owner: u8, tab: *const u8) {
    if k >= dsp::MAX_FX || owner == 0 {
        return;
    }
    let s = ss();
    let inf = info::<C>();
    if !inf.is_null() {
        release::<C>(inf, k);
    }
    let (allow, dflt) = routes_of(fxdsp::fx_flags_of::<C>(k));
    s.allow[k] = allow;
    s.dflt[k] = dflt;
    let name = C::CORE.module_name(owner);
    let mut n = 0;
    while n < 31 && *name.add(n) != 0 {
        s.id[k][n] = *name.add(n);
        n += 1;
    }
    while n < 32 {
        s.id[k][n] = 0;
        n += 1;
    }
    s.idlen[k] = cstr_len(name, 32) as u8;
    // an image with no send page (no cells) has every effect on the master, whatever the settings say
    if inf.is_null() {
        return;
    }
    let want = saved_route::<C>(k) == ROUTE_SEND;
    let mut key = [0u8; KEY_MAX];
    let mut a = [0u8; TRACKS];
    let kn = key_of(&mut key, b"fx/send/", &s.id[k], s.idlen[k] as usize);
    if C::CORE.settings_get(&key[..kn], &mut a) != TRACKS as i32 {
        a = [0u8; TRACKS];
    }
    let mut t = 0;
    while t < TRACKS {
        if a[t] > 127 {
            a[t] = 127;
        }
        t += 1;
    }
    s.amt[k] = a;
    s.dsp[k] = words_of(&a);
    s.want[k] = s.dsp[k];
    let cells = iw(inf, w::CELLS).min(SEND_MAX as u32) as usize;
    let mut c = 0;
    while c < cells && s.row[c] != 0 {
        c += 1;
    }
    if want && c < cells {
        s.route[k] = 1;
        s.cell[k] = c as u8 + 1;
        s.row[c] = k as u8 + 1;
        s.used += 1;
        // the knob: label "<tab> Send", short name the tab; the value class of the Delay rows, the owner of the stock send row, the Sound index of the cell
        let mut i = 0;
        let mut name = [0u8; 16];
        while i < 3 && *tab.add(i) != 0 {
            name[i] = *tab.add(i);
            i += 1;
        }
        let tl = i;
        copy(&mut s.short[c], name.as_ptr(), tl);
        let suffix = b" Send";
        i = 0;
        while i < suffix.len() {
            name[tl + i] = suffix[i];
            i += 1;
        }
        copy(&mut s.name[c], name.as_ptr(), 16);
        let code = iw(inf, w::P_SEND) + c as u32;
        C::CORE.write_send_param_row(
            code,
            iw(inf, w::TPL_PERCENT),
            iw(inf, w::TEMPLATE),
            iw(inf, w::IDX0 + c),
            s.name[c].as_ptr() as u32,
            s.short[c].as_ptr() as u32,
        );
        wr(iw(inf, w::ROW) + 8 + 4 * c as u32, code);
        // every Sound array gets the stored amounts before it is read
        s.restore = 0xffff;
        s.kit = 0;
        publish_count(inf);
    }
    fxdsp::fx_set_route::<C>(k, s.route[k] as u32, s.dsp[k]);
    // the FX view lists the send effects first: the list is built again now that the route is known
    fxpage::relist::<C>();
}

/// Give back the cell of effect row `k` (if it has one).
unsafe fn release<C: StaticCore>(inf: *const u32, k: usize) {
    let s = ss();
    if s.route[k] == 0 {
        return;
    }
    let c = s.cell[k] as usize - 1;
    wr(iw(inf, w::ROW) + 8 + 4 * c as u32, 0);
    C::CORE.reset_param_row(iw(inf, w::P_SEND) + c as u32);
    s.row[c] = 0;
    s.cell[k] = 0;
    s.route[k] = 0;
    s.used -= 1;
    let mut t = 0;
    while t < TRACKS {
        s.shown[c][t] = -1;
        t += 1;
    }
    publish_count(inf);
}

/// The effect row `k` is gone (its module was removed): its cell and its state.
pub unsafe fn remove<C: StaticCore>(k: usize) {
    if k >= dsp::MAX_FX {
        return;
    }
    let inf = info::<C>();
    if !inf.is_null() {
        release::<C>(inf, k);
    }
    fxdsp::fx_forget(&C::CORE, k);
    let s = ss();
    s.allow[k] = 0;
    s.dflt[k] = 0;
    s.amt[k] = [0; TRACKS];
    s.dsp[k] = [0; 4];
    s.want[k] = [0; 4];
    s.idlen[k] = 0;
    s.id[k] = [0; 32];
}

unsafe fn put_amounts<C: StaticCore>(k: usize) {
    let s = ss();
    let mut key = [0u8; KEY_MAX];
    let kn = key_of(&mut key, b"fx/send/", &s.id[k], s.idlen[k] as usize);
    C::CORE.settings_put(&key[..kn], &s.amt[k]);
    s.puts += 1;
}

/// Once per UI-loop pass (after the effect pages' own tick).
pub unsafe fn tick<C: StaticCore>() {
    let s = ss();
    if s.used == 0 {
        return;
    }
    let inf = info::<C>();
    if inf.is_null() {
        return;
    }
    // a kit, a pattern's kit or a project was loaded: wait until the loads are over, then write the amounts back
    let ns = rd(iw(inf, w::NSOUND));
    if ns != s.nsound {
        s.nsound = ns;
        s.settle = SETTLE;
    }
    let mut quiet = true;
    if s.settle != 0 {
        s.settle -= 1;
        if s.settle == 0 {
            s.restore = 0xffff;
        } else {
            quiet = false;
        }
    }
    if quiet {
        tracks::<C>(inf);
    }
    // the amounts go to the link as live values; the DSP's own copy follows the link's confirmation
    let mut k = 0;
    while k < dsp::MAX_FX {
        if s.route[k] != 0 && fxdsp::fx_live::<C>(k) {
            let want = words_of(&s.amt[k]);
            let mut j = 0;
            while j < 4 {
                if let Some(slot) = fxdsp::fx_slot(&C::CORE, k, dsp::FXW_AMT0 + j as u32) {
                    if want[j] != s.want[k][j] {
                        C::CORE.link_set(slot, want[j]);
                        s.want[k][j] = want[j];
                    }
                    if s.dsp[k][j] != s.want[k][j]
                        && C::CORE.link_confirmed(slot) == Some(s.want[k][j])
                    {
                        s.dsp[k][j] = s.want[k][j];
                        s.sent += 1;
                    }
                }
                j += 1;
            }
        }
        k += 1;
    }
    s.errors = C::CORE.link_errors();
}

/// The 16 Sound arrays: the stored amounts back into those that need them, a turn of the knob in the others.
unsafe fn tracks<C: StaticCore>(inf: *const u32) {
    let s = ss();
    let kit = fxpage::kit(inf);
    if kit == 0 {
        return;
    }
    if kit != s.kit {
        s.kit = kit;
        s.restore = 0xffff;
    }
    let (off, stride) = (iw(inf, w::SOUND_OFF), iw(inf, w::SOUND_STRIDE));
    let mut changed = 0u32; // effect rows whose amounts changed
    let mut t = 0;
    while t < TRACKS {
        let p = kit + off + stride * t as u32;
        let mut c = 0;
        while c < SEND_MAX {
            let k = s.row[c] as usize;
            if k != 0 {
                let k = k - 1;
                let at = p + SOUND_ARRAY + 2 * iw(inf, w::IDX0 + c);
                if s.restore & (1 << t) != 0 {
                    let raw = (s.amt[k][t] as i32) << 8;
                    wr16(at, raw);
                    s.shown[c][t] = raw;
                    s.zero_wait[c][t] = 0;
                } else {
                    let v = rd16(at);
                    if v == 0 && v != s.shown[c][t] && s.zero_wait[c][t] < ZERO_PASSES {
                        s.zero_wait[c][t] += 1;
                    } else if v != s.shown[c][t] {
                        s.zero_wait[c][t] = 0;
                        s.shown[c][t] = v;
                        let a = if v < 0 { 0 } else { (v >> 8).min(127) } as u8;
                        s.turns += 1;
                        if a != s.amt[k][t] {
                            s.amt[k][t] = a;
                            changed |= 1 << k;
                        }
                    } else {
                        s.zero_wait[c][t] = 0;
                    }
                }
            }
            c += 1;
        }
        if s.restore & (1 << t) != 0 {
            s.restore &= !(1 << t);
            s.restores += 1;
        }
        t += 1;
    }
    let mut k = 0;
    while k < dsp::MAX_FX {
        if changed & (1 << k) != 0 {
            put_amounts::<C>(k);
        }
        k += 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The offsets that the device tests read from `DKRT_FXS` (`fx-routes-ui.ts`, `fx-track-route-kit.device.test.ts`).
    #[test]
    fn the_layout_the_device_tests_read_is_unchanged() {
        assert_eq!(core::mem::offset_of!(SendState, restores), 16);
        assert_eq!(core::mem::offset_of!(SendState, allow), 40);
        assert_eq!(core::mem::offset_of!(SendState, dflt), 44);
    }
}
