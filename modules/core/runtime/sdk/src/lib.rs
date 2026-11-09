//! dkrt-sdk: the C ABI between the digikit runtime (`core/runtime`) and the modules it loads at run time.
//!
//! Shared by the runtime crate and by module crates (path dependency). `#![no_std]`; every function pointer is a
//! gcc m68k cdecl function (arguments on the stack, result in d0); there are no small-struct results.
//! Format and rules: docs/design/digikit-runtime.md and docs/design/digikit-dkm.md.
#![no_std]

/// Version of the `Api` table layout. A field is only ever appended; `Api::size` tells a module how much is there.
pub const API_VERSION: u32 = 1;
/// Version of the runtime a module was packed against (`runtime_min` in the `.dkm` header). 2: DSP sections and asynchronous loads
/// (finding 32), `register` PAGE records and `own_dsp` (finding 33), codes -25/-26 and -30 to -37. 3: card safety (codes -46/-47, -62), the
/// debug log and its SysEx commands LOG and PEEK, `dkrt_diag`, `dkrt_debug_set`, the crash record (finding 46). 4: SELECT and RESTART.
/// 5: the DSP guard (finding 51): voice rows of 16 words with the knob ranges, DSP sections of version 2 (voices of 48 bytes), the 24-word
/// DSP diagnostics with the guard's fault words, the log events `MACHINE_FAULT` and `MACHINE_DISABLED`.
/// Versions 1 to 5 were plain integers (they read as 0.0.1 to 0.0.5 packed).
/// 0.1.0 = semantic version; contract versions in the .dkm header (a module is refused only on a contract mismatch, docs/design/module-sdk.md 11.5 D3); LOGCFG, log levels and sources, Api.log_lv.
pub use gen_sdk::RUNTIME_VERSION;

/// Facts from the device files (`devices/*.toml`) and `core/machines` types.toml that every device agrees on, written by
/// `patchkit gen` (the `[[gen]]` of core/runtime's module.toml; docs/design/device-data.md). `Api::device` is the device's
/// `[digikit] id`, read from the runtime's device object, not a constant here.
pub mod device {
    include!("device.rs");
}

/// Slots: function pointers the firmware hooks call. A module binds one by exporting `dkm_slot_<n>`.
/// Slot 0 runs once per pass of the UI task's event loop (about 120 Hz), as `extern "C" fn(*const Api)`.
pub const SLOTS: usize = 8;

/// The structs, constants and function types of the runtime's C ABI, written by `patchkit gen` from `core/runtime/abi.toml` (also the C header
/// `core/runtime/include/dkrt.h`): `OK`, `LOADING`, `Dkrt*`, `HEALTH_*`, `SAFE_*`, `CARD_*`, `SCAN_*`, `sig`.
#[rustfmt::skip]
pub mod gen_abi;
pub use gen_abi::*;

// Result codes of `dkrt_load` and `Api::store_read` (negative = error; `OK` is in `gen_abi`). The codes themselves are written by `patchkit gen` from `[load.status]` of `sdk/sdk.toml` (`gen_sdk.rs`, module `load`, with their
// texts): -1 to -14 `dkrt_load`; -20 to -29 the run-time registry (`register` records, docs/design/digikit-registry.md); -30 to -39 the
// DSP side's (docs/design/digikit-dkm.md, "DSP section"); -40 to -49 boot-time loading (finding 37). Additive: a code is never renumbered.
pub use gen_sdk::load::*;

// The `Api` table and `ServiceCmd` are written by `patchkit gen` from `[api]` and `[api.types]` of `sdk/sdk.toml` (`gen_sdk.rs`, module `api`).
pub use gen_sdk::api;
pub use gen_sdk::api::{Api, ServiceCmd};

// `DkrtStatus`, `DkrtCap`, `DkrtHealth`, `DkrtStoreEntry`, `DkrtModule`, `DkrtFx`, `DkrtSelection`, `DkrtBootInfo`, `DkrtDiag` and the scan and fit
// structs, with `STATE_*`, `MOD_*`, `CAP_MAX`, `HEALTH_*`, `SEL_PATH`, `SAFE_*`, `CARD_*` and `DIAG_MAGIC`, are in `gen_abi`.

