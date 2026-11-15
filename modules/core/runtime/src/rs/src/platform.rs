//! The real platform under the core: `RealPlatform`, a zero-sized type that implements `dkrt_abi::platform::Platform` over the device object's words
//! (`src/cf/<dev>/device.s`), the assembly helpers (`tramp.s`) and the firmware calls of the `dkfw` crate. This is the only runtime file that names
//! `dkfw` (`mise run check:layers`); it also holds the one gated card write and the free functions the rest of the core calls (`PLATFORM` is the
//! constant behind them).
//!
//! Host tests link no firmware: under `cfg(test)` the methods that read the device object or the firmware answer like a device with no counters
//! (profile of the first device file, no block clock, no MIDI counter). The tests that drive platform behaviour use `FakePlatform` (`fake_platform.rs`)
//! through the generic functions.

use crate::card;
use crate::state::{State, DKRT_STATE};
use core::ffi::c_void;
use core::ptr::{addr_of, addr_of_mut, read_volatile, write_volatile};
use dkrt_abi::platform::{DeviceProfile, Platform, SamplePool, StagedWriter, Tasking};
use dkrt_sdk::*;

extern "C" {
    /// Per-device constants, defined by `src/cf/<dev>/device.s` from the device file (`patchkit gen`): the digikit id (`[digikit] id`), the name the
    /// INFO reply gives (`[digikit] name`: 3 characters and a NUL, as a big-endian word), where the samples live (`[capabilities] card`: 1 when
    /// they are files of the store) and the longest sample path (`[digikit] sample_path_max`).
    static dkrt_device_id: u32;
    static dkrt_device_name: u32;
    static dkrt_samples_in_store: u32;
    static dkrt_sample_path_max: u32;
    /// The DSP headroom (instructions per block) and the voices (`DSP_HEADROOM`, `DSP_VOICES`).
    static dkrt_dsp_headroom: u32;
    static dkrt_dsp_voices: u32;
    /// The image id patchkit stamps into the image (`src/cf/<dev>/device.s`; `packages/patchkit/src/imageid.rs`): 8 bytes, zero in an object.
    static dkrt_image_id: [u8; 8];
    /// `cpushl` over a byte range (both caches), `src/cf/tramp.s`.
    fn dkrt_flush(start: u32, len: u32);
    /// `src/cf/tramp.s`: mask interrupts (returns the old status register), restore it, and the soft reset.
    fn dkrt_irq_mask() -> u32;
    fn dkrt_irq_restore(sr: u32);
    fn dkrt_soft_reset() -> !;
}

#[cfg(not(test))]
extern "C" {
    /// core/platform (`src/cf/platform.s`): point the firmware's exception hook word at the adapter's trampoline; 1 when it changed the word.
    fn dkplat_exc_install() -> u32;
}

// Presence checks of the optional adapters: only the addresses and the version call (the calls themselves stay with their users, with their own types).
#[allow(clashing_extern_declarations)]
extern "C" {
    fn dkpool_version() -> u32;
    fn dkpool_load();
    fn dkpool_release();
    fn dkpool_check();
    fn dkpool_free_frames();
    fn dkpw_version() -> u32;
    fn dkpw_begin();
    fn dkpw_write();
    fn dkpw_close();
    fn dkpw_abort();
    fn dkpw_delete();
    fn dkpw_recover();
    fn dkpw_staging();
}

/// The version of the sample-pool adapter's plug-in point (`dkrt_pool.h`) and of the staged writer's (`dkrt_pdw.h`) the runtime speaks.
pub const DKPOOL_VERSION: u32 = 1;
pub const DKPW_VERSION: u32 = 1;

/// The real platform: a zero-sized type, so a call through it costs what the direct call cost.
pub struct RealPlatform;

/// The platform of this image. The core calls it through the free functions below, or takes a `P: Platform` where a host test drives the path.
pub const PLATFORM: RealPlatform = RealPlatform;

/// 0 not checked, 1 usable, 2 absent or another version.
static mut POOL_STATE: u8 = 0;
static mut PD_STATE: u8 = 0;

/// The word a firmware code literal points to, when the literal is a DDR address (a misplaced anchor reads nothing).
#[cfg(not(test))]
unsafe fn word_at_literal(lit: *const u32) -> Option<u32> {
    let l = core::hint::black_box(lit as usize) as u32;
    if l == 0 {
        return None;
    }
    let a = read_volatile(l as *const u32);
    if !(dkrt_sdk::device::DDR_CACHED_BASE..dkrt_sdk::device::DDR_CACHED_BASE + dkrt_sdk::device::DDR_BYTES).contains(&a) || a & 3 != 0 {
        return None;
    }
    Some(read_volatile(a as *const u32))
}

