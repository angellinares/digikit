//! UI-task read-only listing service. Queue state is serialized only for short
//! copies. The listing state of a request is a block from the arena (a `Listing`
//! header, then the provider's state of `Storage::list_state_bytes`), taken when
//! the listing starts and freed when it ends, is cancelled or its module retires;
//! at most `MAX_LISTINGS` exist at once across all modules. No lock/model borrow
//! spans firmware calls or module callbacks. DT2 calls can block; one operation is
//! not a latency bound. The storage routes are in storage_adapter.rs.
//!
//! A consumer that does not poll (and is not notified) for `EXPIRE_BLOCKS` firmware blocks (10 s of emulated time at 1500 blocks a second) has abandoned
//! its listing: the next tick frees the block and cancels the request, and the client's next `poll` of that ticket gives `E_LOST` (the closest existing
//! code; there is no timeout code). A request of the same client replaces the expired state. Without a block counter nothing expires.
use core::ptr::{addr_of, addr_of_mut, null_mut};
use dkrt_abi::platform::Platform;
use dkrt_abi::storage::{DirEntry, EntryKind, ListStep, Storage};
use dkrt_sdk::{
    api::FilesEventFn,
    files::*,
    files_job::{Broker, EventState, Handle, Outcome, Poll, Ticket},
    *,
};

#[derive(Clone, Copy)]
struct Request {
    root: u32,
    path: [u8; PATH_BYTES],
    notify: Option<FilesEventFn>,
    context: *mut u8,
}
#[derive(Clone, Copy)]
struct ClientInfo {
    handle: u32,
    ticket: u32,
    notify: Option<FilesEventFn>,
    context: *mut u8,
    /// The listing of `ticket` was abandoned and freed: `poll` of it gives `E_LOST` until a new request or a cancel.
    expired: bool,
}
const INFO: ClientInfo = ClientInfo {
    handle: 0,
    ticket: 0,
    notify: None,
    context: core::ptr::null_mut(),
    expired: false,
};
static mut JOBS: Broker<{ crate::MAX_MODULES }, Request, FileEntry> = Broker::new();
static mut INFO_ROWS: [ClientInfo; crate::MAX_MODULES] = [INFO; crate::MAX_MODULES];
// 0 retired/uninitialized, 1 initializing, 2 ready. Only init scope can attach.
static mut READY: [u8; crate::MAX_MODULES] = [0; crate::MAX_MODULES];
static mut INIT_SCOPE: usize = usize::MAX;
static mut TICKING: bool = false;
static mut SCHED_ERROR: i32 = 0;

unsafe fn locked<T>(
    f: impl FnOnce(&mut Broker<{ crate::MAX_MODULES }, Request, FileEntry>) -> T,
) -> T {
    let sr = crate::irq_mask();
    let result = f(&mut *addr_of_mut!(JOBS));
    crate::irq_restore(sr);
    result
}
pub unsafe fn init_begin(owner: usize) {
    locked(|_| {
        READY[owner] = 1;
        INIT_SCOPE = owner;
        INFO_ROWS[owner] = INFO;
    });
}
pub unsafe fn ready(owner: usize) {
    locked(|_| {
        READY[owner] = 2;
        if INIT_SCOPE == owner {
            INIT_SCOPE = usize::MAX;
        }
    });
}
/// Loader calls before every rollback/free and teardown, including failed init.
/// False means keep module code/context allocated until its leases return.
pub unsafe fn retire(owner: usize) -> bool {
    locked(|jobs| {
        READY[owner] = 0;
        INFO_ROWS[owner] = INFO;
        if INIT_SCOPE == owner {
            INIT_SCOPE = usize::MAX;
        }
        let quiet = jobs.retire(owner).unwrap_or(false);
        // A step of this owner may be running (`busy`): its listing is freed when that step ends, else now.
        let mut i = 0;
        while i < MAX_LISTINGS {
            let l = LISTINGS[i];
            if !l.is_null() && (*l).owner == owner as u32 {
                kill(i);
            }
            i += 1;
        }
        quiet
    })
}
/// Is module record `owner` inside its `init` now? The check that `client` and the settings client share.
pub unsafe fn in_init(owner: usize) -> bool {
    owner < crate::MAX_MODULES && INIT_SCOPE == owner && READY[owner] == 1
}
pub unsafe fn client() -> i32 {
    locked(|jobs| {
        let source = crate::dlog::current_source();
        if source == 0 {
            return E_NOT_FOUND;
        }
        let owner = source as usize - 1;
        if owner >= crate::MAX_MODULES || INIT_SCOPE != owner || READY[owner] != 1 {
            return E_NOT_FOUND;
        }
        match jobs.attach(owner) {
            Ok(handle) => {
                INFO_ROWS[owner].handle = handle.id();
                handle.id() as i32
            }
            Err(error) => error,
        }
    })
}
/// Is a path of `n` bytes too long for `root`? The store's limit is the store path (159 bytes); the samples root's is the device's own
/// (`DeviceProfile::sample_path_max`, from the device file). The empty path is the root itself.
pub(crate) fn path_too_big<P: dkrt_abi::platform::Platform>(p: &P, root: u32, n: usize) -> bool {
    (root == STORE && n > 159) || (root == SAMPLES && n != 0 && n > p.sample_path_max() as usize)
}

