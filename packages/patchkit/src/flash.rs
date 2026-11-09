//! Flashing a device through its OS UPGRADE screen: what to check before a byte is sent, and how to send it. No I/O: the command line,
//! the desktop app and the browser (`wasm32`) use this one core and add a MIDI output.
//!
//! The device side is finding 75 (`docs/findings/75-os-upgrade-receive.md`): the screen takes the plain `.syx` stream, sends no reply,
//! validates after the last message (content checksum, on DT2 the build floor, HMAC) and erases nothing when the validation fails.
//! Nothing tells the sender that a message went wrong, so every check runs here first, including a model of the device's receiver
//! (`device_receive`) run over the exact messages the sender will send.

use crate::apply;
use crate::devdata::{self, DeviceInfo};
use crate::error::*;
use crate::firmware::Firmware;
use crate::imageid;
use crate::module::Library;
use crate::port::Registry;
use crate::preflight::{self, Known};
use crate::syx;
use crate::verify;
use dt2_firmware_loader as loader;
use std::ops::Range;

/// The reply-driven route through the MidiRpc requests (the default); this file's `Sender` is the menu route.
pub use crate::flash_rpc::{
    Action as RpcAction, FailCode as RpcFailCode, Failure as RpcFailure, RpcFlash,
};

/// Pause between two messages (finding 75: 12 times the rate the emulated receiver keeps up with, below the 1 ms USB frame limit).
pub const PACE_MS: u64 = 2;
/// A data message: `F0 00 20 3C <dev> 00 7E seq[3] payload[116] checksum F7`.
pub const DATA_LEN: usize = 128;
/// Where the device's validator reads the build word, counted in the received stream (total length and checksum come first).
pub const BUILD_WORD_AT: usize = 0x10;

#[derive(Clone, Debug)]
pub struct Check {
    pub name: String,
    pub ok: bool,
    pub detail: String,
}

/// An image ready to send, and what was checked.
#[derive(Clone, Debug)]
pub struct Prepared {
    /// The exact bytes to send: the patched image, or with `restore` the stock file. It is the messages one after the other.
    pub image: Vec<u8>,
    /// The messages, as ranges of `image`: a 16-byte header, the 128-byte data messages, a 16-byte footer.
    pub messages: Vec<Range<usize>>,
    pub checks: Vec<Check>,
    /// Short name of the device (`dt2`, `dn2`) and the stock OS version.
    pub device: String,
    pub os: String,
    /// The id the runtime's INFO reports after the flash (`None`: a restore, or a patch without the runtime).
    pub image_id: Option<[u8; 8]>,
    pub sha_stock: String,
    pub sha_image: String,
    pub restore: bool,
}

impl Prepared {
    /// Every check passed (and there is something to send).
    pub fn ok(&self) -> bool {
        !self.messages.is_empty() && self.checks.iter().all(|c| c.ok)
    }
    pub fn failed(&self) -> Vec<&Check> {
        self.checks.iter().filter(|c| !c.ok).collect()
    }
    pub fn message(&self, i: usize) -> &[u8] {
        &self.image[self.messages[i].clone()]
    }
    /// The id as 16 hex digits, the form INFO is compared in.
    pub fn image_id_hex(&self) -> Option<String> {
        self.image_id.map(|i| hex(&i))
    }
}

impl Prepared {
    /// What a user interface shows and the sender needs, without the image itself: the checks, the facts, and the end offset of
    /// each message in `image` (message `i` is `image[ends[i-1]..ends[i]]`, the first from 0). The browser and the desktop app read it.
    pub fn summary_json(&self) -> serde_json::Value {
        serde_json::json!({
            "ok": self.ok(),
            "restore": self.restore,
            "device": self.device,
            "os": self.os,
            "sha_stock": self.sha_stock,
            "sha_image": self.sha_image,
            "image_id": self.image_id_hex(),
            // the runtime version the patch carries (a restore has none)
            "runtime": (!self.restore).then(|| crate::dkm::fmt_runtime(crate::dkm::runtime_version())),
            "pace_ms": PACE_MS,
            "bytes": self.image.len(),
            "ends": self.messages.iter().map(|m| m.end).collect::<Vec<_>>(),
            "checks": self.checks.iter().map(|c| serde_json::json!({"name": c.name, "ok": c.ok, "detail": c.detail})).collect::<Vec<_>>(),
        })
    }
}

