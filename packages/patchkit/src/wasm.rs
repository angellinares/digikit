//! A C ABI over the same `apply` and `verify` the command line uses, for the browser (`wasm32`). No
//! wasm-bindgen: the page allocates buffers with `pk_alloc`, passes the stock `.syx`, a JSON bundle of
//! ports and modules, and a comma-separated module list, and reads the results back by pointer and length.
//!
//! Bundle: `{"ports": ["<port toml>", ...], "modules": [{"manifest": "<module.toml>", "files": {"build/x.dko.json": "<json>"}}]}`.
//! (`tools/patchkit/patchkit.py bundle` and the gallery's build step, `apps/web/scripts/gen-modules.mjs`, make it from `devices/ports/` and `modules/`.)

use crate::apply;
use crate::error::{Error, Result, sha256_hex};
use crate::module::{Library, Module};
use crate::port::Registry;
use crate::typed::Models;
use serde::Deserialize;
use std::cell::RefCell;
use std::collections::BTreeMap;

#[derive(Deserialize)]
struct Bundle {
    ports: Vec<String>,
    modules: Vec<BundleModule>,
    /// Device files (`devices/*.toml`): the knob models the typed module sections are lowered with.
    #[serde(default)]
    devices: Vec<String>,
}

#[derive(Deserialize)]
struct BundleModule {
    manifest: String,
    files: BTreeMap<String, String>,
}

#[derive(Default)]
struct Out {
    syx: Vec<u8>,
    plan: String,
    err: String,
}

thread_local! {
    static OUT: RefCell<Out> = RefCell::new(Out::default());
}

fn load(bundle: &[u8]) -> Result<(Registry, Library)> {
    let b: Bundle = serde_json::from_slice(bundle)?;
    let reg = Registry::from_texts(b.ports.iter().map(|s| s.as_str()))?;
    let mut modules = vec![];
    for m in &b.modules {
        let read = |p: &str| -> Result<String> {
            m.files
                .get(p)
                .cloned()
                .ok_or_else(|| Error(format!("bundle: missing file {p}")))
        };
        modules.push(Module::from_parts(&m.manifest, &read)?);
    }
    let mut lib = Library { modules };
    lib.lower(&Models::from_texts(b.devices.iter().map(|s| s.as_str()))?)?;
    Ok((reg, lib))
}

/// Apply the modules and return the patched image and the plan (for tests and the C ABI).
pub fn apply_bundle(fw: &[u8], bundle: &[u8], ids: &str) -> Result<(Vec<u8>, String)> {
    let (reg, lib) = load(bundle)?;
    let ids: Vec<String> = ids
        .split(',')
        .filter(|s| !s.is_empty())
        .map(String::from)
        .collect();
    // like the command line: the requirements of the selection are added
    let (ids, _added) = lib.with_requires(&ids)?;
    let out = apply::apply(fw, &reg, &lib, &ids)?;
    let plan = serde_json::to_string_pretty(&out.plan)? + "\n";
    Ok((out.syx, plan))
}

/// Identify an image against the bundle's ports: JSON `{device, sha256, port, record}`.
pub fn inspect_bundle(fw: &[u8], bundle: &[u8]) -> Result<String> {
    let (reg, _) = load(bundle)?;
    let f = crate::firmware::Firmware::parse(fw)?;
    let rep = crate::verify::verify(fw, Some(&reg), None)?;
    Ok(serde_json::json!({
        "device": f.device.short(),
        "sha256": f.syx_sha256,
        "port": reg.find(&f.syx_sha256).map(|p| p.id()),
        "record": rep.record,
        "sections": rep.sections,
    })
    .to_string())
}

