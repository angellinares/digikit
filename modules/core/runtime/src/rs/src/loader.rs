//! The `.dkm` loader (docs/design/digikit-dkm.md). Every check runs before anything is written to the arena.

use crate::dsp;
use crate::handlers::{services, ServiceTable};
use crate::selection::BOOT_IDX;
use crate::sha256::Sha;
use crate::entry::Ctx;
use crate::{
    arena, cstr_eq, cstr_len, flush, import_addr, rd32, st, store, wr32, Module,
    MAX_MODULES,
};
use core::ptr::{addr_of, addr_of_mut, read_volatile};
use dkrt_sdk::*;

const MAGIC: u32 = dkneeds::MAGIC;
/// The header size. `init`, `fini`, `menu` and `tick` are image offsets in it (NONE for a missing one): the fields of the module's `dk_module` constant
/// (`dk_module_def`), called with a `dk_module_ctx *` (`entry.rs`).
const HEADER: u32 = dkneeds::HEADER;
const NONE: u32 = dkneeds::NONE;

// header field offsets (big endian): `[format]` of sdk/sdk.toml
const H_SIZE: usize = dkneeds::H_SIZE as usize;
const H_HDRSIZE: usize = dkneeds::H_HDRSIZE as usize;
const H_SHA: usize = dkneeds::H_SHA as usize;
const H_RUNTIME: usize = dkneeds::H_RUNTIME as usize;
const H_VERSION: usize = dkneeds::H_VERSION as usize;
const H_NAME: usize = dkneeds::H_NAME as usize;
const H_IMAGE_OFF: usize = dkneeds::H_IMAGE_OFF as usize;
const H_IMAGE_SIZE: usize = dkneeds::H_IMAGE_SIZE as usize;
const H_BSS: usize = dkneeds::H_BSS as usize;
const H_ALIGN: usize = dkneeds::H_ALIGN as usize;
const H_RELOC_OFF: usize = dkneeds::H_RELOC_OFF as usize;
const H_RELOC_N: usize = dkneeds::H_RELOC_N as usize;
const H_IMPORT_OFF: usize = dkneeds::H_IMPORT_OFF as usize;
const H_IMPORT_N: usize = dkneeds::H_IMPORT_N as usize;
const H_INIT: usize = dkneeds::H_INIT as usize;
const H_FINI: usize = dkneeds::H_FINI as usize;
const H_MENU: usize = dkneeds::H_MENU as usize;
const H_TICK: usize = dkneeds::H_TICK as usize;
const H_REG_OFF: usize = dkneeds::H_REG_OFF as usize;
const H_REG_SIZE: usize = dkneeds::H_REG_SIZE as usize;
const H_DSP_OFF: usize = dkneeds::H_DSP_OFF as usize;
const H_DSP_SIZE: usize = dkneeds::H_DSP_SIZE as usize;

unsafe fn in_file(off: u32, len: u32, size: u32) -> bool {
    off <= size && len <= size - off
}

/// Validate and load the `.dkm` at `path`. See `dkrt_load`. The tables are checked after every attempt, and none starts once a check has
/// failed (`invariants::check`).
pub unsafe fn load(path: *const u8) -> i32 {
    if crate::invariants::bad() {
        return E_TABLE;
    }
    let r = load_inner(path);
    #[cfg(not(test))]
    crate::invariants::check();
    r
}

