//! digikit's settings store at run time (docs/design/digikit-store.md, "The settings record"; the format is `dkrt_sdk::settings`).
//!
//! One record in RAM (8 KiB, `DKRT_SETTINGS`), read at the boot from the two copies on the card (the valid one with the higher `seq`). `put` and
//! `remove` change the RAM record and mark it dirty; `tick` (every UI-loop pass) writes it once nothing has changed for `DELAY_BLOCKS` blocks (2 s):
//! to the copy that is not current, with `seq + 1`, through `block_write` (the range and ownership checks of finding 46), and only the sectors the
//! record needs. A power cut in the write leaves the current copy valid. Unloaded modules keep their entries (nothing is removed but by `remove`).
//!
//! Callers: the UI task (the menu, `core/fxpages`) and the mailbox test commands. Not for interrupts or the MIDI task: the record is one buffer.
//! The C ABI for core modules is `dkrt_settings_get`, `dkrt_settings_put`, `dkrt_settings_remove` and `dkrt_settings_flush` below.
//! A `.dkm` module uses the API table (`settings_client`, `settings_get`, `settings_put`, `settings_remove`, api 1.9, `module_*` below): the runtime
//! stores its keys as `<module name>/<key>`, so a module reaches its own prefix only. There is no task check: `settings_put` and `settings_remove`
//! must be called from the UI task (a menu, slot 0, `dkm_init`), not from a service command handler (MIDI task) or an interrupt.
use crate::{block_read, block_write, card, dlog};
use core::ptr::{addr_of, addr_of_mut, read_volatile};
use dkrt_sdk::proto::*;
use dkrt_sdk::settings as set;
use dkrt_sdk::store::Io;
use dkrt_sdk::*;

/// Blocks (1500 a second) with no change before the record is written: 2 s. Without the firmware's block counter the UI-loop passes count instead
/// (`DELAY_PASSES`: about 8 s at the loop's measured 30 to 32 passes a second, docs/findings/37-boot-load.md).
pub const DELAY_BLOCKS: u32 = 3000;
const DELAY_PASSES: u32 = 240;
/// Failed writes after which the record is left dirty no more (the next change tries again).
const RETRIES: u32 = 3;
/// UI-loop passes after which a block counter that has not moved is taken for dead and the record is written anyway (as the boot counter's rule).
const DEAD_PASSES: u32 = 1800;

#[repr(C, align(16))]
pub struct Area([u8; set::AREA]);
/// The record, in RAM (a proof can peek it). The boot's card read DMAs into it, so the proofs' write-watch leaves it out, as the staging buffer.
#[no_mangle]
pub static mut DKRT_SETTINGS: Area = Area([0; set::AREA]);

#[repr(C, align(16))]
pub struct Sec([u8; 512]);
/// The first sector of a copy, read at the boot to choose the copy (a DMA target, like `DKRT_SECBUF`).
#[no_mangle]
pub static mut DKRT_SETTINGS_HDR: Sec = Sec([0; 512]);

/// The state of the record (a proof can peek it).
#[repr(C)]
pub struct State {
    /// 1 once the record is in RAM (read at the boot, or empty).
    pub loaded: u32,
    /// The copy the card's current record is in (0 = A, 1 = B, `set::NONE`: none) and its `seq`.
    pub copy: u32,
    pub card_seq: u32,
    /// 1 when the RAM record holds a change the card does not, and the time of the last change (blocks, or UI-loop passes without a block counter).
    pub dirty: u32,
    pub changed: u32,
    /// Writes done, writes that failed in a row, the last write's result and copies found damaged at the load.
    pub writes: u32,
    pub fails: u32,
    pub last_result: i32,
    pub damaged: u32,
    /// UI-loop passes (the clock when there is no block counter) and the pass of the last change.
    pub passes: u32,
    pub changed_pass: u32,
    /// 1 when the settings store is off: the store of the card reaches into the settings area (written before the area existed) or its headers do not read.
    /// Nothing is read from or written to the area then; `put` and `remove` answer `E_SET_OFF`.
    pub off: u32,
}

