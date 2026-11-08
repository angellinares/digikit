//! The settings record (docs/design/digikit-store.md, "The settings record"): digikit's own settings, kept in the store range of the card, in two copies
//! of `SECTORS` sectors before the selection record (`store::Layout::set_a`, `set_b`). The runtime holds the record in RAM, changes it there, and
//! writes the copy that is not current with `seq + 1` once nothing has changed for a while; a power cut in the write leaves the other copy valid.
//! Shared by the runtime and by the host readers. `#![no_std]` and ColdFire rules: no `match` on integers, no `core::fmt`, loops bounded by the buffer.
//!
//! ```text
//! 0   4   magic "DKST"
//! 4   2   u16 version = 1
//! 6   4   u32 seq             sequence number of this write
//! 10  4   u32 used            bytes of the record: this header and the entries (18 to AREA)
//! 14  4   u32 crc32           zlib's crc32 over bytes 0..14 and 18..used (the header without this field, then the entries)
//! 18  ..  entries: u8 key_len | key (1 to KEY_MAX bytes, printable ASCII) | u16 value_len | value (0 to VALUE_MAX bytes)
//! ```
//!
//! All integers are big-endian. The bytes after `used` are not part of the record (a shorter record leaves old bytes there; `crc32` ignores them).
//! A copy is **valid** when its magic, version, `used` bounds, crc32 and entries (every entry inside `used`, every key legal, no key twice) hold.
//! A reader takes the valid copy with the higher `seq` (equal: A); no valid copy is an empty record.
use crate::gen_sdk::settings as g;
use crate::proto::{E_SET_FULL, E_SET_KEY, E_SET_NOKEY, E_SET_VALUE, E_WRITE};
use crate::store::{crc32, Io, Layout};
use crate::{E_BAD_STORE, E_READ, OK};

pub const MAGIC: u32 = 0x444b_5354; // "DKST", as the store header (the version field tells them apart: the store's is a u32 3 at this place)
pub const VERSION: u16 = 1;
/// Bytes of the header.
pub const HEADER: usize = 18;
/// Sectors of one copy and its size in bytes (`sdk/sdk.toml [settings]`).
pub const SECTORS: u32 = g::SECTORS;
pub const AREA: usize = SECTORS as usize * 512;
/// Longest key and value (`sdk/sdk.toml [settings]`).
pub const KEY_MAX: usize = g::KEY_MAX;
pub const VALUE_MAX: usize = g::VALUE_MAX;
/// The keys by owner: (prefix, how a host reader decodes the value: `u8`, `route`, `u8x16`).
pub const KEYS: [(&str, &str); g::KEYS.len()] = g::KEYS;
/// `Loaded::copy` when no copy is valid.
pub const NONE: u32 = 2;

const O_VERSION: usize = 4;
const O_SEQ: usize = 6;
const O_USED: usize = 10;
const O_CRC: usize = 14;

fn put16(b: &mut [u8], at: usize, v: u32) {
    b[at] = (v >> 8) as u8;
    b[at + 1] = v as u8;
}
fn put32(b: &mut [u8], at: usize, v: u32) {
    b[at] = (v >> 24) as u8;
    b[at + 1] = (v >> 16) as u8;
    b[at + 2] = (v >> 8) as u8;
    b[at + 3] = v as u8;
}
fn get16(b: &[u8], at: usize) -> u32 {
    ((b[at] as u32) << 8) | (b[at + 1] as u32)
}
fn get32(b: &[u8], at: usize) -> u32 {
    ((b[at] as u32) << 24)
        | ((b[at + 1] as u32) << 16)
        | ((b[at + 2] as u32) << 8)
        | (b[at + 3] as u32)
}

/// The crc32 of a record of `used` bytes: bytes 0..14, then bytes 18..used. `b` holds at least `used` bytes.
pub fn crc_of(b: &[u8], used: usize) -> u32 {
    let c = crc32(0xffff_ffff, b.as_ptr(), O_CRC);
    !crc32(c, b[HEADER..used].as_ptr(), used - HEADER)
}

/// Are the two byte strings equal? A loop, not `==`: a slice comparison compiles to a call of `memcmp`, which the runtime does not link.
fn same(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut i = 0;
    while i < a.len() {
        if a[i] != b[i] {
            return false;
        }
        i += 1;
    }
    true
}

/// May `key` be a key: 1 to `KEY_MAX` printable ASCII characters (0x21 to 0x7e)?
pub fn valid_key(key: &[u8]) -> bool {
    if key.is_empty() || key.len() > KEY_MAX {
        return false;
    }
    let mut i = 0;
    while i < key.len() {
        if key[i] < 0x21 || key[i] > 0x7e {
            return false;
        }
        i += 1;
    }
    true
}

