//! The builder calls of a module (docs/plans/sdk.md 6.2) and their commit (6.3).
//!
//! During `init` every builder call appends to a `Collect`, one block in the module's arena share (`begin` .. `end`). A handle is the address of
//! an element of that block. A call never fails one by one: on overflow, on a bad argument, outside `init` or with a handle that is not ours it
//! returns the placeholder handle (`PLACEHOLDER`), the first error is kept, and the setters ignore a placeholder. The error is reported at commit.
//!
//! Commit (`loader::complete3`) lowers the collection to the records the packer wrote before builders (`lower`: the `register` section format of
//! `registry.rs`, kinds PARAM, MACHINE, PAGE, EFFECT, ACTION and native page v1/v2), checks it (`check_commit` and `registry::check`) and gives it to
//! `registry::apply`, so the registry has one code path for both sources. The record block stays in the arena while the module is loaded
//! (`BLOCKS`); the `MODULE_COMMIT` event carries its address and size.
//!
//! Device facts (knob positions of a machine's words, stock rows by glyph, clone_of, level) come from `Profile`, read from the `dynreg_info` table
//! (`registry::profile`); before builders the packer took them from the device file.
use crate::{cstr_len, MAX_MODULES};
use dkrt_sdk::records::{KIND_ACTION, KIND_EFFECT, KIND_MACHINE, KIND_NATIVE_PAGE, KIND_NATIVE_PAGE_V2, KIND_PAGE, KIND_PARAM};
use core::ptr::{addr_of, addr_of_mut, read_volatile, write_volatile};
use dkrt_abi::service::{DeclItem, Declare};
use dkrt_sdk::*;

/// The length of a record of `kind`, as the service that owns the kind declares it.
fn record_len(kind: u32) -> usize {
    crate::handlers::len(kind).unwrap_or(0) as usize
}

pub const MAX_RECORDS: usize = dkneeds::MAX_RECORDS as usize;
const MAX_M: usize = limits::MACHINE_ROWS;
const MAX_F: usize = crate::dsp::MAX_FX;
const MAX_PG: usize = 16;
const MAX_PA: usize = MAX_RECORDS;
const TEXT: usize = 24;
/// The characters of an effect's short name (the tab label of its page record).
const FX_SHORT: usize = 3;
const KNOBS: usize = limits::PAGE_KNOBS;
const KNOB_PARAM: u32 = 0x8000_0000;
const FX_KNOBS: usize = 7;

// record kinds of the `register` section: `dkrt_sdk::records` (sdk/sdk.toml `[records.kinds]`); the lengths come from the services' declarations (`handlers::len`)
const REG_MAGIC: u32 = 0x444b_5247;
const HEADER: usize = 16;
const POS_END: u32 = 255;

pub const ROUTE_MASTER: u32 = dkneeds::page::DK_ROUTE_MASTER;
pub const ROUTE_SEND: u32 = dkneeds::page::DK_ROUTE_SEND;
use dkneeds::{FXF_DEFAULT_SEND, FXF_NOMASTER, FXF_SEND};
/// `DK_BIPOLAR` of the display kind of `dk_page_add_fx_param` (`DK_PERCENT` is 1).
const DK_BIPOLAR: u32 = 2;

/// The device facts of the lowering (a copy of the words of `dynreg_info` from 260; see core/dynreg).
#[derive(Clone, Copy)]
pub struct Profile {
    pub device: u32,
    /// The device keeps a machine's knobs in page rows (Digitone II).
    pub pages: bool,
    pub slot_base: u32,
    pub clone_of: u32,
    pub level: u32,
    pub own_dsp: bool,
    pub tune_code: u32,
    pub tune_slot: u32,
    pub level_code: u32,
    pub level_knob: u32,
    pub nslots: u32,
    pub slots: [u32; 8],
    /// Stock rows by glyph: encoder, fader, pan, onoff, tune, play.
    pub tpl: [u32; 6],
}

#[derive(Clone, Copy)]
pub struct CMachine {
    /// the machine's index in the module (`DK_MACHINE_ID_<ID>`): the position of its key in the keys table
    pub index: u32,
    pub name: [u8; TEXT],
    pub short: [u8; TEXT],
}
#[derive(Clone, Copy)]
pub struct CFx {
    /// the effect's index in the module (`DK_FX_ID_<ID>`): the position of its id in the keys table, the ordinal of its effect record
    pub index: u32,
    pub name: [u8; TEXT],
    pub short: [u8; TEXT],
    pub routes_set: bool,
    pub allowed: u32,
    pub def: u32,
    /// the MIX knob default 0 to 127 (`dk_fx_mix`)
    pub mix: u32,
    pub page: i32,
}
#[derive(Clone, Copy)]
pub struct CPage {
    /// 0 a machine's page, 1 an effect's page
    pub fx: bool,
    pub owner: u8,
    pub name: [u8; TEXT],
    /// the tab label of the page record (`short_name` of `dk_machine_add_page`)
    pub short: [u8; TEXT],
    pub canvas: bool,
    pub flags: u32,
    pub draw: u32,
    pub event: u32,
    pub arg: u32,
    pub nbind: u8,
    /// (kind 1 param / 2 local, collection param index or local id, encoder slot)
    pub binds: [[u32; 3]; 8],
    pub nparam: u8,
}
#[derive(Clone, Copy)]
pub struct CParam {
    pub page: u8,
    pub fx: bool,
    pub name: [u8; 32],
    pub short: [u8; 16],
    /// machine params: raw = unit * 256
    pub min: i32,
    pub max: i32,
    pub def: i32,
    pub glyph: u32,
    pub latched: bool,
    pub click: u32,
    pub click_arg: u32,
    /// effect params: display kind and the float bits
    pub kind: u32,
    pub fmin: u32,
    pub fmax: u32,
    pub fdef: u32,
}

/// The most generic records (`dk_record_add`) and the bytes of their bodies one `init` may emit.
pub const MAX_G: usize = 8;
pub const G_BYTES: usize = 640;

/// A generic record: its kind and where its body is in `Collect::gbytes`.
#[derive(Clone, Copy)]
pub struct GRec {
    pub kind: u32,
    pub at: u16,
    pub len: u16,
}

const M0: CMachine = CMachine { index: 0, name: [0; TEXT], short: [0; TEXT] };
const F0: CFx = CFx { index: 0, name: [0; TEXT], short: [0; TEXT], routes_set: false, allowed: 0, def: 0, mix: 127, page: -1 };
const G0: CPage = CPage { fx: false, owner: 0, name: [0; TEXT], short: [0; TEXT], canvas: false, flags: 0, draw: 0, event: 0, arg: 0, nbind: 0, binds: [[0; 3]; 8], nparam: 0 };
const P0: CParam = CParam {
    page: 0, fx: false, name: [0; 32], short: [0; 16], min: 0, max: 127 * 256, def: 0, glyph: 0, latched: false, click: 0, click_arg: 0, kind: 0, fmin: 0, fmax: 0, fdef: 0,
};

