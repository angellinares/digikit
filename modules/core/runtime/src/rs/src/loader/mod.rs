//! The `.dkm` loader (docs/design/digikit-dkm.md). Every check runs before anything is written to the arena.

use crate::dsp;
use crate::entry::Ctx;
use discover::Found;
use crate::handlers::{services, ServiceTable};
use crate::plan::{self, Env, Plan, TABLE_MAX};
use dkneeds::keys::Keys;
use install::Installer;
use role::{PlaceArgs, RoleSteps};
use exports::Exports;
use crate::platform::RealPlatform;
use crate::selection::BOOT_IDX;
use crate::{cstr_eq, cstr_len, rd32, st, wr32, Module, MAX_MODULES};
use core::ptr::{addr_of, addr_of_mut, read_volatile};
use dkrt_abi::platform::Platform;
use dkrt_sdk::*;

/// `init`, `fini`, `menu` and `tick` are image offsets in the header (NONE for a missing one): the fields of the module's `dk_module` constant
/// (`dk_module_def`), called with a `dk_module_ctx *` (`entry.rs`).
const NONE: u32 = dkneeds::NONE;

// header field offsets (big endian): `[format]` of sdk/sdk.toml; the parse in `image.rs` reads the rest
const H_VERSION: usize = dkneeds::H_VERSION as usize;
const H_NAME: usize = dkneeds::H_NAME as usize;
const H_REG_OFF: usize = dkneeds::H_REG_OFF as usize;
const H_REG_SIZE: usize = dkneeds::H_REG_SIZE as usize;
const H_DSP_OFF: usize = dkneeds::H_DSP_OFF as usize;
const H_DSP_SIZE: usize = dkneeds::H_DSP_SIZE as usize;

pub(crate) mod discover;
pub(crate) mod exports;
mod image;
// `restore` is kept for hot reload (no caller yet)
#[allow(dead_code)]
pub(crate) mod install;
mod records;
pub(crate) mod role;
mod seams;
#[cfg(test)]
mod tests;
use image::in_file;
pub use seams::{Block, Images, RealImages, RealRegistry, Registry, Staged};

/// The index the log shows for a provider file (`MOD_LOAD` and the events of its load): not a selection index, and not `u32::MAX` (no load running).
pub const PROVIDER_IDX: u32 = 0xffff_fffe;

/// What a provider record keeps after its load started: the copy of its header table (BIND, EXPORTS and SLOTS; the staged file is gone once `init` reads
/// from the store) and the size of its image.
#[derive(Clone, Copy)]
pub(crate) struct ProvRec {
    pub block: Option<Block>,
    pub len: u32,
    pub image_size: u32,
}

impl ProvRec {
    pub const EMPTY: ProvRec = ProvRec { block: None, len: 0, image_size: 0 };
}

/// Which loads the loader accepts. The module set is fixed per session: only the boot hook loads (hot reload is archived, docs/plans/digikit-hot-reload.md).
#[derive(Clone, Copy, PartialEq, Eq)]
enum Phase {
    /// The state after reset and after the boot's loop: `load` is `E_BOOT_ONLY`.
    Closed = 0,
    /// While the boot hook loads the selection: the CF part of a module with DSP code is published at once (the machine view is built soon after the
    /// hook, and the frames that carry DSP code do not flow yet); the DSP part follows over the UI-loop passes.
    Boot = 1,
}