/// Build the stored key of a module's own key: `<module>/<key>` into `out`; returns its length, or `None` when `key` is empty, holds `/` (`test` + `m/x` and `test/m` + `x` would collide),
/// holds `..`, or the whole key is not a legal key (a byte that is not printable ASCII, or more than `KEY_MAX` characters). `module` is the name
/// from the module's own header, never caller input. The prefix stops mistakes, not hostile code: modules are native code with no memory protection.
pub fn module_key(module: &[u8], key: &[u8], out: &mut [u8; KEY_MAX]) -> Option<usize> {
    if module.is_empty() || key.is_empty() || key.contains(&b'/') {
        return None;
    }
    let n = module.len() + 1 + key.len();
    if n > KEY_MAX {
        return None;
    }
    let mut i = 0;
    while i + 1 < key.len() {
        if key[i] == b'.' && key[i + 1] == b'.' {
            return None;
        }
        i += 1;
    }
    i = 0;
    while i < module.len() {
        out[i] = module[i];
        i += 1;
    }
    out[module.len()] = b'/';
    i = 0;
    while i < key.len() {
        out[module.len() + 1 + i] = key[i];
        i += 1;
    }
    if valid_key(&out[..n]) {
        Some(n)
    } else {
        None
    }
}

/// The bytes of the record, `used` of them (an empty record when `b` has no header yet: call `init`).
pub fn used(b: &[u8]) -> usize {
    get32(b, O_USED) as usize
}

/// Make `b` an empty record with sequence number `seq`.
pub fn init(b: &mut [u8], seq: u32) {
    put32(b, 0, MAGIC);
    put16(b, O_VERSION, VERSION as u32);
    put32(b, O_USED, HEADER as u32);
    seal(b, seq);
}

/// Set the sequence number and the crc32 of a record whose entries are final.
pub fn seal(b: &mut [u8], seq: u32) {
    put32(b, O_SEQ, seq);
    let u = used(b);
    let c = crc_of(b, u);
    put32(b, O_CRC, c);
}

/// The first part of a record: what a reader needs to decide which copy to read in full.
pub fn peek(b: &[u8]) -> Option<(u32, usize)> {
    if b.len() < HEADER || get32(b, 0) != MAGIC || get16(b, O_VERSION) != VERSION as u32 {
        return None;
    }
    let u = used(b);
    if !(HEADER..=AREA).contains(&u) {
        return None;
    }
    Some((get32(b, O_SEQ), u))
}

/// One entry: where its key and value are in the record, and where it ends.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Entry {
    pub at: usize,
    pub key: (usize, usize),
    pub value: (usize, usize),
    pub end: usize,
}

/// The entry that starts at `at` in a record of `used` bytes, when it lies inside it and its key is legal.
pub fn entry_at(b: &[u8], used: usize, at: usize) -> Option<Entry> {
    if at + 1 > used || used > b.len() {
        return None;
    }
    let kl = b[at] as usize;
    let ks = at + 1;
    if kl == 0 || kl > KEY_MAX || ks + kl + 2 > used {
        return None;
    }
    let vl = get16(b, ks + kl) as usize;
    let vs = ks + kl + 2;
    if vl > VALUE_MAX || vs + vl > used || !valid_key(&b[ks..ks + kl]) {
        return None;
    }
    Some(Entry {
        at,
        key: (ks, kl),
        value: (vs, vl),
        end: vs + vl,
    })
}

/// A record that is whole: header, crc32 and every entry (no key twice) checked. Returns its `seq` and `used`.
pub fn check(b: &[u8]) -> Option<(u32, usize)> {
    let (seq, u) = peek(b)?;
    if u > b.len() || get32(b, O_CRC) != crc_of(b, u) {
        return None;
    }
    let mut at = HEADER;
    while at < u {
        let e = entry_at(b, u, at)?;
        // a key twice: entries are few (the area is 8 KiB), so a pass for each is cheap
        let mut o = HEADER;
        while o < at {
            let f = entry_at(b, u, o)?;
            if same(
                &b[f.key.0..f.key.0 + f.key.1],
                &b[e.key.0..e.key.0 + e.key.1],
            ) {
                return None;
            }
            o = f.end;
        }
        at = e.end;
    }
    if at != u {
        return None;
    }
    Some((seq, u))
}

