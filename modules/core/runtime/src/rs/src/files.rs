//! UI-task read-only listing service. Queue state is serialized only for short
//! copies. Backend buffers are runtime-owned and UI-only, separate from upload
//! staging and from queued requests. No lock/model borrow spans firmware calls
//! or module callbacks. DT2 calls can block; one operation is not a latency bound.
use core::ptr::{addr_of, addr_of_mut, read_volatile};
use dkrt_sdk::{
    api::FileEvent,
    files::*,
    files_job::{Broker, EventState, Handle, Outcome, Poll, Ticket},
    store::{
        catalog::{CatalogCursor, CatalogEntry, CatalogOpen},
        directory::DirectoryCollector,
    },
    *,
};

#[derive(Clone, Copy)]
struct Request {
    root: u32,
    path: [u8; PATH_BYTES],
    notify: Option<FileEvent>,
    context: *mut u8,
}
#[derive(Clone, Copy)]
struct ClientInfo {
    handle: u32,
    ticket: u32,
    notify: Option<FileEvent>,
    context: *mut u8,
}
const INFO: ClientInfo = ClientInfo {
    handle: 0,
    ticket: 0,
    notify: None,
    context: core::ptr::null_mut(),
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
        jobs.retire(owner).unwrap_or(false)
    })
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
pub unsafe fn list(
    client: u32,
    root: u32,
    path: *const u8,
    notify: Option<FileEvent>,
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
    if (root == STORE && n > 159)
        || (root == SAMPLES && crate::device() == 2 && n != 0 && n + 8 > 159)
        || (root == SAMPLES && crate::device() == 1 && n > 254)
    {
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
        match jobs.begin(Handle::from_id(client), request) {
            Ok(ticket) => {
                INFO_ROWS[owner] = ClientInfo {
                    handle: client,
                    ticket: ticket.id(),
                    notify: request.notify,
                    context: request.context,
                };
                ticket.id() as i32
            }
            Err(error) => error,
        }
    })
}
pub unsafe fn poll(client: u32, ticket: u32, out: *mut u8, capacity: u32) -> i32 {
    // Validate before consuming even if no row is currently available.
    if out.is_null()
        || capacity < core::mem::size_of::<FileEntry>() as u32
        || (out as usize).checked_add(304).is_none()
    {
        return E_FORMAT;
    }
    let result = locked(|jobs| {
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
        jobs.poll(Handle::from_id(client), Ticket::from_id(ticket))
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
pub unsafe fn cancel(client: u32, ticket: u32) -> i32 {
    locked(
        |jobs| match jobs.cancel(Handle::from_id(client), Ticket::from_id(ticket)) {
            Ok(()) => {
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
        },
    )
}

#[repr(C, align(16))]
struct Scratch([u8; 4096]);
#[repr(C, align(16))]
struct Fingerprint([u8; 512]);
static mut SCRATCH: Scratch = Scratch([0; 4096]);
static mut FP: Fingerprint = Fingerprint([0; 512]);
const RAW: CatalogEntry = CatalogEntry {
    path: [0; 160],
    entry: dkrt_sdk::store::Entry {
        first: 0,
        size: 0,
        kind: 0,
        sha: [0; 32],
    },
};
static mut RAW_PAGE: [CatalogEntry; 16] = [RAW; 16];
static mut BACKENDS: [Backend; crate::MAX_MODULES] = [const { Backend::new() }; crate::MAX_MODULES];
extern "C" {
    static dkrt_files_fs: [u32; 3];
}
type DirOpen = unsafe extern "C" fn(*const u8, *mut u32) -> i32;
type DirNext = unsafe extern "C" fn(*mut u32, *mut u8) -> i32;
type Metadata = unsafe extern "C" fn(u32, *mut u32) -> i32;

struct Backend {
    ticket: u32,
    phase: u8,
    public_path: [u8; 256],
    directory: [u8; 160],
    directory_len: usize,
    virtual_root: bool,
    strip: usize,
    opener: Option<CatalogOpen>,
    cursor: Option<CatalogCursor>,
    collector: Option<DirectoryCollector<8>>,
    index: usize,
    more: bool,
    after: [u8; 160],
    dt_path: [u8; 256],
    dt_state: [u32; 4],
    dt_output: [u8; 0x114],
    row: FileEntry,
}
impl Backend {
    const fn new() -> Self {
        Self {
            ticket: 0,
            phase: 0,
            public_path: [0; 256],
            directory: [0; 160],
            directory_len: 0,
            virtual_root: false,
            strip: 0,
            opener: None,
            cursor: None,
            collector: None,
            index: 0,
            more: false,
            after: [0; 160],
            dt_path: [0; 256],
            dt_state: [0; 4],
            dt_output: [0; 0x114],
            row: FileEntry::empty(),
        }
    }
    unsafe fn start(&mut self, ticket: u32, request: &Request) -> Result<(), i32> {
        *self = Self::new();
        self.ticket = ticket;
        self.public_path = request.path;
        let n = path_len(&request.path).ok_or(E_SEL_PATH)?;
        if request.root == SAMPLES && crate::device() == 1 {
            self.dt_path[0] = b'/';
            self.dt_path[1..1 + n].copy_from_slice(&request.path[..n]);
            self.phase = 10;
        } else {
            let prefix = if request.root == SAMPLES {
                b"samples".as_slice()
            } else {
                b"".as_slice()
            };
            self.virtual_root = request.root == SAMPLES && n == 0;
            self.directory[..prefix.len()].copy_from_slice(prefix);
            let mut start = prefix.len();
            if start != 0 && n != 0 {
                self.directory[start] = b'/';
                start += 1;
            }
            self.directory[start..start + n].copy_from_slice(&request.path[..n]);
            self.directory_len = start + n;
            self.strip = if request.root == SAMPLES { 8 } else { 0 };
            self.collector = Some(DirectoryCollector::new(
                &self.directory[..self.directory_len],
                None,
            )?);
            self.opener = Some(CatalogOpen::begin(
                &crate::store::reader(),
                &crate::store::layout(),
            )?);
            self.phase = 1;
        }
        Ok(())
    }
    unsafe fn step(&mut self) -> Result<Outcome<FileEntry>, i32> {
        let fp = addr_of_mut!(FP) as *mut u8;
        let scratch = addr_of_mut!(SCRATCH) as *mut u8;
        if self.phase == 1 {
            let complete = match self.opener.as_mut().unwrap().step(fp, scratch, 4096) {
                Err(error) if error == E_NOT_FOUND && self.virtual_root => {
                    return Ok(Outcome::Done)
                }
                result => result?,
            };
            if complete {
                self.cursor = Some(self.opener.take().unwrap().take()?);
                self.phase = 2;
            }
        } else if self.phase == 2 {
            let n = self.cursor.as_mut().unwrap().read_page(
                fp,
                scratch,
                4096,
                &mut *addr_of_mut!(RAW_PAGE),
            )?;
            if n != 0 {
                self.collector.as_mut().unwrap().feed(&RAW_PAGE[..n])?;
            } else {
                if let Err(error) = self.collector.as_mut().unwrap().finish(Ok(())) {
                    if error == E_NOT_FOUND && self.virtual_root {
                        return Ok(Outcome::Done);
                    }
                    return Err(error);
                }
                let page = self.collector.as_ref().unwrap().page()?;
                self.more = page.more;
                self.index = 0;
                if let Some(last) = page.entries.last() {
                    self.after = last.path;
                }
                self.phase = 3;
            }
        } else if self.phase == 3 {
            let page = self.collector.as_ref().unwrap().page()?;
            if self.index < page.entries.len() {
                let value = page.entries[self.index];
                self.index += 1;
                let n = dkrt_sdk::store::path_len(value.path.as_ptr());
                let mut row = FileEntry::empty();
                row.path[..n - self.strip].copy_from_slice(&value.path[self.strip..n]);
                if value.entry.kind == dkrt_sdk::store::KIND_DIR {
                    row.kind = DIRECTORY;
                } else {
                    row.kind = FILE;
                    row.flags = SIZE_VALID | HASH_VALID;
                    row.size_bytes = value.entry.size;
                    row.hash_kind = HASH_SHA256;
                    row.hash = value.entry.sha;
                }
                return Ok(Outcome::Row(row));
            }
            if !self.more {
                return Ok(Outcome::Done);
            }
            self.cursor.as_mut().unwrap().rewind(fp)?;
            let n = dkrt_sdk::store::path_len(self.after.as_ptr());
            self.collector = Some(DirectoryCollector::new(
                &self.directory[..self.directory_len],
                Some(&self.after[..n]),
            )?);
            self.phase = 2;
        } else {
            let table = addr_of!(dkrt_files_fs) as *const u32;
            let open = read_volatile(table);
            let next = read_volatile(table.add(1));
            let metadata = read_volatile(table.add(2));
            if open == 0 || next == 0 || metadata == 0 {
                return Err(E_NOT_FOUND);
            }
            if self.phase == 10 {
                let f: DirOpen = core::mem::transmute(open as usize);
                if f(self.dt_path.as_ptr(), self.dt_state.as_mut_ptr()) != 0 {
                    return Err(E_NOT_FOUND);
                }
                self.phase = 11;
            } else if self.phase == 11 {
                self.dt_output.fill(0);
                let f: DirNext = core::mem::transmute(next as usize);
                let rc = f(self.dt_state.as_mut_ptr(), self.dt_output.as_mut_ptr());
                if rc == 0 {
                    return Ok(Outcome::Done);
                }
                if rc < 0 {
                    return Err(E_READ);
                }
                if rc != 1 {
                    return Err(E_BAD_STORE);
                }
                let name = &self.dt_output[20..];
                let n = name.iter().position(|b| *b == 0).ok_or(E_BAD_STORE)?;
                if &name[..n] == b"." || &name[..n] == b".." {
                    return Ok(Outcome::Pending);
                }
                let parent = path_len(&self.public_path).ok_or(E_SEL_PATH)?;
                let at = parent + if parent != 0 { 1 } else { 0 };
                if at + n >= 256 {
                    return Err(E_TOO_BIG);
                }
                self.row = FileEntry::empty();
                self.row.path[..parent].copy_from_slice(&self.public_path[..parent]);
                if parent != 0 {
                    self.row.path[parent] = b'/';
                }
                self.row.path[at..at + n].copy_from_slice(&name[..n]);
                if path_len(&self.row.path) != Some(at + n) || n == 0 || name[..n].contains(&b'/') {
                    return Err(E_BAD_STORE);
                }
                let kind = u16::from_be_bytes([self.dt_output[4], self.dt_output[5]]);
                if kind == 1 {
                    self.row.kind = DIRECTORY;
                    return Ok(Outcome::Row(self.row));
                }
                self.row.kind = FILE;
                self.phase = 12;
            } else if self.phase == 12 {
                let f: Metadata = core::mem::transmute(metadata as usize);
                let mut values = [0u32; 4];
                let id = u32::from_be_bytes(self.dt_output[..4].try_into().unwrap());
                if f(id, values.as_mut_ptr()) == 0 {
                    self.row.flags = SIZE_VALID | HASH_VALID;
                    self.row.size_bytes = values[2];
                    self.row.hash_kind = HASH_ELEKTRON32;
                    self.row.hash[..4].copy_from_slice(&values[1].to_be_bytes());
                }
                self.phase = 11;
                return Ok(Outcome::Row(self.row));
            } else {
                return Err(E_FORMAT);
            }
        }
        Ok(Outcome::Pending)
    }
}

// Steps per pass: at most MAX_STEPS provider steps, within BUDGET_BLOCKS firmware blocks (the counter runs at 1500/s, so about 5 ms).
const MAX_STEPS: u32 = 32;
const BUDGET_BLOCKS: u32 = 8;
// Step cap when the firmware's counter is not there.
const FALLBACK_STEPS: u32 = 8;

/// Several provider steps, each with its deferred callback, per UI pass within a time budget, independent of slot0.
pub unsafe fn tick() {
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
    let blocks = crate::fw_blocks();
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
            let backend = &mut BACKENDS[lease.owner];
            let result = if backend.ticket != lease.ticket.id() {
                backend
                    .start(lease.ticket.id(), &lease.request)
                    .and_then(|_| backend.step())
            } else {
                backend.step()
            };
            let outcome = match result {
                Ok(value) => value,
                Err(error) => Outcome::Error(error),
            };
            locked(|jobs| {
                let _ = jobs.finish_work(lease, outcome);
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
                let prev = crate::dlog::enter_module(lease.owner);
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
        let spent = match crate::fw_blocks() {
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
