"""Freeze native binaries, then resume correctness-first SHARC A/B validation.

See docs/TESTING.md, "Resumable SHARC matrix", for the JSON contracts.
"""

from __future__ import annotations

import argparse
import contextlib
import hashlib
import json
import math
import os
import re
import shutil
import statistics
import subprocess
import sys
from collections.abc import Iterator
from pathlib import Path

from tools import python_env
from tools.devtools import (
    benchlock,
    coupled_ab,
    portable_flock,
    render_cache,
    stablebin,
)
from tools.emulator import coupled, digi_env, native_emu

ROOT = Path(__file__).resolve().parents[2]
DEVICES = ("dt2", "dn2")
INPUTS = {"firmware", "DSP image", "DSP state", "CF snapshot"}
THREADS = {"OMP_NUM_THREADS", "RAYON_NUM_THREADS", "OPENBLAS_NUM_THREADS"}


Invalid = coupled_ab.Invalid
wav_pcm = coupled_ab.wav_pcm


def digest(path: Path) -> str:
    h = hashlib.sha256()
    with path.open("rb") as stream:
        for block in iter(lambda: stream.read(1024 * 1024), b""):
            h.update(block)
    return h.hexdigest()


def read(path: Path) -> dict:
    return json.loads(path.read_text())


def write(path: Path, data: dict) -> None:
    partial = path.with_suffix(path.suffix + ".part")
    partial.write_text(json.dumps(data, indent=2, sort_keys=True) + "\n")
    partial.replace(path)


def file(value: str) -> Path:
    path = Path(value).resolve()
    if not path.is_file():
        raise Invalid(f"missing file: {path}")
    return path


def name(value: str) -> str:
    if not isinstance(value, str) or not re.fullmatch(
        r"[A-Za-z0-9][A-Za-z0-9_.-]*", value
    ):
        raise Invalid(f"unsafe name: {value!r}")
    return value


def knob(key: str) -> bool:
    return key.startswith(("SHARC_", "DIGI_", "DSP_", "DIGIKIT_")) or key in THREADS


def overlay(*layers: dict) -> dict[str, str]:
    result: dict[str, str] = {}
    for layer in layers:
        for key, value in layer.items():
            if not knob(key) or (value is not None and not isinstance(value, str)):
                raise Invalid(f"expected emulation environment string or null: {key}")
            if value is None:
                result.pop(key, None)
            else:
                result[key] = value
    return result


def child_env(env: dict[str, str]) -> dict[str, str]:
    return python_env({**{k: v for k, v in os.environ.items() if not knob(k)}, **env})


@contextlib.contextmanager
def writer(folder: Path) -> Iterator[None]:
    folder.mkdir(parents=True, exist_ok=True)
    with (folder / ".writer.lock").open("a") as lock:
        try:
            portable_flock.flock(lock, portable_flock.LOCK_EX | portable_flock.LOCK_NB)
        except BlockingIOError as e:
            raise Invalid(f"another writer owns {folder}") from e
        try:
            yield
        finally:
            portable_flock.flock(lock, portable_flock.LOCK_UN)


def tree_hashes(folder: Path) -> dict[str, str]:
    return {
        str(p.relative_to(folder)): digest(p)
        for p in sorted(folder.rglob("*"))
        if p.is_file()
    }


def tool_hashes() -> dict[str, str]:
    # Includes transitive Python helpers and module inputs used by PLUCK's proof.
    paths = [p for p in (ROOT / "tools").rglob("*.py")]
    paths.extend((ROOT / "devices" / "ports").rglob("*.toml"))
    for base in (
        "modules",
        "sdk",
        "tests/digikit/scenarios",
        "tests/fixtures/modules",
        "packages",
    ):
        paths.extend(
            p
            for p in (ROOT / base).rglob("*")
            if p.is_file()
            and not {
                "__pycache__",
                "build",
                "target",
                "node_modules",
                "dist",
            }.intersection(p.parts)
            and p.suffix
            in {
                ".py",
                ".rs",
                ".toml",
                ".lock",
                ".json",
                ".ts",
                ".c",
                ".h",
                ".s",
                ".inc",
            }
        )
    return {str(p.relative_to(ROOT)): digest(p) for p in sorted(paths)}


