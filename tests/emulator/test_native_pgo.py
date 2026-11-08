"""The PGO profile key covers the emulation crates, not the desktop UI crate."""

import hashlib
import json
import os
import pathlib
import subprocess

import pytest

from tools.emulator import (
    digi_env,
    native_pgo,
)

PACKAGES = ["packages/emulator/sharc", "packages/emulator/machine"]


@pytest.fixture
def tree(tmp_path, monkeypatch):
    """A tiny repo with a trained profile manifest for it."""
    for rel, text in {
        "packages/emulator/sharc/lib.rs": "a",
        "packages/emulator/machine/lib.rs": "b",
        "apps/desktop/src-tauri/main.rs": "ui",
        "Cargo.toml": "[workspace]",
        "Cargo.lock": "lock",
        "gen/core.rs": "core",
    }.items():
        path = tmp_path / rel
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(text)
    state = tmp_path / "state"
    state.mkdir()
    toolchain = tmp_path / "toolchain"
    toolchain.mkdir()
    monkeypatch.setattr(native_pgo, "ROOT", tmp_path)
    monkeypatch.setattr(
        native_pgo, "state_dir", lambda triple=None, device="dn2": state
    )
    monkeypatch.setattr(native_pgo, "toolchain_dir", lambda: toolchain)
    monkeypatch.setattr(native_pgo, "rustc_version", lambda: "rustc 1.98.1")
    profdata = state / "p.profdata"
    profdata.write_bytes(b"profile")
    hasher = native_pgo.FileHasher(state / "hash-cache.json")
    components, _ = native_pgo.compute_components(PACKAGES, tmp_path / "gen", hasher)
    manifest = {
        "components": components,
        "packages": PACKAGES,
        "desktop_sha256": native_pgo.desktop_digest(hasher),
        "profdata": {"path": str(profdata)},
    }
    (state / "manifest.json").write_text(json.dumps(manifest))
    return tmp_path


def state(tree):
    return native_pgo.check_state(tree / "gen", "")


def test_unchanged_tree_is_ok(tree):
    assert state(tree) == ("ok", str(tree / "state" / "p.profdata"), False)


def test_desktop_crate_change_reuses_the_profile(tree):
    (tree / "apps/desktop/src-tauri/main.rs").write_text("ui, diagnostics fix")
    status, _, desktop = state(tree)
    assert (status, desktop) == ("ok", True)
    assert native_pgo.check(tree / "gen", "")[0] == "ok"


def test_desktop_files_in_a_listed_package_list_are_ignored(tree):
    manifest_path = tree / "state" / "manifest.json"
    manifest = json.loads(manifest_path.read_text())
    manifest["packages"] = [*PACKAGES, "apps/desktop/src-tauri"]
    manifest_path.write_text(json.dumps(manifest))
    (tree / "apps/desktop/src-tauri/main.rs").write_text("changed")
    assert state(tree)[0] == "ok"


def test_emulation_crate_change_is_partly_stale(tree):
    (tree / "packages/emulator/sharc/lib.rs").write_text("a changed")
    status, detail, _ = state(tree)
    assert (status, detail) == ("partial", "changed: sources")


def test_rustc_change_is_stale(tree, monkeypatch):
    monkeypatch.setattr(native_pgo, "rustc_version", lambda: "rustc 1.99.0")
    assert state(tree)[:2] == ("stale", "changed: rustc")


def test_sources_plus_generated_change_is_stale(tree):
    (tree / "packages/emulator/sharc/lib.rs").write_text("a changed")
    (tree / "gen/core.rs").write_text("new core")
    status, detail, _ = state(tree)
    assert status == "stale"
    assert "sources" in detail and "generated" in detail


def test_generated_core_change_is_stale_but_fast_regions_are_not(tree):
    assert native_pgo.check_state(tree / "gen", "0x1")[0] == "ok"
    (tree / "gen/core.rs").write_text("new core")
    assert state(tree)[1] == "changed: generated"
    assert native_pgo.check_state(tree / "gen", "0x1")[1] == "changed: generated"


def test_blocktrans_roots_do_not_make_the_profile_stale(tree):
    assert native_pgo.check_state(tree / "gen", "", "0xb8276c")[0] == "ok"


def test_blocktrans_roots_follow_env_then_table(monkeypatch, tmp_path):
    syx = tmp_path / "fw.syx"
    syx.write_bytes(b"dn2")
    table = tmp_path / "fast.json"
    digest = hashlib.sha256(b"dn2").hexdigest()
    table.write_text(json.dumps({digest: {"blocktrans": "0x2"}}))
    monkeypatch.setattr(digi_env, "FAST_TABLE", table)
    monkeypatch.setenv("DIGI_COUPLED_SYX", str(syx))
    monkeypatch.delenv("DIGI_EMU_SYX", raising=False)
    digi_env._syx_digest.cache_clear()
    assert native_pgo.blocktrans_roots({"SHARC_BLOCKTRANS": "0x1"}) == "0x1"
    assert native_pgo.blocktrans_roots({"DIGI_EMU_FAST": "0"}) == ""
    assert native_pgo.blocktrans_roots({"DIGI_COUPLED_SYX": str(syx)}) == "0x2"
    assert native_pgo.blocktrans_roots() == "0x2"