impl Platform for RealPlatform {
    #[cfg(not(test))]
    fn profile(&self) -> DeviceProfile {
        unsafe {
            DeviceProfile {
                id: read_volatile(addr_of!(dkrt_device_id)),
                name: read_volatile(addr_of!(dkrt_device_name)),
                samples_in_store: read_volatile(addr_of!(dkrt_samples_in_store)) != 0,
                sample_path_max: read_volatile(addr_of!(dkrt_sample_path_max)),
                dsp_headroom: read_volatile(addr_of!(dkrt_dsp_headroom)),
                dsp_voices: read_volatile(addr_of!(dkrt_dsp_voices)),
            }
        }
    }
    #[cfg(test)]
    fn profile(&self) -> DeviceProfile {
        dkrt_abi::profiles::DT2
    }

    // The facts one word each: a caller that needs one fact does not read the other three (the reads are volatile and stay).
    #[cfg(not(test))]
    fn device_id(&self) -> u32 {
        unsafe { read_volatile(addr_of!(dkrt_device_id)) }
    }
    #[cfg(not(test))]
    fn device_name(&self) -> u32 {
        unsafe { read_volatile(addr_of!(dkrt_device_name)) }
    }
    #[cfg(not(test))]
    fn samples_in_store(&self) -> bool {
        unsafe { read_volatile(addr_of!(dkrt_samples_in_store)) != 0 }
    }
    #[cfg(not(test))]
    fn sample_path_max(&self) -> u32 {
        unsafe { read_volatile(addr_of!(dkrt_sample_path_max)) }
    }
    #[cfg(not(test))]
    fn dsp_headroom(&self) -> u32 {
        unsafe { read_volatile(addr_of!(dkrt_dsp_headroom)) }
    }
    #[cfg(not(test))]
    fn dsp_voices(&self) -> u32 {
        unsafe { read_volatile(addr_of!(dkrt_dsp_voices)) }
    }

    #[cfg(not(test))]
    fn image_id(&self) -> [u8; 8] {
        let p = addr_of!(dkrt_image_id) as *const u8;
        let mut o = [0u8; 8];
        let mut i = 0;
        while i < 8 {
            o[i] = unsafe { read_volatile(p.add(i)) };
            i += 1;
        }
        o
    }
    #[cfg(test)]
    fn image_id(&self) -> [u8; 8] {
        [0; 8]
    }

    /// The firmware's DSP block counter (1500 per second, from boot; wraps), `None` when the anchor does not point at a DDR word.
    /// `BLOCK_ISR_COUNTER_LIT` is the 4-byte address literal in the firmware's code, whose value is the address of the word.
    #[cfg(not(test))]
    fn blocks(&self) -> Option<u32> {
        unsafe { word_at_literal(addr_of!(dkfw::link::BLOCK_ISR_COUNTER_LIT) as *const u32) }
    }
    #[cfg(test)]
    fn blocks(&self) -> Option<u32> {
        None
    }

    #[cfg(not(test))]
    fn irq_mask(&self) -> u32 {
        unsafe { dkrt_irq_mask() }
    }
    #[cfg(test)]
    fn irq_mask(&self) -> u32 {
        0
    }
    #[cfg(not(test))]
    fn irq_restore(&self, sr: u32) {
        unsafe { dkrt_irq_restore(sr) }
    }
    #[cfg(test)]
    fn irq_restore(&self, _sr: u32) {}

    fn flush(&self, start: u32, len: u32) {
        #[cfg(not(test))]
        unsafe {
            dkrt_flush(start, len)
        }
        #[cfg(test)]
        let _ = (start, len, dkrt_flush as unsafe extern "C" fn(u32, u32));
    }

    unsafe fn block_read(&self, sector: u32, bytes: u32, buf: *mut u8) -> i32 {
        dkfw::storage::block_read(sector, bytes, buf)
    }
    unsafe fn block_write_raw(&self, sector: u32, bytes: u32, buf: *const u8) -> i32 {
        dkfw::storage::block_write(sector, bytes, buf)
    }

