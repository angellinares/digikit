#!/usr/bin/env python3
"""Generate a firmware's block library (AOT core) and register it by DSP image.

    uv run python -m tools.sharc_dev.aot_gen dt2 --card-image IMG [--extra 110000000]
    mise run perf:aot -- dt2 --card-image IMG
    mise run perf:aot -- dn2

The cheap, single-cycle version of tools/sharc_dev/sharc_dn2_aot.py, which needs a
DSPI2 capture; this needs only the .syx (`digi_env.firmware_for`):
  0. the SHARC database rsgen reads (out/sharcdb/<image>.sqlite), from the .syx's
     sections when missing,
  1. an interpreter profile of a trig run from the device's ready snapshot
     (`sharc_live` with DSP_PROFILE, as tools/emulator/ready.py makes it),
  2. `tools/sharc_dev/sharc_rsgen.py` on that profile (core and block code; the model-safe
     DN2 flags),
  3. the gen directory is entered in out/native/gen-index.json under the
     SHA-256 of the packed DSP image, which is how tools/emulator/native_emu.py and
     tools/emulator/coupled.py pick the core to build the player against
     (`digi_env.generated_dir(image=...)`). The first player build against it
     compiles `sharc-gen` (about 5 min) into its own target dir
     (tools/emulator/native_target.py; the DN2 core's is prod's).

Output is firmware-derived: out/native/<device>-audio/gen, with the command in its
PROVENANCE.txt. The DN2 one is the prod core: a DN2 gen this tool did not make (one
from sharc_dn2_aot, say) is refused, not overwritten.
"""

from __future__ import annotations

import argparse
import os
import pathlib
import subprocess
import sys

ROOT = pathlib.Path(__file__).resolve().parents[2]
from tools import python_env  # noqa: E402
from tools.emulator import digi_env, ready  # noqa: E402
from tools.sharc_dev import sharc_dn2_aot  # noqa: E402

MODEL_FLAGS = ["--model-safe", "--explicit-memory-model", "0", "--chain"]
RECIPE = "uv run python -m tools.sharc_dev.aot_gen"
# Cycle 0 runs interpreter-only, so it builds against the firmware-free core (`mise run gen:core`).
# The tool does not write it: other worktrees build against it.
SHARED_GEN = ROOT / "out" / "native" / "shared-core" / "gen"


def check_ours(device: str, gen: pathlib.Path) -> None:
    """Refuse to overwrite a DN2 gen another tool made: its recipe is in its PROVENANCE.txt."""
    if device != "dn2" or not gen.exists():
        return
    try:
        if (gen / "PROVENANCE.txt").read_text().startswith(RECIPE):
            return
    except OSError:
        pass
    raise SystemExit(
        f"{gen} was not made by aot_gen (see its PROVENANCE.txt): move it away to replace it"
    )


def sharc_db(syx: pathlib.Path, image: str) -> None:
    """Build out/sharcdb/IMAGE.sqlite (skipped when up to date), from the .syx's sections."""
    blob = ROOT / "out" / "sections" / image / "section_7_BLOB.bin"
    if not blob.is_file():
        subprocess.run(
            [sys.executable, "-m", "tools.firmware.extract", str(syx), "-o", str(blob.parent)],
            cwd=ROOT, env=python_env(), check=True,
        )  # fmt: skip
    subprocess.run(
        [sys.executable, "-m", "tools.sharc_dev.sharcdb", "build", str(blob)],
        cwd=ROOT, env=python_env(), check=True,
    )  # fmt: skip