/// Cycles in one DSP block (32 samples at 48 kHz, 1500 blocks per second) at the 1 GHz core clock of the emulator's model
/// (`[board]` of the device files). The real clock of the device is not known **[O]** (finding 41): the DSP percentages scale with it.
pub const DSP_BLOCK_CYCLES: u32 = device::DSP_BLOCK_CYCLES;

/// Text for a result code, for a menu line (upper case, at most 14 characters). A table, not a `match`: the compiler turns a
/// dense `match` into a PC-relative jump table that is not ColdFire code.
pub fn result_text(code: i32) -> &'static str {
    const TEXTS: [&str; 15] = [
        "OK",
        "NOT FOUND",
        "BAD STORE",
        "READ ERROR",
        "BAD SHA",
        "BAD FORMAT",
        "RUNTIME OLD",
        "NO IMPORT",
        "NO MEMORY",
        "INIT FAILED",
        "ALREADY LOADED",
        "SLOT TAKEN",
        "BUSY",
        "TOO BIG",
        "TABLE FULL",
    ];
    const DSP_TEXTS: [&str; 8] = [
        "NO DSP LINK",
        "BAD DSP CODE",
        "DSP IMPORT",
        "DSP FULL",
        "NO VOICE ROW",
        "DSP TRANSFER",
        "WRONG DEVICE",
        "BAD VOICE",
    ];
    if (E_DSP_TYPE..=E_NO_DSP).contains(&code) {
        DSP_TEXTS[(-code - 30) as usize]
    } else if code == LOADING {
        "LOADING"
    } else if (E_TABLE_FULL..=OK).contains(&code) {
        TEXTS[(-code) as usize]
    } else {
        "ERROR"
    }
}

/// Mailbox commands (emulator and test use). `CMD_LOAD` calls `dkrt_load`, which answers `E_BOOT_ONLY` once the boot is over; command
/// 2 was `CMD_UNLOAD` (archived with hot reload, tag `digikit-hot-reload-v1`) and is answered `E_FORMAT`.
pub const CMD_LOAD: u32 = 1;
/// `arg`: store paths, each NUL terminated, the list ended by an empty path (an empty list clears the selection):
/// `dkrt_selection_set`. The answer is its result.
pub const CMD_SELECT: u32 = 3;
/// `dkrt_restart`; the answer is only seen when it refuses.
pub const CMD_RESTART: u32 = 4;
/// Write ONE copy of the selection record from `arg` (`arg[0]` = copy 0 or 1, then the paths as for `CMD_SELECT`) with sequence
/// number `seq + 1`: lets a test make the state between the two writes of `dkrt_selection_set` (a power cut).
pub const CMD_SELECT_ONE: u32 = 5;
/// Run `dkrt_health` into the exported `DKRT_HEALTH` (the proof reads it; `result` is 0).
pub const CMD_HEALTH: u32 = 6;
/// Test hook: `arg[0..4]` = sector, `arg[4..8]` = byte count (big endian); the runtime calls its one card-write function with the staging
/// buffer and answers its result (0, `E_RANGE` or `E_CARD_RO`). The proof uses it with sectors outside the store range and on a card that
/// is not ours; it never targets a sector of a store that is ours.
pub const CMD_CARD_PROBE: u32 = 7;
// Mailbox command 8 was the debug switch (removed: the debug module being loaded is the switch).
/// Test hooks of the settings store (`settings.rs` of the runtime): `arg` = `u8 key_len`, the key, `u8 value_len`, the value (96 bytes in all). `CMD_SETTINGS_PUT` and
/// `CMD_SETTINGS_REMOVE` answer their result; `CMD_SETTINGS_GET` answers the value's length (or a negative code) and leaves the value at the start of `arg`;
/// `CMD_SETTINGS_FLUSH` writes the record now (the runtime writes it 2 s after the last change).
pub const CMD_SETTINGS_PUT: u32 = 9;
pub const CMD_SETTINGS_GET: u32 = 10;
pub const CMD_SETTINGS_REMOVE: u32 = 11;
pub const CMD_SETTINGS_FLUSH: u32 = 12;