def freeze(folder: Path, imported: Path | None) -> None:
    """Import {device: {coupled, daemon, profile_firmware}}, or build existing targets."""
    sources = (
        read(imported)
        if imported
        else {
            device: {
                "profile_firmware": str(
                    digi_env.ready_inputs(device=device)["firmware"]
                )
            }
            for device in DEVICES
        }
    )
    prepared = {}
    for device in DEVICES:
        item = sources[device]
        syx = file(item["profile_firmware"])
        prepared[device] = {
            "profile_firmware": str(syx),
            "env": overlay(item["env"])
            if "env" in item
            else digi_env.fast_env({}, syx=syx),
        }
        if imported:
            for kind in ("coupled", "daemon"):
                prepared[device][kind] = str(file(item[kind]))
        else:
            # Check both generated trees before the first build.
            digi_env.check_generated(coupled.generated(device))
    with writer(folder):
        if (folder / "frozen.json").exists():
            raise Invalid("freeze destination already contains frozen.json")
        for device, item in prepared.items():
            if not imported:
                target = coupled.binary_path(device).parent.parent
                with (
                    benchlock.hold("shared", "matrix freeze"),
                    stablebin.build_lock(target),
                ):
                    item["coupled"] = str(
                        stablebin.stable_copy(coupled.build(device), target)
                    )
                # Explicit core avoids native_emu's ambient/default core selection.
                gen = coupled.generated(device)
                target = Path(native_emu.target_dir_for(gen))
                command = native_emu.daemon_command(
                    ["--coupled", str(item["profile_firmware"])]
                )
                command[command.index("--target-dir") + 1] = str(target)
                with (
                    benchlock.hold("shared", "matrix freeze daemon"),
                    stablebin.build_lock(target),
                ):
                    subprocess.run(
                        native_emu.build_command(command),
                        cwd=ROOT,
                        env=digi_env.rust_env(
                            {**child_env({}), "SHARC_GEN_DIR": str(gen)}
                        ),
                        check=True,
                    )
                    item["daemon"] = str(
                        stablebin.stable_copy(
                            native_emu.app_binary(command, None), target
                        )
                    )
            for kind in ("coupled", "daemon"):
                dest = folder / f"{device}-{kind}"
                shutil.copyfile(item[kind], dest)
                dest.chmod(0o755)
                item[kind] = str(dest.resolve())
        files = {
            value: digest(file(value))
            for item in prepared.values()
            for key, value in item.items()
            if key != "env"
        }
        write(
            folder / "frozen.json",
            {
                "version": 1,
                "devices": prepared,
                "files": files,
                "freeze_tools": tool_hashes(),
                "provenance": "imported: source provenance unknown"
                if imported
                else "built here",
                "build_env": {k: v for k, v in os.environ.items() if knob(k)},
            },
        )


def frozen(folder: Path) -> dict:
    doc = read(file(str(folder / "frozen.json")))
    for path, expected in doc["files"].items():
        if digest(file(path)) != expected:
            raise Invalid(f"changed frozen file: {path}")
    return doc


