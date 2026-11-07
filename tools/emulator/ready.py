#!/usr/bin/env python3
"""Build a device's ready inputs from the .syx alone (no captured state).

    uv run python -m tools.emulator.ready dn2|dt2 [--out DIR] [--syx SYX]
                                 [--card-image IMG] [--base-sha256 SHA] [--force]
    mise run emu:ready -- dt2 [--card-image IMG]

Steps (each skipped when its output exists and --force is not given):
  1. digi-audio-dn2-image.bin  packed section 7 image (from the .syx's own section 7,
                               or --dsp-syx's)
  2. cold.state                DSP cold-start state (tools/sharc_dev/sharc_cold_state.py)
  3. cold-idle.dsp             `dsp_cold` example: the cold DSP run alone (about
                               20-60 s) to its first rest in the idle loop
  4. digi-audio-m5.snap(.dsp)  `sharc_live`: ColdFire cold boot with that DSP
                               state attached at frame 0 (for DT2 with its link
                               port 0 fed by the ColdFire's sample loads); saves
                               both at ready

The DSP files live in --out (default out/snapshots/<device>-audio-ready-cold-...,
git-ignored). With --card-image the CF snapshot depends on the card (its
sample list is in the restored RAM), so it goes to <out>/by-card/<sha12>/
next to links to the shared DSP files; the last line printed is
`ready_dir=<dir>` for launchers. Everything written is firmware-derived.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import pathlib
import subprocess
import sys

ROOT = pathlib.Path(__file__).resolve().parents[2]
from tools.devtools import benchlock  # noqa: E402
from tools.emulator import digi_env, native_target  # noqa: E402
from tools.sharc_dev import sharc_cold_state  # noqa: E402

# After ready the example runs a little further so the snapshot is not at the
# very first ready instruction; the QA inputs start after the save.
EXTRA_AFTER_READY = "1000000"

DEVICES = {
    "dn2": {
        "image": "dn2-1.12",
        "out": "out/snapshots/dn2-audio-ready-cold-2026-10-20",
        "env": {},
    },
    "dt2": {
        "image": "dt2-1.17",
        "out": "out/snapshots/dt2-audio-ready-cold-2026-10-20",
        "env": {
            "DSP_DEVICE": "dt2",
            "DSP_SAMPLE_LINK": "lp0",
            "SAVE_QUIET_FRAMES": "400",
        },
    },
}


def build_examples(gen: pathlib.Path | None = None) -> pathlib.Path:
    gen = gen or digi_env.generated_dir()
    digi_env.check_generated(gen)
    target = native_target.target_dir(gen)
    env = {**digi_env.rust_env(), "SHARC_GEN_DIR": str(gen)}
    for example in ("dsp_cold", "sharc_live"):
        cmd = [
            "rustup", "run", digi_env.TOOLCHAIN, "cargo", "build", "--release",
            "--manifest-path", "packages/emulator/boot/Cargo.toml",
            "--features", "sharc", "--example", example,
            "--target-dir", str(target),
        ]  # fmt: skip
        with benchlock.hold("shared", f"ready build {example}"):
            if subprocess.run(cmd, cwd=ROOT, env=env, check=False).returncode:
                raise SystemExit(f"ready: building {example} failed")
    return target / "release" / "examples"


def main(argv: list[str]) -> int:
    p = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    p.add_argument("device", choices=sorted(DEVICES))
    p.add_argument("--out", type=pathlib.Path)
    p.add_argument("--syx", type=pathlib.Path)
    p.add_argument(
        "--dsp-syx",
        type=pathlib.Path,
        help="take the DSP boot stream (section 7) from this .syx instead of --syx; "
        "the ColdFire still boots --syx (a DSP-only change, for example an appended "
        "block: the ColdFire boot of a larger container reaches ready at another "
        "instruction count, which would shift every PCM hash)",
    )
    p.add_argument("--card-image", type=pathlib.Path)
    p.add_argument(
        "--base-sha256",
        help="--syx is a patched build of the stock firmware with this SHA-256: "
        "the ColdFire boot accepts it (sharc_live's BASE_SHA256). Restore the result "
        "with coupled-run --cold-base SHA --dsp sharc",
    )
    p.add_argument("--force", action="store_true")
    a = p.parse_args(argv)
    cfg = DEVICES[a.device]
    syx = a.syx or digi_env.firmware_for(a.device)
    # DIGI_EMU_READY_DIR (DN2) / DIGI_EMU_READY_DIR_<DEVICE> name another ready
    # directory, as in digi_env: a second firmware's inputs live beside the first's.
    env_out = os.environ.get(
        "DIGI_EMU_READY_DIR"
        if a.device == "dn2"
        else f"DIGI_EMU_READY_DIR_{a.device.upper()}"
    )
    out = a.out or pathlib.Path(env_out or cfg["out"])
    out = out if out.is_absolute() else ROOT / out
    out.mkdir(parents=True, exist_ok=True)
    syx_sha = hashlib.sha256(syx.read_bytes()).hexdigest()
    image_name = cfg["image"]

    image = out / "digi-audio-dn2-image.bin"
    state = out / "cold.state"
    idle = out / "cold-idle.dsp"
    todo = lambda path: a.force or not path.exists()  # noqa: E731

    snap_dir = out
    if a.card_image is not None:
        card_sha = hashlib.sha256(a.card_image.read_bytes()).hexdigest()[:12]
        snap_dir = out / "by-card" / card_sha
        try:
            known = json.loads((snap_dir / "SOURCE.json").read_text())["syx_sha256"]
        except (OSError, KeyError, ValueError):
            known = syx_sha
        if known != syx_sha:
            # the same card under another firmware (a relinked MAIN OS can leave the
            # card image byte-identical): its snapshot must not be reused
            snap_dir = out / "by-card" / f"{card_sha}-{syx_sha[:8]}"
        snap_dir.mkdir(parents=True, exist_ok=True)
    snap = snap_dir / "digi-audio-m5.snap"

    # The DSP side comes from the .syx's own section 7 (so a .syx with a changed DSP stream, for example
    # an appended block, gets its own image and cold state); a stock .syx gives the same bytes as the
    # out/sections/<image> copy.
    memory = None
    if todo(image) or todo(state):
        from tools.patchkit import dsp_patch
        from tools.sharc_dev import sharcldr

        stream = dsp_patch.section_blob(a.dsp_syx or syx, 7)
        # The emulator flattens the block list and never runs the SHARC boot ROM: refuse what the ROM would.
        from tools.sharc_dev import sharc_bootcheck

        bad = sharc_bootcheck.check(stream).errors
        if bad:
            raise SystemExit(
                "the SHARC boot ROM would reject this DSP stream:\n  "
                + "\n  ".join(str(f) for f in bad)
            )
        memory = sharcldr.LoadedMemory.from_stream(
            stream, sharcldr.parse_blocks(stream)
        )
    if todo(image):
        from tools.sharc_dev import sharc_transpile_run

        image.write_bytes(sharc_transpile_run.pack_image(memory))
        print(f"{image}: {image.stat().st_size} bytes")
    if todo(state):
        blob, entry = sharc_cold_state.cold_blob_for_memory(memory)
        state.write_bytes(blob)
        print(f"{state}: entry sw {entry:#x}")
    if todo(idle) or todo(snap):
        examples = build_examples()
    env = {**os.environ, "SHARC_FAST_REGIONS": "", **cfg["env"]}
    if todo(idle):
        with benchlock.hold("shared", "ready cold DSP"):
            subprocess.run(
                [str(examples / "dsp_cold"), str(image), str(state), str(idle)],
                cwd=ROOT, env=env, check=True,
            )  # fmt: skip
    sidecar = idle.read_bytes()
    clock = int.from_bytes(sidecar[8:16], "little")
    if todo(snap):
        # sharc_live restores an existing snapshot; a rebuild must boot.
        snap.unlink(missing_ok=True)
        pathlib.Path(f"{snap}.dsp").unlink(missing_ok=True)
        raw = snap_dir / "cold-idle.state"
        raw.write_bytes(sidecar[24:])
        live_env = {
            **env,
            # Ready states are booted without software-forced interrupt delivery
            # (INTC INTFRC, default on since 2026-10-13): the DN2 PCM pins and the
            # DT2 FX oracle intents (encoder acceleration is phase-sensitive) were
            # calibrated on such states. Scenarios still run with it on.
            "DIGI_INTC_FORCE": "0",
            "DSP_CLOCK_BASE": str(clock),
            "CF_SNAPSHOT": str(snap),
            "NOTE_EVENTS": "trig",
        }
        if a.card_image is not None:
            live_env["CARD_IMAGE"] = str(a.card_image)
        if a.base_sha256:
            live_env["BASE_SHA256"] = a.base_sha256.lower()
        with benchlock.hold("shared", "ready CF boot"):
            subprocess.run(
                [
                    str(examples / "sharc_live"), str(syx), str(raw), str(image),
                    str(snap_dir / "boot.wav"), EXTRA_AFTER_READY,
                ],
                cwd=ROOT, env=live_env, check=True,
            )  # fmt: skip
        for scratch in (raw, *snap_dir.glob("boot.wav*")):
            scratch.unlink(missing_ok=True)
    if snap_dir != out:
        for name in ("digi-audio-dn2-image.bin",):
            link = snap_dir / name
            if link.is_symlink() and not link.exists():
                link.unlink()
            if not link.exists():
                # relative, so the ready directory can move
                link.symlink_to(os.path.relpath(out / name, snap_dir))
    (snap_dir / "SOURCE.json").write_text(
        json.dumps(
            {
                "device": a.device,
                "syx_sha256": syx_sha,
                "dsp_syx_sha256": hashlib.sha256(a.dsp_syx.read_bytes()).hexdigest()
                if a.dsp_syx
                else None,
                "dsp_idle_clock": clock,
                "image": image_name,
                "intc_force": False,
                "card_image": str(a.card_image) if a.card_image else None,
                "base_sha256": a.base_sha256.lower() if a.base_sha256 else None,
            },
            indent=1,
        )
        + "\n"
    )
    print(f"ready_dir={snap_dir}")
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