pub unsafe fn list(
    client: u32,
    root: u32,
    path: *const u8,
    notify: Option<FilesEventFn>,
    context: *mut u8,
) -> i32 {
    list_with(&crate::PLATFORM, client, root, path, notify, context)
}

/// Most listings at once, across all modules.
const MAX_LISTINGS: usize = 2;
/// Firmware blocks without a poll or notification after which a listing is abandoned (10 s at 1500 blocks a second).
const EXPIRE_BLOCKS: u32 = 15000;

/// The state of one listing, in front of the provider's state in one arena block. The header is 16 bytes, so the provider state that follows is
/// 16-byte aligned. `busy` is set while a tick step uses the block (under `locked`); a block is freed only when it is not busy, else the step frees it.
/// `seen` is the block count at the start or at the last poll or notification of the consumer (0 without a counter).
#[repr(C, align(16))]
struct Listing {
    owner: u32,
    ticket: u32,
    seen: u32,
    opened: bool,
    dead: bool,
    busy: bool,
}
static mut LISTINGS: [*mut Listing; MAX_LISTINGS] = [null_mut(); MAX_LISTINGS];

#[cfg(not(test))]
unsafe fn take(bytes: usize) -> *mut u8 {
    crate::arena::alloc(bytes as u32)
}
#[cfg(not(test))]
unsafe fn give(p: *mut u8) {
    crate::arena::free(p);
}
// The arena runs on 32-bit addresses; host tests take the blocks from the host heap and count them.
#[cfg(test)]
static mut TEST_LIVE: usize = 0;
#[cfg(test)]
static mut TEST_NOMEM: bool = false;
/// Most blocks the test arena holds at once.
#[cfg(test)]
static mut TEST_CAP: usize = usize::MAX;
#[cfg(test)]
unsafe fn take(bytes: usize) -> *mut u8 {
    if TEST_NOMEM || TEST_LIVE >= TEST_CAP {
        return null_mut();
    }
    let base = std::alloc::alloc_zeroed(std::alloc::Layout::from_size_align(bytes + 16, 16).unwrap());
    *(base as *mut usize) = bytes + 16;
    TEST_LIVE += 1;
    base.add(16)
}
#[cfg(test)]
unsafe fn give(p: *mut u8) {
    let base = p.sub(16);
    std::alloc::dealloc(base, std::alloc::Layout::from_size_align(*(base as *mut usize), 16).unwrap());
    TEST_LIVE -= 1;
}

/// Free the block of slot `i`. The caller holds `locked` and knows no step uses it.
unsafe fn release(i: usize) {
    let l = LISTINGS[i];
    LISTINGS[i] = null_mut();
    if !l.is_null() {
        give(l as *mut u8);
    }
}
/// The listing of slot `i` is no longer wanted: free it now, or, when a tick step uses it, mark it so that the step frees it. Under `locked`.
unsafe fn kill(i: usize) {
    (*LISTINGS[i]).dead = true;
    if !(*LISTINGS[i]).busy {
        release(i);
    }
}