@pytest.fixture
def supported(monkeypatch):
    monkeypatch.setattr(native_pgo, "supported_host", lambda: True)


def test_profile_for_tests_ok_returns_the_profile(tree, supported):
    profile, status, message = native_pgo.profile_for_tests("dn2", tree / "gen", {})
    assert (profile, status, message) == (str(tree / "state" / "p.profdata"), "ok", "")


def test_profile_for_tests_partial_returns_profile_and_notice(tree, supported):
    (tree / "packages/emulator/sharc/lib.rs").write_text("a changed")
    profile, status, message = native_pgo.profile_for_tests("dn2", tree / "gen", {})
    assert (profile, status) == (str(tree / "state" / "p.profdata"), "partial")
    assert message == native_pgo.PARTIAL_NOTICE


def test_profile_for_tests_stale_and_missing_return_no_profile(tree, supported):
    (tree / "gen/core.rs").write_text("new core")
    profile, status, message = native_pgo.profile_for_tests("dn2", tree / "gen", {})
    assert (profile, status) == (None, "stale")
    assert "changed: generated" in message
    (tree / "state" / "manifest.json").unlink()
    profile, status, message = native_pgo.profile_for_tests("dn2", tree / "gen", {})
    assert (profile, status) == (None, "missing")
    assert "Retrain" in message


def test_profile_for_tests_off_when_opted_out(tree, supported):
    env = {"DIGI_EMU_PGO": "0"}
    assert native_pgo.profile_for_tests("dn2", tree / "gen", env) == (None, "off", "")


def test_report_profile_require_pgo_is_fatal_for_stale_only(capsys):
    env = {"DIGI_REQUIRE_PGO": "1"}
    with pytest.raises(SystemExit, match="DIGI_REQUIRE_PGO"):
        native_pgo.report_profile("dn2", "stale", "WARNING: x", False, env)
    native_pgo.report_profile("dn2", "partial", "partly stale", False, env)
    assert "partly stale" in capsys.readouterr().err


def test_current_profile_returns_the_file_even_when_stale(tree):
    (tree / "packages/emulator/sharc/lib.rs").write_text("a changed")
    (tree / "gen/core.rs").write_text("new core")
    assert state(tree)[0] == "stale"
    assert native_pgo.current_profile() == str(tree / "state" / "p.profdata")
    (tree / "state" / "p.profdata").unlink()
    assert native_pgo.current_profile() is None
    (tree / "state" / "manifest.json").unlink()
    assert native_pgo.current_profile() is None


def test_missing_profile_file_and_manifest(tree):
    (tree / "state" / "p.profdata").unlink()
    assert state(tree)[0] == "missing"
    (tree / "state" / "manifest.json").unlink()
    assert state(tree) == ("missing", "no manifest", False)


def test_cmd_check_reports_the_reuse(tree, monkeypatch, capsys):
    monkeypatch.setattr(native_pgo, "fast_regions", lambda: "")
    monkeypatch.setattr(native_pgo, "blocktrans_roots", lambda: "")
    monkeypatch.setattr(native_pgo, "generated_dir", lambda environ=None: tree / "gen")
    (tree / "apps/desktop/src-tauri/main.rs").write_text("ui 2")
    assert native_pgo.cmd_check(None) == 0
    assert (
        capsys.readouterr().out.strip() == "ok (desktop crate changed; profile reused)"
    )


def test_launch_plan_skips_a_stale_profile_with_one_line(tree, monkeypatch):
    monkeypatch.setattr(native_pgo, "supported_host", lambda: True)
    monkeypatch.setattr(native_pgo, "toolchain_rustc", lambda: "/fake/rustc")
    monkeypatch.setattr(native_pgo, "generated_dir", lambda environ=None: tree / "gen")
    (tree / "gen/core.rs").write_text("new core")
    plan, notice = native_pgo.launch_plan({})
    assert plan is None
    assert "stale for this core; building without PGO" in notice
    assert "mise run perf:pgo" in notice
    assert "-no-pgo-warn-mismatch" in native_pgo.profile_use_flags("p")