unsafe fn load_inner(path: *const u8) -> i32 {
    dsp::init_once();
    // the module set is fixed per session: only the boot hook loads (hot reload is archived, docs/plans/digikit-hot-reload.md)
    if !EARLY {
        return E_BOOT_ONLY;
    }
    let found = match store::find(path) {
        Ok(f) => f,
        Err(e) => return e,
    };
    let r = store::load_to_stage(&found);
    if r != OK {
        return r;
    }
    let f = store::stage() as *const u8;
    let size = found.size;

    // ---- checks (nothing is written outside the staging buffer) ----
    if size < HEADER || rd32(f) != MAGIC || rd32(f.add(4)) != 1 {
        return E_FORMAT;
    }
    let hdr = rd32(f.add(H_HDRSIZE));
    if rd32(f.add(H_SIZE)) != size || hdr != HEADER {
        return E_FORMAT;
    }
    // the file's own sha256, over the file with the sha field read as zeros
    let mut want = [0u8; 32];
    let mut i = 0;
    while i < 32 {
        want[i] = *f.add(H_SHA + i);
        i += 1;
    }
    let mut sha = Sha::new();
    sha.update(f, H_SHA as u32);
    sha.update_zeros(32);
    sha.update(f.add(H_SHA + 32), size - (H_SHA as u32 + 32));
    let mut got = [0u8; 32];
    sha.finish(got.as_mut_ptr());
    if got != want {
        return E_SHA;
    }
    // the runtime version the module needs: another major or minor than the runtime's, or a higher patch, refuses (0.x: a minor breaks)
    let needs = rd32(f.add(H_RUNTIME));
    if !crate::runtime_accepts(needs) {
        return E_CONTRACT;
    }
    let name = f.add(H_NAME);
    let nlen = cstr_len(name, dkneeds::NAME_LEN as usize);
    if nlen == 0 || nlen == dkneeds::NAME_LEN as usize {
        return E_FORMAT;
    }
    let image_off = rd32(f.add(H_IMAGE_OFF));
    let image_size = rd32(f.add(H_IMAGE_SIZE));
    let bss = rd32(f.add(H_BSS));
    let align = rd32(f.add(H_ALIGN));
    let reloc_off = rd32(f.add(H_RELOC_OFF));
    let reloc_n = rd32(f.add(H_RELOC_N));
    let import_off = rd32(f.add(H_IMPORT_OFF));
    let import_n = rd32(f.add(H_IMPORT_N));
    let init = rd32(f.add(H_INIT));
    let fini = rd32(f.add(H_FINI));
    let menu = rd32(f.add(H_MENU));
    let tick = rd32(f.add(H_TICK));
    if align == 0
        || align > 16
        || align & (align - 1) != 0
        || image_size == 0
        || image_size > size
        || bss > crate::ARENA_BYTES as u32
    {
        return E_FORMAT;
    }
    if !in_file(image_off, image_size, size)
        || image_off & 15 != 0
        || reloc_n > dkneeds::MAX_RELOCS
        || import_n > dkneeds::MAX_CF_IMPORTS
        || !in_file(reloc_off, reloc_n * dkneeds::CF_RELOC_LEN, size)
        || !in_file(import_off, import_n * dkneeds::CF_IMPORT_LEN, size)
    {
        return E_FORMAT;
    }
    let total = image_size + bss;
    if (init != NONE && init >= image_size)
        || (fini != NONE && fini >= image_size)
        || (menu != NONE && menu >= image_size)
        || (tick != NONE && tick >= image_size)
    {
        return E_FORMAT;
    }
    // imports: every name must be in the runtime's table
    let mut import_addrs = [0u32; 64];
    i = 0;
    while i < import_n as usize {
        let p = f.add(import_off as usize + i * dkneeds::CF_IMPORT_LEN as usize);
        if cstr_len(p, dkneeds::CF_IMPORT_LEN as usize) == dkneeds::CF_IMPORT_LEN as usize {
            return E_FORMAT;
        }
        let a = import_addr(p);
        if a == 0 {
            // the first import that belongs to a module that is not in the image: the boot keeps its name (`MISSING`)
            if let (Some(n), true) = (crate::import_needs(p), MISSING[0] == 0) {
                let b = n.as_bytes();
                let mut k = 0;
                while k < b.len() && k < NEEDS_LEN {
                    MISSING[k] = b[k];
                    k += 1;
                }
            }
            return E_IMPORT;
        }
        import_addrs[i] = a;
        i += 1;
    }
    // relocations: kind, bounds, import index
    i = 0;
    while i < reloc_n as usize {
        let e = f.add(reloc_off as usize + i * dkneeds::CF_RELOC_LEN as usize);
        let off = rd32(e.add(dkneeds::CF_RELOC_OFFSET as usize));
        let kind = rd32(e.add(dkneeds::CF_RELOC_KIND as usize));
        let k = kind >> 24;
        if off > image_size - 4 || (k != 0 && k != 1) || (k == 1 && (kind & 0xff_ffff) >= import_n)
        {
            return E_FORMAT;
        }
        i += 1;
    }
    let s = st();
    // `register` records (machines, parameters): validated now, written after the copy
    let (reg_off, reg_size) = (rd32(f.add(H_REG_OFF)), rd32(f.add(H_REG_SIZE)));
    if !in_file(reg_off, reg_size, size) {
        return E_FORMAT;
    }
    let reg = f.add(reg_off as usize);
    // the table is the keys table; the records come from the builder calls of `init` (complete)
    let keys = match crate::collect::KeysCopy::from_bytes(core::slice::from_raw_parts(reg, reg_size as usize)) {
        Ok(k) => k,
        Err(e) => return e,
    };
    // the DSP section, if any
    let (dsp_off, dsp_size) = (rd32(f.add(H_DSP_OFF)), rd32(f.add(H_DSP_SIZE)));
    let mut job = dsp::Job::empty();
    if dsp_size != 0 {
        let r = dsp::check(f, dsp_off, dsp_size, size, &mut job);
        if r != OK {
            return r;
        }
        let (v, stride) = (job.voices, job.vstride as usize);
        let r = crate::collect::check_dsp_keys(&keys, job.nvoice as usize, |i| rd32(v.add(stride * i)), job.nfx as usize);
        if r != OK {
            return r;
        }
    }
    // already loaded; a free record
    let rec = match capacity::pick_record(MAX_MODULES, |i| s.modules[i].used != 0, |i| cstr_eq(s.modules[i].name.as_ptr(), name)) {
        Ok(r) => r,
        Err(e) => return e,
    };

    // ---- allocate, copy, relocate, zero bss ----
    // The DSP range, the relocated DSP image and the voice rows come first: nothing else is allocated yet if they fail.
    if dsp_size != 0 {
        let r = dsp::reserve(rec, &job);
        if r != OK {
            return r;
        }
    }
    let mem = arena::alloc(total);
    if mem.is_null() {
        if dsp_size != 0 {
            dsp::cancel(rec);
        }
        return E_NOMEM;
    }
    let base = mem as u32;
    i = 0;
    while i < image_size as usize {
        *mem.add(i) = *f.add(image_off as usize + i);
        i += 1;
    }
    i = 0;
    while i < bss as usize {
        *mem.add(image_size as usize + i) = 0;
        i += 1;
    }
    i = 0;
    while i < reloc_n as usize {
        let e = f.add(reloc_off as usize + i * dkneeds::CF_RELOC_LEN as usize);
        let off = rd32(e.add(dkneeds::CF_RELOC_OFFSET as usize)) as usize;
        let kind = rd32(e.add(dkneeds::CF_RELOC_KIND as usize));
        let add = if kind >> 24 == 0 {
            base
        } else {
            import_addrs[(kind & 0xff_ffff) as usize]
        };
        let p = mem.add(off);
        wr32(p, rd32(p).wrapping_add(add));
        i += 1;
    }
    // ---- caches: write the lines back and drop them from both caches ----
    flush(base, total);

    // ---- record, init, publish ----
    let m: &mut Module = &mut s.modules[rec];
    m.used = 1;
    i = 0;
    while i < dkneeds::NAME_LEN as usize {
        m.name[i] = *name.add(i);
        i += 1;
    }
    let dst = &mut (*addr_of_mut!(crate::DKRT_PATHS))[rec];
    let plen = cstr_len(path, 63);
    i = 0;
    while i < 64 {
        dst[i] = if i < plen { *path.add(i) } else { 0 };
        i += 1;
    }
    m.version = rd32(f.add(H_VERSION));
    (*addr_of_mut!(crate::DKRT_MODULE_RUNTIME))[rec] = needs & !crate::FLAG_TRACE;
    m.base = base;
    m.size = total;
    m.fini = if fini == NONE { 0 } else { base + fini };
    crate::entry::bind(rec);
    (*addr_of_mut!(crate::DKRT_MENUS))[rec] = if menu == NONE { 0 } else { base + menu };
    let initaddr = if init == NONE { 0 } else { base + init };
    let tickaddr = if tick == NONE { 0 } else { base + tick };
    if dsp_size != 0 {
        // the CF part goes live now (rows in place before the machine view is built); the DSP code and the voice rows follow
        // when the frames flow
        m.used = 1;
        let r = complete(rec, &keys, initaddr, tickaddr, image_size);
        if r != OK {
            if dsp::retained() { return E_BUSY; }
            dsp::cancel(rec);
            if !crate::files::retire(rec) || !retire_sdk(rec) { return E_BUSY; }
            arena::free(mem);
            m.used = 0;
            crate::entry::unbind(rec);
            return r;
        }
        dsp::launch(dsp::Pending {
            rec: rec as u32,
            reg: 0,
            reg_size: 0,
            init: 0,
            early: 1,
            boot_idx: read_volatile(addr_of!(BOOT_IDX)),
            send_base: 0,
            send_len: 0,
            send_buf: 0,
        });
        return LOADING;
    }
    let r = complete(rec, &keys, initaddr, tickaddr, image_size);
    if r != OK {
        if dsp::retained() || !crate::files::retire(rec) || !retire_sdk(rec) { return E_BUSY; }
        arena::free(mem);
        m.used = 0;
        crate::entry::unbind(rec);
    }
    r
}