struct Checks(Vec<Check>);

impl Checks {
    fn run<T>(&mut self, name: &str, r: Result<(T, String)>) -> Option<T> {
        match r {
            Ok((v, detail)) => {
                self.0.push(Check {
                    name: name.into(),
                    ok: true,
                    detail,
                });
                Some(v)
            }
            Err(e) => {
                self.0.push(Check {
                    name: name.into(),
                    ok: false,
                    detail: e.0,
                });
                None
            }
        }
    }
}

/// The stock images the registry knows, in the form preflight takes.
fn known_of(reg: &Registry) -> Vec<Known> {
    reg.ports
        .iter()
        .map(|p| Known {
            device: p.port.device.clone(),
            version: p.port.version.clone(),
            sha256: p.port.syx_sha256.clone(),
            filename: String::new(),
        })
        .collect()
}

fn info_of(short: &str) -> Result<&'static DeviceInfo> {
    devdata::by_short(short).ok_or_else(|| err!("no device file for {short}"))
}

/// Patch the stock image with the device file's patch set `set` and check the result for sending.
pub fn prepare(stock: &[u8], reg: &Registry, lib: &Library, set: &str) -> Result<Prepared> {
    let sfw = Firmware::parse(stock)?;
    let port = known_port(&sfw, stock, reg)?;
    let (device, os) = (port.port.device.clone(), port.port.version.clone());
    let roots = info_of(&device)?.patch_set(set)?.to_vec();
    let (ids, _added) = lib.with_requires(&roots)?;
    let applied = apply::apply(stock, reg, lib, &ids)?;
    let mut cs = Checks(vec![]);
    cs.run(
        "stock",
        Ok(((), format!("{device} {os}, sha256 {}", sfw.syx_sha256))),
    );
    cs.run(
        "patch",
        Ok((
            (),
            format!("set {set}: {}", applied.plan.modules.join(", ")),
        )),
    );
    let known = known_of(reg);
    let stock_sha = sfw.syx_sha256.clone();
    let code = applied.plan.sharc_code();
    let o = preflight::preflight(
        &applied.syx,
        reg,
        &known,
        &|k| (k.sha256 == stock_sha).then(|| stock.to_vec()),
        Some(&code),
    );
    let bad: Vec<String> = o
        .lines
        .iter()
        .filter(|l| !l.ok)
        .map(|l| l.text.clone())
        .collect();
    cs.0.push(Check {
        name: "preflight".into(),
        ok: o.passed(),
        detail: if o.passed() {
            let changed = o
                .lines
                .iter()
                .find(|l| l.text.starts_with("changed sections"))
                .map_or(String::new(), |l| format!("; {}", l.text));
            format!(
                "sections 2, 4, 5 and 8 identical to the stock image, {} lines{changed}",
                o.lines.len()
            )
        } else {
            bad.join(" | ")
        },
    });
    let mut p = finish(applied.syx, reg, cs, false, Some(&code))?;
    p.os = os;
    p.sha_stock = stock_sha;
    p.image_id = applied.image_id;
    // the id the image carries is the id the patch made (`image id` check: it also matches the sections)
    let carried = read_id(&p.image)?;
    match (carried, p.image_id) {
        (Some((id, true)), Some(made)) if id == made => p.checks.push(Check {
            name: "image id".into(),
            ok: true,
            detail: format!("{}, matches the MAIN OS and DSP sections", hex(&id)),
        }),
        (None, None) => p.checks.push(Check {
            name: "image id".into(),
            ok: true,
            detail: "no runtime in this patch, no id".into(),
        }),
        (c, m) => p.checks.push(Check {
            name: "image id".into(),
            ok: false,
            detail: format!(
                "the image carries {:?}, the patch made {:?}",
                c.map(|c| (hex(&c.0), c.1)),
                m.map(|m| hex(&m))
            ),
        }),
    }
    Ok(p)
}

