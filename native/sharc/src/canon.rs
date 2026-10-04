//! The harness's canonical machine-state format (tools/sharc_diff.py,
//! module docstring: `pack_state`/`unpack_state`, STATE_FORMAT_VERSION 4).
//! Versions 1/2 remain readable with physical stacks disabled; v1 also
//! disables banking and supplies unknown shadows.
//! and the image blob tools/sharc_transpile_run.py `pack_image` writes.

use crate::CfgRefresh;
use crate::mem::Mem;
use crate::rt::*;
use crate::sha256::Sha256;
pub use sharc_rt::insn_table::{InsnTable, Rd, insn_table};

pub const STATE_FORMAT_VERSION: u32 = 4;
const PAGE: u32 = 4096;

fn put_value(out: &mut Vec<u8>, v: V) {
    let (kind, value, mask) = if v.is_c() {
        (1u8, v.b, u32::MAX)
    } else if v.is_unknown() {
        (0u8, 0, 0)
    } else {
        (2u8, v.b, v.m)
    };
    out.push(kind);
    out.extend_from_slice(&value.to_le_bytes());
    out.extend_from_slice(&mask.to_le_bytes());
}

fn put_u80(out: &mut Vec<u8>, v: Int) {
    let v = v & MR_MASK;
    for i in 0..10 {
        out.push((v >> (8 * i)) as u8);
    }
}

/// The image blob: magic "SHIM", version 1, loader segments, the
/// addresses sharcimm.name_address names (exact and ranges), and
/// encoding.CORE_MMR_RESET_VALUES.
pub struct Image {
    pub mem: Mem,
    pub named: Vec<u32>,
    pub ranges: Vec<(u32, u32)>,
    pub core_reset: Vec<u32>,
}

pub fn parse_image(b: &[u8]) -> Result<Image, i32> {
    let mut r = Rd { b, off: 0 };
    if b.is_empty() {
        return Ok(Image {
            mem: Mem::new(),
            named: Vec::new(),
            ranges: Vec::new(),
            core_reset: Vec::new(),
        });
    }
    if r.take(4)? != b"SHIM" || r.u32()? != 1 {
        return Err(-1);
    }
    let mut mem = Mem::new();
    for _ in 0..r.u32()? {
        let a = r.u32()?;
        let n = r.u32()? as usize;
        mem.load(a, r.take(n)?);
    }
    let mut named: Vec<u32> = (0..r.u32()?).map(|_| r.u32()).collect::<Result<_, _>>()?;
    named.sort_unstable();
    let ranges = (0..r.u32()?)
        .map(|_| Ok((r.u32()?, r.u32()?)))
        .collect::<Result<Vec<_>, i32>>()?;
    let core_reset = (0..r.u32()?).map(|_| r.u32()).collect::<Result<_, _>>()?;
    Ok(Image {
        mem,
        named,
        ranges,
        core_reset,
    })
}