/// What `init` declared.
pub struct Collect {
    pub err: i32,
    pub machines: [CMachine; MAX_M],
    pub nm: usize,
    pub fxs: [CFx; MAX_F],
    pub nf: usize,
    pub pages: [CPage; MAX_PG],
    pub np: usize,
    pub params: [CParam; MAX_PA],
    pub nparam: usize,
    /// the generic records (`dk_record_add`): kind and body, appended to the `register` section as they came
    pub grecs: [GRec; MAX_G],
    pub ng: usize,
    /// what a service module declared in `init` (`dk_service_declare_*`), compared with its header table at the commit
    pub decl: Declare,
    gbytes: [u8; G_BYTES],
    gat: usize,
    records: usize,
}

// The host tests' tables (`COLLECT_BYTES`, sdk/sdk.toml `[limits.format]`) are for the ColdFire layout; the host's 64-bit pointers pad it more.
#[cfg(target_pointer_width = "32")]
const _: () = assert!(core::mem::size_of::<Collect>() == dkneeds::COLLECT_BYTES as usize);

impl Collect {
    pub const fn new() -> Collect {
        Collect { err: 0, machines: [M0; MAX_M], nm: 0, fxs: [F0; MAX_F], nf: 0, pages: [G0; MAX_PG], np: 0, params: [P0; MAX_PA], nparam: 0, grecs: [GRec { kind: 0, at: 0, len: 0 }; MAX_G], ng: 0, decl: Declare::new(), gbytes: [0; G_BYTES], gat: 0, records: 0 }
    }

    /// `dk_record_add`: keep one record of a generic kind (`[records.kinds]` `generic = true`; the typed kinds have their builders). The body is
    /// copied; its length is the kind's record length less the 8-byte header. The owning service checks the record at the commit. Returns OK
    /// or the error that the commit reports too.
    pub fn add_record(&mut self, kind: u32, bytes: *const u8, len: u32) -> i32 {
        let want = match crate::handlers::len(kind) {
            Some(l) if is_generic(kind) => l,
            _ => {
                self.fail(E_FORMAT);
                return E_FORMAT;
            }
        };
        if bytes.is_null() || len + 8 != want {
            self.fail(E_FORMAT);
            return E_FORMAT;
        }
        if self.ng >= MAX_G || self.gat + len as usize > G_BYTES || !self.room() {
            self.fail(E_DECL_FULL);
            return E_DECL_FULL;
        }
        let mut i = 0;
        while i < len as usize {
            self.gbytes[self.gat + i] = unsafe { *bytes.add(i) };
            i += 1;
        }
        self.grecs[self.ng] = GRec { kind, at: self.gat as u16, len: len as u16 };
        self.ng += 1;
        self.gat += len as usize;
        OK
    }

    fn fail(&mut self, e: i32) {
        if self.err == 0 {
            self.err = e;
        }
    }

    /// One more record of the bounded collection; false (error kept) when full.
    fn room(&mut self) -> bool {
        if self.records >= MAX_RECORDS {
            self.fail(E_DECL_FULL);
            return false;
        }
        self.records += 1;
        true
    }

    pub fn add_machine(&mut self, index: u32, name: *const u8, short: *const u8) -> *mut u8 {
        if self.nm >= MAX_M || !self.room() {
            self.fail(E_DECL_FULL);
            return placeholder();
        }
        let i = self.nm;
        let mut m = M0;
        m.index = index;
        let mut e = 0;
        unsafe {
            put_text(&mut m.name, name, &mut e);
            put_text(&mut m.short, short, &mut e);
        }
        self.fail(e);
        self.machines[i] = m;
        self.nm += 1;
        &mut self.machines[i] as *mut CMachine as *mut u8
    }

    pub fn add_fx(&mut self, index: u32, name: *const u8, short: *const u8) -> *mut u8 {
        if self.nf >= MAX_F || !self.room() {
            self.fail(E_DECL_FULL);
            return placeholder();
        }
        let i = self.nf;
        let mut f = F0;
        f.index = index;
        let mut e = 0;
        unsafe {
            put_text(&mut f.name, name, &mut e);
            put_text(&mut f.short, short, &mut e);
        }
        if len_of(&f.short) > FX_SHORT {
            e = E_FORMAT; // the effect record's tab label holds 3 characters: refused, not cut
        }
        self.fail(e);
        self.fxs[i] = f;
        self.nf += 1;
        &mut self.fxs[i] as *mut CFx as *mut u8
    }

    fn add_page(&mut self, fx: bool, owner: usize, name: *const u8, short: *const u8) -> *mut u8 {
        if self.np >= MAX_PG || !self.room() {
            self.fail(E_DECL_FULL);
            return placeholder();
        }
        let i = self.np;
        let mut g = G0;
        g.fx = fx;
        g.owner = owner as u8;
        let mut e = 0;
        unsafe {
            put_text(&mut g.name, name, &mut e);
            put_text(&mut g.short, short, &mut e);
        }
        if fx && len_of(&g.short) > FX_SHORT {
            e = E_FORMAT; // the effect record's tab label holds 3 characters: refused, not cut
        }
        self.fail(e);
        if fx {
            if self.fxs[owner].page >= 0 {
                self.fail(E_FORMAT); // an effect has one page
            } else {
                self.fxs[owner].page = i as i32;
            }
        }
        self.pages[i] = g;
        self.np += 1;
        &mut self.pages[i] as *mut CPage as *mut u8
    }

    pub fn machine_add_page(&mut self, m: *mut u8, name: *const u8, short: *const u8) -> *mut u8 {
        match find(&self.machines, m) {
            Some(i) => self.add_page(false, i, name, short),
            None => placeholder(),
        }
    }

    pub fn fx_add_page(&mut self, f: *mut u8, name: *const u8, short: *const u8) -> *mut u8 {
        match find(&self.fxs, f) {
            Some(i) => self.add_page(true, i, name, short),
            None => placeholder(),
        }
    }

    fn add_param(&mut self, p: *mut u8, fx: bool, name: *const u8, short: *const u8) -> Option<*mut u8> {
        let pg = find(&self.pages, p)?;
        if self.pages[pg].fx != fx {
            self.fail(E_FORMAT); // a machine param on an effect page or the other way round
            return None;
        }
        if self.nparam >= MAX_PA || !self.room() {
            self.fail(E_DECL_FULL);
            return None;
        }
        let i = self.nparam;
        let mut q = P0;
        q.page = pg as u8;
        q.fx = fx;
        let mut e = 0;
        unsafe {
            put_text(&mut q.name, name, &mut e);
            put_text(&mut q.short, short, &mut e);
        }
        self.fail(e);
        if self.pages[pg].nparam as usize >= KNOBS {
            self.fail(E_FORMAT);
        } else {
            self.pages[pg].nparam += 1;
        }
        self.params[i] = q;
        self.nparam += 1;
        Some(&mut self.params[i] as *mut CParam as *mut u8)
    }

    /// `dk_page_add_param`: `def` is in knob units.
    pub fn page_add_param(&mut self, p: *mut u8, name: *const u8, short: *const u8, def: u32) -> *mut u8 {
        match self.add_param(p, false, name, short) {
            Some(h) => {
                let i = find(&self.params, h).unwrap_or(0);
                self.params[i].def = (def as i32).wrapping_mul(256);
                h
            }
            None => placeholder(),
        }
    }