/// Why `check` refuses `b`: `"ok"` when it does not, else the first rule it breaks: `"magic"` (no record: too short or no `DKST`), `"version"`, `"used"` (outside
/// `HEADER..=AREA`, or more bytes than `b` holds), `"crc"`, `"entry"` (an entry overruns the record, has an illegal key, or the key is there twice).
/// For the host readers' reports; the vectors carry the expected word.
pub fn why(b: &[u8]) -> &'static str {
    if b.len() < HEADER || get32(b, 0) != MAGIC {
        return "magic";
    }
    if get16(b, O_VERSION) != VERSION as u32 {
        return "version";
    }
    let u = used(b);
    if !(HEADER..=AREA).contains(&u) || u > b.len() {
        return "used";
    }
    if get32(b, O_CRC) != crc_of(b, u) {
        return "crc";
    }
    if check(b).is_none() {
        return "entry";
    }
    "ok"
}

/// The entries of a checked record, in order: (key, value).
pub struct Iter<'a> {
    b: &'a [u8],
    used: usize,
    at: usize,
}

pub fn iter(b: &[u8], used: usize) -> Iter<'_> {
    Iter {
        b,
        used,
        at: HEADER,
    }
}

impl<'a> Iterator for Iter<'a> {
    type Item = (&'a [u8], &'a [u8]);
    fn next(&mut self) -> Option<Self::Item> {
        let e = entry_at(self.b, self.used, self.at)?;
        self.at = e.end;
        Some((
            &self.b[e.key.0..e.key.0 + e.key.1],
            &self.b[e.value.0..e.value.0 + e.value.1],
        ))
    }
}

/// The entry of `key` in the record `b` (checked, `used` bytes), if there is one.
pub fn find(b: &[u8], used: usize, key: &[u8]) -> Option<Entry> {
    let mut at = HEADER;
    while at < used {
        let e = entry_at(b, used, at)?;
        if same(&b[e.key.0..e.key.0 + e.key.1], key) {
            return Some(e);
        }
        at = e.end;
    }
    None
}

/// Copy the value of `key` into `out`; returns its length, `E_SET_NOKEY` when there is no such key, `E_SET_KEY` for a key that breaks the rules,
/// `E_SET_VALUE` when `out` is too small.
pub fn get(b: &[u8], key: &[u8], out: &mut [u8]) -> i32 {
    if !valid_key(key) {
        return E_SET_KEY;
    }
    let used = used(b);
    match find(b, used, key) {
        None => E_SET_NOKEY,
        Some(e) => {
            if e.value.1 > out.len() {
                return E_SET_VALUE;
            }
            let mut i = 0;
            while i < e.value.1 {
                out[i] = b[e.value.0 + i];
                i += 1;
            }
            e.value.1 as i32
        }
    }
}

/// Move `b[from..used]` to `to` (either way); the bytes it leaves behind keep their value.
fn shift(b: &mut [u8], from: usize, used: usize, to: usize) {
    if to < from {
        let mut i = from;
        while i < used {
            b[to + (i - from)] = b[i];
            i += 1;
        }
    } else {
        let mut i = used;
        while i > from {
            i -= 1;
            b[to + (i - from)] = b[i];
        }
    }
}

/// Set `key` to `value` in the record `b` (the whole area, a record with a header): replaces the value or appends the entry. `Ok(true)` when the
/// record changed, `Ok(false)` when the key already had this value. `E_SET_KEY`, `E_SET_VALUE` (over `VALUE_MAX`) or `E_SET_FULL` (the area is
/// full: the record is left as it was). The sequence number and the crc32 are not touched: `seal` them before the record is written.
pub fn put(b: &mut [u8], key: &[u8], value: &[u8]) -> Result<bool, i32> {
    if !valid_key(key) {
        return Err(E_SET_KEY);
    }
    if value.len() > VALUE_MAX {
        return Err(E_SET_VALUE);
    }
    let cap = if b.len() < AREA { b.len() } else { AREA };
    let u = used(b);
    let entry_len = 1 + key.len() + 2 + value.len();
    let new_used;
    match find(b, u, key) {
        Some(e) => {
            let old = e.value.1;
            if same(&b[e.value.0..e.value.0 + old], value) {
                return Ok(false);
            }
            new_used = u - old + value.len();
            if new_used > cap {
                return Err(E_SET_FULL);
            }
            shift(b, e.end, u, e.value.0 + value.len());
            put16(b, e.value.0 - 2, value.len() as u32);
            let mut i = 0;
            while i < value.len() {
                b[e.value.0 + i] = value[i];
                i += 1;
            }
        }
        None => {
            new_used = u + entry_len;
            if new_used > cap {
                return Err(E_SET_FULL);
            }
            let mut at = u;
            b[at] = key.len() as u8;
            at += 1;
            let mut i = 0;
            while i < key.len() {
                b[at + i] = key[i];
                i += 1;
            }
            at += key.len();
            put16(b, at, value.len() as u32);
            at += 2;
            i = 0;
            while i < value.len() {
                b[at + i] = value[i];
                i += 1;
            }
        }
    }
    put32(b, O_USED, new_used as u32);
    Ok(true)
}