/// The module lifecycle: stage, check, place, init, commit (and DSP) with an undo for each step, and the boot's planning tables. One zero-initialised static
/// (`LOADER`); its methods take `&mut self`.
///
/// What module code can reach while `init` or `fini` runs is not in here: the module records (`DKRT_STATE.modules`), `DKRT_PATHS`, `DKRT_MENUS`,
/// `DKRT_MODULE_RUNTIME`, `DKRT_CTXS`, `DKRT_TICKS`, `BOOT_IDX`, `CALLED_NEED`, `collect::ACTIVE`, `DKRT_BOOT` and the service rows. The loader is their
/// only writer.
pub struct Loader<P: Platform, M: Images, R: Registry> {
    p: P,
    m: M,
    r: R,
    phase: Phase,
    /// The role word of each record's file (0 module, 1 service): which `RoleSteps` its undo uses.
    roles: [u8; records::REC_MAX],
    /// The name of the missing module of the last load that failed with E_IMPORT (`import_needs`), NUL padded; empty when none. The boot clears it before
    /// each load and takes it after.
    missing: [u8; NEEDS_LEN],
    /// The first item that `init`'s declarations and the header table do not both hold, of the last load that failed with E_DECL_KIND: (tag of the item, its first
    /// word), and 0x100 added to the tag when only the header has it. `(0, 0)`: none (`report_provider` logs it).
    decl_diff: (u32, u32),
    /// The header table of the module at each selection index, kept while `assign::plan` reads each file once (`capture`); `table_len` 0: none (the file
    /// is missing, torn, of the old format or has a larger table).
    tables: [[u8; TABLE_MAX]; SEL_MAX],
    table_len: [usize; SEL_MAX],
    /// The DSP load of the accepted modules of the last plan: (load, largest per-voice cost), which `dkrt_capacity` reports as used and largest of
    /// `CAPK_DSP_COST`; the module set is fixed per session, so the plan is the load.
    planned_dsp: (u32, u32),
    /// The providers the boot's search found: their paths and tables. Provider `j` is record `MAX_MODULES + j`.
    found: Found,
    /// Whether provider `j` of the plan loaded (settle reads it).
    loaded: [bool; PROVIDER_MAX],
    /// The table copy of each provider record (`role.rs`).
    provs: [ProvRec; PROVIDER_MAX],
    /// The functions the loaded providers offer by name; an import that the runtime's table lacks resolves here.
    exports: Exports,
    /// The hook sites of the loaded providers. Not armed in safe mode.
    installer: Installer,
}

/// The loader of the device.
type Real = Loader<RealPlatform, RealImages, RealRegistry>;

static mut LOADER: Real = Loader::new(RealPlatform, RealImages, RealRegistry);

fn loader() -> &'static mut Real {
    unsafe { &mut *addr_of_mut!(LOADER) }
}

/// The boot's load loop over the selection in plan order (`Loader::boot`).
pub unsafe fn boot(r: &dkrt_sdk::sel::Record, planned: &Plan) {
    loader().boot(r, planned)
}

/// Validate and load the `.dkm` at `path`. See `dkrt_load`.
pub unsafe fn load(path: *const u8) -> i32 {
    loader().load(path)
}

/// The boot's provider stage (`Loader::load_providers`): the providers of the plan load, in dependency order, before any module.
pub unsafe fn load_providers(planned: &Plan) {
    loader().load_providers(planned)
}

/// The settle step (`Loader::settle`): a module whose provider did not load is refused.
pub unsafe fn settle(planned: &mut Plan, count: usize) {
    loader().settle(planned, count)
}

/// Safe mode: the installer writes nothing.
pub unsafe fn set_safe() {
    loader().installer.set_armed(false)
}

/// The DSP part of a module published at boot failed: take it down again.
pub unsafe fn undo_early(rec: usize) -> i32 {
    loader().undo_early(rec)
}

/// Take module record `rec` down (`Loader::unload_rec`).
pub unsafe fn unload_rec(rec: usize) -> i32 {
    loader().unload_rec(rec)
}

/// One UI-loop pass of the DSP loader. `dsp` only advances its state and reports what it found; the module teardown, the machine
/// listing and the boot result belong to the loader, so `dsp` depends on neither the loader nor the boot.
pub unsafe fn dsp_step() {
    dsp_step_with(services())
}

/// `dsp_step` over a given list of services.
pub(crate) unsafe fn dsp_step_with(l: ServiceTable) {
    loader().dsp_step_with(l)
}

/// Keep the header table of the `.dkm` `file` (the whole file in the staging buffer) for selection index `k`.
pub unsafe fn capture(k: usize, file: &[u8]) {
    loader().capture(k, file)
}

/// Forget the tables (the start of a boot's planning).
pub unsafe fn reset_plan() {
    loader().reset_plan()
}

/// Plan the selection `count` modules long from the tables `capture` kept, with the providers the search found for its hard needs (`Loader::plan_with_caps`).
pub unsafe fn plan_selection(count: usize) -> Plan {
    let mut caps = [DkrtCap { kind: 0, limit: 0, used: 0, largest: 0 }; CAP_MAX];
    let k = crate::dkrt_capacity(caps.as_mut_ptr(), CAP_MAX as u32) as usize;
    // the report counts the selection's paths as used from the start of the boot (`boot::selected`); the plan counts them one by one as it takes the modules
    let mut c = 0;
    while c < k {
        if caps[c].kind == dkrt_sdk::proto::CAPK_SELECTION as u32 {
            caps[c].used = 0;
            caps[c].largest = caps[c].limit;
        }
        // the plan starts with nothing loaded (the report counts the plan's own result from its end)
        if caps[c].kind == dkrt_sdk::proto::CAPK_DSP_COST as u32 {
            caps[c].used = 0;
            caps[c].largest = 0;
        }
        c += 1;
    }
    loader().plan_with_caps(count, &caps[..k])
}