def main(argv: list[str] | None = None) -> int:
    p = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    p.add_argument("device", choices=["dn2", "dt2"])
    p.add_argument(
        "--card-image", type=pathlib.Path, help="the +Drive image (DT2 only, required)"
    )
    p.add_argument("--extra", default="110000000", help="CF instructions after ready")
    p.add_argument(
        "--cycles",
        type=int,
        default=2,
        help="profile cycles: 0 is interpreter-only, each later one replays with the "
        "previous cycle's blocks on (that run records the block transitions)",
    )
    a = p.parse_args(argv)
    if (a.device == "dt2") != (a.card_image is not None):
        p.error("--card-image is required for dt2 and not taken for dn2")
    cfg = ready.DEVICES[a.device]
    syx = digi_env.firmware_for(a.device)
    out = ROOT / "out" / "native" / f"{a.device}-audio"
    gen = out / "gen"
    check_ours(a.device, gen)
    # Fail before any work when the shared core is missing or stale; the fix is `mise run gen:core`.
    digi_env.check_generated(SHARED_GEN)
    out.mkdir(parents=True, exist_ok=True)
    sharc_db(syx, cfg["image"])

    card = ["--card-image", str(a.card_image)] if a.card_image else []
    done = subprocess.run(
        [sys.executable, "-m", "tools.emulator.ready", a.device, "--syx", str(syx), *card],
        cwd=ROOT, env=python_env(), stdout=subprocess.PIPE, text=True, check=True,
    )  # fmt: skip
    ready_dir = pathlib.Path(
        next(
            line.partition("=")[2]
            for line in done.stdout.splitlines()
            if line.startswith("ready_dir=")
        )
    )
    examples = ready.build_examples(SHARED_GEN)
    prof = out / "prof"
    profiles = [prof]
    run = out / "run"
    run.mkdir(exist_ok=True)
    for name in ("snap", "snap.dsp"):  # sharc_live restores in place: work on copies
        (run / f"w.{name}").write_bytes(
            (ready_dir / f"digi-audio-m5.{name}").read_bytes()
        )
    image = ready_dir / "digi-audio-dn2-image.bin"
    env = {
        **os.environ,
        **{k: v for k, v in cfg["env"].items() if k != "SAVE_QUIET_FRAMES"},
        "NOTE_EVENTS": "trig",
        "SHARC_FAST_REGIONS": "",
        "SHARC_AOT": "0",
        "SHARC_BLOCKTRANS": "",
        **({"CARD_IMAGE": str(a.card_image)} if a.card_image else {}),
        "CF_SNAPSHOT": str(run / "w.snap"),
        "DSP_PROFILE": str(prof),
    }
    # The DSP files are in ready's own directory; a card's CF snapshot is in <out>/by-card/<sha12>/.
    idle = (ready_dir.parent.parent if a.card_image else ready_dir) / "cold-idle.dsp"
    subprocess.run(
        [str(examples / "sharc_live"), str(syx), str(idle),
         str(image), str(run / "w.wav"), a.extra],
        cwd=ROOT, env=env, check=True, stdout=subprocess.DEVNULL,
    )  # fmt: skip

    def rsgen(base: pathlib.Path) -> None:
        subprocess.run(
            [sys.executable, "-m", "tools.sharc_dev.sharc_rsgen", cfg["image"],
             "--coverage", str(base), "--entries", f"{base}.entries",
             "--transitions", f"{base}.trans", *MODEL_FLAGS,
             "--work", str(out / "work"), "--out", str(gen)],
            cwd=ROOT, env=python_env(), check=True,
        )  # fmt: skip

    rsgen(prof)
    for cycle in range(1, a.cycles):
        # Replay with the blocks built so far; only that run sees the block
        # transitions. rsgen then gets every cycle's profile, merged.
        examples = ready.build_examples(gen)
        this = out / f"prof-c{cycle}"
        for name in ("snap", "snap.dsp"):
            (run / f"w.{name}").write_bytes(
                (ready_dir / f"digi-audio-m5.{name}").read_bytes()
            )
        subprocess.run(
            [str(examples / "sharc_live"), str(syx), str(idle),
             str(image), str(run / "w.wav"), a.extra],
            cwd=ROOT, check=True, stdout=subprocess.DEVNULL,
            env={**env, "SHARC_AOT": "1", "DSP_PROFILE": str(this)},
        )  # fmt: skip
        profiles.append(this)
        merged = out / f"prof-merged-c{cycle}"
        for suffix in sharc_dn2_aot.PROFILE_KINDS:
            merged.with_name(merged.name + suffix).write_text(
                sharc_dn2_aot.merge_kind(
                    [pathlib.Path(str(c) + suffix).read_text() for c in profiles],
                    suffix,
                )
            )
        rsgen(merged)
    command = [RECIPE, *(sys.argv[1:] if argv is None else argv)]
    (gen / "PROVENANCE.txt").write_text(" ".join(command) + "\n")
    digi_env.register_gen(image, gen)
    print(f"gen {gen} registered for {digi_env.GEN_INDEX}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
