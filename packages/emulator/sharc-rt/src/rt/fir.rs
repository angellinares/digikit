//! Opt-in FIR accelerator (FIR0): TCB chains, single-rate windows, status and interrupt.
//!
//! Hand-written twin of tools/sharc_dev/sharc_core/fir.py; keep the two in step (that
//! module's docstring is the specification, from ADSP-2156x HWR chapter 38).
//! A rejected case traps with TRAP_PERIPHERAL (the Python core then raises
//! ValueError for the same case). Each product and each sum is an f32
//! operation, round to nearest even, denormals kept. The DMA helpers and the
//! unjournaled run come from rt/iir.rs, for the same reasons.

use super::iir::{load_tcb, position, put, step, word_read, word_write};
use super::*;

pub const FIR0: u32 = 0x310C_3000;
const FIR_CTL1: u32 = FIR0;
const FIR_DMASTAT: u32 = FIR0 + 0x04;
const FIR_MACSTAT: u32 = FIR0 + 0x08;
const FIR_CTL2: u32 = FIR0 + 0x40;
const FIR_INIDX: u32 = FIR0 + 0x44;
const FIR_INMOD: u32 = FIR0 + 0x48;
const FIR_INCNT: u32 = FIR0 + 0x4C;
const FIR_INBASE: u32 = FIR0 + 0x50;
const FIR_OUTIDX: u32 = FIR0 + 0x54;
const FIR_OUTMOD: u32 = FIR0 + 0x58;
const FIR_OUTCNT: u32 = FIR0 + 0x5C;
const FIR_OUTBASE: u32 = FIR0 + 0x60;
const FIR_COEFIDX: u32 = FIR0 + 0x64;
const FIR_COEFMOD: u32 = FIR0 + 0x68;
const FIR_COEFCNT: u32 = FIR0 + 0x6C;
const FIR_CHNPTR: u32 = FIR0 + 0x70;
const FIR_SCTL1: u32 = FIR0 + 0x74;
const FIR_SCTL2: u32 = FIR0 + 0x78;
const FIR_SGCTL: u32 = FIR0 + 0x7C;
pub const FIR_END: u32 = FIR0 + 0x80;
pub const SID_FIR0_DMA: u32 = 118;

const CTL1_EN: u32 = 1 << 0;
const CTL1_CH_SHIFT: u32 = 1;
const CTL1_CH_MASK: u32 = 0x1F;
const CTL1_DMAEN: u32 = 1 << 8;
const CTL1_CAI: u32 = 1 << 9;
const CTL1_CCINTR: u32 = 1 << 11;
const CTL1_FXD: u32 = 1 << 12;
const CTL1_RND: u32 = 1 << 14;
const CTL1_HALT: u32 = 1 << 30;
const CTL1_ACM: u32 = 1 << 31;
const CTL2_TAPLEN_MASK: u32 = 0xFFF;
const CTL2_WINDOW_SHIFT: u32 = 14;
const CTL2_WINDOW_MASK: u32 = 0x3FF;
const CTL2_IMASK: u32 = 1 << 24;
const CTL2_TWAIT: u32 = 1 << 28;
const CTL2_SRCEN: u32 = 1 << 29;
const SGCTL_FXD: u32 = 1 << 12;
const SGCTL_RND: u32 = 1 << 14;
const DMASTAT_WDONE: u32 = 1 << 5;
const DMASTAT_ACDONE: u32 = 1 << 6;
const DMASTAT_STICKY: u32 = DMASTAT_WDONE | DMASTAT_ACDONE;
const MAX_TAPS: u32 = 1024;
const MACS: usize = 4;
const MAX_ACM_CHANNELS: u32 = 4096;
const ADDR_MASK: u32 = 0x3FFF_FFFF;