/// sharc_diff.import_state: a fresh state (memory back to the loader
/// image, the harness's run configuration) seeded from BLOB.
pub fn import_state(s: &mut St, blob: &[u8]) -> Result<(), i32> {
    let mut r = Rd { b: blob, off: 0 };
    if r.take(4)? != b"SHRD" {
        return Err(-1);
    }
    let version = r.u32()?;
    if !matches!(version, 1 | 2 | 3 | STATE_FORMAT_VERSION) {
        return Err(-4);
    }
    s.mem.reset();
    s.pc_sw = r.u32()? as Int;
    if r.u8()? != 0 {
        r.str()?;
    }
    for code in 0..NUREG {
        s.r[code] = r.value()?;
    }
    for i in 0..7 {
        let kind = r.u8()?;
        let value = r.u80()?;
        let mask = r.u80()?;
        s.special[i] = match kind {
            2 => Spec::M(MR::new(mask, value)),
            1 => Spec::V(V::c(value)),
            _ => Spec::V(V::UNK),
        };
        s.special_present[i] = true;
    }
    s.mmrs.clear();
    for _ in 0..r.u32()? {
        let a = r.u32()?;
        let v = r.value()?;
        s.mmr_put(a, v);
    }
    s.pending = None;
    if r.u8()? != 0 {
        let target_present = r.u8()?;
        let target = r.u32()?;
        let call = r.u8()? != 0;
        let slots = r.u8()?;
        let return_from_call = r.u8()? != 0;
        let return_sw_present = r.u8()?;
        let return_sw = r.i32()?;
        s.pending = Some(Pending {
            target: (target_present != 0).then_some(target as Int),
            call,
            slots: slots as Int,
            return_from_call,
            return_sw: (return_sw_present != 0).then_some(return_sw as Int),
        });
    }
    s.loops.clear();
    for _ in 0..r.u16()? {
        let start_sw = r.u32()? as i64;
        let end_sw = r.u32()? as i64;
        let remaining = r.u32()? as i64;
        let mode = r.u32()? as i64;
        s.loops
            .push_raw(Loop {
                start_sw,
                end_sw,
                remaining,
                mode,
            })
            .map_err(|_| -5)?;
    }
    s.call_stack.clear();
    for _ in 0..r.u16()? {
        let v = r.u32()? as Int;
        s.call_stack.push_raw(v).map_err(|_| -5)?;
    }
    s.status_stack.clear();
    for _ in 0..r.u16()? {
        let t = (r.value()?, r.value()?, r.value()?);
        s.status_stack.push_raw(t).map_err(|_| -5)?;
    }
    for _ in 0..r.u32()? {
        let a = r.u32()?;
        let n = r.u32()? as usize;
        let data = r.take(n)?;
        for (k, &byte) in data.iter().enumerate() {
            // import_state writes through _dm_write(address + i, 1, byte).
            let addr = a as Int + k as Int;
            let Ok(Some(c)) = bnd::_canonical_dm_address(s, addr, 1, true) else {
                return Err(-6);
            };
            s.mem.write_byte(c as u32, byte);
        }
    }
    // Page hashes describe the source's overlay; nothing to import.
    for _ in 0..r.u32()? {
        r.u32()?;
        r.take(32)?;
    }
    s.steps = 0;
    s.at_loaded_entry = false;
    s.cfg = Cfg::default();
    s.bank_alt = [V::UNK; 96];
    s.bank_active_mask = 0;
    s.bank_pending_mask = -1;
    s.bank_requested_mask = -1;
    if version >= 2 {
        s.cfg.bank_model = r.u8()? != 0;
        s.bank_active_mask = r.i32()? as Int;
        s.bank_pending_mask = r.i32()? as Int;
        s.bank_requested_mask = r.i32()? as Int;
        for code in 0..96 {
            s.bank_alt[code] = r.value()?;
        }
    }
    s.pc_stack.clear();
    s.pc_stack_pending = -1;
    s.pc_stack_requested = -1;
    if version >= 3 {
        s.cfg.stack_model = r.u8()? != 0;
        s.pc_stack_pending = r.i32()? as Int;
        s.pc_stack_requested = r.i32()? as Int;
        let depth = r.u16()?;
        if depth > 30 {
            return Err(-5);
        }
        for _ in 0..depth {
            let entry = r.u32()? as Int;
            s.pc_stack.push_raw(entry).map_err(|_| -5)?;
        }
    }
    s.loop_depth = 0;
    s.loop_slots.n = 6;
    s.loop_slots.a = [(V::UNK, V::UNK); 6];
    if version >= 4 {
        s.loop_depth = r.u8()? as Int;
        if s.loop_depth > 6 {
            return Err(-5);
        }
        for slot in &mut s.loop_slots.a {
            *slot = (r.value()?, r.value()?);
        }
    } else if s.cfg.stack_model && !s.loops.items().is_empty() {
        // Older snapshots cannot reconstruct packed loop resource contents.
        return Err(-5);
    }
    s.cfg.refresh();
    s.sync_snapshot();
    s.check_loops();
    s.trap = None;
    Ok(())
}

