//! The header table of a `.dkm` (docs/plans/runtime-and-services.md R7, docs/plans/sdk.md): the header pair `H_KEYS_OFF`/`H_KEYS_SIZE` locates it.
//! A versioned, tagged table that the packer writes and the runtime, the planner, the menu and the host tools read. Big-endian, 4-byte aligned:
//!
//! ```text
//! +0  u32 magic "DKTB"
//! +4  u16 version (1 or 2)
//! +6  u16 n_entries
//! +8  entries, in ascending tag order, each tag at most once:
//!       u16 tag, u16 count, u32 payload_bytes (a multiple of 4), the payload
//! ```
//!
//! The writer picks the version by content: 2 if and only if the table has `PROVIDES` or `BIND` items, else 1. A reader accepts both. `PROVIDES`
//! and `BIND` are valid only in a version 2 table, and the `SLOTS` item has 12 bytes there (8 in version 1).
//!
//! Entries (the payload of each of `count` items):
//! - `NEEDS` (12 bytes): u32 service name hash (`name_hash`), u32 minimum version (major << 16 | minor << 8 | patch), u32 flags (bit 0: hard).
//! - `USES` (8): u32 resource id (`[records.resources]`), u32 count the module takes of it.
//! - `KEYS` (`count` groups of u32 kind, u32 n, then n u32 keys): the identities the module provides for a record kind (machine keys for
//!   `KIND_MACHINE`, effect ids for `KIND_EFFECT`, a shape id for a service's kind): one owner per (kind, key).
//! - `KINDS` (12): u32 kind, u32 record length, u32 handler entry (image offset): the record kinds a service module owns.
//! - `SLOTS` (8, or 12 in version 2): u32 slot id, u32 flags, and in version 2 u32 entry (image offset): the plug-in points the module takes
//!   (one owner each).
//! - `HOOKS` (8): u32 hook site, u32 entry (image offset): the hook sites the module takes (one owner each).
//! - `EXPORTS` (8): u32 name hash, u32 entry (image offset): the functions the module offers to other modules.
//! - `RESOURCES` (8): u32 resource id, u32 limit: the resources a service module adds.
//! - `PROVIDES` (8, version 2): u32 service name hash, u32 version: the services the module provides.
//! - `BIND` (`count` items of u32 site hash, u32 firmware address, u32 stock length, u32 trampoline offset (image offset), then the stock
//!   bytes padded to 4; at most 64 stock bytes): the firmware sites a service is bound to. The entry is used up exactly.
//!
//! The packer always writes a table (a module that needs and keeps nothing has the 8-byte head). A file with no table or another magic is the
//! format of runtime 0.7: the loader refuses it with `E_CONTRACT`.

use crate::MAX_RECORDS;

/// Header fields: offset of the table and its size in bytes.
pub const H_KEYS_OFF: u32 = 0x90;
pub const H_KEYS_SIZE: u32 = 0x94;

/// "DKTB" (`[limits.format] table_magic`).
pub const MAGIC: u32 = crate::TABLE_MAGIC;
/// The newest table version a reader accepts (`[limits.format] table_version`; a change of this table is a MINOR of the runtime). Versions
/// `1..=VERSION` are valid.
pub const VERSION: u16 = crate::TABLE_VERSION as u16;
/// Bytes of the table head and of an entry head.
pub const HEAD: usize = 8;
pub const ENTRY_HEAD: usize = 8;

/// The entry tags.
pub mod tag {
    pub const NEEDS: u16 = 1;
    pub const USES: u16 = 2;
    pub const KEYS: u16 = 3;
    pub const KINDS: u16 = 4;
    pub const SLOTS: u16 = 5;
    pub const HOOKS: u16 = 6;
    pub const EXPORTS: u16 = 7;
    pub const RESOURCES: u16 = 8;
    pub const PROVIDES: u16 = 9;
    pub const BIND: u16 = 10;
}

/// Most stock bytes of one `BIND` item.
pub const BIND_STOCK_MAX: usize = 64;
/// Bytes of the fixed part of one `BIND` item.
pub const BIND_HEAD: usize = 16;

/// Bytes of one item of an entry with a fixed item size in a version 1 table; `None` for `KEYS` and `BIND`, whose items vary.
pub fn item_bytes(t: u16) -> Option<usize> {
    item_bytes_v(t, 1)
}

/// Bytes of one item of an entry with a fixed item size in a table of `version`; `None` for `KEYS` and `BIND`.
pub fn item_bytes_v(t: u16, version: u16) -> Option<usize> {
    if t == tag::NEEDS || t == tag::KINDS || (t == tag::SLOTS && version >= 2) {
        Some(12)
    } else if t == tag::KEYS || t == tag::BIND {
        None
    } else {
        Some(8)
    }
}

/// The 32-bit name hash of a service or an export (FNV-1a), the identity in `NEEDS` and `EXPORTS`.
pub fn name_hash(s: &str) -> u32 {
    name_hash_bytes(s.as_bytes())
}

