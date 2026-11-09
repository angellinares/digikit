//! Opt-in IIR accelerator (IIR0): TCB chains, biquad windows, status and interrupt.
//!
//! Hand-written twin of tools/sharc_dev/sharc_core/iir.py; keep the two in step (that
//! module's docstring is the specification, from ADSP-2156x HWR chapter 39).
//! A rejected case traps with TRAP_PERIPHERAL (the Python core then raises
//! ValueError for the same case). Each product and each sum is an f32
//! operation, round to nearest even, denormals kept: the same bits as the
//! Python twin's double operation rounded once to float32.
//!
//! A run's register and memory writes are not journaled: one run makes far
//! more changes than the per-instruction journal holds (JOURNAL_CAP), and an
//! IIR store never runs in block code (periph::write_acts sends it to the
//! interpreter), so only a trap later in the same instruction could want
//! them undone, and a trap ends the run. rt/fir.rs shares the DMA helpers.

use super::*;

pub const IIR0: u32 = 0x310C_4000;
const IIR_CTL1: u32 = IIR0;
const IIR_DMASTAT: u32 = IIR0 + 0x04;
const IIR_MACSTAT: u32 = IIR0 + 0x08;
const IIR_CTL2: u32 = IIR0 + 0x40;
const IIR_INIDX: u32 = IIR0 + 0x44;
const IIR_INMOD: u32 = IIR0 + 0x48;
const IIR_INLEN: u32 = IIR0 + 0x4C;
const IIR_INBASE: u32 = IIR0 + 0x50;
const IIR_OUTIDX: u32 = IIR0 + 0x54;
const IIR_OUTMOD: u32 = IIR0 + 0x58;
const IIR_OUTLEN: u32 = IIR0 + 0x5C;
const IIR_OUTBASE: u32 = IIR0 + 0x60;
const IIR_COEFIDX: u32 = IIR0 + 0x64;
const IIR_COEFMOD: u32 = IIR0 + 0x68;
const IIR_COEFLEN: u32 = IIR0 + 0x6C;
const IIR_CHNPTR: u32 = IIR0 + 0x70;
const IIR_SCTL1: u32 = IIR0 + 0x74;
const IIR_SCTL2: u32 = IIR0 + 0x78;
const IIR_SGCTL: u32 = IIR0 + 0x7C;
pub const IIR_END: u32 = IIR0 + 0x80;
pub const SID_IIR0_DMA: u32 = 120;

const CTL1_EN: u32 = 1 << 0;
const CTL1_CH_SHIFT: u32 = 1;
const CTL1_CH_MASK: u32 = 0x1F;
const CTL1_DMAEN: u32 = 1 << 8;
const CTL1_CAI: u32 = 1 << 9;
const CTL1_SS: u32 = 1 << 10;
const CTL1_CCINTR: u32 = 1 << 11;
const CTL1_FORTYBIT: u32 = 1 << 12;
const CTL1_RND: u32 = 1 << 14;
const CTL1_HALT: u32 = 1 << 30;
const CTL1_ACM: u32 = 1 << 31;
const CTL2_BIQUADS_MASK: u32 = 0x3F;
const CTL2_WINDOW_SHIFT: u32 = 14;
const CTL2_WINDOW_MASK: u32 = 0x3FF;
const CTL2_IMASK: u32 = 1 << 24;
const CTL2_TWAIT: u32 = 1 << 28;
const SGCTL_SS: u32 = 1 << 10;
const SGCTL_FORTYBIT: u32 = 1 << 12;
const SGCTL_RND: u32 = 1 << 14;
const DMASTAT_WDONE: u32 = 1 << 5;
const DMASTAT_ACDONE: u32 = 1 << 6;
const DMASTAT_STICKY: u32 = DMASTAT_WDONE | DMASTAT_ACDONE;
const MAX_ACM_CHANNELS: u32 = 4096;
const WORDS_PER_BIQUAD: u32 = 7;
const ADDR_MASK: u32 = 0x3FFF_FFFF;

/// iir.LEGACY_TCB: the load order, down from the chain pointer.
const LEGACY_TCB: [u32; 13] = [
    IIR_CTL2,
    IIR_INIDX,
    IIR_INMOD,
    IIR_INLEN,
    IIR_INBASE,
    IIR_OUTIDX,
    IIR_OUTMOD,
    IIR_OUTLEN,
    IIR_OUTBASE,
    IIR_COEFIDX,
    IIR_COEFMOD,
    IIR_COEFLEN,
    IIR_CHNPTR,
];
/// iir.ACM_TCB
const ACM_TCB: [u32; 16] = [
    IIR_CTL2,
    IIR_INIDX,
    IIR_INMOD,
    IIR_INLEN,
    IIR_INBASE,
    IIR_OUTIDX,
    IIR_OUTMOD,
    IIR_OUTLEN,
    IIR_OUTBASE,
    IIR_COEFIDX,
    IIR_COEFMOD,
    IIR_COEFLEN,
    IIR_SGCTL,
    IIR_SCTL2,
    IIR_SCTL1,
    IIR_CHNPTR,
];

