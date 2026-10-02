"""Opt-in system peripherals: the SEC core interface and descriptor DMA.

Enabled by State.peripheral_model. Register state lives in State.mmrs, so
canonical snapshots need no new fields. Public sources: ADSP-2156x HWR
chapter 6 (SEC_SCTL/SSTAT, CSID, CSTAT, END, RAISE and the interrupt flow)
and chapter 27 (channel registers, descriptor lists, DMA_STAT W1C); SHARC+
PRM CEC_SID, the core's SHDBG_SECI_ID at 0x300EB: a read returns the issued
SID, a write of any value acknowledges it.

Not modeled: the SEC preemption stack, CPMSK/CGMSK masking, re-assertion of
a still-asserted level source after END (a source is raised once per event),
DMA timing, 2D, autobuffer and array flows. Unsupported cases raise
ValueError instead of guessing. These functions are hand-written twins of
native/sharc/src/rt/periph.rs (tools/sharc_transpile.py BOUNDARY).
"""

from __future__ import annotations

from .encoding import UREG_CODES
from .state import State, _ureg
from .values import Const

SECI_ID = 0x300EB
SECI_LATCH = 0x8000  # IRPTL/IMASK bit 15, vector 0x3C
SEC0 = 0x31089000
SEC_GCTL = SEC0
SEC_RAISE = SEC0 + 0x008
SEC_END = SEC0 + 0x00C
SEC_CCTL = SEC0 + 0x400
SEC_CSTAT = SEC0 + 0x404
SEC_CSID = SEC0 + 0x41C
SEC_SCTL = SEC0 + 0x800
SEC_SOURCES = 256
CSTAT_ERR = 0x2
CSTAT_ERRC_ACK = 0x10
CSTAT_PNDV = 0x100
CSTAT_SIDV = 0x400
SSTAT_ERR = 0x2
SSTAT_ERRC_END = 0x20
SSTAT_PND = 0x100
SSTAT_ACT = 0x200
SCTL_IEN = 0x1
SCTL_SEN = 0x4
# SPI2 TX/RX (DMA26/27) and SPORT4A/B (DMA10/11).
DMA_BASES = (0x3102D200, 0x3102D280, 0x31023000, 0x31023080)
DMA_CFG = 0x08
DMA_STAT = 0x30
STAT_IRQDONE = 0x1
STAT_IRQERR = 0x2
STAT_RUN = 0x700
STAT_RUN_TRANSFER = 0x200
# DAI1 group SPORT interrupt (HWR ch. 22 DAI_GBL_INT_EN, ch. 23 "Grouping of
# SPORTs"): SPORT4A/B are DAI1 "SP0A/SP0B". A group raises one SEC source
# (DAI1_GBL_SPORT_INT0/1, SID 191/192) when every selected member's
# DMA_STAT.IRQDONE is set; there is no status register of its own, W1C of the
# members' IRQDONE clears it.
DAI1_GBL_SP_EN = 0x310CA2E8
DAI1_GBL_INT_EN = 0x310CA2EC
SPORT4A_CTL = 0x31002400
SPORT4B_CTL = 0x31002480
SPORT4A_DMA = 0x31023000
SPORT4B_DMA = 0x31023080
SID_SPORT4A_DMA = 53
SID_SPORT4B_DMA = 55
SID_DAI1_GRP0 = 191
GBL_SP_ENABLE = 0x1
SPORT_SPEN = 0x1
# DAI_GBL_INT_EN: GRPn_INT_EN is bit 16+n, GRPn_SPxINT_EN is bit 8n+x
# (x = 0 SP0A = SPORT4A, 1 SP0B = SPORT4B).
DAI_MEMBERS = {SPORT4A_DMA: 0, SPORT4B_DMA: 1}
DAI_MEMBER_MASK = 0xFF


def _mmr(state: State, address: int) -> int:
    value = state.mmrs.get(address)
    if value is None:
        return 0
    if not isinstance(value, Const):
        raise ValueError("unknown peripheral register %#x" % address)
    return value.value & 0xFFFFFFFF


def _set(state: State, address: int, value: int) -> None:
    state.mmrs[address] = Const(value & 0xFFFFFFFF)


def _sstat(sid: int) -> int:
    return SEC_SCTL + 8 * sid + 4


def _sec_raise(state: State, sid: int) -> None:
    """A source asserts: SSTAT.PND when SCTL.SEN, then arbitration."""
    if not 0 <= sid < SEC_SOURCES:
        raise ValueError("SEC source %d out of range" % sid)
    if _mmr(state, SEC_SCTL + 8 * sid) & SCTL_SEN:
        _set(state, _sstat(sid), _mmr(state, _sstat(sid)) | SSTAT_PND)
    _sec_arbitrate(state)