/// fir.LEGACY_TCB: the load order, down from the chain pointer.
const LEGACY_TCB: [u32; 13] = [
    FIR_CTL2,
    FIR_INIDX,
    FIR_INMOD,
    FIR_INCNT,
    FIR_INBASE,
    FIR_OUTIDX,
    FIR_OUTMOD,
    FIR_OUTCNT,
    FIR_OUTBASE,
    FIR_COEFIDX,
    FIR_COEFMOD,
    FIR_COEFCNT,
    FIR_CHNPTR,
];
/// fir.ACM_TCB
const ACM_TCB: [u32; 16] = [
    FIR_CTL2,
    FIR_INIDX,
    FIR_INMOD,
    FIR_INCNT,
    FIR_INBASE,
    FIR_OUTIDX,
    FIR_OUTMOD,
    FIR_OUTCNT,
    FIR_OUTBASE,
    FIR_COEFIDX,
    FIR_COEFMOD,
    FIR_COEFCNT,
    FIR_SGCTL,
    FIR_SCTL2,
    FIR_SCTL1,
    FIR_CHNPTR,
];

/// fir._mmr
fn mmr(s: &St, a: u32) -> R<u32> {
    match s.mmr_get(a) {
        None => Ok(0),
        Some(v) if v.is_c() => Ok(v.b),
        Some(_) => Err(TRAP_PERIPHERAL),
    }
}

/// fir._coefficients: TAPS coefficients in memory order; FIR_COEFCNT down by TAPS.
fn coefficients(s: &mut St, taps: u32) -> R<Vec<f32>> {
    let mut index = mmr(s, FIR_COEFIDX)? & ADDR_MASK;
    let modifier = mmr(s, FIR_COEFMOD)?;
    let mut out = Vec::with_capacity(taps as usize);
    for _ in 0..taps {
        out.push(f32::from_bits(word_read(s, index)?));
        index = step(index, modifier, 0, 0);
    }
    let cnt = mmr(s, FIR_COEFCNT)?.wrapping_sub(taps) & 0xFFFF;
    put(s, FIR_COEFCNT, cnt)?;
    Ok(out)
}

/// fir._window
fn window(s: &mut St, coefs: &[f32], layout: &[u32], pointer: u32, acm: bool) -> R<()> {
    let w = ((mmr(s, FIR_CTL2)? >> CTL2_WINDOW_SHIFT) & CTL2_WINDOW_MASK) + 1;
    let taps = coefs.len() as u32;
    let (inmod, inlen, inbase) = (
        mmr(s, FIR_INMOD)?,
        mmr(s, FIR_INCNT)? & 0xFFFF,
        mmr(s, FIR_INBASE)? & ADDR_MASK,
    );
    let (outmod, outlen, outbase) = (
        mmr(s, FIR_OUTMOD)?,
        mmr(s, FIR_OUTCNT)? & 0xFFFF,
        mmr(s, FIR_OUTBASE)? & ADDR_MASK,
    );
    let mut at = mmr(s, FIR_INIDX)? & ADDR_MASK;
    let mut outidx = mmr(s, FIR_OUTIDX)? & ADDR_MASK;
    let mut back = at;
    let mut xs = Vec::with_capacity((taps - 1 + w) as usize);
    for k in 0..taps - 1 + w {
        xs.push(f32::from_bits(word_read(s, at)?));
        at = step(at, inmod, inlen, inbase);
        if k == w - 1 {
            back = at;
        }
    }
    for j in 0..w as usize {
        let mut acc = [0f32; MACS];
        for (i, &c) in coefs.iter().enumerate() {
            let p = c * xs[j + i];
            acc[i % MACS] = if i < MACS { p } else { acc[i % MACS] + p };
        }
        let y = if coefs.len() >= MACS {
            (acc[0] + acc[1]) + (acc[2] + acc[3])
        } else {
            let mut y = acc[0];
            for &a in acc.iter().take(coefs.len()).skip(1) {
                y += a;
            }
            y
        };
        word_write(s, outidx, y.to_bits())?;
        outidx = step(outidx, outmod, outlen, outbase);
    }
    if acm {
        back = 0xFFFF_FFFF;
        outidx = 0x0000_0000;
    }
    word_write(s, pointer.wrapping_sub(position(layout, FIR_INIDX)), back)?;
    word_write(
        s,
        pointer.wrapping_sub(position(layout, FIR_OUTIDX)),
        outidx,
    )?;
    put(s, FIR_INIDX, back)?;
    put(s, FIR_OUTIDX, outidx)
}

