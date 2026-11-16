//! Find the service providers of a boot (docs/plans/runtime-and-services.md section 13.2): the hard needs of the selection that no runtime service meets are
//! looked up as `services/<name>.dks` in the store. NEEDS carries only the hash of a service name, so the boot lists `services/` once and matches
//! `name_hash(stem)` of each file against the needs. A provider found adds its own hard needs to the search (transitively), up to `PROVIDER_MAX` providers.
//!
//! Nothing is allocated: the candidates, the search list and the result are fixed arrays. A provider is checked in this order, and the first failure skips
//! the file (the planner then reports the need as missing): the role word in the 64-byte head (`stat_head`), the header table range, the table read, its parse.
//! Not wired into the boot yet (R12-3 loads the providers).
#![allow(dead_code)]
use crate::handlers::ServiceTable;
use crate::plan::{is_runtime_service, TABLE_MAX};
use super::role::{role_of, role_ok};
use dkneeds::keys::{name_hash, tag, Keys};
use dkrt_abi::storage::{DirEntry, EntryKind, ListStep, Storage, ROOT_STORE};
use dkrt_sdk::PROVIDER_MAX;

/// The folder of the provider files in the store root, and the file extension.
const DIR: &[u8] = b"services";
const EXT: &[u8] = b".dks";
/// Longest file stem the search considers (50: `services/<stem>.dks` and its NUL fit the 64 bytes of a selection path), and the longest path it builds.
const STEM_MAX: usize = 50;
pub const PATH_CAP: usize = DIR.len() + 1 + STEM_MAX + EXT.len();
/// Most `.dks` files of the folder the search remembers. The store has no per-folder limit, only the card's size (`dkrt_sdk::store`): 32 is what the 8 KiB table
/// buffer (`TBL_CAP`) holds in 256-byte entries. A folder with more `.dks` files sets `Found::truncated`; the files past the limit are not looked at.
const CAND_MAX: usize = 32;
/// Most service hashes the search keeps. Distinct non-runtime needs beyond the provider slots can never all be met; 4 times `PROVIDER_MAX` leaves room for
/// needs of refused or unmatched services. A need past the limit sets `Found::truncated`.
const WANT_MAX: usize = 4 * PROVIDER_MAX;
/// Most listing steps of one search (a `Pending` step is asked again).
const STEPS_MAX: u32 = 4096;
/// The header bytes read to get the table range: up to and including the size word.
const HEAD_LEN: usize = dkneeds::H_REG_SIZE as usize + 4;