/// One biquad: A0, A1, B1, A2, B2, s1, s2 (s1 is added to the output), and the
/// word addresses of s1, s2.
struct Biquad {
    c: [f32; 5],
    s1: f32,
    s2: f32,
    at1: u32,
    at2: u32,
}

/// iir._mmr
fn mmr(s: &St, a: u32) -> R<u32> {
    match s.mmr_get(a) {
        None => Ok(0),
        Some(v) if v.is_c() => Ok(v.b),
        Some(_) => Err(TRAP_PERIPHERAL),
    }
}

/// iir._set (journaled: the guest's own register store).
fn set(s: &mut St, a: u32, v: u32) -> R<()> {
    s.mmr_set(a, V::c(v as Int))
}

/// iir._set inside a run (not journaled).
pub(super) fn put(s: &mut St, a: u32, v: u32) -> R<()> {
    s.mmr_put(a, V::c(v as Int));
    Ok(())
}

/// iir._word_read: the word at word address W.
pub(super) fn word_read(s: &St, w: u32) -> R<u32> {
    super::periph::ram_word(s, (w & ADDR_MASK) << 2)
}

/// iir._word_write (inside a run: not journaled).
pub(super) fn word_write(s: &mut St, w: u32, v: u32) -> R<()> {
    s.mem_store_nolog((w & ADDR_MASK) << 2, 4, v);
    Ok(())
}

/// iir._step: the next index of a circular buffer; length 0 is linear.
pub(super) fn step(index: u32, modifier: u32, length: u32, base: u32) -> u32 {
    let m = (modifier & 0xFFFF) as u16 as i16 as i64;
    let mut next = index as i64 + m;
    if length != 0 {
        if next >= base as i64 + length as i64 {
            next -= length as i64;
        } else if next < base as i64 {
            next += length as i64;
        }
    }
    (next as u32) & ADDR_MASK
}

/// iir._load_tcb
pub(super) fn load_tcb(s: &mut St, pointer: u32, layout: &[u32]) -> R<()> {
    for (k, &reg) in layout.iter().enumerate() {
        let v = word_read(s, pointer.wrapping_sub(k as u32))?;
        put(s, reg, v)?;
    }
    Ok(())
}

/// iir._coefficients
fn coefficients(s: &St) -> R<Vec<Biquad>> {
    let count = (mmr(s, IIR_CTL2)? & CTL2_BIQUADS_MASK) + 1;
    let mut index = mmr(s, IIR_COEFIDX)? & ADDR_MASK;
    let modifier = mmr(s, IIR_COEFMOD)?;
    let length = mmr(s, IIR_COEFLEN)? & 0xFFFF;
    let base = index;
    let mut out = Vec::with_capacity(count as usize);
    for _ in 0..count {
        let mut w = [0f32; 7];
        let mut places = [0u32; 7];
        for k in 0..WORDS_PER_BIQUAD as usize {
            places[k] = index;
            w[k] = f32::from_bits(word_read(s, index)?);
            index = step(index, modifier, length, base);
        }
        out.push(Biquad {
            c: [w[0], w[1], w[2], w[3], w[4]],
            s1: w[5],
            s2: w[6],
            at1: places[5],
            at2: places[6],
        });
    }
    Ok(out)
}

/// iir._save_state
fn save_state(s: &mut St, biquads: &[Biquad]) -> R<()> {
    for bq in biquads {
        word_write(s, bq.at1, bq.s1.to_bits())?;
        word_write(s, bq.at2, bq.s2.to_bits())?;
    }
    Ok(())
}

pub(super) fn position(layout: &[u32], reg: u32) -> u32 {
    layout.iter().position(|&r| r == reg).unwrap_or(0) as u32
}

/// iir._window
fn window(s: &mut St, biquads: &mut [Biquad], layout: &[u32], pointer: u32, acm: bool) -> R<()> {
    let ctl2 = mmr(s, IIR_CTL2)?;
    let w = ((ctl2 >> CTL2_WINDOW_SHIFT) & CTL2_WINDOW_MASK) + 1;
    let mut inidx = mmr(s, IIR_INIDX)? & ADDR_MASK;
    let mut outidx = mmr(s, IIR_OUTIDX)? & ADDR_MASK;
    let (inmod, inlen, inbase) = (
        mmr(s, IIR_INMOD)?,
        mmr(s, IIR_INLEN)? & 0xFFFF,
        mmr(s, IIR_INBASE)? & ADDR_MASK,
    );
    let (outmod, outlen, outbase) = (
        mmr(s, IIR_OUTMOD)?,
        mmr(s, IIR_OUTLEN)? & 0xFFFF,
        mmr(s, IIR_OUTBASE)? & ADDR_MASK,
    );
    for _ in 0..w {
        let mut x = f32::from_bits(word_read(s, inidx)?);
        for bq in biquads.iter_mut() {
            let [a0, a1, b1, a2, b2] = bq.c;
            let y = a0 * x + bq.s1;
            bq.s1 = (a1 * x + bq.s2) + b1 * y;
            bq.s2 = a2 * x + b2 * y;
            x = y;
        }
        word_write(s, outidx, x.to_bits())?;
        inidx = step(inidx, inmod, inlen, inbase);
        outidx = step(outidx, outmod, outlen, outbase);
    }
    if acm {
        inidx = 0xFFFF_FFFF;
        outidx = 0x0000_0000;
    }
    word_write(s, pointer.wrapping_sub(position(layout, IIR_INIDX)), inidx)?;
    word_write(
        s,
        pointer.wrapping_sub(position(layout, IIR_OUTIDX)),
        outidx,
    )?;
    put(s, IIR_INIDX, inidx)?;
    put(s, IIR_OUTIDX, outidx)
}