/// fir._channel: the loaded channel's settings checked, its coefficients and one window.
fn channel(s: &mut St, layout: &[u32], pointer: u32, acm: bool) -> R<()> {
    let ctl2 = mmr(s, FIR_CTL2)?;
    if ctl2 & CTL2_SRCEN != 0
        || (acm && ctl2 & CTL2_TWAIT != 0)
        || (acm && mmr(s, FIR_SGCTL)? & (SGCTL_FXD | SGCTL_RND) != 0)
    {
        return Err(TRAP_PERIPHERAL);
    }
    let taps = (ctl2 & CTL2_TAPLEN_MASK) + 1;
    if taps > MAX_TAPS {
        return Err(TRAP_PERIPHERAL);
    }
    let coefs = coefficients(s, taps)?;
    window(s, &coefs, layout, pointer, acm)
}

/// fir._channel_done: WDONE; 1 when the channel raises FIR0_DMA.
fn channel_done(s: &mut St, interrupt: bool) -> R<u32> {
    let v = mmr(s, FIR_DMASTAT)? | DMASTAT_WDONE;
    put(s, FIR_DMASTAT, v)?;
    Ok(interrupt as u32)
}

/// fir._run: the whole chain; how many times it raises FIR0_DMA.
fn run(s: &mut St) -> R<u32> {
    let ctl1 = mmr(s, FIR_CTL1)?;
    if ctl1 & (CTL1_FXD | CTL1_RND | CTL1_CAI | CTL1_HALT) != 0 {
        return Err(TRAP_PERIPHERAL);
    }
    let stat = mmr(s, FIR_DMASTAT)? & !DMASTAT_STICKY;
    put(s, FIR_DMASTAT, stat)?;
    let mut pointer = mmr(s, FIR_CHNPTR)? & ADDR_MASK;
    let mut raises = 0;
    if ctl1 & CTL1_ACM != 0 {
        let mut n = 0;
        while pointer != 0 {
            if n == MAX_ACM_CHANNELS {
                return Err(TRAP_PERIPHERAL);
            }
            n += 1;
            load_tcb(s, pointer, &ACM_TCB)?;
            channel(s, &ACM_TCB, pointer, true)?;
            let masked = mmr(s, FIR_CTL2)? & CTL2_IMASK != 0;
            raises += channel_done(s, !masked)?;
            pointer = mmr(s, FIR_CHNPTR)? & ADDR_MASK;
        }
        let v = mmr(s, FIR_DMASTAT)? | DMASTAT_ACDONE;
        put(s, FIR_DMASTAT, v)?;
        return Ok(raises);
    }
    let channels = ((ctl1 >> CTL1_CH_SHIFT) & CTL1_CH_MASK) + 1;
    let each = ctl1 & CTL1_CCINTR != 0;
    for _ in 0..channels {
        load_tcb(s, pointer, &LEGACY_TCB)?;
        channel(s, &LEGACY_TCB, pointer, false)?;
        raises += channel_done(s, each)?;
        pointer = mmr(s, FIR_CHNPTR)? & ADDR_MASK;
    }
    let v = mmr(s, FIR_DMASTAT)? | DMASTAT_ACDONE;
    put(s, FIR_DMASTAT, v)?;
    Ok(if each { raises } else { 1 })
}

/// fir._fir_read: a FIR register's value; None outside FIR0.
pub fn fir_read(s: &St, a: u32) -> R<Option<V>> {
    if !(FIR0..FIR_END).contains(&a) {
        return Ok(None);
    }
    Ok(Some(V::c(mmr(s, a)? as Int)))
}

/// fir._fir_write: 0 outside FIR0, else 1 + how many times the write raises FIR0_DMA.
pub fn fir_write(s: &mut St, a: u32, value: u32) -> R<u32> {
    if !(FIR0..FIR_END).contains(&a) {
        return Ok(0);
    }
    if a == FIR_DMASTAT || a == FIR_MACSTAT {
        return Ok(1);
    }
    let old = mmr(s, FIR_CTL1)?;
    s.mmr_set(a, V::c(value as Int))?;
    let want = CTL1_EN | CTL1_DMAEN;
    if a == FIR_CTL1 && value & want == want && old & want != want {
        return Ok(1 + run(s)?);
    }
    Ok(1)
}