// The selection (what the next boot loads): `SEL_MAX` (`[limits.max] modules` of sdk/sdk.toml, the record's format), `SEL_PATH` and `DkrtSelection` are in
// `gen_abi`, as are `SAFE_*`, `SAFE_AFTER`, `STABLE_BLOCKS` and `DkrtBootInfo`.
/// The key held at power-on for safe mode (`[digikit] safe_key` of the device files; RECORD, panel button 19).
pub const SAFE_KEY_CODE: u32 = device::SAFE_KEY_CODE;

/// The selection record (docs/design/digikit-store.md): one sector, big endian, in two copies at the end of the store range.
pub mod sel {
    use super::*;

    pub const MAGIC: u32 = 0x444b_534c; // "DKSL"
    pub const VERSION: u32 = 1;
    // The absolute sectors of the two copies are `store::Layout::sel_a` and `sel_b`: the end of the store area, which differs per device.

    /// `Record::flags` bit 0: start in safe mode once. Bit 1 is reserved (it was the debug-off flag; the runtime ignores it and does not write it).
    pub const FLAG_SAFE_ONCE: u32 = 1;

    #[derive(Clone, Copy)]
    pub struct Record {
        pub seq: u32,
        pub boot_counter: u32,
        pub flags: u32,
        pub count: u32,
        /// The stable period of this card in DSP blocks (bytes 24 to 27 of the record, reserved zero before): 0 takes `STABLE_BLOCKS`. A
        /// setting for tests and for people who debug a module; nothing in the menu writes it.
        pub stable_blocks: u32,
        /// The sticky machine types (docs/design/module-sdk.md 11.5 D7): up to 8 entries, each `hash24 << 8 | type`, 0 = empty. Entry 0 is
        /// bytes 28 to 31 of the record, entries 1 to 7 bytes 480 to 507.
        pub map: [u32; MAP_N],
        pub paths: [[u8; SEL_PATH]; SEL_MAX],
    }

    /// Entries of the sticky type map.
    pub const MAP_N: usize = crate::gen_sdk::limits::MAP_N; // `[limits.max] map_entries` of sdk/sdk.toml

    /// Byte offset of map entry `i`.
    pub const fn map_at(i: usize) -> usize {
        if i == 0 {
            28
        } else {
            480 + 4 * (i - 1)
        }
    }

    /// The record a new selection writes over `cur`: the sequence number goes up, the boot counter, the stable period and the sticky type map are
    /// kept (the map survives a new selection), the flags keep only `FLAG_SAFE_ONCE`.
    pub fn next_record(
        cur: &Record,
        flags: u32,
        count: u32,
        paths: &[[u8; SEL_PATH]; SEL_MAX],
    ) -> Record {
        let mut r = Record::empty();
        r.seq = cur.seq.wrapping_add(1);
        r.boot_counter = cur.boot_counter;
        r.stable_blocks = cur.stable_blocks;
        r.map = cur.map;
        r.flags = flags & FLAG_SAFE_ONCE;
        r.count = count;
        let mut k = 0;
        while k < count as usize && k < SEL_MAX {
            r.paths[k] = paths[k];
            k += 1;
        }
        r
    }

    /// `E_SEL_COUNT` when a selection names more than `SEL_MAX` paths, else `OK`.
    pub fn check_count(count: u32) -> i32 {
        if count as usize > SEL_MAX {
            E_SEL_COUNT
        } else {
            OK
        }
    }

    /// `check_count` for `CMD_SELECT_ONE`, whose copy index must be 0 or 1 (a bad index has the same code).
    pub fn check_one(count: u32, copy: u32) -> i32 {
        if copy > 1 {
            E_SEL_COUNT
        } else {
            check_count(count)
        }
    }

    /// The mailbox list of paths takes path number `n` (0-based): `E_SEL_COUNT` when `SEL_MAX` paths are already there.
    pub fn check_slot(n: usize) -> i32 {
        if n >= SEL_MAX {
            E_SEL_COUNT
        } else {
            OK
        }
    }

    impl Record {
        pub const fn empty() -> Record {
            Record {
                seq: 0,
                boot_counter: 0,
                flags: 0,
                count: 0,
                stable_blocks: 0,
                map: [0; MAP_N],
                paths: [[0; SEL_PATH]; SEL_MAX],
            }
        }
    }

