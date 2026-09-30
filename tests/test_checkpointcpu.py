"""Synthetic instruction-clock CPU/RAM trace checks (no firmware needed)."""

import pytest
from unicorn import UC_HOOK_CODE, UC_HOOK_MEM_READ, UC_HOOK_MEM_WRITE

from tools import checkpointcpu


class FakeUc:
    def __init__(self, instructions):
        self.instructions = instructions
        self.hooks = {}
        self.pc = 0
        self.stopped = False

    def hook_add(self, kind, callback):
        self.hooks[kind] = callback
        return kind

    def hook_del(self, handle):
        del self.hooks[handle]

    def emu_stop(self):
        self.stopped = True

    def emu_start(self, pc, _end, *, count):
        self.pc = pc
        for address, accesses in self.instructions[:count]:
            if self.stopped:
                break
            self.pc = address
            self.hooks[UC_HOOK_CODE](self, address, 2, None)
            for access in accesses:
                if len(access) == 3:
                    addr, size, value = access
                    kind = "WR"
                else:
                    kind, addr, size, value = access
                hook = UC_HOOK_MEM_READ if kind == "RD" else UC_HOOK_MEM_WRITE
                self.hooks[hook](self, 0, addr, size, value, None)
            self.pc = address + 2


def test_zero_based_samples_and_interleaved_guest_effects():
    uc = FakeUc(
        [
            (
                0x4000,
                [
                    (0x4000_0000, 4, 0x1234),
                    ("RD", 0xFC00_0000, 4, 0),
                    (0x4000_0004, 1, 0x1AB),
                ],
            ),
            (0x4002, [(0xFC00_0000, 4, 0x8888)]),
            (0x4004, [(0x4000_0008, 2, 0x5678)]),
            (0x4006, []),
        ]
    )
    result = checkpointcpu.capture_window(
        uc, 0x4000, 4, 2, lambda machine: {"pc": machine.pc}
    )
    assert [sample["step"] for sample in result["samples"]] == [0, 2, 4]
    assert [
        (w["kind"], w["step"], w["size"], w["value"]) for w in result["effects"]
    ] == [
        ("RAM_WR", 0, 4, 0x1234),
        ("RD", 0, 4, None),
        ("RAM_WR", 0, 1, 0xAB),
        ("WR", 1, 4, 0x8888),
        ("RAM_WR", 2, 2, 0x5678),
    ]
    assert [w["pc"] for w in result["effects"]] == [
        0x4000,
        0x4000,
        0x4000,
        0x4002,
        0x4004,
    ]
    assert not uc.hooks


def test_checked_recorder_binds_pre_read_value_after_matching_mmio_order():
    effects = [
        {
            "kind": "RAM_WR",
            "step": 0,
            "pc": 0x4000,
            "address": 0x4000_0000,
            "size": 4,
            "value": 1,
        },
        {
            "kind": "RD",
            "step": 0,
            "pc": 0x4000,
            "address": 0xFC00_0000,
            "size": 4,
            "value": None,
        },
        {
            "kind": "WR",
            "step": 0,
            "pc": 0x4000,
            "address": 0xFC00_0004,
            "size": 4,
            "value": 3,
        },
    ]
    recorded = [
        {
            "kind": "RD",
            "step": 0,
            "pc": 0x4000,
            "address": 0xFC00_0000,
            "size": 4,
            "value": 2,
        },
        {
            "kind": "WR",
            "step": 0,
            "pc": 0x4000,
            "address": 0xFC00_0004,
            "size": 4,
            "value": 3,
        },
    ]
    checkpointcpu.bind_recorded_mmio(effects, recorded)
    assert [effect["value"] for effect in effects] == [1, 2, 3]
    with pytest.raises(ValueError, match="order differs"):
        checkpointcpu.bind_recorded_mmio(effects, list(reversed(recorded)))
    with pytest.raises(ValueError, match="count differs"):
        checkpointcpu.bind_recorded_mmio(effects, recorded[:1])


def test_short_run_rejected_and_hooks_removed():
    uc = FakeUc([(0x4000, [])])
    with pytest.raises(ValueError, match="stopped before"):
        checkpointcpu.capture_window(uc, 0x4000, 2, 1, lambda machine: {})
    assert not uc.hooks


def test_unsupported_write_width_fails_closed():
    uc = FakeUc([(0x4000, [(0x4000_0000, 3, 1)])])
    with pytest.raises(ValueError, match="unsupported guest access"):
        checkpointcpu.capture_window(uc, 0x4000, 1, 1, lambda machine: {})
    assert not uc.hooks


def test_guest_effect_limit_stops_before_unbounded_trace(monkeypatch):
    monkeypatch.setattr(checkpointcpu, "MAX_EFFECTS", 1)
    uc = FakeUc([(0x4000, [(0x4000_0000, 4, 1), (0x4000_0004, 4, 2)])])
    with pytest.raises(ValueError, match="exceeded bounded guest effect count"):
        checkpointcpu.capture_window(uc, 0x4000, 1, 1, lambda machine: {})
    assert not uc.hooks


@pytest.mark.parametrize("limit,every", [(0, 1), (1, 0), (1, 2), (True, 1)])
def test_invalid_bounds_rejected_before_guest_execution(limit, every):
    uc = FakeUc([])
    with pytest.raises(ValueError):
        checkpointcpu.capture_window(uc, 0, limit, every, lambda machine: {})
    assert not uc.hooks
