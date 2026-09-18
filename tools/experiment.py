#!/usr/bin/env python3
"""Run one repeatable baseline-versus-button emulator experiment.

Recipes are deliberately small JSON documents: a snapshot, exact endpoint,
panel dwell, repeat count, one press/release gesture, and memory ranges.
Outputs are firmware-derived and must remain below ignored ``out/``.
"""

import argparse
import hashlib
import json
import re
import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT / "tools"))
import snapdiff  # noqa: E402

SRAM = {"lo": 0x80000000, "hi": 0x80010000, "name": "sram"}
SAVED = re.compile(r"saved snapshot at (\d+) instrs -> (.+)$", re.MULTILINE)
DELIVERED_INPUT = re.compile(r"^\[guirun\] input ~", re.MULTILINE)
DELIVERED_FEED = re.compile(
    r"^\[guirun\] input --feed (\d+):([0-9a-f]+) \(asked (\d+)\)$", re.MULTILINE
)
FAULTS = re.compile(r"^\[guirun\] faults: (\d+) distinct pages touched$", re.MULTILINE)
PANEL_RAW = re.compile(
    r"^\[guirun\] panel raw asked (\d+) latched (\d+) -> (.+)$", re.MULTILINE
)
MAINLOOP = re.compile(r"^\[guirun\] end: .* mainloop=(\d+) ", re.MULTILINE)
A1_FEEDS = [(400000, "2201"), (8400000, "2002"), (20400000, "2000"), (28400000, "2200")]


class RecipeError(ValueError):
    """A recipe cannot describe this narrow experiment."""


def sha256(path):
    h = hashlib.sha256()
    with open(path, "rb") as f:
        for block in iter(lambda: f.read(1024 * 1024), b""):
            h.update(block)
    return h.hexdigest()


def _integer(value, name):
    if isinstance(value, bool) or not isinstance(value, int) or value < 0:
        raise RecipeError("%s must be a non-negative integer" % name)
    return value


def load_recipe(path):
    """Load JSON while refusing duplicate keys (notably two manipulations)."""

    def object_without_duplicates(pairs):
        result = {}
        for key, value in pairs:
            if key in result:
                raise RecipeError("duplicate JSON key: %s" % key)
            result[key] = value
        return result

    return json.loads(
        Path(path).read_text(), object_pairs_hook=object_without_duplicates
    )