/// [`name_hash`] of the bytes of a name.
pub fn name_hash_bytes(s: &[u8]) -> u32 {
    let mut h: u32 = 0x811c_9dc5;
    for b in s {
        h ^= u32::from(*b);
        h = h.wrapping_mul(0x0100_0193);
    }
    h
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KeysError {
    /// The size does not match the entries, or the buffer is too small.
    Size,
    /// Not a table of this format (no table, another magic or version).
    Format,
    /// An entry tag is unknown, out of order or twice.
    Tag,
    /// A count is above the limit.
    Count,
}
// A version 1 table cannot hold a slot with a non-zero entry: `write_table` refuses it with `Format`.

/// One `NEEDS` item: the service (`name_hash`), the version it needs at least, hard (the module cannot load without it) or optional.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Need {
    pub service: u32,
    pub min_version: u32,
    pub hard: bool,
}
/// One `USES` item.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Use {
    pub resource: u32,
    pub count: u32,
}
/// One group of `KEYS`: the keys the module provides for record kind `kind`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct KeyGroup<'a> {
    pub kind: u32,
    pub keys: &'a [u32],
}
/// One `KINDS` item.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct KindDecl {
    pub kind: u32,
    pub len: u32,
    pub entry: u32,
}
/// One `SLOTS`, `HOOKS`, `EXPORTS` or `RESOURCES` item: two words (the meaning is the entry's).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Pair {
    pub a: u32,
    pub b: u32,
}

/// One `SLOTS` item: the slot id, its flags and (version 2 only) the entry (image offset; 0 in a version 1 table).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Slot {
    pub id: u32,
    pub flags: u32,
    pub entry: u32,
}

/// One `BIND` item as the packer writes it: the site (name hash), its firmware address, the offset of the trampoline in the image and the stock
/// bytes of the site (at most [`BIND_STOCK_MAX`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Bind<'a> {
    pub site: u32,
    pub addr: u32,
    pub tramp: u32,
    pub stock: &'a [u8],
}
/// One `BIND` item as read from a checked table.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BindRef<'a> {
    pub site: u32,
    pub addr: u32,
    pub len: u32,
    pub tramp: u32,
    pub stock: &'a [u8],
}

/// Bytes of the `BIND` item with `n` stock bytes.
fn bind_item_bytes(n: usize) -> usize {
    BIND_HEAD + n.div_ceil(4) * 4
}

/// What the packer writes: every list may be empty.
#[derive(Clone, Copy, Debug, Default)]
pub struct Table<'a> {
    pub needs: &'a [Need],
    pub uses: &'a [Use],
    pub keys: &'a [KeyGroup<'a>],
    pub kinds: &'a [KindDecl],
    pub slots: &'a [Slot],
    pub hooks: &'a [Pair],
    pub exports: &'a [Pair],
    pub resources: &'a [Pair],
    /// (service name hash, version): a table with any is written as version 2.
    pub provides: &'a [Pair],
    /// A table with any is written as version 2.
    pub bind: &'a [Bind<'a>],
}

impl Table<'_> {
    /// The version `write_table` writes: 2 if and only if the table has `PROVIDES` or `BIND` items.
    pub fn version(&self) -> u16 {
        if self.provides.is_empty() && self.bind.is_empty() {
            1
        } else {
            2
        }
    }

    /// (tag, items, payload bytes) of each entry, in tag order.
    fn entries(&self) -> [(u16, usize, usize); 10] {
        let kb: usize = self.keys.iter().map(|g| 8 + 4 * g.keys.len()).sum();
        let bb: usize = self
            .bind
            .iter()
            .map(|b| bind_item_bytes(b.stock.len()))
            .sum();
        let sw = if self.version() >= 2 { 12 } else { 8 };
        [
            (tag::NEEDS, self.needs.len(), 12 * self.needs.len()),
            (tag::USES, self.uses.len(), 8 * self.uses.len()),
            (tag::KEYS, self.keys.len(), kb),
            (tag::KINDS, self.kinds.len(), 12 * self.kinds.len()),
            (tag::SLOTS, self.slots.len(), sw * self.slots.len()),
            (tag::HOOKS, self.hooks.len(), 8 * self.hooks.len()),
            (tag::EXPORTS, self.exports.len(), 8 * self.exports.len()),
            (
                tag::RESOURCES,
                self.resources.len(),
                8 * self.resources.len(),
            ),
            (tag::PROVIDES, self.provides.len(), 8 * self.provides.len()),
            (tag::BIND, self.bind.len(), bb),
        ]
    }
}

/// Size of the table `t` writes: the head and one entry for each non-empty list.
pub fn table_size(t: &Table) -> usize {
    HEAD + t
        .entries()
        .iter()
        .filter(|e| e.1 != 0)
        .map(|e| ENTRY_HEAD + e.2)
        .sum::<usize>()
}

fn put32(out: &mut [u8], at: &mut usize, v: u32) {
    out[*at..*at + 4].copy_from_slice(&v.to_be_bytes());
    *at += 4;
}