pub unsafe fn list_with<P: Platform + Storage>(
    p: &P,
    client: u32,
    root: u32,
    path: *const u8,
    notify: Option<FilesEventFn>,
    context: *mut u8,
) -> i32 {
    if path.is_null() {
        return E_SEL_PATH;
    }
    if root != SAMPLES && root != STORE {
        return E_FORMAT;
    }
    let mut request = Request {
        root,
        path: [0; PATH_BYTES],
        notify,
        context,
    };
    let mut i = 0;
    while i < PATH_BYTES {
        request.path[i] = *path.add(i);
        if request.path[i] == 0 {
            break;
        }
        i += 1;
    }
    let n = match path_len(&request.path) {
        Some(n) => n,
        None => return E_SEL_PATH,
    };
    if path_too_big(p, root, n) {
        return E_TOO_BIG;
    }
    locked(|jobs| {
        if SCHED_ERROR != 0 {
            return SCHED_ERROR;
        }
        let owner = match INFO_ROWS
            .iter()
            .position(|row| row.handle == client && client != 0)
        {
            Some(owner) => owner,
            None => return E_NOT_FOUND,
        };
        // A new request replaces the one of this client: its old listing is released first (when a step uses it, it is marked and the step frees it).
        // Then a slot and memory must be free, else E_BUSY or E_NOMEM (the old request then ends with an error).
        let old = LISTINGS
            .iter()
            .position(|&l| !l.is_null() && (*l).owner == owner as u32 && !(*l).dead);
        if let Some(i) = old {
            kill(i);
        }
        let slot = match LISTINGS.iter().position(|l| l.is_null()) {
            Some(i) => i,
            None => return E_BUSY,
        };
        let block = take(core::mem::size_of::<Listing>() + p.list_state_bytes(root));
        if block.is_null() {
            return E_NOMEM;
        }
        match jobs.begin(Handle::from_id(client), request) {
            Ok(ticket) => {
                let l = block as *mut Listing;
                l.write(Listing {
                    owner: owner as u32,
                    ticket: ticket.id(),
                    seen: p.blocks().unwrap_or(0),
                    opened: false,
                    dead: false,
                    busy: false,
                });
                LISTINGS[slot] = l;
                INFO_ROWS[owner] = ClientInfo {
                    handle: client,
                    ticket: ticket.id(),
                    notify: request.notify,
                    context: request.context,
                    expired: false,
                };
                ticket.id() as i32
            }
            Err(error) => {
                give(block);
                error
            }
        }
    })
}
pub unsafe fn poll(client: u32, ticket: u32, out: *mut u8, capacity: u32) -> i32 {
    poll_with(&crate::PLATFORM, client, ticket, out, capacity)
}
pub(crate) unsafe fn poll_with<P: Platform>(p: &P, client: u32, ticket: u32, out: *mut u8, capacity: u32) -> i32 {
    // Validate before consuming even if no row is currently available.
    if out.is_null()
        || capacity < core::mem::size_of::<FileEntry>() as u32
        || (out as usize).checked_add(304).is_none()
    {
        return E_FORMAT;
    }
    let result = locked(|jobs| {
        // An abandoned listing (freed by `tick`) answers its ticket with `E_LOST`.
        if INFO_ROWS.iter().any(|row| {
            row.expired && row.handle == client && client != 0 && row.ticket == ticket && ticket != 0
        }) {
            return Err(E_LOST);
        }
        if SCHED_ERROR != 0 {
            return Err(
                if INFO_ROWS.iter().any(|row| {
                    row.handle == client && client != 0 && row.ticket == ticket && ticket != 0
                }) {
                    SCHED_ERROR
                } else {
                    E_NOT_FOUND
                },
            );
        }
        let polled = jobs.poll(Handle::from_id(client), Ticket::from_id(ticket));
        if polled.is_ok() {
            touch(ticket, p.blocks());
        }
        polled
    });
    match result {
        Ok(Poll::Pending) => 0,
        Ok(Poll::Row(row)) => {
            core::ptr::copy_nonoverlapping(addr_of!(row) as *const u8, out, 304);
            1
        }
        Ok(Poll::Done) => 2,
        Ok(Poll::Error(error)) | Err(error) => error,
    }
}
/// The consumer of the listing of `ticket` polled or was notified at block count `now`. Under `locked`.
unsafe fn touch(ticket: u32, now: Option<u32>) {
    let mut i = 0;
    while i < MAX_LISTINGS {
        let l = LISTINGS[i];
        if !l.is_null() && (*l).ticket == ticket && !(*l).dead {
            (*l).seen = now.unwrap_or(0);
        }
        i += 1;
    }
}
pub unsafe fn cancel(client: u32, ticket: u32) -> i32 {
    locked(|jobs| {
        // The request of an abandoned listing is already cancelled: this clears the expired state.
        if let Some(row) = INFO_ROWS.iter_mut().find(|row| {
            row.expired && row.handle == client && client != 0 && row.ticket == ticket && ticket != 0
        }) {
            *row = ClientInfo { handle: client, ..INFO };
            return OK;
        }
        match jobs.cancel(Handle::from_id(client), Ticket::from_id(ticket)) {
            Ok(()) => {
                let mut i = 0;
                while i < MAX_LISTINGS {
                    let l = LISTINGS[i];
                    if !l.is_null() && (*l).ticket == ticket && !(*l).dead {
                        kill(i);
                    }
                    i += 1;
                }
                for row in &mut INFO_ROWS {
                    if row.handle == client && row.ticket == ticket {
                        row.ticket = 0;
                        row.notify = None;
                        row.context = core::ptr::null_mut();
                    }
                }
                OK
            }
            Err(error) => error,
        }
    })
}