    unsafe fn midi_send(&self, port: u32, msg: &[u8]) {
        dkfw::midi::send(msg.len() as u32, msg.as_ptr(), port)
    }
    /// Bytes the firmware has handed to the USB stack for MIDI and not yet sent (0 when the host reads), `None` as for `blocks`.
    #[cfg(not(test))]
    fn midi_tx_in_flight(&self) -> Option<u32> {
        unsafe { word_at_literal(addr_of!(dkfw::midi::TX_INFLIGHT_LIT) as *const u32) }
    }
    #[cfg(test)]
    fn midi_tx_in_flight(&self) -> Option<u32> {
        None
    }
    /// Is an OS upgrade stream being taken (finding 75)? The receive state object's byte at +0 is 0 when idle and 1 to 3 from the header on. False when the
    /// anchor does not point at a DDR object, so the caller then acts as before.
    #[cfg(not(test))]
    fn os_upgrade_active(&self) -> bool {
        unsafe { matches!(word_at_literal(addr_of!(dkfw::midi::OS_UPGRADE_STATE_LIT) as *const u32), Some(w) if w >> 24 != 0) }
    }
    #[cfg(test)]
    fn os_upgrade_active(&self) -> bool {
        false
    }

    /// `Project *f(void)`, `Kit *f(Project *)` (the active kit), `int f(Kit *, int track)` (-1: no such track).
    unsafe fn track_type(&self, track: u32) -> Option<i32> {
        let p = dkfw::project::singleton();
        if p == 0 {
            return None;
        }
        if track >= 16 {
            return Some(-1);
        }
        Some(dkfw::project::track_type(dkfw::project::active_kit(p), track as i32))
    }
    /// The stock key state byte. Returns false before the first channel-2 report.
    unsafe fn key_down(&self, key: u32) -> bool {
        dkfw::input::key_down(key) != 0
    }
    #[cfg(not(test))]
    fn soft_reset(&self) -> ! {
        unsafe { dkrt_soft_reset() }
    }
    #[cfg(test)]
    fn soft_reset(&self) -> ! {
        let _ = dkrt_soft_reset as unsafe extern "C" fn() -> !;
        panic!("soft reset")
    }
    #[cfg(not(test))]
    fn install_exception_hook(&self) -> bool {
        unsafe { dkplat_exc_install() != 0 }
    }
    #[cfg(test)]
    fn install_exception_hook(&self) -> bool {
        false
    }
}

impl Tasking for RealPlatform {
    unsafe fn sem_init(&self, sem: *mut u32, count: u32) {
        dkfw::rtos::sem_init(sem, count)
    }
    unsafe fn sem_pend(&self, sem: *mut u32) {
        dkfw::rtos::sem_pend(sem)
    }
    unsafe fn sem_post(&self, sem: *mut u32) {
        dkfw::rtos::sem_post(sem)
    }
    unsafe fn sleep_us(&self, us: u32) {
        dkfw::rtos::sleep_us(us)
    }
    unsafe fn task_create(&self, tcb: u32, entry: u32, priority: u32, stack: u32, stack_bytes: u32) -> bool {
        dkfw::rtos::task_create(tcb, entry, priority, stack, stack_bytes) == 1
    }
    unsafe fn task_start(&self, tcb: u32) {
        dkfw::rtos::task_start(tcb)
    }
    /// The kernel's pointer variable of the current task.
    unsafe fn current_task(&self) -> u32 {
        read_volatile(addr_of!(dkfw::rtos::CURRENT_TCB) as *const u32)
    }
    unsafe fn alloc(&self, bytes: u32) -> *mut u8 {
        dkfw::mem::new(bytes)
    }
    unsafe fn free(&self, p: *mut u8) {
        dkfw::mem::free(p)
    }
}

impl SamplePool for RealPlatform {
    /// Is a backing adapter of the right version in the image?
    fn present(&self) -> bool {
        unsafe {
            if read_volatile(addr_of!(POOL_STATE)) == 0 {
                let here = core::hint::black_box(dkpool_version as *const () as usize) != 0
                    && core::hint::black_box(dkpool_load as *const () as usize) != 0
                    && core::hint::black_box(dkpool_release as *const () as usize) != 0
                    && core::hint::black_box(dkpool_check as *const () as usize) != 0
                    && core::hint::black_box(dkpool_free_frames as *const () as usize) != 0;
                write_volatile(addr_of_mut!(POOL_STATE), if here && dkpool_version() == DKPOOL_VERSION { 1 } else { 2 });
            }
            read_volatile(addr_of!(POOL_STATE)) == 1
        }
    }
    unsafe fn worker(&self) -> u32 {
        dkfw::samples::worker_get()
    }
    unsafe fn worker_queue(&self, worker: u32, name: *const u8, job: *mut c_void, done: *mut c_void) {
        dkfw::samples::worker_queue(worker, name, job, done);
    }
}