def test_launch_plan_uses_a_partly_stale_profile(tree, monkeypatch):
    monkeypatch.setattr(native_pgo, "supported_host", lambda: True)
    monkeypatch.setattr(native_pgo, "toolchain_rustc", lambda: "/fake/rustc")
    monkeypatch.setattr(native_pgo, "generated_dir", lambda environ=None: tree / "gen")
    monkeypatch.setattr(
        native_pgo,
        "native_target",
        lambda: type("T", (), {"target_dir": lambda *a, **k: tree / "target"}),
    )
    (tree / "packages/emulator/sharc/lib.rs").write_text("a changed")
    plan, notice = native_pgo.launch_plan({})
    assert plan is not None
    assert "-no-pgo-warn-mismatch" in plan.env["RUSTFLAGS"]
    assert str(tree / "state" / "p.profdata") in plan.env["RUSTFLAGS"]
    assert notice == (
        "PGO profile partly stale (sources changed since training); using it. "
        "Retrain at milestones with `mise run perf:pgo`."
    )


def test_dt2_state_reads_the_dt2_manifest(tree, monkeypatch):
    dt2 = tree / "state-dt2"
    dt2.mkdir()
    monkeypatch.setattr(
        native_pgo,
        "state_dir",
        lambda triple=None, device="dn2": tree / "state" if device == "dn2" else dt2,
    )
    assert native_pgo.check_state(tree / "gen", "", "", "dt2")[0] == "missing"
    manifest = json.loads((tree / "state" / "manifest.json").read_text())
    # The DT2 key carries the DT2 workload name, so a DN2 manifest is stale.
    (dt2 / "manifest.json").write_text(json.dumps(manifest))
    assert native_pgo.check_state(tree / "gen", "", "", "dt2")[1] == "changed: workload"
    manifest["components"]["workload"] = native_pgo.DT2_WORKLOAD_NAME
    (dt2 / "manifest.json").write_text(json.dumps(manifest))
    assert native_pgo.check_state(tree / "gen", "", "", "dt2")[0] == "ok"
    assert native_pgo.current_profile("dt2") == str(tree / "state" / "p.profdata")


def trained_here(tree, name="lane.profdata"):
    """Write by-key/<key>.json for the tree as it is now, with its own profile file."""
    state_dir = tree / "state"
    hasher = native_pgo.FileHasher(state_dir / "hash-cache.json")
    components, _ = native_pgo.compute_components(PACKAGES, tree / "gen", hasher)
    profdata = state_dir / name
    profdata.write_bytes(b"lane profile")
    manifest = {
        "key": native_pgo.key_of(components),
        "components": components,
        "packages": PACKAGES,
        "desktop_sha256": native_pgo.desktop_digest(hasher),
        "profdata": {"path": str(profdata)},
    }
    native_pgo.write_json_atomic(
        native_pgo.by_key_dir() / f"{manifest['key']}.json", manifest
    )
    return profdata


def test_exact_key_match_uses_its_profile_while_the_shared_manifest_is_partial(tree):
    (tree / "packages/emulator/sharc/lib.rs").write_text("a lane edit")
    assert state(tree)[0] == "partial"
    lane = trained_here(tree)
    assert state(tree) == ("ok", str(lane), False)
    assert native_pgo.current_profile(gen=tree / "gen") == str(lane)


def test_key_miss_falls_back_to_the_shared_manifest(tree):
    trained_here(tree)  # the key of the tree as trained
    (tree / "packages/emulator/sharc/lib.rs").write_text("another edit")
    status, detail, _ = state(tree)
    assert (status, detail) == ("partial", "changed: sources")
    assert native_pgo.current_profile(gen=tree / "gen") == str(
        tree / "state" / "p.profdata"
    )


def test_exact_match_without_its_profile_file_is_not_used(tree):
    (tree / "packages/emulator/sharc/lib.rs").write_text("a lane edit")
    trained_here(tree).unlink()
    assert state(tree)[0] == "partial"


def test_linked_worktree_training_leaves_the_shared_manifest(tree, monkeypatch, capsys):
    shared = (tree / "state" / "manifest.json").read_text()
    manifest = {"key": "k" * 64, "profdata": {"path": "x"}}
    monkeypatch.setattr(native_pgo, "in_linked_worktree", lambda: True)
    native_pgo.save_manifest("dn2", manifest)
    assert (tree / "state" / "manifest.json").read_text() == shared
    assert native_pgo.read_by_key("dn2", "k" * 64) == manifest
    assert "manifest.json is unchanged" in capsys.readouterr().out
    native_pgo.save_manifest("dn2", manifest, set_current=True)
    assert native_pgo.read_manifest() == manifest


def test_main_worktree_training_writes_both(tree, monkeypatch):
    manifest = {"key": "m" * 64, "profdata": {"path": "x"}}
    monkeypatch.setattr(native_pgo, "in_linked_worktree", lambda: False)
    native_pgo.save_manifest("dn2", manifest)
    assert native_pgo.read_manifest() == manifest
    assert native_pgo.read_by_key("dn2", "m" * 64) == manifest