def validate_recipe(recipe):
    """Validate and normalize the intentionally fixed Phase 1 recipe shape."""
    if not isinstance(recipe, dict):
        raise RecipeError("recipe must be an object")
    required = {
        "name",
        "snapshot",
        "endpoint",
        "repeat",
        "exact",
        "panel_dwell",
        "manipulation",
    }
    unknown = set(recipe) - (required | {"ranges", "a1"})
    missing = required - set(recipe)
    if missing or unknown:
        raise RecipeError(
            "recipe keys missing=%s unknown=%s" % (sorted(missing), sorted(unknown))
        )
    if not isinstance(recipe["name"], str) or not re.fullmatch(
        r"[A-Za-z0-9._-]+", recipe["name"]
    ):
        raise RecipeError(
            "name must contain only letters, digits, dot, underscore, or dash"
        )
    if not isinstance(recipe["snapshot"], str) or not recipe["snapshot"]:
        raise RecipeError("snapshot must be a path string")
    endpoint = _integer(recipe["endpoint"], "endpoint")
    if endpoint == 0:
        raise RecipeError("endpoint must be positive")
    if recipe["exact"] is not True:
        raise RecipeError("Phase 1 requires exact=true (fast mode is forbidden)")
    repeat = _integer(recipe["repeat"], "repeat")
    if repeat < 2:
        raise RecipeError("repeat must be at least 2")
    dwell = _integer(recipe["panel_dwell"], "panel_dwell")
    if dwell == 0:
        raise RecipeError("panel_dwell must be positive")
    if "manipulations" in recipe or not isinstance(recipe["manipulation"], dict):
        raise RecipeError("exactly one manipulation object is required")
    action = recipe["manipulation"]
    if set(action) == {"code", "press", "release"}:
        code = _integer(action["code"], "manipulation.code")
        press = _integer(action["press"], "manipulation.press")
        release = _integer(action["release"], "manipulation.release")
        if release <= press or press >= endpoint or release >= endpoint:
            raise RecipeError(
                "manipulation must occur before endpoint and release after press"
            )
        manipulation = {"code": code, "press": press, "release": release}
    elif set(action) == {"feeds"}:
        feeds = action["feeds"]
        if not isinstance(feeds, list) or not feeds:
            raise RecipeError("manipulation.feeds must be a non-empty list")
        normalized_feeds, previous = [], -1
        for i, feed in enumerate(feeds):
            if not isinstance(feed, dict) or set(feed) != {"at", "hex"}:
                raise RecipeError(
                    "manipulation.feeds[%d] must contain only at, hex" % i
                )
            at = _integer(feed["at"], "manipulation.feeds[%d].at" % i)
            text = feed["hex"]
            if (
                at <= previous
                or at >= endpoint
                or not isinstance(text, str)
                or not re.fullmatch(r"[0-9a-fA-F]{4}", text)
            ):
                raise RecipeError(
                    "feeds must be strictly increasing, before endpoint, and exactly two bytes"
                )
            normalized_feeds.append({"at": at, "hex": text.lower()})
            previous = at
        manipulation = {"feeds": normalized_feeds}
    else:
        raise RecipeError("manipulation must be legacy code/press/release or feeds")
    a1 = recipe.get("a1")
    observation = None
    if a1 is not None:
        if not isinstance(a1, dict) or set(a1) != {
            "observation_at",
            "panel_raw",
            "ui_trace",
            "block_coverage",
        }:
            raise RecipeError("a1 has unknown or missing keys")
        observation = _integer(a1["observation_at"], "a1.observation_at")
        if observation == 0 or not all(
            a1[k] is True for k in ("panel_raw", "ui_trace", "block_coverage")
        ):
            raise RecipeError("a1 requires positive observation and approved booleans")
        feeds_for_a1 = manipulation.get("feeds")
        if (
            not isinstance(feeds_for_a1, list)
            or len(feeds_for_a1) != 4
            or [(feed["at"], feed["hex"]) for feed in feeds_for_a1] != A1_FEEDS
            or not (
                feeds_for_a1[1]["at"] < observation < feeds_for_a1[2]["at"] < endpoint
            )
        ):
            raise RecipeError(
                "a1 observation must be after SRC-down and before SRC-up/end"
            )
    ranges = recipe.get("ranges", [SRAM])
    if not isinstance(ranges, list) or not ranges:
        raise RecipeError("ranges must be a non-empty list")
    normalized = []
    for i, region in enumerate(ranges):
        if (
            not isinstance(region, dict)
            or set(region) - {"lo", "hi", "name"}
            or "lo" not in region
            or "hi" not in region
        ):
            raise RecipeError("ranges[%d] must contain lo, hi, optional name" % i)
        lo, hi = (
            _integer(region["lo"], "ranges[%d].lo" % i),
            _integer(region["hi"], "ranges[%d].hi" % i),
        )
        if hi <= lo:
            raise RecipeError("ranges[%d].hi must exceed lo" % i)
        name = region.get("name", "%#x-%#x" % (lo, hi))
        if not isinstance(name, str) or not name:
            raise RecipeError("ranges[%d].name must be a non-empty string" % i)
        normalized.append({"lo": lo, "hi": hi, "name": name})
    return {
        "name": recipe["name"],
        "snapshot": recipe["snapshot"],
        "endpoint": endpoint,
        "repeat": repeat,
        "exact": True,
        "panel_dwell": dwell,
        "manipulation": manipulation,
        "ranges": normalized,
        **(
            {
                "a1": {
                    "observation_at": observation,
                    "panel_raw": True,
                    "ui_trace": True,
                    "block_coverage": True,
                }
            }
            if observation is not None
            else {}
        ),
    }


def resolve_snapshot_path(path):
    """Resolve recipe snapshot paths from the repository root, not the cwd."""
    path = Path(path)
    return path.resolve() if path.is_absolute() else (ROOT / path).resolve()


def endpoint_path(run_dir, case, repetition, lane=None, observation=False):
    root = run_dir / lane if lane else run_dir
    suffix = "observation" if observation else "repeat"
    return root / case / ("%s-%d.snap" % (suffix, repetition))


