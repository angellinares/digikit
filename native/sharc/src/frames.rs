//! Frame packs: replay capture frames through an [`Engine`] with no Python.
//!
//! A pack comes from `tools/sharc_transpile_run.py pack` (the image blob,
//! the start state, and each frame's DMA data). Every frame does what
//! `NativeFrames.frame` does: the DMA transfer poke, the DMA completion
//! call, then the block handler call, each run until the call returns.
//! `sharc-frames` (native) and the WebAssembly frame runner
//! (`native/sharc-jit/wasm-frames`) share this module, so both time the
//! same work.

use crate::rt::{Int, V};
use crate::sha256::Sha256;
use crate::{Engine, canon, trap_name};

struct Rd<'a> {
    b: &'a [u8],
    off: usize,
}

impl<'a> Rd<'a> {
    fn take(&mut self, n: usize) -> Result<&'a [u8], String> {
        let end = self.off.checked_add(n).filter(|&e| e <= self.b.len());
        let Some(end) = end else {
            return Err("frame pack truncated".into());
        };
        let s = &self.b[self.off..end];
        self.off = end;
        Ok(s)
    }
    fn u32(&mut self) -> Result<u32, String> {
        Ok(u32::from_le_bytes(self.take(4)?.try_into().unwrap()))
    }
    fn i64(&mut self) -> Result<i64, String> {
        Ok(i64::from_le_bytes(self.take(8)?.try_into().unwrap()))
    }
    fn blob(&mut self) -> Result<&'a [u8], String> {
        let n = self.u32()? as usize;
        self.take(n)
    }
}

/// A parsed frame pack (borrowing the pack bytes).
pub struct Pack<'a> {
    pub image: &'a [u8],
    pub state: &'a [u8],
    pub opts: Vec<(u32, i64)>,
    pub shift_src: u32,
    pub command_word: u32,
    pub ring: u32,
    pub dma_cb: u32,
    pub r8: u32,
    pub r8_value: u32,
    pub block_handler: u32,
    /// Capture frame number of `frames[0]`.
    pub first: u32,
    pub frames: Vec<&'a [u8]>,
}

/// Parse an "SHFP" version 1 frame pack.
pub fn parse(b: &[u8]) -> Result<Pack<'_>, String> {
    let mut r = Rd { b, off: 0 };
    if r.take(4)? != b"SHFP" {
        return Err("not a frame pack".into());
    }
    if r.u32()? != 1 {
        return Err("frame pack version".into());
    }
    let image = r.blob()?;
    let state = r.blob()?;
    let n = r.u32()?;
    let mut opts = Vec::new();
    for _ in 0..n {
        opts.push((r.u32()?, r.i64()?));
    }
    let mut c = [0u32; 7];
    for v in &mut c {
        *v = r.u32()?;
    }
    let first = r.u32()?;
    let n = r.u32()?;
    let mut frames = Vec::new();
    for _ in 0..n {
        frames.push(r.blob()?);
    }
    Ok(Pack {
        image,
        state,
        opts,
        shift_src: c[0],
        command_word: c[1],
        ring: c[2],
        dma_cb: c[3],
        r8: c[4],
        r8_value: c[5],
        block_handler: c[6],
        first,
        frames,
    })
}

const CALL_END: &str = "return without followed call";

/// Run the current call until it returns (the clean 9a/9b stop the
/// Python replay also ends on). Instructions, including the return.
pub fn run_call(e: &mut Engine, max: u32) -> Result<u64, String> {
    run_call_with(e, max, &mut |e: &mut Engine, n: u32| e.step(n))
}

/// `run_call` with another stepper (the JIT runtime's).
pub fn run_call_with(e: &mut Engine, max: u32, step: &mut dyn FnMut(&mut Engine, u32) -> u32) -> Result<u64, String> {
    // One step call runs until a trap halts the engine or the budget ends.
    let done = step(e, max) as u64;
    if done >= max as u64 {
        return Err("max-steps".into());
    }
    let t = e.last_trap.map(trap_name).unwrap_or_default();
    if t.contains(CALL_END)
        && (t.starts_with("sharc_core.forms_flow._type_9b_abs:")
            || t.starts_with("sharc_core.forms_flow._type_9a_abs:"))
    {
        return Ok(done + 1);
    }
    Err(format!(
        "{} (the Python core would run this instruction)",
        e.halt.clone().unwrap_or_default()
    ))
}

/// Back to the pack's start state (the image stays parsed).
pub fn reload(e: &mut Engine, p: &Pack) -> Result<(), String> {
    e.import(p.state).map_err(|c| format!("state blob ({c})"))?;
    for &(k, v) in &p.opts {
        if e.set_option(k, v) != 0 {
            return Err(format!("option {k}"));
        }
    }
    e.s.icount = 0;
    Ok(())
}

/// An engine over the pack's image, at its start state.
pub fn load(p: &Pack) -> Result<Engine, String> {
    let mut e = Engine::from_image(p.image).map_err(|c| format!("image blob ({c})"))?;
    reload(&mut e, p)?;
    Ok(e)
}

/// One frame's result: the block handler call's time (by CLOCK, in its
/// units) and its instructions.
pub struct FrameOut {
    pub handler_time: u64,
    pub instructions: u64,
}

/// Run one frame with DATA (its 4 KB DMA transfer). CLOCK is read around
/// the block handler call only.
pub fn frame(
    e: &mut Engine,
    p: &Pack,
    data: &[u8],
    clock: &dyn Fn() -> u64,
) -> Result<FrameOut, String> {
    frame_with(e, p, data, clock, &mut |e: &mut Engine, n: u32| e.step(n))
}

/// `frame` with another stepper (the JIT runtime's).
pub fn frame_with(
    e: &mut Engine,
    p: &Pack,
    data: &[u8],
    clock: &dyn Fn() -> u64,
    step: &mut dyn FnMut(&mut Engine, u32) -> u32,
) -> Result<FrameOut, String> {
    let shift = e
        .peek(p.shift_src as u64, 4)
        .map_err(|_| "shift source is an unmodelled MMR".to_string())?
        .unwrap_or(0)
        & 1;
    let base = (p.command_word as u64 + (1 - shift as u64) * p.ring as u64) & 0xFFFF_FFFF;
    if e.poke(base, data, 1) != data.len() as i32 {
        return Err("DMA transfer poke did not take effect".into());
    }
    e.fresh_call(p.dma_cb, None);
    e.set_reg(p.r8 as usize, V::c(p.r8_value as Int));
    run_call_with(e, 64, step).map_err(|w| format!("DMA completion call: {w}"))?;
    e.fresh_call(p.block_handler, None);
    let t = clock();
    let n = run_call_with(e, 4_000_000, step)?;
    let dt = clock().wrapping_sub(t);
    Ok(FrameOut {
        handler_time: dt,
        instructions: n,
    })
}

/// SHA-256 of the engine's exported canonical state (the per-frame hash
/// `sharc-frames --record/--check` keeps).
pub fn state_hash(e: &Engine) -> [u8; 32] {
    let mut h = Sha256::new();
    h.update(&canon::export_state(&e.s, false));
    h.finish()
}

pub fn hex(d: &[u8]) -> String {
    d.iter().map(|b| format!("{b:02x}")).collect()
}

/// The median of V (sorted in place); 0 when empty.
pub fn median(v: &mut [f64]) -> f64 {
    if v.is_empty() {
        return 0.0;
    }
    v.sort_by(|a, b| a.partial_cmp(b).unwrap());
    v[v.len() / 2]
}