# --- crate-hash guard -------------------------------------------------------------

PROFILE_TEXT = """Counters:
  _RNvNtCsAAA111_12sharc_native6engine3run:
  _RNvCsBBB222_9sharc_gen4core:
  _RNvCsCCC333_8sharc_rt3mem:
  elektron_native_boot.1234-cgu.03;_RNvCsDDD444_20elektron_native_boot4boot:
  _RNvNtCsOTHER_4core3ptr9drop_glue:
  _RNvCsEEE555_12sharc_native3dup:
"""


def test_crates_in_maps_crates_to_disambiguators():
    assert native_pgo.crates_in(PROFILE_TEXT) == {
        "sharc_native": {"AAA111", "EEE555"},
        "sharc_gen": {"BBB222"},
        "sharc_rt": {"CCC333"},
        "elektron_native_boot": {"DDD444"},
    }


def test_dt2_app_ignores_its_own_boot_crate():
    profile = {"sharc_native": {"A"}, "elektron_native_boot": {"B"}}
    binary = {"sharc_native": {"A"}, "elektron_native_boot": {"C"}}
    assert native_pgo.crate_mismatches(profile, binary)
    ignore = native_pgo.DT2_APP_UNPROFILED
    assert native_pgo.crate_mismatches(profile, binary, ignore) == []
    binary["sharc_native"] = {"Z"}
    assert native_pgo.crate_mismatches(profile, binary, ignore)


def test_crate_mismatches_reports_only_disjoint_crates():
    profile = native_pgo.crates_in(PROFILE_TEXT)
    same = {k: set(v) for k, v in profile.items()}
    assert native_pgo.crate_mismatches(profile, same) == []
    other = {**same, "sharc_gen": {"NEW"}}
    assert native_pgo.crate_mismatches(profile, other) == [
        "MISMATCH crate sharc_gen: profile ['BBB222'] binary ['NEW']"
    ]


def test_fallback_regions_follow_explicit_fast_optout_and_table(monkeypatch, tmp_path):
    syx = tmp_path / "fw.syx"
    syx.write_bytes(b"fw")
    table = tmp_path / "fast.json"
    table.write_text(
        json.dumps({hashlib.sha256(b"fw").hexdigest(): {"fallback_regions": "0x2"}})
    )
    monkeypatch.setattr(digi_env, "FAST_TABLE", table)
    base = {"DIGI_EMU_SYX": str(syx)}
    assert native_pgo.fallback_regions(base) == "0x2"
    assert native_pgo.fallback_regions({**base, "SHARC_FAST_REGIONS": ""}) == ""
    assert native_pgo.fallback_regions({**base, "DIGI_EMU_FAST": "0"}) == ""
    assert native_pgo.fallback_regions({**base, "SHARC_FAST_AUTO": "10,100"}) == ""
    assert (
        native_pgo.fallback_regions(
            {**base, "SHARC_FAST_AUTO": "10,100", "SHARC_FAST_FALLBACK_REGIONS": "0x3"}
        )
        == "0x3"
    )
    assert (
        native_pgo.fallback_regions(
            {**base, "SHARC_FAST_REGIONS": "", "SHARC_FAST_FALLBACK_REGIONS": "0x3"}
        )
        == "0x3"
    )
    assert (
        native_pgo.fallback_regions({**base, "SHARC_FAST_FALLBACK_REGIONS": ""}) == ""
    )


def test_fallback_configuration_reports_change_without_invalidating_profile(
    tree, monkeypatch, capsys
):
    monkeypatch.setenv("SHARC_FAST_FALLBACK_REGIONS", "0x2")
    monkeypatch.setattr(native_pgo, "fast_regions", lambda: "")
    monkeypatch.setattr(native_pgo, "blocktrans_roots", lambda: "")
    monkeypatch.setattr(native_pgo, "generated_dir", lambda environ=None: tree / "gen")
    assert state(tree)[0] == "ok"
    assert native_pgo.cmd_check(None) == 0
    assert (
        "fast regions changed since training; the profile still applies"
        in capsys.readouterr().out
    )


# --- both: the two-core build -------------------------------------------------------