def command(
    recipe,
    snapshot,
    endpoint,
    case,
    firmware=None,
    observation=None,
    panel_raw=None,
    profile=None,
):
    """Build a guirun command; manipulated adds precisely two input transitions."""
    cmd = [
        sys.executable,
        str(ROOT / "tools" / "guirun.py"),
        str(snapshot),
        "--exact",
        "--limit",
        str(recipe["endpoint"]),
        "--panel-dwell",
        str(recipe["panel_dwell"]),
        "--save-at",
        "%d:%s" % (recipe["endpoint"], endpoint),
    ]
    if firmware is not None:
        cmd += ["--syx", str(firmware)]
    if observation is not None:
        cmd += ["--save-at", "%d:%s" % (recipe["a1"]["observation_at"], observation)]
    if panel_raw is not None:
        cmd += ["--panel-raw-at", "%d:%s" % (recipe["a1"]["observation_at"], panel_raw)]
    if profile is not None:
        cmd += [
            "--trace-ui-json",
            str(profile / "ui.json"),
            "--block-profile",
            str(profile / "blocks.json"),
        ]
    if case == "manipulated":
        action = recipe["manipulation"]
        if "feeds" in action:
            for feed in action["feeds"]:
                cmd += ["--feed", "%d:%s" % (feed["at"], feed["hex"])]
        else:
            cmd += [
                "--input",
                "%d:press:%d" % (action["press"], action["code"]),
                "--input",
                "%d:release:%d" % (action["release"], action["code"]),
            ]
    elif case != "baseline":
        raise ValueError("unknown case %r" % case)
    return cmd


def diff_snapshots(a_path, b_path, ranges):
    """Return snapdiff-compatible changed-byte report for selected ranges."""
    a, b = snapdiff._load_blob(str(a_path)), snapdiff._load_blob(str(b_path))
    regions = []
    for region in ranges:
        lo, hi = region["lo"], region["hi"]
        left, right = snapdiff.read(a, lo, hi), snapdiff.read(b, lo, hi)
        changed = [
            {
                "start": start,
                "end": end,
                "label": snapdiff.label(start),
                "a": left[start - lo : end - lo].hex(),
                "b": right[start - lo : end - lo].hex(),
            }
            for start, end in snapdiff.runs(left, right, lo, 4)
        ]
        regions.append(
            {
                "name": region["name"],
                "lo": lo,
                "hi": hi,
                "changed_bytes": sum(x != y for x, y in zip(left, right)),
                "runs": changed,
            }
        )
    return {"a": str(a_path), "b": str(b_path), "regions": regions}


def deterministic(records, requested=None, expected_input_batches=None):
    """Return whether a case saved the same actual boundary and endpoint each time."""
    if len(records) < 2:
        return False
    if any(record.get("returncode") != 0 for record in records):
        return False
    if any(record.get("fault_pages") != 0 for record in records):
        return False
    actuals = [record.get("actual_saved_instructions") for record in records]
    if any(actual is None for actual in actuals) or len(set(actuals)) != 1:
        return False
    if requested is not None and actuals[0] < requested:
        return False
    hashes = [record.get("endpoint_sha256") for record in records]
    if not all(hashes) or len(set(hashes)) != 1:
        return False
    return expected_input_batches is None or all(
        record.get("delivered_input_batches") == expected_input_batches
        for record in records
    )


def determinism_reason(records, requested=None, expected_input_batches=None):
    """Explain why a case is not deterministic at its requested save boundary."""
    if len(records) < 2:
        return "fewer than two repetitions"
    if any(record.get("returncode") != 0 for record in records):
        return "one or more repetitions returned nonzero"
    if any(record.get("fault_pages") is None for record in records):
        return "one or more repetitions did not report a final fault count"
    if any(record.get("fault_pages") != 0 for record in records):
        return "one or more repetitions touched fault pages"
    actuals = [record.get("actual_saved_instructions") for record in records]
    if any(actual is None for actual in actuals):
        return "one or more successful repetitions did not report a saved boundary"
    if len(set(actuals)) != 1:
        return "successful repetitions saved at different actual instruction boundaries"
    if requested is not None and actuals[0] < requested:
        return "successful repetitions saved before the requested instruction bound"
    hashes = [record.get("endpoint_sha256") for record in records]
    if not all(hashes):
        return "one or more repetitions did not create the requested endpoint"
    if len(set(hashes)) != 1:
        return "successful repetitions produced different endpoint hashes"
    if expected_input_batches is not None and any(
        record.get("delivered_input_batches") != expected_input_batches
        for record in records
    ):
        return (
            "one or more repetitions delivered the wrong number of panel input batches"
        )
    return None


