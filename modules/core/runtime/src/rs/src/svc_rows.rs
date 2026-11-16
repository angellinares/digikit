//! The rows of loaded service files (R12-4 step 4): `Registry::add` puts the `ServiceRow` a `.dks` declared into the tail of the runtime's row table
//! (`gen_services::ROWS`), `Registry::remove` takes it out again.
//!
//! The table is the in-crate rows (`gen_services::STATIC`, from `[[service]]` with `rust =`) and then the loaded rows, sorted by the ordinal of the service in
//! `[[service]]` (`gen_services::ORDER`), so the order the core calls services in does not depend on the order files load in. At most `PROVIDER_MAX` rows are
//! loaded. `FAILED` (`handlers.rs`) covers the in-crate prefix only, and `is_runtime_service` reads that prefix only.
//!
//! `Rows` holds the owners and the count; the pointers are in the slice it is given, so a host test uses a table of its own.
use dkrt_abi::service::{ServiceRow, ServiceTable};
use dkrt_sdk::{E_DECL_KIND, E_PLAN_FIT, E_PLAN_OWNER, OK, PROVIDER_MAX};

/// The loaded rows: how many, and the owner (module record + 1) of each, in table order.
pub struct Rows {
    n: usize,
    owner: [u8; PROVIDER_MAX],
}

impl Rows {
    pub const fn new() -> Rows {
        Rows { n: 0, owner: [0; PROVIDER_MAX] }
    }

    /// The loaded rows.
    pub fn len(&self) -> usize {
        self.n
    }

    /// Add `row` of module record `rec` to `table` (`fixed` in-crate rows, then room for `PROVIDER_MAX`): `E_DECL_KIND` when its name is no `[[service]]` (`order`),
    /// `E_PLAN_OWNER` when a row of that name is there already (a service is one owner's, in-crate or loaded), `E_PLAN_FIT` when the room is full.
    ///
    /// # Safety
    /// The pointers of `table[..fixed + len()]` point at rows.
    pub unsafe fn add(&mut self, table: &mut [*const ServiceRow], fixed: usize, order: &[&str], rec: usize, row: &ServiceRow) -> i32 {
        let ord = |name: &str| order.iter().position(|n| *n == name);
        let mine = match ord(row.name()) {
            Some(o) => o,
            None => return E_DECL_KIND,
        };
        let mut i = 0;
        while i < fixed + self.n {
            if (*table[i]).name() == row.name() {
                return E_PLAN_OWNER;
            }
            i += 1;
        }
        if self.n == PROVIDER_MAX {
            return E_PLAN_FIT;
        }
        // the first loaded row of a later ordinal: the new row goes before it
        let mut at = self.n;
        let mut k = 0;
        while k < self.n {
            let o = ord((*table[fixed + k]).name()).unwrap_or(usize::MAX);
            if o > mine {
                at = k;
                break;
            }
            k += 1;
        }
        let mut k = self.n;
        while k > at {
            table[fixed + k] = table[fixed + k - 1];
            self.owner[k] = self.owner[k - 1];
            k -= 1;
        }
        table[fixed + at] = row as *const ServiceRow;
        self.owner[at] = rec as u8 + 1;
        self.n += 1;
        OK
    }

    /// Take the row of module record `rec` out of `table` (nothing when it has none).
    pub fn remove(&mut self, table: &mut [*const ServiceRow], fixed: usize, rec: usize) {
        let mut at = 0;
        while at < self.n && self.owner[at] != rec as u8 + 1 {
            at += 1;
        }
        if at == self.n {
            return;
        }
        let mut k = at;
        while k + 1 < self.n {
            table[fixed + k] = table[fixed + k + 1];
            self.owner[k] = self.owner[k + 1];
            k += 1;
        }
        table[fixed + self.n - 1] = core::ptr::null();
        self.owner[self.n - 1] = 0;
        self.n -= 1;
    }
}

static mut LIVE: Rows = Rows::new();

/// `Registry::add` of the runtime: the row of a service file joins the table of the services.
pub unsafe fn add(rec: usize, row: &ServiceRow) -> i32 {
    let t = &mut *core::ptr::addr_of_mut!(crate::gen_services::ROWS);
    (*core::ptr::addr_of_mut!(LIVE)).add(t, crate::gen_services::STATIC, &crate::gen_services::ORDER, rec, row)
}

/// `Registry::remove` of the runtime.
pub unsafe fn remove(rec: usize) {
    let t = &mut *core::ptr::addr_of_mut!(crate::gen_services::ROWS);
    (*core::ptr::addr_of_mut!(LIVE)).remove(t, crate::gen_services::STATIC, rec)
}

