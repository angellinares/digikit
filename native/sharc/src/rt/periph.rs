//! Opt-in system peripherals: the SEC core interface and descriptor DMA.
//!
//! Hand-written twin of tools/sharc_core/periph.py; keep the two in step.
//! Register state lives in St::mmrs, so canonical snapshots need no new
//! fields. A rejected case traps with TRAP_PERIPHERAL (the Python core then
//! raises ValueError for the same case).

use super::*;

pub const SECI_ID: u32 = 0x300EB;
pub const SECI_LATCH: u32 = 0x8000;
const SEC0: u32 = 0x3108_9000;
pub const SEC_GCTL: u32 = SEC0;
pub const SEC_RAISE: u32 = SEC0 + 0x008;
pub const SEC_END: u32 = SEC0 + 0x00C;
pub const SEC_CCTL: u32 = SEC0 + 0x400;
pub const SEC_CSTAT: u32 = SEC0 + 0x404;
pub const SEC_CSID: u32 = SEC0 + 0x41C;
pub const SEC_SCTL: u32 = SEC0 + 0x800;
const SEC_SOURCES: u32 = 256;
const CSTAT_ERR: u32 = 0x2;
const CSTAT_ERRC_ACK: u32 = 0x10;
const CSTAT_PNDV: u32 = 0x100;
const CSTAT_SIDV: u32 = 0x400;
const SSTAT_ERR: u32 = 0x2;
const SSTAT_ERRC_END: u32 = 0x20;
const SSTAT_PND: u32 = 0x100;
const SSTAT_ACT: u32 = 0x200;
const SCTL_IEN: u32 = 0x1;
const SCTL_SEN: u32 = 0x4;
/// SPI2 TX/RX (DMA26/27) and SPORT4A/B (DMA10/11).
pub const DMA_BASES: [u32; 4] = [0x3102_D200, 0x3102_D280, 0x3102_3000, 0x3102_3080];
const DMA_CFG: u32 = 0x08;
const DMA_STAT: u32 = 0x30;
const STAT_IRQDONE: u32 = 0x1;
const STAT_IRQERR: u32 = 0x2;
const STAT_RUN: u32 = 0x700;
const STAT_RUN_TRANSFER: u32 = 0x200;
pub const DAI1_GBL_SP_EN: u32 = 0x310C_A2E8;
pub const DAI1_GBL_INT_EN: u32 = 0x310C_A2EC;
pub const SPORT4A_CTL: u32 = 0x3100_2400;
pub const SPORT4B_CTL: u32 = 0x3100_2480;
pub const SPORT4A_DMA: u32 = 0x3102_3000;
pub const SPORT4B_DMA: u32 = 0x3102_3080;
pub const SID_SPORT4A_DMA: u32 = 53;
pub const SID_SPORT4B_DMA: u32 = 55;
const SID_DAI1_GRP0: u32 = 191;
const GBL_SP_ENABLE: u32 = 0x1;
const SPORT_SPEN: u32 = 0x1;
const DAI_MEMBER_MASK: u32 = 0xFF;
const UREG_IRPTL: usize = 122;
const UREG_IMASKP: usize = 124;

/// periph._mmr: a missing register reads 0.
fn mmr(s: &St, a: u32) -> R<u32> {
    match s.mmr_get(a) {
        None => Ok(0),
        Some(v) if v.is_c() => Ok(v.b),
        Some(_) => Err(TRAP_PERIPHERAL),
    }
}

/// periph._set (journaled).
fn set(s: &mut St, a: u32, v: u32) -> R<()> {
    s.mmr_set(a, V::c(v as Int))
}

fn sstat(sid: u32) -> u32 {
    SEC_SCTL + 8 * sid + 4
}

/// periph._sec_raise
pub fn sec_raise(s: &mut St, sid: u32) -> R<()> {
    if sid >= SEC_SOURCES {
        return Err(TRAP_PERIPHERAL);
    }
    if mmr(s, SEC_SCTL + 8 * sid)? & SCTL_SEN != 0 {
        let v = mmr(s, sstat(sid))? | SSTAT_PND;
        set(s, sstat(sid), v)?;
    }
    sec_arbitrate(s)
}