/// Writes the table to `out` and returns its size. A key group is checked against `MAX_RECORDS` and a bind item against `BIND_STOCK_MAX`.
/// A slot with a non-zero entry in a table that is written as version 1 is `Format`.
pub fn write_table(t: &Table, out: &mut [u8]) -> Result<usize, KeysError> {
    if t.keys.iter().any(|g| g.keys.len() > MAX_RECORDS as usize)
        || t.bind.iter().any(|b| b.stock.len() > BIND_STOCK_MAX)
    {
        return Err(KeysError::Count);
    }
    let version = t.version();
    if version < 2 && t.slots.iter().any(|s| s.entry != 0) {
        return Err(KeysError::Format);
    }
    let size = table_size(t);
    if size > out.len() {
        return Err(KeysError::Size);
    }
    out[..size].fill(0);
    let ents = t.entries();
    let n = ents.iter().filter(|e| e.1 != 0).count();
    let mut at = 0;
    put32(out, &mut at, MAGIC);
    out[at..at + 2].copy_from_slice(&version.to_be_bytes());
    out[at + 2..at + 4].copy_from_slice(&(n as u16).to_be_bytes());
    at += 4;
    for (tg, count, bytes) in ents {
        if count == 0 {
            continue;
        }
        out[at..at + 2].copy_from_slice(&tg.to_be_bytes());
        out[at + 2..at + 4].copy_from_slice(&(count as u16).to_be_bytes());
        at += 4;
        put32(out, &mut at, bytes as u32);
        if tg == tag::NEEDS {
            for x in t.needs {
                put32(out, &mut at, x.service);
                put32(out, &mut at, x.min_version);
                put32(out, &mut at, u32::from(x.hard));
            }
        } else if tg == tag::USES {
            for x in t.uses {
                put32(out, &mut at, x.resource);
                put32(out, &mut at, x.count);
            }
        } else if tg == tag::KEYS {
            for g in t.keys {
                put32(out, &mut at, g.kind);
                put32(out, &mut at, g.keys.len() as u32);
                for k in g.keys {
                    put32(out, &mut at, *k);
                }
            }
        } else if tg == tag::KINDS {
            for x in t.kinds {
                put32(out, &mut at, x.kind);
                put32(out, &mut at, x.len);
                put32(out, &mut at, x.entry);
            }
        } else if tg == tag::SLOTS {
            for x in t.slots {
                put32(out, &mut at, x.id);
                put32(out, &mut at, x.flags);
                if version >= 2 {
                    put32(out, &mut at, x.entry);
                }
            }
        } else if tg == tag::BIND {
            for x in t.bind {
                put32(out, &mut at, x.site);
                put32(out, &mut at, x.addr);
                put32(out, &mut at, x.stock.len() as u32);
                put32(out, &mut at, x.tramp);
                out[at..at + x.stock.len()].copy_from_slice(x.stock);
                at += x.stock.len().div_ceil(4) * 4;
            }
        } else {
            let list = if tg == tag::HOOKS {
                t.hooks
            } else if tg == tag::EXPORTS {
                t.exports
            } else if tg == tag::PROVIDES {
                t.provides
            } else {
                t.resources
            };
            for x in list {
                put32(out, &mut at, x.a);
                put32(out, &mut at, x.b);
            }
        }
    }
    debug_assert_eq!(at, size);
    Ok(size)
}

fn u16_at(b: &[u8], at: usize) -> u16 {
    u16::from_be_bytes([b[at], b[at + 1]])
}

fn u32_at(b: &[u8], at: usize) -> u32 {
    u32::from_be_bytes([b[at], b[at + 1], b[at + 2], b[at + 3]])
}

/// A checked view of a header table. Every entry was checked on `parse`: its tag is known and in order, its payload is inside the table and
/// has the size its count gives, a key group is inside its entry.
#[derive(Clone, Copy, Debug)]
pub struct Keys<'a> {
    bytes: &'a [u8],
}

/// The payload of the entry `t` in `bytes` (a table checked by `parse`): (items, payload bytes); `None` when the table has no such entry.
fn entry(bytes: &[u8], t: u16) -> Option<(usize, &[u8])> {
    let n = u16_at(bytes, 6) as usize;
    let mut at = HEAD;
    for _ in 0..n {
        let tg = u16_at(bytes, at);
        let count = u16_at(bytes, at + 2) as usize;
        let len = u32_at(bytes, at + 4) as usize;
        if tg == t {
            return Some((count, &bytes[at + ENTRY_HEAD..at + ENTRY_HEAD + len]));
        }
        at += ENTRY_HEAD + len;
    }
    None
}

