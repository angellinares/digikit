"""Boot-time loading of run-time modules (finding 37): the selection record, the anchors of the boot hook on the two ports, the
packed test modules; and, slow, the emulator proof (tools/patchkit/pk_dkrt_boot.py)."""

import hashlib
import re
import shutil
import subprocess
import tomllib

import pytest
from support.paths import REPO_ROOT

ROOT = REPO_ROOT

from tools.firmware import config  # noqa: E402
from tools.patchkit import patchkit as front_end  # noqa: E402
from tools.patchkit import (  # noqa: E402
    pk_device,
    pk_dkrt,
    pk_dkrt_boot,
    pk_toolchains,
)

RUNTIME = ROOT / "modules/core/runtime"
IMAGES = {
    "dt2-1.17": "Digitakt_II_OS1.17.syx",
    "dn2-1.12": "Digitone_II_OS1.12.syx",
}


def test_the_record_layout_is_the_documented_one():
    rec = pk_dkrt_boot.make_record(["modules/a.dkm", "modules/b.dkm"], seq=7, counter=1)
    assert len(rec) == 512 and rec[:4] == b"DKSL"
    # the same bytes as the Rust test of dkrt_sdk::sel (modules/core/runtime/sdk/src/lib.rs)
    assert rec[508:] == bytes.fromhex("ce5a5f11")
    assert hashlib.sha256(rec).hexdigest().startswith("f274068ed39da26c")
    back = pk_dkrt_boot.parse_record(rec)
    assert back == {
        "seq": 7,
        "counter": 1,
        "flags": 0,
        "paths": ["modules/a.dkm", "modules/b.dkm"],
        "map": [0] * 8,
    }
    # the sticky machine-type map: entry 0 at byte 28, entries 1 to 7 at 480 (docs/design/digikit-store.md); the same bytes as the Rust test of dkrt_sdk::sel
    tm = [0xE53B3A10, 0xCF317D11, 0, 0, 0, 0, 0, 0x00000017]
    withmap = pk_dkrt_boot.make_record(["modules/a.dkm"], seq=2, tmap=tm)
    assert withmap[28:32] == bytes.fromhex("e53b3a10")
    assert withmap[480:484] == bytes.fromhex("cf317d11") and withmap[
        504:508
    ] == bytes.fromhex("00000017")
    assert pk_dkrt_boot.parse_record(withmap)["map"] == tm
    torn = bytearray(rec)
    torn[100] ^= 1
    assert pk_dkrt_boot.parse_record(bytes(torn)) is None
    assert pk_dkrt_boot.parse_record(bytes(512)) is None
    with pytest.raises(pk_dkrt_boot.Fail):
        pk_dkrt_boot.make_record(["x" * 56])


def test_the_sdk_record_code_passes_its_own_tests(tmp_path):
    rustc = shutil.which("rustc")
    if rustc is None:
        pytest.skip("rustc not found")
    exe = tmp_path / "sdk_test"
    sdk = RUNTIME / "sdk/src/lib.rs"
    r = subprocess.run(
        [rustc, "--edition", "2021", "--test", str(sdk), "-o", str(exe)],
        capture_output=True,
        text=True,
    )
    assert r.returncode == 0, r.stderr
    r = subprocess.run([str(exe)], capture_output=True, text=True)
    # the record, the version 3 store and the settings record (the shared vectors included: the test reads modules/core/runtime/vectors/settings-vectors.json)
    m = re.search(r"test result: ok\. (\d+) passed; 0 failed", r.stdout)
    assert r.returncode == 0 and m and int(m.group(1)) > 0, r.stdout


def test_the_store_profile_reserves_the_two_record_sectors():
    for dev, short in (("digitakt-ii", "dt2"), ("digitone-ii", "dn2")):
        store = tomllib.loads((ROOT / "devices" / f"{dev}.toml").read_text())["card"][
            "store"
        ]
        s = pk_device.store_of(short)
        assert store["end_sector"] == s.first + s.sectors
        assert store["first_sector"] == s.first
        assert store["selection_sectors"] == 2
        assert store["settings_sectors"] == 32
        assert store["header_b_sector"] == store["end_sector"] - 2 - 32 - 1
        assert store["header_b_sector"] == s.header_b
        assert (s.sel_a, s.sel_b) == (store["end_sector"] - 2, store["end_sector"] - 1)


@pytest.mark.parametrize("image", sorted(IMAGES))
def test_the_boot_anchors_sit_on_the_prologue_of_the_main_os_task_and_the_block_write(
    image,
):
    from tools.patchkit import port_resolve

    syx = config.stock(IMAGES[image])
    if not syx.exists():
        pytest.skip(f"{IMAGES[image]} missing")
    anchors = {
        a["name"]: a
        for a in tomllib.loads((ROOT / "devices/ports" / f"{image}.toml").read_text())[
            "anchor"
        ]
    }
    main = port_resolve.sections_of(syx)[3]
    base = 0x40000400
    boot = anchors["cf.rtos.site.main_os_task"]
    # `link a6,#-0xa8; movem.l d2-d7/a2-a5,(sp)`: the 8 bytes the hook displaces
    assert main[boot["addr"] - base : boot["addr"] - base + 8] == bytes.fromhex(
        "4e56ff5848d73cfc"
    )
    assert boot["len"] == 8
    # the block write is a function (it starts with `lea -N(sp),sp`, like the block read), the queue literal sits in a `pea <queue>`
    w = anchors["cf.storage.fn.block.write"]["addr"]
    assert main[w - base : w - base + 2] == bytes.fromhex("4fef")
    q = anchors["cf.view.site.ui_loop.queue_lit"]["addr"]
    assert main[q - base - 2 : q - base] == bytes.fromhex("4879") and main[
        q - base + 4 : q - base + 6
    ] == bytes.fromhex("4eb9")