/// Which modules of the bundle are usable on this image: JSON `{device, sha256, port, modules: [{id, ok, error}]}`.
/// `port` is null when no port has the image's hash (then every module is refused).
pub fn check_bundle(fw: &[u8], bundle: &[u8]) -> Result<String> {
    let (reg, lib) = load(bundle)?;
    let f = crate::firmware::Firmware::parse(fw)?;
    let port = reg.find(&f.syx_sha256);
    let modules: Vec<_> = lib
        .modules
        .iter()
        .map(|m| {
            let r = match port {
                Some(p) => apply::compatible(m, p),
                None => Err(err!("no port for firmware {}", f.syx_sha256)),
            };
            serde_json::json!({"id": m.id(), "ok": r.is_ok(), "error": r.err().map(|e| e.0)})
        })
        .collect();
    Ok(serde_json::json!({
        "device": f.device.short(),
        "sha256": f.syx_sha256,
        "port": port.map(|p| p.id()),
        "modules": modules,
    })
    .to_string())
}

/// What `pk_sharc_object` takes: the SHARC+ sources of one object as texts, the files their `#include "name"` lines name (the page has no
/// directories: the keys are the names as written), and the build options (`#ifdef NAME`).
#[derive(Deserialize)]
struct SharcInput {
    sources: Vec<String>,
    #[serde(default)]
    includes: BTreeMap<String, String>,
    #[serde(default)]
    defines: Vec<String>,
}

/// SHARC+ sources to the text of the `.dko.json` object `patchkit build` writes (the same bytes): the sources joined, `#include`s expanded,
/// `#ifdef` alternatives selected, assembled with Selache as a library, guarded and re-decoded. For tests and the C ABI.
pub fn sharc_object_json(input: &[u8]) -> Result<String> {
    let i: SharcInput = serde_json::from_slice(input)?;
    let text = crate::build::sharc::source_text_from(&i.sources, &i.includes, &i.defines)?;
    crate::build::to_text(&crate::build::sharc::sharc_object(&text)?)
}

/// `flash::prepare` (the bundle's runtime set) or, with `restore`, `flash::restore`: the image to send and the summary JSON
/// (`Prepared::summary_json`). A failed check is in the summary, not an error; the error is a file that is no known stock image.
pub fn prepare_bundle(
    stock: &[u8],
    bundle: &[u8],
    set: &str,
    restore: bool,
) -> Result<(Vec<u8>, String)> {
    let (reg, lib) = load(bundle)?;
    let p = if restore {
        crate::flash::restore(stock, &reg)?
    } else {
        crate::flash::prepare(
            stock,
            &reg,
            &lib,
            if set.is_empty() { "device" } else { set },
        )?
    };
    let json = p.summary_json().to_string();
    Ok((p.image, json))
}

fn set(r: Result<(Vec<u8>, String)>) -> i32 {
    OUT.with(|o| {
        let mut o = o.borrow_mut();
        match r {
            Ok((syx, plan)) => {
                *o = Out {
                    syx,
                    plan,
                    err: String::new(),
                };
                0
            }
            Err(e) => {
                *o = Out {
                    err: e.0,
                    ..Out::default()
                };
                1
            }
        }
    })
}

/// # Safety
/// `p..p+n` must be a readable range.
unsafe fn slice<'a>(p: *const u8, n: usize) -> &'a [u8] {
    if n == 0 {
        &[]
    } else {
        unsafe { std::slice::from_raw_parts(p, n) }
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn pk_alloc(n: usize) -> *mut u8 {
    let mut v = Vec::<u8>::with_capacity(n.max(1));
    let p = v.as_mut_ptr();
    std::mem::forget(v);
    p
}

/// # Safety
/// `p` and `n` must be a pointer from `pk_alloc(n)`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pk_free(p: *mut u8, n: usize) {
    drop(unsafe { Vec::from_raw_parts(p, 0, n.max(1)) });
}

/// Apply. 0 on success (read `pk_out_*` and `pk_plan_*`), 1 on failure (read `pk_err_*`).
///
/// # Safety
/// The three ranges must be readable.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pk_apply(
    fw: *const u8,
    fw_len: usize,
    bundle: *const u8,
    bundle_len: usize,
    ids: *const u8,
    ids_len: usize,
) -> i32 {
    let (fw, bundle, ids) = unsafe {
        (
            slice(fw, fw_len),
            slice(bundle, bundle_len),
            slice(ids, ids_len),
        )
    };
    let ids = String::from_utf8_lossy(ids).to_string();
    set(apply_bundle(fw, bundle, &ids))
}