/// One step of the listing `l` of request `request`: open it on the first step, then one `list_next`. `Pending` (also the store lock taken, `E_BUSY`)
/// is asked again on a later step. An entry becomes the row: the request path, `/` and the entry's name. Only a tick step calls this, with `l` busy.
unsafe fn step<P: Storage>(
    p: &P,
    l: *mut Listing,
    request: &Request,
) -> Result<Outcome<FileEntry>, i32> {
    let parent = path_len(&request.path).ok_or(E_SEL_PATH)?;
    let state = (l as *mut u8).add(core::mem::size_of::<Listing>());
    if !(*l).opened {
        p.list_open(request.root, &request.path[..parent], state)?;
        (*l).opened = true;
    }
    let mut e = DirEntry::empty();
    match p.list_next(state, &mut e) {
        Ok(ListStep::End) => return Ok(Outcome::Done),
        Ok(ListStep::Pending) => return Ok(Outcome::Pending),
        Err(error) if error == E_BUSY => return Ok(Outcome::Pending),
        Err(error) => return Err(error),
        Ok(ListStep::Entry) => {}
    }
    let name = e.name();
    let at = parent + if parent != 0 { 1 } else { 0 };
    if at + name.len() >= PATH_BYTES {
        return Err(E_TOO_BIG);
    }
    let mut row = FileEntry::empty();
    row.path[..parent].copy_from_slice(&request.path[..parent]);
    if parent != 0 {
        row.path[parent] = b'/';
    }
    row.path[at..at + name.len()].copy_from_slice(name);
    if path_len(&row.path) != Some(at + name.len()) || name.is_empty() || name.contains(&b'/') {
        return Err(E_BAD_STORE);
    }
    if e.stat.kind == EntryKind::Dir {
        row.kind = DIRECTORY;
    } else {
        row.kind = FILE;
        if e.stat.hash_valid {
            row.flags = SIZE_VALID | HASH_VALID;
            row.size_bytes = e.stat.size;
            row.hash_kind = e.stat.hash_kind as u32;
            row.hash = e.stat.hash;
        } else if e.stat.size != 0 {
            row.flags = SIZE_VALID;
            row.size_bytes = e.stat.size;
        }
    }
    Ok(Outcome::Row(row))
}

// Steps per pass: at most MAX_STEPS provider steps, within BUDGET_BLOCKS firmware blocks (the counter runs at 1500/s, so about 5 ms).
const MAX_STEPS: u32 = 32;
const BUDGET_BLOCKS: u32 = 8;
// Step cap when the firmware's counter is not there.
const FALLBACK_STEPS: u32 = 8;