    pub fn crc32(p: &[u8]) -> u32 {
        let mut crc = 0xffff_ffffu32;
        for &b in p {
            crc ^= b as u32;
            let mut k = 0;
            while k < 8 {
                crc = (crc >> 1) ^ (0xedb8_8320 & 0u32.wrapping_sub(crc & 1));
                k += 1;
            }
        }
        !crc
    }

    fn put(b: &mut [u8], off: usize, v: u32) {
        b[off] = (v >> 24) as u8;
        b[off + 1] = (v >> 16) as u8;
        b[off + 2] = (v >> 8) as u8;
        b[off + 3] = v as u8;
    }
    fn get(b: &[u8], off: usize) -> u32 {
        ((b[off] as u32) << 24)
            | ((b[off + 1] as u32) << 16)
            | ((b[off + 2] as u32) << 8)
            | (b[off + 3] as u32)
    }

    pub fn encode(r: &Record, out: &mut [u8; 512]) {
        let mut i = 0;
        while i < 512 {
            out[i] = 0;
            i += 1;
        }
        put(out, 0, MAGIC);
        put(out, 4, VERSION);
        put(out, 8, r.seq);
        put(out, 12, r.boot_counter);
        put(out, 16, r.flags);
        put(out, 20, r.count);
        put(out, 24, r.stable_blocks);
        let mut m = 0;
        while m < MAP_N {
            put(out, map_at(m), r.map[m]);
            m += 1;
        }
        let mut k = 0;
        while k < SEL_MAX {
            let mut j = 0;
            while j < SEL_PATH {
                out[32 + k * SEL_PATH + j] = r.paths[k][j];
                j += 1;
            }
            k += 1;
        }
        let c = crc32(&out[..508]);
        put(out, 508, c);
    }

    /// A valid record: magic, version, CRC, count, every used path NUL terminated.
    pub fn decode(b: &[u8; 512]) -> Option<Record> {
        if get(b, 0) != MAGIC || get(b, 4) != VERSION || get(b, 508) != crc32(&b[..508]) {
            return None;
        }
        let count = get(b, 20);
        if count as usize > SEL_MAX {
            return None;
        }
        let mut r = Record::empty();
        r.seq = get(b, 8);
        r.boot_counter = get(b, 12);
        r.flags = get(b, 16);
        r.count = count;
        r.stable_blocks = get(b, 24);
        let mut m = 0;
        while m < MAP_N {
            r.map[m] = get(b, map_at(m));
            m += 1;
        }
        let mut k = 0;
        while k < SEL_MAX {
            let mut j = 0;
            let mut nul = false;
            while j < SEL_PATH {
                let c = b[32 + k * SEL_PATH + j];
                r.paths[k][j] = c;
                nul |= c == 0;
                j += 1;
            }
            if (k as u32) < count && (!nul || r.paths[k][0] == 0) {
                return None;
            }
            k += 1;
        }
        Some(r)
    }
}

/// The mailbox the UI-task hook polls. A host writes `arg`, then `cmd`; the runtime runs it, stores `result`, adds one
/// to `seq` and zeroes `cmd`.
#[repr(C)]
pub struct Mailbox {
    pub cmd: u32,
    pub seq: u32,
    pub result: i32,
    pub arg: [u8; 96],
}

/// The module and sample store, version 3 (docs/design/digikit-store.md).
pub mod store;

/// Bounded native sample validation and integer-only PCM decoding.
pub mod sample;
pub mod sample_job;
pub mod sample_tracks;

pub mod files;
/// Internal fixed-memory listing work/notification lifecycle model.
pub mod files_job;
pub mod sample_stream;

/// The sticky machine type assignment (docs/design/module-sdk.md 11.5 D7) as a pure function, tested on the host; the runtime's `assign` calls it.
pub mod sticky;

/// The loader's limit decisions (DSP placement, row counts, refusal codes) as pure functions, tested on the host; the runtime calls them.
pub mod capacity;

/// The settings record, two copies before the selection record (docs/design/digikit-store.md, "The settings record").
pub mod settings;

/// The last fault (a CPU crash or a DSP stop) as the settings key `dk/fault`, and the ABOUT rows made from it.
pub mod fault;

// `CARD_*` (the state of the card's store area as the runtime found it at boot), `DkrtDiag` and `DIAG_MAGIC` are in `gen_abi`
// (docs/findings/46-digikit-debug.md).

