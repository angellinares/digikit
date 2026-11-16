//! The Exports table (docs/plans/runtime-and-services.md section 13.2, R12-3a): the functions the loaded providers offer by name. A provider publishes its
//! EXPORTS after its commit and the table withdraws them when its record goes away. A later load resolves an import that the runtime's table does not have
//! to a published address (`resolve`; R12-4: the order is `IMPORTS`, then `SVC_IMPORTS`, then these).
//!
//! The table is a field of the Loader: only the loader reads and writes it, and module code cannot reach it.
use dkrt_sdk::{E_PLAN_OWNER, E_TABLE_FULL};

/// Most exports published at once, over all providers.
pub const EXPORTS_MAX: usize = 32;

#[derive(Clone, Copy)]
struct Row {
    hash: u32,
    addr: u32,
    rec: u8,
}

pub struct Exports {
    rows: [Row; EXPORTS_MAX],
    n: usize,
}

impl Exports {
    pub const fn new() -> Exports {
        Exports { rows: [Row { hash: 0, addr: 0, rec: 0 }; EXPORTS_MAX], n: 0 }
    }

    /// Published rows.
    pub fn count(&self) -> usize {
        self.n
    }

    /// Publish `addr` as the export `hash` of record `rec`. `E_PLAN_OWNER`: another record published the name; `E_TABLE_FULL`: no room.
    pub fn publish(&mut self, rec: usize, hash: u32, addr: u32) -> Result<(), i32> {
        if self.resolve(hash).is_some() {
            return Err(E_PLAN_OWNER);
        }
        if self.n >= EXPORTS_MAX {
            return Err(E_TABLE_FULL);
        }
        self.rows[self.n] = Row { hash, addr, rec: rec as u8 };
        self.n += 1;
        Ok(())
    }

    /// Take every export of record `rec` down (idempotent).
    pub fn withdraw(&mut self, rec: usize) {
        let mut i = 0;
        while i < self.n {
            if self.rows[i].rec == rec as u8 {
                self.n -= 1;
                self.rows[i] = self.rows[self.n];
            } else {
                i += 1;
            }
        }
    }

    /// The address published for `hash`.
    pub fn resolve(&self, hash: u32) -> Option<u32> {
        let mut i = 0;
        while i < self.n {
            if self.rows[i].hash == hash {
                return Some(self.rows[i].addr);
            }
            i += 1;
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn publish_resolve_and_withdraw_by_record() {
        let mut e = Exports::new();
        e.publish(14, 0xaa, 0x1000).unwrap();
        e.publish(14, 0xbb, 0x1010).unwrap();
        e.publish(15, 0xcc, 0x2000).unwrap();
        assert_eq!((e.resolve(0xaa), e.resolve(0xcc), e.resolve(0xdd)), (Some(0x1000), Some(0x2000), None));
        e.withdraw(14);
        assert_eq!((e.resolve(0xaa), e.resolve(0xbb), e.resolve(0xcc), e.count()), (None, None, Some(0x2000), 1));
        e.withdraw(14);
        assert_eq!(e.count(), 1);
    }

    #[test]
    fn a_name_has_one_owner_and_the_table_has_a_limit() {
        let mut e = Exports::new();
        e.publish(14, 1, 0x10).unwrap();
        assert_eq!(e.publish(15, 1, 0x20), Err(E_PLAN_OWNER));
        assert_eq!(e.resolve(1), Some(0x10));
        let mut e = Exports::new();
        for i in 0..EXPORTS_MAX as u32 {
            e.publish(14, i + 1, i).unwrap();
        }
        assert_eq!(e.publish(15, 0x9999, 0), Err(E_TABLE_FULL));
    }
}
