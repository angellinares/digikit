//! The native core's C ABI (tools/sharc_diff.py NativeEngine,
//! tools/sharc_transpile_run.py NativeCore) over the JIT: a drop-in for
//! libsharc_native. `sharc_native_step` runs translated regions where the
//! runtime has them; `sharc_native_exec_insn` translates the one
//! instruction it is given (the compute corpus exercises the translator)
//! and falls back to the interpreter where translation or the region
//! declines.

use crate::jit::Machine;
use sharc_translate::pe::mach::DInsn;
use std::rc::Rc;
use wasmtime::TypedFunc;

pub struct Handle {
    pub m: Machine,
    /// sharc_native_exec_insn translates (else interprets).
    pub jit_exec: bool,
    pub exec_jit: u64,
    pub exec_fallback: u64,
}

fn h<'a>(p: *mut Handle) -> &'a mut Handle {
    // SAFETY: the caller passes a handle from sharc_native_create.
    unsafe { &mut *p }
}

fn slice<'a>(p: *const u8, n: usize) -> &'a [u8] {
    if p.is_null() || n == 0 {
        return &[];
    }
    // SAFETY: caller contract (N readable bytes).
    unsafe { std::slice::from_raw_parts(p, n) }
}

fn copy_out(v: &[u8], out: *mut u8, cap: usize) -> i32 {
    if v.len() > cap {
        return -(v.len() as i32);
    }
    // SAFETY: caller contract (CAP writable bytes).
    unsafe { std::ptr::copy_nonoverlapping(v.as_ptr(), out, v.len()) };
    v.len() as i32
}

/// # Safety
/// IMAGE points to IMAGE_LEN bytes (a pack_image blob).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sharc_native_create(image: *const u8, image_len: usize) -> *mut Handle {
    match Machine::new(slice(image, image_len)) {
        Ok(m) => {
            let mut hd = Handle {
                m,
                jit_exec: std::env::var("SHARC_JIT_EXEC").map(|v| v != "0").unwrap_or(true),
                exec_jit: 0,
                exec_fallback: 0,
            };
            if let Ok(t) = std::env::var("SHARC_JIT_THRESHOLD")
                && let Ok(t) = t.parse::<u32>()
            {
                let _ = hd.m.configure(t, t > 0);
            }
            Box::into_raw(Box::new(hd))
        }
        Err(e) => {
            eprintln!("sharc-jit: {e}");
            std::ptr::null_mut()
        }
    }
}

/// # Safety
/// HANDLE from sharc_native_create, not used afterwards.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sharc_native_destroy(handle: *mut Handle) {
    if !handle.is_null() {
        // SAFETY: from Box::into_raw.
        drop(unsafe { Box::from_raw(handle) });
    }
}

/// # Safety
/// HANDLE from sharc_native_create; BLOB covers BLOB_LEN bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sharc_native_import_state(handle: *mut Handle, blob: *const u8, blob_len: usize) -> i32 {
    h(handle).m.in_call("jit_import_state", slice(blob, blob_len)).unwrap_or(-100)
}

/// # Safety
/// HANDLE from sharc_native_create; OUT covers OUT_CAP bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sharc_native_export_state(handle: *mut Handle, out: *mut u8, out_cap: usize) -> i32 {
    match h(handle).m.out_call("jit_export_state", 1 << 16) {
        Ok(v) => copy_out(&v, out, out_cap),
        Err(_) => -1,
    }
}

/// # Safety
/// HANDLE from sharc_native_create.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sharc_native_step(handle: *mut Handle, n: u32) -> i32 {
    h(handle).m.step(n).unwrap_or_else(|e| {
        eprintln!("sharc-jit step: {e}");
        -1
    })
}

/// # Safety
/// HANDLE from sharc_native_create; OUT covers OUT_CAP bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sharc_native_halt_reason(handle: *mut Handle, out: *mut u8, out_cap: usize) -> i32 {
    match h(handle).m.out_call("jit_halt_reason", 1024) {
        Ok(v) => {
            let n = v.len().min(out_cap);
            copy_out(&v[..n], out, out_cap)
        }
        Err(_) => 0,
    }
}

fn cfg_option(hd: &mut Handle, key: u32, value: i64) {
    let c = &mut hd.m.store.data_mut().cfg;
    let b = value != 0;
    match key {
        10 => c.explicit_memory_model = b,
        11 => c.approx_recips = b,
        12 => c.assume_nw32 = b,
        13 => c.follow_loaded_calls = b,
        14 => c.max_call_depth = value as i128,
        15 => c.continue_external_calls = b,
        16 => c.data_memory_tainted = b,
        17 => c.has_concrete = b,
        18 => c.dossier_bytes = value as i128,
        _ => {}
    }
    c.fast_mem = c.has_concrete && c.assume_nw32 && !c.data_memory_tainted;
}