/// Event kinds of the debug log (docs/findings/46-digikit-debug.md has the argument meanings; `tools/device/dklog.py` the readable names).
/// An event is 32 bytes: `seq`, `time` (the firmware's block counter, 1500 per second), `kind`, `flags` (bit 0: an error, kept when debug is
/// off), five argument words.
/// Written by `patchkit gen` from `sdk/sdk.toml` (`gen_sdk.rs`), with the contract version numbers (`contract`).
#[rustfmt::skip]
mod gen_sdk;
pub use gen_sdk::{contract, limits, logk, proto};

#[cfg(test)]
mod tests {
    use super::sel::*;
    use super::*;

    fn example() -> Record {
        let mut r = Record::empty();
        r.seq = 7;
        r.boot_counter = 1;
        r.count = 2;
        r.paths[0][..13].copy_from_slice(b"modules/a.dkm");
        r.paths[1][..13].copy_from_slice(b"modules/b.dkm");
        r
    }

    /// The same record as `tools/patchkit/pk_dkrt_boot.py make_record(["modules/a.dkm", "modules/b.dkm"], seq=7, counter=1)`:
    /// the layout of docs/design/digikit-store.md; the CRC is the one that Python's zlib computes.
    #[test]
    fn the_record_has_the_documented_bytes() {
        let mut b = [0u8; 512];
        encode(&example(), &mut b);
        assert_eq!(&b[0..4], b"DKSL");
        assert_eq!(
            &b[4..24],
            &[0, 0, 0, 1, 0, 0, 0, 7, 0, 0, 0, 1, 0, 0, 0, 0, 0, 0, 0, 2]
        );
        assert_eq!(&b[32..45], b"modules/a.dkm");
        assert_eq!(&b[32 + SEL_PATH..45 + SEL_PATH], b"modules/b.dkm");
        assert_eq!(&b[508..512], &0xce5a_5f11u32.to_be_bytes());
    }

    /// The sticky type map: entry 0 is bytes 28 to 31, entries 1 to 7 are bytes 480 to 507 (docs/design/digikit-store.md); the same bytes as
    /// `make_record(..., tmap=...)` in tests/patchkit/test_patchkit_dkrt_boot.py. The words before and after keep their places.
    #[test]
    fn the_sticky_map_is_in_the_reserved_bytes() {
        let mut r = example();
        r.map = [0xe53b_3a10, 0xcf31_7d11, 0, 0, 0, 0, 0, 0x17];
        let mut b = [0u8; 512];
        encode(&r, &mut b);
        assert_eq!(&b[28..32], &0xe53b_3a10u32.to_be_bytes());
        assert_eq!(&b[480..484], &0xcf31_7d11u32.to_be_bytes());
        assert_eq!(&b[504..508], &0x17u32.to_be_bytes());
        assert_eq!(&b[24..28], &[0, 0, 0, 0]); // stable_blocks stays where it was
        assert_eq!(&b[32..45], b"modules/a.dkm");
        assert_eq!(decode(&b).unwrap().map, r.map);
        // a record without a map (the builder's, an older runtime's) has an empty one
        encode(&example(), &mut b);
        assert_eq!(decode(&b).unwrap().map, [0; MAP_N]);
    }

    #[test]
    fn decode_takes_what_encode_made_and_refuses_damage() {
        let mut b = [0u8; 512];
        encode(&example(), &mut b);
        let r = decode(&b).unwrap();
        assert_eq!((r.seq, r.boot_counter, r.flags, r.count), (7, 1, 0, 2));
        assert_eq!(&r.paths[1][..13], b"modules/b.dkm");
        // a flipped bit anywhere before the CRC, a bad magic, a count over 8, a used path with no NUL
        for at in [0usize, 9, 40, 300, 507] {
            let mut c = b;
            c[at] ^= 1;
            assert!(decode(&c).is_none(), "byte {at}");
        }
        let mut c = [0u8; 512];
        assert!(decode(&c).is_none());
        let mut r2 = example();
        r2.count = 9;
        encode(&r2, &mut c);
        assert!(decode(&c).is_none());
        let mut r3 = example();
        r3.paths[0] = [b'x'; SEL_PATH];
        encode(&r3, &mut c);
        assert!(decode(&c).is_none());
    }
}