@pytest.fixture
def both(tree, monkeypatch):
    """The tree with a second generated core and a manifest trained for device `both`."""
    # The real shared out/ may hold a reset fixture; it must not change this workload.
    monkeypatch.setattr(native_pgo, "RESET_FIXTURE_DIR", tree / "no-reset-fixture")
    (tree / "gen2").mkdir()
    (tree / "gen2/core.rs").write_text("dt2 core")
    state_both = tree / "state-both"
    state_both.mkdir()
    monkeypatch.setattr(
        native_pgo,
        "state_dir",
        lambda triple=None, device="dn2": (
            state_both if device == "both" else tree / "state"
        ),
    )
    hasher = native_pgo.FileHasher(state_both / "hash-cache.json")
    components, _ = native_pgo.compute_components(
        PACKAGES, tree / "gen", hasher, gen2=tree / "gen2"
    )
    (state_both / "manifest.json").write_text(
        json.dumps(
            {
                "components": components,
                "packages": PACKAGES,
                "desktop_sha256": native_pgo.desktop_digest(hasher),
                "profdata": {"path": str(tree / "state" / "p.profdata")},
            }
        )
    )
    return tree


def both_state(tree):
    return native_pgo.check_state(tree / "gen", "", "", "both", tree / "gen2")


def test_both_key_covers_the_second_core_the_feature_and_the_slot_order(tree):
    hasher = native_pgo.FileHasher(tree / "state" / "hash-cache.json")
    (tree / "gen2").mkdir()
    (tree / "gen2/core.rs").write_text("dt2 core")
    one, _ = native_pgo.compute_components(PACKAGES, tree / "gen", hasher)
    two, _ = native_pgo.compute_components(
        PACKAGES, tree / "gen", hasher, gen2=tree / "gen2"
    )
    assert "generated2" not in one
    assert two["generated2"] != two["generated"]
    assert two["features"] == "second-core"
    assert two["workload"] == "both-v1"
    assert two["slots"] == "generated=dn2,generated2=dt2"
    assert native_pgo.key_of(one) != native_pgo.key_of(two)


def test_both_profile_is_current_until_either_core_changes(both):
    assert both_state(both)[:2] == ("ok", str(both / "state" / "p.profdata"))
    (both / "gen2/core.rs").write_text("new dt2 core")
    assert both_state(both)[:2] == ("stale", "changed: generated2")
    (both / "gen2/core.rs").write_text("dt2 core")
    (both / "gen/core.rs").write_text("new dn2 core")
    assert both_state(both)[:2] == ("stale", "changed: generated")


def test_both_profile_is_not_a_single_core_profile(both):
    # The single-core manifest has no second core: the two keys never match.
    assert native_pgo.check_state(both / "gen", "", "", "dn2")[0] == "ok"
    status, detail, _ = native_pgo.check_state(
        both / "gen", "", "", "both", both / "gen2"
    )
    assert status == "ok"
    (both / "state-both" / "manifest.json").write_text(
        (both / "state" / "manifest.json").read_text()
    )
    changed = both_state(both)[1]
    assert "generated2" in changed and "features" in changed


def test_both_build_never_lands_in_the_single_core_pgo_use_dir(tmp_path, monkeypatch):
    from tools.emulator import native_target

    gens = {}
    for name in ("dn2", "dt2"):
        gens[name] = tmp_path / name
        gens[name].mkdir()
        (gens[name] / "image.rs").write_text("")
    monkeypatch.setattr(native_target, "TARGET_ROOT", tmp_path / "target")
    monkeypatch.setattr(native_pgo, "toolchain_rustc", lambda: "/fake/rustc")
    single = {"SHARC_GEN_DIR": str(gens["dn2"])}
    two = {**single, "SHARC_GEN_DIR_2": str(gens["dt2"])}
    one_dir = native_pgo.plan_for("p", single).cargo_args[3]
    two_dir = native_pgo.plan_for("p", two).cargo_args[3]
    assert two_dir != one_dir
    assert two_dir.endswith("/pgo-use")
    assert pathlib.Path(two_dir).parent.name.count("+") == 1
    assert str(native_pgo.pgo_binary(two)).startswith(two_dir)
    assert not str(native_pgo.pgo_binary(single)).startswith(two_dir)


def test_second_engine_crates_are_guarded():
    text = "_RNvCsAAA111_14sharc_native_26engine3run\n_RNvCsBBB222_12sharc_native3run\n_RNvCsCCC333_11sharc_gen_24core\n"
    assert native_pgo.crates_in(text) == {
        "sharc_native_2": {"AAA111"},
        "sharc_native": {"BBB222"},
        "sharc_gen_2": {"CCC333"},
    }
    profile = {"sharc_native_2": {"A"}, "sharc_native": {"B"}}
    binary = {"sharc_native_2": {"Z"}, "sharc_native": {"B"}}
    assert native_pgo.crate_mismatches(profile, binary) == [
        "MISMATCH crate sharc_native_2: profile ['A'] binary ['Z']"
    ]


