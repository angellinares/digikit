//! The native SHARC+ core as a [`FrameSource`]: a capture's SPI2 TX frames
//! rendered live, one SHARC frame per 32 output samples.
//!
//! Three layers:
//! - [`LivePack`]: the pack `tools/sharc_transpile_run.py live-pack` writes
//!   (the image blob, the start state as SHRD, its options, the host
//!   constants of a frame, the capture's frames, and a trailer naming the
//!   voice records to read).
//! - [`SharcRenderer`]: the pure frame step over any [`SharcCore`]: state +
//!   one TX frame -> voice 0/1 output. It does what the Python replay does
//!   per frame (`sharc_transpile_run.NativeFrames.frame`, checked against
//!   `sharc_replay`): the DMA transfer into the RX ring
//!   (`sharc_harness.write_dma_transfer`), the DMA completion call
//!   (`drive_dma_completion`), the block handler call to its return, then
//!   each voice's work buffer decimated 2:1
//!   (`read_voice_work_buffer_decimated`). No clock, no OS calls.
//! - [`CaptureSource`]: the frame sequence (capture frames, then loop or
//!   hold the last frame with its one-shot trig/release words cleared),
//!   timing and coverage counters, and the f32 conversion for the ring.
//! - [`LiveSource`]: the frames the emulated ColdFire sends, live (the
//!   player's frame queue hands one per SHARC frame, `src/repeater.rs`),
//!   rendered from a state pack's start state.

use std::sync::{Arc, Mutex};
use std::time::Instant;

use crate::ring::{FRAME_LEN, StereoSample};
use crate::source::FrameSource;

/// A peek's result (`sharc_native_peek`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Peek {
    Known(u32),
    Unknown,
    /// An unmodelled MMR.
    Mmr,
}

/// The engine counters (`sharc_native_stats`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CoreStats {
    pub instructions: u64,
    pub block_entries: u64,
    pub block_instructions: u64,
    /// Instructions run through the one-instruction interpreter (blocks the
    /// build did not specialise: coverage misses).
    pub single_steps: u64,
    pub traps: u64,
}

/// What the frame step needs from an engine: `native/sharc`'s C ABI.
pub trait SharcCore: Send {
    fn import_state(&mut self, blob: &[u8]) -> Result<(), i32>;
    fn set_option(&mut self, key: u32, value: i64) -> i32;
    fn poke(&mut self, address: u64, data: &[u8], width: u32) -> i32;
    fn peek(&mut self, address: u64, width: u32) -> Peek;
    fn fresh_call(&mut self, pc: u32);
    fn set_reg_const(&mut self, code: u32, value: u32);
    /// Packed native known-bit mask (high 32) and register value (low 32).
    fn get_reg(&mut self, code: u32) -> u64;
    /// Run up to N instructions; fewer means a halt (see `halt_reason`).
    fn step(&mut self, n: u32) -> u32;
    /// The halt reason into BUF; its length (0 when not halted).
    fn halt_reason(&mut self, buf: &mut [u8]) -> usize;
    fn stats(&mut self) -> CoreStats;
}

/// One voice record's work buffer (`sharc_harness.FIELD_WORK_BUFFER`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct VoiceTap {
    pub record: u32,
    pub offset: u32,
    pub words: u32,
}

/// The frame's host constants (`pack_frames`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct HostConstants {
    pub shift_src: u32,
    pub command_word: u32,
    pub ring: u32,
    pub dma_cb: u32,
    pub r8: u32,
    pub r8_value: u32,
    pub block_handler: u32,
}

/// A parsed live pack (see the module docs).
pub struct LivePack {
    bytes: Vec<u8>,
    image: (usize, usize),
    state: (usize, usize),
    pub options: Vec<(u32, i64)>,
    pub host: HostConstants,
    /// The capture index of the first frame.
    pub first: u32,
    frames: Vec<(usize, usize)>,
    pub voices: Vec<VoiceTap>,
    pub key: String,
    /// SHA-256 (hex) of the +Drive card image the LP0 feed in the start
    /// state came from (trailer version 2; None in a version 1 pack or when
    /// the builder named no card).
    pub card: Option<String>,
    /// SHA-256 (hex) of the tools/sharc_core sources the start state was
    /// built with (trailer version 3; None before). A library generated
    /// from other sources is refused (`check_library`).
    pub core: Option<String>,
}

/// The string value of KEY in a flat JSON object (sharc_native_info).
fn json_str<'a>(json: &'a str, key: &str) -> Option<&'a str> {
    let at = json.find(&format!("\"{key}\""))? + key.len() + 2;
    let rest = json[at..].trim_start().strip_prefix(':')?.trim_start();
    let rest = rest.strip_prefix('"')?;
    Some(&rest[..rest.find('"')?])
}

struct Rd<'a> {
    b: &'a [u8],
    off: usize,
}

impl Rd<'_> {
    fn take(&mut self, n: usize) -> Result<(usize, usize), String> {
        if self.off + n > self.b.len() {
            return Err(format!("pack truncated at byte {}", self.off));
        }
        let at = self.off;
        self.off += n;
        Ok((at, n))
    }
    fn u32(&mut self) -> Result<u32, String> {
        let (at, _) = self.take(4)?;
        Ok(u32::from_le_bytes(self.b[at..at + 4].try_into().unwrap()))
    }
    fn i64(&mut self) -> Result<i64, String> {
        let (at, _) = self.take(8)?;
        Ok(i64::from_le_bytes(self.b[at..at + 8].try_into().unwrap()))
    }
    fn blob(&mut self) -> Result<(usize, usize), String> {
        let n = self.u32()? as usize;
        self.take(n)
    }
}

impl LivePack {
    pub fn load(path: &std::path::Path) -> Result<LivePack, String> {
        let bytes = std::fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?;
        LivePack::parse(bytes)
    }

    pub fn parse(bytes: Vec<u8>) -> Result<LivePack, String> {
        let mut r = Rd { b: &bytes, off: 0 };
        let (m, _) = r.take(4)?;
        if &bytes[m..m + 4] != b"SHFP" {
            return Err("not a frame pack (magic)".into());
        }
        if r.u32()? != 1 {
            return Err("unsupported frame pack version".into());
        }
        let image = r.blob()?;
        let state = r.blob()?;
        let n = r.u32()?;
        let mut options = Vec::with_capacity(n as usize);
        for _ in 0..n {
            options.push((r.u32()?, r.i64()?));
        }
        let mut c = [0u32; 7];
        for v in &mut c {
            *v = r.u32()?;
        }
        let host = HostConstants {
            shift_src: c[0],
            command_word: c[1],
            ring: c[2],
            dma_cb: c[3],
            r8: c[4],
            r8_value: c[5],
            block_handler: c[6],
        };
        let first = r.u32()?;
        let n = r.u32()?;
        let mut frames = Vec::with_capacity(n as usize);
        for _ in 0..n {
            frames.push(r.blob()?);
        }
        let (t, _) = r
            .take(4)
            .map_err(|_| "frame pack has no live trailer (use live-pack, not pack)".to_string())?;
        if &bytes[t..t + 4] != b"SHLV" {
            return Err("frame pack has no live trailer (use live-pack, not pack)".into());
        }
        let trailer_version = r.u32()?;
        if !(1..=3).contains(&trailer_version) {
            return Err("unsupported live trailer version".into());
        }
        let nv = r.u32()?;
        let mut voices = Vec::with_capacity(nv as usize);
        for _ in 0..nv {
            voices.push(VoiceTap {
                record: r.u32()?,
                offset: r.u32()?,
                words: r.u32()?,
            });
        }
        let (k, kn) = r.blob()?;
        let key = String::from_utf8_lossy(&bytes[k..k + kn]).into_owned();
        let card = if trailer_version >= 2 {
            let (c, cn) = r.blob()?;
            Some(String::from_utf8_lossy(&bytes[c..c + cn]).into_owned()).filter(|c| !c.is_empty())
        } else {
            None
        };
        let core = if trailer_version >= 3 {
            let (c, cn) = r.blob()?;
            Some(String::from_utf8_lossy(&bytes[c..c + cn]).into_owned())
        } else {
            None
        };
        Ok(LivePack {
            bytes,
            image,
            state,
            options,
            host,
            first,
            frames,
            voices,
            key,
            card,
            core,
        })
    }