impl RealPlatform {
    /// The profile facts the storage routes need.
    fn storage_facts(&self) -> crate::storage_adapter::Facts {
        crate::storage_adapter::Facts { in_store: Platform::samples_in_store(self), sample_path_max: Platform::sample_path_max(self) }
    }
}

// The routes (store, DT2 firmware file system) are in storage_adapter.rs.
impl dkrt_abi::storage::Storage for RealPlatform {
    fn path_max(&self, root: u32) -> u32 {
        match root {
            dkrt_abi::storage::ROOT_SAMPLES => Platform::sample_path_max(self),
            dkrt_abi::storage::ROOT_STORE => dkrt_sdk::store::PATH_MAX as u32,
            _ => 0,
        }
    }
    unsafe fn stat(&self, root: u32, path: &[u8]) -> Result<dkrt_abi::storage::Stat, i32> {
        crate::storage_adapter::stat(root, &self.storage_facts(), path)
    }
    unsafe fn read_head(&self, root: u32, path: &[u8], out: &mut [u8; 64]) -> Result<(), i32> {
        crate::storage_adapter::read_head(root, &self.storage_facts(), path, out)
    }
    unsafe fn stat_head(&self, root: u32, path: &[u8], out: &mut [u8; 64]) -> Result<(dkrt_abi::storage::Stat, usize), i32> {
        crate::storage_adapter::stat_head(root, &self.storage_facts(), path, out)
    }
    unsafe fn read_range(&self, root: u32, path: &[u8], offset: u32, out: &mut [u8]) -> Result<usize, i32> {
        crate::storage_adapter::read_range(root, &self.storage_facts(), path, offset, out)
    }
    unsafe fn read_all(&self, root: u32, path: &[u8], dst: *mut u8, cap: u32) -> Result<u32, i32> {
        crate::storage_adapter::read_all(root, &self.storage_facts(), path, dst, cap)
    }
    fn list_state_bytes(&self, _root: u32) -> usize {
        crate::storage_adapter::list_state_bytes()
    }
    unsafe fn list_open(&self, root: u32, dir: &[u8], state: *mut u8) -> Result<(), i32> {
        crate::storage_adapter::list_open(root, &self.storage_facts(), dir, state)
    }
    unsafe fn list_next(&self, state: *mut u8, out: &mut dkrt_abi::storage::DirEntry) -> Result<dkrt_abi::storage::ListStep, i32> {
        crate::storage_adapter::list_next(state, out)
    }
}

impl StagedWriter for RealPlatform {
    /// Is an adapter of the right version in the image?
    fn present(&self) -> bool {
        unsafe {
            if read_volatile(addr_of!(PD_STATE)) == 0 {
                let here = core::hint::black_box(dkpw_begin as *const () as usize) != 0
                    && core::hint::black_box(dkpw_write as *const () as usize) != 0
                    && core::hint::black_box(dkpw_close as *const () as usize) != 0
                    && core::hint::black_box(dkpw_abort as *const () as usize) != 0
                    && core::hint::black_box(dkpw_delete as *const () as usize) != 0
                    && core::hint::black_box(dkpw_recover as *const () as usize) != 0
                    && core::hint::black_box(dkpw_version as *const () as usize) != 0
                    && core::hint::black_box(dkpw_staging as *const () as usize) != 0;
                write_volatile(addr_of_mut!(PD_STATE), if here && dkpw_version() == DKPW_VERSION { 1 } else { 2 });
            }
            read_volatile(addr_of!(PD_STATE)) == 1
        }
    }
}

// ---- firmware words the core still reaches directly; they move into the platform adapter at R3b ------------------------------------

/// The address of the firmware's pointer to the intro's current entry (`INTRO_ENTRY`), a firmware object the intro takes over.
pub fn intro_entry_addr() -> *mut u32 {
    addr_of!(dkfw::rtos::INTRO_ENTRY) as *mut u32
}

/// Draw the intro frame `src` onto the firmware bitmap `dst` (the firmware's blit).
pub unsafe fn blit(dst: *mut u8, src: *mut u8) {
    dkfw::draw::blit(dst, src, 0, 0, 0)
}