/// `prepare` or `restore` for a checkout: the registry, the module library and the device files are read from `root` (the desktop app).
#[cfg(not(target_arch = "wasm32"))]
pub fn prepare_in_repo(
    root: &std::path::Path,
    stock: &[u8],
    restore_stock: bool,
    set: &str,
) -> Result<Prepared> {
    let reg = Registry::from_dir(&root.join("devices/ports"))?;
    if restore_stock {
        return restore(stock, &reg);
    }
    // the patch set is `core/*` modules: their objects are committed, no content module is built
    let fw = Firmware::parse(stock)?;
    let device = known_port(&fw, stock, &reg)?.port.device.clone();
    let roots = info_of(&device)?.patch_set(set)?.to_vec();
    let mut lib = Library::scan_repo_for(root, false, Some(&roots))?;
    lib.lower(&crate::typed::Models::from_dir(&root.join("devices"))?)?;
    prepare(stock, &reg, &lib, set)
}

/// The stock file itself, through the same checks (a way back from a patched image). It needs no patch and carries no record.
pub fn restore(stock: &[u8], reg: &Registry) -> Result<Prepared> {
    let sfw = Firmware::parse(stock)?;
    let port = known_port(&sfw, stock, reg)?;
    let (device, os) = (port.port.device.clone(), port.port.version.clone());
    let mut cs = Checks(vec![]);
    cs.run(
        "stock",
        Ok(((), format!("{device} {os}, sha256 {}", sfw.syx_sha256))),
    );
    let mut p = finish(stock.to_vec(), reg, cs, true, None)?;
    p.os = os;
    p.sha_stock = sfw.syx_sha256;
    Ok(p)
}

/// The port of a stock image: a known firmware, and not one patchkit made.
fn known_port<'a>(fw: &Firmware, raw: &[u8], reg: &'a Registry) -> Result<&'a crate::port::Port> {
    reg.find(&fw.syx_sha256).ok_or_else(|| {
        let patched = verify::patch_record(raw).ok().flatten().is_some();
        if patched {
            err!("this file is a patched image; give the stock file it was made from (sha256 {})", fw.syx_sha256)
        } else {
            err!(
                "this file ({}) is not a stock firmware patchkit knows (no port in devices/ports/ has its sha256 {})",
                fw.device.short(),
                fw.syx_sha256
            )
        }
    })
}

/// The checks every image takes, patched or stock: verify (checksums, HMAC, limits, record), the product byte, the build floor and
/// the round trip through the model of the device's receiver. `check_image` runs them on any image (the tests tamper with images).
pub fn check_image(image: Vec<u8>, reg: &Registry) -> Result<Prepared> {
    finish(image, reg, Checks(vec![]), false, None)
}

fn finish(
    image: Vec<u8>,
    reg: &Registry,
    mut cs: Checks,
    restore: bool,
    code: Option<&[(u64, u64)]>,
) -> Result<Prepared> {
    let device = Firmware::parse(&image)?.device.short().to_string();
    let info = info_of(&device)?;
    let mut messages: Vec<Range<usize>> = vec![];
    let rep = cs.run(
        "verify",
        verify::verify(&image, Some(reg), None).map(|r| {
            let d = format!(
                "{}; container {} bytes, {} data messages",
                r.checks.join("; "),
                r.container_bytes,
                r.data_messages
            );
            (r, d)
        }),
    );
    if let Some(r) = &rep
        && restore
        && r.record.is_some()
    {
        cs.0.push(Check {
            name: "stock".into(),
            ok: false,
            detail: "the file carries a patch record: it is not a stock image".into(),
        });
    }
    cs.run("product", product_check(&image, info));
    cs.run("build floor", floor_check(&image, info));
    cs.run("DSP scan", dsp_scan(&image, code));
    let rt = cs.run("round trip", round_trip(&image, info));
    if rt.is_some() {
        messages = split(&image)?;
    }
    let sha_image = sha256_hex(&image);
    Ok(Prepared {
        image,
        messages,
        checks: cs.0,
        device,
        os: String::new(),
        image_id: None,
        sha_stock: String::new(),
        sha_image,
        restore,
    })
}

/// The message ranges of a `.syx`.
fn split(image: &[u8]) -> Result<Vec<Range<usize>>> {
    let mut v = vec![];
    let mut at = 0;
    for m in syx::messages(image)? {
        v.push(at..at + m.len());
        at += m.len();
    }
    Ok(v)
}