    pub fn page_add_fx_param(&mut self, p: *mut u8, name: *const u8, short: *const u8, kind: u32, min: f32, max: f32, def: f32) -> *mut u8 {
        match self.add_param(p, true, name, short) {
            Some(h) => {
                let i = find(&self.params, h).unwrap_or(0);
                let q = &mut self.params[i];
                q.kind = kind;
                q.fmin = min.to_bits();
                q.fmax = max.to_bits();
                q.fdef = def.to_bits();
                h
            }
            None => placeholder(),
        }
    }

    pub fn param_range(&mut self, h: *mut u8, min: u32, max: u32) {
        if let Some(i) = find(&self.params, h) {
            self.params[i].min = (min as i32).wrapping_mul(256);
            self.params[i].max = (max as i32).wrapping_mul(256);
        }
    }

    pub fn param_glyph(&mut self, h: *mut u8, glyph: u32) {
        if let Some(i) = find(&self.params, h) {
            self.params[i].glyph = glyph;
        }
    }

    pub fn param_latched(&mut self, h: *mut u8) {
        if let Some(i) = find(&self.params, h) {
            self.params[i].latched = true;
        }
    }

    pub fn param_on_click(&mut self, h: *mut u8, f: u32, arg: u32) {
        if let Some(i) = find(&self.params, h) {
            if self.params[i].click == 0 && f != 0 && !self.room() {
                return;
            }
            self.params[i].click = f;
            self.params[i].click_arg = arg;
        }
    }

    pub fn page_canvas(&mut self, p: *mut u8, flags: u32, draw: u32, event: u32, arg: u32) {
        if let Some(i) = find(&self.pages, p) {
            let g = &mut self.pages[i];
            g.canvas = true;
            g.flags = flags;
            g.draw = draw;
            g.event = event;
            g.arg = arg;
        }
    }

    fn bind(&mut self, p: *mut u8, kind: u32, value: u32, slot: u32) {
        if let Some(i) = find(&self.pages, p) {
            let g = &mut self.pages[i];
            if g.nbind as usize >= 8 || slot >= 8 {
                self.fail(E_FORMAT);
                return;
            }
            let n = g.nbind as usize;
            g.binds[n] = [kind, value, slot];
            g.nbind += 1;
        }
    }

    pub fn page_bind(&mut self, p: *mut u8, prm: *mut u8, slot: u32) {
        match find(&self.params, prm) {
            Some(i) => self.bind(p, 1, i as u32, slot),
            None => {
                if find(&self.pages, p).is_some() {
                    self.fail(E_FORMAT); // a param that is not ours (a placeholder after an earlier error: the error is kept already)
                }
            }
        }
    }

    pub fn page_bind_local(&mut self, p: *mut u8, local: u32, slot: u32) {
        self.bind(p, 2, local, slot);
    }

    pub fn fx_routes(&mut self, f: *mut u8, allowed: u32, def: u32) {
        if let Some(i) = find(&self.fxs, f) {
            self.fxs[i].routes_set = true;
            self.fxs[i].allowed = allowed;
            self.fxs[i].def = def;
        }
    }

    pub fn fx_mix(&mut self, f: *mut u8, def: u32) {
        if let Some(i) = find(&self.fxs, f) {
            self.fxs[i].mix = def.min(127);
        }
    }
}

/// The index of the element `h` points to in `a`, None for the placeholder or any other pointer.
fn find<T>(a: &[T], h: *mut u8) -> Option<usize> {
    let base = a.as_ptr() as usize;
    let at = h as usize;
    let size = core::mem::size_of::<T>();
    if at < base || at >= base + a.len() * size || (at - base) % size != 0 {
        return None;
    }
    Some((at - base) / size)
}

unsafe fn put_text(dst: &mut [u8], src: *const u8, err: &mut i32) {
    if src.is_null() {
        *err = E_FORMAT;
        return;
    }
    let n = cstr_len(src, dst.len());
    if n == 0 || n == dst.len() {
        *err = E_FORMAT; // empty, or no room for the NUL
        return;
    }
    let mut i = 0;
    while i < n {
        dst[i] = *src.add(i);
        i += 1;
    }
}

// ---- the active collection and the placeholder ----------------------------------------------------------------------------------

static PLACEHOLDER: u32 = 0;
static mut ACTIVE: *mut Collect = core::ptr::null_mut();

pub fn placeholder() -> *mut u8 {
    addr_of!(PLACEHOLDER) as usize as *mut u8
}

/// Start collecting into the block `c` (initialised here); the builder calls reach it until `end`.
pub unsafe fn begin(c: *mut Collect) {
    core::ptr::write(c, Collect::new());
    write_volatile(addr_of_mut!(ACTIVE), c);
}

/// Stop collecting: later builder calls return the placeholder.
pub unsafe fn end() {
    write_volatile(addr_of_mut!(ACTIVE), core::ptr::null_mut());
}

unsafe fn active() -> Option<&'static mut Collect> {
    let c = read_volatile(addr_of!(ACTIVE));
    if c.is_null() {
        None
    } else {
        Some(&mut *c)
    }
}

// the builder imports (registered in imports.rs)

