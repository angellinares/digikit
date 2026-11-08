"""test/rust-demo: Rust compiled for the ColdFire (rustc_codegen_gcc, finding 26) running in a patched image.

Slow (a ColdFire boot per image). Skips without the Rust toolchain only for the build check; the committed object
is enough to patch and run."""

import json
import struct
import subprocess

import pytest
from support.paths import REPO_ROOT

ROOT = REPO_ROOT

from tools.firmware import config  # noqa: E402
from tools.patchkit import modroots, pk_cfcall, pk_toolchains  # noqa: E402
from tools.patchkit import patchkit as front_end  # noqa: E402

IMAGES = [
    "Digitakt_II_OS1.17.syx",
    "Digitone_II_OS1.12.syx",
]
MODULE = ROOT / "tests/fixtures/modules/rust-demo"
TABLE = [3, 1, 4, 1, 5, 9, 2, 6]
M32, M64 = 0xFFFFFFFF, 0xFFFFFFFFFFFFFFFF


def ref_hash(data: bytes) -> int:
    h = 0xCBF29CE484222325
    for b in data:
        h = ((h ^ b) * 0x100000001B3) & M64
    return ((h >> 32) ^ h) & M32


def ref_apply(op: int, x: int) -> int:
    return {
        0: (x + 1) & M32,
        1: ((x << 7) | (x >> 25)) & M32,
        2: (-x) & M32,
        3: x // 10,
        4: x % 7,
        5: ((x * x) >> 16) & M32,
        6: ((x * 1_000_003) & M64) // 7919 & M32,
    }.get(op, x)


def ref_selftest() -> int:
    acc = ref_hash(struct.pack(">8I", *TABLE)) ^ sum(TABLE)
    for op in range(7):
        acc = ref_apply(op, acc)
    return acc


def patchkit(*args):
    exe = (
        front_end.binary()
    )  # one build under a lock, then an immutable copy (tools/devtools/stablebin.py)
    r = subprocess.run(
        [str(exe), "--root", str(ROOT), *map(str, args)],
        capture_output=True,
        text=True,
    )
    assert r.returncode == 0, r.stderr + r.stdout


def patched(fw, tmp_path):
    out = tmp_path / (fw.stem + "-rust.syx")
    plan_path = tmp_path / "plan.json"
    patchkit(
        "apply",
        "--firmware",
        fw,
        "--modules",
        "test/rust-demo",
        "-o",
        out,
        "--plan",
        plan_path,
    )
    plan = json.loads(plan_path.read_text())
    sym = {
        k.split(":", 1)[1]: v
        for k, v in plan["symbols"].items()
        if k.startswith("test/rust-demo")
    }
    return out, plan, sym


def code_range(sym):
    """[lo, hi) of the module's code: the text sections come first, then rodata, data and bss."""
    text = [v for k, v in sym.items() if k.startswith(".sec..text.")]
    after = [
        v
        for k, v in sym.items()
        if k.startswith((".sec..rodata", ".sec..data", ".sec..bss"))
    ]
    return min(text), min(after)


def test_built_object_is_current_and_plain_coldfire():
    """The object is a fresh build (needs the Rust toolchain) and every relocation is R_68K_32."""
    if not pk_toolchains.codegen_gcc_ready():
        pytest.skip("Rust-for-ColdFire toolchain not installed")
    front_end.build_module(MODULE)  # a stale object is rebuilt into out/cache/dko
    front_end.build_module(MODULE, check=True)
    obj = json.loads(
        modroots.cached_objects(MODULE)["build/rust-demo.dko.json"].read_text()
    )
    assert {r["type"] for r in obj["relocs"]} == {"cf.32"}
    assert int(obj["toolchain"]["instructions_redecoded"]) > 100
    assert obj["toolchain"]["rustc_codegen_gcc"] == pk_toolchains.CG_REV


@pytest.mark.parametrize("name", IMAGES)
def test_rust_functions_run_in_place(name, tmp_path):
    fw = config.stock(name)
    if not fw.exists():
        pytest.skip(f"{name} missing")
    out, plan, sym = patched(fw, tmp_path)
    m = pk_cfcall.CfMachine(
        out, state=tmp_path / "boot.state", base_sha256=plan["base"]["syx_sha256"]
    )
    table, state = sym["PK_RS_TABLE"], sym["PK_RS_STATE"]
    calls = [
        {"pc": sym["pk_rs_tick"], "reads": [(state, 16)]},
        {"pc": sym["pk_rs_tick"]},
        {"pc": sym["pk_rs_tick"], "reads": [(state, 16)]},
        {"pc": sym["pk_rs_hash"], "args": [table, 32]},
        {"pc": sym["pk_rs_hash"], "args": [table, 0]},
        *({"pc": sym["pk_rs_apply"], "args": [op, 0x12345678]} for op in range(8)),
        {"pc": sym["pk_rs_selftest"], "reads": [(state, 16), (table, 32)]},
    ]
    r = m.run({}, calls)
    assert all(c.ok and not c.clobbered for c in r), [(c.error, c.clobbered) for c in r]
    # the bss was zero before the first call; the magic and the count follow
    assert struct.unpack(">4I", r[0].reads[0]) == (0x52555354, 1, 0, 0)
    assert [r[0].d0, r[1].d0, r[2].d0] == [1, 2, 3]
    assert struct.unpack(">4I", r[2].reads[0]) == (0x52555354, 3, 0, 0)
    # the u64 multiply (compiler_builtins __muldi3, ColdFire code) agrees with the reference
    assert r[3].d0 == ref_hash(struct.pack(">8I", *TABLE))
    assert r[4].d0 == ref_hash(b"")
    assert [c.d0 for c in r[5:13]] == [ref_apply(op, 0x12345678) for op in range(8)]
    final = r[13]
    assert final.d0 == ref_selftest()
    st, tb = final.reads
    assert struct.unpack(">4I", st) == (0x52555354, 3, ref_selftest(), 0)
    assert struct.unpack(">8I", tb) == tuple(TABLE)


def test_instrumented_boot_and_call_have_no_stray_writes_or_fetches(tmp_path):
    """A boot with the write watch on cf.expansion (code range allowed) and the flush check: the loader runs before the
    caches, the module runs from loaded code: 0 events, 0 flush hits (finding 26)."""
    fw = config.stock("Digitakt_II_OS1.17.syx")
    if not fw.exists():
        pytest.skip("Digitakt_II_OS1.17.syx missing")
    out, plan, sym = patched(fw, tmp_path)
    lo, hi = code_range(sym)
    log = tmp_path / "ww.jsonl"
    env = {
        "DIGI_FLUSH_CHECK": "1",
        "DIGI_WRITE_WATCH": "47e8b000-47fe0000",
        "DIGI_WRITE_WATCH_ALLOW_PC": f"{lo:x}-{hi:x}",
        "DIGI_WRITE_WATCH_AFTER": "cacr",
        "DIGI_WRITE_WATCH_OUT": str(log),
    }
    m = pk_cfcall.CfMachine(
        out,
        state=tmp_path / "wboot.state",
        base_sha256=plan["base"]["syx_sha256"],
        env=env,
    )
    r = m.run({}, [{"pc": sym["pk_rs_tick"]}, {"pc": sym["pk_rs_selftest"]}])
    assert r[0].ok and r[1].ok and r[1].d0 == ref_selftest()
    summary = json.loads(log.with_suffix(".summary.json").read_text())
    assert summary["events"] == 0, summary
    assert summary["flush_hits"] == [], summary
    assert summary["flush_fetches_checked"] > 0 and summary["cpushl"] > 0