def test_both_verify_ignores_the_boot_crate_but_not_the_second_engine(monkeypatch):
    profile = {"elektron_native_boot": {"B"}, "sharc_native_2": {"A"}}
    binary = {"elektron_native_boot": {"C"}, "sharc_native_2": {"A"}}
    monkeypatch.setattr(native_pgo, "profile_crates", lambda _p: profile)
    monkeypatch.setattr(native_pgo, "binary_crates", lambda _b: binary)
    assert native_pgo.verify_binary("bin", "p", "dn2")
    assert native_pgo.verify_binary("bin", "p", "both") == []
    binary["sharc_native_2"] = {"Z"}
    assert native_pgo.verify_binary("bin", "p", "both")


def test_each_device_sets_the_region_lists_of_its_own_engine_slot():
    env = {
        "SHARC_FAST_REGIONS": "0x1",
        "SHARC_BLOCKTRANS": "0x2",
        "NOTE_EVENTS": "trig",
    }
    assert digi_env.slot_env("dn2", env) == env
    assert digi_env.slot_env("dt2", env) == {
        "SHARC_FAST_REGIONS_2": "0x1",
        "SHARC_BLOCKTRANS_2": "0x2",
        "NOTE_EVENTS": "trig",
    }


def run_one_item(tmp_path, monkeypatch, device, gate_ok=True):
    """run_items with a fake coupled-run; returns the environment and argv it was given."""
    seen = {}

    def run(argv, env, **_kwargs):
        seen["argv"], seen["env"] = argv, env
        report = pathlib.Path(argv[argv.index("--out") + 1])
        report.write_text(
            json.dumps({"scenarios": [{"name": "s", "gates": {"ok": gate_ok}}]})
        )
        return subprocess.CompletedProcess(argv, 0, "", "")

    monkeypatch.setattr(subprocess, "run", run)
    monkeypatch.setattr(
        digi_env,
        "fast_env",
        lambda environ=None, *, syx=None, context="tests": {"SHARC_FAST_REGIONS": "0xaa"},
    )
    item = native_pgo.WorkItem(
        f"{device}-x",
        device,
        "tests/scenarios/dn2.json",
        ("s",),
        ("--syx", "fw"),
        tmp_path / "fw",
        5.0,
    )
    native_pgo.run_items(tmp_path / "coupled-run", tmp_path, {}, [item], slots=True)
    return seen


def test_training_runs_one_process_with_the_tiers_of_its_slot(tmp_path, monkeypatch):
    monkeypatch.delenv("SHARC_FAST_REGIONS", raising=False)
    monkeypatch.delenv("SHARC_FAST_REGIONS_2", raising=False)
    dn2 = run_one_item(tmp_path, monkeypatch, "dn2")
    assert dn2["env"]["SHARC_FAST_REGIONS"] == "0xaa"
    assert "SHARC_FAST_REGIONS_2" not in dn2["env"]
    assert dn2["argv"][dn2["argv"].index("--seconds") + 1] == "5.0"
    dt2 = run_one_item(tmp_path, monkeypatch, "dt2")
    assert dt2["env"]["SHARC_FAST_REGIONS_2"] == "0xaa"
    assert "SHARC_FAST_REGIONS" not in dt2["env"]


def test_a_failed_gate_aborts_the_training(tmp_path, monkeypatch):
    with pytest.raises(SystemExit, match="gate failed"):
        run_one_item(tmp_path, monkeypatch, "dt2", gate_ok=False)


# --- runtime fixtures ---------------------------------------------------------------


def write_fixture(directory, card=True):
    directory.mkdir(parents=True)
    names = {
        "firmware": "firmware.syx",
        "dsp_image": "dsp-image.bin",
        "dsp_state": "dsp-state.snap.dsp",
        "cf_snapshot": "cf-snapshot.snap",
        "card": "card.img" if card else None,
    }
    sums = {}
    for key, name in names.items():
        if name:
            (directory / name).write_bytes(key.encode())
            sums[key] = hashlib.sha256(key.encode()).hexdigest()
        else:
            sums[key] = None
    (directory / "provenance.json").write_text(
        json.dumps(
            {
                "schema": 1,
                "device": "dt2",
                "firmware_version": "1.17",
                "patch_set": "emulator",
                "files": names,
                "sha256": sums,
                "source_head": "abc123",
                "created": "2026-11-01T00:00:00Z",
            }
        )
    )


def test_fixture_is_checked_against_its_provenance_and_recorded(tmp_path):
    write_fixture(tmp_path / "dt2")
    fixture = native_pgo.load_fixture("dt2", tmp_path / "dt2")
    args = fixture.input_args()
    assert args[args.index("--card-image") + 1] == str(tmp_path / "dt2" / "card.img")
    record = fixture.record()
    assert record["source_head"] == "abc123"
    assert record["sha256"]["dsp_image"] == hashlib.sha256(b"dsp_image").hexdigest()


