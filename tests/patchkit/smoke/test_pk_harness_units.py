"""Unit tests of the harness logic that need no daemon (tools/patchkit/pk_harness.py)."""

from __future__ import annotations

import hashlib
import json
from pathlib import Path

import pytest
from support.paths import REPO_ROOT

ROOT = REPO_ROOT
from tools.firmware import config  # noqa: E402
from tools.patchkit import pk_harness as h  # noqa: E402


def test_cell_names_and_keys():
    c = h.parse_cell("dt2-1.17:test/cf-demo,core/fx")
    assert (c.device, c.firmware) == ("dt2", "1.17")
    assert c.modset == "core/fx+test/cf-demo"
    assert c.golden_prefix == "dt2-1.17/core/fx+test/cf-demo"
    assert "/" not in c.slug
    stock = h.parse_cell("dn2-1.12")
    assert stock.modset == "stock" and stock.golden_prefix == "dn2-1.12/stock"


def test_the_default_matrix_and_tour_parse():
    cells = h.load_matrix(h.DEFAULT_MATRIX)
    assert {(c.device, c.firmware) for c in cells} == {
        ("dt2", "1.17"),
        ("dn2", "1.12"),
    }
    assert any(c.modules for c in cells) and any(not c.modules for c in cells)
    segments = h.smoke_segments()
    names = [n for n, _ in segments]
    assert names == [
        "boot", "track", "pattern", "audition", "play_stop", "machine_menu", "save_reload", "stress",
    ]  # fmt: skip
    for _, steps in segments:
        for step in steps:
            actions = set(step) & {
                "tap", "hold", "turn", "wait_ms", "expect_frame", "expect_pcm_db", "expect_no_fault", "stress",
            }  # fmt: skip
            assert len(actions) == 1, step
            for key in [step["tap"]] if "tap" in step else []:
                for name in key if isinstance(key, list) else [key]:
                    h.button(name)


def test_module_tests_are_found_by_device():
    found = dict(h.module_tests(["core/fx", "test/cf-demo"], "dt2", quick_only=True))
    assert "core.fx.audible" in found and "test.cf-demo.boots" in found
    assert all(isinstance(steps, list) and steps for steps in found.values())
    assert dict(h.module_tests(["test/cf-demo"], "dn2", quick_only=False))


def test_goldens_round_trip_and_only_hashes_are_committed(tmp_path):
    g = h.Goldens(tmp_path / "g.json")
    assert g.get("a") is None
    g.set("a", "ab" * 32)
    g.save()
    again = h.Goldens(tmp_path / "g.json")
    assert again.get("a") == "ab" * 32
    committed = json.loads(h.GOLDENS.read_text())
    assert committed, "goldens are committed"
    assert all(len(v) == 64 and int(v, 16) >= 0 for v in committed.values())
    assert all(k.count("/") >= 3 for k in committed)


def test_stale_or_foreign_ready_inputs_are_refused(tmp_path, monkeypatch):
    cell = h.Cell("dn2", "1.12")
    syx = config.stock(h.FIRMWARE[("dn2", "1.12")]["syx"])
    if not syx.is_file():
        pytest.skip("no firmware")
    digest = hashlib.sha256(syx.read_bytes()).hexdigest()
    ready = tmp_path / "ready"
    ready.mkdir()
    monkeypatch.setenv("DIGI_PK_READY_DN2_112", str(ready))
    with pytest.raises(h.HarnessError, match="SOURCE.json"):
        h.firmware_inputs(cell)
    (ready / "SOURCE.json").write_text(json.dumps({"syx_sha256": digest}))
    with pytest.raises(h.HarnessError, match="INTC-force"):
        h.firmware_inputs(cell)
    (ready / "SOURCE.json").write_text(
        json.dumps({"syx_sha256": "00", "intc_force": False})
    )
    with pytest.raises(h.HarnessError, match="another .syx"):
        h.firmware_inputs(cell)
    (ready / "SOURCE.json").write_text(
        json.dumps({"syx_sha256": digest, "intc_force": False})
    )
    with pytest.raises(h.HarnessError, match="is missing"):
        h.firmware_inputs(cell)


def test_goldens_save_merges_with_other_processes(tmp_path, monkeypatch):
    """Two processes recording different cells must not overwrite each other (`--update` with `--jobs`)."""
    monkeypatch.setattr(h, "OUT", tmp_path)
    path = tmp_path / "goldens.json"
    a, b = h.Goldens(path), h.Goldens(path)
    a.set("dt2-1.17/stock/boot/ready", "aa")
    b.set("dn2-1.12/stock/boot/ready", "bb")
    a.save()
    b.save()
    assert json.loads(path.read_text()) == {
        "dt2-1.17/stock/boot/ready": "aa",
        "dn2-1.12/stock/boot/ready": "bb",
    }


