//! Find the service providers of a boot (docs/plans/runtime-and-services.md section 13.2): the hard needs of the selection that no runtime service meets are
//! looked up as `services/<name>.dks` in the store. NEEDS carries only the hash of a service name, so the boot lists `services/` once and matches
//! `name_hash_bytes(stem)` of each file against the needs. A provider found adds its own hard needs to the search (transitively), up to `PROVIDER_MAX` providers.
//!
//! The listing and the file read are the runtime's own (`Cards`: `store::list` with the prefix `services/`, and `Images::stage`, the read that loads a module),
//! so the search links no code of its own for the card. Nothing is allocated: the candidates, the search list and the result are fixed arrays. A provider is
//! checked in this order, and the first failure skips the file (the planner then reports the need as missing): the file's header and role word, the header
//! table range, the table walk and its parse.
use super::role::role_ok;
use super::{Images, Staged};
use crate::handlers::ServiceTable;
use crate::plan::{is_runtime_service, TABLE_MAX};
use dkneeds::keys::{name_hash_bytes, tag, Keys};
use dkrt_sdk::{DkrtStoreEntry, PROVIDER_MAX};

/// The folder of the provider files in the store root, and the file extension.
const DIR: &[u8] = b"services/";
const EXT: &[u8] = b".dks";
/// Longest file stem the search considers (50: `services/<stem>.dks` and its NUL fit the 64 bytes of a selection path), and the longest path it builds.
const STEM_MAX: usize = 50;
pub const PATH_CAP: usize = DIR.len() + STEM_MAX + EXT.len();
/// Most `.dks` files of the folder the search remembers. A folder with more `.dks` files sets `Found::truncated`; the files past the limit are not looked at.
const CAND_MAX: usize = 32;
/// Most service hashes the search keeps. Distinct non-runtime needs beyond the provider slots can never all be met; 4 times `PROVIDER_MAX` leaves room for
/// needs of refused or unmatched services. A need past the limit sets `Found::truncated`.
const WANT_MAX: usize = 4 * PROVIDER_MAX;

/// What the search needs of the card: the runtime's store listing (`prefix` NUL-terminated, files only, in path order) and the read of one whole file into
/// the staging buffer. Every `Images` is one (the real one lists and reads the store; the fakes of the host tests list and read a map).
pub trait Cards {
    unsafe fn list(&mut self, prefix: *const u8, out: *mut DkrtStoreEntry, max: u32) -> i32;
    unsafe fn stage(&mut self, path: *const u8) -> Result<Staged, i32>;
}

impl<M: Images> Cards for M {
    unsafe fn list(&mut self, prefix: *const u8, out: *mut DkrtStoreEntry, max: u32) -> i32 {
        Images::list(self, prefix, out, max)
    }
    unsafe fn stage(&mut self, path: *const u8) -> Result<Staged, i32> {
        Images::stage(self, path)
    }
}

/// The providers found: for provider `k`, its store path and its header table (the bytes the planner parses).
pub struct Found {
    pub n: usize,
    /// A limit of the search was passed (more `.dks` files than `CAND_MAX`, more needs than `WANT_MAX`): a provider may be missing from the result.
    pub truncated: bool,
    // the search's work arrays live here, not on the boot thread's stack: the files of the folder and whether each is taken, and the service hashes searched for
    cands: [DkrtStoreEntry; CAND_MAX + 1],
    c_used: [bool; CAND_MAX],
    search: [u32; WANT_MAX],
    pub path_len: [usize; PROVIDER_MAX],
    pub paths: [[u8; PATH_CAP]; PROVIDER_MAX],
    pub table_len: [usize; PROVIDER_MAX],
    pub tables: [[u8; TABLE_MAX]; PROVIDER_MAX],
}

impl Found {
    pub const fn new() -> Found {
        Found {
            n: 0,
            truncated: false,
            cands: unsafe { core::mem::zeroed() },
            c_used: [false; CAND_MAX],
            search: [0; WANT_MAX],
            path_len: [0; PROVIDER_MAX],
            paths: [[0; PATH_CAP]; PROVIDER_MAX],
            table_len: [0; PROVIDER_MAX],
            tables: [[0; TABLE_MAX]; PROVIDER_MAX],
        }
    }
    /// The store path of provider `k`.
    pub fn path(&self, k: usize) -> &[u8] {
        &self.paths[k][..self.path_len[k]]
    }
    /// The header table of provider `k`.
    pub fn table(&self, k: usize) -> &[u8] {
        &self.tables[k][..self.table_len[k]]
    }
}