def preflight(config: dict, baseline: dict, candidate: dict, partial: bool) -> dict:
    files: set[Path] = set()
    cells = config["cells"]
    if not cells:
        raise Invalid("no replay cells")
    ids = set()
    coverage = set()
    envs = {}
    for cell in cells:
        ident = name(cell["id"])
        if ident in ids:
            raise Invalid(f"duplicate cell: {ident}")
        ids.add(ident)
        device, kind = cell["device"], cell["kind"]
        if device not in DEVICES or kind not in ("stock", "runtime"):
            raise Invalid(f"invalid device/kind: {ident}")
        coverage.add(f"{device}/{kind}")
        if set(cell["inputs"]) != INPUTS:
            raise Invalid(f"{ident}: require exactly four coupled inputs")
        files.update(file(p) for p in cell["inputs"].values())
        files.add(file(cell["scenarios"]))
        if cell.get("card"):
            files.add(file(cell["card"]))
        if not cell["names"] or len(set(cell["names"])) != len(cell["names"]):
            raise Invalid(f"{ident}: empty or duplicate scenarios")
        document = coupled.scenario_document(cell["scenarios"])
        available = {s["name"] for s in document["scenarios"]}
        for scenario in cell["names"]:
            if name(scenario) not in available:
                raise Invalid(f"{ident}: missing scenario {scenario}")
        for label, artifact in (("A", baseline), ("B", candidate)):
            env = overlay(
                artifact["devices"][device]["env"],
                config.get("env", {}),
                cell.get("env", {}),
                config.get("variants", {}).get(label, {}),
                cell.get("variants", {}).get(label, {}),
            )
            envs[f"{ident}/{label}"] = env
    instruments = config.get("instruments", list(DEVICES))
    if len(set(instruments)) != len(instruments) or any(
        d not in DEVICES for d in instruments
    ):
        raise Invalid("instruments must list dt2 and/or dn2")
    coverage.update(f"{d}/pluck" for d in instruments)
    required = {f"{d}/{k}" for d in DEVICES for k in ("stock", "runtime", "pluck")}
    missing = sorted(required - coverage)
    if missing and not partial:
        raise Invalid(f"incomplete coverage (use --allow-partial for pilot): {missing}")
    for device in instruments:
        for label, artifact in (("A", baseline), ("B", candidate)):
            envs[f"pluck-{device}/{label}"] = overlay(
                artifact["devices"][device]["env"],
                config.get("env", {}),
                config.get("variants", {}).get(label, {}),
            )
    for env in envs.values():
        for value in env.values():
            if value.startswith("@"):
                files.add(file(value[1:]))
    for field, default in (
        ("pairs", 3),
        ("seconds", 3),
        ("timeout", 300),
        ("instrument_timeout", 1800),
    ):
        value = config.get(field, default)
        if (
            not isinstance(value, (int, float))
            or not math.isfinite(value)
            or value <= 0
        ):
            raise Invalid(f"{field} must be finite and positive")
    if not isinstance(config.get("pairs", 3), int):
        raise Invalid("pairs must be an integer")
    return {
        "files": {str(p): digest(p) for p in sorted(files)},
        "env": envs,
        "scenario_documents": {
            cell["id"]: coupled.scenario_document(cell["scenarios"]) for cell in cells
        },
        "devices": {
            label: render_cache.devices_digest(child_env(env))
            for label, env in envs.items()
        },
        "coverage": sorted(coverage),
        "missing": missing,
    }


def process(argv: list[str], env: dict, folder: Path, timeout: float) -> None:
    coupled_ab.process(argv, child_env(env), folder, timeout)


def replay(
    binary: str,
    cell: dict,
    scenario: str,
    env: dict,
    seconds: float,
    folder: Path,
    timeout: float,
) -> dict:
    return coupled_ab.artifact_run(
        binary,
        scenario,
        cell["scenarios"],
        inputs=cell["inputs"],
        env=child_env(env),
        seconds=seconds,
        folder=folder,
        timeout=timeout,
        card=cell.get("card"),
    )


def equal(a: dict, b: dict) -> None:
    for key in ("pcm", "state", "icount", "audio_s"):
        if a[key] != b[key]:
            raise Invalid(f"A/B mismatch: {key}")


def instrument(
    binary: str, device: str, env: dict, folder: Path, timeout: float
) -> dict:
    salt = hashlib.sha256(
        json.dumps(
            {
                "stage": str(folder.resolve()),
                "binary": digest(Path(binary)),
                "env": env,
            },
            sort_keys=True,
        ).encode()
    ).hexdigest()
    env = {**env, f"DIGI_EMUD_{device.upper()}": binary, "DIGIKIT_RUN_SALT": salt}
    image = {"dt2": "dt2-1.17", "dn2": "dn2-1.12"}[device]
    process(
        [
            sys.executable,
            "-m",
            "tools.patchkit.pk_digikit_scen",
            "run",
            "equiv-dyn-pluck",
            "--image",
            image,
            "--keep",
            str(folder / "keep"),
        ],
        env,
        folder,
        timeout,
    )
    work = folder / "keep" / f"equiv-dyn-pluck-{image}"
    if read(work / "report.json")["pass"] is not True:
        raise Invalid(f"PLUCK proof failed: {folder}")
    return {key: wav_pcm(work / f"{key}.wav")[0] for key in ("run", "ref")}