#[no_mangle]
pub static mut DKRT_SET: State = State {
    loaded: 0,
    copy: set::NONE,
    card_seq: 0,
    dirty: 0,
    changed: 0,
    writes: 0,
    fails: 0,
    last_result: 0,
    damaged: 0,
    passes: 0,
    changed_pass: 0,
    off: 0,
};

fn state() -> &'static mut State {
    unsafe { &mut *addr_of_mut!(DKRT_SET) }
}

fn area() -> &'static mut [u8; set::AREA] {
    unsafe { &mut (*addr_of_mut!(DKRT_SETTINGS)).0 }
}

unsafe extern "C" fn rd(sector: u32, bytes: u32, buf: *mut u8) -> i32 {
    block_read(sector, bytes, buf)
}

unsafe extern "C" fn wr(sector: u32, bytes: u32, buf: *mut u8) -> i32 {
    block_write(sector, bytes, buf as *const u8)
}

fn io() -> Io {
    Io { rd, wr }
}

/// The time now and the delay in the same unit: blocks when the firmware's counter is there, else UI-loop passes.
unsafe fn clock() -> (u32, u32) {
    match crate::fw_blocks() {
        Some(b) => (b, DELAY_BLOCKS),
        None => (state().passes, DELAY_PASSES),
    }
}

/// Read the card's two copies into the RAM record (the boot hook, after `card::check`). A card with no store (too small) has an empty record that
/// is never written. Logs `SETTINGS_LOAD`.
pub unsafe fn load() {
    let s = state();
    s.loaded = 1;
    s.dirty = 0;
    s.fails = 0;
    s.off = 0;
    if card::state() == CARD_TOO_SMALL {
        set::init(&mut area()[..], 0);
        s.copy = set::NONE;
        s.card_seq = 0;
        return;
    }
    // An older store (written before the settings area, with header B 32 sectors further on) may have a table or a file where the settings copies
    // and header B are now: `store::open` checks that nothing reaches into the tail (and so does every later open, before a commit). No store header
    // at all is a card with no files; a header that does not read or reaches into the tail turns the settings store off: nothing is read or written.
    let opened = if crate::store_writer::acquire() {
        let r = crate::store::open();
        crate::store_writer::unlock();
        r.map(|_| ())
    } else {
        Err(E_BUSY)
    };
    if let Err(e) = opened {
        if e == E_BUSY {
            // the store was in use (an SDK write on the I/O task) for the whole wait: not a damaged store. Nothing is decided; the next call of `ensure` loads again.
            s.loaded = 0;
            return;
        }
        if e != E_NOT_FOUND {
            set::init(&mut area()[..], 0);
            s.copy = set::NONE;
            s.card_seq = 0;
            s.off = 1;
            dlog::ev(logk::SETTINGS_LOAD, logk::F_ERROR, [4, set::NONE, 0, e as u32, 0]);
            return;
        }
    }
    let r = set::load(&io(), &crate::store::layout(), addr_of_mut!(DKRT_SETTINGS_HDR) as *mut u8, area());
    match r {
        Ok(l) => {
            s.copy = l.copy;
            s.card_seq = l.seq;
            s.damaged = l.damaged;
            let n = count_entries();
            // state: 0 none valid, 1 a copy loaded, 2 a copy is damaged and the other one was taken
            let state = if l.copy == set::NONE {
                0
            } else if l.damaged != 0 {
                2
            } else {
                1
            };
            let e = if l.damaged != 0 { logk::F_ERROR } else { dlog::INFO };
            dlog::ev(logk::SETTINGS_LOAD, e, [state, l.copy, l.seq, l.used, n]);
        }
        Err(e) => {
            s.copy = set::NONE;
            s.card_seq = 0;
            dlog::ev(logk::SETTINGS_LOAD, logk::F_ERROR, [3, set::NONE, 0, e as u32, 0]);
        }
    }
}

/// Is the settings store off (see `State::off`)?
pub unsafe fn is_off() -> bool {
    ensure();
    state().off != 0
}

unsafe fn ensure() {
    if read_volatile(addr_of!(DKRT_SET.loaded)) == 0 {
        load();
    }
}