/// periph._sec_arbitrate
fn sec_arbitrate(s: &mut St) -> R<()> {
    if mmr(s, SEC_GCTL)? & 1 == 0 || mmr(s, SEC_CCTL)? & 1 == 0 {
        return Ok(());
    }
    if mmr(s, SEC_CSTAT)? & CSTAT_SIDV != 0 {
        return Ok(());
    }
    let mut best: Option<u32> = None;
    let mut best_prio = 0x100;
    let mut active_prio = 0x100;
    for sid in 0..SEC_SOURCES {
        let status = mmr(s, sstat(sid))?;
        if status & (SSTAT_PND | SSTAT_ACT) == 0 {
            continue;
        }
        let sctl = mmr(s, SEC_SCTL + 8 * sid)?;
        let prio = (sctl >> 8) & 0xFF;
        if status & SSTAT_ACT != 0 {
            if prio < active_prio {
                active_prio = prio;
            }
        } else if sctl & SCTL_IEN != 0 && (sctl >> 24) & 0xF == 0 && prio < best_prio {
            best = Some(sid);
            best_prio = prio;
        }
    }
    let Some(best) = best else {
        return Ok(());
    };
    if best_prio >= active_prio {
        return Ok(());
    }
    set(s, SEC_CSID, best)?;
    set(s, SECI_ID, best)?;
    let cstat = mmr(s, SEC_CSTAT)? | CSTAT_SIDV | CSTAT_PNDV;
    set(s, SEC_CSTAT, cstat)
}

/// periph._sec_ack
fn sec_ack(s: &mut St) -> R<()> {
    let cstat = mmr(s, SEC_CSTAT)?;
    if cstat & CSTAT_SIDV == 0 {
        return set(s, SEC_CSTAT, cstat | CSTAT_ERR | CSTAT_ERRC_ACK);
    }
    let sid = mmr(s, SEC_CSID)? & 0xFF;
    let status = mmr(s, sstat(sid))?;
    set(s, sstat(sid), (status & !SSTAT_PND) | SSTAT_ACT)?;
    set(s, SEC_CSTAT, cstat & !(CSTAT_SIDV | CSTAT_PNDV))?;
    sec_arbitrate(s)
}

/// periph._sec_end
fn sec_end(s: &mut St, sid: u32) -> R<()> {
    let status = mmr(s, sstat(sid))?;
    if status & SSTAT_ACT != 0 {
        set(s, sstat(sid), status & !SSTAT_ACT)?;
    } else {
        set(s, sstat(sid), status | SSTAT_ERR | SSTAT_ERRC_END)?;
    }
    sec_arbitrate(s)
}

/// periph._sec_line: the SEC request latches IRPTL.SECI at an instruction
/// boundary unless SECI is being serviced. A hardware latch, not a guest
/// IRPTL write.
pub fn sec_line(s: &mut St) -> R<()> {
    if mmr(s, SEC_CSTAT)? & CSTAT_SIDV == 0 {
        return Ok(());
    }
    let latch = s.r[UREG_IRPTL];
    let active = s.r[UREG_IMASKP];
    if !latch.is_c() || !active.is_c() {
        return Err(TRAP_PERIPHERAL);
    }
    if active.b & SECI_LATCH == 0 {
        s.set_r(UREG_IRPTL, V::c((latch.b | SECI_LATCH) as Int))?;
    }
    Ok(())
}

/// periph._ram_word: a little-endian word from the loaded image or overlay.
pub fn ram_word(s: &St, a: u32) -> R<u32> {
    if !s.mem.all_present(a as Int, 4) {
        return Err(TRAP_PERIPHERAL);
    }
    Ok(s.mem.read_le(a, 4) as u32)
}

/// periph._dma_fetch
fn dma_fetch(s: &mut St, base: u32, ndsize: u32) -> R<()> {
    if ndsize > 6 {
        return Err(TRAP_PERIPHERAL);
    }
    let pointer = mmr(s, base)?;
    let current = mmr(s, base + 0x24)?;
    set(s, base + 0x28, current)?;
    for k in 0..=ndsize {
        let word = ram_word(s, pointer.wrapping_add(4 * k))?;
        set(s, base + 4 * k, word)?;
    }
    set(s, base + 0x24, pointer.wrapping_add(4 * (ndsize + 1)))?;
    let start = mmr(s, base + 0x04)?;
    set(s, base + 0x2C, start)?;
    let x = mmr(s, base + 0x0C)?;
    set(s, base + 0x34, x)?;
    let y = mmr(s, base + 0x14)?;
    set(s, base + 0x38, y)?;
    let stat = mmr(s, base + DMA_STAT)?;
    set(s, base + DMA_STAT, (stat & !STAT_RUN) | STAT_RUN_TRANSFER)
}

/// periph._dma_start: the current work unit's start address.
pub fn dma_start(s: &mut St, base: u32) -> R<u32> {
    let cfg = mmr(s, base + DMA_CFG)?;
    if cfg & 1 == 0 {
        return Err(TRAP_PERIPHERAL);
    }
    if mmr(s, base + DMA_STAT)? & STAT_RUN == 0 {
        if (cfg >> 12) & 7 != 4 {
            return Err(TRAP_PERIPHERAL);
        }
        dma_fetch(s, base, (cfg >> 16) & 7)?;
    }
    mmr(s, base + 0x04)
}

