//! The role of a `.dkm` or `.dks` file (docs/plans/runtime-and-services.md section 13.2): the role word of the header and what the header table may hold for it.
//! `RoleSteps` are the steps that differ by role, chosen from the role word: a module has none, a service (`.dks`) checks, keeps its table, publishes its exports and
//! installs its hooks (the row of the service is R12-4).
//! The service search (`discover.rs`) uses `role_of` and `role_ok`; the loader runs the steps.
use super::exports::EXPORTS_MAX;
use super::image::Head;
use super::{Images, Loader, ProvRec, Registry};
use dkneeds::keys::{tag, BindRef, Keys};
use dkrt_abi::platform::Platform;
use dkrt_sdk::{E_FORMAT, E_NOMEM, E_SVC_ABI, OK};

/// What a role step may need of the file besides the record: the staged file (valid until `init` runs) and where its table and image are.
#[derive(Clone, Copy)]
pub struct PlaceArgs {
    pub file: *const u8,
    pub keys_off: u32,
    pub keys_len: u32,
    pub image_size: u32,
}

/// The steps of one role, in the order the loader runs them. `check` runs after the header parse and before anything is allocated (`file` is the staged
/// file); `place` after the image is copied and before `init` (it copies what `init` could overwrite in the staging buffer); `activate` after the commit,
/// the last thing a load does; `undo` when a later step fails or the record goes away. A step has what it needs in the Loader and its seams.
pub struct RoleSteps<P: Platform, M: Images, R: Registry> {
    pub check: unsafe fn(&mut Loader<P, M, R>, file: *const u8, head: &Head) -> i32,
    pub place: unsafe fn(&mut Loader<P, M, R>, rec: usize, args: &PlaceArgs) -> i32,
    pub activate: unsafe fn(&mut Loader<P, M, R>, rec: usize) -> i32,
    pub undo: unsafe fn(&mut Loader<P, M, R>, rec: usize),
}

impl<P: Platform, M: Images, R: Registry> Clone for RoleSteps<P, M, R> {
    fn clone(&self) -> Self {
        *self
    }
}
impl<P: Platform, M: Images, R: Registry> Copy for RoleSteps<P, M, R> {}

impl<P: Platform, M: Images, R: Registry> RoleSteps<P, M, R> {
    /// The steps of the role word `role`: 1 is a service; every other word is a module, as before the table (`role_ok` refuses another word in the search).
    pub const fn of(role: u32) -> Self {
        if role == 1 {
            Self::SERVICE
        } else {
            Self::MODULE
        }
    }

    /// A module has no step of its own.
    pub const MODULE: Self = RoleSteps { check: no_check, place: no_place, activate: no_activate, undo: no_undo };

    /// A service (a provider record): the check refuses an unknown ABI, a DSP section, a bad table and a hook site that is not stock, all before anything is
    /// allocated; place keeps the table; activate publishes the exports and installs the hooks, last; undo withdraws and frees.
    pub const SERVICE: Self = RoleSteps { check: service_check, place: service_place, activate: service_activate, undo: service_undo };
}

unsafe fn no_check<P: Platform, M: Images, R: Registry>(_: &mut Loader<P, M, R>, _: *const u8, _: &Head) -> i32 {
    OK
}
unsafe fn no_place<P: Platform, M: Images, R: Registry>(_: &mut Loader<P, M, R>, _: usize, _: &PlaceArgs) -> i32 {
    OK
}
unsafe fn no_activate<P: Platform, M: Images, R: Registry>(_: &mut Loader<P, M, R>, _: usize) -> i32 {
    OK
}
unsafe fn no_undo<P: Platform, M: Images, R: Registry>(_: &mut Loader<P, M, R>, _: usize) {}

/// The table of the staged file, or the refusal.
#[inline(never)]
unsafe fn table_of<'a>(file: *const u8, off: u32, len: u32) -> Result<Keys<'a>, i32> {
    Keys::parse(core::slice::from_raw_parts(file.add(off as usize), len as usize)).map_err(|_| E_FORMAT)
}

/// The `BIND` items of `keys` into `out`; their count.
#[inline(never)]
fn binds_of<'a>(keys: &Keys<'a>, out: &mut [BindRef<'a>; BIND_MAX]) -> usize {
    let mut n = 0;
    while n < BIND_MAX {
        match keys.bind(n) {
            Some(b) => out[n] = b,
            None => break,
        }
        n += 1;
    }
    n
}

const BIND_MAX: usize = dkneeds::BIND_MAX as usize;
const NO_BIND: BindRef<'static> = BindRef { site: 0, addr: 0, len: 0, tramp: 0, stock: &[] };