/// Identify an image; the JSON is in `pk_plan_*`.
///
/// # Safety
/// The two ranges must be readable.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pk_inspect(
    fw: *const u8,
    fw_len: usize,
    bundle: *const u8,
    bundle_len: usize,
) -> i32 {
    let (fw, bundle) = unsafe { (slice(fw, fw_len), slice(bundle, bundle_len)) };
    set(inspect_bundle(fw, bundle).map(|j| (vec![], j)))
}

/// Per-module compatibility with an image; the JSON is in `pk_plan_*`.
///
/// # Safety
/// The two ranges must be readable.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pk_check(
    fw: *const u8,
    fw_len: usize,
    bundle: *const u8,
    bundle_len: usize,
) -> i32 {
    let (fw, bundle) = unsafe { (slice(fw, fw_len), slice(bundle, bundle_len)) };
    set(check_bundle(fw, bundle).map(|j| (vec![], j)))
}

/// What a load of a `.dkm` takes (arena, DSP area, rows), inferred from the file's contents (`dkneeds`, the code the device's menu runs): JSON
/// `{file_size, ARENA, ARENA_XFER, DSP_AREA, DSP_ALIGN, MACHINE_ROWS, VOICE_ROWS, PARAM_ROWS, PAGE_ROWS, FX_SLOTS}` in `pk_plan_*`, or the reason in
/// `pk_err_*` (1) for a file the loader would refuse.
///
/// # Safety
/// The range must be readable.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pk_needs(dkm: *const u8, dkm_len: usize) -> i32 {
    let dkm = unsafe { slice(dkm, dkm_len) };
    set(crate::dkm::needs_json(dkm).map(|j| (vec![], j.to_string())))
}

/// The fit rule (`dkrt-fit`, the code the DIGIKIT menu runs; packages/patchkit/src/fit.rs): JSON `{caps, needs, empty?}` in; `{each, set, line}` in
/// `pk_plan_*`, or the reason in `pk_err_*` (1).
///
/// # Safety
/// The range must be readable.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pk_fit(req: *const u8, req_len: usize) -> i32 {
    let req = unsafe { slice(req, req_len) };
    set(crate::fit::fit_json(req).map(|j| (vec![], j.to_string())))
}

/// Build a SHARC+ object from source texts: JSON `{sources: [text], includes: {name: text}, defines: [name]}` in; the `.dko.json` text in
/// `pk_plan_*`, or the reason in `pk_err_*` (1).
///
/// # Safety
/// The range must be readable.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pk_sharc_object(input: *const u8, input_len: usize) -> i32 {
    let input = unsafe { slice(input, input_len) };
    set(sharc_object_json(input).map(|j| (vec![], j)))
}

/// Verify an image (and, if `base_len` is not 0, against that stock image); the report JSON is in `pk_plan_*`.
///
/// # Safety
/// The ranges must be readable.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pk_verify(
    syx: *const u8,
    syx_len: usize,
    bundle: *const u8,
    bundle_len: usize,
    base: *const u8,
    base_len: usize,
) -> i32 {
    let (syx, bundle, base) = unsafe {
        (
            slice(syx, syx_len),
            slice(bundle, bundle_len),
            slice(base, base_len),
        )
    };
    let r = load(bundle).and_then(|(reg, _)| {
        let rep = crate::verify::verify(syx, Some(&reg), (!base.is_empty()).then_some(base))?;
        Ok((vec![], serde_json::to_string(&rep)?))
    });
    set(r)
}