/// periph._dma_irq: a SPORT4 channel in an enabled DAI1 group raises the
/// group source once every member's IRQDONE is set, else its own source.
fn dma_irq(s: &mut St, base: u32, sid: u32) -> R<()> {
    let member = match base {
        SPORT4A_DMA => Some(0),
        SPORT4B_DMA => Some(1),
        _ => None,
    };
    if let Some(member) = member {
        let enable = mmr(s, DAI1_GBL_INT_EN)?;
        let mut routed = false;
        for group in 0..2u32 {
            let members = (enable >> (8 * group)) & DAI_MEMBER_MASK;
            if enable & (1 << (16 + group)) == 0 || members & (1 << member) == 0 {
                continue;
            }
            if members & !3 != 0 {
                return Err(TRAP_PERIPHERAL);
            }
            routed = true;
            let mut all = true;
            for (other, index) in [(SPORT4A_DMA, 0), (SPORT4B_DMA, 1)] {
                if members & (1 << index) != 0 && mmr(s, other + DMA_STAT)? & STAT_IRQDONE == 0 {
                    all = false;
                }
            }
            if all {
                sec_raise(s, SID_DAI1_GRP0 + group)?;
            }
        }
        if routed {
            return Ok(());
        }
    }
    sec_raise(s, sid)
}

/// periph._sport_running
pub fn sport_running(s: &St) -> R<bool> {
    Ok(mmr(s, DAI1_GBL_SP_EN)? & GBL_SP_ENABLE != 0
        && mmr(s, SPORT4A_CTL)? & SPORT_SPEN != 0
        && mmr(s, SPORT4B_CTL)? & SPORT_SPEN != 0)
}

/// periph._dma_done
pub fn dma_done(s: &mut St, base: u32, sid: u32) -> R<()> {
    dma_start(s, base)?;
    let cfg = mmr(s, base + DMA_CFG)?;
    let count = mmr(s, base + 0x0C)?;
    let step = mmr(s, base + 0x10)?;
    let start = mmr(s, base + 0x04)?;
    set(s, base + 0x2C, start.wrapping_add(count.wrapping_mul(step)))?;
    set(s, base + 0x34, 0)?;
    match (cfg >> 20) & 3 {
        0 => {}
        1 => {
            let stat = mmr(s, base + DMA_STAT)? | STAT_IRQDONE;
            set(s, base + DMA_STAT, stat)?;
            dma_irq(s, base, sid)?;
        }
        _ => return Err(TRAP_PERIPHERAL),
    }
    match (cfg >> 12) & 7 {
        0 => {
            let stat = mmr(s, base + DMA_STAT)? & !STAT_RUN;
            set(s, base + DMA_STAT, stat)
        }
        4 => dma_fetch(s, base, (cfg >> 16) & 7),
        _ => Err(TRAP_PERIPHERAL),
    }
}

/// periph._periph_read
pub fn periph_read(s: &St, a: u32) -> R<Option<V>> {
    if a == SECI_ID {
        return Ok(Some(V::c(mmr(s, SECI_ID)? as Int)));
    }
    Ok(None)
}

/// periph._periph_write: true when the write had a peripheral effect.
pub fn periph_write(s: &mut St, a: u32, value: u32) -> R<bool> {
    if a == SECI_ID || a == SEC_CSID {
        sec_ack(s)?;
        return Ok(true);
    }
    if a == SEC_END {
        sec_end(s, value & 0xFF)?;
        return Ok(true);
    }
    if a == SEC_RAISE {
        sec_raise(s, value & 0xFF)?;
        return Ok(true);
    }
    if a >= SEC_SCTL && a < SEC_SCTL + 8 * SEC_SOURCES && (a - SEC_SCTL) & 7 == 4 {
        let v = mmr(s, a)? & !(value & 0x302);
        set(s, a, v)?;
        sec_arbitrate(s)?;
        return Ok(true);
    }
    for base in DMA_BASES {
        if a == base + DMA_STAT {
            let v = mmr(s, a)? & !(value & (STAT_IRQDONE | STAT_IRQERR | 0x4));
            set(s, a, v)?;
            return Ok(true);
        }
        if a == base + DMA_CFG {
            let old = mmr(s, a)?;
            set(s, a, value)?;
            let stat = mmr(s, base + DMA_STAT)?;
            if value & 1 != 0 && old & 1 == 0 {
                // EN 0 -> 1 resets the channel state; IRQERR survives.
                set(s, base + DMA_STAT, stat & STAT_IRQERR)?;
            } else if value & 1 == 0 {
                set(s, base + DMA_STAT, stat & !STAT_RUN)?;
            }
            return Ok(true);
        }
    }
    Ok(false)
}