unsafe fn service_check<P: Platform, M: Images, R: Registry>(l: &mut Loader<P, M, R>, file: *const u8, head: &Head) -> i32 {
    if !l.r.abi_ok(crate::rd32(file.add(dkneeds::H_SVC_ABI as usize))) {
        return E_SVC_ABI;
    }
    // a provider's DSP section waits for R12-8 (the DSP part of a service)
    if crate::rd32(file.add(super::H_DSP_SIZE)) != 0 {
        return E_FORMAT;
    }
    let (off, len) = (crate::rd32(file.add(super::H_REG_OFF)), crate::rd32(file.add(super::H_REG_SIZE)));
    let size = crate::rd32(file.add(dkneeds::H_SIZE as usize));
    if !super::in_file(off, len, size) {
        return E_FORMAT;
    }
    let keys = match table_of(file, off, len) {
        Ok(k) => k,
        Err(e) => return e,
    };
    if let Err(e) = role_ok(1, &keys) {
        return e;
    }
    if keys.bind_count() > BIND_MAX || keys.count(tag::EXPORTS) > EXPORTS_MAX {
        return E_FORMAT;
    }
    // every hook site holds the stock bytes the bind recorded: checked here, before `init` runs, and again under the mask at the install
    let mut binds = [NO_BIND; BIND_MAX];
    let n = binds_of(&keys, &mut binds);
    match l.installer.check(&l.p, &binds[..n], 0, head.image_size) {
        Ok(()) => OK,
        Err(e) => e,
    }
}

/// The provider index of record `rec`.
fn index_of(rec: usize) -> usize {
    rec - crate::MAX_MODULES
}

unsafe fn service_place<P: Platform, M: Images, R: Registry>(l: &mut Loader<P, M, R>, rec: usize, a: &PlaceArgs) -> i32 {
    let idx = index_of(rec);
    l.provs[idx] = ProvRec::EMPTY;
    l.provs[idx].image_size = a.image_size;
    let keys = match table_of(a.file, a.keys_off, a.keys_len) {
        Ok(k) => k,
        Err(e) => return e,
    };
    // the exports are image offsets
    let mut i = 0;
    while let Some(w) = keys.words(tag::EXPORTS, i) {
        if w.1 >= a.image_size {
            return E_FORMAT;
        }
        i += 1;
    }
    if keys.bind_count() == 0 && keys.count(tag::EXPORTS) == 0 && keys.count(tag::SLOTS) == 0 {
        return OK;
    }
    // the staged file is overwritten while `init` reads from the store: BIND, EXPORTS and SLOTS are kept in an arena block (the whole table)
    let blk = match l.m.alloc(a.keys_len) {
        Some(b) => b,
        None => return E_NOMEM,
    };
    core::ptr::copy_nonoverlapping(a.file.add(a.keys_off as usize), blk.ptr, a.keys_len as usize);
    l.provs[idx].block = Some(blk);
    l.provs[idx].len = a.keys_len;
    OK
}

unsafe fn service_activate<P: Platform, M: Images, R: Registry>(l: &mut Loader<P, M, R>, rec: usize) -> i32 {
    let idx = index_of(rec);
    let pr = l.provs[idx];
    let blk = match pr.block {
        Some(b) => b,
        None => return OK,
    };
    let base = super::records::module(rec).base;
    let keys = match Keys::parse(core::slice::from_raw_parts(blk.ptr, pr.len as usize)) {
        Ok(k) => k,
        Err(_) => return E_FORMAT,
    };
    let mut i = 0;
    while let Some(w) = keys.words(tag::EXPORTS, i) {
        if let Err(e) = l.exports.publish(rec, w.0, base.wrapping_add(w.1)) {
            return e;
        }
        i += 1;
    }
    let mut binds = [NO_BIND; BIND_MAX];
    let nb = binds_of(&keys, &mut binds);
    // the pointer slots the file fills: a SLOTS item is the name hash of a `[[slot]]` of an image adapter (the image's info table gives the address of its word) and the entry
    // of this file. A slot the image does not have, or a name that is an `[[info]]` data table, refuses the file, before anything is installed.
    let mut slots = [(0u32, 0u32); dkneeds::SLOTS_MAX as usize];
    let mut ns = 0;
    for sl in keys.slots() {
        if ns == slots.len() || sl.entry >= pr.image_size {
            return E_FORMAT;
        }
        let at = l.p.info_slot(sl.id);
        if at.is_null() {
            return E_FORMAT;
        }
        slots[ns] = (at as usize as u32, base.wrapping_add(sl.entry));
        ns += 1;
    }
    // the hooks go in last: nothing after this can fail, so a live site is never taken down again
    match l.installer.install(&l.p, rec as u8, &binds[..nb], base, pr.image_size, &slots[..ns]) {
        Ok(()) => OK,
        Err(e) => e,
    }
}