/// Prepare a flash (see `prepare_bundle`): the image is in `pk_out_*`, the summary JSON in `pk_plan_*`. `set` names the patch set (empty:
/// `device`); `restore` is 0 or 1.
///
/// # Safety
/// The three ranges must be readable.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pk_prepare(
    stock: *const u8,
    stock_len: usize,
    bundle: *const u8,
    bundle_len: usize,
    set: *const u8,
    set_len: usize,
    restore: i32,
) -> i32 {
    let (stock, bundle, set) = unsafe {
        (
            slice(stock, stock_len),
            slice(bundle, bundle_len),
            slice(set, set_len),
        )
    };
    let set = String::from_utf8_lossy(set).to_string();
    self::set(prepare_bundle(stock, bundle, &set, restore != 0))
}

// ---- the sender -------------------------------------------------------------------------------------------------------------

thread_local! {
    static SENDERS: RefCell<Vec<Option<crate::flash::Sender>>> = const { RefCell::new(Vec::new()) };
}

/// A sender for `total` messages (`flash::Sender`, the 2 ms pacing); its handle, to give to `pk_sender_next` and `pk_sender_free`.
#[unsafe(no_mangle)]
pub extern "C" fn pk_sender_new(total: usize) -> u32 {
    SENDERS.with(|s| {
        let mut s = s.borrow_mut();
        let sender = crate::flash::Sender::new(total);
        match s.iter().position(Option::is_none) {
            Some(i) => {
                s[i] = Some(sender);
                i as u32
            }
            None => {
                s.push(Some(sender));
                s.len() as u32 - 1
            }
        }
    })
}

/// The next step at the millisecond clock `now_ms` (any monotonic clock; it wraps at 2^32 ms): 0 or more is `Send(i)` (message `i`
/// goes out now), `-1` is `Done`, `-2` is `Aborted` (also an unknown handle), and `-3 - ms` is `Wait(ms)`.
#[unsafe(no_mangle)]
pub extern "C" fn pk_sender_next(handle: u32, now_ms: f64) -> i32 {
    SENDERS.with(|s| match s.borrow_mut().get_mut(handle as usize) {
        Some(Some(sender)) => match sender.next(now_ms as u64) {
            crate::flash::Step::Send(i) => i as i32,
            crate::flash::Step::Done => -1,
            crate::flash::Step::Aborted => -2,
            crate::flash::Step::Wait(ms) => -3 - ms.min(1 << 20) as i32,
        },
        _ => -2,
    })
}

/// An output error: the sender stops for good (every later `pk_sender_next` is `-2`).
#[unsafe(no_mangle)]
pub extern "C" fn pk_sender_fail(handle: u32) {
    SENDERS.with(|s| {
        if let Some(Some(sender)) = s.borrow_mut().get_mut(handle as usize) {
            sender.fail();
        }
    });
}

/// Messages handed out so far.
#[unsafe(no_mangle)]
pub extern "C" fn pk_sender_sent(handle: u32) -> usize {
    SENDERS.with(|s| match s.borrow().get(handle as usize) {
        Some(Some(sender)) => sender.sent(),
        _ => 0,
    })
}

#[unsafe(no_mangle)]
pub extern "C" fn pk_sender_free(handle: u32) {
    SENDERS.with(|s| {
        if let Some(slot) = s.borrow_mut().get_mut(handle as usize) {
            *slot = None;
        }
    });
}

// ---- the RPC flash machine ----------------------------------------------------------------------------------------------------

thread_local! {
    static RPC: RefCell<Vec<Option<crate::flash_rpc::RpcFlash>>> = const { RefCell::new(Vec::new()) };
}

fn rpc_with<R>(h: u32, f: impl FnOnce(&mut crate::flash_rpc::RpcFlash) -> R) -> Option<R> {
    RPC.with(|r| match r.borrow_mut().get_mut(h as usize) {
        Some(Some(m)) => Some(f(m)),
        _ => None,
    })
}

