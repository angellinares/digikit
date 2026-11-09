//! Sticky machine type numbers: the assignment loop of the runtime's `assign::plan`, free of the card, the firmware and the debug log, so it
//! runs on the host. The rules and the map format are described in `src/rs/src/assign.rs`.
//!
//! A machine key is `hash24 << 8` (low byte 0); a map entry is `hash24 << 8 | type`, 0 = empty. A key with hash 0 is a pinned number and is not
//! assigned here.
//!
//! ColdFire rules (as in `store.rs`): no `match` on integers, no `core::fmt`.
use crate::device::{DYN_TYPE_FIRST, DYN_TYPE_LAST};
use crate::logk;
use crate::sel::MAP_N;
use crate::SEL_MAX;

/// Machines a boot can name: 8 selected modules with 8 machines each.
pub const MAX_KEYS: usize = SEL_MAX * 8;
const NONE: u32 = 0xffff_ffff;

/// A log event of the assignment; the runtime writes it to its debug log (`error`: an error event, else info level).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Event {
    /// `logk::MACHINE_TYPE_ASSIGNED` or `logk::MACHINE_TYPE_EVICTED`.
    pub kind: u16,
    pub error: bool,
    pub args: [u32; 5],
}

pub fn in_range(t: u32) -> bool {
    t >= DYN_TYPE_FIRST && t <= DYN_TYPE_LAST
}

fn bit(t: u32) -> u32 {
    1 << (t - DYN_TYPE_FIRST)
}

/// Type numbers referenced by the map, leaving out the entry of hash `except` (0: none left out).
pub fn referenced_but(map: &[u32; MAP_N], except: u32) -> u32 {
    let mut m = 0;
    let mut i = 0;
    while i < MAP_N {
        if map[i] != 0 && map[i] >> 8 != except {
            m |= bit(map[i] & 0xff);
        }
        i += 1;
    }
    m
}

/// Type numbers referenced by the map.
pub fn referenced(map: &[u32; MAP_N]) -> u32 {
    referenced_but(map, 0)
}

/// The lowest number of the run-time range whose bit is not in `taken` (0: none).
pub fn lowest_free(taken: u32) -> u32 {
    let mut t = DYN_TYPE_FIRST;
    while t <= DYN_TYPE_LAST {
        if taken & bit(t) == 0 {
            return t;
        }
        t += 1;
    }
    0
}

/// The map as read from the record without entries that cannot be right: a type outside the range, no hash, a number or a hash that an earlier
/// entry has.
pub fn clean(map: &[u32; MAP_N]) -> [u32; MAP_N] {
    let mut out = [0u32; MAP_N];
    let mut seen = 0u32;
    let mut i = 0;
    while i < MAP_N {
        let (h, t) = (map[i] >> 8, map[i] & 0xff);
        let mut dup_hash = false;
        let mut j = 0;
        while j < i {
            dup_hash |= out[j] != 0 && out[j] >> 8 == h;
            j += 1;
        }
        if h != 0 && in_range(t) && seen & bit(t) == 0 && !dup_hash {
            out[i] = map[i];
            seen |= bit(t);
        }
        i += 1;
    }
    out
}

pub fn find_hash(map: &[u32; MAP_N], h: u32) -> Option<usize> {
    let mut i = 0;
    while i < MAP_N {
        if map[i] != 0 && map[i] >> 8 == h {
            return Some(i);
        }
        i += 1;
    }
    None
}

/// The first empty slot, `MAP_N` when the map is full.
pub fn first_empty(map: &[u32; MAP_N]) -> usize {
    let mut i = 0;
    while i < MAP_N {
        if map[i] == 0 {
            return i;
        }
        i += 1;
    }
    MAP_N
}

/// Is `h` the hash of one of the first `n` machine keys of this boot?
pub fn wanted(key: &[u32], n: usize, h: u32) -> bool {
    let mut i = 0;
    while i < n {
        if key[i] >> 8 == h {
            return true;
        }
        i += 1;
    }
    false
}

/// Assign the numbers of the first `n` machine `key`s (load order) into `ty` (0: none was free or the key is a pinned number). `rec_map` is the
/// map the boot started from; `map` is the cleaned map on entry and the map to keep on return. `log` gets every assignment and eviction in
/// order. Returns `dirty`: 1 when `map` differs from `rec_map`.
pub fn assign<F: FnMut(&Event)>(
    rec_map: &[u32; MAP_N],
    map: &mut [u32; MAP_N],
    key: &[u32],
    ty: &mut [u32],
    n: usize,
    log: &mut F,
) -> u32 {
    let mut used = 0u32;
    let mut e = 0;
    while e < n {
        ty[e] = 0;
        e += 1;
    }
    e = 0;
    while e < n {
        let k = key[e];
        let h = k >> 8;
        if h == 0 {
            e += 1;
            continue; // a pinned number (key = type) or nothing to assign
        }
        let mut first = true;
        let mut j = 0;
        while j < e {
            first &= key[j] != k;
            j += 1;
        }
        let mut t = 0;
        let mut sticky = 0;
        let mut rule = 0; // 0 lowest free number, 1 the map
        if first {
            let at = find_hash(map, h);
            if let Some(i) = at {
                let old = map[i] & 0xff;
                if used & bit(old) == 0 {
                    t = old;
                    sticky = 1;
                    rule = 1;
                }
            }
            if t == 0 {
                t = lowest_free(used | referenced(map));
                if t == 0 {
                    // no number is unreferenced: drop the lowest entry of a machine that is not in this boot
                    let mut victim = NONE;
                    let mut i = 0;
                    while i < MAP_N && victim == NONE {
                        if map[i] != 0 && !wanted(key, n, map[i] >> 8) {
                            victim = i as u32;
                        }
                        i += 1;
                    }
                    if victim != NONE {
                        let v = map[victim as usize];
                        log(&Event {
                            kind: logk::MACHINE_TYPE_EVICTED,
                            error: true,
                            args: [v >> 8, v & 0xff, 0, 0, 0],
                        });
                        map[victim as usize] = 0;
                        t = v & 0xff;
                    }
                }
                if t != 0 {
                    // record the entry: the slot the hash had, else the first empty one
                    let slot = match at {
                        Some(i) => i,
                        None => first_empty(map),
                    };
                    if slot < MAP_N {
                        map[slot] = h << 8 | t;
                    }
                }
            }
        } else {
            t = lowest_free(used | referenced(map));
            if t == 0 {
                t = lowest_free(used);
            }
        }
        if t != 0 {
            used |= bit(t);
        }
        ty[e] = t;
        log(&Event {
            kind: logk::MACHINE_TYPE_ASSIGNED,
            error: false,
            args: [h, t, sticky, rule, 0],
        });
        e += 1;
    }
    let mut dirty = 0;
    let mut i = 0;
    while i < MAP_N {
        dirty |= (map[i] != rec_map[i]) as u32;
        i += 1;
    }
    dirty
}

#[cfg(test)]
mod tests;