/// iir._channel_done: WDONE; 1 when the channel raises IIR0_DMA.
fn channel_done(s: &mut St, interrupt: bool) -> R<u32> {
    let v = mmr(s, IIR_DMASTAT)? | DMASTAT_WDONE;
    put(s, IIR_DMASTAT, v)?;
    Ok(interrupt as u32)
}

/// iir._run: the whole chain; how many times it raises IIR0_DMA.
fn run(s: &mut St) -> R<u32> {
    let ctl1 = mmr(s, IIR_CTL1)?;
    if ctl1 & (CTL1_FORTYBIT | CTL1_RND | CTL1_CAI | CTL1_HALT) != 0 {
        return Err(TRAP_PERIPHERAL);
    }
    let stat = mmr(s, IIR_DMASTAT)? & !DMASTAT_STICKY;
    put(s, IIR_DMASTAT, stat)?;
    let mut pointer = mmr(s, IIR_CHNPTR)? & ADDR_MASK;
    let mut raises = 0;
    if ctl1 & CTL1_ACM != 0 {
        let mut n = 0;
        while pointer != 0 {
            if n == MAX_ACM_CHANNELS {
                return Err(TRAP_PERIPHERAL);
            }
            n += 1;
            load_tcb(s, pointer, &ACM_TCB)?;
            if mmr(s, IIR_SGCTL)? & (SGCTL_FORTYBIT | SGCTL_RND) != 0
                || mmr(s, IIR_CTL2)? & CTL2_TWAIT != 0
            {
                return Err(TRAP_PERIPHERAL);
            }
            let mut biquads = coefficients(s)?;
            window(s, &mut biquads, &ACM_TCB, pointer, true)?;
            if mmr(s, IIR_SGCTL)? & SGCTL_SS != 0 {
                save_state(s, &biquads)?;
            }
            let masked = mmr(s, IIR_CTL2)? & CTL2_IMASK != 0;
            raises += channel_done(s, !masked)?;
            pointer = mmr(s, IIR_CHNPTR)? & ADDR_MASK;
        }
        let v = mmr(s, IIR_DMASTAT)? | DMASTAT_ACDONE;
        put(s, IIR_DMASTAT, v)?;
        return Ok(raises);
    }
    let channels = ((ctl1 >> CTL1_CH_SHIFT) & CTL1_CH_MASK) + 1;
    let first = pointer;
    let mut loaded = Vec::with_capacity(channels as usize);
    for _ in 0..channels {
        load_tcb(s, pointer, &LEGACY_TCB)?;
        loaded.push(coefficients(s)?);
        pointer = mmr(s, IIR_CHNPTR)? & ADDR_MASK;
    }
    pointer = first;
    let each = ctl1 & CTL1_CCINTR != 0;
    for biquads in loaded.iter_mut() {
        load_tcb(s, pointer, &LEGACY_TCB)?;
        window(s, biquads, &LEGACY_TCB, pointer, false)?;
        raises += channel_done(s, each)?;
        pointer = mmr(s, IIR_CHNPTR)? & ADDR_MASK;
    }
    if ctl1 & CTL1_SS != 0 {
        for biquads in loaded.iter() {
            save_state(s, biquads)?;
        }
    }
    let v = mmr(s, IIR_DMASTAT)? | DMASTAT_ACDONE;
    put(s, IIR_DMASTAT, v)?;
    Ok(if each { raises } else { 1 })
}

/// iir._iir_read: an IIR register's value; None outside IIR0.
pub fn iir_read(s: &St, a: u32) -> R<Option<V>> {
    if !(IIR0..IIR_END).contains(&a) {
        return Ok(None);
    }
    Ok(Some(V::c(mmr(s, a)? as Int)))
}

/// iir._iir_write: 0 outside IIR0, else 1 + how many times the write raises IIR0_DMA.
pub fn iir_write(s: &mut St, a: u32, value: u32) -> R<u32> {
    if !(IIR0..IIR_END).contains(&a) {
        return Ok(0);
    }
    if a == IIR_DMASTAT || a == IIR_MACSTAT {
        return Ok(1);
    }
    let old = mmr(s, IIR_CTL1)?;
    set(s, a, value)?;
    let want = CTL1_EN | CTL1_DMAEN;
    if a == IIR_CTL1 && value & want == want && old & want != want {
        return Ok(1 + run(s)?);
    }
    Ok(1)
}