/// The firmware's own save of the working state: the project into the persistent arena, the arena into the boot record.
pub unsafe fn project_serialize() {
    dkfw::project::serialize();
}
pub unsafe fn boot_record_write() {
    dkfw::storage::boot_record_write();
}

/// The selected track and the first sound words of its Sound array: the objects of `debug_selected_params`.
pub unsafe fn selected_sound() -> Option<(u32, u32)> {
    let p = dkfw::project::singleton();
    if p == 0 {
        return None;
    }
    let track = dkfw::page::view_track(dkfw::project::settings(p) as *const u8).min(15);
    let sound = dkfw::page::native_track_sound(dkfw::project::active_kit(p) as *mut u8, track) as u32;
    Some((track, sound))
}

/// The firmware's heap and block read as the `dk_fw_alloc`, `dk_fw_free` and `dk_fw_block_read` imports.
pub use dkfw::mem::{free as fw_free, new as fw_alloc};
pub use dkfw::storage::block_read as fw_block_read;

// ---- the free functions of the core ------------------------------------------------------------------------------------------------

pub fn st() -> &'static mut State {
    unsafe { &mut *addr_of_mut!(DKRT_STATE) }
}

pub fn flush(start: u32, len: u32) {
    PLATFORM.flush(start, len)
}

/// The device's profile (generated from the device file; the runtime never compares the device id).
pub fn profile() -> DeviceProfile {
    PLATFORM.profile()
}

/// The digikit id of the device (a value for logs and replies; the runtime branches on `profile()` facts, never on this).
pub fn device() -> u32 {
    PLATFORM.device_id()
}

/// The device's short name as the INFO reply gives it: 3 characters and a NUL.
pub fn device_name() -> u32 {
    PLATFORM.device_name()
}

/// The 8-byte id of this image, as patchkit stamped it (zeros when the object was not patched in by it).
pub fn image_id() -> [u8; 8] {
    PLATFORM.image_id()
}

/// The firmware's DSP block counter: `None` when it is not available.
pub fn fw_blocks() -> Option<u32> {
    PLATFORM.blocks()
}

pub fn tx_in_flight() -> Option<u32> {
    PLATFORM.midi_tx_in_flight()
}

pub fn os_upgrade_active() -> bool {
    PLATFORM.os_upgrade_active()
}

pub fn irq_mask() -> u32 {
    PLATFORM.irq_mask()
}

pub fn irq_restore(sr: u32) {
    PLATFORM.irq_restore(sr)
}

pub unsafe fn block_read(sector: u32, bytes: u32, buf: *mut u8) -> i32 {
    PLATFORM.block_read(sector, bytes, buf)
}

/// The firmware's block write, ungated. Only `block_write` and `card::claim` call it.
pub unsafe fn raw_block_write(sector: u32, bytes: u32, buf: *const u8) -> i32 {
    PLATFORM.block_write_raw(sector, bytes, buf)
}

/// The one function that writes the card. Write `bytes` (a multiple of 512) from `buf` (16-byte aligned) to `sector`: refused with `E_RANGE`
/// outside the store range of the device file, and with `E_CARD_RO` when the card's area is not ours (`card::admit`; finding 46). Every
/// runtime card write goes through here: the selection record and the boot counter (boot.rs), uploads (store_cmds.rs), and the claim of a blank
/// range (card.rs, which calls `raw_block_write` for its own headers after the same checks).
pub unsafe fn block_write(sector: u32, bytes: u32, buf: *const u8) -> i32 {
    block_write_with(&PLATFORM, sector, bytes, buf)
}

/// `block_write` over a platform (the host tests drive it with `FakePlatform`).
pub unsafe fn block_write_with<P: Platform>(p: &P, sector: u32, bytes: u32, buf: *const u8) -> i32 {
    let g = card::admit_with(p, sector, bytes);
    if g != OK {
        return g;
    }
    card::info().writes = card::info().writes.wrapping_add(1);
    p.block_write_raw(sector, bytes, buf)
}

/// Send one complete MIDI message (F0 .. F7 for SysEx) to `port` (2 = USB). The firmware takes the MIDI TX mutex itself and spins
/// when the 4 KiB TX ring is full.
pub unsafe fn midi_send(len: u32, msg: *const u8, port: u32) {
    PLATFORM.midi_send(port, core::slice::from_raw_parts(msg, len as usize))
}

pub use dkrt_abi::mem::{cstr_eq, cstr_len, rd32, wr32};