fn read_id(image: &[u8]) -> Result<Option<([u8; 8], bool)>> {
    let fw = Firmware::parse(image)?;
    let (Some(main), Some(dsp)) = (fw.section(3), fw.section(7)) else {
        return Ok(None);
    };
    let base = match verify::patch_record(image)? {
        Some(r) => unhex(&r.base_sha256)?,
        None => return Ok(None),
    };
    imageid::read(&base, &main.data, &dsp.data)
}

/// The stream id byte of both framing messages is the device's (`[patch] stream_id`): the device refuses a header with another id,
/// and the checksum of every data message depends on it.
pub fn product_check(image: &[u8], info: &DeviceInfo) -> Result<((), String)> {
    let msgs = syx::messages(image)?;
    let (first, last) = (msgs.first(), msgs.last());
    for (what, m) in [("header", first), ("footer", last)] {
        let m = m.ok_or_else(|| err!("no {what} message"))?;
        if m.len() != 16 {
            return Err(err!("the {what} message has {} bytes, not 16", m.len()));
        }
        if m[4] != info.syx_id {
            return Err(err!(
                "the {what} message is for device byte {:#04x}, {} takes {:#04x}",
                m[4],
                info.short,
                info.syx_id
            ));
        }
        if m[8] != info.stream_id {
            return Err(err!(
                "the {what} message has stream id {:#04x}, {} takes {:#04x}: the device would refuse the stream",
                m[8],
                info.short,
                info.stream_id
            ));
        }
    }
    Ok((
        (),
        format!(
            "stream id {:#04x}, device byte {:#04x} ({})",
            info.stream_id, info.syx_id, info.short
        ),
    ))
}

/// The build word of a received stream (the container header's build number as four ASCII characters).
pub fn build_word(stream: &[u8]) -> Result<[u8; 4]> {
    stream
        .get(BUILD_WORD_AT..BUILD_WORD_AT + 4)
        .map(|w| w.try_into().unwrap())
        .ok_or_else(|| err!("the stream is too short for a build word"))
}

fn floor_of(word: [u8; 4], info: &DeviceInfo) -> Result<String> {
    let w = String::from_utf8_lossy(&word).into_owned();
    match info.build_floor {
        // the device compares the two words as unsigned numbers
        Some(f) if u32::from_be_bytes(word) < u32::from_be_bytes(f) => Err(err!(
            "build {w:?} is below the floor {:?} of {}: the device answers \"Downgrade not possible\" and erases nothing",
            String::from_utf8_lossy(&f),
            info.short
        )),
        Some(f) => Ok(format!(
            "build {w:?}, at or above the floor {:?}",
            String::from_utf8_lossy(&f)
        )),
        None => Ok(format!("build {w:?}; {} has no build floor", info.short)),
    }
}

/// The DSP section (7) holds no store the SHARC+ ISA excludes (`dspscan`; the stock images have none). With `code` (the plan's SHARC code ranges), appended blocks are scanned only inside those ranges.
pub fn dsp_scan(image: &[u8], code: Option<&[(u64, u64)]>) -> Result<((), String)> {
    let fw = Firmware::parse(image)?;
    let dsp = fw
        .section(7)
        .ok_or_else(|| err!("the image has no DSP section (7)"))?;
    Ok(((), crate::dspscan::check_section7(&dsp.data, code)?))
}

/// The device's build floor (`[patch] build_floor`), on the image's container.
pub fn floor_check(image: &[u8], info: &DeviceInfo) -> Result<((), String)> {
    let dec = loader::decode_syx(image)?;
    let d = floor_of(build_word(&dec)?, info)?;
    Ok(((), d))
}