def guarded_run_dir(out, name, run_id):
    root = (ROOT / "out").resolve()
    out = Path(out).resolve()
    if out != root and root not in out.parents:
        raise RecipeError("output must be inside ignored %s" % root)
    if not re.fullmatch(r"[A-Za-z0-9._-]+", run_id):
        raise RecipeError(
            "run-id must contain only letters, digits, dot, underscore, or dash"
        )
    run_dir = (out / "experiments" / name / run_id).resolve()
    if root not in run_dir.parents:
        raise RecipeError("run directory resolves outside ignored %s" % root)
    if run_dir.exists():
        raise RecipeError("run directory already exists: %s" % run_dir)
    return run_dir


def resolve_inputs():
    from emu import config

    main = Path(config.main_image()).resolve()
    firmware = Path(config.firmware()).resolve()
    sections = Path(config.sections_dir()).resolve()
    source = sections / ".source-sha256"
    if not source.is_file():
        raise RecipeError("missing sections source hash: %s" % source)
    expected = source.read_text().strip().split()[0].lower()
    actual = sha256(firmware)
    if expected != actual:
        raise RecipeError("sections source SHA-256 does not match firmware")
    return main, firmware, sections


def feed_tuples(output):
    """Exact ordered raw-feed deliveries: requested, actual, lowercase bytes."""
    return [
        (int(asked), int(actual), data.lower())
        for actual, data, asked in DELIVERED_FEED.findall(output)
    ]


def _a1_record(recipe, run_dir, lane, case, repetition, snapshot, firmware, runner):
    endpoint = endpoint_path(run_dir, case, repetition, lane)
    observation = endpoint_path(run_dir, case, repetition, lane, observation=True)
    panel_path = endpoint.with_suffix(".panel")
    profile = (
        endpoint.parent / ("profile-%d" % repetition) if lane == "profile" else None
    )
    endpoint.parent.mkdir(parents=True, exist_ok=True)
    if profile is not None:
        profile.mkdir()
    cmd = command(
        recipe, snapshot, endpoint, case, firmware, observation, panel_path, profile
    )
    completed = runner(cmd, cwd=str(ROOT), text=True, capture_output=True)
    output = completed.stdout + completed.stderr
    endpoint.with_suffix(".log").write_text(output)
    saved = SAVED.findall(output)
    panels = PANEL_RAW.findall(output)
    expected_saves = {
        str(observation): recipe["a1"]["observation_at"],
        str(endpoint): recipe["endpoint"],
    }
    save_records = [(int(actual), path) for actual, path in saved]
    save_ok = (
        len(save_records) == 2
        and {path for _, path in save_records} == set(expected_saves)
        and all(actual >= expected_saves[path] for actual, path in save_records)
    )
    save_at = {path: actual for actual, path in save_records}
    panel_records = [(int(asked), int(latch), path) for asked, latch, path in panels]
    panel_ok = (
        len(panel_records) == 1
        and panel_records[0][0] == recipe["a1"]["observation_at"]
        and panel_records[0][2] == str(panel_path)
        and panel_records[0][1] <= save_at.get(str(observation), -1)
    )
    faults = FAULTS.findall(output)
    expected = [] if case == "baseline" else A1_FEEDS
    delivered = feed_tuples(output)
    feed_ok = len(delivered) == len(expected) and all(
        requested == wanted_at and actual >= requested and data == wanted_hex
        for (requested, actual, data), (wanted_at, wanted_hex) in zip(
            delivered, expected
        )
    )
    return {
        "repetition": repetition,
        "command": cmd,
        "returncode": completed.returncode,
        "endpoint": str(endpoint),
        "observation": str(observation),
        "endpoint_sha256": sha256(endpoint) if endpoint.is_file() else None,
        "observation_sha256": sha256(observation) if observation.is_file() else None,
        "panel": str(panel_path),
        "panel_sha256": sha256(panel_path) if panel_path.is_file() else None,
        "panel_size": panel_path.stat().st_size if panel_path.is_file() else None,
        "panel_latch": panel_records[0][1] if panel_records else None,
        "actual_saved_instructions": save_at.get(str(endpoint)),
        "observation_saved_instructions": save_at.get(str(observation)),
        "save_ok": save_ok,
        "panel_ok": panel_ok,
        "fault_pages": int(faults[-1]) if faults else None,
        "mainloop": int(MAINLOOP.findall(output)[-1])
        if MAINLOOP.findall(output)
        else 0,
        "feeds": delivered,
        "expected_feeds": expected,
        "feed_match": feed_ok,
        "ui": str(profile / "ui.json") if profile else None,
        "blocks": str(profile / "blocks.json") if profile else None,
    }