    /// Refuse a native library (its `sharc_native_info` JSON) generated
    /// from other tools/sharc_core sources than the ones this pack's start
    /// state was built with: it would run the old semantics. A pack before
    /// trailer version 3 carries no hash and is not checked.
    pub fn check_library(&self, info: &str) -> Result<(), String> {
        let Some(core) = self.core.as_deref() else {
            return Ok(());
        };
        let lib = json_str(info, "core_sha256").unwrap_or("none");
        if lib.eq_ignore_ascii_case(core) {
            return Ok(());
        }
        Err(format!(
            "stale native SHARC library: generated from tools/sharc_core {lib}, but live pack {} \
             was built with {core}; regenerate and rebuild it (the command is in \
             tools/sharc_transpile_run.py REGENERATE_HINT: tools/sharc_rsgen.py, then \
             cargo build --release --manifest-path native/sharc/Cargo.toml with SHARC_GEN_DIR)",
            self.key
        ))
    }

    /// Refuse a pack whose start state was fed from another card image
    /// than EXPECT (the running emulator's card, SHA-256 hex).
    pub fn check_card(&self, expect: &str) -> Result<(), String> {
        match self.card.as_deref() {
            Some(card) if card.eq_ignore_ascii_case(expect) => Ok(()),
            Some(card) => Err(format!(
                "live pack {} was built for card image sha256 {card}, but this run uses {expect}",
                self.key
            )),
            None => Err(format!(
                "live pack {} names no card image, so it cannot be checked against {expect}",
                self.key
            )),
        }
    }

    pub fn image(&self) -> &[u8] {
        &self.bytes[self.image.0..self.image.0 + self.image.1]
    }

    pub fn state(&self) -> &[u8] {
        &self.bytes[self.state.0..self.state.0 + self.state.1]
    }

    pub fn frame_count(&self) -> usize {
        self.frames.len()
    }

    /// Frame K of the pack (capture frame `first + K`): the byte-swapped
    /// DMA data, one ring.
    pub fn frame(&self, k: usize) -> &[u8] {
        let (at, n) = self.frames[k];
        &self.bytes[at..at + n]
    }
}

/// The Python replay's clean end of a call: the return with no followed
/// call (`sharc_transpile_run.CLEAN_CALL_END`).
const CALL_END: &[u8] = b"return without followed call";
const CLEAN_END_PREFIXES: [&[u8]; 2] = [
    b"native-trap: sharc_core.forms_flow._type_9b_abs:",
    b"native-trap: sharc_core.forms_flow._type_9a_abs:",
];
const DMA_CALL_BUDGET: u32 = 64;
const FRAME_CALL_BUDGET: u32 = 4_000_000;

/// Samples per voice per frame (the 64-float work buffer decimated 2:1).
pub const VOICE_SAMPLES: usize = FRAME_LEN;

/// How a frame ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FrameEnd {
    /// The block handler returned (the normal end).
    Clean { instructions: u64 },
    /// The DMA transfer or completion call failed.
    Dma,
    /// The handler stopped early: a trap the native core cannot take on
    /// (the Python replay also stops these frames) or its budget ran out.
    Stopped { instructions: u64 },
}

/// One frame's output: each voice's decimated work buffer, as the Python
/// replay's voice hook reads it (f64, bit for bit).
#[derive(Clone, Debug, PartialEq)]
pub struct FrameOutput {
    pub voices: Vec<[f64; VOICE_SAMPLES]>,
}

/// The pure frame step (see the module docs).
pub struct SharcRenderer<C: SharcCore> {
    core: C,
    host: HostConstants,
    voices: Vec<VoiceTap>,
    halt_buf: [u8; 512],
    work: Vec<f64>,
}

impl<C: SharcCore> SharcRenderer<C> {
    /// Import PACK's start state into CORE.
    pub fn new(mut core: C, pack: &LivePack) -> Result<Self, String> {
        load_state(&mut core, pack)?;
        Ok(SharcRenderer {
            core,
            host: pack.host,
            voices: pack.voices.clone(),
            halt_buf: [0; 512],
            work: vec![0.0; 128],
        })
    }

    /// Back to PACK's start state.
    pub fn reset(&mut self, pack: &LivePack) -> Result<(), String> {
        load_state(&mut self.core, pack)
    }

    pub fn core(&mut self) -> &mut C {
        &mut self.core
    }

    fn halted_cleanly(&mut self) -> bool {
        let n = self.core.halt_reason(&mut self.halt_buf);
        let r = &self.halt_buf[..n];
        CLEAN_END_PREFIXES.iter().any(|p| r.starts_with(p))
            && r.windows(CALL_END.len()).any(|w| w == CALL_END)
    }

    /// The last halt reason (for reports; allocates).
    pub fn halt_text(&mut self) -> String {
        let n = self.core.halt_reason(&mut self.halt_buf);
        let reason = String::from_utf8_lossy(&self.halt_buf[..n]);
        if reason.starts_with("native-trap: unmodeled MMR") {
            // UREG I0 = 16, I2 = 18, M1 = 33, M5 = 37
            // (tools/sharc_core/encoding.py). I0 is last defined by
            // `I0 = modify(I2, M1)` before the observed 0x1c1cd7 stop.
            // These are post-trap registers, not a claim about the address:
            // the instruction may have already modified I0 when it trapped.
            let i0 = self.core.get_reg(16);
            let i2 = self.core.get_reg(18);
            let m1 = self.core.get_reg(33);
            let m5 = self.core.get_reg(37);
            format!(
                "{reason}; post-trap I0={:#010x}/mask={:#010x}, I2={:#010x}/mask={:#010x}, M1={:#010x}/mask={:#010x}, M5={:#010x}/mask={:#010x}",
                i0 as u32,
                (i0 >> 32) as u32,
                i2 as u32,
                (i2 >> 32) as u32,
                m1 as u32,
                (m1 >> 32) as u32,
                m5 as u32,
                (m5 >> 32) as u32
            )
        } else {
            reason.into_owned()
        }
    }

    /// Run the current call to its return; instructions including the
    /// return, or None when it did not end cleanly.
    fn run_call(&mut self, budget: u32) -> Result<u64, u64> {
        let done = self.core.step(budget);
        if done >= budget {
            return Err(done as u64);
        }
        if self.halted_cleanly() {
            Ok(done as u64 + 1)
        } else {
            Err(done as u64)
        }
    }