/// Several provider steps, each with its deferred callback, per UI pass within a time budget, independent of slot0.
pub unsafe fn tick() {
    tick_with(&crate::PLATFORM)
}

pub(crate) unsafe fn tick_with<P: Platform + Storage>(p: &P) {
    let entered = locked(|_| {
        if TICKING {
            false
        } else {
            TICKING = true;
            true
        }
    });
    if !entered {
        return;
    }
    // Listings that were marked while a step used them and are idle now (no step runs between here and the loop).
    // Listings whose consumer did not poll for `EXPIRE_BLOCKS` are abandoned: the request is cancelled, the block freed, and the client's `poll` answers `E_LOST`.
    let now = p.blocks();
    locked(|jobs| {
        let mut i = 0;
        while i < MAX_LISTINGS {
            let l = LISTINGS[i];
            if !l.is_null() && (*l).dead && !(*l).busy {
                release(i);
            } else if !l.is_null() && !(*l).dead {
                if now.map_or(false, |now| now.wrapping_sub((*l).seen) >= EXPIRE_BLOCKS) {
                    let owner = (*l).owner as usize;
                    let ticket = (*l).ticket;
                    let _ = jobs.cancel(Handle::from_id(INFO_ROWS[owner].handle), Ticket::from_id(ticket));
                    INFO_ROWS[owner].expired = true;
                    INFO_ROWS[owner].notify = None;
                    INFO_ROWS[owner].context = null_mut();
                    kill(i);
                }
            }
            i += 1;
        }
    });
    let blocks = p.blocks();
    let start = blocks.unwrap_or(0);
    let mut steps = 0;
    loop {
        let work = locked(|jobs| match jobs.take_work() {
            Ok(Some(lease)) if READY[lease.owner] == 2 => Some(lease),
            Ok(Some(lease)) => {
                let _ = jobs.finish_work(lease, Outcome::Pending);
                None
            }
            Err(error) => {
                SCHED_ERROR = error;
                None
            }
            _ => None,
        });
        let worked = work.is_some();
        if let Some(lease) = work {
            // The listing of this ticket is busy for the step. A listing that is gone (retired or cancelled meanwhile) ends the request with an error.
            let slot = locked(|_| {
                let ticket = lease.ticket.id();
                let found = LISTINGS
                    .iter()
                    .position(|&l| !l.is_null() && (*l).ticket == ticket && !(*l).dead);
                if let Some(i) = found {
                    (*LISTINGS[i]).busy = true;
                }
                found
            });
            let outcome = match slot {
                Some(i) => match step(p, LISTINGS[i], &lease.request) {
                    Ok(value) => value,
                    Err(error) => Outcome::Error(error),
                },
                None => Outcome::Error(E_NOT_FOUND),
            };
            locked(|jobs| {
                let _ = jobs.finish_work(lease, outcome);
                if let Some(i) = slot {
                    (*LISTINGS[i]).busy = false;
                    // The state is not needed after the end or an error (the broker keeps that result).
                    if (*LISTINGS[i]).dead || matches!(outcome, Outcome::Done | Outcome::Error(_)) {
                        release(i);
                    }
                }
            });
            steps += 1;
        }
        // Capture callback AND context in the same serialization as lease validation.
        let event = locked(|jobs| {
            let lease = match jobs.take_event() {
                Ok(Some(value)) => value,
                Err(error) => {
                    SCHED_ERROR = error;
                    return None;
                }
                _ => return None,
            };
            let info = INFO_ROWS[lease.owner];
            if READY[lease.owner] != 2
                || info.handle != lease.handle.id()
                || info.ticket != lease.ticket.id()
            {
                let _ = jobs.finish_event(lease);
                return None;
            }
            touch(lease.ticket.id(), p.blocks());
            Some((lease, info.notify, info.context))
        });
        let called = event.is_some();
        if let Some((lease, notify, context)) = event {
            if let Some(call) = notify {
                let state = match lease.state {
                    EventState::Row => 1,
                    EventState::Done => 2,
                    EventState::Error(error) => error,
                };
                let prev = crate::dlog::enter_module(lease.owner, crate::dlog::PHASE_CALLBACK);
                call(lease.handle.id(), lease.ticket.id(), state, context);
                crate::dlog::leave_module(prev);
            }
            locked(|jobs| {
                let _ = jobs.finish_event(lease);
            });
        }
        if !worked && !called {
            break;
        }
        let spent = match p.blocks() {
            Some(now) => now.wrapping_sub(start) >= BUDGET_BLOCKS,
            None => steps >= FALLBACK_STEPS,
        };
        if spent || steps >= MAX_STEPS {
            break;
        }
    }
    locked(|_| {
        TICKING = false;
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fake_platform::FakePlatform;
    use dkrt_abi::profiles;
    use dkrt_abi::storage::{HASH_ELEKTRON32 as FAKE_K32, HASH_NONE as FAKE_NONE};
    use crate::test_lock::LOCK;
    use std::vec::Vec;

    const H: [u8; 32] = [7; 32];

    unsafe fn reset() {
        *addr_of_mut!(JOBS) = Broker::new();
        INFO_ROWS = [INFO; crate::MAX_MODULES];
        READY = [0; crate::MAX_MODULES];
        INIT_SCOPE = usize::MAX;
        TICKING = false;
        SCHED_ERROR = 0;
        LISTINGS = [null_mut(); MAX_LISTINGS];
        TEST_LIVE = 0;
        TEST_NOMEM = false;
        TEST_CAP = usize::MAX;
    }
    /// A module record with a listing client, as `client` gives it in `init`.
    unsafe fn attach(owner: usize) -> u32 {
        init_begin(owner);
        let h = locked(|jobs| jobs.attach(owner).unwrap());
        INFO_ROWS[owner].handle = h.id();
        ready(owner);
        h.id()
    }
    unsafe fn start(p: &FakePlatform, client: u32, root: u32, path: &[u8]) -> i32 {
        let mut z = [0u8; PATH_BYTES];
        z[..path.len()].copy_from_slice(path);
        list_with(p, client, root, z.as_ptr(), None, null_mut())
    }
    /// Tick and poll until the listing ends; the rows in order.
    unsafe fn rows(p: &FakePlatform, client: u32, ticket: i32) -> Vec<FileEntry> {
        let mut out = Vec::new();
        for _ in 0..1000 {
            tick_with(p);
            loop {
                let mut e = FileEntry::empty();
                match poll_with(p, client, ticket as u32, addr_of_mut!(e) as *mut u8, 304) {
                    1 => out.push(e),
                    2 => return out,
                    0 => break,
                    n => panic!("poll gave {n}"),
                }
            }
        }
        panic!("the listing did not end")
    }
    fn row(path: &[u8], kind: u32, flags: u32, size: u32, hash_kind: u32, hash: [u8; 32]) -> FileEntry {
        let mut r = FileEntry::empty();
        r.path[..path.len()].copy_from_slice(path);
        r.kind = kind;
        r.flags = flags;
        r.size_bytes = size;
        r.hash_kind = hash_kind;
        r.hash = hash;
        r
    }
    fn fixture(profile: dkrt_abi::platform::DeviceProfile) -> FakePlatform {
        let f = FakePlatform::new(profile);
        f.storage.add_dir(SAMPLES, b"KICKS");
        f.storage.add_file(SAMPLES, b"KICKS/K1", b"0123456789", FAKE_K32, H);
        f.storage.add_file(SAMPLES, b"KICKS/K2", &[9u8; 100], FAKE_NONE, [0; 32]);
        f.storage.add_dir(SAMPLES, b"KICKS/SUB");
        f.storage.add_dir(STORE, b"modules");
        f.storage.add_file(STORE, b"modules/a.dkm", b"abc", FAKE_K32, H);
        f
    }
    fn kicks() -> Vec<FileEntry> {
        std::vec![
            row(b"KICKS/K1", FILE, SIZE_VALID | HASH_VALID, 10, HASH_ELEKTRON32, H),
            row(b"KICKS/K2", FILE, SIZE_VALID, 100, HASH_NONE, [0; 32]),
            row(b"KICKS/SUB", DIRECTORY, 0, 0, HASH_NONE, [0; 32]),
        ]
    }
    fn modules() -> Vec<FileEntry> {
        std::vec![row(b"modules/a.dkm", FILE, SIZE_VALID | HASH_VALID, 3, HASH_ELEKTRON32, H)]
    }

    #[test]
    fn a_store_dir_and_a_samples_dir_list_the_content_of_the_provider() {
        let _g = LOCK.lock().unwrap_or_else(|e| e.into_inner());
        unsafe {
            for profile in [profiles::DN2, profiles::DT2] {
                reset();
                let f = fixture(profile);
                let h = attach(0);
                let t = start(&f, h, SAMPLES, b"KICKS");
                assert!(t > 0);
                assert_eq!(rows(&f, h, t), kicks(), "{profile:?}");
                let t = start(&f, h, STORE, b"modules");
                assert!(t > 0);
                assert_eq!(rows(&f, h, t), modules(), "{profile:?}");
                assert_eq!(TEST_LIVE, 0, "the end frees the block");
            }
        }
    }

    #[test]
    fn a_provider_that_pends_still_completes_with_the_same_rows() {
        let _g = LOCK.lock().unwrap_or_else(|e| e.into_inner());
        unsafe {
            reset();
            let f = fixture(profiles::DN2);
            f.storage.pending_every.set(1);
            let h = attach(0);
            let t = start(&f, h, SAMPLES, b"KICKS");
            assert_eq!(rows(&f, h, t), kicks());
            assert_eq!(TEST_LIVE, 0);
        }
    }

    #[test]
    fn two_listings_run_at_once_and_a_third_is_busy_until_one_ends() {
        let _g = LOCK.lock().unwrap_or_else(|e| e.into_inner());
        unsafe {
            reset();
            let f = fixture(profiles::DN2);
            let (h0, h1, h2) = (attach(0), attach(1), attach(2));
            let t0 = start(&f, h0, SAMPLES, b"KICKS");
            let t1 = start(&f, h1, STORE, b"modules");
            assert!(t0 > 0 && t1 > 0);
            assert_eq!(start(&f, h2, SAMPLES, b"KICKS"), E_BUSY);
            assert_eq!(TEST_LIVE, 2);
            assert_eq!(rows(&f, h0, t0), kicks());
            assert_eq!(TEST_LIVE, 1);
            let t2 = start(&f, h2, SAMPLES, b"KICKS");
            assert!(t2 > 0);
            assert_eq!(rows(&f, h1, t1), modules());
            assert_eq!(rows(&f, h2, t2), kicks());
            assert_eq!(TEST_LIVE, 0);
        }
    }

    #[test]
    fn cancel_and_a_new_request_free_the_block_and_no_memory_is_an_error() {
        let _g = LOCK.lock().unwrap_or_else(|e| e.into_inner());
        unsafe {
            reset();
            let f = fixture(profiles::DN2);
            let h = attach(0);
            let t = start(&f, h, SAMPLES, b"KICKS");
            assert_eq!(TEST_LIVE, 1);
            assert_eq!(cancel(h, t as u32), OK);
            assert_eq!(TEST_LIVE, 0);
            assert_eq!(cancel(h, t as u32), E_NOT_FOUND);
            // a second request of the client replaces the first
            let first = start(&f, h, SAMPLES, b"KICKS");
            let second = start(&f, h, STORE, b"modules");
            assert_eq!(TEST_LIVE, 1);
            let mut e = FileEntry::empty();
            assert_eq!(poll(h, first as u32, addr_of_mut!(e) as *mut u8, 304), E_NOT_FOUND);
            assert_eq!(rows(&f, h, second), modules());
            assert_eq!(TEST_LIVE, 0);
            // no block: E_NOMEM, and no request was made
            TEST_NOMEM = true;
            assert_eq!(start(&f, h, SAMPLES, b"KICKS"), E_NOMEM);
            TEST_NOMEM = false;
            assert_eq!(TEST_LIVE, 0);
        }
    }

    #[test]
    fn a_client_replaces_its_own_request_when_the_arena_holds_one_block() {
        let _g = LOCK.lock().unwrap_or_else(|e| e.into_inner());
        unsafe {
            reset();
            let f = fixture(profiles::DN2);
            let h = attach(0);
            TEST_CAP = 1;
            let first = start(&f, h, SAMPLES, b"KICKS");
            assert!(first > 0);
            let second = start(&f, h, STORE, b"modules");
            assert!(second > 0);
            assert_eq!(TEST_LIVE, 1);
            let mut e = FileEntry::empty();
            assert_eq!(poll(h, first as u32, addr_of_mut!(e) as *mut u8, 304), E_NOT_FOUND);
            assert_eq!(rows(&f, h, second), modules());
            assert_eq!(TEST_LIVE, 0);
        }
    }

    #[test]
    fn a_listing_nobody_polls_for_10_s_is_freed_and_its_poll_gives_e_lost() {
        let _g = LOCK.lock().unwrap_or_else(|e| e.into_inner());
        unsafe {
            reset();
            let f = fixture(profiles::DN2);
            let h = attach(0);
            f.clock.set(Some(1000));
            let t = start(&f, h, SAMPLES, b"KICKS");
            assert!(t > 0);
            tick_with(&f); // a row waits for the consumer
            // a poll restarts the 10 s
            f.clock.set(Some(1000 + EXPIRE_BLOCKS - 1000));
            let mut e = FileEntry::empty();
            assert_eq!(poll_with(&f, h, t as u32, addr_of_mut!(e) as *mut u8, 304), 1);
            tick_with(&f);
            f.clock.set(Some(1000 + EXPIRE_BLOCKS - 1000 + EXPIRE_BLOCKS - 1));
            tick_with(&f);
            assert_eq!(TEST_LIVE, 1, "not yet 10 s without a poll");
            f.clock.set(Some(1000 + EXPIRE_BLOCKS - 1000 + EXPIRE_BLOCKS));
            tick_with(&f);
            assert_eq!(TEST_LIVE, 0);
            assert_eq!(poll_with(&f, h, t as u32, addr_of_mut!(e) as *mut u8, 304), E_LOST);
            assert_eq!(poll_with(&f, h, t as u32, addr_of_mut!(e) as *mut u8, 304), E_LOST);
            assert_eq!(cancel(h, t as u32), OK);
            assert_eq!(poll_with(&f, h, t as u32, addr_of_mut!(e) as *mut u8, 304), E_NOT_FOUND);
            // a new request works again
            let t = start(&f, h, STORE, b"modules");
            assert!(t > 0);
            assert_eq!(rows(&f, h, t), modules());
        }
    }

    #[test]
    fn a_failed_open_ends_the_request_with_the_error_and_frees_the_block() {
        let _g = LOCK.lock().unwrap_or_else(|e| e.into_inner());
        unsafe {
            reset();
            let f = fixture(profiles::DN2);
            let h = attach(0);
            let t = start(&f, h, SAMPLES, b"NOPE");
            assert!(t > 0);
            tick_with(&f);
            let mut e = FileEntry::empty();
            assert_eq!(poll(h, t as u32, addr_of_mut!(e) as *mut u8, 304), E_NOT_FOUND);
            assert_eq!(TEST_LIVE, 0);
        }
    }

    #[test]
    fn retire_frees_the_listing_now_or_at_the_next_tick_when_a_step_uses_it() {
        let _g = LOCK.lock().unwrap_or_else(|e| e.into_inner());
        unsafe {
            reset();
            let f = fixture(profiles::DN2);
            let h = attach(0);
            let t = start(&f, h, SAMPLES, b"KICKS");
            assert!(t > 0);
            tick_with(&f);
            assert_eq!(TEST_LIVE, 1);
            retire(0);
            assert_eq!(TEST_LIVE, 0);
            // a step is running when the module retires: the block stays, marked, and the next tick frees it
            let h = attach(0);
            assert!(start(&f, h, SAMPLES, b"KICKS") > 0);
            let at = LISTINGS.iter().position(|l| !l.is_null()).unwrap();
            (*LISTINGS[at]).busy = true;
            retire(0);
            assert_eq!(TEST_LIVE, 1);
            assert!((*LISTINGS[at]).dead);
            (*LISTINGS[at]).busy = false;
            tick_with(&f);
            assert_eq!(TEST_LIVE, 0);
        }
    }
}