/// # Safety
/// HANDLE from sharc_native_create.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sharc_native_set_option(handle: *mut Handle, key: u32, value: i64) -> i32 {
    let hd = h(handle);
    let f: TypedFunc<(u32, i64), i32> = match hd.m.func("jit_set_option") {
        Ok(f) => f,
        Err(_) => return -1,
    };
    let rc = f.call(&mut hd.m.store, (key, value)).unwrap_or(-1);
    if rc == 0 {
        cfg_option(hd, key, value);
    }
    rc
}

/// # Safety
/// HANDLE from sharc_native_create; the pointers cover their lengths.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sharc_native_set_provisional(
    handle: *mut Handle,
    name: *const u8,
    name_len: usize,
    mode: *const u8,
    mode_len: usize,
) -> i32 {
    let hd = h(handle);
    let (n, m) = (slice(name, name_len).to_vec(), slice(mode, mode_len).to_vec());
    let run = |hd: &mut Handle| -> wasmtime::Result<i32> {
        let pn = hd.m.put(&n)?;
        let pm = hd.m.put(&m)?;
        let f: TypedFunc<(u32, u32, u32, u32), i32> = hd.m.func("jit_set_provisional")?;
        let r = f.call(&mut hd.m.store, (pn, n.len() as u32, pm, m.len() as u32))?;
        hd.m.free(pn, n.len())?;
        hd.m.free(pm, m.len())?;
        Ok(r)
    };
    let rc = run(hd).unwrap_or(-1);
    if rc == 0 {
        // Region code is translated for the default configuration only.
        hd.jit_exec = false;
    }
    rc
}

/// canon::parse_insn's blob as a decoded instruction.
fn parse_insn(b: &[u8]) -> Option<DInsn> {
    let mut o = 0usize;
    let take = |o: &mut usize, n: usize| -> Option<&[u8]> {
        let s = b.get(*o..*o + n)?;
        *o += n;
        Some(s)
    };
    if take(&mut o, 4)? != b"SHIN" {
        return None;
    }
    let pstr = |o: &mut usize| -> Option<String> {
        let n = u16::from_le_bytes(take(o, 2)?.try_into().ok()?) as usize;
        Some(String::from_utf8(take(o, n)?.to_vec()).ok()?)
    };
    let type_name = pstr(&mut o)?;
    let length = i32::from_le_bytes(take(&mut o, 4)?.try_into().ok()?);
    let kind = pstr(&mut o)?;
    let n = u16::from_le_bytes(take(&mut o, 2)?.try_into().ok()?);
    let mut fields = Vec::new();
    for _ in 0..n {
        let k = pstr(&mut o)?;
        let v = i64::from_le_bytes(take(&mut o, 8)?.try_into().ok()?);
        fields.push((k, v));
    }
    let kind: &'static str = match kind.as_str() {
        "confident" => "confident",
        "uncertain" => "uncertain",
        _ => "unknown",
    };
    Some(DInsn {
        type_name,
        kind,
        length_bytes: (length >= 0).then_some(length as u32),
        fields,
    })
}

/// # Safety
/// HANDLE from sharc_native_create; BLOB covers BLOB_LEN bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sharc_native_exec_insn(handle: *mut Handle, blob: *const u8, blob_len: usize) -> i32 {
    let hd = h(handle);
    let b = slice(blob, blob_len);
    if hd.jit_exec
        && let Some(d) = parse_insn(b)
        && let Ok(1) = exec_translated(hd, d)
    {
        hd.exec_jit += 1;
        return 1;
    }
    hd.exec_fallback += 1;
    hd.m.in_call("jit_exec_insn", b).unwrap_or(-100)
}

/// Translate D as a one-instruction region at the current PC and run it:
/// 1 when it completed, 0 when the region declined (state unchanged).
fn exec_translated(hd: &mut Handle, d: DInsn) -> wasmtime::Result<i32> {
    let pending_none: TypedFunc<(), i32> = hd.m.func("jit_pending_none")?;
    if pending_none.call(&mut hd.m.store, ())? == 0 {
        return Ok(0);
    }
    let pc: TypedFunc<(), i64> = hd.m.func("jit_pc")?;
    let pc = pc.call(&mut hd.m.store, ())?;
    if !(0..(1 << 24)).contains(&pc) {
        return Ok(0);
    }
    let slot = match hd.m.translate_now(pc as u32, Some(Rc::new(d))) {
        Ok(s) => s,
        Err(_) => return Ok(0),
    };
    let icount: TypedFunc<(), i64> = hd.m.func("jit_icount")?;
    let before = icount.call(&mut hd.m.store, ())?;
    let run: TypedFunc<(u32, u32), i32> = hd.m.func("jit_run_slot")?;
    let code = run.call(&mut hd.m.store, (slot, 1000))?;
    let after = icount.call(&mut hd.m.store, ())?;
    Ok((code == 0 && after == before + 1) as i32)
}