/// The device's receiver (finding 75), run over the messages: header (device and stream id, start counter, announced count), each data
/// message (sequence, packet checksum, 101 bytes from the 8-in-7 payload), footer (count, id, start counter). Returns the staged stream.
/// A wrong sequence or checksum resets the device's receiver, so here it is an error with the message number.
pub fn device_receive(msgs: &[&[u8]], info: &DeviceInfo) -> Result<Vec<u8>> {
    let (Some(h), Some(f)) = (msgs.first(), msgs.last()) else {
        return Err(err!("no messages"));
    };
    if msgs.len() < 3 {
        return Err(err!(
            "{} messages: a header, data and a footer are needed",
            msgs.len()
        ));
    }
    let head = [0xf0, 0x00, 0x20, 0x3c, info.syx_id, 0x00, 0x7f];
    let framing = |what: &str, m: &[u8], kind: u8| -> Result<(u32, usize)> {
        if m.len() != 16 || m[..7] != head || m[15] != 0xf7 {
            return Err(err!("{what}: not a {} header message", info.short));
        }
        if m[7] != kind {
            return Err(err!("{what}: byte 7 is {:#04x}, not {kind:#04x}", m[7]));
        }
        if m[8] != info.stream_id {
            return Err(err!(
                "{what}: stream id {:#04x} is refused (the device takes {:#04x})",
                m[8],
                info.stream_id
            ));
        }
        let counter = (m[9] as u32) << 14 | (m[10] as u32) << 7 | m[11] as u32;
        let count = (m[12] as usize) << 14 | (m[13] as usize) << 7 | m[14] as usize;
        Ok((counter, count))
    };
    let (start, announced) = framing("header", h, 1)?;
    let (fstart, fcount) = framing("footer", f, 2)?;
    if (fstart, fcount) != (start, announced) {
        return Err(err!(
            "the footer repeats start counter {fstart} and count {fcount}, the header said {start} and {announced}"
        ));
    }
    let mut out = Vec::with_capacity(announced * 101);
    for (n, m) in msgs[1..msgs.len() - 1].iter().enumerate() {
        if m.len() != DATA_LEN
            || m[..7] != [0xf0, 0x00, 0x20, 0x3c, info.syx_id, 0x00, 0x7e]
            || m[127] != 0xf7
        {
            return Err(err!(
                "message {n}: not a data message of {} bytes",
                DATA_LEN
            ));
        }
        let seq = (m[7] as u32) << 14 | (m[8] as u32) << 7 | m[9] as u32;
        if seq != start + n as u32 {
            return Err(err!(
                "message {n}: sequence {seq}, the device expects {}: it would reset",
                start + n as u32
            ));
        }
        if m[126] != syx::packet_checksum(&m[1..127], info.stream_id) {
            return Err(err!("message {n}: bad checksum: the device would reset"));
        }
        let data = dkupload::decode_8in7(&m[10..126]).map_err(|e| err!("message {n}: {e}"))?;
        if data.len() != syx::CHUNK_SIZE {
            return Err(err!(
                "message {n}: {} data bytes, not {}",
                data.len(),
                syx::CHUNK_SIZE
            ));
        }
        out.extend_from_slice(&data);
    }
    let taken = msgs.len() - 2;
    if taken != announced {
        return Err(err!(
            "{taken} data messages, the header announced {announced}"
        ));
    }
    Ok(out)
}

/// The device's validator (finding 75) on a received stream: content checksum, build floor, HMAC trailer.
pub fn device_validate(stream: &[u8], info: &DeviceInfo) -> Result<()> {
    if stream.len() < 8 {
        return Err(err!("received stream too short"));
    }
    let total = u32::from_be_bytes(stream[0..4].try_into().unwrap()) as usize;
    let sum = u32::from_be_bytes(stream[4..8].try_into().unwrap());
    let container = stream.get(8..8 + total).ok_or_else(|| {
        err!("the stream is shorter than its announced container ({total} bytes)")
    })?;
    if !total.is_multiple_of(16) || total < 64 || syx::content_checksum(container) != sum {
        return Err(err!(
            "content checksum fails: the device shows \"Checksum error!\""
        ));
    }
    floor_of(build_word(stream)?, info)?;
    let dev = syx::Device::from_id(info.syx_id)?;
    if syx::trailer(container, dev)[..] != container[total - 32..] {
        return Err(err!("HMAC fails: the device shows \"Checksum error!\""));
    }
    Ok(())
}

/// The outgoing round trip: the messages as the sender will send them go through the receiver model, the validator runs on what was
/// received, and the received bytes equal the decoded image.
pub fn round_trip(image: &[u8], info: &DeviceInfo) -> Result<((), String)> {
    let msgs = syx::messages(image)?;
    let got = device_receive(&msgs, info)?;
    device_validate(&got, info)?;
    let want = loader::decode_syx(image)?;
    if got != want {
        return Err(err!("the received stream differs from the image"));
    }
    Ok((
        (),
        format!(
            "{} messages received in order, checksums, validator, {} bytes equal to the image",
            msgs.len(),
            got.len()
        ),
    ))
}