/// A flash machine (`flash_rpc::RpcFlash`) for the `.syx` image at `image` (it is copied); `chunk` is the stream bytes per Write (0: the
/// default). Its handle.
///
/// # Safety
/// `image..image+image_len` must be readable.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pk_rpc_new(image: *const u8, image_len: usize, chunk: usize) -> u32 {
    let m = crate::flash_rpc::RpcFlash::new(unsafe { slice(image, image_len) }.to_vec(), chunk);
    RPC.with(|r| {
        let mut r = r.borrow_mut();
        match r.iter().position(Option::is_none) {
            Some(i) => {
                r[i] = Some(m);
                i as u32
            }
            None => {
                r.push(Some(m));
                r.len() as u32 - 1
            }
        }
    })
}

/// What to do next. The message to send is in `pk_out_*`; `pk_plan_*` holds `{"kind": "send"|"ask"|"reboot"|"done"|"failed", "timeout_ms",
/// "code", "message", "taken"}`. Returns 0, or 1 for an unknown handle.
#[unsafe(no_mangle)]
pub extern "C" fn pk_rpc_next(h: u32) -> i32 {
    use crate::flash_rpc::Action;
    let Some(a) = rpc_with(h, |m| m.next()) else {
        return 1;
    };
    let (bytes, j) = match a {
        Action::Send { bytes, timeout_ms } => (
            bytes,
            serde_json::json!({"kind": "send", "timeout_ms": timeout_ms}),
        ),
        Action::AskConfirm => (vec![], serde_json::json!({"kind": "ask"})),
        Action::WaitReboot => (vec![], serde_json::json!({"kind": "reboot"})),
        Action::Done => (vec![], serde_json::json!({"kind": "done"})),
        Action::Failed(f) => (
            vec![],
            serde_json::json!({"kind": "failed", "code": f.code.name(), "message": f.message, "taken": f.taken}),
        ),
    };
    set(Ok((bytes, j.to_string())));
    0
}

/// A message from the device: 1 when it was the reply the machine waits for, 0 when not (keep waiting), -1 for an unknown handle.
///
/// # Safety
/// `msg..msg+len` must be readable.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pk_rpc_reply(h: u32, msg: *const u8, len: usize) -> i32 {
    let msg = unsafe { slice(msg, len) };
    match rpc_with(h, |m| m.on_reply(msg)) {
        Some(true) => 1,
        Some(false) => 0,
        None => -1,
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn pk_rpc_timeout(h: u32) {
    rpc_with(h, |m| m.on_timeout());
}

#[unsafe(no_mangle)]
pub extern "C" fn pk_rpc_confirm(h: u32, yes: i32) {
    rpc_with(h, |m| m.confirm(yes != 0));
}

/// 1 when the cancel took effect (End with commit 0 follows), 0 when it is too late.
#[unsafe(no_mangle)]
pub extern "C" fn pk_rpc_cancel(h: u32) -> i32 {
    rpc_with(h, |m| m.cancel()).map_or(0, i32::from)
}

/// Bytes of the image the device has acknowledged.
#[unsafe(no_mangle)]
pub extern "C" fn pk_rpc_taken(h: u32) -> usize {
    rpc_with(h, |m| m.taken()).unwrap_or(0)
}

#[unsafe(no_mangle)]
pub extern "C" fn pk_rpc_free(h: u32) {
    RPC.with(|r| {
        if let Some(slot) = r.borrow_mut().get_mut(h as usize) {
            *slot = None;
        }
    });
}

macro_rules! getter {
    ($ptr:ident, $len:ident, $field:ident) => {
        #[unsafe(no_mangle)]
        pub extern "C" fn $ptr() -> *const u8 {
            OUT.with(|o| o.borrow().$field.as_ptr())
        }
        #[unsafe(no_mangle)]
        pub extern "C" fn $len() -> usize {
            OUT.with(|o| o.borrow().$field.len())
        }
    };
}
getter!(pk_out_ptr, pk_out_len, syx);
getter!(pk_plan_ptr, pk_plan_len, plan);
getter!(pk_err_ptr, pk_err_len, err);

/// sha256 of the bytes in `pk_out_*`, hex (for the page to show without hashing again).
pub fn out_sha256() -> String {
    OUT.with(|o| sha256_hex(&o.borrow().syx))
}