/// # Safety
/// HANDLE from sharc_native_create; OUT covers OUT_CAP u64s.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sharc_native_stats(handle: *mut Handle, out: *mut u64, out_cap: usize) -> i32 {
    let hd = h(handle);
    let run = |hd: &mut Handle| -> wasmtime::Result<Vec<u64>> {
        let p = hd.m.alloc(8 * 9)?;
        let f: TypedFunc<(u32, u32), i32> = hd.m.func("jit_stats")?;
        let n = f.call(&mut hd.m.store, (p, 9))? as usize;
        let b = hd.m.bytes(p, 8 * n);
        hd.m.free(p, 8 * 9)?;
        Ok(b.chunks(8).map(|c| u64::from_le_bytes(c.try_into().unwrap())).collect())
    };
    let v = run(hd).unwrap_or_default();
    let n = v.len().min(out_cap);
    // SAFETY: OUT has OUT_CAP u64s.
    unsafe { std::ptr::copy_nonoverlapping(v.as_ptr(), out, n) };
    n as i32
}

/// # Safety
/// HANDLE from sharc_native_create.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sharc_native_set_reg(handle: *mut Handle, code: u32, kind: u32, value: u32, mask: u32) -> i32 {
    let hd = h(handle);
    let f: TypedFunc<(u32, u32, u32, u32), i32> = match hd.m.func("jit_set_reg") {
        Ok(f) => f,
        Err(_) => return -1,
    };
    f.call(&mut hd.m.store, (code, kind, value, mask)).unwrap_or(-1)
}

/// # Safety
/// HANDLE from sharc_native_create.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sharc_native_get_reg(handle: *mut Handle, code: u32) -> u64 {
    let hd = h(handle);
    let f: TypedFunc<u32, i64> = match hd.m.func("jit_get_reg") {
        Ok(f) => f,
        Err(_) => return 0,
    };
    f.call(&mut hd.m.store, code).unwrap_or(0) as u64
}

/// # Safety
/// HANDLE from sharc_native_create; DATA covers DATA_LEN bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sharc_native_poke(handle: *mut Handle, address: u64, data: *const u8, data_len: usize, width: u32) -> i32 {
    let hd = h(handle);
    let b = slice(data, data_len).to_vec();
    let run = |hd: &mut Handle| -> wasmtime::Result<i32> {
        let p = hd.m.put(&b)?;
        let f: TypedFunc<(i64, u32, u32, u32), i32> = hd.m.func("jit_poke")?;
        let r = f.call(&mut hd.m.store, (address as i64, p, b.len() as u32, width))?;
        hd.m.free(p, b.len())?;
        Ok(r)
    };
    run(hd).unwrap_or(-1)
}

/// # Safety
/// HANDLE from sharc_native_create.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sharc_native_peek(handle: *mut Handle, address: u64, width: u32) -> i64 {
    let hd = h(handle);
    let f: TypedFunc<(i64, u32), i64> = match hd.m.func("jit_peek") {
        Ok(f) => f,
        Err(_) => return -1,
    };
    f.call(&mut hd.m.store, (address as i64, width)).unwrap_or(-1)
}

/// # Safety
/// HANDLE from sharc_native_create.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sharc_native_fresh_call(handle: *mut Handle, pc: u32, return_address: i64) -> i32 {
    let hd = h(handle);
    let f: TypedFunc<(u32, i64), i32> = match hd.m.func("jit_fresh_call") {
        Ok(f) => f,
        Err(_) => return -1,
    };
    f.call(&mut hd.m.store, (pc, return_address)).unwrap_or(-1)
}

/// The build's description: the transpiled core's hash and generator
/// version (the translator's input), marked as the JIT.
///
/// # Safety
/// OUT covers OUT_CAP bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sharc_native_info(out: *mut u8, out_cap: usize) -> i32 {
    copy_out(crate::info().as_bytes(), out, out_cap)
}

/// JIT counters as JSON (regions, instructions translated, times, bytes,
/// failure reasons, the modules' running SHA-256, exec_insn split).
///
/// # Safety
/// HANDLE from sharc_native_create; OUT covers OUT_CAP bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sharc_jit_stats(handle: *mut Handle, out: *mut u8, out_cap: usize) -> i32 {
    let hd = h(handle);
    let mut s = crate::stats_json(&hd.m);
    s.pop();
    s.push_str(&format!(", \"exec_jit\": {}, \"exec_fallback\": {}}}", hd.exec_jit, hd.exec_fallback));
    copy_out(s.as_bytes(), out, out_cap)
}

/// Hot threshold (interpreted run starts before a PC is translated) and
/// whether regions run at all.
///
/// # Safety
/// HANDLE from sharc_native_create.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sharc_jit_configure(handle: *mut Handle, threshold: u32, enabled: i32, jit_exec: i32) -> i32 {
    let hd = h(handle);
    hd.jit_exec = jit_exec != 0;
    hd.m.configure(threshold, enabled != 0).map_or(-1, |_| 0)
}