// ---- the container in the flash --------------------------------------------------------------------------------------------

/// The container of an image (what the device writes to flash at 0x80000): the received stream without its length and checksum words.
pub fn container_of(image: &[u8]) -> Result<Vec<u8>> {
    let dec = loader::decode_syx(image)?;
    let total = u32::from_be_bytes(
        dec.get(0..4)
            .ok_or_else(|| err!("the image is too short"))?
            .try_into()
            .unwrap(),
    ) as usize;
    dec.get(8..8 + total)
        .map(<[u8]>::to_vec)
        .ok_or_else(|| err!("the image is shorter than its container"))
}

/// The `.syx` for a container read back from flash (from its first byte on; erased bytes may follow). The container's length is
/// its last section's end, rounded up to 16, and the 32-byte trailer; the trailer must be the device's HMAC. The framing messages
/// come from `like`, an image of the same device (the emulator boots what the device flashed).
pub fn image_from_flash(flash: &[u8], like: &[u8]) -> Result<Vec<u8>> {
    let fw = Firmware::parse(like)?;
    if flash.get(..4) != Some(b"ELE3") {
        return Err(err!("the flash holds no container at its start"));
    }
    let be = |at: usize| -> Result<usize> {
        flash
            .get(at..at + 4)
            .map(|b| u32::from_be_bytes(b.try_into().unwrap()) as usize)
            .ok_or_else(|| err!("the container table is cut"))
    };
    let count = be(syx::COUNT_OFF)?;
    if count == 0 || count > 32 {
        return Err(err!("the flash container has {count} sections"));
    }
    let mut end = 0;
    for i in 0..count {
        let e = syx::TABLE_OFF + i * syx::ENTRY_SZ;
        end = end.max(be(e + 4)? + be(e + 8)?);
    }
    let total = end.div_ceil(16) * 16 + 32;
    let container = flash
        .get(..total)
        .ok_or_else(|| err!("the container ({total} bytes) is cut"))?;
    if syx::trailer(container, fw.device)[..] != container[total - 32..] {
        return Err(err!("the flash container does not carry the device's HMAC"));
    }
    let mut stream = (total as u32).to_be_bytes().to_vec();
    stream.extend_from_slice(&syx::content_checksum(container).to_be_bytes());
    stream.extend_from_slice(container);
    Ok(syx::encode_syx(
        &stream,
        fw.device_id,
        &fw.framing_start,
        &fw.framing_end,
    ))
}

// ---- the sender ------------------------------------------------------------------------------------------------------------

/// What the sender wants next.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Step {
    /// Send message `i` now.
    Send(usize),
    /// Nothing yet: ask again after this many milliseconds.
    Wait(u64),
    /// Every message was sent.
    Done,
    /// An error was reported: stop. The device keeps its old image (a cut stream is never validated); start a new run from the header.
    Aborted,
}

/// A message list sent one by one, `PACE_MS` apart. The caller owns the clock and the output; no resume.
#[derive(Clone, Debug)]
pub struct Sender {
    total: usize,
    next: usize,
    last: Option<u64>,
    aborted: bool,
}