impl<'a> Keys<'a> {
    /// `bytes` is exactly the table (`H_KEYS_SIZE` bytes from `H_KEYS_OFF`). Size 0, another magic or a version other than `1..=VERSION` is
    /// `Format`: the file is from an older or a newer packer. `PROVIDES` and `BIND` in a version 1 table are `Tag`.
    pub fn parse(bytes: &'a [u8]) -> Result<Keys<'a>, KeysError> {
        if bytes.len() < HEAD
            || !bytes.len().is_multiple_of(4)
            || u32_at(bytes, 0) != MAGIC
            || !(1..=VERSION).contains(&u16_at(bytes, 4))
        {
            return Err(KeysError::Format);
        }
        let version = u16_at(bytes, 4);
        let n = u16_at(bytes, 6) as usize;
        let mut at = HEAD;
        let mut last = 0u16;
        for _ in 0..n {
            if bytes.len() - at < ENTRY_HEAD {
                return Err(KeysError::Size);
            }
            let tg = u16_at(bytes, at);
            let count = u16_at(bytes, at + 2) as usize;
            let len = u32_at(bytes, at + 4) as usize;
            let top = if version >= 2 {
                tag::BIND
            } else {
                tag::RESOURCES
            };
            if !(tag::NEEDS..=top).contains(&tg) || tg <= last {
                return Err(KeysError::Tag);
            }
            last = tg;
            if !len.is_multiple_of(4) || len > bytes.len() - at - ENTRY_HEAD {
                return Err(KeysError::Size);
            }
            let body = &bytes[at + ENTRY_HEAD..at + ENTRY_HEAD + len];
            match item_bytes_v(tg, version) {
                Some(w) => {
                    if count.checked_mul(w) != Some(len) {
                        return Err(KeysError::Size);
                    }
                }
                None if tg == tag::BIND => {
                    // the bind items: each (head, stock bytes padded to 4) inside the entry, the entry used up exactly
                    let mut g = 0;
                    for _ in 0..count {
                        if len - g < BIND_HEAD {
                            return Err(KeysError::Size);
                        }
                        let sl = u32_at(body, g + 8) as usize;
                        if sl > BIND_STOCK_MAX {
                            return Err(KeysError::Count);
                        }
                        g += bind_item_bytes(sl);
                        if g > len {
                            return Err(KeysError::Size);
                        }
                    }
                    if g != len {
                        return Err(KeysError::Size);
                    }
                }
                None => {
                    // the key groups: each (kind, n, keys) inside the entry, the entry used up exactly
                    let mut g = 0;
                    for _ in 0..count {
                        if len - g < 8 {
                            return Err(KeysError::Size);
                        }
                        let nk = u32_at(body, g + 4) as usize;
                        if nk > MAX_RECORDS as usize {
                            return Err(KeysError::Count);
                        }
                        g += 8 + 4 * nk;
                        if g > len {
                            return Err(KeysError::Size);
                        }
                    }
                    if g != len {
                        return Err(KeysError::Size);
                    }
                }
            }
            at += ENTRY_HEAD + len;
        }
        if at != bytes.len() {
            return Err(KeysError::Size);
        }
        Ok(Keys { bytes })
    }

    /// How many entries the table has.
    pub fn n_entries(&self) -> usize {
        u16_at(self.bytes, 6) as usize
    }

    /// The version of the table (1 or 2).
    pub fn version(&self) -> u16 {
        u16_at(self.bytes, 4)
    }

    // The readers below are plain index loops, not iterator chains: the runtime and the boot planner call them, and each distinct iterator type is code of
    // its own on the ColdFire. The iterator forms for host tools follow them.

    /// Items of entry `t` (key groups for `KEYS`); 0 when the table has none.
    pub fn count(&self, t: u16) -> usize {
        match entry(self.bytes, t) {
            Some((n, _)) => n,
            None => 0,
        }
    }

    /// Item `i` of the fixed-size entry `t` as `(first, second, third)` words (0 for a missing word, so the entry of a version 1 slot is 0);
    /// `None` outside the entry.
    pub fn words(&self, t: u16, i: usize) -> Option<(u32, u32, u32)> {
        let w = item_bytes_v(t, self.version())?;
        let (n, body) = entry(self.bytes, t)?;
        if i >= n {
            return None;
        }
        let at = w * i;
        Some((
            u32_at(body, at),
            u32_at(body, at + 4),
            if w == 12 { u32_at(body, at + 8) } else { 0 },
        ))
    }

    /// Key group `g`: its record kind and its keys.
    pub fn group(&self, g: usize) -> Option<(u32, KeyList<'a>)> {
        let (n, body) = entry(self.bytes, tag::KEYS)?;
        if g >= n {
            return None;
        }
        let mut at = 0usize;
        let mut k = 0;
        while k < g {
            at += 8 + 4 * u32_at(body, at + 4) as usize;
            k += 1;
        }
        let nk = u32_at(body, at + 4) as usize;
        Some((
            u32_at(body, at),
            KeyList {
                bytes: &body[at + 8..at + 8 + 4 * nk],
            },
        ))
    }

    /// Whether the module provides key `key` for record kind `kind`.
    pub fn has_key(&self, kind: u32, key: u32) -> bool {
        let mut g = 0;
        while let Some((k, list)) = self.group(g) {
            if k == kind {
                let mut j = 0;
                while j < list.len() {
                    if list.get(j) == key {
                        return true;
                    }
                    j += 1;
                }
            }
            g += 1;
        }
        false
    }

    pub fn needs(&self) -> impl Iterator<Item = Need> + 'a {
        let me = *self;
        (0..self.count(tag::NEEDS)).filter_map(move |i| {
            me.words(tag::NEEDS, i).map(|w| Need {
                service: w.0,
                min_version: w.1,
                hard: w.2 & 1 != 0,
            })
        })
    }