fn has(list: &[u32], n: usize, h: u32) -> bool {
    let mut i = 0;
    while i < n {
        if list[i] == h {
            return true;
        }
        i += 1;
    }
    false
}

/// The length of the NUL-terminated path of candidate `c`.
fn path_of(c: &DkrtStoreEntry) -> &[u8] {
    let mut n = 0;
    while n < c.path.len() && c.path[n] != 0 {
        n += 1;
    }
    &c.path[..n]
}

/// The stem of `services/<stem>.dks`: None for another extension, a stem over `STEM_MAX` bytes or a file in a subfolder.
fn stem_of(path: &[u8]) -> Option<&[u8]> {
    if path.len() <= DIR.len() + EXT.len() || !path.ends_with(EXT) {
        return None;
    }
    let stem = &path[DIR.len()..path.len() - EXT.len()];
    if stem.len() > STEM_MAX || stem.contains(&b'/') {
        return None;
    }
    Some(stem)
}

#[inline(never)]
/// Check the staged file `file` as a provider and put its table, without the BIND entry, into slot `k` of `out`; false when the file is not a usable provider.
fn table_of(file: &[u8], k: usize, out: &mut Found) -> bool {
    let be = |p: usize| u32::from_be_bytes([file[p], file[p + 1], file[p + 2], file[p + 3]]);
    if file.len() < dkneeds::H_REG_SIZE as usize + 4 || be(dkneeds::H_ROLE as usize) != 1 {
        return false;
    }
    let (off, len) = (be(dkneeds::H_REG_OFF as usize) as usize, be(dkneeds::H_REG_SIZE as usize) as usize);
    if len < 8 || off > file.len() || len > file.len() - off {
        return false;
    }
    let src = &file[off..off + len];
    // The full table must parse (heads and sizes fit, the entries use it up exactly). The planner's copy is the table without its BIND entry: a service with
    // many BIND items has a table over TABLE_MAX, and the planner never needs BIND (the load reads it). The entries are in ascending tag order and BIND has
    // the highest tag, so it is the last entry: the copy is the table up to it, with one entry less.
    let full = match Keys::parse(src) {
        Ok(k) => k,
        Err(_) => return false,
    };
    if role_ok(1, &full).is_err() {
        return false;
    }
    let mut n_entries = u16::from_be_bytes([src[6], src[7]]) as usize;
    let mut end = len;
    if full.count(tag::BIND) != 0 {
        let mut pos = 8;
        let mut e = 1;
        while e < n_entries {
            pos += 8 + u32::from_be_bytes([src[pos + 4], src[pos + 5], src[pos + 6], src[pos + 7]]) as usize;
            e += 1;
        }
        end = pos;
        n_entries -= 1;
    }
    if end > TABLE_MAX {
        return false; // the copy does not hold the table
    }
    let t = &mut out.tables[k];
    t[..end].copy_from_slice(&src[..end]);
    t[6..8].copy_from_slice(&(n_entries as u16).to_be_bytes());
    out.table_len[k] = end;
    true
}

/// Read provider candidate `c` into slot `out.n` of `out`; false when the file is not a usable provider.
unsafe fn read_provider<C: Cards>(cards: &mut C, c: usize, out: &mut Found) -> bool {
    let k = out.n;
    let at = path_of(&out.cands[c]).len();
    out.paths[k][..at].copy_from_slice(path_of(&out.cands[c]));
    out.path_len[k] = at;
    let mut path = [0u8; PATH_CAP + 1];
    path[..at].copy_from_slice(&out.paths[k][..at]);
    let staged = match cards.stage(path.as_ptr()) {
        Ok(s) => s,
        Err(_) => return false,
    };
    table_of(core::slice::from_raw_parts(staged.ptr, staged.size as usize), k, out)
}