pub unsafe extern "C" fn dk_module_ctx_add_machine(ctx: *mut u8, index: u32, name: *const u8, short_name: *const u8) -> *mut u8 {
    match active() {
        Some(c) if crate::entry::is_ctx(ctx) => c.add_machine(index, name, short_name),
        _ => placeholder(),
    }
}
pub unsafe extern "C" fn dk_module_ctx_add_fx(ctx: *mut u8, index: u32, name: *const u8, short_name: *const u8) -> *mut u8 {
    match active() {
        Some(c) if crate::entry::is_ctx(ctx) => c.add_fx(index, name, short_name),
        _ => placeholder(),
    }
}
pub unsafe extern "C" fn dk_machine_add_page(m: *mut u8, name: *const u8, short_name: *const u8) -> *mut u8 {
    match active() {
        Some(c) => c.machine_add_page(m, name, short_name),
        None => placeholder(),
    }
}
pub unsafe extern "C" fn dk_fx_add_page(f: *mut u8, name: *const u8, short_name: *const u8) -> *mut u8 {
    match active() {
        Some(c) => c.fx_add_page(f, name, short_name),
        None => placeholder(),
    }
}
pub unsafe extern "C" fn dk_page_add_param(p: *mut u8, name: *const u8, short_name: *const u8, def: u32) -> *mut u8 {
    match active() {
        Some(c) => c.page_add_param(p, name, short_name, def),
        None => placeholder(),
    }
}
pub unsafe extern "C" fn dk_page_add_fx_param(p: *mut u8, name: *const u8, short_name: *const u8, kind: u32, min: f32, max: f32, def: f32) -> *mut u8 {
    match active() {
        Some(c) => c.page_add_fx_param(p, name, short_name, kind, min, max, def),
        None => placeholder(),
    }
}
pub unsafe extern "C" fn dk_param_range(prm: *mut u8, min: u32, max: u32) {
    if let Some(c) = active() {
        c.param_range(prm, min, max);
    }
}
pub unsafe extern "C" fn dk_param_glyph(prm: *mut u8, glyph: u32) {
    if let Some(c) = active() {
        c.param_glyph(prm, glyph);
    }
}
pub unsafe extern "C" fn dk_param_latched(prm: *mut u8) {
    if let Some(c) = active() {
        c.param_latched(prm);
    }
}
pub unsafe extern "C" fn dk_param_on_click(prm: *mut u8, f: Option<api::ParamClickFn>, arg: *mut u8) {
    if let Some(c) = active() {
        c.param_on_click(prm, f.map_or(0, |f| f as usize as u32), arg as usize as u32);
    }
}
pub unsafe extern "C" fn dk_page_canvas(p: *mut u8, flags: u32, draw: Option<api::PageDrawFn>, event: Option<api::PageEventFn>, arg: *mut u8) {
    if let Some(c) = active() {
        c.page_canvas(p, flags, draw.map_or(0, |f| f as usize as u32), event.map_or(0, |f| f as usize as u32), arg as usize as u32);
    }
}
pub unsafe extern "C" fn dk_page_bind(p: *mut u8, prm: *mut u8, slot: u32) {
    if let Some(c) = active() {
        c.page_bind(p, prm, slot);
    }
}
pub unsafe extern "C" fn dk_page_bind_local(p: *mut u8, local: u32, slot: u32) {
    if let Some(c) = active() {
        c.page_bind_local(p, local, slot);
    }
}
pub unsafe extern "C" fn dk_fx_routes(f: *mut u8, allowed: u32, def: u32) {
    if let Some(c) = active() {
        c.fx_routes(f, allowed, def);
    }
}
pub unsafe extern "C" fn dk_record_add(ctx: *mut u8, kind: u32, bytes: *const u8, len: u32) -> i32 {
    match active() {
        Some(c) if crate::entry::is_ctx(ctx) => c.add_record(kind, bytes, len),
        _ => E_CONTEXT,
    }
}
/// One `dk_service_declare_*` call: the item goes to the declarations (they take one record of the bounded collection each).
unsafe fn declare(ctx: *mut u8, tag: u16, a: u32, b: u32, c: u32) -> i32 {
    match active() {
        Some(col) if crate::entry::is_ctx(ctx) => {
            if col.decl.n >= Declare::MAX || !col.room() {
                col.fail(E_DECL_FULL);
                return E_DECL_FULL;
            }
            col.decl.add(tag, a, b, c);
            OK
        }
        _ => E_CONTEXT,
    }
}
/// `dk_service_declare_row` (a service file's `init`, `SVC_IMPORTS` only): the `ServiceRow` the file provides. One row per file; the commit hands it to the
/// registry (`Registry::add`).
#[cfg_attr(test, no_mangle)]
pub unsafe extern "C" fn dk_service_declare_row(ctx: *mut u8, row: *const dkrt_abi::service::ServiceRow) -> i32 {
    match active() {
        Some(col) if crate::entry::is_ctx(ctx) => {
            if row.is_null() || col.decl.row_ptr().is_some() {
                col.fail(E_DECL_KIND);
                return E_DECL_KIND;
            }
            if col.decl.n >= Declare::MAX || !col.room() {
                col.fail(E_DECL_FULL);
                return E_DECL_FULL;
            }
            col.decl.row(row);
            OK
        }
        _ => E_CONTEXT,
    }
}
#[cfg_attr(test, no_mangle)]
pub unsafe extern "C" fn dk_service_declare_kind(ctx: *mut u8, kind: u32, len: u32, handler: *const u8) -> i32 {
    // a handler is an image address: the header table carries its offset in the image
    declare(ctx, Declare::KIND, kind, len, handler as usize as u32)
}
#[cfg_attr(test, no_mangle)]
pub unsafe extern "C" fn dk_service_declare_slot(ctx: *mut u8, slot: u32, flags: u32, entry: *const u8) -> i32 {
    // the entry is an image address: the header table carries its offset in the image
    declare(ctx, Declare::SLOT, slot, flags, entry as usize as u32)
}
#[cfg_attr(test, no_mangle)]
pub unsafe extern "C" fn dk_service_declare_hook(ctx: *mut u8, site: u32, entry: *const u8) -> i32 {
    declare(ctx, Declare::HOOK, site, entry as usize as u32, 0)
}
#[cfg_attr(test, no_mangle)]
pub unsafe extern "C" fn dk_service_declare_export(ctx: *mut u8, name_hash: u32, entry: *const u8) -> i32 {
    declare(ctx, Declare::EXPORT, name_hash, entry as usize as u32, 0)
}
#[cfg_attr(test, no_mangle)]
pub unsafe extern "C" fn dk_service_declare_resource(ctx: *mut u8, resource: u32, limit: u32) -> i32 {
    declare(ctx, Declare::RESOURCE, resource, limit, 0)
}
pub unsafe extern "C" fn dk_fx_mix(f: *mut u8, def: u32) {
    if let Some(c) = active() {
        c.fx_mix(f, def);
    }
}

// ---- the keys table of the header --------------------------------------------------------------------------------------------

/// A copy of the keys table (the staged file is overwritten by anything `init` reads from the store).
#[derive(Clone, Copy)]
pub struct KeysCopy {
    pub machine_rows: u32,
    pub param_rows: u32,
    pub page_rows: u32,
    pub nm: usize,
    pub nf: usize,
    pub mk: [u32; MAX_RECORDS],
    pub fk: [u32; MAX_RECORDS],
    /// the KINDS, SLOTS, HOOKS, EXPORTS and RESOURCES entries of the table, as `init`'s declarations must repeat them (`decl_matches`)
    pub decl: Declare,
}

impl KeysCopy {
    pub const fn empty() -> KeysCopy {
        KeysCopy { machine_rows: 0, param_rows: 0, page_rows: 0, nm: 0, nf: 0, mk: [0; MAX_RECORDS], fk: [0; MAX_RECORDS], decl: Declare::new() }
    }

    /// Parse the header table bytes. A file with no table, or another magic or version, is the format of runtime 0.7: `E_CONTRACT`; a damaged
    /// table is `E_FORMAT`.
    pub fn from_bytes(b: &[u8]) -> Result<KeysCopy, i32> {
        let mut k = KeysCopy::empty();
        let keys = dkneeds::keys::Keys::parse(b).map_err(|e| if e == dkneeds::keys::KeysError::Format { E_CONTRACT } else { E_FORMAT })?;
        k.machine_rows = keys.machine_rows();
        k.param_rows = keys.param_rows();
        k.page_rows = keys.page_rows();
        let mut g = 0;
        while let Some((kind, list)) = keys.group(g) {
            g += 1;
            let mut j = 0;
            while j < list.len() {
                let v = list.get(j);
                j += 1;
                if kind == KIND_MACHINE {
                    if k.nm >= MAX_RECORDS {
                        return Err(E_FORMAT);
                    }
                    k.mk[k.nm] = v;
                    k.nm += 1;
                } else if kind == KIND_EFFECT {
                    if k.nf >= MAX_RECORDS {
                        return Err(E_FORMAT);
                    }
                    k.fk[k.nf] = v;
                    k.nf += 1;
                }
            }
        }
        for tg in [dkneeds::keys::tag::KINDS, dkneeds::keys::tag::SLOTS, dkneeds::keys::tag::HOOKS, dkneeds::keys::tag::EXPORTS, dkneeds::keys::tag::RESOURCES] {
            let mut i = 0;
            while let Some(w) = keys.words(tg, i) {
                k.decl.add(tg, w.0, w.1, w.2);
                i += 1;
            }
        }
        if k.decl.full {
            return Err(E_FORMAT);
        }
        Ok(k)
    }
}