    /// One SPI2 frame: DATA is the pack's (byte-swapped) DMA data. OUT gets
    /// each voice's 32 samples; zeros when the frame did not end cleanly
    /// (the replay's voice hook never fires in a frame that stops before
    /// the master stage).
    pub fn frame(&mut self, data: &[u8], out: &mut FrameOutput) -> FrameEnd {
        out.voices.resize(self.voices.len(), [0.0; VOICE_SAMPLES]);
        let h = self.host;
        let shift = match self.core.peek(h.shift_src as u64, 4) {
            Peek::Known(v) => v & 1,
            Peek::Unknown => 0,
            Peek::Mmr => return self.silent(out, FrameEnd::Dma),
        };
        let base = (h.command_word as u64 + (1 - shift as u64) * h.ring as u64) & 0xFFFF_FFFF;
        if self.core.poke(base, data, 1) != data.len() as i32 {
            return self.silent(out, FrameEnd::Dma);
        }
        self.core.fresh_call(h.dma_cb);
        self.core.set_reg_const(h.r8, h.r8_value);
        if self.run_call(DMA_CALL_BUDGET).is_err() {
            return self.silent(out, FrameEnd::Dma);
        }
        self.core.fresh_call(h.block_handler);
        match self.run_call(FRAME_CALL_BUDGET) {
            Ok(instructions) => {
                self.read_voices(out);
                FrameEnd::Clean { instructions }
            }
            Err(instructions) => self.silent(out, FrameEnd::Stopped { instructions }),
        }
    }

    fn silent(&mut self, out: &mut FrameOutput, end: FrameEnd) -> FrameEnd {
        for v in &mut out.voices {
            *v = [0.0; VOICE_SAMPLES];
        }
        end
    }

    /// `sharc_harness.read_voice_work_buffer_decimated` per voice: 64 f32
    /// words (an unknown word reads 0.0), then 0.5 * (a + b) pairwise in
    /// f64, as Python computes it.
    fn read_voices(&mut self, out: &mut FrameOutput) {
        for (vi, tap) in self.voices.iter().enumerate() {
            let words = (tap.words as usize).min(2 * VOICE_SAMPLES);
            for i in 0..words {
                let addr = tap.record as u64 + tap.offset as u64 + 4 * i as u64;
                self.work[i] = match self.core.peek(addr, 4) {
                    Peek::Known(w) => f32::from_bits(w) as f64,
                    _ => 0.0,
                };
            }
            let dst = &mut out.voices[vi];
            for (i, s) in dst.iter_mut().enumerate() {
                *s = if 2 * i + 1 < words {
                    0.5 * (self.work[2 * i] + self.work[2 * i + 1])
                } else {
                    0.0
                };
            }
        }
    }
}

fn load_state<C: SharcCore>(core: &mut C, pack: &LivePack) -> Result<(), String> {
    core.import_state(pack.state())
        .map_err(|c| format!("state import failed ({c})"))?;
    for &(k, v) in &pack.options {
        let rc = core.set_option(k, v);
        if rc != 0 {
            return Err(format!("set_option {k} = {v} failed ({rc})"));
        }
    }
    Ok(())
}

/// A copy of FRAME with its one-shot words (`repeater::ONE_SHOT_OFFSETS`)
/// cleared (a held repeat). Byte-swapping within halfwords keeps them in
/// place, so this works on wire-order and on pack (swapped) frames alike.
pub fn cleared_repeat(frame: &[u8]) -> Vec<u8> {
    let mut f = frame.to_vec();
    crate::repeater::clear_one_shot_fields(&mut f);
    f
}

/// FRAME (wire order: the ColdFire's big-endian halfwords) into DST as the
/// SHARC's receive DMA lands it: the two bytes of every halfword swapped
/// (an odd trailing byte kept), at most LIMIT bytes (one ring). The same
/// transform the pack builder applies (`sharc_harness._swap16` of
/// `tx[:RING_SIZE_BYTES]`; see `write_dma_transfer`'s docstring).
pub fn swap16_into(frame: &[u8], dst: &mut Vec<u8>, limit: usize) {
    let n = frame.len().min(limit);
    dst.clear();
    dst.extend_from_slice(&frame[..n]);
    for pair in dst.chunks_exact_mut(2) {
        pair.swap(0, 1);
    }
}

/// Voice 0 left, voice 1 right (voice 0 on both when there is one), times
/// GAIN.
fn voices_to_stereo(out: &FrameOutput, gain: f32, dst: &mut [StereoSample; FRAME_LEN]) {
    let v = &out.voices;
    for (i, s) in dst.iter_mut().enumerate() {
        let l = v.first().map_or(0.0, |x| x[i]) as f32 * gain;
        let r = v.get(1).map_or(l as f64, |x| x[i]) as f32 * gain;
        *s = StereoSample { l, r };
    }
}

/// What happens after the capture's last frame.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AfterEnd {
    /// Hold the last frame (one-shots cleared) for GAP frames, then play
    /// the capture again from its first frame, state carried on (its trig
    /// frame re-triggers the voices).
    Loop { gap: usize },
    /// Hold the last frame (one-shots cleared) for ever.
    Hold,
}

/// Render counters shared with the player (read while playing).
#[derive(Debug, Default)]
pub struct RenderLog {
    /// Per-frame render time, ns (the whole frame step, voice reads
    /// included), in render order.
    pub frame_ns: Vec<u32>,
    /// Per-frame instructions and interpreter instructions, same order.
    pub frame_instructions: Vec<u32>,
    pub frame_single_steps: Vec<u32>,
    pub frames: u64,
    pub clean: u64,
    pub stopped: u64,
    pub dma_failures: u64,
    /// Interpreter (coverage-miss) instructions, total and the frames that
    /// had any.
    pub single_steps: u64,
    pub frames_with_single_steps: u64,
    pub max_single_steps: u64,
    /// Time of the frames that had interpreter instructions, ns.
    pub single_step_frame_ns: u64,
    pub instructions: u64,
    pub first_stop: Option<(u64, String)>,
    /// Frames with a nonzero voice sample, and the first of them (a voice
    /// sounding: armed and rendering).
    pub nonzero_frames: u64,
    pub first_nonzero: Option<u64>,
    /// Frames rendered as silence without running the core, because no TX
    /// frame had arrived yet (`LiveSource` only).
    pub idle_frames: u64,
}

/// Summary of a [`RenderLog`].
#[derive(Clone, Copy, Debug, Default)]
pub struct RenderSummary {
    pub frames: u64,
    pub median_us: f64,
    pub p99_us: f64,
    pub max_us: f64,
    pub mean_us: f64,
    pub clean: u64,
    pub stopped: u64,
    pub single_steps: u64,
    pub frames_with_single_steps: u64,
}

/// A bounded diagnostic record of the exact DMA bytes a live frame used.
/// `bytes` and `sha256` are after the queue's take/repeat/merge semantics
/// and after `swap16_into`; the digest is over `bytes` alone.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RenderedInput {
    pub ordinal: u64,
    pub source: crate::repeater::TakeSource,
    pub byte_len: usize,
    pub sha256: String,
    pub bytes: Vec<u8>,
    pub frame_end: RenderedFrameEnd,
    pub stop_pc: Option<u32>,
    /// Total native instructions between the frame's counters before and after.
    pub instructions: u64,
}

/// The renderer result preserved in a [`RenderedInput`] without halt text.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RenderedFrameEnd {
    Clean,
    Dma,
    Stopped,
}

