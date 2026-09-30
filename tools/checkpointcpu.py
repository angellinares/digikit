"""Bounded, instruction-clock CPU/guest-RAM Oracle observation.

This observes one Unicorn ``emu_start`` interval. It does not schedule timer
boundaries, authenticate snapshots, or claim Device behavior; the source-chain
wrapper owns those preconditions. Writes below 0x80000000 are compared as
guest RAM; peripheral reads/writes in the recorder's ranges are interleaved
with them, while host writes are excluded.
"""

from unicorn import UC_HOOK_CODE, UC_HOOK_MEM_READ, UC_HOOK_MEM_WRITE

MAX_STEPS = 1_000_000
MAX_EFFECTS = 1_000_000


def _mmio(addr: int) -> bool:
    return 0x8C00_0000 <= addr <= 0x8FFF_FFFF or addr >= 0xC000_0000


def bind_recorded_mmio(effects: list[dict], recorded: list[dict]) -> None:
    """Confirm the raw probe's MMIO order, then supply checked read values."""
    mmio = [effect for effect in effects if effect["kind"] != "RAM_WR"]
    if len(mmio) != len(recorded):
        raise ValueError("CPU/RAM Oracle MMIO event count differs from recorded window")
    for actual, event in zip(mmio, recorded, strict=True):
        if any(
            actual[name] != event[name]
            for name in ("kind", "step", "pc", "address", "size")
        ) or (actual["kind"] == "WR" and actual["value"] != event["value"]):
            raise ValueError("CPU/RAM Oracle MMIO order differs from recorded window")
        # UC_HOOK_MEM_READ fires before the value is returned.
        if actual["kind"] == "RD":
            actual["value"] = event["value"]


def capture_window(uc, pc: int, limit: int, every: int, read_regs):
    """Return sampled pre-instruction registers and ordered guest effects.

    Sample zero precedes the first instruction; sample ``limit`` is the state
    after the last. A write's step is the zero-based instruction executing it.
    ``read_regs`` returns the bounded D/A/PC/SR register dictionary. MMIO read
    values are filled from the checked recorder trace by the source wrapper;
    Unicorn's pre-read hook does not provide the value.
    """
    if type(limit) is not int or not 1 <= limit <= MAX_STEPS:
        raise ValueError("CPU/RAM window must be 1..1000000 instructions")
    if type(every) is not int or not 1 <= every <= limit:
        raise ValueError("CPU sample interval is invalid")
    count = 0
    current_pc = 0
    overflow = False
    invalid_access = False
    samples = []
    effects = []

    def on_code(machine, addr, _size, _user):
        nonlocal count, current_pc
        current_pc = addr & 0xFFFF_FFFF
        if count % every == 0:
            samples.append({"step": count, "regs": read_regs(machine)})
        count += 1

    def record(machine, kind, addr, size, value):
        nonlocal overflow, invalid_access
        if size not in (1, 2, 4) or count == 0:
            invalid_access = True
            machine.emu_stop()
            return
        if len(effects) >= MAX_EFFECTS:
            overflow = True
            machine.emu_stop()
            return
        effects.append(
            {
                "kind": kind,
                "step": count - 1,
                "pc": current_pc,
                "address": addr,
                "size": size,
                "value": None if value is None else value & ((1 << (8 * size)) - 1),
            }
        )

    def on_read(machine, _access, addr, size, _value, _user):
        if _mmio(addr):
            record(machine, "RD", addr, size, None)

    def on_write(machine, _access, addr, size, value, _user):
        if addr >= 0 and addr + size <= 0x8000_0000:
            record(machine, "RAM_WR", addr, size, value)
        elif _mmio(addr):
            record(machine, "WR", addr, size, value)

    code_hook = uc.hook_add(UC_HOOK_CODE, on_code)
    read_hook = uc.hook_add(UC_HOOK_MEM_READ, on_read)
    write_hook = uc.hook_add(UC_HOOK_MEM_WRITE, on_write)
    try:
        uc.emu_start(pc, 0, count=limit)
        if invalid_access:
            raise ValueError("CPU/RAM reference observed unsupported guest access")
        if overflow:
            raise ValueError(
                f"CPU/RAM reference exceeded bounded guest effect count at step {count - 1}"
            )
        if count != limit:
            raise ValueError(
                "CPU/RAM Oracle stopped before requested instruction count"
            )
        samples.append({"step": limit, "regs": read_regs(uc)})
    finally:
        uc.hook_del(write_hook)
        uc.hook_del(read_hook)
        uc.hook_del(code_hook)
    return {"limit": limit, "every": every, "samples": samples, "effects": effects}