/// The DSP load of the last plan: (load, largest per-voice cost). A read of the field that takes no reference: module code can reach it (`dkrt_capacity`) while
/// a load runs.
pub unsafe fn planned_dsp() -> (u32, u32) {
    *addr_of!(LOADER.planned_dsp)
}

impl<P: Platform, M: Images, R: Registry> Loader<P, M, R> {
    pub const fn new(p: P, m: M, r: R) -> Self {
        Loader {
            p,
            m,
            r,
            phase: Phase::Closed,
            roles: [0; records::REC_MAX],
            missing: [0; NEEDS_LEN],
            decl_diff: (0, 0),
            tables: [[0; TABLE_MAX]; SEL_MAX],
            table_len: [0; SEL_MAX],
            planned_dsp: (0, 0),
            found: Found::new(),
            loaded: [false; PROVIDER_MAX],
            provs: [ProvRec::EMPTY; PROVIDER_MAX],
            exports: Exports::new(),
            installer: Installer::new(),
        }
    }

    /// The name of the missing module of the last load that failed with `E_IMPORT`, NUL padded; empty when none.
    pub fn missing(&self) -> [u8; NEEDS_LEN] {
        self.missing
    }

    /// The boot's load loop: the selection in plan order. A module the plan refuses is not loaded; each result goes to `boot::report_module`.
    pub unsafe fn boot(&mut self, r: &dkrt_sdk::sel::Record, planned: &Plan) {
        self.phase = Phase::Boot;
        let mut pos = 0;
        while pos < r.count as usize {
            let k = planned.order[pos] as usize;
            crate::selection::BOOT_IDX = k as u32;
            self.missing = [0; NEEDS_LEN];
            let res = match planned.verdicts[k] {
                dkrt_abi::plan::Verdict::Accepted => {
                    crate::boot::states().init(k);
                    let res = self.load(r.paths[k].as_ptr());
                    crate::boot::states().finished(k, res);
                    res
                }
                dkrt_abi::plan::Verdict::Refused(why) => why.code(),
            };
            crate::boot::report_module(k, r.paths[k].as_ptr(), res, &self.missing);
            pos += 1;
        }
        self.phase = Phase::Closed;
    }

    /// Validate and load the `.dkm` at `path`. The tables are checked after every attempt, and none starts once a check has failed (`invariants::check`).
    pub unsafe fn load(&mut self, path: *const u8) -> i32 {
        self.load_file(path, None)
    }

    /// Load the service file at `path` into provider record `j`.
    unsafe fn load_provider(&mut self, path: *const u8, j: usize) -> i32 {
        self.load_file(path, Some(MAX_MODULES + j))
    }

    unsafe fn load_file(&mut self, path: *const u8, fixed: Option<usize>) -> i32 {
        if crate::invariants::bad() {
            return E_TABLE;
        }
        let r = self.load_inner(path, fixed);
        #[cfg(not(test))]
        crate::invariants::check();
        r
    }