/// Find the providers for the service hashes `want` (the unresolved hard needs of the selection). No `services/` folder, a busy store (`E_BUSY`) or any
/// listing error finds nothing, without an error. `services` is the runtime's own table: a need of a runtime service is not searched for.
pub unsafe fn find<C: Cards>(cards: &mut C, services: &ServiceTable, want: &[u32], out: &mut Found) {
    out.n = 0;
    out.truncated = false;
    if want.is_empty() {
        return;
    }
    // the folder is listed once: the `.dks` files are remembered
    out.c_used = [false; CAND_MAX];
    let found = cards.list(b"services/\0".as_ptr(), out.cands.as_mut_ptr(), (CAND_MAX + 1) as u32);
    if found <= 0 {
        return;
    }
    let nc = if found as usize > CAND_MAX {
        out.truncated = true;
        CAND_MAX
    } else {
        found as usize
    };
    // the search list: the needs given, then the needs of each provider found
    let mut ns = 0;
    let mut i = 0;
    while i < want.len() {
        if !has(&out.search, ns, want[i]) {
            if ns < WANT_MAX {
                out.search[ns] = want[i];
                ns += 1;
            } else {
                out.truncated = true;
            }
        }
        i += 1;
    }
    let mut w = 0;
    while w < ns && out.n < PROVIDER_MAX {
        let h = out.search[w];
        w += 1;
        let mut c = 0;
        while c < nc {
            let hit = !out.c_used[c] && matches!(stem_of(path_of(&out.cands[c])), Some(stem) if name_hash_bytes(stem) == h);
            if hit {
                out.c_used[c] = true;
                if read_provider(cards, c, out) {
                    let k = out.n;
                    if let Ok(t) = Keys::parse(&out.tables[k][..out.table_len[k]]) {
                        let mut ni = 0;
                        while let Some(n) = t.words(tag::NEEDS, ni) {
                            ni += 1;
                            if n.2 & 1 != 0 && !is_runtime_service(n.0, services) && !has(&out.search, ns, n.0) {
                                if ns < WANT_MAX {
                                    out.search[ns] = n.0;
                                    ns += 1;
                                } else {
                                    out.truncated = true;
                                }
                            }
                        }
                    }
                    out.n += 1;
                    break;
                }
            }
            c += 1;
        }
    }
}

/// The store of the host tests: files by path, and a switch that makes every listing busy.
#[cfg(test)]
pub(crate) mod fake {
    use super::*;
    use std::vec::Vec;

    pub struct FakeCards {
        pub files: Vec<(Vec<u8>, Vec<u8>)>,
        pub busy: bool,
        staged: Vec<u8>,
    }

    impl Cards for FakeCards {
        unsafe fn list(&mut self, prefix: *const u8, out: *mut DkrtStoreEntry, max: u32) -> i32 {
            if self.busy {
                return dkrt_sdk::E_BUSY;
            }
            let n = crate::cstr_len(prefix, 64);
            let prefix = core::slice::from_raw_parts(prefix, n);
            let mut k = 0;
            for (name, bytes) in &self.files {
                if k < max as usize && name.starts_with(prefix) && name.len() < 64 {
                    let e = &mut *out.add(k);
                    e.path = [0; 64];
                    e.path[..name.len()].copy_from_slice(name);
                    e.size = bytes.len() as u32;
                    k += 1;
                }
            }
            k as i32
        }
        unsafe fn stage(&mut self, path: *const u8) -> Result<Staged, i32> {
            let n = crate::cstr_len(path, 64);
            let key = core::slice::from_raw_parts(path, n);
            match self.files.iter().find(|(name, _)| name == key) {
                Some((_, bytes)) => {
                    self.staged = bytes.clone();
                    Ok(Staged { ptr: self.staged.as_ptr(), size: self.staged.len() as u32 })
                }
                None => Err(dkrt_sdk::E_NOT_FOUND),
            }
        }
    }

    pub fn cards(files: Vec<(std::string::String, Vec<u8>)>) -> FakeCards {
        FakeCards { files: files.into_iter().map(|(n, b)| (n.into_bytes(), b)).collect(), busy: false, staged: Vec::new() }
    }
}

#[cfg(test)]
mod tests {
    use super::fake::{cards, FakeCards};
    use super::*;
    use crate::core_api::{CoreApi, REAL_CORE};
    use dkneeds::keys::{name_hash, write_table, Need, Pair, Table};
    use dkrt_abi::service::{Service, ServiceRow};
    use std::vec;
    use std::vec::Vec;

    struct S;
    impl<C: CoreApi> Service<C> for S {}
    static ROWS: [ServiceRow; 1] = [ServiceRow::of::<S, crate::core_api::RealCore>("machines", &[])];
    static ROWS_P: dkrt_abi::service::RowPtrs<1> = dkrt_abi::service::RowPtrs::of(&ROWS);

    const KEEP: Table<'static> =
        Table { needs: &[], uses: &[], keys: &[], kinds: &[], slots: &[], hooks: &[], exports: &[], resources: &[], provides: &[], bind: &[] };

    /// A role `role` file with `table` as it is.
    fn raw_file(role: u32, table: &[u8]) -> Vec<u8> {
        let off = 0xa0usize;
        let mut f = vec![0u8; off];
        f[dkneeds::H_ROLE as usize..][..4].copy_from_slice(&role.to_be_bytes());
        f[dkneeds::H_REG_OFF as usize..][..4].copy_from_slice(&(off as u32).to_be_bytes());
        f[dkneeds::H_REG_SIZE as usize..][..4].copy_from_slice(&(table.len() as u32).to_be_bytes());
        f.extend_from_slice(table);
        f
    }