/// Header fields of a `.dkm` that the keys table lookup needs (the loader reads the rest; offsets are big endian).
pub(crate) mod dkm {
    pub use dkneeds::{HEADER, MAGIC};
    pub const H_MAGIC: usize = dkneeds::H_MAGIC as usize;
    pub const H_SIZE: usize = dkneeds::H_SIZE as usize;
    pub const H_REG_OFF: usize = dkneeds::H_REG_OFF as usize;
    pub const H_REG_SIZE: usize = dkneeds::H_REG_SIZE as usize;
}

/// The machine keys of the keys table of a whole `.dkm` file, for the boot's type assignment (`assign::plan`): at most 8, the count returned.
/// A file that is torn or not a `.dkm` has none (its load reports the error).
pub fn machine_keys(file: &[u8], out: &mut [u32; 8]) -> usize {
    let be = |at: usize| u32::from_be_bytes([file[at], file[at + 1], file[at + 2], file[at + 3]]);
    let size = file.len() as u32;
    if size < dkm::HEADER || be(dkm::H_MAGIC) != dkm::MAGIC || be(dkm::H_SIZE) != size {
        return 0;
    }
    let (off, len) = (be(dkm::H_REG_OFF), be(dkm::H_REG_SIZE));
    // the keys table of the header (the pair at 0x90 is keys_off, keys_size)
    if !(off <= size && len <= size - off) {
        return 0;
    }
    let k = match KeysCopy::from_bytes(&file[off as usize..(off + len) as usize]) {
        Ok(k) => k,
        Err(_) => return 0,
    };
    let mut n = 0;
    while n < k.nm && n < 8 {
        out[n] = k.mk[n];
        n += 1;
    }
    n
}

// ---- lowering to records -----------------------------------------------------------------------------------------------------

struct Out {
    buf: *mut u8,
    cap: usize,
    at: usize,
    count: u32,
}

impl Out {
    fn p32(&mut self, v: u32) {
        if !self.buf.is_null() && self.at + 4 <= self.cap {
            unsafe { crate::wr32(self.buf.add(self.at), v) };
        }
        self.at += 4;
    }
    fn text(&mut self, s: &[u8], field: usize) {
        let mut i = 0;
        while i < field {
            if !self.buf.is_null() && self.at + i < self.cap {
                unsafe { *self.buf.add(self.at + i) = if i < s.len() { s[i] } else { 0 } };
            }
            i += 1;
        }
        self.at += field;
    }
    fn bytes(&mut self, s: &[u8]) {
        let mut i = 0;
        while i < s.len() {
            if !self.buf.is_null() && self.at + i < self.cap {
                unsafe { *self.buf.add(self.at + i) = s[i] };
            }
            i += 1;
        }
        self.at += s.len();
    }
    fn head(&mut self, kind: u32, len: usize) {
        self.p32(kind);
        self.p32(len as u32);
        self.count += 1;
    }
}

/// Whether `kind` is a generic kind: a record a module emits with `dk_record_add` (`[records.kinds]` `generic = true`).
fn is_generic(kind: u32) -> bool {
    let mut i = 0;
    while i < dkrt_sdk::records::GENERIC_KINDS.len() {
        if dkrt_sdk::records::GENERIC_KINDS[i] == kind {
            return true;
        }
        i += 1;
    }
    false
}

fn len_of(s: &[u8]) -> usize {
    let mut n = 0;
    while n < s.len() && s[n] != 0 {
        n += 1;
    }
    n
}

/// The (min, max) knob units a glyph allows (`typed.rs glyph_info`); None for an unknown glyph.
fn glyph_range(g: u32) -> Option<(i32, i32)> {
    if g == 0 || g == 1 || g == 2 {
        Some((0, 127))
    } else if g == 3 {
        Some((0, 1))
    } else if g == 4 {
        Some((4, 124))
    } else if g == 5 {
        Some((0, 3))
    } else {
        None
    }
}

/// How the collection lowers: per machine, the knob position of each of its words, and the file index of each collected machine param.
pub struct Plan {
    /// file index (PARAM record order) of collection param i, u8::MAX for an effect param
    pub file: [u8; MAX_PA],
    pub nfile: usize,
    /// collection param index of file param k
    pub of_file: [u8; MAX_PA],
    /// knob position of collection param i (machine params)
    pub pos: [u8; MAX_PA],
    /// word of collection param i within its machine
    pub word: [u8; MAX_PA],
    /// the machine a collection param belongs to
    pub machine: [u8; MAX_PA],
}

impl Plan {
    pub const fn new() -> Plan {
        Plan { file: [u8::MAX; MAX_PA], nfile: 0, of_file: [0; MAX_PA], pos: [0; MAX_PA], word: [0; MAX_PA], machine: [0; MAX_PA] }
    }
}

/// The key of a machine of the collection: the keys table row at its index (`check_commit` refused an index outside the table).
pub fn machine_key(keys: &KeysCopy, m: &CMachine) -> u32 {
    let i = m.index as usize;
    if i < keys.nm { keys.mk[i] } else { 0 }
}

/// The id of an effect of the collection: the keys table row at its index.
pub fn fx_id(keys: &KeysCopy, f: &CFx) -> u32 {
    let i = f.index as usize;
    if i < keys.nf { keys.fk[i] } else { 0 }
}

/// The knob page of machine `m`: the index of its only non-canvas page, None without one; Err when it has two.
fn knob_page(c: &Collect, m: usize) -> Result<Option<usize>, i32> {
    let mut found = None;
    let mut i = 0;
    while i < c.np {
        let g = &c.pages[i];
        if !g.fx && g.owner as usize == m && !g.canvas {
            if found.is_some() {
                return Err(E_FORMAT); // a machine has one knob page for now
            }
            found = Some(i);
        }
        i += 1;
    }
    Ok(found)
}

