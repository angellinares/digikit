"""clean_runs: only old digi- directories directly under the temp dir, never one in use."""

import os
import time

from tools.devtools import clean_runs
from tools.devtools.clean_runs import Proc


def make(root, name, age_h):
    d = root / name
    d.mkdir()
    (d / "file").write_bytes(b"x")
    t = time.time() - age_h * 3600
    os.utime(d, (t, t))
    return d


def test_prefix_and_age(tmp_path):
    old = make(tmp_path, "digi-session-a", 30)
    make(tmp_path, "digi-session-new", 1)
    make(tmp_path, "other-old", 30)
    (tmp_path / "digi-file").write_text("not a directory")
    assert clean_runs.candidates(tmp_path, 24, []) == [old.resolve()]


def test_in_use_by_command_line_or_cwd(tmp_path):
    named = make(tmp_path, "digi-session-named", 30)
    cwd = make(tmp_path, "digi-session-cwd", 30)
    free = make(tmp_path, "digi-session-free", 30)
    procs = [
        Proc(10, 2, 100, f"/x/digi-emud-abc --run-dir {named}"),
        Proc(11, 2, 100, "node worker.js", cwd=str(cwd / "sub")),
        Proc(12, 2, 100, f"vim {free}"),  # not an emulator or node process
    ]
    assert clean_runs.candidates(tmp_path, 24, procs) == [free.resolve()]


def test_symlink_out_of_temp_is_refused(tmp_path):
    outside = tmp_path / "outside"
    outside.mkdir()
    root = tmp_path / "tmp"
    root.mkdir()
    (root / "digi-link").symlink_to(outside, target_is_directory=True)
    assert clean_runs.candidates(root, 0, []) == []


def test_main_dry_run_keeps_and_yes_deletes(tmp_path, monkeypatch, capsys):
    old = make(tmp_path, "digi-session-a", 30)
    monkeypatch.setattr(clean_runs, "list_procs", lambda: [])
    assert clean_runs.main(["--root", str(tmp_path)]) == 0
    assert old.exists() and "would remove" in capsys.readouterr().out
    assert clean_runs.main(["--root", str(tmp_path), "--yes"]) == 0
    assert not old.exists()


def test_orphans_only_digi_emud_with_parent_1():
    procs = [
        Proc(1, 1, 5, "digi-emud a"),
        Proc(2, 1, 5, "node x"),
        Proc(3, 99, 5, "digi-emud b"),
    ]
    assert [p.pid for p in clean_runs.orphans(procs)] == [1]