def _a1_ui_trajectory(events):
    """Describe the selected low-backlog FUNC+SRC UI trajectory."""

    def first_after(start, *parts):
        return next(
            (
                index
                for index, event in enumerate(events[start:], start)
                if all(part in event for part in parts)
            ),
            None,
        )

    src_press = first_after(0, " q+ ", "SRC(2) 0x03")
    activation = (
        first_after(src_press + 1, " activate MachineSelectionView@")
        if src_press is not None
        else None
    )
    func_release = (
        first_after(activation + 1, " q+ ", "FUNC(17) 0x00")
        if activation is not None
        else None
    )
    after_activation = events[activation + 1 :] if activation is not None else []
    src_release_record = any("SRC(2) 0x12" in event for event in events)
    src_repeat_after_activation = any(
        "SRC(2)" in event and "repeat" in event for event in after_activation
    )
    view_close_after_activation = any(
        "MachineSelectionView@" in event
        and (" close-call " in event or " closed " in event)
        for event in after_activation
    )
    return {
        "src_press_queue_index": src_press,
        "machine_selection_activation_index": activation,
        "src_release_record": src_release_record,
        "src_repeat_after_activation": src_repeat_after_activation,
        "machine_selection_close_after_activation": view_close_after_activation,
        "func_release_queue_index": func_release,
        "valid": (
            src_press is not None
            and activation is not None
            and func_release is not None
            and not src_release_record
            and not src_repeat_after_activation
            and not view_close_after_activation
        ),
    }


def _a1_profile_outputs_valid(ui_data, block_data):
    """Accept only event evidence and sorted, explicitly perturbing blocks."""
    if not isinstance(ui_data, dict) or not isinstance(block_data, dict):
        return False
    events = ui_data.get("events")
    entries = block_data.get("entries")
    if (
        set(ui_data) != {"kind", "events"}
        or ui_data.get("kind") != "scoped dynamic call/view evidence"
        or not isinstance(events, list)
        or not all(isinstance(event, str) for event in events)
        or any("[ui] hook error" in event for event in events)
        or set(block_data) != {"perturbing", "kind", "entries"}
        or block_data.get("perturbing") is not True
        or block_data.get("kind") != "basic-block entries"
        or not isinstance(entries, list)
        or not entries
    ):
        return False
    addresses = []
    for entry in entries:
        if (
            not isinstance(entry, dict)
            or set(entry) != {"address", "hits"}
            or isinstance(entry["address"], bool)
            or not isinstance(entry["address"], int)
            or entry["address"] < 0
            or isinstance(entry["hits"], bool)
            or not isinstance(entry["hits"], int)
            or entry["hits"] <= 0
        ):
            return False
        addresses.append(entry["address"])
    return addresses == sorted(set(addresses))


def _a1_repeatable(records, lane):
    if len(records) < 2 or any(
        r["returncode"]
        or r["fault_pages"] != 0
        or not r["feed_match"]
        or not r["save_ok"]
        or not r["panel_ok"]
        or r["mainloop"] <= 0
        for r in records
    ):
        return False
    keys = [
        "endpoint_sha256",
        "observation_sha256",
        "actual_saved_instructions",
        "observation_saved_instructions",
        "panel_sha256",
        "panel_latch",
    ]
    if any(not r[k] for r in records for k in keys) or any(
        r["panel_size"] != 1024 for r in records
    ):
        return False
    if any(len({r[k] for r in records}) != 1 for k in keys):
        return False
    if len({tuple(r["feeds"]) for r in records}) != 1:
        return False
    if lane == "profile":
        for r in records:
            try:
                r["ui_data"] = json.loads(Path(r["ui"]).read_text())
                r["block_data"] = json.loads(Path(r["blocks"]).read_text())
            except (OSError, json.JSONDecodeError):
                return False
            if not _a1_profile_outputs_valid(r["ui_data"], r["block_data"]):
                return False
            r["ui_trajectory"] = _a1_ui_trajectory(r["ui_data"]["events"])
        if (
            len(
                {
                    json.dumps(r["ui_data"].get("events"), sort_keys=True)
                    for r in records
                }
            )
            != 1
            or len(
                {
                    json.dumps(r["block_data"].get("entries"), sort_keys=True)
                    for r in records
                }
            )
            != 1
        ):
            return False
    return True