/// The end of a load, shared by the immediate and the DSP path (both run in the boot hook; docs/plans/sdk.md 6.3): `init` runs first and its builder calls are collected; then the commit checks them, fills the
/// voice rows' ranges, writes the registry rows and keeps the record block. Anything that fails undoes what `init` did and writes no row.
unsafe fn complete(rec: usize, keys: &crate::collect::KeysCopy, init: u32, tick: u32, image_size: u32) -> i32 {
    let s = st();
    let owner = rec as u8 + 1;
    crate::files::init_begin(rec);
    let block = arena::alloc(core::mem::size_of::<crate::collect::Collect>() as u32);
    let fail = |r: i32| -> i32 {
        crate::intro::discard(rec);
        let held = !crate::files::retire(rec) | !retire_sdk(rec);
        crate::module_cmds::release_cmds(owner);
        crate::dlog::release_sinks(rec);
        crate::registry::remove(owner);
        if !dsp::release(rec) || held {
            return E_BUSY;
        }
        r
    };
    if block.is_null() {
        return fail(E_NOMEM);
    }
    crate::collect::begin(block as *mut crate::collect::Collect);
    if init != 0 {
        let call: unsafe extern "C" fn(*const Ctx) -> i32 = core::mem::transmute(init as usize);
        let ctx = crate::entry::ctx_addr(rec) as usize as *const Ctx;
        CALLED_NEED = [0; NEEDS_LEN];
        let r = crate::dlog::call_init(rec as usize, call, ctx);
        if r != 0 {
            crate::collect::end();
            arena::free(block);
            let missing = r == E_MISSING && CALLED_NEED[0] != 0;
            if missing && MISSING[0] == 0 {
                MISSING = CALLED_NEED;
            }
            let e = fail(if missing { E_IMPORT } else { E_INIT });
            return if e == E_BUSY { e } else if missing { E_IMPORT } else { E_INIT };
        }
    }
    crate::collect::end();
    let r = commit(rec, &*(block as *const crate::collect::Collect), keys, image_size);
    arena::free(block);
    if r != OK {
        return fail(r);
    }
    if tick != 0 {
        crate::entry::publish_tick(rec, tick);
    }
    let m: &mut Module = &mut s.modules[rec];
    m.used = 1;
    s.loads = s.loads.wrapping_add(1);
    crate::files::ready(rec);
    crate::intro::commit(rec);
    OK
}