    /// `fixed`: the provider record of a service file; `None`: a module takes the first free module record. A service file loads only into a provider
    /// record, and a module file never does.
    unsafe fn load_inner(&mut self, path: *const u8, fixed: Option<usize>) -> i32 {
        dsp::init_once();
        // the module set is fixed per session: only the boot hook loads (hot reload is archived, docs/plans/digikit-hot-reload.md)
        if self.phase != Phase::Boot {
            return E_BOOT_ONLY;
        }
        let staged = match self.m.stage(path) {
            Ok(s) => s,
            Err(e) => return e,
        };
        let f = staged.ptr;
        let size = staged.size;

        // ---- checks (nothing is written outside the staging buffer) ----
        let head = match image::head(f, size) {
            Ok(h) => h,
            Err(e) => return e,
        };
        let image::Head { needs, image_off, image_size, bss, reloc_off, reloc_n, import_off, import_n, init, fini, menu, tick } = head;
        // the steps of the file's role
        let role = rd32(f.add(dkneeds::H_ROLE as usize));
        if (role == 1) != fixed.is_some() {
            return E_FORMAT;
        }
        let steps = RoleSteps::<P, M, R>::of(role);
        let r = (steps.check)(self, f, &head);
        if r != OK {
            return r;
        }
        let name = f.add(H_NAME);
        let total = image_size + bss;
        // imports: every name must be in the runtime's table
        let mut import_addrs = [0u32; 64];
        let mut i = 0;
        while i < import_n as usize {
            let p = f.add(import_off as usize + i * dkneeds::CF_IMPORT_LEN as usize);
            if cstr_len(p, dkneeds::CF_IMPORT_LEN as usize) == dkneeds::CF_IMPORT_LEN as usize {
                return E_FORMAT;
            }
            let mut a = self.m.import_addr(p);
            if a == 0 && role == 1 {
                // a service file also links the service-only imports (`SVC_IMPORTS`)
                a = self.m.svc_import_addr(p);
            }
            if a == 0 {
                // the runtime's table has no such name: a loaded provider may export it
                let n = cstr_len(p, dkneeds::CF_IMPORT_LEN as usize);
                a = self.exports.resolve(dkneeds::keys::name_hash_bytes(core::slice::from_raw_parts(p, n))).unwrap_or(0);
            }
            if a == 0 {
                // the first import that belongs to a module that is not in the image: the boot keeps its name (`MISSING`)
                if let (Some(n), true) = (self.m.import_needs(p), self.missing[0] == 0) {
                    let b = n.as_bytes();
                    let mut k = 0;
                    while k < b.len() && k < NEEDS_LEN {
                        self.missing[k] = b[k];
                        k += 1;
                    }
                }
                return E_IMPORT;
            }
            import_addrs[i] = a;
            i += 1;
        }
        // relocations: kind, bounds, import index
        if let Err(e) = image::sections(f, &head) {
            return e;
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
        let rec = match fixed {
            Some(rec) => {
                if records::module(rec).used != 0 {
                    return E_LOADED;
                }
                rec
            }
            None => match capacity::pick_record(MAX_MODULES, |i| s.modules[i].used != 0, |i| cstr_eq(s.modules[i].name.as_ptr(), name)) {
                Ok(r) => r,
                Err(e) => return e,
            },
        };

        // ---- allocate, copy, relocate, zero bss ----
        // The DSP range, the relocated DSP image and the voice rows come first: nothing else is allocated yet if they fail.
        if dsp_size != 0 {
            let r = dsp::reserve(rec, &job);
            if r != OK {
                return r;
            }
        }
        let blk = match self.m.alloc(total) {
            Some(b) => b,
            None => {
                if dsp_size != 0 {
                    dsp::cancel(rec);
                }
                return E_NOMEM;
            }
        };
        let (mem, base) = (blk.ptr, blk.addr);
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
        self.p.flush(base, total);

        // ---- record, init, publish ----
        self.roles[rec] = (role == 1) as u8;
        let m: &mut Module = records::module(rec);
        m.used = 1;
        i = 0;
        while i < dkneeds::NAME_LEN as usize {
            m.name[i] = *name.add(i);
            i += 1;
        }
        let dst = records::path(rec);
        let plen = cstr_len(path, 63);
        i = 0;
        while i < 64 {
            dst[i] = if i < plen { *path.add(i) } else { 0 };
            i += 1;
        }
        m.version = rd32(f.add(H_VERSION));
        records::set_runtime(rec, needs & !crate::FLAG_TRACE);
        m.base = base;
        m.size = total;
        m.fini = if fini == NONE { 0 } else { base + fini };
        crate::entry::bind(rec);
        records::set_menu(rec, if menu == NONE { 0 } else { base + menu });
        let place = PlaceArgs { file: f, keys_off: reg_off, keys_len: reg_size, image_size };
        let initaddr = if init == NONE { 0 } else { base + init };
        let tickaddr = if tick == NONE { 0 } else { base + tick };
        if dsp_size != 0 {
            // the CF part goes live now (rows in place before the machine view is built); the DSP code and the voice rows follow
            // when the frames flow
            m.used = 1;
            let r = self.complete(rec, &keys, initaddr, tickaddr, image_size, steps, place);
            if r != OK {
                if dsp::retained() { return E_BUSY; }
                dsp::cancel(rec);
                if !crate::files::retire(rec) || !retire_sdk(rec) { return E_BUSY; }
                self.m.free(base);
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
        let r = self.complete(rec, &keys, initaddr, tickaddr, image_size, steps, place);
        if r != OK {
            if self.cannot_retire(rec) { return E_BUSY; }
            self.m.free(base);
            m.used = 0;
            crate::entry::unbind(rec);
        }
        r
    }


    /// The end of a load, shared by the immediate and the DSP path (both run in the boot hook; docs/plans/sdk.md 6.3): `init` runs first and its builder calls are collected; then the commit checks them, fills the
    /// voice rows' ranges, writes the registry rows and keeps the record block. Anything that fails undoes what `init` did and writes no row.
    #[allow(clippy::too_many_arguments)]
    unsafe fn complete(&mut self, rec: usize, keys: &crate::collect::KeysCopy, init: u32, tick: u32, image_size: u32, steps: RoleSteps<P, M, R>, place: PlaceArgs) -> i32 {
        let s = st();
        let module = rec < MAX_MODULES;
        // the role's part of the file that `init` could overwrite in the staging buffer
        let r = (steps.place)(self, rec, &place);
        if r != OK {
            return self.fail(rec, r);
        }
        if module {
            crate::files::init_begin(rec);
        }
        let blk = match self.m.alloc(core::mem::size_of::<crate::collect::Collect>() as u32) {
            Some(b) => b,
            None => return self.fail(rec, E_NOMEM),
        };
        let block = blk.ptr;
        crate::collect::begin(block as *mut crate::collect::Collect);
        if init != 0 {
            let ctx = crate::entry::ctx_addr(rec) as usize as *const Ctx;
            CALLED_NEED = [0; NEEDS_LEN];
            let r = self.m.enter_init(rec, init, ctx);
            if r != 0 {
                crate::collect::end();
                self.m.free(blk.addr);
                let missing = r == E_MISSING && CALLED_NEED[0] != 0;
                if missing && self.missing[0] == 0 {
                    self.missing = CALLED_NEED;
                }
                let e = self.fail(rec, if missing { E_IMPORT } else { E_INIT });
                return if e == E_BUSY { e } else if missing { E_IMPORT } else { E_INIT };
            }
        }
        crate::collect::end();
        let r = self.commit(rec, &*(block as *const crate::collect::Collect), keys, image_size);
        self.m.free(blk.addr);
        if r != OK {
            return self.fail(rec, r);
        }
        let r = (steps.activate)(self, rec);
        if r != OK {
            return self.fail(rec, r);
        }
        let m: &mut Module = records::module(rec);
        m.used = 1;
        // a provider has no tick, no files, no intro and is no module of the count
        if module {
            if tick != 0 {
                crate::entry::publish_tick(rec, tick);
            }
            s.loads = s.loads.wrapping_add(1);
            crate::files::ready(rec);
            crate::intro::commit(rec);
        }
        OK
    }



    /// Undo what `init` and the checks before the commit did for record `rec`; `r` is the result to return (`E_BUSY` when a step of the module is still running).
    unsafe fn fail(&mut self, rec: usize, r: i32) -> i32 {
        let module = rec < MAX_MODULES;
        let mut held = false;
        if module {
            crate::intro::discard(rec);
            held = !crate::files::retire(rec) | !retire_sdk(rec);
        }
        crate::module_cmds::release_cmds(rec as u8 + 1);
        crate::dlog::release_sinks(rec);
        (RoleSteps::<P, M, R>::of(self.roles[rec] as u32).undo)(self, rec);
        self.r.remove(rec);
        if module && !dsp::release(rec) || held {
            return E_BUSY;
        }
        r
    }

    /// A module's file or I/O step still runs, or the DSP loader keeps its rows: the record must not be freed yet. A provider record has none of these.
    unsafe fn cannot_retire(&mut self, rec: usize) -> bool {
        rec < MAX_MODULES && (dsp::retained() || !crate::files::retire(rec) || !retire_sdk(rec))
    }


    /// The commit of 6.3 steps 1 to 3: check, fill the voice rows, apply the records; the record block stays in the arena.
    unsafe fn commit(&mut self, rec: usize, c: &crate::collect::Collect, keys: &crate::collect::KeysCopy, image_size: u32) -> i32 {
        use crate::collect as col;
        let owner = rec as u8 + 1;
        let base = records::module(rec).base;
        // the declarations of a service module against its header table (declare-check-commit)
        if let Some(d) = col::decl_diff(c, keys, base) {
            self.decl_diff = d;
            return E_DECL_KIND;
        }
        // a service file declares its row in `init` (`dk_service_declare_row`); a module declares none. `fail` takes whatever was added down again
        let row = c.decl.row_ptr().map(|p| &*p);
        let r = self.r.add(rec, row);
        if r != OK {
            return r;
        }
        let declared = c.nm + c.nf + c.nparam + c.np + c.ng != 0;
        // a provider adds no machine, effect or page row of its own (the machines and fx services move in R12-5 and R12-6)
        if rec >= MAX_MODULES && declared {
            return E_FORMAT;
        }
        let mut block: *mut u8 = core::ptr::null_mut();
        let mut baddr = 0u32;
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
            match self.m.alloc(size as u32) {
                Some(b) => {
                    block = b.ptr;
                    baddr = b.addr;
                }
                None => return E_NOMEM,
            }
            if let Err(e) = col::lower(c, &p, keys, &voice, base, block, size, &mut plan) {
                self.m.free(baddr);
                return e;
            }
            let r = crate::registry::check(block, size as u32, image_size);
            if r != OK {
                self.m.free(baddr);
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
                        self.m.free(baddr);
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
                    self.m.free(baddr);
                    return E_FORMAT; // no such effect in the module's DSP section
                }
                f += 1;
            }
            crate::registry::set_args(Some(col::apply_args(c, &plan)));
            let r = crate::registry::apply(owner, block, size as u32, base);
            crate::registry::set_args(None);
            if r != OK {
                self.m.free(baddr);
                return r;
            }
            rows = col::counts_of(c, &p);
        }
        if rec < MAX_MODULES {
            col::keep(rec, baddr, size as u32, keys);
        }
        crate::dlog::ev(
            logk::MODULE_COMMIT,
            crate::dlog::INFO,
            [read_volatile(addr_of!(BOOT_IDX)), rows.0 | rows.1 << 8 | rows.2 << 16, baddr, size as u32, 0],
        );
        OK
    }


    /// The DSP part of a module published at boot failed (the machine rows and `init` are in already): take it down again.
    pub unsafe fn undo_early(&mut self, rec: usize) -> i32 {
        self.unload_rec(rec)
    }


    /// One UI-loop pass of the DSP loader. `dsp` only advances its state and reports what it found; the module teardown, the machine
    /// listing and the boot result belong here, so `dsp` depends on neither the loader nor the boot.
    pub unsafe fn dsp_step_with(&mut self, l: ServiceTable) {
        match dsp::step_with(l) {
            dsp::Advance::Pending => {}
            dsp::Advance::Failed(code) => {
                let (rec, idx) = {
                    let p = &dsp::ds().pending;
                    (p.rec as usize, p.boot_idx)
                };
                // Teardown invalidates once, after the tick is stopped, before any fini/free.
                let reclaimed = self.undo_early(rec) == OK;
                crate::boot::report_dsp(idx, code);
                dsp::fail_done(reclaimed, code);
            }
            dsp::Advance::Loaded => {
                let (rec, idx) = {
                    let p = &dsp::ds().pending;
                    (p.rec as usize, p.boot_idx)
                };
                l.on_loaded(rec);
                crate::boot::report_dsp(idx, OK);
                dsp::loaded_done();
            }
        }
    }


    /// Take module record `rec` down: its tick, `fini`, machine rows, DSP voice rows and range, memory. Used only
    /// by the DSP loader when the DSP part of a module published at boot fails (`undo_early`). The run-time `unload` that also called
    /// it (with the in-use scan) is archived with hot reload. The tables are checked after it (`invariants::check`).
    pub unsafe fn unload_rec(&mut self, rec: usize) -> i32 {
        let r = self.unload_rec_inner(rec);
        #[cfg(not(test))]
        crate::invariants::check();
        r
    }

    unsafe fn unload_rec_inner(&mut self, rec: usize) -> i32 {
        if !crate::files::retire(rec) { return E_BUSY; }
        if !crate::worker::retire(rec) { return E_BUSY; }
        if !retire_sdk(rec) { return E_BUSY; }
        // an intro of this module that has not run `done` yet: `done` must not point into freed code
        crate::intro::discard(rec);
        let m: &mut Module = records::module(rec);
        // 1. clear the tick (one aligned store) so no new call starts; the UI task, where it runs, is this task
        crate::entry::stop_tick(rec);
        // DSP rows must be invalidated before fini can mutate/release module state.
        if !dsp::quiesce(rec) { return E_BUSY; }
        // 2. fini, 3. free
        if m.fini != 0 {
            let ctx = crate::entry::ctx_addr(rec) as usize as *const Ctx;
            self.m.enter_fini(rec, m.fini, ctx);
        }
        // a sink it set is released with it, with or without a `fini`; nothing may point into its range
        crate::dlog::release_sinks(rec);
        crate::dlog::sink_drop(m.base, m.base.wrapping_add(m.size));
        // the SysEx commands it registered are released with it
        crate::module_cmds::release_cmds(rec as u8 + 1);
        drop_tick_cb(m.base, m.size);
        crate::api::poll_drop_range(m.base, m.size);
        // the machine row first, then the DSP voice rows, then the range in the DSP area
        (RoleSteps::<P, M, R>::of(self.roles[rec] as u32).undo)(self, rec);
        self.r.remove(rec);
        let committed = crate::collect::release(rec);
        if committed != 0 {
            self.m.free(committed);
        }
        dsp::release_range(rec);
        self.m.free(m.base);
        m.used = 0;
        records::set_menu(rec, 0);
        crate::entry::unbind(rec);
        OK
    }

    /// Keep the header table of the `.dkm` `file` (the whole file in the staging buffer) for selection index `k`.
    pub unsafe fn capture(&mut self, k: usize, file: &[u8]) {
        if k >= SEL_MAX || file.len() < dkneeds::HEADER as usize {
            return;
        }
        let be = |at: usize| u32::from_be_bytes([file[at], file[at + 1], file[at + 2], file[at + 3]]) as usize;
        let (off, len) = (be(dkneeds::keys::H_KEYS_OFF as usize), be(dkneeds::keys::H_KEYS_SIZE as usize));
        if len == 0 || len > TABLE_MAX || off > file.len() || len > file.len() - off {
            return;
        }
        self.tables[k][..len].copy_from_slice(&file[off..off + len]);
        self.table_len[k] = len;
    }

    /// Forget the tables (the start of a boot's planning).
    pub fn reset_plan(&mut self) {
        self.table_len = [0; SEL_MAX];
        self.planned_dsp = (0, 0);
    }

    /// Plan the selection `count` modules long from the tables `capture` kept. The hard needs that the runtime does not meet are looked up as `services/<name>.dks`
    /// (`discover::find`, one listing, only when there is such a need); the providers found are planned with the modules and load before them.
    #[inline(never)]
    pub unsafe fn plan_with_caps(&mut self, count: usize, caps: &[DkrtCap]) -> Plan {
        let n = count.min(SEL_MAX);
        let mut mods: [Option<Keys>; SEL_MAX] = [None; SEL_MAX];
        let mut i = 0;
        while i < n {
            if self.table_len[i] != 0 {
                mods[i] = Keys::parse(&self.tables[i][..self.table_len[i]]).ok();
            }
            i += 1;
        }
        // the hard needs that no service of the runtime meets
        self.found.n = 0;
        let mut want = [0u32; WANT_MAX];
        let nw = unmet_needs(&mods[..n], &mut want);
        if nw != 0 {
            discover::find(&mut self.m, &services(), &want[..nw], &mut self.found);
        }
        let provs = parse_provs(&self.found, self.found.n);
        let env = Env { services: services(), runtime: dkrt_sdk::RUNTIME_VERSION, caps };
        let p = plan::plan_with(&env, &mods[..n], &provs[..self.found.n]);
        self.planned_dsp = p.dsp_load;
        p
    }

    /// The provider stage of the boot: each provider of the plan in plan order, before any module. A provider the plan refused is not loaded; a provider that
    /// fails (its own provider did not load: E_IMPORT) undoes itself. The result goes to the log (`boot::report_provider`); the module states do not
    /// change (`LOADS` and `DkrtBootInfo` know no provider).
    #[inline(never)]
    pub unsafe fn load_providers(&mut self, planned: &Plan) {
        self.phase = Phase::Boot;
        self.loaded = [false; PROVIDER_MAX];
        crate::selection::BOOT_IDX = PROVIDER_IDX;
        let mut hashes = [0u32; PROVIDER_MAX];
        {
            let provs = parse_provs(&self.found, planned.np);
            let mut j = 0;
            while j < planned.np {
                if let Some(t) = &provs[j] {
                    hashes[j] = t.words(dkneeds::keys::tag::PROVIDES, 0).map_or(0, |w| w.0);
                }
                j += 1;
            }
        }
        let mut pos = 0;
        while pos < planned.np {
            let j = planned.provider_order[pos] as usize;
            pos += 1;
            self.missing = [0; NEEDS_LEN];
            self.decl_diff = (0, 0);
            let res = match planned.provider_verdicts[j] {
                dkrt_abi::plan::Verdict::Refused(why) => why.code(),
                dkrt_abi::plan::Verdict::Accepted => {
                    // a provider whose own provider did not load fails at its imports (E_IMPORT)
                    let mut path = [0u8; discover::PATH_CAP + 1];
                    let at = self.found.path(j);
                    path[..at.len()].copy_from_slice(at);
                    self.load_provider(path.as_ptr(), j)
                }
            };
            self.loaded[j] = res == OK;
            let base = if res == OK { records::module(MAX_MODULES + j).base } else { 0 };
            // a refusal of the declare-check names the item that differs, in the words of the base (the tag) and the service hash (the item's first word)
            let (hash, base) = if res == E_DECL_KIND && self.decl_diff.0 != 0 { (self.decl_diff.1, self.decl_diff.0) } else { (hashes[j], base) };
            crate::boot::report_provider(self.found.path(j), res, hash, base);
        }
        crate::selection::BOOT_IDX = u32::MAX;
        self.phase = Phase::Closed;
    }

    /// The settle step: the modules that need a provider that did not load are refused before the module stage starts (`plan::settle`).
    #[inline(never)]
    pub unsafe fn settle(&mut self, planned: &mut Plan, count: usize) {
        let n = count.min(SEL_MAX);
        let mut mods: [Option<Keys>; SEL_MAX] = [None; SEL_MAX];
        let mut i = 0;
        while i < n {
            if self.table_len[i] != 0 {
                mods[i] = Keys::parse(&self.tables[i][..self.table_len[i]]).ok();
            }
            i += 1;
        }
        let provs = parse_provs(&self.found, planned.np);
        plan::settle(&services(), planned, &mods[..n], &provs[..planned.np], &self.loaded[..planned.np]);
    }
}

/// The header tables of the first `n` providers the search found.
#[inline(never)]
fn parse_provs(found: &Found, n: usize) -> [Option<Keys<'_>>; PROVIDER_MAX] {
    let mut provs: [Option<Keys>; PROVIDER_MAX] = [None; PROVIDER_MAX];
    let mut j = 0;
    while j < n.min(PROVIDER_MAX) {
        provs[j] = Keys::parse(found.table(j)).ok();
        j += 1;
    }
    provs
}

/// Most hard needs the search is asked for: one boot's distinct needs of services that the runtime does not provide.
const WANT_MAX: usize = 24;

/// The distinct hard needs of `mods` that no service of the runtime meets, into `out`; their count.
#[inline(never)]
fn unmet_needs(mods: &[Option<Keys>], out: &mut [u32; WANT_MAX]) -> usize {
    let mut n = 0;
    let mut m = 0;
    while m < mods.len() {
        if let Some(t) = &mods[m] {
            let mut i = 0;
            while let Some(w) = t.words(dkneeds::keys::tag::NEEDS, i) {
                i += 1;
                if w.2 & 1 == 0 || plan::is_runtime_service(w.0, &services()) {
                    continue;
                }
                let mut seen = false;
                let mut k = 0;
                while k < n {
                    seen |= out[k] == w.0;
                    k += 1;
                }
                if !seen && n < WANT_MAX {
                    out[n] = w.0;
                    n += 1;
                }
            }
        }
        m += 1;
    }
    n
}

/// The provider of the last `_open` stub that answered DK_E_MISSING since the clear before `init` (`stub_missing`), NUL padded; empty when none.
/// When `init` returns DK_E_MISSING, `Loader::missing` takes it and the load fails with E_IMPORT, as for an unresolved import.
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

/// The module `rec` is going away (an unload or a rollback): its I/O jobs end without `done`, and a file it was writing is dropped. False: an
/// I/O step of it is running (it is cancelled); the caller keeps the module's memory (`E_BUSY`) until a later call returns true.
pub(crate) unsafe fn retire_sdk(rec: usize) -> bool {
    if !crate::io::retire(rec) {
        return false;
    }
    crate::store_writer::retire(rec as u8 + 1);
    true
}