/// The providers found: for provider `k`, its store path and its header table (the bytes the planner parses).
pub struct Found {
    pub n: usize,
    /// A limit of the search was passed (more `.dks` files than `CAND_MAX`, more needs than `WANT_MAX`): a provider may be missing from the result.
    pub truncated: bool,
    // the search's work arrays live here, not on the boot thread's stack: the `.dks` files of the folder (hash, stem, taken) and the service hashes searched for
    c_hash: [u32; CAND_MAX],
    c_stem: [[u8; STEM_MAX]; CAND_MAX],
    c_len: [usize; CAND_MAX],
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
            c_hash: [0; CAND_MAX],
            c_stem: [[0; STEM_MAX]; CAND_MAX],
            c_len: [0; CAND_MAX],
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

/// Read provider `stem` into slot `out.n` of `out`; false when the file is not a usable provider.
unsafe fn read_provider<S: Storage>(s: &S, stem: &[u8], out: &mut Found) -> bool {
    let k = out.n;
    let mut at = 0;
    out.paths[k][..DIR.len()].copy_from_slice(DIR);
    at += DIR.len();
    out.paths[k][at] = b'/';
    at += 1;
    out.paths[k][at..at + stem.len()].copy_from_slice(stem);
    at += stem.len();
    out.paths[k][at..at + EXT.len()].copy_from_slice(EXT);
    at += EXT.len();
    out.path_len[k] = at;
    let path = &out.paths[k][..at];
    let mut head = [0u8; 64];
    let size = match s.stat_head(ROOT_STORE, path, &mut head) {
        Ok((st, _)) if st.kind == EntryKind::File => st.size,
        _ => return false,
    };
    if role_of(&head) != 1 {
        return false;
    }
    let mut hdr = [0u8; HEAD_LEN];
    match s.read_range(ROOT_STORE, path, 0, &mut hdr) {
        Ok(n) if n == HEAD_LEN => {}
        _ => return false,
    }
    let be = |p: usize| u32::from_be_bytes([hdr[p], hdr[p + 1], hdr[p + 2], hdr[p + 3]]);
    let (off, len) = (be(dkneeds::H_REG_OFF as usize), be(dkneeds::H_REG_SIZE as usize));
    if len < 8 || off as u64 + len as u64 > size as u64 {
        return false;
    }
    // The planner's copy is the table without its BIND entry: a service with many BIND items has a table over TABLE_MAX, and the planner never needs BIND
    // (the load reads it). Walk every entry of the full table, entry head by entry head: the heads and sizes must fit the table and the entries must use
    // it up exactly, as many as the table declares. The entries other than BIND are copied; BIND is skipped.
    let len = len as usize;
    let t = &mut out.tables[k];
    match s.read_range(ROOT_STORE, path, off, &mut t[..8]) {
        Ok(n) if n == 8 => {}
        _ => return false,
    }
    let n_entries = u16::from_be_bytes([t[6], t[7]]) as usize;
    let mut pos = 8usize; // in the file's table
    let mut copied = 8usize; // in the copy
    let mut kept = 0usize;
    let mut e = 0;
    while e < n_entries {
        e += 1;
        if pos + 8 > len {
            return false; // the entry head is past the table
        }
        if copied + 8 > TABLE_MAX {
            return false; // the copy does not hold another entry head
        }
        match s.read_range(ROOT_STORE, path, off + pos as u32, &mut t[copied..copied + 8]) {
            Ok(n) if n == 8 => {}
            _ => return false,
        }
        let tg = u16::from_be_bytes([t[copied], t[copied + 1]]);
        let plen = u32::from_be_bytes([t[copied + 4], t[copied + 5], t[copied + 6], t[copied + 7]]) as usize;
        if plen % 4 != 0 || plen > len - pos - 8 {
            return false;
        }
        if tg != tag::BIND {
            if plen > TABLE_MAX - copied - 8 {
                return false; // the copy does not hold the entry
            }
            if plen != 0 {
                match s.read_range(ROOT_STORE, path, off + (pos + 8) as u32, &mut t[copied + 8..copied + 8 + plen]) {
                    Ok(n) if n == plen => {}
                    _ => return false,
                }
            }
            copied += 8 + plen;
            kept += 1;
        }
        pos += 8 + plen;
    }
    if pos != len {
        return false; // bytes after the declared entries
    }
    t[6..8].copy_from_slice(&(kept as u16).to_be_bytes());
    match Keys::parse(&t[..copied]) {
        // BIND is not in the copy and role 1 does not look at it
        Ok(keys) if role_ok(1, &keys).is_ok() => {}
        _ => return false,
    }
    out.table_len[k] = copied;
    true
}

/// Find the providers for the service hashes `want` (the unresolved hard needs of the selection). `state` is a listing block: at least
/// `list_state_bytes(ROOT_STORE)` bytes, 16-byte aligned. No `services/` folder, a busy store (`E_BUSY`) or any listing error finds nothing, without an error.
/// `services` is the runtime's own table: a need of a runtime service is not searched for.
pub unsafe fn find<S: Storage>(s: &S, services: &ServiceTable, state: *mut u8, want: &[u32], out: &mut Found) {
    out.n = 0;
    out.truncated = false;
    if want.is_empty() {
        return;
    }
    // the folder is listed once: the `.dks` files are remembered by the hash of their stem
    out.c_used = [false; CAND_MAX];
    let mut nc = 0;
    if s.list_open(ROOT_STORE, DIR, state).is_err() {
        return;
    }
    let mut e = DirEntry::empty();
    let mut steps = 0;
    loop {
        steps += 1;
        if steps > STEPS_MAX {
            break;
        }
        match s.list_next(state, &mut e) {
            Ok(ListStep::End) => break,
            Ok(ListStep::Pending) => continue,
            Ok(ListStep::Entry) => {}
            Err(_) => return,
        }
        let name = e.name();
        if e.stat.kind != EntryKind::File || name.len() <= EXT.len() || !name.ends_with(EXT) {
            continue;
        }
        let stem = &name[..name.len() - EXT.len()];
        if stem.len() > STEM_MAX {
            continue;
        }
        if let Ok(text) = core::str::from_utf8(stem) {
            if nc >= CAND_MAX {
                out.truncated = true;
                continue;
            }
            out.c_hash[nc] = name_hash(text);
            out.c_stem[nc][..stem.len()].copy_from_slice(stem);
            out.c_len[nc] = stem.len();
            nc += 1;
        }
    }
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
            if !out.c_used[c] && out.c_hash[c] == h {
                out.c_used[c] = true;
                // a copy of the stem: `read_provider` borrows all of `out`
                let mut stem = [0u8; STEM_MAX];
                let sl = out.c_len[c];
                stem[..sl].copy_from_slice(&out.c_stem[c][..sl]);
                if read_provider(s, &stem[..sl], out) {
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core_api::{CoreApi, REAL_CORE};
    use crate::fake_storage::FakeStorage;
    use dkneeds::keys::{write_table, Need, Pair, Table};
    use dkrt_abi::service::{Service, ServiceRow};
    use dkrt_abi::storage::Stat;
    use dkrt_sdk::E_BUSY;
    use std::vec;
    use std::vec::Vec;

    struct S;
    impl<C: CoreApi> Service<C> for S {}
    static ROWS: [ServiceRow; 1] = [ServiceRow::of::<S, crate::core_api::RealCore>("machines", &[])];
    static ROWS_P: dkrt_abi::service::RowPtrs<1> = dkrt_abi::service::RowPtrs::of(&ROWS);

    #[repr(C, align(16))]
    struct Block([u8; 1024]);

    const KEEP: Table<'static> =
        Table { needs: &[], uses: &[], keys: &[], kinds: &[], slots: &[], hooks: &[], exports: &[], resources: &[], provides: &[], bind: &[] };

    /// A provider file: a header of `role`, then the table of a service `name` that needs `needs`.
    fn file(role: u32, name: &str, needs: &[&str]) -> Vec<u8> {
        let ns: Vec<Need> = needs.iter().map(|n| Need { service: name_hash(n), min_version: 0, hard: true }).collect();
        let t = Table { needs: &ns, provides: &[Pair { a: name_hash(name), b: 0x010000 }], ..KEEP };
        let mut table = vec![0u8; dkneeds::keys::table_size(&t)];
        write_table(&t, &mut table).unwrap();
        let off = 0xa0usize;
        let mut f = vec![0u8; off];
        f[dkneeds::H_ROLE as usize..][..4].copy_from_slice(&role.to_be_bytes());
        f[dkneeds::H_REG_OFF as usize..][..4].copy_from_slice(&(off as u32).to_be_bytes());
        f[dkneeds::H_REG_SIZE as usize..][..4].copy_from_slice(&(table.len() as u32).to_be_bytes());
        f.extend_from_slice(&table);
        f
    }

    fn store_with(files: &[(&str, u32, &[&str])]) -> FakeStorage {
        let fs = FakeStorage::new();
        fs.add_dir(ROOT_STORE, b"services");
        for (name, role, needs) in files {
            let path = std::format!("services/{name}.dks");
            fs.add_file(ROOT_STORE, path.as_bytes(), &file(*role, name, needs), 0, [0; 32]);
        }
        fs
    }

    fn run<St: Storage>(st: &St, want: &[&str]) -> Found {
        let services = ServiceTable::new(&ROWS_P.0, &REAL_CORE);
        let want: Vec<u32> = want.iter().map(|n| name_hash(n)).collect();
        let mut block = Block([0; 1024]);
        assert!(st.list_state_bytes(ROOT_STORE) <= block.0.len());
        let mut out = Found::new();
        unsafe { find(st, &services, block.0.as_mut_ptr(), &want, &mut out) };
        out
    }

    #[test]
    fn no_services_directory_finds_nothing() {
        let fs = FakeStorage::new();
        assert_eq!(run(&fs, &["lfo"]).n, 0);
    }

    #[test]
    fn one_file_whose_stem_hashes_to_the_need_is_found_with_its_table() {
        let fs = store_with(&[("lfo", 1, &[]), ("other", 1, &[])]);
        let out = run(&fs, &["lfo"]);
        assert_eq!(out.n, 1);
        assert_eq!(out.path(0), b"services/lfo.dks");
        let keys = Keys::parse(out.table(0)).unwrap();
        assert_eq!(keys.words(tag::PROVIDES, 0).unwrap().0, name_hash("lfo"));
        assert_eq!(run(&fs, &["missing"]).n, 0);
    }

    #[test]
    fn the_needs_of_a_provider_are_searched_too_whatever_the_listing_order() {
        // "base" lists before "top": it is found only because the folder is listed once and remembered
        let fs = store_with(&[("top", 1, &["base", "runtime"]), ("base", 1, &[])]);
        let out = run(&fs, &["top"]);
        assert_eq!(out.n, 2);
        assert_eq!(out.path(0), b"services/top.dks");
        assert_eq!(out.path(1), b"services/base.dks");
    }

    /// The file of a service whose BIND entry holds `n` items of 64 stock bytes: its table is over `TABLE_MAX`.
    fn file_with_bind(name: &str, n: usize) -> Vec<u8> {
        let stock = [0xa5u8; 64];
        let binds: Vec<dkneeds::keys::Bind> =
            (0..n).map(|i| dkneeds::keys::Bind { site: i as u32 + 1, addr: 0x1000, tramp: 0x20, stock: &stock }).collect();
        let t = Table { provides: &[Pair { a: name_hash(name), b: 0x010000 }], bind: &binds, ..KEEP };
        let mut table = vec![0u8; dkneeds::keys::table_size(&t)];
        write_table(&t, &mut table).unwrap();
        let off = 0xa0usize;
        let mut f = vec![0u8; off];
        f[dkneeds::H_ROLE as usize..][..4].copy_from_slice(&1u32.to_be_bytes());
        f[dkneeds::H_REG_OFF as usize..][..4].copy_from_slice(&(off as u32).to_be_bytes());
        f[dkneeds::H_REG_SIZE as usize..][..4].copy_from_slice(&(table.len() as u32).to_be_bytes());
        f.extend_from_slice(&table);
        f
    }

    #[test]
    fn a_provider_whose_full_table_is_over_the_planner_limit_because_of_bind_is_found_and_planned() {
        let f = file_with_bind("big", 8);
        let full = u32::from_be_bytes(f[dkneeds::H_REG_SIZE as usize..][..4].try_into().unwrap()) as usize;
        assert!(full > TABLE_MAX, "{full}");
        let fs = FakeStorage::new();
        fs.add_dir(ROOT_STORE, b"services");
        fs.add_file(ROOT_STORE, b"services/big.dks", &f, 0, [0; 32]);
        let out = run(&fs, &["big"]);
        assert_eq!(out.n, 1);
        // the planner's copy is the table without BIND, still version 2 with its one PROVIDES
        let keys = Keys::parse(out.table(0)).unwrap();
        assert_eq!(keys.version(), 2);
        assert_eq!(keys.count(tag::BIND), 0);
        assert_eq!(keys.count(tag::PROVIDES), 1);
        assert!(out.table(0).len() <= TABLE_MAX);
        let user = Table { needs: &[Need { service: name_hash("big"), min_version: 0x010000, hard: true }], ..KEEP };
        let mut ub = vec![0u8; dkneeds::keys::table_size(&user)];
        write_table(&user, &mut ub).unwrap();
        let services = ServiceTable::new(&ROWS_P.0, &REAL_CORE);
        let caps = [dkrt_sdk::DkrtCap { kind: dkrt_sdk::proto::CAPK_MODULES as u32, limit: 8, used: 0, largest: 8 }];
        let env = crate::plan::Env { services, runtime: 0x000900, caps: &caps };
        let p = crate::plan::plan_with(&env, &[Some(Keys::parse(&ub).unwrap())], &[Some(keys)]);
        assert_eq!((p.provider_verdicts[0], p.verdicts[0]), (dkrt_abi::plan::Verdict::Accepted, dkrt_abi::plan::Verdict::Accepted));
    }

    #[test]
    fn a_stem_over_50_bytes_is_skipped_so_its_path_fits_a_selection_path() {
        let ok = "a".repeat(50);
        let long = "b".repeat(51);
        assert!(PATH_CAP < 64, "services/ + 50 + .dks + NUL fit 64 bytes");
        let fs = store_with(&[(&ok, 1, &[]), (&long, 1, &[])]);
        assert_eq!(run(&fs, &[&ok]).n, 1);
        assert_eq!(run(&fs, &[&long]).n, 0);
    }

    /// A store whose listing is always busy.
    struct Busy<'a>(&'a FakeStorage);
    impl Storage for Busy<'_> {
        fn path_max(&self, root: u32) -> u32 {
            self.0.path_max(root)
        }
        unsafe fn stat(&self, root: u32, path: &[u8]) -> Result<Stat, i32> {
            self.0.stat(root, path)
        }
        unsafe fn read_head(&self, root: u32, path: &[u8], out: &mut [u8; 64]) -> Result<(), i32> {
            self.0.read_head(root, path, out)
        }
        unsafe fn stat_head(&self, root: u32, path: &[u8], out: &mut [u8; 64]) -> Result<(Stat, usize), i32> {
            self.0.stat_head(root, path, out)
        }
        unsafe fn read_range(&self, root: u32, path: &[u8], offset: u32, out: &mut [u8]) -> Result<usize, i32> {
            self.0.read_range(root, path, offset, out)
        }
        unsafe fn read_all(&self, root: u32, path: &[u8], dst: *mut u8, cap: u32) -> Result<u32, i32> {
            self.0.read_all(root, path, dst, cap)
        }
        fn list_state_bytes(&self, root: u32) -> usize {
            self.0.list_state_bytes(root)
        }
        unsafe fn list_open(&self, root: u32, dir: &[u8], state: *mut u8) -> Result<(), i32> {
            self.0.list_open(root, dir, state)
        }
        unsafe fn list_next(&self, _state: *mut u8, _out: &mut DirEntry) -> Result<ListStep, i32> {
            Err(E_BUSY)
        }
    }

    #[test]
    fn a_busy_listing_finds_nothing() {
        let fs = store_with(&[("lfo", 1, &[])]);
        assert_eq!(run(&Busy(&fs), &["lfo"]).n, 0);
        assert_eq!(run(&fs, &["lfo"]).n, 1);
    }

    #[test]
    fn a_file_with_the_wrong_role_is_skipped() {
        let fs = store_with(&[("lfo", 0, &[]), ("fx", 1, &[])]);
        assert_eq!(run(&fs, &["lfo"]).n, 0);
        assert_eq!(run(&fs, &["lfo", "fx"]).n, 1);
    }

    /// A role 1 file named `name` with `table` as it is (the header's table size is `table.len()`).
    fn raw_file(table: &[u8]) -> Vec<u8> {
        let off = 0xa0usize;
        let mut f = vec![0u8; off];
        f[dkneeds::H_ROLE as usize..][..4].copy_from_slice(&1u32.to_be_bytes());
        f[dkneeds::H_REG_OFF as usize..][..4].copy_from_slice(&(off as u32).to_be_bytes());
        f[dkneeds::H_REG_SIZE as usize..][..4].copy_from_slice(&(table.len() as u32).to_be_bytes());
        f.extend_from_slice(table);
        f
    }

    fn table_bytes(provides: &[Pair]) -> Vec<u8> {
        let t = Table { provides, ..KEEP };
        let mut b = vec![0u8; dkneeds::keys::table_size(&t)];
        write_table(&t, &mut b).unwrap();
        b
    }

    fn found_in(table: &[u8]) -> usize {
        let fs = FakeStorage::new();
        fs.add_dir(ROOT_STORE, b"services");
        fs.add_file(ROOT_STORE, b"services/bad.dks", &raw_file(table), 0, [0; 32]);
        run(&fs, &["bad"]).n
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
        let fs = store_with(&files);
        assert!(run(&fs, &["c0"]).truncated);
        let fs = store_with(&files[..CAND_MAX]);
        assert!(!run(&fs, &["c0"]).truncated);
        // more distinct needs than the search list holds
        let want: Vec<u32> = (0..WANT_MAX as u32 + 1).collect();
        let services = ServiceTable::new(&ROWS_P.0, &REAL_CORE);
        let mut block = Block([0; 1024]);
        let mut out = Found::new();
        unsafe { find(&fs, &services, block.0.as_mut_ptr(), &want, &mut out) };
        assert!(out.truncated);
    }

    #[test]
    fn the_search_stops_at_the_provider_cap() {
        let names: Vec<std::string::String> = (0..PROVIDER_MAX + 2).map(|k| std::format!("s{k}")).collect();
        let files: Vec<(&str, u32, &[&str])> = names.iter().map(|n| (n.as_str(), 1u32, &[][..])).collect();
        let fs = store_with(&files);
        let want: Vec<&str> = names.iter().map(|n| n.as_str()).collect();
        assert_eq!(run(&fs, &want).n, PROVIDER_MAX);
    }
}
