#!/usr/bin/env python3
"""Build the shared panel and run the native desktop emulator.

With local DN2 ready inputs and a generated core, the bare launcher restores
the audible DN2 session. The private inputs stay out of the repository: every
path is overridable, and unavailable inputs leave the ordinary CF diagnostic
emulator available.
"""

from __future__ import annotations

import os
import pathlib
import subprocess
import sys

ROOT = pathlib.Path(__file__).resolve().parents[1]
READY = ROOT / "snapshots" / "dn2-audio-ready-2026-10-03"
DEFAULT_GENERATED = ROOT / "out" / "native" / "dn2-audio" / "gen"


def generated_core() -> pathlib.Path:
    configured = os.environ.get("SHARC_GEN_DIR")
    return pathlib.Path(configured) if configured else DEFAULT_GENERATED


def default_audio_args(args: list[str] | None = None) -> list[str]:
    """Add local ready audio inputs to a normal launcher command line."""
    args = [] if args is None else list(args)
    paths = {
        "firmware": pathlib.Path(
            os.environ.get("DIGI_EMU_SYX", ROOT / "Digitone_II_OS1.11.syx")
        ),
        "DSP image": pathlib.Path(
            os.environ.get("DIGI_EMU_DSP_IMAGE", READY / "digi-audio-dn2-image.bin")
        ),
        "DSP state": pathlib.Path(
            os.environ.get("DIGI_EMU_DSP_STATE", READY / "digi-audio-m5.snap.dsp")
        ),
        "CF snapshot": pathlib.Path(
            os.environ.get("DIGI_EMU_CF_SNAPSHOT", READY / "digi-audio-m5.snap")
        ),
    }
    missing = [label for label, path in paths.items() if not path.is_file()]
    generated = generated_core()
    if not generated.is_dir():
        missing.append("generated DN2 core (set SHARC_GEN_DIR)")
    if missing:
        print(
            "Native audio is not set up (missing "
            + ", ".join(missing)
            + "); launching the CF-only diagnostic emulator. "
            "Use --cf-only to silence this notice.",
            file=sys.stderr,
        )
        return []
    os.environ.setdefault("SHARC_GEN_DIR", str(generated))
    value_options = {"--card-image", "--audio-buffer"}
    positional = next(
        (
            value
            for index, value in enumerate(args)
            if not value.startswith("--")
            and (index == 0 or args[index - 1] not in value_options)
        ),
        None,
    )
    if positional is None:
        args.insert(0, str(paths["firmware"]))
    args.extend(
        [
            "--auto-coupled",
            "--audio-profile-syx",
            str(paths["firmware"]),
            "--dsp-image",
            str(paths["DSP image"]),
            "--dsp-state",
            str(paths["DSP state"]),
            "--cf-snapshot",
            str(paths["CF snapshot"]),
        ]
    )
    if "--audio-buffer" not in args:
        args.extend(["--audio-buffer", "0"])
    return args


def cargo_command(args: list[str], pgo: object | None = None) -> list[str]:
    """The cargo command line; pgo is a native_pgo.LaunchPlan for the PGO build."""
    command = [
        "cargo",
        "run",
        "--release",
        "--locked",
        "--manifest-path",
        "packages/desktop/src-tauri/Cargo.toml",
    ]
    if "--coupled" in args or "--auto-coupled" in args:
        command.extend(["--features", "coupled-audio"])
        if pgo is not None:
            command = [*pgo.prefix, *command, *pgo.cargo_args]  # type: ignore[attr-defined]
    return [*command, "--", *args]


def pgo_plan(args: list[str]) -> object | None:
    """Profile-use plan for the coupled build, or None (with a notice) for the ordinary build."""
    if "--coupled" not in args and "--auto-coupled" not in args:
        return None
    sys.path.insert(0, str(ROOT / "tools"))
    try:
        import native_pgo
    except ImportError:
        return None
    finally:
        sys.path.pop(0)
    plan, notice = native_pgo.launch_plan(os.environ)
    if notice:
        print(notice, file=sys.stderr)
    return plan


def main(args: list[str] | None = None) -> None:
    args = sys.argv[1:] if args is None else args
    force_cf = "--cf-only" in args
    args = [arg for arg in args if arg != "--cf-only"]
    if force_cf and ("--coupled" in args or "--auto-coupled" in args):
        raise SystemExit("--cf-only cannot be combined with coupled audio options")
    if not force_cf and "--coupled" not in args and "--auto-coupled" not in args:
        args = default_audio_args(args)
    if "--coupled" in args or "--auto-coupled" in args:
        generated = generated_core()
        if not generated.is_dir():
            raise SystemExit(
                "--coupled needs SHARC_GEN_DIR pointing to a local generated DN2 core"
            )
        os.environ.setdefault("SHARC_GEN_DIR", str(generated))
    # The native shell serves the built panel; it does not load browser WASM.
    # Avoid rebuilding the browser core (and its local DSP code) for a desktop run.
    subprocess.run(
        ["pnpm", "--filter", "@digi/web", "exec", "astro", "build"],
        cwd=ROOT,
        check=True,
    )
    plan = pgo_plan(args)
    env = {**os.environ, **plan.env} if plan is not None else None  # type: ignore[attr-defined]
    subprocess.run(cargo_command(args, plan), cwd=ROOT, check=True, env=env)


if __name__ == "__main__":
    main()
