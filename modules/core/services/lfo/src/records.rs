//! `lfo` service, record handlers (interface B): the `lfo_shape` records of the `register` section. A record is `u32 shape id` (not 0) and 32 samples
//! of `i16`, big-endian, after the 8-byte record header (`[records.kinds]` of sdk/sdk.toml: 76 bytes). The table keeps the shape with the module
//! record that owns it (module record index + 1); one owner per shape id.
use core::ptr::{addr_of, addr_of_mut};
use dkrt_abi::core_api::CoreApi;
use dkrt_abi::mem::rd32;
use dkrt_abi::registry::each;
use dkrt_abi::service::{check_step, ApplyCtx, CheckCtx, Declare, Service};
use dkrt_sdk::records::{KIND_LFO_SHAPE, LEN_LFO_SHAPE};
use dkrt_sdk::*;

/// Samples of one shape.
pub const SAMPLES: usize = 32;
/// Shapes the table keeps.
pub const MAX_SHAPES: usize = 8;

#[derive(Clone, Copy)]
struct Shape {
    /// module record index + 1; 0: free
    owner: u8,
    id: u32,
    samples: [i16; SAMPLES],
}

const FREE: Shape = Shape {
    owner: 0,
    id: 0,
    samples: [0; SAMPLES],
};
static mut SHAPES: [Shape; MAX_SHAPES] = [FREE; MAX_SHAPES];

fn shapes() -> &'static mut [Shape; MAX_SHAPES] {
    unsafe { &mut *addr_of_mut!(SHAPES) }
}

/// The samples of shape `id`, if a module keeps it.
pub fn shape(id: u32) -> Option<[i16; SAMPLES]> {
    let t = unsafe { &*addr_of!(SHAPES) };
    let mut i = 0;
    while i < MAX_SHAPES {
        if t[i].owner != 0 && t[i].id == id {
            return Some(t[i].samples);
        }
        i += 1;
    }
    None
}

/// The owner (module record index + 1) of shape `id`; 0 when none keeps it.
pub fn owner_of(id: u32) -> u8 {
    let t = unsafe { &*addr_of!(SHAPES) };
    let mut i = 0;
    while i < MAX_SHAPES {
        if t[i].owner != 0 && t[i].id == id {
            return t[i].owner;
        }
        i += 1;
    }
    0
}

/// Shapes in the table.
pub fn used() -> usize {
    let t = unsafe { &*addr_of!(SHAPES) };
    let mut n = 0;
    let mut i = 0;
    while i < MAX_SHAPES {
        if t[i].owner != 0 {
            n += 1;
        }
        i += 1;
    }
    n
}

/// The lfo service.
pub struct Lfo;

impl<C: CoreApi> Service<C> for Lfo {
    unsafe fn declare(d: &mut Declare) {
        d.kind(KIND_LFO_SHAPE, LEN_LFO_SHAPE);
    }

    unsafe fn check(_core: &C, step: u32, c: &CheckCtx) -> i32 {
        if step == check_step::RECORDS {
            // each shape has an id, and an id is once in the file
            let mut r = OK;
            let mut seen = [0u32; MAX_SHAPES];
            let mut n = 0;
            each(c.reg, |kind, b| {
                if kind == KIND_LFO_SHAPE && r == OK {
                    let id = rd32(b);
                    let mut k = 0;
                    while k < n {
                        if seen[k] == id {
                            r = E_TYPE_USED;
                        }
                        k += 1;
                    }
                    if id == 0 {
                        r = E_FORMAT;
                    } else if n < MAX_SHAPES {
                        seen[n] = id;
                        n += 1;
                    }
                }
            });
            r
        } else if step == check_step::ROWS {
            // the table has room for the file's shapes, and no id is kept by a module already
            let mut r = OK;
            let mut add = 0;
            each(c.reg, |kind, b| {
                if kind == KIND_LFO_SHAPE && r == OK {
                    if owner_of(rd32(b)) != 0 {
                        r = E_TYPE_USED;
                    }
                    add += 1;
                }
            });
            if r == OK && used() + add > MAX_SHAPES {
                r = E_TABLE_FULL;
            }
            r
        } else {
            OK
        }
    }

    unsafe fn apply_record(_core: &C, kind: u32, body: *const u8, c: &ApplyCtx) -> i32 {
        if kind != KIND_LFO_SHAPE {
            return OK;
        }
        let t = shapes();
        let mut i = 0;
        while i < MAX_SHAPES {
            if t[i].owner == 0 {
                t[i].owner = c.owner;
                t[i].id = rd32(body);
                let mut s = 0;
                while s < SAMPLES {
                    t[i].samples[s] =
                        i16::from_be_bytes([*body.add(4 + 2 * s), *body.add(5 + 2 * s)]);
                    s += 1;
                }
                return OK;
            }
            i += 1;
        }
        E_TABLE_FULL
    }

    unsafe fn remove(_core: &C, owner: u8) {
        let t = shapes();
        let mut i = 0;
        while i < MAX_SHAPES {
            if t[i].owner == owner {
                t[i] = FREE;
            }
            i += 1;
        }
    }
}