def summary(state: dict) -> dict:
    ratios = {}
    cycle_ratios: dict[str, list[float]] = {}
    insn_ratios: dict[str, list[float]] = {}
    for key, step in state["steps"].items():
        if key.startswith("timing/"):
            cell = key.rsplit("/", 1)[0].removeprefix("timing/")
            ratios.setdefault(cell, []).append(
                step["result"]["B"]["dsp_cpu"] / step["result"]["A"]["dsp_cpu"]
            )
            for name, store in (
                ("dsp_cycles", cycle_ratios),
                ("dsp_insns", insn_ratios),
            ):
                a = step["result"]["A"].get(name)
                b = step["result"]["B"].get(name)
                if a and b:
                    store.setdefault(cell, []).append(b / a)

    def stats(groups: dict[str, list[float]]) -> dict:
        return {
            key: {
                "median": statistics.median(v),
                "min": min(v),
                "max": max(v),
                "pairs": len(v),
            }
            for key, v in groups.items()
        }

    return {
        "status": state["status"],
        "coverage": state["identity"]["preflight"]["coverage"],
        "missing": state["identity"]["preflight"]["missing"],
        "failure_stage": state.get("failure_stage"),
        "completed_steps": len(state["steps"]),
        "ratios_B_over_A": {
            key: {
                "median": statistics.median(v),
                "min": min(v),
                "max": max(v),
                "pairs": len(v),
            }
            for key, v in ratios.items()
        },
        "cycle_ratios_B_over_A": stats(cycle_ratios),
        "insn_ratios_B_over_A": stats(insn_ratios),
        "tiers": {
            key.removeprefix("correctness/"): {
                label: result["tiers"] for label, result in step["result"].items()
            }
            for key, step in state["steps"].items()
            if key.startswith("correctness/")
        },
    }


def validate_steps(state: dict, folder: Path) -> None:
    config = state["identity"]["config"]
    expected = set()
    for cell in config["cells"]:
        for scenario in cell["names"]:
            expected.add(f"correctness/{cell['id']}/{scenario}")
            expected.update(
                f"timing/{cell['id']}/{scenario}/{i}"
                for i in range(config.get("pairs", 3))
            )
    expected.update(f"instrument/{d}" for d in config.get("instruments", list(DEVICES)))
    if not set(state["steps"]) <= expected:
        raise Invalid("unexpected checkpoint steps")
    for key, step in state["steps"].items():
        if (
            read(folder / key / "result.json") != step["result"]
            or tree_hashes(folder / key) != step["files"]
        ):
            raise Invalid(f"resume artifact missing or changed: {key}")
    if state["status"] in ("pass", "partial-pass") and set(state["steps"]) != expected:
        raise Invalid("checkpoint claims completion with missing steps")


def status(folder: Path) -> dict:
    state = read(folder / "checkpoint.json")
    identity = state["identity"]
    if (
        digest(file(identity["config_path"])) != identity["config_sha"]
        or tool_hashes() != identity["tools"]
    ):
        raise Invalid("status identity changed: config or tools")
    for files in [
        identity["preflight"]["files"],
        *[a["files"] for a in identity["frozen"].values()],
    ]:
        for path, expected in files.items():
            if digest(file(path)) != expected:
                raise Invalid(f"status input changed: {path}")
    if {
        key: render_cache.devices_digest(child_env(env))
        for key, env in identity["preflight"]["env"].items()
    } != identity["preflight"]["devices"]:
        raise Invalid("status device tables changed")
    validate_steps(state, folder)
    return summary(state)


def current_identity(
    config_path: Path, baseline_path: Path, candidate_path: Path, partial: bool
) -> dict:
    config = read(config_path)
    artifacts = {"A": frozen(baseline_path), "B": frozen(candidate_path)}
    prepared = preflight(config, artifacts["A"], artifacts["B"], partial)
    for folder in (baseline_path, candidate_path):
        manifest = file(str(folder / "frozen.json"))
        prepared["files"][str(manifest)] = digest(manifest)
    return {
        "config": config,
        "config_sha": digest(config_path),
        "frozen": artifacts,
        "tools": tool_hashes(),
        "python": sys.version,
        "allow_partial": partial,
        "config_path": str(config_path),
        "preflight": prepared,
    }