    pub fn uses(&self) -> impl Iterator<Item = Use> + 'a {
        let me = *self;
        (0..self.count(tag::USES)).filter_map(move |i| {
            me.words(tag::USES, i).map(|w| Use {
                resource: w.0,
                count: w.1,
            })
        })
    }

    /// What the module takes of resource `id` (0 when it lists none; a resource listed twice adds up).
    pub fn use_of(&self, id: u32) -> u32 {
        let mut sum = 0u32;
        let mut i = 0;
        while let Some(w) = self.words(tag::USES, i) {
            if w.0 == id {
                sum = sum.saturating_add(w.1);
            }
            i += 1;
        }
        sum
    }

    pub fn machine_rows(&self) -> u32 {
        self.use_of(crate::RES_MACHINE_ROWS)
    }

    pub fn param_rows(&self) -> u32 {
        self.use_of(crate::RES_PARAM_ROWS)
    }

    pub fn page_rows(&self) -> u32 {
        self.use_of(crate::RES_PAGE_ROWS)
    }

    /// The payload bytes of the record block the module keeps in the arena after `init` (0: it declares nothing).
    pub fn record_bytes(&self) -> u32 {
        self.use_of(crate::RES_RECORD_BYTES)
    }

    /// The DSP instructions per block of one voice of the module's machines (0: not measured).
    pub fn dsp_cost_voice(&self) -> u32 {
        self.use_of(crate::RES_DSP_COST_VOICE)
    }

    /// The DSP instructions per block of the module's effects, summed (0: not measured).
    pub fn dsp_cost_fx(&self) -> u32 {
        self.use_of(crate::RES_DSP_COST_FX)
    }

    /// The key groups, in file order: (record kind, its keys).
    pub fn key_groups(&self) -> impl Iterator<Item = (u32, impl Iterator<Item = u32> + 'a)> + 'a {
        let me = *self;
        (0..self.count(tag::KEYS))
            .filter_map(move |g| me.group(g))
            .map(|(kind, l)| (kind, l.iter()))
    }

    /// The keys the module provides for record kind `kind` (all groups of that kind).
    pub fn keys_of(&self, kind: u32) -> impl Iterator<Item = u32> + 'a {
        self.key_groups()
            .filter(move |g| g.0 == kind)
            .flat_map(|g| g.1)
    }

    pub fn n_machines(&self) -> usize {
        self.keys_of(crate::KIND_MACHINE).count()
    }

    pub fn n_fx(&self) -> usize {
        self.keys_of(crate::KIND_EFFECT).count()
    }

    pub fn machine_keys(&self) -> impl Iterator<Item = u32> + 'a {
        self.keys_of(crate::KIND_MACHINE)
    }

    pub fn fx_keys(&self) -> impl Iterator<Item = u32> + 'a {
        self.keys_of(crate::KIND_EFFECT)
    }

    pub fn kinds(&self) -> impl Iterator<Item = KindDecl> + 'a {
        let me = *self;
        (0..self.count(tag::KINDS)).filter_map(move |i| {
            me.words(tag::KINDS, i).map(|w| KindDecl {
                kind: w.0,
                len: w.1,
                entry: w.2,
            })
        })
    }

    fn pairs(&self, t: u16) -> impl Iterator<Item = Pair> + 'a {
        let me = *self;
        (0..self.count(t)).filter_map(move |i| me.words(t, i).map(|w| Pair { a: w.0, b: w.1 }))
    }

    pub fn slots(&self) -> impl Iterator<Item = Slot> + 'a {
        let me = *self;
        (0..self.count(tag::SLOTS)).filter_map(move |i| {
            me.words(tag::SLOTS, i).map(|w| Slot {
                id: w.0,
                flags: w.1,
                entry: w.2,
            })
        })
    }

    pub fn hooks(&self) -> impl Iterator<Item = Pair> + 'a {
        self.pairs(tag::HOOKS)
    }

    pub fn exports(&self) -> impl Iterator<Item = Pair> + 'a {
        self.pairs(tag::EXPORTS)
    }

    pub fn resources(&self) -> impl Iterator<Item = Pair> + 'a {
        self.pairs(tag::RESOURCES)
    }

    /// The services the module provides: (name hash, version).
    pub fn provides(&self) -> impl Iterator<Item = (u32, u32)> + 'a {
        self.pairs(tag::PROVIDES).map(|p| (p.a, p.b))
    }

    /// How many `BIND` items the table has.
    pub fn bind_count(&self) -> usize {
        self.count(tag::BIND)
    }

    /// `BIND` item `i`; `None` outside the entry.
    pub fn bind(&self, i: usize) -> Option<BindRef<'a>> {
        let (n, body) = entry(self.bytes, tag::BIND)?;
        if i >= n {
            return None;
        }
        let mut at = 0usize;
        let mut k = 0;
        while k < i {
            at += bind_item_bytes(u32_at(body, at + 8) as usize);
            k += 1;
        }
        let len = u32_at(body, at + 8);
        Some(BindRef {
            site: u32_at(body, at),
            addr: u32_at(body, at + 4),
            len,
            tramp: u32_at(body, at + 12),
            stock: &body[at + BIND_HEAD..at + BIND_HEAD + len as usize],
        })
    }
}

/// The keys of one group, as the bytes of the table.
#[derive(Clone, Copy, Debug)]
pub struct KeyList<'a> {
    bytes: &'a [u8],
}

impl<'a> KeyList<'a> {
    pub fn len(&self) -> usize {
        self.bytes.len() / 4
    }
    pub fn is_empty(&self) -> bool {
        self.bytes.is_empty()
    }
    pub fn get(&self, i: usize) -> u32 {
        u32_at(self.bytes, 4 * i)
    }
    pub fn iter(&self) -> impl Iterator<Item = u32> + 'a {
        let me = *self;
        (0..me.len()).map(move |i| me.get(i))
    }
}