def _sec_arbitrate(state: State) -> None:
    """Forward the most urgent pending source to core 0 (HWR SEC flow).

    Lower PRIO is more urgent and ties go to the lowest SID. While an issued
    SID awaits its acknowledge nothing else is issued, and a pending source
    waits while one of equal or higher priority is active (END releases it).
    """
    if not _mmr(state, SEC_GCTL) & 1 or not _mmr(state, SEC_CCTL) & 1:
        return
    if _mmr(state, SEC_CSTAT) & CSTAT_SIDV:
        return
    best = -1
    best_prio = 0x100
    active_prio = 0x100
    for sid in range(SEC_SOURCES):
        sstat = _mmr(state, _sstat(sid))
        if not sstat & (SSTAT_PND | SSTAT_ACT):
            continue
        sctl = _mmr(state, SEC_SCTL + 8 * sid)
        prio = (sctl >> 8) & 0xFF
        if sstat & SSTAT_ACT:
            if prio < active_prio:
                active_prio = prio
        elif sctl & SCTL_IEN and not (sctl >> 24) & 0xF and prio < best_prio:
            best = sid
            best_prio = prio
    if best < 0 or best_prio >= active_prio:
        return
    _set(state, SEC_CSID, best)
    _set(state, SECI_ID, best)
    _set(state, SEC_CSTAT, _mmr(state, SEC_CSTAT) | CSTAT_SIDV | CSTAT_PNDV)


def _sec_ack(state: State) -> None:
    """Core acknowledge (CSID or SHDBG_SECI_ID write): PND -> ACT."""
    cstat = _mmr(state, SEC_CSTAT)
    if not cstat & CSTAT_SIDV:
        _set(state, SEC_CSTAT, cstat | CSTAT_ERR | CSTAT_ERRC_ACK)
        return
    sid = _mmr(state, SEC_CSID) & 0xFF
    sstat = _mmr(state, _sstat(sid))
    _set(state, _sstat(sid), (sstat & ~SSTAT_PND) | SSTAT_ACT)
    _set(state, SEC_CSTAT, cstat & ~(CSTAT_SIDV | CSTAT_PNDV))
    _sec_arbitrate(state)


def _sec_end(state: State, sid: int) -> None:
    """SEC_END: clear ACT, or record an end error when it was not active."""
    sstat = _mmr(state, _sstat(sid))
    if sstat & SSTAT_ACT:
        _set(state, _sstat(sid), sstat & ~SSTAT_ACT)
    else:
        _set(state, _sstat(sid), sstat | SSTAT_ERR | SSTAT_ERRC_END)
    _sec_arbitrate(state)


def _sec_line(state: State) -> None:
    """The SEC's interrupt request to the core, held from issue to ACK.

    Drivers call this at an instruction boundary. It latches IRPTL.SECI
    unless the core is servicing SECI (PRM: SECI is not stored in IRPTL
    during an SEC ISR), so a request issued inside the ISR is taken after
    its RTI. A hardware latch, not a guest IRPTL write.
    """
    if not _mmr(state, SEC_CSTAT) & CSTAT_SIDV:
        return
    code = UREG_CODES["IRPTL"]
    latch = _ureg(state.uregs, code)
    active = _ureg(state.uregs, UREG_CODES["IMASKP"])
    if not isinstance(latch, Const) or not isinstance(active, Const):
        raise ValueError("unknown interrupt latch")
    if not active.value & SECI_LATCH:
        state.uregs[code] = Const(latch.value | SECI_LATCH)


def _signed32(value: int) -> int:
    return value - (1 << 32) if value & 0x80000000 else value


def _ram_word(state: State, address: int) -> int:
    """A 32-bit little-endian word from the overlay or the loaded image."""
    backing = state.concrete
    raw = 0
    for k in range(4):
        here = address + k
        if here in state.overlay:
            byte = state.overlay[here]
        else:
            data = backing.read(here, 1) if backing is not None else None
            if data is None:
                raise ValueError("DMA access outside known memory %#x" % here)
            byte = data[0]
        raw |= byte << (8 * k)
    return raw


def _dma_fetch(state: State, base: int, ndsize: int) -> None:
    """Load NDSIZE+1 descriptor words from DSCPTR_NXT into the channel."""
    if ndsize > 6:
        raise ValueError("reserved DMA descriptor size")
    pointer = _mmr(state, base)
    _set(state, base + 0x28, _mmr(state, base + 0x24))
    for k in range(ndsize + 1):
        _set(state, base + 4 * k, _ram_word(state, pointer + 4 * k))
    _set(state, base + 0x24, pointer + 4 * (ndsize + 1))
    _set(state, base + 0x2C, _mmr(state, base + 0x04))
    _set(state, base + 0x34, _mmr(state, base + 0x0C))
    _set(state, base + 0x38, _mmr(state, base + 0x14))
    stat = _mmr(state, base + DMA_STAT)
    _set(state, base + DMA_STAT, (stat & ~STAT_RUN) | STAT_RUN_TRANSFER)


