//! The loader's limit decisions as pure functions: where a DSP range fits, how many table rows are free, which refusal code a count gets. The
//! runtime (`src/rs`) calls these with its own tables, so the host tests see the same decisions the device takes.
//!
//! ColdFire rules: no `match` on integers, no `core::fmt`.
use crate::{E_DSP_ROWS, E_LOADED, E_PAGE_FULL, E_PARAM_FULL, E_TABLE_FULL, E_TOO_BIG, E_TYPE_RANGE, OK};

/// First fit: the lowest offset in `0..total` whose byte address (`base + offset`) is `align`-aligned (a power of two) and whose `len` bytes touch
/// none of the ranges `offs[i] .. offs[i] + lens[i]` (a range of length 0 is unused). `None` when no offset fits.
pub fn first_fit(base: u32, total: u32, offs: &[u32], lens: &[u32], len: u32, align: u32) -> Option<u32> {
    let mut cand = 0u32;
    loop {
        let b = (base + cand + align - 1) & !(align - 1);
        cand = b - base;
        if len > total || cand > total - len {
            return None;
        }
        let mut moved = false;
        let mut i = 0;
        while i < offs.len() && i < lens.len() {
            let (o, l) = (offs[i], lens[i]);
            if l != 0 && cand < o + l && o < cand + len {
                cand = o + l;
                moved = true;
            }
            i += 1;
        }
        if !moved {
            return Some(cand);
        }
    }
}

/// Rows in `owner` that nobody owns (0) and that lie below `limit`, the rows the firmware image reserved.
pub fn free_rows(owner: &[u8], limit: u32) -> u32 {
    let mut free = 0;
    let mut k = 0;
    while k < owner.len() {
        if owner[k] == 0 && (k as u32) < limit {
            free += 1;
        }
        k += 1;
    }
    free
}

/// Rows in `owner` that somebody owns.
pub fn used_rows(owner: &[u8]) -> u32 {
    let mut used = 0;
    let mut k = 0;
    while k < owner.len() {
        if owner[k] != 0 {
            used += 1;
        }
        k += 1;
    }
    used
}

/// `E_DSP_ROWS` when a DSP section needs more voice or effect rows than are free, else `OK`.
pub fn dsp_rows(need: u32, free: u32) -> i32 {
    if need > free {
        E_DSP_ROWS
    } else {
        OK
    }
}

/// The registry's row limits: `E_PAGE_FULL` when the records need more page rows than `free_pages`, else `E_PARAM_FULL` when they need more
/// parameter rows than `free_params`, else `OK`. The page limit is checked first.
pub fn registry_rows(need_pages: u32, free_pages: u32, need_params: u32, free_params: u32) -> i32 {
    if free_pages < need_pages {
        return E_PAGE_FULL;
    }
    if free_params < need_params {
        return E_PARAM_FULL;
    }
    OK
}

/// A machine record's type must lie in the run-time range `first .. first + count` and its clone source must be a stock machine (`< stock_end`).
pub fn machine_type(t: u32, first: u32, count: u32, clone: u32, stock_end: u32) -> i32 {
    if t < first || t >= first + count || clone >= stock_end {
        E_TYPE_RANGE
    } else {
        OK
    }
}

/// A file of `size` bytes in a staging buffer of `stage` bytes: `E_TOO_BIG` when it is empty or does not fit.
pub fn stage_fit(size: u32, stage: u32) -> i32 {
    if size == 0 || size > stage {
        E_TOO_BIG
    } else {
        OK
    }
}

/// A file of `size` bytes read into a caller buffer of `cap` bytes: `E_TOO_BIG` when it does not fit.
pub fn read_fit(size: u32, cap: u32) -> i32 {
    if size > cap {
        E_TOO_BIG
    } else {
        OK
    }
}

/// The module record for a new load among `count` records: the first record that is not `used`. `E_LOADED` when a used record satisfies `same`
/// (checked only for used records, in order, before any free record that follows is considered); `E_TABLE_FULL` when every record is used.
pub fn pick_record<U: Fn(usize) -> bool, S: Fn(usize) -> bool>(count: usize, used: U, same: S) -> Result<usize, i32> {
    let mut rec = count;
    let mut i = 0;
    while i < count {
        if used(i) {
            if same(i) {
                return Err(E_LOADED);
            }
        } else if rec == count {
            rec = i;
        }
        i += 1;
    }
    if rec == count {
        return Err(E_TABLE_FULL);
    }
    Ok(rec)
}

#[cfg(test)]
mod tests;