    fn table_of_t(t: &Table) -> Vec<u8> {
        let mut b = vec![0u8; dkneeds::keys::table_size(t)];
        write_table(t, &mut b).unwrap();
        b
    }

    /// A provider file: a header of `role`, then the table of a service `name` that needs `needs`.
    fn file(role: u32, name: &str, needs: &[&str]) -> Vec<u8> {
        let ns: Vec<Need> = needs.iter().map(|n| Need { service: name_hash(n), min_version: 0, hard: true }).collect();
        raw_file(role, &table_of_t(&Table { needs: &ns, provides: &[Pair { a: name_hash(name), b: 0x010000 }], ..KEEP }))
    }

    fn store_with(files: &[(&str, u32, &[&str])]) -> FakeCards {
        cards(files.iter().map(|(name, role, needs)| (std::format!("services/{name}.dks"), file(*role, name, needs))).collect())
    }

    fn run(c: &mut FakeCards, want: &[&str]) -> Found {
        let services = ServiceTable::new(&ROWS_P.0, &REAL_CORE);
        let want: Vec<u32> = want.iter().map(|n| name_hash(n)).collect();
        let mut out = Found::new();
        unsafe { find(c, &services, &want, &mut out) };
        out
    }

    #[test]
    fn no_services_directory_finds_nothing() {
        assert_eq!(run(&mut cards(vec![]), &["lfo"]).n, 0);
    }

    #[test]
    fn one_file_whose_stem_hashes_to_the_need_is_found_with_its_table() {
        let mut fs = store_with(&[("lfo", 1, &[]), ("other", 1, &[])]);
        let out = run(&mut fs, &["lfo"]);
        assert_eq!(out.n, 1);
        assert_eq!(out.path(0), b"services/lfo.dks");
        let keys = Keys::parse(out.table(0)).unwrap();
        assert_eq!(keys.words(tag::PROVIDES, 0).unwrap().0, name_hash("lfo"));
        assert_eq!(run(&mut fs, &["missing"]).n, 0);
    }

    #[test]
    fn the_needs_of_a_provider_are_searched_too_whatever_the_listing_order() {
        // "base" lists before "top": it is found only because the folder is listed once and remembered
        let mut fs = store_with(&[("top", 1, &["base", "runtime"]), ("base", 1, &[])]);
        let out = run(&mut fs, &["top"]);
        assert_eq!(out.n, 2);
        assert_eq!(out.path(0), b"services/top.dks");
        assert_eq!(out.path(1), b"services/base.dks");
    }

    /// The file of a service whose BIND entry holds `n` items of 64 stock bytes: its table is over `TABLE_MAX`.
    fn file_with_bind(name: &str, n: usize) -> Vec<u8> {
        let stock = [0xa5u8; 64];
        let binds: Vec<dkneeds::keys::Bind> =
            (0..n).map(|i| dkneeds::keys::Bind { site: i as u32 + 1, addr: 0x1000, tramp: 0x20, stock: &stock }).collect();
        raw_file(1, &table_of_t(&Table { provides: &[Pair { a: name_hash(name), b: 0x010000 }], bind: &binds, ..KEEP }))
    }

    #[test]
    fn a_provider_whose_full_table_is_over_the_planner_limit_because_of_bind_is_found_and_planned() {
        let f = file_with_bind("big", 8);
        let full = u32::from_be_bytes(f[dkneeds::H_REG_SIZE as usize..][..4].try_into().unwrap()) as usize;
        assert!(full > TABLE_MAX, "{full}");
        let mut fs = cards(vec![("services/big.dks".into(), f)]);
        let out = run(&mut fs, &["big"]);
        assert_eq!(out.n, 1);
        // the planner's copy is the table without BIND, still version 2 with its one PROVIDES
        let keys = Keys::parse(out.table(0)).unwrap();
        assert_eq!(keys.version(), 2);
        assert_eq!(keys.count(tag::BIND), 0);
        assert_eq!(keys.count(tag::PROVIDES), 1);
        assert!(out.table(0).len() <= TABLE_MAX);
        let user = table_of_t(&Table { needs: &[Need { service: name_hash("big"), min_version: 0x010000, hard: true }], ..KEEP });
        let services = ServiceTable::new(&ROWS_P.0, &REAL_CORE);
        let caps = [dkrt_sdk::DkrtCap { kind: dkrt_sdk::proto::CAPK_MODULES as u32, limit: 8, used: 0, largest: 8 }];
        let env = crate::plan::Env { services, runtime: 0x000900, caps: &caps };
        let p = crate::plan::plan_with(&env, &[Some(Keys::parse(&user).unwrap())], &[Some(keys)]);
        assert_eq!((p.provider_verdicts[0], p.verdicts[0]), (dkrt_abi::plan::Verdict::Accepted, dkrt_abi::plan::Verdict::Accepted));
    }