/// Remove `key` from the record. `Ok(true)` when it was there, `Ok(false)` when it was not.
pub fn remove(b: &mut [u8], key: &[u8]) -> Result<bool, i32> {
    if !valid_key(key) {
        return Err(E_SET_KEY);
    }
    let u = used(b);
    match find(b, u, key) {
        None => Ok(false),
        Some(e) => {
            shift(b, e.end, u, e.at);
            put32(b, O_USED, (u - (e.end - e.at)) as u32);
            Ok(true)
        }
    }
}

/// What `load` found.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Loaded {
    /// The copy the record came from (0 = A, 1 = B), or `NONE`.
    pub copy: u32,
    pub seq: u32,
    pub used: u32,
    /// Copies that were not valid although they held something that looked like a record (magic and version): a damaged copy.
    pub damaged: u32,
}

/// Read the settings area of the card: both copies' first sectors (`hdr`: 512 bytes, 16-byte aligned), then the one with the higher `seq` in full
/// into `buf` (`AREA` bytes, 16-byte aligned). A copy whose full read or check fails is skipped for the other one. No valid copy: `buf` becomes an
/// empty record with `seq` 0 and the result has `copy == NONE`. `Err(E_READ)` when a read fails (`buf` is then an empty record too).
pub unsafe fn load(
    io: &Io,
    lay: &Layout,
    hdr: *mut u8,
    buf: &mut [u8; AREA],
) -> Result<Loaded, i32> {
    let start = [lay.set_a(), lay.set_b()];
    let mut seqs = [0u32; 2];
    let mut useds = [0usize; 2];
    let mut cand = [false; 2];
    let mut damaged = 0;
    let mut failed = false;
    let mut c = 0;
    while c < 2 {
        if (io.rd)(start[c], 512, hdr) != 0 {
            failed = true;
        } else if let Some((seq, u)) = peek(core::slice::from_raw_parts(hdr, 512)) {
            seqs[c] = seq;
            useds[c] = u;
            cand[c] = true;
        }
        c += 1;
    }
    // the order: the higher seq first, A first when equal
    let first = if cand[1] && (!cand[0] || seqs[1] > seqs[0]) {
        1
    } else {
        0
    };
    let order = [first, 1 - first];
    let mut k = 0;
    while k < 2 {
        let c = order[k];
        k += 1;
        if !cand[c] {
            continue;
        }
        let sectors = (useds[c] as u32 + 511) / 512;
        if (io.rd)(start[c], sectors * 512, buf.as_mut_ptr()) != 0 {
            failed = true;
            continue;
        }
        match check(&buf[..]) {
            Some((seq, u)) => {
                return Ok(Loaded {
                    copy: c as u32,
                    seq,
                    used: u as u32,
                    damaged,
                })
            }
            None => damaged += 1,
        }
    }
    init(&mut buf[..], 0);
    if failed {
        return Err(E_READ);
    }
    Ok(Loaded {
        copy: NONE,
        seq: 0,
        used: HEADER as u32,
        damaged,
    })
}

/// Write the record in `buf` (sealed: its `seq` and crc32 are final) to copy `copy` (0 or 1): the sectors it needs, not the whole area.
pub unsafe fn write(io: &Io, lay: &Layout, copy: u32, buf: &[u8; AREA]) -> i32 {
    let u = used(&buf[..]);
    if u < HEADER || u > AREA {
        return E_BAD_STORE;
    }
    let sector = if copy == 0 { lay.set_a() } else { lay.set_b() };
    let sectors = (u as u32 + 511) / 512;
    if (io.wr)(sector, sectors * 512, buf.as_ptr() as *mut u8) != 0 {
        return E_WRITE;
    }
    OK
}

/// How a host reader decodes the value of `key` (`u8`, `route` or `u8x16`, from `KEYS`); `None` for a key that no owner's prefix covers.
pub fn kind_of(key: &[u8]) -> Option<&'static str> {
    let mut i = 0;
    while i < KEYS.len() {
        let p = KEYS[i].0.as_bytes();
        if key.len() > p.len() && same(&key[..p.len()], p) {
            return Some(KEYS[i].1);
        }
        i += 1;
    }
    None
}

#[cfg(test)]
mod tests;