/// Lower the collection to the `register` section. With `buf` null only the size is computed. `base` and `image_size` turn callback addresses into image offsets.
/// Returns the size in bytes; the errors are refusals of the commit.
/// `voice[m]`: the DSP section has a voice for machine `m` (only then may it render with its own DSP routine).
pub fn lower(c: &Collect, p: &Profile, keys: &KeysCopy, voice: &[bool; MAX_M], base: u32, buf: *mut u8, cap: usize, plan: &mut Plan) -> Result<usize, i32> {
    if c.err != 0 {
        return Err(c.err);
    }
    *plan = Plan::new();
    // machine params: file order is machine by machine, then word order
    let mut m = 0;
    while m < c.nm {
        let mut word = 0usize;
        let mut i = 0;
        while i < c.nparam {
            let q = &c.params[i];
            let g = &c.pages[q.page as usize];
            if !q.fx && !g.fx && g.owner as usize == m {
                if g.canvas {
                    return Err(E_FORMAT); // a canvas page binds params, it adds none
                }
                if word >= p.nslots as usize || word >= KNOBS {
                    return Err(E_FORMAT); // more params than the device has knob positions
                }
                plan.file[i] = plan.nfile as u8;
                plan.of_file[plan.nfile] = i as u8;
                plan.nfile += 1;
                plan.pos[i] = p.slots[word] as u8;
                plan.word[i] = word as u8;
                plan.machine[i] = m as u8;
                word += 1;
            }
            i += 1;
        }
        m += 1;
    }
    let mut o = Out { buf, cap, at: HEADER, count: 0 };
    // PARAM
    let mut k = 0;
    while k < plan.nfile {
        let i = plan.of_file[k] as usize;
        let q = &c.params[i];
        let (gmin, gmax) = match glyph_range(q.glyph) {
            Some(r) => r,
            None => return Err(E_FORMAT),
        };
        if q.min < gmin * 256 || q.max > gmax * 256 {
            return Err(E_FORMAT); // the range does not fit the glyph
        }
        let tpl = p.tpl[q.glyph as usize];
        o.head(KIND_PARAM, record_len(KIND_PARAM));
        o.p32(k as u32);
        o.p32(tpl);
        o.p32(p.slot_base + plan.pos[i] as u32);
        o.p32(q.min as u32);
        o.p32(q.max as u32);
        o.p32(q.def as u32);
        o.text(&q.name, 32);
        o.text(&q.short, 16);
        k += 1;
    }
    // MACHINE, then the knob page of each (page devices)
    let mut m = 0;
    while m < c.nm {
        let mc = &c.machines[m];
        let mut knobs = [0u32; KNOBS];
        if !p.pages {
            if p.tune_code != 0 && (p.tune_slot as usize) < KNOBS {
                knobs[p.tune_slot as usize] = p.tune_code;
            }
            if p.level_code != 0 && (p.level_knob as usize) < KNOBS {
                knobs[p.level_knob as usize] = p.level_code;
            }
            let mut i = 0;
            while i < c.nparam {
                if plan.file[i] != u8::MAX && plan.machine[i] as usize == m {
                    knobs[plan.pos[i] as usize] = KNOB_PARAM | plan.file[i] as u32;
                }
                i += 1;
            }
        }
        o.head(KIND_MACHINE, record_len(KIND_MACHINE));
        o.p32(machine_key(keys, mc));
        o.p32(p.clone_of);
        o.p32(POS_END);
        o.p32((p.own_dsp && voice[m]) as u32);
        o.p32(p.level);
        let mut j = 0;
        while j < KNOBS {
            o.p32(knobs[j]);
            j += 1;
        }
        o.text(&mc.name, TEXT);
        o.text(&mc.short, TEXT);
        m += 1;
    }
    if p.pages {
        let mut m = 0;
        while m < c.nm {
            let kp = knob_page(c, m)?;
            let mut knobs = [0u32; KNOBS];
            let mut i = 0;
            while i < c.nparam {
                if plan.file[i] != u8::MAX && plan.machine[i] as usize == m {
                    knobs[plan.pos[i] as usize] = KNOB_PARAM | plan.file[i] as u32;
                }
                i += 1;
            }
            let mc = &c.machines[m];
            // the page's own name and short name; a machine without a knob page gets its name in capitals as both
            let mut upper = [0u8; TEXT];
            let (name, tab): (&[u8], &[u8]) = match kp {
                Some(g) => (&c.pages[g].name, &c.pages[g].short),
                None => {
                    let n = len_of(&mc.name);
                    let mut j = 0;
                    while j < n {
                        upper[j] = mc.name[j].to_ascii_uppercase();
                        j += 1;
                    }
                    (&mc.name, &upper)
                }
            };
            o.head(KIND_PAGE, record_len(KIND_PAGE));
            o.p32(machine_key(keys, mc));
            let mut j = 0;
            while j < KNOBS {
                o.p32(knobs[j]);
                j += 1;
            }
            o.p32(p.level);
            o.text(tab, TEXT);
            o.text(name, TEXT);
            m += 1;
        }
    } else {
        // the page of a machine-row device still must be unique
        let mut m = 0;
        while m < c.nm {
            knob_page(c, m)?;
            m += 1;
        }
    }
    // EFFECT: an effect with a page (its tab label is the first three characters of its short name)
    let mut f = 0;
    while f < c.nf {
        let fx = &c.fxs[f];
        if fx.page >= 0 {
            let g = &c.pages[fx.page as usize];
            let mut knobs = [0usize; FX_KNOBS];
            let mut n = 0;
            let mut i = 0;
            while i < c.nparam {
                if c.params[i].fx && c.params[i].page as i32 == fx.page {
                    if n >= FX_KNOBS {
                        return Err(E_FORMAT);
                    }
                    knobs[n] = i;
                    n += 1;
                }
                i += 1;
            }
            o.head(KIND_EFFECT, record_len(KIND_EFFECT));
            o.p32(fx.index); // the effect record of the DSP section: the keys table order
            o.p32(fx.mix); // MIX default
            o.p32(n as u32);
            o.p32(fx_id(keys, fx));
            let mut tab = [0u8; 8];
            let label = if len_of(&g.short) > 0 { &g.short } else { &fx.short };
            let mut j = 0;
            while j < FX_SHORT && j < len_of(label) {
                tab[j] = label[j];
                j += 1;
            }
            o.text(&tab, 8);
            o.text(&g.name, TEXT);
            let mut j = 0;
            while j < FX_KNOBS {
                if j < n {
                    let q = &c.params[knobs[j]];
                    if len_of(&q.name) >= TEXT || len_of(&q.short) > 6 {
                        return Err(E_FORMAT);
                    }
                    o.p32(j as u32); // position: the knob's place in the list
                    o.p32(if q.kind == DK_BIPOLAR { 1 } else { 0 });
                    o.p32(q.fmin);
                    o.p32(q.fmax);
                    o.p32(q.fdef);
                    o.text(&q.name, TEXT);
                    o.text(&q.short, 8);
                } else {
                    o.text(&[], 52);
                }
                j += 1;
            }
        }
        f += 1;
    }
    // ACTION
    let mut i = 0;
    while i < c.nparam {
        let q = &c.params[i];
        if q.click != 0 && plan.file[i] != u8::MAX {
            o.head(KIND_ACTION, record_len(KIND_ACTION));
            o.p32(plan.file[i] as u32);
            o.p32(q.click.wrapping_sub(base));
            if q.fx {
                return Err(E_FORMAT);
            }
        }
        i += 1;
    }
    // native pages
    let mut m = 0;
    while m < c.nm {
        let mut canvas = [0usize; MAX_PG];
        let mut n = 0;
        let mut i = 0;
        while i < c.np {
            let g = &c.pages[i];
            if !g.fx && g.owner as usize == m && g.canvas {
                canvas[n] = i;
                n += 1;
            }
            i += 1;
        }
        let v1 = n == 1 && c.pages[canvas[0]].flags == 0;
        let mut taken = 0u32;
        let mut j = 0;
        while j < n {
            let g = &c.pages[canvas[j]];
            let index = if g.flags & 1 != 0 {
                0
            } else if g.flags & 2 != 0 {
                1
            } else if v1 {
                1
            } else if taken & 1 == 0 {
                0
            } else {
                1
            };
            taken |= 1 << index;
            if v1 {
                o.head(KIND_NATIVE_PAGE, record_len(KIND_NATIVE_PAGE));
                o.p32(1);
            } else {
                o.head(KIND_NATIVE_PAGE_V2, record_len(KIND_NATIVE_PAGE_V2));
                o.p32(2);
            }
            o.p32(machine_key(keys, &c.machines[m]));
            o.p32(g.draw.wrapping_sub(base));
            o.p32(g.event.wrapping_sub(base));
            o.text(&g.name, TEXT);
            if !v1 {
                o.p32(index);
                o.p32(g.flags);
            } else {
                // v1 has the binding table at byte 40 of the body: the title ends there
            }
            let mut b = 0;
            while b < 8 {
                if b < g.nbind as usize {
                    let [kind, value, slot] = g.binds[b];
                    let v = if kind == 1 {
                        let i = value as usize;
                        if i >= c.nparam || plan.file[i] == u8::MAX {
                            return Err(E_FORMAT);
                        }
                        plan.file[i] as u32
                    } else {
                        value
                    };
                    o.p32(kind);
                    o.p32(v);
                    o.p32(slot);
                } else {
                    o.p32(0);
                    o.p32(0);
                    o.p32(0);
                }
                b += 1;
            }
            j += 1;
        }
        m += 1;
    }
    // the generic records, as they were added: the owning service checks and applies them
    let mut g = 0;
    while g < c.ng {
        let r = c.grecs[g];
        o.head(r.kind, 8 + r.len as usize);
        o.bytes(&c.gbytes[r.at as usize..r.at as usize + r.len as usize]);
        g += 1;
    }
    let size = o.at;
    if !buf.is_null() {
        if size > cap {
            return Err(E_FORMAT);
        }
        unsafe {
            crate::wr32(buf, REG_MAGIC);
            crate::wr32(buf.add(4), 1);
            crate::wr32(buf.add(8), p.device);
            crate::wr32(buf.add(12), o.count);
        }
    }
    Ok(size)
}