/// The commit of 6.3 steps 1 to 3: check, fill the voice rows, apply the records; the record block stays in the arena.
unsafe fn commit(rec: usize, c: &crate::collect::Collect, keys: &crate::collect::KeysCopy, image_size: u32) -> i32 {
    use crate::collect as col;
    let owner = rec as u8 + 1;
    let base = st().modules[rec].base;
    // the declarations of a service module against its header table (declare-check-commit)
    if !col::decl_matches(c, keys, base) {
        return E_DECL_KIND;
    }
    let declared = c.nm + c.nf + c.nparam + c.np + c.ng != 0;
    let mut block: *mut u8 = core::ptr::null_mut();
    let mut size = 0usize;
    let mut rows = (0u32, 0u32, 0u32);
    if declared {
        let p = match crate::registry::profile() {
            Some(p) => p,
            None => return E_NO_REGISTRY,
        };
        let free = match crate::registry::free_rows() {
            Some(f) => f,
            None => return E_NO_REGISTRY,
        };
        let mut loaded = [0u32; MAX_MODULES * col::MAX_RECORDS];
        let n = col::loaded_fx_ids(rec, &mut loaded);
        let r = col::check_commit(c, &p, keys, &loaded[..n], &free);
        if r != OK {
            return r;
        }
        let mut plan = col::Plan::new();
        let mut voice = [false; limits::MACHINE_ROWS];
        let mut m = 0;
        while m < c.nm {
            voice[m] = services().has_voice(rec, crate::assign::resolve(col::machine_key(keys, &c.machines[m])));
            m += 1;
        }
        size = match col::lower(c, &p, keys, &voice, base, core::ptr::null_mut(), 0, &mut plan) {
            Ok(n) => n,
            Err(e) => return e,
        };
        block = arena::alloc(size as u32);
        if block.is_null() {
            return E_NOMEM;
        }
        if let Err(e) = col::lower(c, &p, keys, &voice, base, block, size, &mut plan) {
            arena::free(block);
            return e;
        }
        let r = crate::registry::check(block, size as u32, image_size);
        if r != OK {
            arena::free(block);
            return r;
        }
        // the ranges and latch mask of each machine's voice row, the routes and defaults of each effect's row
        let mut m = 0;
        while m < c.nm {
            let t = crate::assign::resolve(col::machine_key(keys, &c.machines[m]));
            if services().has_voice(rec, t) {
                let (rng, latched) = col::voice_declared(c, &plan, m);
                if latched != 0 {
                    crate::dlog::ev(
                        logk::MODULE_LATCH,
                        crate::dlog::INFO,
                        [read_volatile(addr_of!(BOOT_IDX)), m as u32, latched, 0, 0],
                    );
                }
                let r = services().declare_voice(rec, t, rng, latched).unwrap_or(OK);
                if r != OK {
                    arena::free(block);
                    return r;
                }
            }
            m += 1;
        }
        let mut f = 0;
        while f < c.nf {
            let fx = &c.fxs[f];
            let ord = fx.index; // the index in the keys table is the ordinal of the effect record
            // without `dk_fx_routes` the effect runs on the master only (the rule of the old `routes` field: unset means the default route)
            let flags = Some(col::fx_flags_of_decl(fx));
            let par = if fx.page >= 0 { Some(col::fx_defaults(c, f)) } else { None };
            if (flags.is_some() || par.is_some()) && !services().declare_fx(rec, ord, flags, par).unwrap_or(false) {
                arena::free(block);
                return E_FORMAT; // no such effect in the module's DSP section
            }
            f += 1;
        }
        crate::registry::set_args(Some(col::apply_args(c, &plan)));
        let r = crate::registry::apply(owner, block, size as u32, base);
        crate::registry::set_args(None);
        if r != OK {
            arena::free(block);
            return r;
        }
        rows = col::counts_of(c, &p);
    }
    col::keep(rec, block as u32, size as u32, keys);
    crate::dlog::ev(
        logk::MODULE_COMMIT,
        crate::dlog::INFO,
        [read_volatile(addr_of!(BOOT_IDX)), rows.0 | rows.1 << 8 | rows.2 << 16, block as u32, size as u32, 0],
    );
    OK
}