def execute_a1(recipe, run_dir, runner, snapshot, firmware, report):
    report["lanes"] = {}
    for lane in ("state", "profile"):
        cases = {}
        for case in ("baseline", "manipulated"):
            records = [
                _a1_record(recipe, run_dir, lane, case, n, snapshot, firmware, runner)
                for n in range(1, recipe["repeat"] + 1)
            ]
            repeatable = _a1_repeatable(records, lane)
            if lane == "profile" and case == "manipulated":
                repeatable = repeatable and all(
                    r["ui_trajectory"]["valid"] for r in records
                )
            cases[case] = {"runs": records, "repeatable": repeatable}
        report["lanes"][lane] = {"perturbing": lane == "profile", "cases": cases}
    state, profile = (
        report["lanes"]["state"]["cases"],
        report["lanes"]["profile"]["cases"],
    )
    healthy = all(
        value["repeatable"]
        for lane in report["lanes"].values()
        for value in lane["cases"].values()
    )
    # Boundaries are comparable only inside the non-perturbing state lane.
    if healthy:
        state_boundaries = [
            (
                state[case]["runs"][0]["observation_saved_instructions"],
                state[case]["runs"][0]["actual_saved_instructions"],
            )
            for case in ("baseline", "manipulated")
        ]
        healthy = len(set(state_boundaries)) == 1
    panel_diff = (
        healthy
        and state["baseline"]["runs"][0]["panel_sha256"]
        != state["manipulated"]["runs"][0]["panel_sha256"]
    )
    ui_diff = block_diff = False
    if healthy:
        base, changed = (
            profile["baseline"]["runs"][0],
            profile["manipulated"]["runs"][0],
        )
        ui_diff = base["ui_data"]["events"] != changed["ui_data"]["events"]
        base_entries = {
            (entry["address"], entry["hits"])
            for entry in base["block_data"].get("entries", [])
        }
        changed_entries = {
            (entry["address"], entry["hits"])
            for entry in changed["block_data"].get("entries", [])
        }
        block_diff = base_entries != changed_entries
        report["profile_comparison"] = {
            "ui_evidence": "scoped dynamic call/view evidence, not a complete call graph",
            "manipulated_ui_trajectory": changed["ui_trajectory"],
            "baseline_block_entries": sorted(base_entries),
            "manipulated_block_entries": sorted(changed_entries),
        }
    report["state_diffs"] = {
        "observation": diff_snapshots(
            state["baseline"]["runs"][0]["observation"],
            state["manipulated"]["runs"][0]["observation"],
            recipe["ranges"],
        )
        if healthy
        else None,
        "final": diff_snapshots(
            state["baseline"]["runs"][0]["endpoint"],
            state["manipulated"]["runs"][0]["endpoint"],
            recipe["ranges"],
        )
        if healthy
        else None,
    }
    report["comparison_reason"] = (
        None
        if healthy and panel_diff and ui_diff and block_diff
        else "A1 lane health or required panel/UI/block difference failed"
    )
    report["success"] = report["comparison_reason"] is None
    return report


