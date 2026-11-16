"""The PGO profile key covers the emulation crates, not the desktop UI crate."""

import json
import os
import pathlib

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


def test_sources_plus_generated_change_is_stale(tree):
    (tree / "packages/emulator/sharc/lib.rs").write_text("a changed")
    (tree / "gen/core.rs").write_text("new core")
    status, detail, _ = state(tree)
    assert status == "stale"
    assert "sources" in detail and "generated" in detail


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


def test_not_current_notice_names_the_command_per_device():
    key = "ab" * 32
    assert "NOT current" in native_pgo.not_current_notice("dn2", key)
    assert native_pgo.not_current_notice("dn2", key).endswith(
        "native_pgo train --set-current"
    )
    assert native_pgo.not_current_notice("both", key).endswith(
        "native_pgo train --device both --set-current"
    )


@pytest.mark.parametrize(
    ("set_current", "at_main", "head", "origin", "expected"),
    [
        (True, False, "a", "b", True),
        (False, True, "a", "a", True),
        (False, True, "a", "b", False),
        (False, True, None, None, False),
        (False, False, "a", "a", False),
    ],
)
def test_decide_set_current(set_current, at_main, head, origin, expected):
    assert native_pgo.decide_set_current(set_current, at_main, head, origin) is expected