/// The DSP part of a module published at boot failed (the machine rows and `init` are in already): take it down again.
pub unsafe fn undo_early(rec: usize) -> i32 {
    unload_rec(rec)
}

/// One UI-loop pass of the DSP loader. `dsp` only advances its state and reports what it found; the module teardown, the machine
/// listing and the boot result belong here, so `dsp` depends on neither the loader nor the boot.
pub unsafe fn dsp_step() {
    dsp_step_with(services())
}

/// `dsp_step` over a given list of services.
pub(crate) unsafe fn dsp_step_with(l: ServiceTable) {
    match dsp::step_with(l) {
        dsp::Advance::Pending => {}
        dsp::Advance::Failed(code) => {
            let (rec, idx) = {
                let p = &dsp::ds().pending;
                (p.rec as usize, p.boot_idx)
            };
            // Teardown invalidates once, after the tick is stopped, before any fini/free.
            let reclaimed = undo_early(rec) == OK;
            crate::boot::note_result(idx, code);
            dsp::fail_done(reclaimed, code);
        }
        dsp::Advance::Loaded => {
            let (rec, idx) = {
                let p = &dsp::ds().pending;
                (p.rec as usize, p.boot_idx)
            };
            l.on_loaded(rec);
            crate::boot::note_result(idx, OK);
            dsp::loaded_done();
        }
    }
}

/// While the boot hook loads the selection: the CF part of a module with DSP code is published at once (the machine view is built
/// soon after the hook, and the frames that carry DSP code do not flow yet); the DSP part follows over the UI-loop passes.
pub static mut EARLY: bool = false;
/// The name of the missing module of the last load that failed with E_IMPORT (`import_needs`), NUL padded; empty when none. The boot clears it
/// before each load and takes it after.
pub static mut MISSING: [u8; NEEDS_LEN] = [0; NEEDS_LEN];
/// The provider of the last `_open` stub that answered DK_E_MISSING since the clear before `init` (`stub_missing`), NUL padded; empty when none.
/// When `init` returns DK_E_MISSING, `MISSING` takes it and the load fails with E_IMPORT, as for an unresolved import.
static mut CALLED_NEED: [u8; NEEDS_LEN] = [0; NEEDS_LEN];

