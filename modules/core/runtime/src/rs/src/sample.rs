//! Session-pinned module sample banks. Only path submission runs outside the UI task.
use crate::{arena, dsp, irq_mask, irq_restore, sample_source};
use core::ptr::addr_of_mut;
use dkrt_sdk::{
    sample_job::{Job, Link},
    *,
};
const PATH: usize = 256;
#[derive(Clone, Copy)]
struct Bank {
    id: u32,
    destination: u32,
    capacity: u32,
    descriptor: u32,
    status: i32,
    live: bool,
}
const EMPTY: Bank = Bank {
    id: 0,
    destination: 0,
    capacity: 0,
    descriptor: 0,
    status: 0,
    live: false,
};
static mut BANKS: [Bank; crate::MAX_MODULES] = [EMPTY; crate::MAX_MODULES];
static mut JOB: Job = Job::new();
static mut OWNER: usize = usize::MAX;
static mut PATH_BUF: [u8; PATH] = [0; PATH];

pub unsafe fn reserve(rec: usize, id: u32, destination: u32, capacity: u32, descriptor: u32) {
    BANKS[rec] = Bank {
        id,
        destination,
        capacity,
        descriptor,
        status: 0,
        live: false,
    };
}
pub unsafe fn ready(rec: usize) {
    BANKS[rec].live = true;
}
pub unsafe fn remove(rec: usize) {
    let sr = irq_mask();
    if OWNER == rec {
        (&mut *addr_of_mut!(JOB)).cancel(E_NOT_FOUND);
    }
    BANKS[rec] = EMPTY;
    irq_restore(sr);
}
pub unsafe fn has_bank(rec: usize) -> bool {
    BANKS[rec].capacity != 0
}
pub unsafe fn bank(id: u32) -> i32 {
    let owner = crate::dlog::current_source();
    if owner == 0 {
        return E_NOT_FOUND;
    }
    let rec = owner as usize - 1;
    if crate::tracks::has(rec) {
        return dkrt_sdk::E_CONTRACT;
    }
    if BANKS[rec].capacity == 0 || BANKS[rec].id != id {
        return E_NOT_FOUND;
    }
    rec as i32 + 1
}
pub unsafe fn status(handle: u32) -> i32 {
    let sr = irq_mask();
    let r = if handle == 0
        || handle as usize > crate::MAX_MODULES
        || BANKS[handle as usize - 1].capacity == 0
    {
        E_NOT_FOUND
    } else {
        BANKS[handle as usize - 1].status
    };
    irq_restore(sr);
    r
}
pub unsafe fn load(handle: u32, path: *const u8) -> i32 {
    if path.is_null() {
        return E_SEL_PATH;
    }
    let mut copy = [0u8; PATH];
    let mut n = 0;
    while n < PATH {
        let v = *path.add(n);
        copy[n] = v;
        if v == 0 {
            break;
        }
        n += 1;
    }
    if n == 0 || n == PATH {
        return E_SEL_PATH;
    }
    let sr = irq_mask();
    let r = if handle == 0
        || handle as usize > crate::MAX_MODULES
        || BANKS[handle as usize - 1].capacity == 0
    {
        E_NOT_FOUND
    } else if OWNER != usize::MAX {
        E_BUSY
    } else if BANKS[handle as usize - 1].status != 0 {
        E_LOADED
    } else {
        OWNER = handle as usize - 1;
        PATH_BUF = copy;
        BANKS[OWNER].status = 1;
        OK
    };
    irq_restore(sr);
    r
}
pub unsafe fn busy() -> bool {
    OWNER != usize::MAX
}
struct Transport;
impl Link for Transport {
    fn send(&mut self, d: u32, s: *const u8, n: u32) -> i32 {
        unsafe { dsp::sample_send(d, s, n) }
    }
    fn poll(&mut self) -> Option<bool> {
        unsafe { dsp::sample_poll() }
    }
}
pub unsafe fn tick() {
    if OWNER == usize::MAX
        || !dsp::sample_idle()
        || crate::fxpage::busy()
        || crate::fxsend::busy()
        || crate::tracks::busy()
    {
        return;
    }
    let rec = OWNER;
    let j = &mut *addr_of_mut!(JOB);
    if BANKS[rec].status == 1 {
        if !BANKS[rec].live {
            return;
        }
        // Claim in a short critical section; file acquisition itself stays in UI context.
        let sr = irq_mask();
        BANKS[rec].status = 2;
        irq_restore(sr);
        match sample_source::load(addr_of_mut!(PATH_BUF) as *const u8) {
            Err(e) => {
                BANKS[rec].status = e;
                OWNER = usize::MAX;
                return;
            }
            Ok(source) => {
                if source.frames == 0 || source.frames > BANKS[rec].capacity {
                    arena::free(source.ptr);
                    BANKS[rec].status = E_TOO_BIG;
                    OWNER = usize::MAX;
                    return;
                }
                let b = BANKS[rec];
                let word = 0x0800_0000 + (b.destination - 0x2000_0000) / 4;
                j.begin(source.ptr, source.frames, b.destination, b.descriptor, word);
            }
        }
    }
    if j.busy() {
        j.step(&mut Transport, 6000);
        BANKS[rec].status = j.status;
    }
    if !j.busy() {
        let p = j.take_source();
        if !p.is_null() {
            arena::free(p);
        }
        OWNER = usize::MAX;
    }
}
