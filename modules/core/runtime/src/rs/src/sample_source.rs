//! Complete private source snapshots for the bounded mono sample proof. UI task
//! only. DT2 file reads are synchronous and require no simultaneous Transfer
//! writes: an open handle is not yet a proven immutable-file lease (finding 105).
use crate::{arena, sha256, store};
use core::ptr::{addr_of, read_volatile};
use dkrt_sdk::{sample, E_FORMAT, E_NOMEM, E_NOT_FOUND, E_READ, E_TOO_BIG};

const INPUT_CAP: usize = (sample::MAX_FILE_BYTES + 511) & !511;
const SCRATCH_BYTES: usize = 1024;
const READ_CHUNK: u32 = 4096;

/// Arena allocation containing LE float32 words for DSP-link, with no Drop.
/// The transport owner frees `ptr` only after proving transport quiescence;
/// ambiguous failures must retain/quarantine it until restart.
pub struct Source {
    pub ptr: *mut u8,
    pub frames: u32,
}

/// Complete private copy followed by validation/decode. All temporary buffers
/// are released before returning; success transfers ownership of `Source::ptr`.
/// Call serially from the UI task and pass a valid NUL-terminated path.
pub unsafe fn load(path: *const u8) -> Result<Source, i32> {
    if path.is_null() {
        return Err(E_FORMAT);
    }
    // IDs come from devices/{digitakt-ii,digitone-ii}.toml [digikit].
    let device = crate::device();
    if device != 1 && device != 2 {
        return Err(E_NOT_FOUND);
    }
    let input = arena::alloc((INPUT_CAP + SCRATCH_BYTES) as u32);
    if input.is_null() {
        return Err(E_NOMEM);
    }
    let result = (|| {
        let bytes = if device == 1 {
            read_dt2(path, input)?
        } else {
            if crate::card::state() == dkrt_sdk::CARD_TOO_SMALL {
                return Err(E_NOT_FOUND);
            }
            let entry = dkrt_sdk::store::snapshot(
                &store::reader(),
                &store::layout(),
                path,
                input.add(INPUT_CAP),
                input.add(INPUT_CAP + 512),
                512,
                input,
                INPUT_CAP,
                sample::MAX_FILE_BYTES as u32,
                |bytes| {
                    let mut sha = [0; 32];
                    sha256::sha256(bytes.as_ptr(), bytes.len() as u32, sha.as_mut_ptr());
                    sha
                },
            )?;
            entry.size as usize
        };
        let file = core::slice::from_raw_parts(input, bytes);
        let frames = sample::native_frames(file)?;
        let output = arena::alloc(frames * 4);
        if output.is_null() {
            return Err(E_NOMEM);
        }
        if let Err(error) = sample::decode_native(file, core::slice::from_raw_parts_mut(output as *mut u32, frames as usize)) {
            arena::free(output);
            return Err(error);
        }
        // ColdFire is big endian; DSP-link takes raw LE byte words. Read each
        // decoded host word before replacing its bytes in place.
        let mut i = 0;
        while i < frames as usize {
            let bits = *(output as *const u32).add(i);
            let le = bits.to_le_bytes();
            core::ptr::copy_nonoverlapping(le.as_ptr(), output.add(i * 4), 4);
            i += 1;
        }
        Ok(Source { ptr: output, frames })
    })();
    arena::free(input);
    result
}

// Signatures from devices/fw/storage.toml, finding 105: size takes entry id,
// all other routines take a pointer to the four-word handle. ColdFire cdecl.
type FsOpen = unsafe extern "C" fn(*const u8, *const u8, *mut u32) -> i32;
type FsRead = unsafe extern "C" fn(*mut u8, u32, *mut u32) -> i32;
type FsClose = unsafe extern "C" fn(*mut u32) -> i32;
type FsSize = unsafe extern "C" fn(u32) -> u32;

extern "C" {
    static dkrt_sample_fs: [u32; 4];
}

unsafe fn read_dt2(path: *const u8, dst: *mut u8) -> Result<usize, i32> {
    // The per-device assembly object supplies these current-port symbols on
    // DT2 and four zeros on DN2. Shared code has no DT2-only firmware imports.
    let table = addr_of!(dkrt_sample_fs) as *const u32;
    let open_addr = read_volatile(table);
    let read_addr = read_volatile(table.add(1));
    let close_addr = read_volatile(table.add(2));
    let size_addr = read_volatile(table.add(3));
    if open_addr == 0 || read_addr == 0 || close_addr == 0 || size_addr == 0 {
        return Err(E_NOT_FOUND);
    }
    let open: FsOpen = core::mem::transmute(open_addr as usize);
    let read: FsRead = core::mem::transmute(read_addr as usize);
    let close: FsClose = core::mem::transmute(close_addr as usize);
    let size_of: FsSize = core::mem::transmute(size_addr as usize);
    let mut handle = [0u32; 4];
    if open(path, b"r\0".as_ptr(), handle.as_mut_ptr()) != 0 {
        return Err(E_NOT_FOUND);
    }
    let result = (|| {
        let size = size_of(handle[0]);
        if size == u32::MAX {
            return Err(E_READ);
        }
        if size == 0 || size > sample::MAX_FILE_BYTES as u32 {
            return Err(E_TOO_BIG);
        }
        let mut done = 0;
        while done < size {
            let count = core::cmp::min(size - done, READ_CHUNK);
            let got = read(dst.add(done as usize), count, handle.as_mut_ptr());
            if got != count as i32 {
                return Err(E_READ);
            }
            done += count;
        }
        // Detect an appended source or changed metadata. This does not claim
        // to detect concurrent same-length replacement, which is unsupported.
        let mut extra = 0u8;
        if read(&mut extra, 1, handle.as_mut_ptr()) != 0 || size_of(handle[0]) != size {
            return Err(E_READ);
        }
        Ok(size as usize)
    })();
    // Every successful open reaches exactly one close, including size/read
    // errors. A close failure invalidates an otherwise successful acquisition.
    let closed = close(handle.as_mut_ptr());
    if closed != 0 {
        return Err(E_READ);
    }
    result
}