def test_shared_boot_cache_links(tmp_path):
    """Every daemon sees the same `patched` and `cards` directories and the same card path."""
    cache = tmp_path / "cache"
    run1, run2 = tmp_path / "run1", tmp_path / "run2"
    for run in (run1, run2):
        h.link_shared(run, cache)
    assert (run1 / "patched").resolve() == (run2 / "patched").resolve()
    assert (run1 / "cards").resolve() == (run2 / "cards").resolve()
    (
        run1 / "cards" / "abc123"
    ).mkdir()  # the daemon builds the card through its run dir
    (run1 / "cards" / "abc123" / "dt2.img").write_bytes(b"card")
    c1 = h.canonical_card(cache, str(run1 / "cards" / "abc123" / "dt2.img"))
    c2 = h.canonical_card(cache, str(run2 / "cards" / "abc123" / "dt2.img"))
    assert c1 == c2 and Path(c1).read_bytes() == b"card"
    # a private cache dir from an earlier run is kept aside
    run3 = tmp_path / "run3"
    (run3 / "patched").mkdir(parents=True)
    h.link_shared(run3, cache)
    assert (run3 / "patched").is_symlink() and list(run3.glob("patched.private-*"))


def test_proofkit_child_command_strips_job_options():
    from tools.patchkit import pk_proofkit as k

    argv = ["--quick", "--only", "a,b", "--jobs=4", "--out", "x", "--result", "r"]
    assert k._strip_options(argv, ("--only", "--jobs", "--result")) == [
        "--quick",
        "--out",
        "x",
    ]


def test_proofkit_runs_parts_and_reports(tmp_path, capsys):
    """Parts in one process: a crash is a failed check, not the end of the run; the verdict and exit code follow."""
    import argparse

    from tools.patchkit import pk_proofkit as k

    def good(p):
        p.check("fine", True, 1)

    def bad(p):
        p.check("broken", False, "detail")

    def crash(p):
        raise RuntimeError("boom")

    parts = [
        k.Part("g", "good", good),
        k.Part("b", "bad", bad, quick=False),
        k.Part("c", "crash", crash, quick=False),
    ]
    ap = argparse.ArgumentParser()
    k.add_args(ap, parts)
    a = ap.parse_args(["--out", str(tmp_path / "o"), "--jobs", "1"])
    assert k.run_tool("x.py", k.Proof, parts, a, argv=[]) == 1
    text = capsys.readouterr().out
    assert "part g" in text and "PASS" in text
    assert (
        "part b" in text
        and "part c crashed" in text
        and text.rstrip().splitlines()[-1].startswith("FAIL: parts b, c")
    )
    q = ap.parse_args(["--out", str(tmp_path / "q"), "--jobs", "1", "--quick"])
    assert k.run_tool("x.py", k.Proof, parts, q, argv=[]) == 0