def run(
    config_path: Path,
    baseline_path: Path,
    candidate_path: Path,
    folder: Path,
    partial: bool,
) -> dict:
    identity = current_identity(config_path, baseline_path, candidate_path, partial)
    config, artifacts = identity["config"], identity["frozen"]
    with writer(folder):
        checkpoint = folder / "checkpoint.json"
        if checkpoint.exists():
            state = read(checkpoint)
            if state["identity"] != identity:
                raise Invalid(
                    "resume identity changed: config, inputs, binary, tools or environment"
                )
            validate_steps(state, folder)
        else:
            state = {"identity": identity, "steps": {}, "status": "incomplete"}
            write(checkpoint, state)
        state["status"] = "incomplete"
        state.pop("failure_stage", None)
        seconds, timeout = config.get("seconds", 3), config.get("timeout", 300)

        def step(key: str, action) -> None:
            if key in state["steps"]:
                return
            state["failure_stage"] = key
            write(checkpoint, state)
            dest = folder / key
            if dest.exists():
                shutil.rmtree(dest)  # incomplete pairs restart both sides
            dest.mkdir(parents=True)
            result = action(dest)
            write(dest / "result.json", result)
            state["steps"][key] = {"result": result, "files": tree_hashes(dest)}
            state.pop("failure_stage", None)
            write(checkpoint, state)

        def pair(cell: dict, scenario: str, order: tuple, dest: Path) -> dict:
            results = {}
            for label in order:
                results[label] = replay(
                    artifacts[label]["devices"][cell["device"]]["coupled"],
                    cell,
                    scenario,
                    identity["preflight"]["env"][f"{cell['id']}/{label}"],
                    seconds,
                    dest / label,
                    timeout,
                )
            equal(results["A"], results["B"])
            return results

        try:
            with benchlock.hold("shared", "SHARC matrix correctness"):
                for cell in config["cells"]:
                    for scenario in cell["names"]:
                        step(
                            f"correctness/{cell['id']}/{scenario}",
                            lambda dest, cell=cell, scenario=scenario: pair(
                                cell, scenario, ("A", "B"), dest
                            ),
                        )
                for device in config.get("instruments", list(DEVICES)):

                    def proof(dest: Path, device=device) -> dict:
                        results = {
                            label: instrument(
                                artifacts[label]["devices"][device]["daemon"],
                                device,
                                identity["preflight"]["env"][f"pluck-{device}/{label}"],
                                dest / label,
                                config.get("instrument_timeout", 1800),
                            )
                            for label in ("A", "B")
                        }
                        if results["A"] != results["B"]:
                            raise Invalid(f"PLUCK A/B mismatch: {device}")
                        return results

                    step(f"instrument/{device}", proof)
            if benchlock.disabled():
                raise Invalid("BENCHLOCK=0 cannot protect matrix timings")
            with benchlock.hold("exclusive", "SHARC validation matrix"):
                for cell in config["cells"]:
                    for scenario in cell["names"]:
                        for index in range(config.get("pairs", 3)):
                            order = ("A", "B") if index % 2 == 0 else ("B", "A")

                            def timed(
                                dest: Path, cell=cell, scenario=scenario, order=order
                            ) -> dict:
                                results = pair(cell, scenario, order, dest)
                                proof = state["steps"][
                                    f"correctness/{cell['id']}/{scenario}"
                                ]["result"]
                                for label in ("A", "B"):
                                    equal(proof[label], results[label])
                                return results

                            step(f"timing/{cell['id']}/{scenario}/{index}", timed)
            state["failure_stage"] = "completion/identity"
            write(checkpoint, state)
            if (
                current_identity(config_path, baseline_path, candidate_path, partial)
                != identity
            ):
                raise Invalid(
                    "completion identity changed: config, inputs, frozen manifests, tools or environment"
                )
            state.pop("failure_stage", None)
            state["status"] = (
                "partial-pass" if identity["preflight"]["missing"] else "pass"
            )
        except Exception:
            state["status"] = "failed"
            raise
        finally:
            write(checkpoint, state)
            write(folder / "summary.json", summary(state))
        return summary(state)


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    commands = parser.add_subparsers(dest="command", required=True)
    fr = commands.add_parser("freeze")
    fr.add_argument("--out", required=True, type=Path)
    fr.add_argument("--import-manifest", type=Path)
    runner = commands.add_parser("run")
    runner.add_argument("config", type=Path)
    runner.add_argument("--baseline", required=True, type=Path)
    runner.add_argument("--candidate", required=True, type=Path)
    runner.add_argument("--out", required=True, type=Path)
    runner.add_argument("--allow-partial", action="store_true")
    status_parser = commands.add_parser("status")
    status_parser.add_argument("folder", type=Path)
    args = parser.parse_args()
    try:
        if args.command == "freeze":
            freeze(args.out.resolve(), args.import_manifest)
        elif args.command == "run":
            print(
                json.dumps(
                    run(
                        args.config.resolve(),
                        args.baseline,
                        args.candidate,
                        args.out.resolve(),
                        args.allow_partial,
                    ),
                    indent=2,
                )
            )
        else:
            print(json.dumps(status(args.folder), indent=2))
    except (
        Invalid,
        OSError,
        ValueError,
        KeyError,
        StopIteration,
        subprocess.CalledProcessError,
    ) as error:
        print(f"matrix: {error}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