def execute(recipe, run_dir, runner=subprocess.run):
    snapshot = resolve_snapshot_path(recipe["snapshot"])
    if not snapshot.is_file():
        raise RecipeError("snapshot does not exist: %s" % snapshot)
    main, firmware, sections = resolve_inputs()
    report = {
        "recipe": recipe,
        "recipe_sha256": hashlib.sha256(
            json.dumps(recipe, sort_keys=True, separators=(",", ":")).encode()
        ).hexdigest(),
        "snapshot": str(snapshot),
        "snapshot_sha256": sha256(snapshot),
        "main_image": str(main),
        "main_image_sha256": sha256(main),
        "firmware": str(firmware),
        "firmware_sha256": sha256(firmware),
        "sections": str(sections),
        "cases": {},
    }
    if "a1" in recipe:
        return execute_a1(recipe, run_dir, runner, snapshot, firmware, report)
    for case in ("baseline", "manipulated"):
        records = []
        for repetition in range(1, recipe["repeat"] + 1):
            endpoint = endpoint_path(run_dir, case, repetition)
            endpoint.parent.mkdir(parents=True, exist_ok=True)
            cmd = command(recipe, snapshot, endpoint, case, firmware)
            completed = runner(cmd, cwd=str(ROOT), text=True, capture_output=True)
            output = completed.stdout + completed.stderr
            log = endpoint.with_suffix(".log")
            log.write_text(output)
            saved = SAVED.findall(output)
            faults = FAULTS.findall(output)
            records.append(
                {
                    "repetition": repetition,
                    "command": cmd,
                    "returncode": completed.returncode,
                    "log": str(log),
                    "endpoint": str(endpoint),
                    "requested_save_instructions": recipe["endpoint"],
                    "actual_saved_instructions": int(saved[-1][0]) if saved else None,
                    "endpoint_sha256": sha256(endpoint) if endpoint.is_file() else None,
                    "delivered_input_batches": len(DELIVERED_INPUT.findall(output)),
                    "fault_pages": int(faults[-1]) if faults else None,
                }
            )
        actuals = [record["actual_saved_instructions"] for record in records]
        expected_input_batches = 0 if case == "baseline" else 2
        report["cases"][case] = {
            "runs": records,
            "requested_save_instructions": recipe["endpoint"],
            "actual_saved_instructions": actuals[0] if len(set(actuals)) == 1 else None,
            "expected_input_batches": expected_input_batches,
            "deterministic": deterministic(
                records, recipe["endpoint"], expected_input_batches
            ),
            "determinism_reason": determinism_reason(
                records, recipe["endpoint"], expected_input_batches
            ),
        }
    baseline = report["cases"]["baseline"]
    manipulated = report["cases"]["manipulated"]
    if not baseline["deterministic"] or not manipulated["deterministic"]:
        report["comparison_reason"] = (
            "baseline or manipulated case is not deterministic"
        )
        report["diff"] = None
        report["success"] = False
    elif (
        baseline["actual_saved_instructions"]
        != manipulated["actual_saved_instructions"]
    ):
        report["comparison_reason"] = (
            "baseline and manipulated cases saved at different actual "
            "instruction boundaries"
        )
        report["diff"] = None
        report["success"] = False
    else:
        report["comparison_reason"] = None
        report["diff"] = diff_snapshots(
            endpoint_path(run_dir, "baseline", 1),
            endpoint_path(run_dir, "manipulated", 1),
            recipe["ranges"],
        )
        report["success"] = True
    return report


def main(argv=None):
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument("recipe")
    p.add_argument("--run-id", required=True)
    p.add_argument("--out", default=str(ROOT / "out"))
    args = p.parse_args(argv)
    try:
        raw = load_recipe(args.recipe)
        recipe = validate_recipe(raw)
        run_dir = guarded_run_dir(args.out, recipe["name"], args.run_id)
        run_dir.mkdir(parents=True)
        report = execute(recipe, run_dir)
        (run_dir / "report.json").write_text(
            json.dumps(report, indent=2, sort_keys=True) + "\n"
        )
    except (OSError, json.JSONDecodeError, RecipeError) as exc:
        p.error(str(exc))
    summary = {"run_dir": str(run_dir), "success": report["success"]}
    if "lanes" in report:
        summary["lanes"] = {
            lane: {case: value["repeatable"] for case, value in data["cases"].items()}
            for lane, data in report["lanes"].items()
        }
    else:
        summary["baseline_deterministic"] = report["cases"]["baseline"]["deterministic"]
        summary["manipulated_deterministic"] = report["cases"]["manipulated"][
            "deterministic"
        ]
    print(json.dumps(summary, indent=2))
    return 0 if report["success"] else 1


if __name__ == "__main__":
    sys.exit(main())