/// Optional, bounded collection of rendered DMA inputs. Once full, later
/// frames are counted in `dropped` but neither their bytes nor digest are kept.
#[derive(Debug)]
pub struct RenderedInputLog {
    max_records: usize,
    next_ordinal: u64,
    pub dropped: u64,
    records: Vec<RenderedInput>,
}

impl RenderedInputLog {
    pub fn new(max_records: usize) -> Self {
        RenderedInputLog {
            max_records,
            next_ordinal: 0,
            dropped: 0,
            records: Vec::with_capacity(max_records),
        }
    }

    pub fn records(&self) -> &[RenderedInput] {
        &self.records
    }

    fn push(
        &mut self,
        source: crate::repeater::TakeSource,
        bytes: &[u8],
        end: FrameEnd,
        stop_text: Option<&str>,
        instructions: u64,
    ) {
        let ordinal = self.next_ordinal;
        self.next_ordinal += 1;
        if self.records.len() == self.max_records {
            self.dropped += 1;
            return;
        }
        let frame_end = match end {
            FrameEnd::Clean { .. } => RenderedFrameEnd::Clean,
            FrameEnd::Dma => RenderedFrameEnd::Dma,
            FrameEnd::Stopped { .. } => RenderedFrameEnd::Stopped,
        };
        let stop_pc = matches!(end, FrameEnd::Stopped { .. })
            .then(|| stop_text.and_then(stop_pc))
            .flatten();
        self.records.push(RenderedInput {
            ordinal,
            source,
            byte_len: bytes.len(),
            sha256: sha256_hex(bytes),
            bytes: bytes.to_vec(),
            frame_end,
            stop_pc,
            instructions,
        });
    }

    /// Write newline-delimited JSON suitable for the ignored
    /// `out/native/sharc-rendered-lane/` diagnostic directory.
    pub fn write_ndjson(&self, path: &std::path::Path) -> Result<(), String> {
        use std::io::Write;

        let file = std::fs::File::create(path).map_err(|e| format!("{}: {e}", path.display()))?;
        let mut out = std::io::BufWriter::new(file);
        for r in &self.records {
            let source = match r.source {
                crate::repeater::TakeSource::Taken => "taken",
                crate::repeater::TakeSource::Repeat => "repeat",
            };
            let end = match r.frame_end {
                RenderedFrameEnd::Clean => "clean",
                RenderedFrameEnd::Dma => "dma",
                RenderedFrameEnd::Stopped => "stopped",
            };
            let stop_pc = r.stop_pc.map_or("null".to_string(), |pc| format!("{pc}"));
            writeln!(out, "{{\"ordinal\":{},\"source\":\"{source}\",\"byte_len\":{},\"sha256\":\"{}\",\"bytes_hex\":\"{}\",\"frame_end\":\"{end}\",\"stop_pc\":{stop_pc},\"instructions\":{}}}", r.ordinal, r.byte_len, r.sha256, hex(&r.bytes), r.instructions).map_err(|e| format!("{}: {e}", path.display()))?;
        }
        out.flush().map_err(|e| format!("{}: {e}", path.display()))
    }
}

fn stop_pc(reason: &str) -> Option<u32> {
    let (_, hex) = reason.rsplit_once(" at 0x")?;
    let digits: String = hex.chars().take_while(|c| c.is_ascii_hexdigit()).collect();
    u32::from_str_radix(&digits, 16).ok()
}

fn hex(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        use std::fmt::Write;
        write!(out, "{b:02x}").unwrap();
    }
    out
}

fn sha256_hex(data: &[u8]) -> String {
    const K: [u32; 64] = [
        0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4,
        0xab1c5ed5, 0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe,
        0x9bdc06a7, 0xc19bf174, 0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f,
        0x4a7484aa, 0x5cb0a9dc, 0x76f988da, 0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7,
        0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967, 0x27b70a85, 0x2e1b2138, 0x4d2c6dfc,
        0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85, 0xa2bfe8a1, 0xa81a664b,
        0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070, 0x19a4c116,
        0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
        0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7,
        0xc67178f2,
    ];
    let mut padded = data.to_vec();
    let bits = (padded.len() as u64).wrapping_mul(8);
    padded.push(0x80);
    while padded.len() % 64 != 56 {
        padded.push(0);
    }
    padded.extend_from_slice(&bits.to_be_bytes());
    let mut h: [u32; 8] = [
        0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab,
        0x5be0cd19,
    ];
    for block in padded.as_chunks::<64>().0 {
        let mut w = [0u32; 64];
        for i in 0..16 {
            w[i] = u32::from_be_bytes(block[4 * i..4 * i + 4].try_into().unwrap());
        }
        for i in 16..64 {
            let s0 = w[i - 15].rotate_right(7) ^ w[i - 15].rotate_right(18) ^ (w[i - 15] >> 3);
            let s1 = w[i - 2].rotate_right(17) ^ w[i - 2].rotate_right(19) ^ (w[i - 2] >> 10);
            w[i] = w[i - 16]
                .wrapping_add(s0)
                .wrapping_add(w[i - 7])
                .wrapping_add(s1);
        }
        let mut v = h;
        for i in 0..64 {
            let t1 = v[7]
                .wrapping_add(v[4].rotate_right(6) ^ v[4].rotate_right(11) ^ v[4].rotate_right(25))
                .wrapping_add((v[4] & v[5]) ^ (!v[4] & v[6]))
                .wrapping_add(K[i])
                .wrapping_add(w[i]);
            let t2 = (v[0].rotate_right(2) ^ v[0].rotate_right(13) ^ v[0].rotate_right(22))
                .wrapping_add((v[0] & v[1]) ^ (v[0] & v[2]) ^ (v[1] & v[2]));
            v = [
                t1.wrapping_add(t2),
                v[0],
                v[1],
                v[2],
                v[3].wrapping_add(t1),
                v[4],
                v[5],
                v[6],
            ];
        }
        for i in 0..8 {
            h[i] = h[i].wrapping_add(v[i]);
        }
    }
    hex(&h.into_iter().flat_map(u32::to_be_bytes).collect::<Vec<_>>())
}

impl RenderLog {
    /// Record one core frame: its time NS, the core counters BEFORE and
    /// AFTER it, how it ended, the halt text of a frame that did not end
    /// cleanly, and whether any voice sample was nonzero.
    fn record(
        &mut self,
        ns: u32,
        before: CoreStats,
        after: CoreStats,
        end: FrameEnd,
        stop_text: Option<String>,
        nonzero: bool,
    ) {
        let ss = after.single_steps - before.single_steps;
        if self.frame_ns.len() < LOG_RESERVE {
            self.frame_ns.push(ns);
            let n = (after.instructions - before.instructions).min(u32::MAX as u64) as u32;
            self.frame_instructions.push(n);
            self.frame_single_steps.push(ss.min(u32::MAX as u64) as u32);
        }
        self.frames += 1;
        self.single_steps += ss;
        self.instructions += after.instructions - before.instructions;
        if ss > 0 {
            self.frames_with_single_steps += 1;
            self.single_step_frame_ns += ns as u64;
            self.max_single_steps = self.max_single_steps.max(ss);
        }
        match end {
            FrameEnd::Clean { .. } => self.clean += 1,
            FrameEnd::Stopped { .. } => self.stopped += 1,
            FrameEnd::Dma => self.dma_failures += 1,
        }
        if let Some(t) = stop_text
            && self.first_stop.is_none()
        {
            self.first_stop = Some((self.frames - 1, t));
        }
        if nonzero {
            self.nonzero_frames += 1;
            self.first_nonzero.get_or_insert(self.frames - 1);
        }
    }