    #[test]
    fn a_stem_over_50_bytes_is_skipped_so_its_path_fits_a_selection_path() {
        let ok = "a".repeat(50);
        let long = "b".repeat(51);
        assert!(PATH_CAP < 64, "services/ + 50 + .dks + NUL fit 64 bytes");
        let mut fs = store_with(&[(&ok, 1, &[]), (&long, 1, &[])]);
        assert_eq!(run(&mut fs, &[&ok]).n, 1);
        assert_eq!(run(&mut fs, &[&long]).n, 0);
    }

    #[test]
    fn a_busy_listing_finds_nothing() {
        let mut fs = store_with(&[("lfo", 1, &[])]);
        fs.busy = true;
        assert_eq!(run(&mut fs, &["lfo"]).n, 0);
        fs.busy = false;
        assert_eq!(run(&mut fs, &["lfo"]).n, 1);
    }

    #[test]
    fn a_file_with_the_wrong_role_or_in_a_subfolder_is_skipped() {
        let mut fs = store_with(&[("lfo", 0, &[]), ("fx", 1, &[])]);
        assert_eq!(run(&mut fs, &["lfo"]).n, 0);
        assert_eq!(run(&mut fs, &["lfo", "fx"]).n, 1);
        let mut fs = cards(vec![("services/sub/lfo.dks".into(), file(1, "lfo", &[]))]);
        assert_eq!(run(&mut fs, &["lfo"]).n, 0);
    }

    fn table_bytes(provides: &[Pair]) -> Vec<u8> {
        table_of_t(&Table { provides, ..KEEP })
    }

    fn found_in(table: &[u8]) -> usize {
        run(&mut cards(vec![("services/bad.dks".into(), raw_file(1, table))]), &["bad"]).n
    }

    #[test]
    fn a_provider_with_a_malformed_or_wrong_table_is_skipped() {
        let one = [Pair { a: name_hash("bad"), b: 0x010000 }];
        assert_eq!(found_in(&table_bytes(&one)), 1);
        // a version 1 table
        let mut v1 = table_bytes(&one);
        v1[4..6].copy_from_slice(&1u16.to_be_bytes());
        assert_eq!(found_in(&v1), 0);
        // two PROVIDES items
        assert_eq!(found_in(&table_bytes(&[Pair { a: name_hash("bad"), b: 0x010000 }, Pair { a: name_hash("other"), b: 0 }])), 0);
        // a header that claims more entries than the table holds
        let mut more = table_bytes(&one);
        let n = u16::from_be_bytes([more[6], more[7]]);
        more[6..8].copy_from_slice(&(n + 1).to_be_bytes());
        assert_eq!(found_in(&more), 0);
        // bytes after the last entry
        let mut trailing = table_bytes(&one);
        trailing.extend_from_slice(&[0; 4]);
        assert_eq!(found_in(&trailing), 0);
    }

    #[test]
    fn a_folder_with_more_files_than_the_search_remembers_sets_truncated() {
        let names: Vec<std::string::String> = (0..CAND_MAX + 1).map(|k| std::format!("c{k}")).collect();
        let files: Vec<(&str, u32, &[&str])> = names.iter().map(|n| (n.as_str(), 1u32, &[][..])).collect();
        assert!(run(&mut store_with(&files), &["c0"]).truncated);
        let mut fs = store_with(&files[..CAND_MAX]);
        assert!(!run(&mut fs, &["c0"]).truncated);
        // more distinct needs than the search list holds
        let want: Vec<u32> = (0..WANT_MAX as u32 + 1).collect();
        let services = ServiceTable::new(&ROWS_P.0, &REAL_CORE);
        let mut out = Found::new();
        unsafe { find(&mut fs, &services, &want, &mut out) };
        assert!(out.truncated);
    }

    #[test]
    fn the_search_stops_at_the_provider_cap() {
        let names: Vec<std::string::String> = (0..PROVIDER_MAX + 2).map(|k| std::format!("s{k}")).collect();
        let files: Vec<(&str, u32, &[&str])> = names.iter().map(|n| (n.as_str(), 1u32, &[][..])).collect();
        let want: Vec<&str> = names.iter().map(|n| n.as_str()).collect();
        assert_eq!(run(&mut store_with(&files), &want).n, PROVIDER_MAX);
    }
}