def _dma_start(state: State, base: int) -> int:
    """The current work unit's start address; fetches the first descriptor."""
    cfg = _mmr(state, base + DMA_CFG)
    if not cfg & 1:
        raise ValueError("DMA channel %#x is not enabled" % base)
    if not _mmr(state, base + DMA_STAT) & STAT_RUN:
        if (cfg >> 12) & 7 != 4:
            raise ValueError("only descriptor-list DMA is modeled")
        _dma_fetch(state, base, (cfg >> 16) & 7)
    return _mmr(state, base + 0x04)


def _dma_irq(state: State, base: int, sid: int) -> None:
    """Route a channel's IRQDONE: a DAI1 group source or its own SID.

    A SPORT4 channel that belongs to an enabled DAI1 group raises the group
    source only when every member's IRQDONE is set (one source per event);
    the channel's own source stays quiet. Other members of a group are not
    modeled and raise ValueError.
    """
    member = DAI_MEMBERS.get(base)
    if member is not None:
        enable = _mmr(state, DAI1_GBL_INT_EN)
        routed = False
        for group in (0, 1):
            members = (enable >> (8 * group)) & DAI_MEMBER_MASK
            if not enable & (1 << (16 + group)) or not members & (1 << member):
                continue
            if members & ~3:
                raise ValueError("only SPORT4A/B group members are modeled")
            routed = True
            complete = True
            for base_of, index in DAI_MEMBERS.items():
                if members & (1 << index) and not (
                    _mmr(state, base_of + DMA_STAT) & STAT_IRQDONE
                ):
                    complete = False
            if complete:
                _sec_raise(state, SID_DAI1_GRP0 + group)
        if routed:
            return
    _sec_raise(state, sid)


def _sport_running(state: State) -> bool:
    """DAI1_GBL_SP_EN.GBL_SP_EN set and both SPORT4 halves enabled."""
    return bool(
        _mmr(state, DAI1_GBL_SP_EN) & GBL_SP_ENABLE
        and _mmr(state, SPORT4A_CTL) & SPORT_SPEN
        and _mmr(state, SPORT4B_CTL) & SPORT_SPEN
    )


def _dma_done(state: State, base: int, sid: int) -> None:
    """Complete the work unit: IRQDONE and its SEC source, next descriptor."""
    _dma_start(state, base)
    cfg = _mmr(state, base + DMA_CFG)
    count = _mmr(state, base + 0x0C)
    step = _signed32(_mmr(state, base + 0x10))
    _set(state, base + 0x2C, _mmr(state, base + 0x04) + count * step)
    _set(state, base + 0x34, 0)
    interrupt = (cfg >> 20) & 3
    if interrupt == 1:
        _set(state, base + DMA_STAT, _mmr(state, base + DMA_STAT) | STAT_IRQDONE)
        _dma_irq(state, base, sid)
    elif interrupt:
        raise ValueError("only X-count DMA interrupts are modeled")
    flow = (cfg >> 12) & 7
    if flow == 0:
        _set(state, base + DMA_STAT, _mmr(state, base + DMA_STAT) & ~STAT_RUN)
    elif flow == 4:
        _dma_fetch(state, base, (cfg >> 16) & 7)
    else:
        raise ValueError("only stop and descriptor-list DMA flows are modeled")


def _periph_read(state: State, address: int) -> Const | None:
    """SHDBG_SECI_ID reads the issued SID; other addresses read plainly."""
    if address == SECI_ID:
        return Const(_mmr(state, SECI_ID))
    return None


def _periph_write(state: State, address: int, value: int) -> bool:
    """Apply a guest MMR write with peripheral effects; False leaves it plain."""
    if address in (SECI_ID, SEC_CSID):
        _sec_ack(state)
        return True
    if address == SEC_END:
        _sec_end(state, value & 0xFF)
        return True
    if address == SEC_RAISE:
        _sec_raise(state, value & 0xFF)
        return True
    offset = address - SEC_SCTL
    if 0 <= offset < 8 * SEC_SOURCES and offset & 7 == 4:
        _set(state, address, _mmr(state, address) & ~(value & 0x302))
        _sec_arbitrate(state)
        return True
    for base in DMA_BASES:
        if address == base + DMA_STAT:
            mask = value & (STAT_IRQDONE | STAT_IRQERR | 0x4)
            _set(state, address, _mmr(state, address) & ~mask)
            return True
        if address == base + DMA_CFG:
            old = _mmr(state, address)
            _set(state, address, value)
            stat = _mmr(state, base + DMA_STAT)
            if value & 1 and not old & 1:
                # EN 0 -> 1 resets the channel state; IRQERR survives.
                _set(state, base + DMA_STAT, stat & STAT_IRQERR)
            elif not value & 1:
                _set(state, base + DMA_STAT, stat & ~STAT_RUN)
            return True
    return False