    pub fn summary(&self, skip_first: usize) -> RenderSummary {
        let mut v: Vec<u32> = self.frame_ns.iter().skip(skip_first).copied().collect();
        v.sort_unstable();
        let pick = |q: f64| -> f64 {
            if v.is_empty() {
                0.0
            } else {
                let i = ((v.len() - 1) as f64 * q).round() as usize;
                v[i] as f64 / 1000.0
            }
        };
        let mean = if v.is_empty() {
            0.0
        } else {
            v.iter().map(|&x| x as f64).sum::<f64>() / v.len() as f64 / 1000.0
        };
        RenderSummary {
            frames: self.frames,
            median_us: pick(0.5),
            p99_us: pick(0.99),
            max_us: pick(1.0),
            mean_us: mean,
            clean: self.clean,
            stopped: self.stopped,
            single_steps: self.single_steps,
            frames_with_single_steps: self.frames_with_single_steps,
        }
    }
}

/// The capture played through a [`SharcRenderer`] (see the module docs).
pub struct CaptureSource<C: SharcCore> {
    renderer: SharcRenderer<C>,
    pack: Arc<LivePack>,
    held: Vec<u8>,
    after: AfterEnd,
    /// Position: frames 0..frame_count are the capture, then the held gap.
    pos: usize,
    gain: f32,
    out: FrameOutput,
    log: Arc<Mutex<RenderLog>>,
}

/// Frame-time slots reserved up front (10 min of frames), so recording a
/// frame's time does not allocate while playing.
const LOG_RESERVE: usize = 10 * 60 * 1500;

fn new_log() -> RenderLog {
    RenderLog {
        frame_ns: Vec::with_capacity(LOG_RESERVE),
        frame_instructions: Vec::with_capacity(LOG_RESERVE),
        frame_single_steps: Vec::with_capacity(LOG_RESERVE),
        ..RenderLog::default()
    }
}

fn any_nonzero(out: &FrameOutput) -> bool {
    out.voices.iter().any(|v| v.iter().any(|&s| s != 0.0))
}

impl<C: SharcCore> CaptureSource<C> {
    pub fn new(core: C, pack: Arc<LivePack>, after: AfterEnd, gain: f32) -> Result<Self, String> {
        if pack.frame_count() == 0 {
            return Err("pack has no frames".into());
        }
        let renderer = SharcRenderer::new(core, &pack)?;
        let held = cleared_repeat(pack.frame(pack.frame_count() - 1));
        let log = new_log();
        Ok(CaptureSource {
            renderer,
            held,
            after,
            pos: 0,
            gain,
            out: FrameOutput { voices: Vec::new() },
            log: Arc::new(Mutex::new(log)),
            pack,
        })
    }

    pub fn log(&self) -> Arc<Mutex<RenderLog>> {
        Arc::clone(&self.log)
    }

    /// The capture frame index the next render plays (None: a held frame).
    pub fn next_capture_frame(&self) -> Option<u32> {
        (self.pos < self.pack.frame_count()).then(|| self.pack.first + self.pos as u32)
    }

    /// Render the next frame of the sequence into self.out; its end.
    pub fn step(&mut self) -> FrameEnd {
        let n = self.pack.frame_count();
        let before = self.renderer.core().stats();
        let t0 = Instant::now();
        let end = if self.pos < n {
            let pack = Arc::clone(&self.pack);
            self.renderer.frame(pack.frame(self.pos), &mut self.out)
        } else {
            self.renderer.frame(&self.held, &mut self.out)
        };
        let ns = t0.elapsed().as_nanos().min(u32::MAX as u128) as u32;
        let after = self.renderer.core().stats();
        self.pos += 1;
        match self.after {
            AfterEnd::Loop { gap } if self.pos >= n + gap => self.pos = 0,
            AfterEnd::Hold if self.pos > n => self.pos = n,
            _ => {}
        }
        let stop_text = match end {
            FrameEnd::Clean { .. } => None,
            _ => Some(self.renderer.halt_text()),
        };
        let nonzero = any_nonzero(&self.out);
        self.log
            .lock()
            .expect("render log poisoned")
            .record(ns, before, after, end, stop_text, nonzero);
        end
    }

    /// The last rendered frame's output.
    pub fn output(&self) -> &FrameOutput {
        &self.out
    }
}

impl<C: SharcCore> FrameSource for CaptureSource<C> {
    /// INPUT_FRAME is ignored: the capture supplies the TX frames.
    fn render_frame(&mut self, _input_frame: &[u8], out: &mut [StereoSample; FRAME_LEN]) {
        self.step();
        voices_to_stereo(&self.out, self.gain, out);
    }
}

/// The emulated ColdFire's frames rendered live (see the module docs):
/// each `render_frame` input is the next wire-order TX frame from the
/// player's queue, or the last one again with its one-shot words cleared.
/// Before the first frame arrives the output is silence and the core does
/// not run.
pub struct LiveSource<C: SharcCore> {
    renderer: SharcRenderer<C>,
    /// One ring's bytes (the pack's `ring` host constant).
    ring_bytes: usize,
    dma: Vec<u8>,
    gain: f32,
    out: FrameOutput,
    log: Arc<Mutex<RenderLog>>,
    rendered_inputs: Option<Arc<Mutex<RenderedInputLog>>>,
}

impl<C: SharcCore> LiveSource<C> {
    /// Start from PACK's state (its frames, if any, are not played).
    pub fn new(core: C, pack: &LivePack, gain: f32) -> Result<Self, String> {
        let renderer = SharcRenderer::new(core, pack)?;
        let ring_bytes = pack.host.ring as usize;
        Ok(LiveSource {
            renderer,
            ring_bytes,
            dma: Vec::with_capacity(ring_bytes),
            gain,
            out: FrameOutput { voices: Vec::new() },
            log: Arc::new(Mutex::new(new_log())),
            rendered_inputs: None,
        })
    }

    /// Enable bounded recording of post-swap DMA inputs for this source.
    /// Normal playback leaves this disabled, avoiding byte copies, hashing,
    /// diagnostic locking, and artifact writes.
    pub fn enable_rendered_input_log(
        &mut self,
        max_records: usize,
    ) -> Arc<Mutex<RenderedInputLog>> {
        crate::repeater::enable_render_diagnostics();
        let log = Arc::new(Mutex::new(RenderedInputLog::new(max_records)));
        self.rendered_inputs = Some(Arc::clone(&log));
        log
    }

    pub fn log(&self) -> Arc<Mutex<RenderLog>> {
        Arc::clone(&self.log)
    }