fn count_entries() -> u32 {
    let a = area();
    let u = set::used(&a[..]);
    let mut n = 0;
    let mut it = set::iter(&a[..], u);
    while it.next().is_some() {
        n += 1;
    }
    n
}

unsafe fn touch() {
    let s = state();
    s.dirty = 1;
    s.fails = 0;
    s.changed = clock().0;
    s.changed_pass = s.passes;
}

/// Copy the value of `key` into `out`: its length, or `E_SET_NOKEY`, `E_SET_KEY`, `E_SET_VALUE` (`out` too small).
pub unsafe fn get(key: &[u8], out: &mut [u8]) -> i32 {
    ensure();
    if state().off != 0 {
        return E_SET_OFF;
    }
    set::get(&area()[..], key, out)
}

/// Set `key` to `value`: 0, or `E_SET_KEY`, `E_SET_VALUE`, `E_SET_FULL`. A value the key already has changes nothing (and writes nothing).
pub unsafe fn put(key: &[u8], value: &[u8]) -> i32 {
    ensure();
    if state().off != 0 {
        return E_SET_OFF;
    }
    match set::put(&mut area()[..], key, value) {
        Ok(true) => {
            touch();
            OK
        }
        Ok(false) => OK,
        Err(e) => e,
    }
}

/// Remove `key`: 0 (also when it was not there), or `E_SET_KEY`.
pub unsafe fn remove(key: &[u8]) -> i32 {
    ensure();
    if state().off != 0 {
        return E_SET_OFF;
    }
    match set::remove(&mut area()[..], key) {
        Ok(true) => {
            touch();
            OK
        }
        Ok(false) => OK,
        Err(e) => e,
    }
}

/// The record's size now (header and entries) and the area of one copy: the CAPACITY entry.
pub unsafe fn capacity() -> (u32, u32) {
    ensure();
    if state().off != 0 {
        return (0, 0);
    }
    (set::AREA as u32, set::used(&area()[..]) as u32)
}

/// The record for the SETTINGS command: (flags, the card's current copy, its `seq`, the record's bytes). Flag bit 0: the RAM record has changes that the
/// card does not hold yet (its header then carries the card's `seq`). `None`: no valid copy on the card and nothing changed (no record).
pub unsafe fn snapshot() -> Option<(u8, u32, u32, &'static [u8])> {
    ensure();
    let s = state();
    if s.off != 0 || (s.copy == set::NONE && s.dirty == 0) {
        return None;
    }
    let a = area();
    if s.dirty != 0 {
        set::seal(&mut a[..], s.card_seq);
    }
    Some((s.dirty as u8, s.copy, s.card_seq, &a[..set::used(&a[..])]))
}

/// Write the RAM record to the copy that is not current, with `seq + 1`. Returns the result (0, or `E_WRITE`, `E_RANGE`, `E_CARD_RO`).
pub unsafe fn flush() -> i32 {
    ensure();
    let s = state();
    if s.off != 0 {
        return E_SET_OFF;
    }
    let target = if s.copy == 0 { 1 } else { 0 };
    let seq = s.card_seq.wrapping_add(1);
    set::seal(&mut area()[..], seq);
    let r = set::write(&io(), &crate::store::layout(), target, area());
    s.last_result = r;
    let used = set::used(&area()[..]) as u32;
    dlog::ev(logk::SETTINGS_WRITE, if r == OK { dlog::INFO } else { logk::F_ERROR }, [target, seq, used, r as u32, 0]);
    if r == OK {
        s.copy = target;
        s.card_seq = seq;
        s.dirty = 0;
        s.fails = 0;
        s.writes = s.writes.wrapping_add(1);
    } else if r == E_CARD_RO || r == E_RANGE {
        // the runtime may not write this card: the record holds until the next boot
        s.dirty = 0;
    } else {
        s.fails += 1;
        if s.fails >= RETRIES {
            s.dirty = 0;
        } else {
            s.changed = clock().0;
            s.changed_pass = s.passes;
        }
    }
    r
}