def test_fixture_without_a_card_runs_without_card_image(tmp_path):
    write_fixture(tmp_path / "dt2", card=False)
    assert (
        "--card-image"
        not in native_pgo.load_fixture("dt2", tmp_path / "dt2").input_args()
    )


def test_a_changed_fixture_file_is_refused_with_the_make_command(tmp_path):
    write_fixture(tmp_path / "dt2")
    (tmp_path / "dt2" / "dsp-image.bin").write_bytes(b"other")
    with pytest.raises(SystemExit, match=r"perf:fixture -- --device dt2"):
        native_pgo.load_fixture("dt2", tmp_path / "dt2")


def test_a_missing_fixture_names_the_make_command(tmp_path):
    with pytest.raises(SystemExit, match=r"perf:fixture -- --device dn2"):
        native_pgo.load_fixture("dn2", tmp_path / "none")


def test_the_dn2_reset_fixture_is_an_extra_training_input_and_names_the_workload(
    tmp_path, capsys
):
    directory = tmp_path / "dn2-reset"
    assert native_pgo.load_reset_fixture(directory) is None
    assert "no DN2 reset fixture" in capsys.readouterr().out
    assert not native_pgo.reset_fixture_present(directory)
    assert native_pgo.both_workload_name(False) == native_pgo.BOTH_WORKLOAD_NAME

    write_fixture(directory)
    provenance = json.loads((directory / "provenance.json").read_text())
    provenance.update(device="dn2", state="reset")
    (directory / "provenance.json").write_text(json.dumps(provenance))
    reset = native_pgo.load_reset_fixture(directory)
    assert reset is not None
    assert native_pgo.reset_fixture_present(directory)
    assert native_pgo.both_workload_name(True) != native_pgo.both_workload_name(False)

    # a fixture that is present but changed is an error, not a silent skip
    (directory / "dsp-image.bin").write_bytes(b"other")
    with pytest.raises(SystemExit, match="provenance sha256"):
        native_pgo.load_reset_fixture(directory)


def test_a_fixture_of_another_device_is_refused(tmp_path):
    write_fixture(tmp_path / "dt2")
    with pytest.raises(SystemExit, match=r"perf:fixture -- --device dn2"):
        native_pgo.load_fixture("dn2", tmp_path / "dt2")


# --- clean -------------------------------------------------------------------------------

NOW = 1_800_000_000.0
DAY = 86400


@pytest.fixture
def pgo(tmp_path):
    root = tmp_path / "pgo"
    state = root / "dn2" / "tri"
    (state / "profiles").mkdir(parents=True)
    (state / "by-key").mkdir()
    return root, state


def age(path, days):
    when = NOW - days * DAY
    os.utime(path, (when, when), follow_symlinks=False)


def profile(state, name, days):
    path = state / "profiles" / f"{name}.profdata"
    path.write_bytes(b"p")
    crates = state / "profiles" / f"{name}.profdata.crates.json"
    crates.write_text("{}")
    age(path, days)
    age(crates, days)
    return path


def entry(state, key, name, days, where=None):
    path = state / "by-key" / f"{key}.json"
    where = where or state / "profiles"
    path.write_text(json.dumps({"profdata": {"path": str(where / f"{name}.profdata")}}))
    age(path, days)


def manifest(state, name, where=None):
    where = where or state / "profiles"
    (state / "manifest.json").write_text(
        json.dumps({"profdata": {"path": str(where / f"{name}.profdata")}})
    )


def clean(pgo, days=14):
    root, state = pgo
    plan = native_pgo.plan_clean(state, days, now=NOW, root=root)
    native_pgo.apply_clean(plan, root=root)
    return plan


def test_clean_keeps_old_profile_named_by_manifest_even_from_another_lane(pgo):
    _, state = pgo
    profile(state, "a", 90)
    manifest(state, "a", where=pgo[0] / "elsewhere-lane" / "out" / "profiles")
    clean(pgo)
    assert (state / "profiles" / "a.profdata").exists()
    assert (state / "profiles" / "a.profdata.crates.json").exists()


def test_clean_removes_old_unreferenced_profile_and_keeps_young_one(pgo):
    _, state = pgo
    manifest(state, "m")
    profile(state, "m", 90)
    profile(state, "old", 30)
    profile(state, "young", 2)
    clean(pgo)
    names = {p.name for p in (state / "profiles").iterdir()}
    assert names == {
        "m.profdata",
        "m.profdata.crates.json",
        "young.profdata",
        "young.profdata.crates.json",
    }