/// sharc_diff.export_state + pack_state. With RANGES, every overlay byte
/// also goes out as an explicit memory range (for handing the state to
/// the Python core).
pub fn export_state(s: &St, ranges: bool) -> Vec<u8> {
    let mut out = Vec::with_capacity(4096);
    out.extend_from_slice(b"SHRD");
    out.extend_from_slice(&STATE_FORMAT_VERSION.to_le_bytes());
    out.extend_from_slice(&(s.pc_sw as u32).to_le_bytes());
    out.push(0); // never stopped: a stop traps first
    for code in 0..NUREG {
        put_value(&mut out, s.r[code]);
    }
    for i in 0..7 {
        let (kind, value, mask): (u8, Int, Int) = match (s.special_present[i], s.special[i]) {
            (true, Spec::M(m)) => (2, m.bits, m.mask),
            (true, Spec::V(v)) if v.is_c() => (1, v.val(), 0xFFFF_FFFF),
            _ => (0, 0, 0),
        };
        out.push(kind);
        put_u80(&mut out, value);
        put_u80(&mut out, mask);
    }
    out.extend_from_slice(&(s.mmrs.len() as u32).to_le_bytes());
    for &(a, v) in &s.mmrs {
        out.extend_from_slice(&a.to_le_bytes());
        put_value(&mut out, v);
    }
    match s.pending {
        None => out.push(0),
        Some(p) => {
            out.push(1);
            out.push(p.target.is_some() as u8);
            out.extend_from_slice(&(p.target.unwrap_or(0) as u32).to_le_bytes());
            out.push(p.call as u8);
            out.push(p.slots as u8);
            out.push(p.return_from_call as u8);
            out.push(p.return_sw.is_some() as u8);
            out.extend_from_slice(&(p.return_sw.unwrap_or(0) as i32).to_le_bytes());
        }
    }
    out.extend_from_slice(&(s.loops.len() as u16).to_le_bytes());
    for l in s.loops.items() {
        for v in [l.start_sw, l.end_sw, l.remaining, l.mode] {
            out.extend_from_slice(&(v as u32).to_le_bytes());
        }
    }
    out.extend_from_slice(&(s.call_stack.len() as u16).to_le_bytes());
    for &v in s.call_stack.items() {
        out.extend_from_slice(&(v as u32).to_le_bytes());
    }
    out.extend_from_slice(&(s.status_stack.len() as u16).to_le_bytes());
    for &(a, b, c) in s.status_stack.items() {
        put_value(&mut out, a);
        put_value(&mut out, b);
        put_value(&mut out, c);
    }
    let dirty = s.mem.dirty_bytes();
    if ranges {
        let mut runs: Vec<(u32, Vec<u8>)> = Vec::new();
        for &(a, v) in &dirty {
            match runs.last_mut() {
                Some((start, bytes)) if *start as u64 + bytes.len() as u64 == a as u64 => {
                    bytes.push(v)
                }
                _ => runs.push((a, vec![v])),
            }
        }
        out.extend_from_slice(&(runs.len() as u32).to_le_bytes());
        for (a, bytes) in runs {
            out.extend_from_slice(&a.to_le_bytes());
            out.extend_from_slice(&(bytes.len() as u32).to_le_bytes());
            out.extend_from_slice(&bytes);
        }
    } else {
        out.extend_from_slice(&0u32.to_le_bytes());
    }
    // Page hashes: sha256 of (address:u64 LE, value:u8) per 4096-byte page.
    let mut hashes: Vec<(u32, [u8; 32])> = Vec::new();
    let mut i = 0;
    while i < dirty.len() {
        let page = dirty[i].0 - dirty[i].0 % PAGE;
        let mut h = Sha256::new();
        while i < dirty.len() && dirty[i].0 - dirty[i].0 % PAGE == page {
            h.update(&(dirty[i].0 as u64).to_le_bytes());
            h.update(&[dirty[i].1]);
            i += 1;
        }
        hashes.push((page, h.finish()));
    }
    out.extend_from_slice(&(hashes.len() as u32).to_le_bytes());
    for (page, digest) in hashes {
        out.extend_from_slice(&page.to_le_bytes());
        out.extend_from_slice(&digest);
    }
    out.push(s.cfg.bank_model as u8);
    out.extend_from_slice(&(s.bank_active_mask as i32).to_le_bytes());
    out.extend_from_slice(&(s.bank_pending_mask as i32).to_le_bytes());
    out.extend_from_slice(&(s.bank_requested_mask as i32).to_le_bytes());
    for value in s.bank_alt {
        put_value(&mut out, value);
    }
    out.push(s.cfg.stack_model as u8);
    out.extend_from_slice(&(s.pc_stack_pending as i32).to_le_bytes());
    out.extend_from_slice(&(s.pc_stack_requested as i32).to_le_bytes());
    out.extend_from_slice(&(s.pc_stack.len() as u16).to_le_bytes());
    for &entry in s.pc_stack.items() {
        out.extend_from_slice(&(entry as u32).to_le_bytes());
    }
    out.push(s.loop_depth as u8);
    for &(address, counter) in s.loop_slots.items() {
        put_value(&mut out, address);
        put_value(&mut out, counter);
    }
    out
}