impl Sender {
    pub fn new(total: usize) -> Sender {
        Sender {
            total,
            next: 0,
            last: None,
            aborted: false,
        }
    }
    /// The next step at time `now_ms` (any monotonic millisecond clock). `Send(i)` counts the message as sent.
    pub fn next(&mut self, now_ms: u64) -> Step {
        if self.aborted {
            return Step::Aborted;
        }
        if self.next >= self.total {
            return Step::Done;
        }
        if let Some(t) = self.last {
            let due = t + PACE_MS;
            if now_ms < due {
                return Step::Wait(due - now_ms);
            }
        }
        self.last = Some(now_ms);
        self.next += 1;
        Step::Send(self.next - 1)
    }
    /// An output error: stop for good.
    pub fn fail(&mut self) {
        self.aborted = true;
    }
    pub fn sent(&self) -> usize {
        self.next
    }
    pub fn total(&self) -> usize {
        self.total
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dt2() -> &'static DeviceInfo {
        devdata::by_short("dt2").unwrap()
    }
    fn dn2() -> &'static DeviceInfo {
        devdata::by_short("dn2").unwrap()
    }

    /// A small stream a receiver accepts: `ELE3`, a build word at stream offset 0x10, padding, a sealed trailer.
    fn stream(info: &DeviceInfo, build: &[u8; 4]) -> Vec<u8> {
        let mut c = vec![0u8; 400];
        c[..4].copy_from_slice(b"ELE3");
        c[8..12].copy_from_slice(build);
        for (i, b) in c.iter_mut().enumerate().take(128).skip(16) {
            *b = (i * 7) as u8;
        }
        let c = syx::seal(c, syx::Device::from_id(info.syx_id).unwrap());
        let mut s = (c.len() as u32).to_be_bytes().to_vec();
        s.extend_from_slice(&syx::content_checksum(&c).to_be_bytes());
        s.extend_from_slice(&c);
        s
    }

    fn framing(info: &DeviceInfo, kind: u8, count: usize) -> Vec<u8> {
        let mut m = vec![
            0xf0,
            0,
            0x20,
            0x3c,
            info.syx_id,
            0,
            0x7f,
            kind,
            info.stream_id,
        ];
        m.extend_from_slice(&[0, 1, 0x72]);
        m.extend_from_slice(&[
            (count >> 14) as u8 & 0x7f,
            (count >> 7) as u8 & 0x7f,
            count as u8 & 0x7f,
            0xf7,
        ]);
        m
    }

    fn syx_of(info: &DeviceInfo, s: &[u8]) -> Vec<u8> {
        syx::encode_syx(s, info.syx_id, &framing(info, 1, 0), &framing(info, 2, 0))
    }

    fn msgs(image: &[u8]) -> Vec<Vec<u8>> {
        syx::messages(image)
            .unwrap()
            .iter()
            .map(|m| m.to_vec())
            .collect()
    }
    fn join(m: &[Vec<u8>]) -> Vec<u8> {
        m.concat()
    }
    fn receive(m: &[Vec<u8>], info: &DeviceInfo) -> Result<Vec<u8>> {
        let r: Vec<&[u8]> = m.iter().map(|x| x.as_slice()).collect();
        device_receive(&r, info)
    }

    #[test]
    fn the_receiver_model_takes_a_good_stream_and_the_validator_passes_it() {
        for info in [dt2(), dn2()] {
            let s = stream(info, b"0082");
            let image = syx_of(info, &s);
            let got = device_receive(&syx::messages(&image).unwrap(), info).unwrap();
            assert_eq!(&got[..s.len()], &s[..]);
            device_validate(&got, info).unwrap();
            round_trip(&image, info).unwrap();
            product_check(&image, info).unwrap();
        }
    }

    #[test]
    fn a_dropped_message_breaks_the_round_trip() {
        let info = dt2();
        let mut m = msgs(&syx_of(info, &stream(info, b"0082")));
        assert!(m.len() > 4);
        m.remove(2);
        let e = receive(&m, info).unwrap_err().0;
        assert!(e.contains("sequence"), "{e}");
        assert!(round_trip(&join(&m), info).is_err());
    }

    #[test]
    fn a_dropped_last_data_message_is_caught_by_the_announced_count() {
        let info = dt2();
        let mut s = stream(info, b"0082");
        s.resize(s.len() + 200, 0);
        let image = syx::encode_syx(&s, info.syx_id, &framing(info, 1, 0), &framing(info, 2, 0));
        let mut m = msgs(&image);
        let n = m.len();
        assert!(n > 4);
        m.remove(n - 2);
        let e = receive(&m, info).unwrap_err().0;
        assert!(e.contains("announced"), "{e}");
    }

    #[test]
    fn reordered_messages_break_the_round_trip() {
        let info = dt2();
        let mut m = msgs(&syx_of(info, &stream(info, b"0082")));
        m.swap(1, 2);
        assert!(receive(&m, info).unwrap_err().0.contains("sequence"));
        assert!(round_trip(&join(&m), info).is_err());
    }

    #[test]
    fn an_altered_message_breaks_the_round_trip_with_or_without_a_fixed_checksum() {
        let info = dt2();
        let good = msgs(&syx_of(info, &stream(info, b"0082")));
        // the payload changed, the checksum not
        let mut m = good.clone();
        m[2][60] ^= 1;
        assert!(receive(&m, info).unwrap_err().0.contains("checksum"));
        assert!(round_trip(&join(&m), info).is_err());
        // the payload changed and the packet checksum fixed: the device's receiver takes it, its content checksum does not
        let mut m = good;
        m[1][60] ^= 1;
        m[1][126] = syx::packet_checksum(&m[1][1..127], info.stream_id);
        let got = receive(&m, info).unwrap();
        assert!(
            device_validate(&got, info)
                .unwrap_err()
                .0
                .contains("checksum")
        );
        assert!(round_trip(&join(&m), info).is_err());
    }

    #[test]
    fn a_wrong_product_byte_is_refused() {
        let info = dt2();
        let mut m = msgs(&syx_of(info, &stream(info, b"0082")));
        let n = m.len();
        m[0][8] = dn2().stream_id;
        m[n - 1][8] = dn2().stream_id;
        let image = join(&m);
        assert!(
            product_check(&image, info)
                .unwrap_err()
                .0
                .contains("stream id")
        );
        assert!(receive(&m, info).unwrap_err().0.contains("refused"));
        // a DN2 stream sent where a DT2 is expected: the device byte differs too
        let other = syx_of(dn2(), &stream(dn2(), b"0062"));
        assert!(
            product_check(&other, info)
                .unwrap_err()
                .0
                .contains("device byte")
        );
    }

    #[test]
    fn the_dt2_build_floor_applies_and_the_dn2_has_none() {
        let low = stream(dt2(), b"0059");
        let image = syx_of(dt2(), &low);
        let got = receive(&msgs(&image), dt2()).unwrap();
        let e = device_validate(&got, dt2()).unwrap_err().0;
        assert!(e.contains("below the floor"), "{e}");
        assert!(round_trip(&image, dt2()).is_err());
        // the floor itself and above pass
        for b in [b"0060", b"0082"] {
            let got = receive(&msgs(&syx_of(dt2(), &stream(dt2(), b))), dt2()).unwrap();
            device_validate(&got, dt2()).unwrap();
        }
        // DN2 1.12 has no floor: the same low build is taken
        assert!(dn2().build_floor.is_none());
        let got = receive(&msgs(&syx_of(dn2(), &stream(dn2(), b"0059"))), dn2()).unwrap();
        device_validate(&got, dn2()).unwrap();
    }

    #[test]
    fn a_container_with_a_bad_hmac_is_refused_by_the_validator() {
        let info = dt2();
        let mut s = stream(info, b"0082");
        s[8 + 40] ^= 0x10; // a container byte; the content checksum is made to match again
        let total = u32::from_be_bytes(s[0..4].try_into().unwrap()) as usize;
        let sum = syx::content_checksum(&s[8..8 + total]);
        s[4..8].copy_from_slice(&sum.to_be_bytes());
        let e = device_validate(&s, info).unwrap_err().0;
        assert!(e.contains("HMAC"), "{e}");
    }

    #[test]
    fn a_short_or_empty_message_list_is_refused() {
        assert!(device_receive(&[], dt2()).is_err());
        let h = framing(dt2(), 1, 0);
        assert!(device_receive(&[&h, &h], dt2()).is_err());
    }

    #[test]
    fn the_sender_paces_and_finishes() {
        let mut s = Sender::new(3);
        assert_eq!(s.next(100), Step::Send(0));
        assert_eq!(s.next(100), Step::Wait(PACE_MS));
        assert_eq!(s.next(101), Step::Wait(PACE_MS - 1));
        assert_eq!(s.next(102), Step::Send(1));
        // a late caller is not rushed: the next message waits a full pause from its own send time
        assert_eq!(s.next(110), Step::Send(2));
        assert_eq!(s.next(110), Step::Done);
        assert_eq!((s.sent(), s.total()), (3, 3));
    }

    #[test]
    fn a_failed_output_stops_the_run_and_does_not_resume() {
        let mut s = Sender::new(5);
        assert_eq!(s.next(0), Step::Send(0));
        s.fail();
        assert_eq!(s.next(100), Step::Aborted);
        assert_eq!(s.next(1000), Step::Aborted);
        // a new run starts from the header
        let mut n = Sender::new(5);
        assert_eq!(n.next(2000), Step::Send(0));
    }

    #[test]
    fn an_empty_list_is_done() {
        assert_eq!(Sender::new(0).next(0), Step::Done);
    }
}