def _frame(*pixels: tuple[int, int]) -> bytes:
    """A raw 128x64 OLED frame with the given (x, y) pixels on (byte (7 - y / 8) + 8 x, bit y % 8)."""
    raw = bytearray(1024)
    for x, y in pixels:
        raw[(7 - y // 8) + 8 * x] |= 1 << (y % 8)
    return bytes(raw)


def test_frame_diff_counts_pixels_and_gives_the_box():
    base = _frame((3, 4), (50, 20), (127, 63))
    moved = _frame((3, 4), (50, 20), (126, 62), (10, 9))
    n, box, changed = h.frame_diff(moved, base)
    # (127, 63) went off, (126, 62) and (10, 9) came on
    assert n == 3 and box == (10, 9, 127, 63)
    assert changed == _frame((127, 63), (126, 62), (10, 9))
    assert h.frame_diff(base, base) == (0, None, bytes(1024))


def test_a_mismatch_is_explained_against_the_remembered_frame(tmp_path, monkeypatch):
    from tools.control.digi_client import oled_png

    monkeypatch.setattr(h, "SMOKE_FRAMES", tmp_path / "cache")
    want, got = _frame((3, 4), (50, 20)), _frame((3, 4), (50, 21), (51, 22))
    wd, gd = (hashlib.sha256(x).hexdigest() for x in (want, got))
    out = tmp_path / "run"
    # no frame of the golden yet: got.png, and a text that says what to do
    text, rec = h.explain_mismatch("dt2-1.17/m", "seg/x", "about", got, gd, wd, out)
    assert (
        "no local frame for the golden" in text
        and f"got {gd[:8]} want {wd[:8]}" in text
    )
    assert rec["want_cached"] is False and (out / "got.png").read_bytes() == oled_png(
        got
    )
    assert not (out / "want.png").exists()
    # after a pass or an update it is in the cache; an older frame of the checkpoint is dropped
    h.remember_frame(
        "dt2-1.17/m", "seg/x", "about", hashlib.sha256(b"old").hexdigest(), b"old"
    )
    h.remember_frame("dt2-1.17/m", "seg/x", "about", wd, want)
    assert (
        len(list((tmp_path / "cache" / "dt2-1.17/m" / "seg_x").glob("about.*.oled")))
        == 1
    )
    text, rec = h.explain_mismatch("dt2-1.17/m", "seg/x", "about", got, gd, wd, out)
    assert (
        text.startswith("3 px differ in x 50..51, y 20..22 (OLED pixels")
        and str(out) in text
    )
    assert (
        rec["px"] == 3 and rec["box"] == (50, 20, 51, 22) and rec["want_cached"] is True
    )
    assert (out / "want.png").read_bytes() == oled_png(want)
    assert (out / "diff.png").read_bytes() == oled_png(
        _frame((50, 20), (50, 21), (51, 22))
    )


def test_plan_symbol_finds_a_mangled_static_in_its_module():
    plan = {
        "symbols": {
            "core/a@0.1.0:DK_ABOUT": 0x100,
            "core/a@0.1.0:.sec..bss._RNvCs1_15pk_digikit_menu5STORE": 0x200,
            "core/b@0.1.0:.sec..bss._RNvCs2_4dkrt5store10STORE_SEEN": 0x300,
            "core/b@0.1.0:.sec..bss._RNvCs2_4dkrt5STORE": 0x400,
        }
    }
    assert h.plan_symbol(plan, "DK_ABOUT") == 0x100
    assert h.plan_symbol(plan, "STORE", "core/a") == 0x200
    with pytest.raises(h.HarnessError):
        h.plan_symbol(plan, "STORE")  # two modules
    with pytest.raises(h.HarnessError):
        h.plan_symbol(plan, "NOPE")


def test_text_rows_and_the_runtime_version():
    from tools.device.dklog_kinds import RUNTIME_PACKED as v

    mem = {}
    for i, t in enumerate([b"DIGIKIT 0.1.0", b"RUNTIME " + h.runtime_text().encode()]):
        for k, c in enumerate(t.ljust(24, b"\0")):
            mem[0x1000 + 24 * i + k] = c
    word = lambda a: int.from_bytes(bytes(mem.get(a + j, 0) for j in range(4)), "big")  # noqa: E731
    rows = h.text_rows(word, 0x1000, 2, 24)
    assert rows == ["DIGIKIT 0.1.0", f"RUNTIME {h.runtime_text()}"]
    assert h.runtime_text() == f"{v >> 16}.{v >> 8 & 255}.{v & 255}"
    assert h.missing_rows(rows, ["DIGIKIT 0.1.0", "RUNTIME {runtime}"]) == []
    assert h.missing_rows(rows, ["FREE 1 KB"]) == ["FREE 1 KB"]


def test_part_jobs_bounds_the_parallel_parts(monkeypatch):
    import threading
    import time

    from tools.patchkit import pk_digikit_scen, pk_proofkit

    assert h.part_jobs(4) == 4  # unset: all parts, as before
    monkeypatch.setenv("DIGIKIT_PART_JOBS", "1")
    assert h.part_jobs(4) == 1
    monkeypatch.setenv("DIGIKIT_PART_JOBS", "9")
    assert h.part_jobs(3) == 3

    def peak(run, width):
        if width is None:
            monkeypatch.delenv("DIGIKIT_PART_JOBS", raising=False)
        else:
            monkeypatch.setenv("DIGIKIT_PART_JOBS", width)
        live = top = 0
        lock = threading.Lock()

        def part():
            nonlocal live, top
            with lock:
                live += 1
                top = max(top, live)
            time.sleep(0.05)
            with lock:
                live -= 1

        run([part] * 4)
        return top

    ctx = pk_digikit_scen.Ctx.__new__(pk_digikit_scen.Ctx)  # parallel() uses no state
    proof = pk_proofkit.Proof.__new__(pk_proofkit.Proof)
    for run in (lambda fns: ctx.parallel(*fns), lambda fns: proof.parallel(*fns)):
        assert peak(run, "1") == 1
        assert peak(run, "2") == 2
        assert peak(run, None) == 4