/// Every UI-loop pass: write the record when it has been quiet for the delay.
pub unsafe fn tick() {
    let s = state();
    s.passes = s.passes.wrapping_add(1);
    if s.dirty == 0 {
        return;
    }
    let (now, delay) = clock();
    if now.wrapping_sub(s.changed) >= delay || s.passes.wrapping_sub(s.changed_pass) >= DEAD_PASSES {
        flush();
    }
}

unsafe fn key_of<'a>(key: *const u8) -> &'a [u8] {
    let mut n = 0;
    while n <= set::KEY_MAX && *key.add(n) != 0 {
        n += 1;
    }
    core::slice::from_raw_parts(key, n)
}

/// `dkrt_settings_get(key, buf, cap)`: the value of `key` (NUL-terminated) into `buf` (`cap` bytes): its length, or `E_SET_NOKEY`, `E_SET_KEY`,
/// `E_SET_VALUE` (`cap` too small). UI task.
#[no_mangle]
pub unsafe extern "C" fn dkrt_settings_get(key: *const u8, buf: *mut u8, cap: u32) -> i32 {
    crate::init_once();
    get(key_of(key), core::slice::from_raw_parts_mut(buf, cap as usize))
}

/// `dkrt_settings_put(key, value, len)`: set `key` (NUL-terminated) to the `len` bytes at `value`: 0, or `E_SET_KEY`, `E_SET_VALUE`, `E_SET_FULL`.
/// The card is written about 2 s after the last change. UI task.
#[no_mangle]
pub unsafe extern "C" fn dkrt_settings_put(key: *const u8, value: *const u8, len: u32) -> i32 {
    crate::init_once();
    if len as usize > set::VALUE_MAX {
        return E_SET_VALUE;
    }
    put(key_of(key), core::slice::from_raw_parts(value, len as usize))
}

/// `dkrt_settings_remove(key)`: remove `key` (NUL-terminated): 0 (also when it was not there), or `E_SET_KEY`. UI task.
#[no_mangle]
pub unsafe extern "C" fn dkrt_settings_remove(key: *const u8) -> i32 {
    crate::init_once();
    remove(key_of(key))
}

// ---- the API of a .dkm module (api 1.9) --------------------------------------------------------------------------------------

/// `settings_client`: the module's handle, only inside its `dkm_init` (the check of `files_client`). The handle is the module's record index + 1: the
/// same module gets the same handle every time. `E_NOT_FOUND` outside `dkm_init`.
pub unsafe fn module_client() -> i32 {
    let source = dlog::current_source();
    if source == 0 {
        return E_NOT_FOUND;
    }
    let rec = source as usize - 1;
    if !crate::files::in_init(rec) {
        return E_NOT_FOUND;
    }
    rec as i32 + 1
}

/// The stored key `<module name>/<key>` for `client` and the NUL-terminated `key`: the module name comes from the handle's record, never from the
/// caller. A handle that is 0, past the table or of a record that is not loaded (and not inside its `dkm_init`) is stale: `E_SET_KEY`, the settings
/// argument error, as for a key that breaks the rules. The prefix stops mistakes, not hostile code: modules are native code with no memory protection.
unsafe fn module_stored_key(client: u32, key: *const u8, out: &mut [u8; set::KEY_MAX]) -> Result<usize, i32> {
    if client == 0 || client as usize > crate::MAX_MODULES || key.is_null() {
        return Err(E_SET_KEY);
    }
    let m = &crate::st().modules[client as usize - 1];
    if m.used == 0 {
        return Err(E_SET_KEY);
    }
    let name = core::slice::from_raw_parts(m.name.as_ptr(), crate::cstr_len(m.name.as_ptr(), 31));
    match set::module_key(name, key_of(key), out) {
        Some(n) => Ok(n),
        None => Err(E_SET_KEY),
    }
}

/// `settings_get`: the length of the value (0 is valid), `E_SET_NOKEY`, `E_SET_VALUE` (`cap` too small: nothing is written), `E_SET_KEY` (bad handle or
/// key), `E_SET_OFF`. In `dkm_init` and in the UI task.
pub unsafe fn module_get(client: u32, key: *const u8, out: *mut u8, cap: u32) -> i32 {
    let mut k = [0u8; set::KEY_MAX];
    let n = match module_stored_key(client, key, &mut k) {
        Ok(n) => n,
        Err(e) => return e,
    };
    if out.is_null() && cap != 0 {
        return E_SET_KEY;
    }
    let mut none = [0u8; 0];
    let buf: &mut [u8] = if cap == 0 { &mut none } else { core::slice::from_raw_parts_mut(out, cap as usize) };
    get(&k[..n], buf)
}