def test_clean_evicts_by_key_beyond_newest_four_and_removes_profile_when_old(pgo):
    _, state = pgo
    manifest(state, "m")
    profile(state, "m", 1)
    for number in range(6):
        entry(state, f"k{number}", f"p{number}", 100 - number)
        profile(state, f"p{number}", 100 - number)
    entry(state, "young-extra", "py", 3)  # newest, so k0..k2 fall out of the top four
    profile(state, "py", 3)
    clean(pgo)
    kept = {p.stem for p in (state / "by-key").iterdir()}
    # newest four by mtime: young-extra, k5, k4, k3; k2 and older are old and evicted
    assert kept == {"young-extra", "k5", "k4", "k3"}
    profiles = {p.name for p in (state / "profiles").glob("*.profdata")}
    assert profiles == {
        "m.profdata",
        "py.profdata",
        "p5.profdata",
        "p4.profdata",
        "p3.profdata",
    }


def test_clean_keeps_young_by_key_entry_beyond_four(pgo):
    _, state = pgo
    manifest(state, "m")
    profile(state, "m", 1)
    for number in range(6):
        entry(state, f"k{number}", f"p{number}", 1 + number)
        profile(state, f"p{number}", 1 + number)
    clean(pgo)
    assert len(list((state / "by-key").iterdir())) == 6
    assert len(list((state / "profiles").glob("*.profdata"))) == 7


def test_clean_leaves_corrupt_by_key_and_keeps_all_profiles_without_manifest(pgo):
    _, state = pgo
    profile(state, "a", 90)
    (state / "by-key" / "bad.json").write_text("{not json")
    age(state / "by-key" / "bad.json", 90)
    plan = clean(pgo)
    assert (state / "by-key" / "bad.json").exists()
    assert (state / "profiles" / "a.profdata").exists()
    assert any("bad.json" in note for note in plan.notes)
    assert any("manifest.json unreadable" in note for note in plan.notes)


def test_clean_removes_old_experiment_dirs_by_name_and_age(pgo):
    _, state = pgo
    manifest(state, "m")
    for name in (
        "bench-1",
        "ab-new-bin",
        "control-target",
        "raw-abc",
        "train-2",
        "bench-young",
    ):
        (state / name).mkdir()
        (state / name / "f").write_text("x")
        age(state / name / "f", 90)
        age(state / name, 90)
    (state / "bench-young" / "f").write_text("touched")  # newest mtime inside is now
    os.utime(state / "bench-young" / "f", (NOW - DAY, NOW - DAY))
    for name in ("profile-1", "other-target", "train-x", "bench-file"):
        path = state / name
        path.mkdir() if name != "bench-file" else path.write_text("x")
        age(path, 90)
    clean(pgo)
    left = {p.name for p in state.iterdir()} - {"profiles", "by-key", "manifest.json"}
    assert left == {
        "bench-young",
        "other-target",
        "profile-1",
        "train-x",
        "bench-file",
    }


def test_clean_never_follows_a_symlink(pgo, tmp_path):
    root, state = pgo
    manifest(state, "m")
    outside = tmp_path / "outside"
    outside.mkdir()
    (outside / "keep").write_text("x")
    age(outside / "keep", 90)
    (state / "bench-link").symlink_to(outside)
    age(state / "bench-link", 90)
    (state / "bench-real").mkdir()
    (state / "bench-real" / "inner").symlink_to(outside)
    age(state / "bench-real", 90)
    age(state / "bench-real" / "inner", 90)
    clean(pgo)
    assert (outside / "keep").exists()
    assert not (state / "bench-link").is_symlink()
    assert not (state / "bench-real").exists()


def test_clean_refuses_a_state_dir_outside_the_pgo_root(pgo, tmp_path):
    root, state = pgo
    other = tmp_path / "other"
    (other / "profiles").mkdir(parents=True)
    old = other / "bench-1"
    old.mkdir()
    age(old, 90)
    plan = native_pgo.plan_clean(other, 14, now=NOW, root=root)
    assert plan.items == [] and old.exists()


def test_auto_clean_failure_does_not_fail_training(monkeypatch, capsys):
    def boom(*_args, **_kwargs):
        raise RuntimeError("disk gone")

    monkeypatch.setattr(native_pgo, "AUTO_CLEAN", True)
    monkeypatch.setattr(native_pgo, "clean_all", boom)
    native_pgo.auto_clean()
    assert "pgo clean failed: disk gone" in capsys.readouterr().err


def test_the_tree_digest_ignores_generated_output_inside_a_crate(tmp_path):
    """A test run that writes `out/` into a crate must not make the profile stale."""
    (tmp_path / "src").mkdir()
    (tmp_path / "src" / "lib.rs").write_text("fn main() {}")
    before = [p.name for p in native_pgo.tree_files(tmp_path)]
    (tmp_path / "out" / "x").mkdir(parents=True)
    (tmp_path / "out" / "x" / "REPORT.txt").write_text("report")
    assert [p.name for p in native_pgo.tree_files(tmp_path)] == before == ["lib.rs"]