/// The callback arguments of the ACTION and native page records `lower` wrote, in record order.
pub fn apply_args(c: &Collect, plan: &Plan) -> crate::registry::ApplyArgs {
    let mut a = crate::registry::ApplyArgs { click: [0; 32], native: [0; 8] };
    let mut n = 0;
    let mut i = 0;
    while i < c.nparam {
        if c.params[i].click != 0 && plan.file[i] != u8::MAX {
            a.click[n % 32] = c.params[i].click_arg;
            n += 1;
        }
        i += 1;
    }
    n = 0;
    let mut m = 0;
    while m < c.nm {
        let mut i = 0;
        while i < c.np {
            let g = &c.pages[i];
            if !g.fx && g.owner as usize == m && g.canvas {
                a.native[n % 8] = g.arg;
                n += 1;
            }
            i += 1;
        }
        m += 1;
    }
    a
}

/// The eight knob ranges and the latch mask of machine `m` for its voice row: knob position j holds `min | max << 16` of the param on it (0: none or out of range);
/// bit w of the mask is the machine's word w.
pub fn voice_declared(c: &Collect, plan: &Plan, m: usize) -> ([u32; 8], u32) {
    let mut rng = [0u32; 8];
    let mut latched = 0u32;
    let mut i = 0;
    while i < c.nparam {
        if plan.file[i] != u8::MAX && plan.machine[i] as usize == m {
            let q = &c.params[i];
            let j = plan.pos[i] as usize;
            if q.min >= 0 && q.max <= 0xffff && q.min <= q.max && j < 8 {
                rng[j] = (q.min as u32) | (q.max as u32) << 16;
            }
            if q.latched {
                latched |= 1 << plan.word[i];
            }
        }
        i += 1;
    }
    (rng, latched)
}

/// The route flags of the DSP effect record (`FXF_*`) for `dk_fx_routes(allowed, def)`.
pub fn fx_flags(allowed: u32, def: u32) -> u32 {
    let mut f = 0;
    if allowed & ROUTE_SEND != 0 {
        f |= FXF_SEND;
    }
    if allowed & ROUTE_MASTER == 0 {
        f |= FXF_NOMASTER;
    }
    if def == ROUTE_SEND {
        f |= FXF_DEFAULT_SEND;
    }
    f
}

/// The flags of effect `fx` of the collection: `dk_fx_routes` when it was called, else the master only (allowed and default MASTER).
pub fn fx_flags_of_decl(fx: &CFx) -> u32 {
    if fx.routes_set { fx_flags(fx.allowed, fx.def) } else { fx_flags(ROUTE_MASTER, ROUTE_MASTER) }
}

/// The words p0 .. of an effect's row: the defaults of the knobs of its page (float bits), 0 after them.
pub fn fx_defaults(c: &Collect, f: usize) -> [u32; 8] {
    let mut out = [0u32; 8];
    let fx = &c.fxs[f];
    if fx.page < 0 {
        return out;
    }
    let mut n = 0;
    let mut i = 0;
    while i < c.nparam {
        if c.params[i].fx && c.params[i].page as i32 == fx.page && n < 8 {
            out[n] = c.params[i].fdef;
            n += 1;
        }
        i += 1;
    }
    out
}

// ---- the commit checks -------------------------------------------------------------------------------------------------------

/// The numbers of free rows a commit needs; the loader reads them from the registry and the DSP tables.
pub struct Free {
    pub machine_rows: u32,
    pub param_rows: u32,
    pub page_rows: u32,
}

/// The rows the module's own declarations use, as `[uses]` counts them: (machine rows, param rows, page rows) of the lowered records.
pub fn counts_of(c: &Collect, p: &Profile) -> (u32, u32, u32) {
    let mut pages = 0;
    if p.pages {
        pages = c.nm as u32;
    }
    let mut params = 0;
    let mut i = 0;
    while i < c.nparam {
        if !c.params[i].fx && !c.pages[c.params[i].page as usize].fx {
            params += 1;
        }
        i += 1;
    }
    (c.nm as u32, params, pages)
}