/// Called by the stub of an absent service's `_open`: records `provider` (`core/usb-bulk` becomes `usb-bulk`) as the need, replacing an earlier one.
pub fn stub_missing(provider: &'static str) {
    let n = match provider.rfind('/') {
        Some(at) => &provider[at + 1..],
        None => provider,
    };
    let b = n.as_bytes();
    unsafe {
        CALLED_NEED = [0; NEEDS_LEN];
        let mut k = 0;
        while k < b.len() && k < NEEDS_LEN {
            CALLED_NEED[k] = b[k];
            k += 1;
        }
    }
}

extern "C" {
    /// `core/dsp-link`: clears the per-block callback when it points into [lo, hi). Resolves to 0 when the module is not in the image.
    fn dl_tick_cb_drop();
}

/// After a module's `fini`: no per-block callback of `core/dsp-link` may point into its memory (a module that forgot to clear it).
unsafe fn drop_tick_cb(base: u32, size: u32) {
    let a = core::hint::black_box(dl_tick_cb_drop as unsafe extern "C" fn() as usize);
    if a != 0 {
        let f: unsafe extern "C" fn(u32, u32) = core::mem::transmute(a);
        f(base, base.wrapping_add(size));
    }
}

/// Take module record `rec` down: its tick, `fini`, machine rows, DSP voice rows and range, memory. Used only
/// by the DSP loader when the DSP part of a module published at boot fails (`undo_early`). The run-time `unload` that also called
/// it (with the in-use scan) is archived with hot reload. The tables are checked after it (`invariants::check`).
pub unsafe fn unload_rec(rec: usize) -> i32 {
    let r = unload_rec_inner(rec);
    #[cfg(not(test))]
    crate::invariants::check();
    r
}

/// The module `rec` is going away (an unload or a rollback): its I/O jobs end without `done`, and a file it was writing is dropped. False: an
/// I/O step of it is running (it is cancelled); the caller keeps the module's memory (`E_BUSY`) until a later call returns true.
pub(crate) unsafe fn retire_sdk(rec: usize) -> bool {
    if !crate::io::retire(rec) {
        return false;
    }
    crate::store_writer::retire(rec as u8 + 1);
    true
}

unsafe fn unload_rec_inner(rec: usize) -> i32 {
    if !crate::files::retire(rec) { return E_BUSY; }
    if !crate::worker::retire(rec) { return E_BUSY; }
    if !retire_sdk(rec) { return E_BUSY; }
    // an intro of this module that has not run `done` yet: `done` must not point into freed code
    crate::intro::discard(rec);
    let s = st();
    let m: &mut Module = &mut s.modules[rec];
    // 1. clear the tick (one aligned store) so no new call starts; the UI task, where it runs, is this task
    crate::entry::stop_tick(rec);
    // DSP rows must be invalidated before fini can mutate/release module state.
    if !dsp::quiesce(rec) { return E_BUSY; }
    // 2. fini, 3. free
    if m.fini != 0 {
        #[cfg(test)] crate::cleanup_tests::fini(rec, m.fini);
        #[cfg(not(test))] {
            let call: unsafe extern "C" fn(*const Ctx) = core::mem::transmute(m.fini as usize);
            let ctx = crate::entry::ctx_addr(rec) as usize as *const Ctx;
            crate::dlog::call_fini(rec as usize, call, ctx);
        }
    }
    // a sink it set is released with it, with or without a `fini`; nothing may point into its range
    crate::dlog::release_sinks(rec);
    crate::dlog::sink_drop(m.base, m.base.wrapping_add(m.size));
    // the SysEx commands it registered are released with it
    crate::module_cmds::release_cmds(rec as u8 + 1);
    drop_tick_cb(m.base, m.size);
    crate::api::poll_drop_range(m.base, m.size);
    // the machine row first, then the DSP voice rows, then the range in the DSP area
    #[cfg(test)] crate::cleanup_tests::registry_remove(rec as u8 + 1);
    #[cfg(not(test))] crate::registry::remove(rec as u8 + 1);
    let committed = crate::collect::release(rec);
    if committed != 0 {
        arena::free(committed as *mut u8);
    }
    dsp::release_range(rec);
    arena::free(m.base as *mut u8);
    m.used = 0;
    (*addr_of_mut!(crate::DKRT_MENUS))[rec] = 0;
    crate::entry::unbind(rec);
    OK
}