/// Writes the table of a module that declares machines and effects (and nothing else of R12): the uses of the three row kinds and the record
/// bytes (a zero count is left out), and the machine and effect keys. The packer's common case.
pub fn write_keys(
    rows: (u32, u32, u32),
    record_bytes: u32,
    machine_keys: &[u32],
    fx_keys: &[u32],
    out: &mut [u8],
) -> Result<usize, KeysError> {
    let mut uses = [Use {
        resource: 0,
        count: 0,
    }; 4];
    let mut n = 0;
    for (resource, count) in [
        (crate::RES_MACHINE_ROWS, rows.0),
        (crate::RES_PARAM_ROWS, rows.1),
        (crate::RES_PAGE_ROWS, rows.2),
        (crate::RES_RECORD_BYTES, record_bytes),
    ] {
        if count != 0 {
            uses[n] = Use { resource, count };
            n += 1;
        }
    }
    let mut groups = [KeyGroup { kind: 0, keys: &[] }; 2];
    let mut g = 0;
    if !machine_keys.is_empty() {
        groups[g] = KeyGroup {
            kind: crate::KIND_MACHINE,
            keys: machine_keys,
        };
        g += 1;
    }
    if !fx_keys.is_empty() {
        groups[g] = KeyGroup {
            kind: crate::KIND_EFFECT,
            keys: fx_keys,
        };
        g += 1;
    }
    write_table(
        &Table {
            uses: &uses[..n],
            keys: &groups[..g],
            ..Table::default()
        },
        out,
    )
}

/// Size of the table `write_keys` writes.
pub fn keys_size(
    rows: (u32, u32, u32),
    record_bytes: u32,
    n_machines: usize,
    n_fx: usize,
) -> usize {
    let uses = [rows.0, rows.1, rows.2, record_bytes]
        .iter()
        .filter(|c| **c != 0)
        .count();
    let mut size = HEAD;
    if uses != 0 {
        size += ENTRY_HEAD + 8 * uses;
    }
    if n_machines + n_fx != 0 {
        size += ENTRY_HEAD; // one KEYS entry, one group for each kind that has keys
    }
    if n_machines != 0 {
        size += 8 + 4 * n_machines;
    }
    if n_fx != 0 {
        size += 8 + 4 * n_fx;
    }
    size
}

#[cfg(test)]
mod tests {
    extern crate std;
    use super::*;
    use crate::{KIND_MACHINE, RES_DSP_COST_FX, RES_DSP_COST_VOICE, RES_MACHINE_ROWS};
    use std::{vec, vec::Vec};

    #[test]
    fn a_table_of_every_entry_round_trips() {
        let k1 = [0x11223344u32, 5];
        let k2 = [0xdeadbeefu32];
        let t = Table {
            needs: &[
                Need {
                    service: name_hash("core/ui"),
                    min_version: 0x000800,
                    hard: true,
                },
                Need {
                    service: 7,
                    min_version: 0x000801,
                    hard: false,
                },
            ],
            uses: &[
                Use {
                    resource: RES_MACHINE_ROWS,
                    count: 2,
                },
                Use {
                    resource: RES_DSP_COST_VOICE,
                    count: 9,
                },
                Use {
                    resource: RES_DSP_COST_FX,
                    count: 11,
                },
            ],
            keys: &[
                KeyGroup {
                    kind: KIND_MACHINE,
                    keys: &k1,
                },
                KeyGroup {
                    kind: 99,
                    keys: &k2,
                },
            ],
            kinds: &[KindDecl {
                kind: 90,
                len: 24,
                entry: 0x100,
            }],
            slots: &[Slot {
                id: 1,
                flags: 2,
                entry: 0,
            }],
            hooks: &[Pair { a: 3, b: 4 }, Pair { a: 5, b: 6 }],
            exports: &[Pair {
                a: name_hash("lfo_set"),
                b: 0x200,
            }],
            resources: &[Pair { a: 40, b: 8 }],
            ..Table::default()
        };
        let mut out = [0xaau8; 512];
        let n = write_table(&t, &mut out).unwrap();
        assert_eq!(n, table_size(&t));
        assert_eq!(n % 4, 0);
        let k = Keys::parse(&out[..n]).unwrap();
        assert_eq!(k.version(), 1);
        assert_eq!(k.n_entries(), 8);
        assert!(k.needs().eq(t.needs.iter().copied()));
        assert!(k.uses().eq(t.uses.iter().copied()));
        assert_eq!(k.use_of(RES_MACHINE_ROWS), 2);
        assert_eq!((k.dsp_cost_voice(), k.dsp_cost_fx()), (9, 11));
        assert!(k.machine_keys().eq(k1));
        assert!(k.keys_of(99).eq(k2));
        assert!(k.keys_of(1234).next().is_none());
        assert!(k.kinds().eq(t.kinds.iter().copied()));
        assert!(k.slots().eq(t.slots.iter().copied()));
        assert!(k.hooks().eq(t.hooks.iter().copied()));
        assert!(k.exports().eq(t.exports.iter().copied()));
        assert!(k.resources().eq(t.resources.iter().copied()));
    }