    /// Render one wire-order TX frame into self.out; its end (None: FRAME
    /// is empty, nothing ran, the output is silence).
    pub fn step(&mut self, frame: &[u8]) -> Option<FrameEnd> {
        if frame.is_empty() {
            for v in &mut self.out.voices {
                *v = [0.0; VOICE_SAMPLES];
            }
            self.log.lock().expect("render log poisoned").idle_frames += 1;
            return None;
        }
        swap16_into(frame, &mut self.dma, self.ring_bytes);
        let before = self.renderer.core().stats();
        let t0 = Instant::now();
        let end = self.renderer.frame(&self.dma, &mut self.out);
        let ns = t0.elapsed().as_nanos().min(u32::MAX as u128) as u32;
        let after = self.renderer.core().stats();
        let stop_text = match end {
            FrameEnd::Clean { .. } => None,
            _ => Some(self.renderer.halt_text()),
        };
        let nonzero = any_nonzero(&self.out);
        self.log.lock().expect("render log poisoned").record(
            ns,
            before,
            after,
            end,
            stop_text.clone(),
            nonzero,
        );
        if let Some(log) = &self.rendered_inputs {
            // The queue sets this immediately before the FrameSource call.
            // Direct callers of `step` supply a frame as a taken frame.
            let source = crate::repeater::take_source_for_render()
                .unwrap_or(crate::repeater::TakeSource::Taken);
            log.lock().expect("rendered input log poisoned").push(
                source,
                &self.dma,
                end,
                stop_text.as_deref(),
                after.instructions - before.instructions,
            );
        }
        Some(end)
    }

    /// The last rendered frame's output.
    pub fn output(&self) -> &FrameOutput {
        &self.out
    }
}