/// What `init` declared (`dk_service_declare_*`, addresses in the loaded image at `base`) with the entries that are addresses made image offsets, as the header table
/// has them.
fn declared_offsets(c: &Collect, base: u32) -> Declare {
    let mut d = c.decl;
    let mut i = 0;
    while i < d.n {
        let DeclItem { tag, a, b, c: e } = d.items[i];
        // the entries that are addresses become image offsets, as the table has them
        d.items[i] = if tag == Declare::KIND {
            DeclItem { tag, a, b, c: e.wrapping_sub(base) }
        } else if tag == Declare::SLOT {
            DeclItem { tag, a, b, c: e.wrapping_sub(base) }
        } else if tag == Declare::HOOK || tag == Declare::EXPORT {
            DeclItem { tag, a, b: b.wrapping_sub(base), c: e }
        } else {
            DeclItem { tag, a, b, c: e }
        };
        i += 1;
    }
    d
}

/// Whether what `init` declared (`dk_service_declare_*`, addresses in the loaded image at `base`) is what the header table carries (declare-check-commit):
/// the same kinds with the same lengths and handlers, slots, hooks, exports and resources. A module that declares nothing and has no such entries passes.
pub fn decl_matches(c: &Collect, keys: &KeysCopy, base: u32) -> bool {
    declared_offsets(c, base).same_as(&keys.decl)
}

/// The first item that `a` and `b` do not both hold (the `ROW` item of `a` is left out, as in `Declare::same_as`): the item, and whether it is `a`'s (else it is `b`'s).
fn first_unmatched(a: &Declare, b: &Declare) -> Option<(DeclItem, bool)> {
    let has = |d: &Declare, x: DeclItem| {
        let mut k = 0;
        while k < d.n {
            if d.items[k] == x {
                return true;
            }
            k += 1;
        }
        false
    };
    let mut i = 0;
    while i < a.n {
        if a.items[i].tag != Declare::ROW && !has(b, a.items[i]) {
            return Some((a.items[i], true));
        }
        i += 1;
    }
    i = 0;
    while i < b.n {
        if !has(a, b.items[i]) {
            return Some((b.items[i], false));
        }
        i += 1;
    }
    None
}

/// `None` when `decl_matches`; else the first item that `init` and the header table do not both hold: (its tag, its first word), with 0x100 added to the tag when only
/// the header table has it. A difference without such an item (a full collection) is `(0, 0)` with a tag of 1.
pub fn decl_diff(c: &Collect, keys: &KeysCopy, base: u32) -> Option<(u32, u32)> {
    let d = declared_offsets(c, base);
    if d.same_as(&keys.decl) {
        return None;
    }
    Some(match first_unmatched(&d, &keys.decl) {
        Some((item, declared)) => (item.tag as u32 | if declared { 0 } else { 0x100 }, item.a),
        None => (1, 0),
    })
}

/// The checks of 6.3 that need only the collection, the header's keys table, the effect ids of the other loaded modules and the free rows. Returns OK or the refusal.
pub fn check_commit(c: &Collect, p: &Profile, keys: &KeysCopy, loaded_fx: &[u32], free: &Free) -> i32 {
    if c.err != 0 {
        return c.err;
    }
    // every machine and effect index of a builder call is a row of the keys table
    let mut i = 0;
    while i < c.nm {
        if c.machines[i].index as usize >= keys.nm {
            return E_DECL_ID;
        }
        i += 1;
    }
    i = 0;
    while i < c.nf {
        if c.fxs[i].index as usize >= keys.nf {
            return E_DECL_ID;
        }
        i += 1;
    }
    // an effect id is not twice in this module, and not in a module that is loaded
    i = 0;
    while i < keys.nf {
        if contains(&keys.fk[..i], keys.fk[i]) || contains(loaded_fx, keys.fk[i]) {
            return E_FX_ID_DUP;
        }
        i += 1;
    }
    // the collected rows do not exceed [uses]
    let (m, pa, pg) = counts_of(c, p);
    if m > keys.machine_rows || pa > keys.param_rows || pg > keys.page_rows {
        return E_DECL_USES;
    }
    // a free row for each
    if m > free.machine_rows {
        return E_TYPE_USED;
    }
    if pa > free.param_rows {
        return E_PARAM_FULL;
    }
    if pg > free.page_rows {
        return E_PAGE_FULL;
    }
    OK
}

/// A module binds its DSP section to the machines and effects of its keys table: each voice record names a machine key of the
/// table (`voice_key(i)`), and the section has one effect record for each effect id of the table, in the table's order (`fx_ordinal`).
pub fn check_dsp_keys(keys: &KeysCopy, nvoice: usize, voice_key: impl Fn(usize) -> u32, nfx: usize) -> i32 {
    let mut i = 0;
    while i < nvoice {
        if !contains(&keys.mk[..keys.nm], voice_key(i)) {
            return E_DSP_TYPE;
        }
        i += 1;
    }
    if nfx != keys.nf {
        return E_DECL_ID;
    }
    OK
}

fn contains(a: &[u32], v: u32) -> bool {
    let mut i = 0;
    while i < a.len() {
        if a[i] == v {
            return true;
        }
        i += 1;
    }
    false
}

// ---- what a loaded module keeps ---------------------------------------------------------------------------------------------

/// Per module record: the arena block of the committed record block (address, size; 0: none) and the effect ids the module's header declares.
static mut BLOCKS: [(u32, u32); MAX_MODULES] = [(0, 0); MAX_MODULES];
static mut FX_IDS: [[u32; MAX_RECORDS]; MAX_MODULES] = [[0; MAX_RECORDS]; MAX_MODULES];

/// Remember the committed block and the module's effect ids.
pub unsafe fn keep(rec: usize, addr: u32, size: u32, keys: &KeysCopy) {
    (*addr_of_mut!(BLOCKS))[rec] = (addr, size);
    (*addr_of_mut!(FX_IDS))[rec] = keys.fk;
    let mut i = keys.nf;
    while i < MAX_RECORDS {
        (*addr_of_mut!(FX_IDS))[rec][i] = 0;
        i += 1;
    }
}

/// The effect ids of every loaded module but `rec` (0 marks no id: ids are 1 to 0xffff), for the duplicate check.
pub unsafe fn loaded_fx_ids(rec: usize, out: &mut [u32; MAX_MODULES * MAX_RECORDS]) -> usize {
    let mut n = 0;
    let mut r = 0;
    while r < MAX_MODULES {
        if r != rec {
            let mut i = 0;
            while i < MAX_RECORDS {
                let v = (*addr_of!(FX_IDS))[r][i];
                if v != 0 {
                    out[n] = v;
                    n += 1;
                }
                i += 1;
            }
        }
        r += 1;
    }
    n
}

/// The module `rec` goes away: free its record block and forget its effect ids. Returns the block's address (0: none) for the caller to free.
pub unsafe fn release(rec: usize) -> u32 {
    let a = (*addr_of!(BLOCKS))[rec].0;
    (*addr_of_mut!(BLOCKS))[rec] = (0, 0);
    (*addr_of_mut!(FX_IDS))[rec] = [0; MAX_RECORDS];
    a
}

/// The committed block of module `rec`: (address, size), (0, 0) when it has none.
pub fn block_of(rec: usize) -> (u32, u32) {
    unsafe { (*addr_of!(BLOCKS))[rec] }
}

#[cfg(test)]
mod tests;