/// The rows the core calls now: the in-crate ones and the loaded ones.
pub fn rows() -> &'static [*const ServiceRow] {
    unsafe {
        let n = crate::gen_services::STATIC + (*core::ptr::addr_of!(LIVE)).len();
        core::slice::from_raw_parts(core::ptr::addr_of!(crate::gen_services::ROWS) as *const *const ServiceRow, n)
    }
}

/// Host tests of the runtime run with the machines service as a loaded file has it (the service is a `.dks` on the device): its row joins the table once, at the last
/// provider record, the first time the services are asked for.
#[cfg(test)]
pub fn test_machines() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| unsafe {
        add(crate::MAX_MODULES + PROVIDER_MAX - 1, crate::handlers::exports::machines_row());
    });
}

/// The table of the services the core calls now (`handlers::services`): `base` with the live row list.
pub fn table(base: ServiceTable) -> ServiceTable {
    ServiceTable { rows: rows(), ..base }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core_api::RealCore;
    use dkrt_abi::core_api::CoreApi;
    use dkrt_abi::service::Service;

    struct Nothing;
    impl<C: CoreApi> Service<C> for Nothing {}

    macro_rules! row {
        ($name:literal) => {{
            static R: ServiceRow = ServiceRow::of::<Nothing, RealCore>($name, &[]);
            &R
        }};
    }

    const ORDER: [&str; 5] = ["a", "b", "c", "d", "e"];

    fn names(t: &[*const ServiceRow], n: usize) -> std::vec::Vec<&'static str> {
        t[..n].iter().map(|p| unsafe { (**p).name() }).collect()
    }

    #[test]
    fn rows_are_sorted_by_ordinal_whatever_the_order_they_are_added_in() {
        let mut t = [core::ptr::null(); 1 + PROVIDER_MAX];
        t[0] = row!("a");
        let mut r = Rows::new();
        unsafe {
            assert_eq!(r.add(&mut t, 1, &ORDER, 3, row!("d")), OK);
            assert_eq!(r.add(&mut t, 1, &ORDER, 1, row!("b")), OK);
            assert_eq!(r.add(&mut t, 1, &ORDER, 2, row!("e")), OK);
        }
        assert_eq!(names(&t, 1 + r.len()), ["a", "b", "d", "e"]);
        assert_eq!(r.owner[..3], [2, 4, 3]);
        // taking one out closes the gap and clears the end; a second remove does nothing
        r.remove(&mut t, 1, 3);
        assert_eq!(names(&t, 1 + r.len()), ["a", "b", "e"]);
        assert!(t[1 + r.len()].is_null());
        r.remove(&mut t, 1, 3);
        r.remove(&mut t, 1, 0);
        assert_eq!(r.len(), 2);
        assert_eq!(r.owner[..3], [2, 3, 0]);
    }

    #[test]
    fn a_name_that_is_no_service_or_has_an_owner_or_does_not_fit_is_refused() {
        let mut t = [core::ptr::null(); 1 + PROVIDER_MAX];
        t[0] = row!("a");
        let mut r = Rows::new();
        unsafe {
            assert_eq!(r.add(&mut t, 1, &ORDER, 0, row!("zzz")), E_DECL_KIND);
            // an in-crate service is not provided twice, and a loaded one has one owner
            assert_eq!(r.add(&mut t, 1, &ORDER, 0, row!("a")), E_PLAN_OWNER);
            assert_eq!(r.add(&mut t, 1, &ORDER, 0, row!("b")), OK);
            assert_eq!(r.add(&mut t, 1, &ORDER, 1, row!("b")), E_PLAN_OWNER);
            assert_eq!(r.len(), 1);
            // the room is PROVIDER_MAX: fill it with a longer order list
            let long = ["a", "b", "c", "d", "e", "f", "g", "h"];
            assert_eq!(r.add(&mut t, 1, &long, 2, row!("c")), OK);
            assert_eq!(r.add(&mut t, 1, &long, 3, row!("d")), OK);
            assert_eq!(r.add(&mut t, 1, &long, 4, row!("e")), OK);
            assert_eq!(r.add(&mut t, 1, &long, 5, row!("f")), OK);
            assert_eq!(r.add(&mut t, 1, &long, 6, row!("g")), OK);
            assert_eq!(r.add(&mut t, 1, &long, 7, row!("h")), E_PLAN_FIT);
        }
        assert_eq!(r.len(), PROVIDER_MAX);
        assert_eq!(names(&t, 1 + r.len()), ["a", "b", "c", "d", "e", "f", "g"]);
    }
}