    fn v2_table<'a>(binds: &'a [Bind<'a>]) -> Table<'a> {
        Table {
            needs: &[Need {
                service: 7,
                min_version: 0x000800,
                hard: true,
            }],
            slots: &[
                Slot {
                    id: 1,
                    flags: 2,
                    entry: 0x300,
                },
                Slot {
                    id: 4,
                    flags: 0,
                    entry: 0,
                },
            ],
            hooks: &[Pair { a: 3, b: 4 }],
            provides: &[Pair {
                a: 0x1111,
                b: 0x000801,
            }],
            bind: binds,
            ..Table::default()
        }
    }

    #[test]
    fn a_version_2_table_of_every_entry_round_trips() {
        let stock = [1u8, 2, 3, 4, 5, 6];
        let binds = [
            Bind {
                site: 5,
                addr: 0x1234,
                tramp: 0x20,
                stock: &stock,
            },
            Bind {
                site: 6,
                addr: 0x5678,
                tramp: 0x40,
                stock: &[],
            },
        ];
        let t = v2_table(&binds);
        assert_eq!(t.version(), 2);
        let mut out = [0xaau8; 512];
        let n = write_table(&t, &mut out).unwrap();
        assert_eq!(n, table_size(&t));
        assert_eq!(n % 4, 0);
        let k = Keys::parse(&out[..n]).unwrap();
        assert_eq!(k.version(), 2);
        assert!(k.needs().eq(t.needs.iter().copied()));
        assert!(k.slots().eq(t.slots.iter().copied()));
        assert!(k.hooks().eq(t.hooks.iter().copied()));
        assert!(k.provides().eq([(0x1111, 0x000801)]));
        assert_eq!(k.bind_count(), 2);
        let b0 = k.bind(0).unwrap();
        assert_eq!((b0.site, b0.addr, b0.len, b0.tramp), (5, 0x1234, 6, 0x20));
        assert_eq!(b0.stock, &stock);
        let b1 = k.bind(1).unwrap();
        assert_eq!((b1.site, b1.addr, b1.len, b1.tramp), (6, 0x5678, 0, 0x40));
        assert!(b1.stock.is_empty());
        assert!(k.bind(2).is_none());
    }

    #[test]
    fn a_table_without_provides_and_bind_is_written_as_version_1_with_8_byte_slots() {
        let t = Table {
            slots: &[Slot {
                id: 1,
                flags: 2,
                entry: 0,
            }],
            ..Table::default()
        };
        let mut out = [0u8; 64];
        let n = write_table(&t, &mut out).unwrap();
        assert_eq!(n, HEAD + ENTRY_HEAD + 8);
        assert_eq!(&out[..8], &[b'D', b'K', b'T', b'B', 0, 1, 0, 1]);
        assert_eq!(&out[HEAD + ENTRY_HEAD..n], &[0, 0, 0, 1, 0, 0, 0, 2]);
        let k = Keys::parse(&out[..n]).unwrap();
        assert_eq!(k.version(), 1);
        assert!(k.slots().eq(t.slots.iter().copied()));
    }

    #[test]
    fn a_table_with_provides_is_written_as_version_2_with_12_byte_slots() {
        let t = Table {
            slots: &[Slot {
                id: 1,
                flags: 2,
                entry: 0x300,
            }],
            provides: &[Pair { a: 9, b: 1 }],
            ..Table::default()
        };
        let mut out = [0u8; 64];
        let n = write_table(&t, &mut out).unwrap();
        // head, SLOTS (head + 12), PROVIDES (head + 8)
        assert_eq!(n, HEAD + ENTRY_HEAD + 12 + ENTRY_HEAD + 8);
        assert_eq!(&out[4..6], &[0, 2]);
        assert_eq!(
            &out[HEAD + ENTRY_HEAD..HEAD + ENTRY_HEAD + 12],
            &[0, 0, 0, 1, 0, 0, 0, 2, 0, 0, 3, 0]
        );
        assert!(Keys::parse(&out[..n])
            .unwrap()
            .slots()
            .eq(t.slots.iter().copied()));
    }

    #[test]
    fn a_slot_entry_in_a_version_1_table_is_refused_at_write() {
        let t = Table {
            slots: &[Slot {
                id: 1,
                flags: 0,
                entry: 8,
            }],
            ..Table::default()
        };
        let mut out = [0u8; 64];
        assert_eq!(write_table(&t, &mut out), Err(KeysError::Format));
    }

    /// A table with the entries `(tag, count, payload)` and the version `v`.
    fn raw(v: u16, ents: &[(u16, u16, &[u8])]) -> Vec<u8> {
        let mut b = Vec::new();
        b.extend_from_slice(&MAGIC.to_be_bytes());
        b.extend_from_slice(&v.to_be_bytes());
        b.extend_from_slice(&(ents.len() as u16).to_be_bytes());
        for (tg, count, body) in ents {
            b.extend_from_slice(&tg.to_be_bytes());
            b.extend_from_slice(&count.to_be_bytes());
            b.extend_from_slice(&(body.len() as u32).to_be_bytes());
            b.extend_from_slice(body);
        }
        b
    }

    #[test]
    fn provides_and_bind_in_a_version_1_table_and_other_versions_are_refused_at_parse() {
        let prov = [0u8; 8];
        assert_eq!(
            Keys::parse(&raw(1, &[(tag::PROVIDES, 1, &prov)])).unwrap_err(),
            KeysError::Tag
        );
        assert_eq!(
            Keys::parse(&raw(1, &[(tag::BIND, 0, &[])])).unwrap_err(),
            KeysError::Tag
        );
        assert!(Keys::parse(&raw(2, &[(tag::PROVIDES, 1, &prov)])).is_ok());
        assert_eq!(Keys::parse(&raw(0, &[])).unwrap_err(), KeysError::Format);
        assert_eq!(Keys::parse(&raw(3, &[])).unwrap_err(), KeysError::Format);
        // a version 1 slot item has 8 bytes, a version 2 item 12
        assert!(Keys::parse(&raw(1, &[(tag::SLOTS, 1, &[0u8; 8])])).is_ok());
        assert_eq!(
            Keys::parse(&raw(2, &[(tag::SLOTS, 1, &[0u8; 8])])).unwrap_err(),
            KeysError::Size
        );
    }

    #[test]
    fn a_bind_entry_with_leftover_or_missing_bytes_is_refused_at_parse() {
        // one item with 4 stock bytes: head 16 + 4
        let mut item = vec![0u8; 20];
        item[8..12].copy_from_slice(&4u32.to_be_bytes());
        assert!(Keys::parse(&raw(2, &[(tag::BIND, 1, &item)])).is_ok());
        let mut more = item.clone();
        more.extend_from_slice(&[0; 4]); // leftover bytes
        assert_eq!(
            Keys::parse(&raw(2, &[(tag::BIND, 1, &more)])).unwrap_err(),
            KeysError::Size
        );
        // the count says 2 items, the payload holds one
        assert_eq!(
            Keys::parse(&raw(2, &[(tag::BIND, 2, &item)])).unwrap_err(),
            KeysError::Size
        );
        // the stock length reaches past the entry
        let mut short = item.clone();
        short[8..12].copy_from_slice(&8u32.to_be_bytes());
        assert_eq!(
            Keys::parse(&raw(2, &[(tag::BIND, 1, &short)])).unwrap_err(),
            KeysError::Size
        );
        // more stock bytes than the limit
        let mut big = vec![0u8; BIND_HEAD + 68];
        big[8..12].copy_from_slice(&68u32.to_be_bytes());
        assert_eq!(
            Keys::parse(&raw(2, &[(tag::BIND, 1, &big)])).unwrap_err(),
            KeysError::Count
        );
    }

    #[test]
    fn the_machine_and_effect_writer_gives_the_accessors_the_old_table_had() {
        let mut out = [0xaau8; 128];
        let n = write_keys((2, 3, 4), 700, &[0x11223344, 5], &[0xdeadbeef], &mut out).unwrap();
        assert_eq!(n, keys_size((2, 3, 4), 700, 2, 1));
        let k = Keys::parse(&out[..n]).unwrap();
        assert_eq!((k.machine_rows(), k.param_rows(), k.page_rows()), (2, 3, 4));
        assert_eq!(k.record_bytes(), 700);
        assert!(k.machine_keys().eq([0x11223344, 5]));
        assert!(k.fx_keys().eq([0xdeadbeef]));
        assert_eq!((k.n_machines(), k.n_fx()), (2, 1));
    }

    #[test]
    fn a_module_that_needs_and_keeps_nothing_has_the_head_only() {
        let mut out = [0u8; 16];
        assert_eq!(write_keys((0, 0, 0), 0, &[], &[], &mut out), Ok(HEAD));
        let k = Keys::parse(&out[..HEAD]).unwrap();
        assert_eq!(k.n_entries(), 0);
        assert_eq!(k.machine_rows(), 0);
        assert!(k.machine_keys().next().is_none());
    }

    #[test]
    fn an_old_or_empty_table_is_the_wrong_format_and_a_damaged_one_is_refused() {
        assert_eq!(Keys::parse(&[]).unwrap_err(), KeysError::Format);
        // a runtime 0.7 table starts with the machine row count, not the magic
        let old = [0u8, 2, 0, 3, 0, 4, 0, 1, 0, 0, 0, 0, 0, 0, 0, 1];
        assert_eq!(Keys::parse(&old).unwrap_err(), KeysError::Format);
        let mut out = [0u8; 64];
        let n = write_keys((1, 0, 0), 0, &[1], &[2], &mut out).unwrap();
        assert_eq!(Keys::parse(&out[..n - 4]).unwrap_err(), KeysError::Size);
        let mut bad = out;
        bad[HEAD + 1] = 9; // an unknown tag
        assert_eq!(Keys::parse(&bad[..n]).unwrap_err(), KeysError::Tag);
        let mut more = out;
        more[6..8].copy_from_slice(&5u16.to_be_bytes()); // more entries than the bytes hold
        assert!(Keys::parse(&more[..n]).is_err());
        let mut many = out;
        // the machine key group's n: above MAX_RECORDS
        let g = HEAD + ENTRY_HEAD + 8 + ENTRY_HEAD + 4;
        many[g..g + 4].copy_from_slice(&33u32.to_be_bytes());
        assert!(Keys::parse(&many[..n]).is_err());
        assert_eq!(
            write_keys((0, 0, 0), 0, &[1], &[2], &mut out[..HEAD]),
            Err(KeysError::Size)
        );
    }
}