/// Run configuration: 10 explicit_memory_model, 11 approx_recips, 12
/// assume_nw32, 13 follow_loaded_calls, 14 max_call_depth, 15
/// continue_external_calls, 16 data_memory_tainted, 17 has_concrete (a
/// State with concrete=None), 18 dossier_bytes, 19 bank_model, 20
/// stack_model, 21 core_timer, 22 peripheral_model.
pub fn set_option(s: &mut St, key: u32, value: i64) -> i32 {
    let b = value != 0;
    match key {
        10 => s.cfg.explicit_memory_model = b,
        11 => s.cfg.approx_recips = b,
        12 => s.cfg.assume_nw32 = b,
        13 => s.cfg.follow_loaded_calls = b,
        14 => s.cfg.max_call_depth = value as Int,
        15 => s.cfg.continue_external_calls = b,
        16 => s.cfg.data_memory_tainted = b,
        17 => s.cfg.has_concrete = b,
        18 => s.cfg.dossier_bytes = value as Int,
        19 => s.cfg.bank_model = b,
        20 => s.cfg.stack_model = b,
        21 => s.cfg.core_timer = b,
        22 => s.cfg.peripheral_model = b,
        _ => return -1,
    }
    s.cfg.refresh();
    0
}

/// One instruction for sharc_native_exec_insn: magic "SHIN", type name,
/// length_bytes (i32, -1 for None), kind, then u16 field count and
/// (key, value:i64) pairs, strings as u16 length + UTF-8.
pub fn parse_insn(b: &[u8]) -> Result<Insn, i32> {
    let mut r = Rd { b, off: 0 };
    if r.take(4)? != b"SHIN" {
        return Err(-1);
    }
    let type_name = crate::sym_of(r.str()?).ok_or(-7)?;
    let length = r.i32()?;
    let kind = crate::sym_of(r.str()?).ok_or(-7)?;
    let n = r.u16()?;
    let mut entries = Vec::with_capacity(n as usize);
    for _ in 0..n {
        let key = r.str()?;
        let value = r.i64()? as Int;
        entries.push(field_entry(key, value).ok_or(-7)?);
    }
    if entries.len() > MAX_INSN_FIELDS {
        return Err(-8);
    }
    let mut kv = [FieldEntry(S_EMPTY, S_EMPTY, -1, -1, 0); MAX_INSN_FIELDS];
    for (dst, entry) in kv.iter_mut().zip(entries) {
        *dst = entry;
    }
    Ok(Insn {
        type_name,
        fields: Fields::from_entries(kv, n as u8),
        length_bytes: (length >= 0).then_some(length as Int),
        kind,
        offset: 0,
    })
}

/// A field entry for KEY (e.g. "data[31:16]"): its stem and bit range.
pub fn field_entry(key: &str, value: Int) -> Option<FieldEntry> {
    let k = crate::sym_of(key)?;
    let (stem, range) = match key.find('[') {
        Some(i) => (&key[..i], &key[i..]),
        None => (key, ""),
    };
    let st = crate::sym_of(stem)?;
    let (mut hi, mut lo) = (-1i8, -1i8);
    if let Some(inner) = range.strip_prefix('[').and_then(|x| x.strip_suffix(']'))
        && let Some((h, l)) = inner.split_once(':')
    {
        hi = h.parse().unwrap_or(-1);
        lo = l.parse().unwrap_or(-1);
    }
    Some(FieldEntry(k, st, hi, lo, i64::try_from(value).ok()?))
}