/// `settings_put`: 0, or `E_SET_KEY` (bad handle or key, or `value` null with `len` above 0), `E_SET_VALUE` (over 1024 bytes), `E_SET_FULL`, `E_SET_OFF`. UI task.
pub unsafe fn module_put(client: u32, key: *const u8, value: *const u8, len: u32) -> i32 {
    let mut k = [0u8; set::KEY_MAX];
    let n = match module_stored_key(client, key, &mut k) {
        Ok(n) => n,
        Err(e) => return e,
    };
    if len as usize > set::VALUE_MAX {
        return E_SET_VALUE;
    }
    if value.is_null() && len != 0 {
        return E_SET_KEY;
    }
    let v: &[u8] = if len == 0 { &[] } else { core::slice::from_raw_parts(value, len as usize) };
    put(&k[..n], v)
}

/// `settings_remove`: 0 (also when the key was not there), or `E_SET_KEY` (bad handle or key), `E_SET_OFF`. UI task.
pub unsafe fn module_remove(client: u32, key: *const u8) -> i32 {
    let mut k = [0u8; set::KEY_MAX];
    let n = match module_stored_key(client, key, &mut k) {
        Ok(n) => n,
        Err(e) => return e,
    };
    remove(&k[..n])
}

/// Write the record now when it has changes the card does not hold: 0 (also when nothing was changed), or `E_WRITE`, `E_RANGE`, `E_CARD_RO`, `E_SET_OFF`. UI task.
pub unsafe fn flush_if_dirty() -> i32 {
    ensure();
    if state().off != 0 {
        return E_SET_OFF;
    }
    if state().dirty == 0 {
        return OK;
    }
    flush()
}

/// `dkrt_settings_flush()`: write the record to the card now when it has changes the card does not hold (a menu does this before it restarts, so the change is not lost
/// to the 2 s delay; `dkrt_restart` does it too): 0 (also when nothing was changed), or `E_WRITE`, `E_RANGE`, `E_CARD_RO`, `E_SET_OFF`. UI task.
#[no_mangle]
pub unsafe extern "C" fn dkrt_settings_flush() -> i32 {
    crate::init_once();
    flush_if_dirty()
}

/// The mailbox test commands: `arg` = `u8 key_len`, the key, `u8 value_len`, the value (96 bytes in all). `CMD_SETTINGS_GET` answers the value's
/// length and leaves the value in `arg`; `CMD_SETTINGS_PUT` and `CMD_SETTINGS_REMOVE` answer their result; `CMD_SETTINGS_FLUSH` writes now.
pub unsafe fn mailbox(cmd: u32, arg: *mut u8) -> i32 {
    if cmd == CMD_SETTINGS_FLUSH {
        return flush();
    }
    let kl = *arg as usize;
    if kl == 0 || 1 + kl + 1 > 96 {
        return E_SET_KEY;
    }
    let key = core::slice::from_raw_parts(arg.add(1), kl);
    let vl = *arg.add(1 + kl) as usize;
    if cmd == CMD_SETTINGS_PUT {
        if 1 + kl + 1 + vl > 96 {
            return E_SET_VALUE;
        }
        return put(key, core::slice::from_raw_parts(arg.add(2 + kl), vl));
    }
    if cmd == CMD_SETTINGS_REMOVE {
        return remove(key);
    }
    // CMD_SETTINGS_GET: the value comes back in `arg` from its start (the key is copied out first)
    let mut k = [0u8; set::KEY_MAX];
    if kl > set::KEY_MAX {
        return E_SET_KEY;
    }
    let mut i = 0;
    while i < kl {
        k[i] = key[i];
        i += 1;
    }
    let out = core::slice::from_raw_parts_mut(arg, 96);
    get(&k[..kl], out)
}