impl<C: SharcCore> FrameSource for LiveSource<C> {
    fn render_frame(&mut self, input_frame: &[u8], out: &mut [StereoSample; FRAME_LEN]) {
        if self.step(input_frame).is_none() {
            *out = [StereoSample::default(); FRAME_LEN];
            return;
        }
        voices_to_stereo(&self.out, self.gain, out);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A scripted engine: records calls, returns scripted halts, and holds
    /// a small word memory for peeks.
    #[derive(Default)]
    struct Mock {
        calls: Vec<String>,
        mem: std::collections::HashMap<u64, u32>,
        regs: std::collections::HashMap<u32, u64>,
        halt: String,
        /// Instructions each handler call takes; None: budget exhausted.
        handler_steps: Option<u32>,
        handler_halt: String,
        in_handler: bool,
        steps: u64,
    }

    impl SharcCore for Mock {
        fn import_state(&mut self, blob: &[u8]) -> Result<(), i32> {
            self.calls.push(format!("import {}", blob.len()));
            Ok(())
        }
        fn set_option(&mut self, key: u32, value: i64) -> i32 {
            self.calls.push(format!("opt {key}={value}"));
            0
        }
        fn poke(&mut self, address: u64, data: &[u8], _width: u32) -> i32 {
            self.calls.push(format!("poke {address:#x} {}", data.len()));
            data.len() as i32
        }
        fn peek(&mut self, address: u64, _width: u32) -> Peek {
            self.mem
                .get(&address)
                .map_or(Peek::Unknown, |&v| Peek::Known(v))
        }
        fn fresh_call(&mut self, pc: u32) {
            self.calls.push(format!("call {pc:#x}"));
            self.in_handler = pc == 0x1000;
        }
        fn set_reg_const(&mut self, code: u32, value: u32) {
            self.calls.push(format!("reg {code}={value:#x}"));
        }
        fn get_reg(&mut self, code: u32) -> u64 {
            self.regs.get(&code).copied().unwrap_or(0)
        }
        fn step(&mut self, n: u32) -> u32 {
            let clean = "native-trap: sharc_core.forms_flow._type_9b_abs: \
                         return without followed call at 0x1c75d3";
            if self.in_handler {
                self.halt = self.handler_halt.clone();
                let s = self.handler_steps.unwrap_or(n);
                self.steps += s as u64;
                s
            } else {
                self.halt = clean.to_string();
                3
            }
        }
        fn halt_reason(&mut self, buf: &mut [u8]) -> usize {
            let b = self.halt.as_bytes();
            let n = b.len().min(buf.len());
            buf[..n].copy_from_slice(&b[..n]);
            n
        }
        fn stats(&mut self) -> CoreStats {
            CoreStats {
                instructions: self.steps,
                ..CoreStats::default()
            }
        }
    }

    const CLEAN: &str = "native-trap: sharc_core.forms_flow._type_9a_abs: \
                         return without followed call at 0x1c75d3";

    fn pack(frames: &[&[u8]]) -> LivePack {
        let mut b = Vec::new();
        b.extend_from_slice(b"SHFP");
        b.extend_from_slice(&1u32.to_le_bytes());
        for blob in [&b"IMG"[..], &b"STATE"[..]] {
            b.extend_from_slice(&(blob.len() as u32).to_le_bytes());
            b.extend_from_slice(blob);
        }
        b.extend_from_slice(&1u32.to_le_bytes());
        b.extend_from_slice(&10u32.to_le_bytes());
        b.extend_from_slice(&7i64.to_le_bytes());
        // shift_src, command_word, ring, dma_cb, r8, r8_value, block_handler
        for v in [0x500u32, 0x8000, 0x1000, 0x2000, 8, 0x77, 0x1000] {
            b.extend_from_slice(&v.to_le_bytes());
        }
        b.extend_from_slice(&74u32.to_le_bytes());
        b.extend_from_slice(&(frames.len() as u32).to_le_bytes());
        for f in frames {
            b.extend_from_slice(&(f.len() as u32).to_le_bytes());
            b.extend_from_slice(f);
        }
        b.extend_from_slice(b"SHLV");
        b.extend_from_slice(&1u32.to_le_bytes());
        b.extend_from_slice(&2u32.to_le_bytes());
        for rec in [0x100u32, 0x400] {
            for v in [rec, 4, 64] {
                b.extend_from_slice(&v.to_le_bytes());
            }
        }
        b.extend_from_slice(&3u32.to_le_bytes());
        b.extend_from_slice(b"key");
        LivePack::parse(b).unwrap()
    }

    fn mock() -> Mock {
        Mock {
            handler_steps: Some(100),
            handler_halt: CLEAN.to_string(),
            ..Mock::default()
        }
    }

    #[test]
    fn pack_parses_with_trailer() {
        let p = pack(&[&[1, 2], &[3, 4, 5]]);
        assert_eq!(p.image(), b"IMG");
        assert_eq!(p.state(), b"STATE");
        assert_eq!(p.options, vec![(10, 7)]);
        assert_eq!(p.first, 74);
        assert_eq!(p.frame_count(), 2);
        assert_eq!(p.frame(1), &[3, 4, 5]);
        assert_eq!(p.voices[1].record, 0x400);
        assert_eq!(p.key, "key");
        assert_eq!(p.host.block_handler, 0x1000);
    }

    #[test]
    fn pack_without_trailer_is_rejected() {
        let p = pack(&[&[1]]);
        let mut b = p.bytes.clone();
        b.truncate(b.len() - 4 - 4 - 4 - 24 - 7);
        assert!(LivePack::parse(b).is_err());
    }

    #[test]
    fn frame_does_the_replay_host_steps_in_order() {
        let p = pack(&[&[9; 8]]);
        let mut m = mock();
        m.mem.insert(0x500, 1); // shift 1: the ring at command_word + 0
        let mut r = SharcRenderer::new(m, &p).unwrap();
        let mut out = FrameOutput { voices: vec![] };
        let end = r.frame(p.frame(0), &mut out);
        assert_eq!(end, FrameEnd::Clean { instructions: 101 });
        let calls = &r.core().calls;
        assert_eq!(
            calls,
            &[
                "import 5",
                "opt 10=7",
                "poke 0x8000 8",
                "call 0x2000",
                "reg 8=0x77",
                "call 0x1000",
            ]
        );
    }

    #[test]
    fn shift_zero_writes_the_other_ring_half() {
        let p = pack(&[&[9; 4]]);
        let mut r = SharcRenderer::new(mock(), &p).unwrap();
        let mut out = FrameOutput { voices: vec![] };
        r.frame(p.frame(0), &mut out);
        assert!(r.core().calls.contains(&"poke 0x9000 4".to_string()));
    }

    #[test]
    fn voices_are_decimated_like_python() {
        let p = pack(&[&[0; 4]]);
        let mut m = mock();
        let a = 0.1f32;
        let b = 0.3f32;
        m.mem.insert(0x100 + 4, a.to_bits());
        m.mem.insert(0x100 + 8, b.to_bits());
        // voice 1 word 0 known, word 1 unknown (reads 0.0)
        m.mem.insert(0x400 + 4, (-0.5f32).to_bits());
        let mut r = SharcRenderer::new(m, &p).unwrap();
        let mut out = FrameOutput { voices: vec![] };
        r.frame(p.frame(0), &mut out);
        assert_eq!(out.voices.len(), 2);
        assert_eq!(
            out.voices[0][0].to_bits(),
            (0.5 * (a as f64 + b as f64)).to_bits()
        );
        assert_eq!(out.voices[0][1], 0.0);
        assert_eq!(out.voices[1][0], -0.25);
    }

    #[test]
    fn a_stopped_frame_is_silent_and_reported() {
        let p = pack(&[&[0; 4]]);
        let mut m = mock();
        m.mem.insert(0x104, 1.0f32.to_bits());
        m.handler_halt = "native-trap: unmodelled MMR 0x30000 at 0x1c1cf9".into();
        let mut r = SharcRenderer::new(m, &p).unwrap();
        let mut out = FrameOutput { voices: vec![] };
        assert_eq!(
            r.frame(p.frame(0), &mut out),
            FrameEnd::Stopped { instructions: 100 }
        );
        assert!(out.voices.iter().all(|v| v.iter().all(|&s| s == 0.0)));
    }

    #[test]
    fn unmodeled_mmr_stop_reports_post_trap_registers_without_guessing_address() {
        let p = pack(&[&[0; 4]]);
        let mut m = mock();
        m.handler_halt = "native-trap: unmodeled MMR at 0x1c1cd7".into();
        m.regs.insert(16, (0xffff_ffff_u64 << 32) | 0x30000);
        m.regs.insert(18, (0xffff_ffff_u64 << 32) | 0x2fff8);
        m.regs.insert(33, (0xffff_ffff_u64 << 32) | 8);
        m.regs.insert(37, 0xffff_ff00_u64 << 32);
        let mut r = SharcRenderer::new(m, &p).unwrap();
        let mut out = FrameOutput { voices: vec![] };
        assert!(matches!(
            r.frame(p.frame(0), &mut out),
            FrameEnd::Stopped { .. }
        ));
        assert_eq!(
            r.halt_text(),
            "native-trap: unmodeled MMR at 0x1c1cd7; post-trap \
             I0=0x00030000/mask=0xffffffff, I2=0x0002fff8/mask=0xffffffff, \
             M1=0x00000008/mask=0xffffffff, M5=0x00000000/mask=0xffffff00"
        );
        assert!(out.voices.iter().all(|v| v.iter().all(|&s| s == 0.0)));
    }

    #[test]
    fn budget_exhaustion_is_a_stop() {
        let p = pack(&[&[0; 4]]);
        let mut m = mock();
        m.handler_steps = None;
        let mut r = SharcRenderer::new(m, &p).unwrap();
        let mut out = FrameOutput { voices: vec![] };
        assert!(matches!(
            r.frame(p.frame(0), &mut out),
            FrameEnd::Stopped { .. }
        ));
    }

    #[test]
    fn cleared_repeat_zeroes_the_one_shot_words_only() {
        let f: Vec<u8> = (0..0x30).map(|i| i as u8 | 0x80).collect();
        let c = cleared_repeat(&f);
        for (i, (&x, &y)) in f.iter().zip(&c).enumerate() {
            if (0x22..0x2c).contains(&i) {
                assert_eq!(y, 0, "byte {i:#x}");
            } else {
                assert_eq!(x, y, "byte {i:#x}");
            }
        }
    }

    #[test]
    fn swap16_swaps_each_halfword_and_truncates_to_one_ring() {
        let mut d = Vec::new();
        swap16_into(&[0x00, 0x03, 0x12, 0x34, 0x56], &mut d, 16);
        assert_eq!(d, vec![0x03, 0x00, 0x34, 0x12, 0x56]);
        swap16_into(&[1, 2, 3, 4, 5, 6], &mut d, 4);
        assert_eq!(d, vec![2, 1, 4, 3]);
    }

    #[test]
    fn a_version_2_trailer_names_the_card_and_is_checked() {
        let p = pack(&[&[1, 2]]);
        assert_eq!(p.card, None);
        assert!(p.check_card("ab").is_err(), "no card: refused");
        let mut b = p.bytes.clone();
        // trailer version 1 -> 2, then append the card blob
        let at = b.windows(4).rposition(|w| w == b"SHLV").unwrap() + 4;
        b[at..at + 4].copy_from_slice(&2u32.to_le_bytes());
        b.extend_from_slice(&4u32.to_le_bytes());
        b.extend_from_slice(b"beef");
        let p2 = LivePack::parse(b).unwrap();
        assert_eq!(p2.card.as_deref(), Some("beef"));
        assert!(p2.check_card("BEEF").is_ok());
        let e = p2.check_card("cafe").unwrap_err();
        assert!(e.contains("beef") && e.contains("cafe"), "{e}");
    }

    #[test]
    fn a_version_3_trailer_refuses_a_library_from_another_core() {
        let p = pack(&[&[1, 2]]);
        assert_eq!(p.core, None);
        assert!(
            p.check_library("{}").is_ok(),
            "no hash in the pack: not checked"
        );
        let mut b = p.bytes.clone();
        // trailer version 1 -> 3: an empty card blob, then the core hash
        let at = b.windows(4).rposition(|w| w == b"SHLV").unwrap() + 4;
        b[at..at + 4].copy_from_slice(&3u32.to_le_bytes());
        b.extend_from_slice(&0u32.to_le_bytes());
        b.extend_from_slice(&4u32.to_le_bytes());
        b.extend_from_slice(b"c0de");
        let p3 = LivePack::parse(b).unwrap();
        assert_eq!(p3.card, None);
        assert_eq!(p3.core.as_deref(), Some("c0de"));
        let ok = r#"{"core_sha256": "C0DE", "generator_version": 1, "blocks": 3}"#;
        assert!(p3.check_library(ok).is_ok());
        let stale = r#"{"core_sha256": "beef", "generator_version": 1}"#;
        let e = p3.check_library(stale).unwrap_err();
        assert!(
            e.contains("stale") && e.contains("beef") && e.contains("c0de"),
            "{e}"
        );
        assert!(
            e.contains("sharc_rsgen.py"),
            "names the regenerate command: {e}"
        );
        assert!(
            p3.check_library("{}").is_err(),
            "a library without a hash is refused"
        );
    }

    #[test]
    fn json_str_reads_a_flat_object() {
        let j = r#"{"core_sha256": "ab12", "generator_version": 0, "image_sha256":"ff"}"#;
        assert_eq!(json_str(j, "core_sha256"), Some("ab12"));
        assert_eq!(json_str(j, "image_sha256"), Some("ff"));
        assert_eq!(json_str(j, "generator_version"), None);
        assert_eq!(json_str(j, "missing"), None);
    }

    #[test]
    fn live_source_is_silent_and_idle_until_the_first_frame() {
        let p = pack(&[]);
        let mut s = LiveSource::new(mock(), &p, 1.0).unwrap();
        let mut out = [StereoSample { l: 9.0, r: 9.0 }; FRAME_LEN];
        s.render_frame(&[], &mut out);
        assert!(out.iter().all(|x| *x == StereoSample::default()));
        let log = s.log();
        let log = log.lock().unwrap();
        assert_eq!((log.idle_frames, log.frames), (1, 0));
        drop(log);
        assert!(
            !s.renderer
                .core()
                .calls
                .iter()
                .any(|c| c.starts_with("poke"))
        );
    }

    #[test]
    fn live_source_swaps_the_wire_frame_into_the_ring() {
        let p = pack(&[]);
        let mut m = mock();
        m.mem.insert(0x104, 0.5f32.to_bits());
        m.mem.insert(0x108, 0.5f32.to_bits());
        let mut s = LiveSource::new(m, &p, 1.0).unwrap();
        let mut out = [StereoSample::default(); FRAME_LEN];
        s.render_frame(&[0x00, 0x03, 0xAB, 0xCD], &mut out);
        assert!(
            s.renderer
                .core()
                .calls
                .contains(&"poke 0x9000 4".to_string())
        );
        assert_eq!(s.dma, vec![0x03, 0x00, 0xCD, 0xAB]);
        assert_eq!(out[0].l, 0.5);
        let log = s.log();
        let log = log.lock().unwrap();
        assert_eq!((log.frames, log.clean, log.nonzero_frames), (1, 1, 1));
        assert_eq!(log.first_nonzero, Some(0));
    }

    #[test]
    fn rendered_input_log_records_post_swap_taken_and_repeat_bytes() {
        let p = pack(&[]);
        let mut s = LiveSource::new(mock(), &p, 1.0).unwrap();
        let inputs = s.enable_rendered_input_log(1);
        // Exercise the actual offline player -> queue -> source seam, rather
        // than calling `step` with bytes the queue never produced.
        let player = crate::player::LivePlayer::offline(Box::new(s));
        let mut out = [StereoSample::default(); FRAME_LEN];
        player.push_frame(&[0x00, 0x03, 0xAB, 0xCD]);
        assert_eq!(player.render_offline(&mut out).unwrap(), 1);
        assert_eq!(player.render_offline(&mut out).unwrap(), 1);
        let inputs = inputs.lock().unwrap();
        assert_eq!(inputs.records().len(), 1);
        assert_eq!(inputs.dropped, 1);
        let record = &inputs.records()[0];
        assert_eq!(record.ordinal, 0);
        assert_eq!(record.source, crate::repeater::TakeSource::Taken);
        assert_eq!(record.bytes, [0x03, 0x00, 0xCD, 0xAB]);
        assert_eq!(record.byte_len, 4);
        assert_eq!(
            record.sha256,
            "6fae2c3fca6a7e597d6b9e125c1ea9894a5a76f96d691cf95b7571c7518d0451"
        );
        assert_eq!(record.frame_end, RenderedFrameEnd::Clean);
        assert_eq!(record.stop_pc, None);
        assert_eq!(record.instructions, 100);
    }

    #[test]
    fn diagnostic_sha256_covers_multiple_compression_blocks() {
        assert_eq!(
            sha256_hex(&[b'a'; 64]),
            "ffe054fe7ae0cb6dc65c3af9b61d5209f439851db43d0ba5997337df154668eb"
        );
        assert_eq!(
            sha256_hex(&vec![b'a'; 2748]),
            "4ec2234febfd986ca1ad542de49ff7445074c67e4583295a296d01128fcc1fd1"
        );
    }

    #[test]
    fn rendered_input_stop_records_the_trap_pc() {
        let p = pack(&[]);
        let mut m = mock();
        m.handler_halt = "native-trap: unmodeled MMR at 0x1c1cd7".into();
        let mut s = LiveSource::new(m, &p, 1.0).unwrap();
        let inputs = s.enable_rendered_input_log(1);
        s.step(&[1, 2]).unwrap();
        let inputs = inputs.lock().unwrap();
        let record = &inputs.records()[0];
        assert_eq!(record.frame_end, RenderedFrameEnd::Stopped);
        assert_eq!(record.stop_pc, Some(0x1c1cd7));
        assert_eq!(record.instructions, 100);
    }

    fn pokes(src: &mut CaptureSource<Mock>) -> Vec<String> {
        src.renderer
            .core()
            .calls
            .iter()
            .filter(|c| c.starts_with("poke"))
            .cloned()
            .collect()
    }

    #[test]
    fn loop_plays_capture_then_gap_then_again() {
        let f0 = vec![1u8; 0x28];
        let f1 = vec![2u8; 0x30];
        let p = Arc::new(pack(&[&f0, &f1]));
        let mut s = CaptureSource::new(mock(), p, AfterEnd::Loop { gap: 1 }, 1.0).unwrap();
        let mut order = Vec::new();
        for _ in 0..5 {
            order.push(s.next_capture_frame());
            s.step();
        }
        assert_eq!(order, vec![Some(74), Some(75), None, Some(74), Some(75)]);
        let lens: Vec<String> = pokes(&mut s);
        // the held gap frame is frame 1 (0x30 bytes) with its one-shots cleared
        assert_eq!(lens.len(), 5);
        assert!(lens[2].ends_with(" 48"));
        assert_eq!(s.log().lock().unwrap().frames, 5);
        assert_eq!(s.log().lock().unwrap().frame_ns.len(), 5);
    }

    #[test]
    fn hold_repeats_the_last_frame_for_ever() {
        let p = Arc::new(pack(&[&[1; 0x28]]));
        let mut s = CaptureSource::new(mock(), p, AfterEnd::Hold, 1.0).unwrap();
        let order: Vec<_> = (0..4)
            .map(|_| {
                let k = s.next_capture_frame();
                s.step();
                k
            })
            .collect();
        assert_eq!(order, vec![Some(74), None, None, None]);
    }

    #[test]
    fn render_frame_maps_voice_0_left_voice_1_right() {
        let p = Arc::new(pack(&[&[0; 4]]));
        let mut m = mock();
        m.mem.insert(0x104, 0.5f32.to_bits());
        m.mem.insert(0x108, 0.5f32.to_bits());
        m.mem.insert(0x404, (-0.25f32).to_bits());
        m.mem.insert(0x408, (-0.25f32).to_bits());
        let mut s = CaptureSource::new(m, p, AfterEnd::Hold, 2.0).unwrap();
        let mut out = [StereoSample::default(); FRAME_LEN];
        s.render_frame(&[], &mut out);
        assert_eq!(out[0], StereoSample { l: 1.0, r: -0.5 });
        assert_eq!(out[1], StereoSample { l: 0.0, r: 0.0 });
    }

    #[test]
    fn summary_percentiles() {
        let log = RenderLog {
            frame_ns: (1..=100).map(|x| x * 1000).collect(),
            frames: 100,
            ..RenderLog::default()
        };
        let s = log.summary(0);
        assert_eq!(s.max_us, 100.0);
        assert_eq!(s.p99_us, 99.0);
        assert!((s.median_us - 50.0).abs() <= 1.0);
    }
}