unsafe fn service_undo<P: Platform, M: Images, R: Registry>(l: &mut Loader<P, M, R>, rec: usize) {
    let idx = index_of(rec);
    l.exports.withdraw(rec);
    if let Some(b) = l.provs[idx].block {
        l.m.free(b.addr);
    }
    l.provs[idx] = ProvRec::EMPTY;
}

/// The role word of a file: 0 a module, 1 a service (`.dks`). `head` is the first 64 bytes of the file; the word is big endian, like every header field.
pub fn role_of(head: &[u8; 64]) -> u32 {
    let at = dkneeds::H_ROLE as usize;
    u32::from_be_bytes([head[at], head[at + 1], head[at + 2], head[at + 3]])
}

/// Whether the header table `keys` fits the role: a module (0) has no PROVIDES, HOOKS or BIND item; a service (1) has table version 2 and exactly one
/// PROVIDES item. Another role is `E_FORMAT`, like the other refusals.
pub fn role_ok(role: u32, keys: &Keys) -> Result<(), i32> {
    if role == 0 {
        // No packer writes HOOKS for a module (patchkit major3.rs writes only USES and KEYS, dkm.rs adds NEEDS): HOOKS are a service's (role 1).
        if keys.count(tag::PROVIDES) != 0 || keys.count(tag::HOOKS) != 0 || keys.count(tag::BIND) != 0 {
            return Err(E_FORMAT);
        }
        Ok(())
    } else if role == 1 {
        if keys.version() != 2 || keys.count(tag::PROVIDES) != 1 {
            return Err(E_FORMAT);
        }
        Ok(())
    } else {
        Err(E_FORMAT)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use dkneeds::keys::{write_table, Pair, Table};
    use std::vec;
    use std::vec::Vec;

    const KEEP: Table<'static> =
        Table { needs: &[], uses: &[], keys: &[], kinds: &[], slots: &[], hooks: &[], exports: &[], resources: &[], provides: &[], bind: &[] };

    fn bytes(t: &Table) -> Vec<u8> {
        let mut b = vec![0u8; dkneeds::keys::table_size(t)];
        write_table(t, &mut b).unwrap();
        b
    }

    fn check(role: u32, t: &Table) -> Result<(), i32> {
        let b = bytes(t);
        role_ok(role, &Keys::parse(&b).unwrap())
    }

    const ONE: &[Pair] = &[Pair { a: 7, b: 0x010000 }];

    #[test]
    fn the_role_word_is_read_big_endian_from_the_header() {
        let mut head = [0u8; 64];
        assert_eq!(role_of(&head), 0);
        head[dkneeds::H_ROLE as usize + 3] = 1;
        assert_eq!(role_of(&head), 1);
        head[dkneeds::H_ROLE as usize] = 1;
        assert_eq!(role_of(&head), 0x0100_0001);
    }

    #[test]
    fn a_module_with_nothing_for_a_service_is_ok() {
        assert_eq!(check(0, &KEEP), Ok(()));
    }

    #[test]
    fn a_module_with_a_provides_hooks_or_bind_item_is_refused() {
        assert_eq!(check(0, &Table { provides: ONE, ..KEEP }), Err(E_FORMAT));
        assert_eq!(check(0, &Table { hooks: &[Pair { a: 9, b: 4 }], ..KEEP }), Err(E_FORMAT));
    }

    #[test]
    fn a_service_needs_version_2_and_exactly_one_provides_item() {
        assert_eq!(check(1, &Table { provides: ONE, ..KEEP }), Ok(()));
        assert_eq!(check(1, &KEEP), Err(E_FORMAT));
        assert_eq!(check(1, &Table { provides: &[Pair { a: 7, b: 0 }, Pair { a: 8, b: 0 }], ..KEEP }), Err(E_FORMAT));
        // a table of version 1
        let mut b = bytes(&Table { provides: ONE, ..KEEP });
        b[4..6].copy_from_slice(&1u16.to_be_bytes());
        if let Ok(k) = Keys::parse(&b) {
            assert_eq!(role_ok(1, &k), Err(E_FORMAT));
        }
    }

    #[test]
    fn another_role_is_refused() {
        assert_eq!(check(2, &Table { provides: ONE, ..KEEP }), Err(E_FORMAT));
        assert_eq!(check(u32::MAX, &KEEP), Err(E_FORMAT));
    }
}
