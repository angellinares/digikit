//! Read leases on store files (docs/plans/usb-fast.md step 1b): a bounded table of extents that a reader (the sample pool of the Digitone II) is
//! still reading. A commit that replaces or deletes a file frees its extent and the next write may reuse it, so a reader that holds the old
//! extent would get the bytes of another file. The writer therefore refuses to replace or delete a leased file and keeps new data and tables out
//! of leased extents (`extents` goes into the `extra` list of `first_fit` and `commit`).
//!
//! A pure table: the runtime owns one instance, takes a lease under the store lock (so the extent is the committed one) and changes the table
//! with the interrupts masked, because a release may come from another task. Ids are 1 to 0x7fff. Leases are RAM only: a reboot clears any lease that was never released.

/// Most leases at once (`STORE_LEASES` of abi.toml; the sample pool checks its request count against it). The free-space code keeps `3 + 2 + MAX` extents apart (`FIXED_MAX` of `store.rs`).
pub const MAX: usize = crate::gen_abi::STORE_LEASES;

#[derive(Clone, Copy)]
struct Lease {
    /// 0: the slot is free.
    id: i32,
    owner: u8,
    first: u32,
    sectors: u32,
}

const FREE: Lease = Lease {
    id: 0,
    owner: 0,
    first: 0,
    sectors: 0,
};

pub struct Leases {
    v: [Lease; MAX],
    next: i32,
}

impl Leases {
    pub const fn new() -> Leases {
        Leases {
            v: [FREE; MAX],
            next: 0,
        }
    }

    /// Lease the extent of `sectors` from sector `first` for `owner`. The id (> 0), or `E_NOMEM` when the table is full. A file with no data
    /// (`sectors` 0) gets a lease that guards nothing.
    pub fn take(&mut self, owner: u8, first: u32, sectors: u32) -> i32 {
        let mut slot = MAX;
        let mut i = 0;
        while i < MAX {
            if self.v[i].id == 0 {
                slot = i;
                break;
            }
            i += 1;
        }
        if slot >= MAX {
            return crate::E_NOMEM;
        }
        // a fresh id that no live lease has (the counter wraps after 0x7fff takes)
        let mut id;
        loop {
            self.next = (self.next + 1) & 0x7fff;
            id = self.next;
            if id != 0 && !self.has(id) {
                break;
            }
        }
        self.v[slot] = Lease {
            id,
            owner,
            first,
            sectors,
        };
        id
    }

    fn has(&self, id: i32) -> bool {
        let mut i = 0;
        while i < MAX {
            if self.v[i].id == id {
                return true;
            }
            i += 1;
        }
        false
    }

    /// Release lease `id`: 0, or `E_NOT_FOUND` for an id that is not live (released already, or dropped with its owner).
    pub fn end(&mut self, id: i32) -> i32 {
        let mut i = 0;
        while i < MAX {
            if id != 0 && self.v[i].id == id {
                self.v[i] = FREE;
                return 0;
            }
            i += 1;
        }
        crate::E_NOT_FOUND
    }

    /// Drop every lease of `owner` (its module is going away).
    pub fn drop_owner(&mut self, owner: u8) {
        let mut i = 0;
        while i < MAX {
            if self.v[i].id != 0 && self.v[i].owner == owner {
                self.v[i] = FREE;
            }
            i += 1;
        }
    }

    /// Is any lease live?
    pub fn any(&self) -> bool {
        let mut i = 0;
        while i < MAX {
            if self.v[i].id != 0 {
                return true;
            }
            i += 1;
        }
        false
    }

    /// Does the data extent of a file (`first`, `sectors`) overlap a lease? An entry with no data never does.
    pub fn guards(&self, first: u32, sectors: u32) -> bool {
        if sectors == 0 {
            return false;
        }
        let end = first as u64 + sectors as u64;
        let mut i = 0;
        while i < MAX {
            let l = &self.v[i];
            if l.id != 0
                && l.sectors != 0
                && (first as u64) < l.first as u64 + l.sectors as u64
                && (l.first as u64) < end
            {
                return true;
            }
            i += 1;
        }
        false
    }

    /// Put the leased extents (not empty ones) into `out` from `at`, and return the new length; extents that do not fit are not added (`out`
    /// has room for `MAX`).
    pub fn extents(&self, out: &mut [(u32, u32)], at: usize) -> usize {
        let mut n = at;
        let mut i = 0;
        while i < MAX {
            let l = &self.v[i];
            if l.id != 0 && l.sectors != 0 {
                if let Some(slot) = out.get_mut(n) {
                    *slot = (l.first, l.sectors);
                    n += 1;
                }
            }
            i += 1;
        }
        n
    }
}