@pytest.mark.parametrize("image", sorted(IMAGES))
def test_the_upload_hook_sits_on_the_router_call_of_the_midi_task_and_the_send_function_is_found(
    image,
):
    """docs/design/digikit-upload.md: the hook replaces `movea.l (0,A2,D1*4),A0; jsr (A0)` (the router call with its arguments pushed)
    and is followed by the task's `lea 12,sp`; the MIDI send function starts like the one the rule hashes."""
    from tools.patchkit import port_resolve

    syx = config.stock(IMAGES[image])
    if not syx.exists():
        pytest.skip(f"{IMAGES[image]} missing")
    anchors = {
        a["name"]: a
        for a in tomllib.loads((ROOT / "devices/ports" / f"{image}.toml").read_text())[
            "anchor"
        ]
    }
    main = port_resolve.sections_of(syx)[3]
    base = 0x40000400
    hook = anchors["cf.midi.site.rx"]
    at = hook["addr"] - base
    assert hook["len"] == 6 and main[at : at + 6] == bytes.fromhex("20721c004e90")
    assert main[at + 6 : at + 10] == bytes.fromhex(
        "4fef000c"
    )  # lea 12(sp),sp: pops msg, len, source
    send = anchors["cf.midi.fn.send"]["addr"]
    assert main[send - base : send - base + 4] == bytes.fromhex("4feffff4")


@pytest.mark.parametrize("image", sorted(IMAGES))
def test_the_clock_and_tx_literals_sit_in_the_instructions_that_read_the_words(image):
    """Finding 37 (the block counter) and finding 43 (the TX ring): each anchor is the 4-byte literal of one instruction, and the word it
    names is in RAM: `addq.l #1,<counter>` at the tail of the block handler (`52b9`), `movea.l <bytes in flight>,a0` in the USB MIDI transmit
    function (`2079`). The counter's other mention is the one-line getter `move.l <counter>,d0; rts`; the in-flight word is subtracted by the
    transfer-complete callback."""
    from tools.patchkit import port_resolve

    syx = config.stock(IMAGES[image])
    if not syx.exists():
        pytest.skip(f"{IMAGES[image]} missing")
    anchors = {
        a["name"]: a
        for a in tomllib.loads((ROOT / "devices/ports" / f"{image}.toml").read_text())[
            "anchor"
        ]
    }
    main = port_resolve.sections_of(syx)[3]
    base = 0x40000400

    def lit(name: str) -> tuple[bytes, int]:
        a = anchors[name]
        assert a["len"] == 4
        at = a["addr"] - base
        return main[at - 2 : at], int.from_bytes(main[at : at + 4], "big")

    op, counter = lit("cf.link.site.block_isr.counter_lit")
    assert op == bytes.fromhex("52b9") and 0x40000000 <= counter < 0x48000000
    getter = main.find(
        bytes.fromhex("2039") + counter.to_bytes(4, "big") + bytes.fromhex("4e75")
    )
    assert getter > 0, (
        "the counter has no one-line getter: the word is not the block counter"
    )
    op, flight = lit("cf.midi.site.tx_inflight_lit")
    assert op == bytes.fromhex("2079") and 0x40000000 <= flight < 0x48000000
    # the transfer-complete callback subtracts the length: `move.l 4(sp),d0; sub.l d0,<flight>; rts`
    sub = main.find(
        bytes.fromhex("202f000491b9")
        + flight.to_bytes(4, "big")
        + bytes.fromhex("4e75")
    )
    assert sub > 0, "no callback subtracts from the in-flight word"


def test_committed_runtime_objects_are_current():
    if not pk_toolchains.codegen_gcc_ready():
        pytest.skip("Rust-for-ColdFire toolchain not installed")
    front_end.build_module(RUNTIME, check=True)
    front_end.build_module(ROOT / "tests/fixtures/modules/dyn-fault", check=True)
    front_end.build_module(ROOT / "tests/fixtures/modules/dyn-fault-late", check=True)


def test_the_fault_module_packs_for_both_devices(tmp_path):
    tools = pk_dkrt.Tools()
    for dev in ("dt2", "dn2"):
        for module in ("test/dyn-fault", "test/dyn-fault-late"):
            out = tmp_path / f"{dev}.dkm"
            tools.patchkit_run("pack", module, "--device", dev, "-o", out)
            assert module in tools.patchkit_run("dkm-info", out)


@pytest.mark.slow
@pytest.mark.parametrize("image", ["dt2-1.17", "dn2-1.12"])
def test_boot_load_restart_cut_and_safe_mode(image, tmp_path):
    if not config.stock(IMAGES[image]).exists():
        pytest.skip(f"{IMAGES[image]} missing")
    proof = pk_dkrt_boot.Proof(image, True, tmp_path, set())
    report = proof.run()
    failed = [c for c in report["checks"] if not c["ok"]]
    assert not failed, failed
