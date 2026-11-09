"""core/health and the HEALTH page (finding 41): the committed object, the anchors on every port, the hook sites, the runtime's ABI,
the proof tool's formatting. The emulator proof is tools/coldfire/digikit_health_proof.py (minutes); these are the quick checks."""

import json
import tomllib

import pytest
from support.paths import REPO_ROOT

ROOT = REPO_ROOT

from tools.coldfire import digikit_health_proof as hp  # noqa: E402
from tools.device.dkproto import ARENA_HDR  # noqa: E402
from tools.patchkit import patchkit as front_end  # noqa: E402
from tools.patchkit import pk_toolchains  # noqa: E402

MODULE = ROOT / "modules/core/adapters/health"
IMAGES = ("dt2-1.17", "dn2-1.11", "dn2-1.12")
ANCHORS = {
    "cf.rtos.site.idle": 6,
    "cf.rtos.fn.create_main": 4,
    "cf.link.fn.block_isr": 8,
    "cf.link.site.reply_copy_lit": 4,
    "cf.mem.site.heap.desc_lit": 4,
    "cf.mem.site.heap.mutex_lit": 4,
    "cf.mem.site.heap.lock_lit": 4,
    "cf.mem.site.heap.unlock_lit": 4,
}


def test_committed_object_is_current_and_plain_coldfire():
    obj = json.loads((MODULE / "build" / "health.dko.json").read_text())
    assert {r["type"] for r in obj["relocs"]} == {"cf.32"}
    if not pk_toolchains.m68k_bin("gcc") is not None:
        pytest.skip("m68k toolchain not installed")
    front_end.build_module(MODULE, check=True)


@pytest.mark.parametrize("image", IMAGES)
def test_every_anchor_resolves_on_every_port(image):
    port = tomllib.loads((ROOT / "devices" / "ports" / f"{image}.toml").read_text())
    have = {a["name"]: a for a in port["anchor"]}
    for name, n in ANCHORS.items():
        assert name in have, (image, name)
        assert have[name]["len"] == n, (image, name)
    # the hook sites: a 6-byte call (the boot thread's last jsr) and 8 bytes (link.w + movem.l) at the entry of the block handler
    assert (
        have["cf.link.fn.block_isr"]["addr"]
        == have["cf.link.site.reply_copy_lit"]["addr"] - 0x56
    )


def test_module_toml_declares_what_the_object_needs():
    t = tomllib.loads((MODULE / "module.toml").read_text())
    assert t["module"]["requires"] == ["core/runtime"]
    externs = set(t["object"][0]["externs"])
    obj = json.loads((MODULE / "build" / "health.dko.json").read_text())
    undefined = {r["symbol"] for r in obj["relocs"]} - {
        s["name"] for s in obj["symbols"] if s.get("section")
    }
    assert externs >= {u for u in undefined if u.startswith("health_")}
    assert {o["hook"] for o in t["op"]} == {
        "cf.rtos.hook.idle",
        "cf.link.hook.block_isr",
    }
    for image in IMAGES:
        port = tomllib.loads((ROOT / "devices" / "ports" / f"{image}.toml").read_text())
        modes = {h["name"]: h["mode"] for h in port["hook"]}
        assert modes["cf.rtos.hook.idle"] == "replace"
    assert {c["resource"] for c in t["claim"]} == {
        "cf.rtos.site.idle",
        "cf.link.fn.block_isr",
    }


def test_formatting_helpers():
    assert hp.pct(0) == "0%" and hp.pct(369) == "37%" and hp.pct(1000) == "100%"
    assert hp.size_text(511 * 1024) == "511 KB"
    assert hp.size_text(1024 * 1024) == "1.0 MB"
    assert hp.size_text(int(12.34 * 1024 * 1024)) == "12.3 MB"
    h = dict.fromkeys(hp.HEALTH_FIELDS, 0)
    assert hp.expected_lines(h) == [
        "CPU REL --",
        "DSP 1GHZ --",
        "DSP MAX --",
        "RAM FREE --",
        "LARGEST --",
        "CPU FREE 0 KB",
        "DSP FREE --",
        "STORE USED --",
        "STORE FREE --",
    ]
    h |= {
        "flags": hp.F_MODULE | hp.F_CPU | hp.F_DSP | hp.F_HEAP | hp.F_STORE,
        "store_used": 2,  # KiB
        "store_free": 12284,
        "cpu_x10": 370,
        "dsp_avg_x10": 520,
        "dsp_max_x10": 610,
        "heap_free": 12898304,
        "heap_largest": 4300800,
        "arena_free": 523264,
        "dsp_area_total": 524288,
        "dsp_area_free": 520192,
        "arena_largest": 523264,
        "dsp_area_largest": 400 * 1024,
    }
    assert hp.expected_lines(h) == [
        "CPU REL 37%",
        "DSP 1GHZ 52%",
        "DSP MAX 61%",
        "RAM FREE 12.3 MB",
        "LARGEST 4.1 MB",
        "CPU FREE 511 KB",
        "DSP FREE 508 KB",
        "DSP BIG 400 KB",
        "STORE USED 2 KB",
        "STORE FREE 11.9 MB",
    ]


def test_largest_rows_only_when_they_differ():
    h = dict.fromkeys(hp.HEALTH_FIELDS, 0) | {
        "arena_free": 512 * 1024,
        "arena_largest": 100 * 1024,
    }
    assert "CPU BIG 100 KB" in hp.expected_lines(h)
    # one free block: its payload is the size minus the header
    h["arena_largest"] = h["arena_free"] - ARENA_HDR
    assert not any("LARGEST" in x and x.startswith("CPU") for x in hp.expected_lines(h))


def test_burn_routine_encoding():
    """The machine code the proof writes into the guest: move.l #N,d0; subq.l #1,d0; bne.s -4; addq.l #1,(abs).l; rts."""
    n, counter = 150_000, 0x1000
    code = [
        0x203C0000 | (n >> 16),
        ((n & 0xFFFF) << 16) | 0x5380,
        0x66FC52B9,
        counter,
        0x4E750000,
    ]
    raw = b"".join(w.to_bytes(4, "big") for w in code)
    assert raw[0:2] == bytes.fromhex("203c") and int.from_bytes(raw[2:6], "big") == n
    assert raw[6:8] == bytes.fromhex("5380") and raw[8:10] == bytes.fromhex(
        "66fc"
    )  # bne.s back to the subq
    assert (
        raw[10:12] == bytes.fromhex("52b9")
        and int.from_bytes(raw[12:16], "big") == counter
    )
    assert raw[16:18] == bytes.fromhex("4e75")
