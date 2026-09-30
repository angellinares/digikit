"""Record the first forced-frame Oracle CPU/RAM/MMIO window from a local ready state.

This is the accepted Python GUI's explicit *host* vector-191 stimulus, not a
Device interrupt. Inputs must be operator-checked; digest checks below are
local integrity checks, not fixture authentication. Output stays under out/.
"""

import argparse
import hashlib
import json
import os
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
sys.path.insert(0, str(ROOT))

from unicorn import m68k_const  # noqa: E402

import checkpointcpu  # noqa: E402
import checkpointprep  # noqa: E402
import framelink  # noqa: E402
from emu.dspi2 import ZeroPeer  # noqa: E402
from emu.livesharc import FrameForcer  # noqa: E402
from emu.longrun import build, spin  # noqa: E402
from emu.pit import interrupt_level  # noqa: E402
from emu.symbols import resolve  # noqa: E402


def _sha256(path: Path) -> str:
    with path.open("rb") as stream:
        return hashlib.file_digest(stream, "sha256").hexdigest()


class _FirstForce(Exception):
    """Stop the bounded Oracle spin at its first GUI frame-force boundary."""


def capture(
    snapshot: Path,
    source: Path,
    sections: Path,
    card: Path,
    state: Path,
    output: Path,
    profile_output: Path,
    count: int = 32,
) -> None:
    if not 32 <= count <= 60_000:
        raise ValueError("frame IRQ window must be 32..60000 instructions")
    out = (ROOT / "out").resolve()
    if any(
        not path.parent.resolve().is_relative_to(out)
        for path in (output, profile_output)
    ):
        raise ValueError("Oracle artifacts must stay in ignored out/")
    if _sha256(source) != (sections / ".source-sha256").read_text().strip():
        raise ValueError("source file differs from sections source marker")
    saved = state.read_bytes()
    if saved[:8] != b"MSTATE\0\x02":
        raise ValueError("ready state is not compact MSTATE v2")
    size = int.from_bytes(saved[8:12], "little")
    if size > 1 << 20:
        raise ValueError("MSTATE header exceeds bound")
    header = json.loads(saved[12 : 12 + size])
    main_sha, profile = framelink.profile_for(sections / "section_3_MAIN_OS.bin")
    if header["manifest"]["main_sha256"] != main_sha:
        raise ValueError("ready state and loaded MAIN OS differ")

    previous = os.environ.get("DT2_SECTIONS")
    os.environ["DT2_SECTIONS"] = str(sections.resolve())
    try:
        machine, ev, _st, pc, _inq, _at = build(
            str(snapshot),
            syx=str(source),
            card_image=str(card),
            unblock=True,
            softfloat=True,
            bitmap=True,
            dsp=True,
            deferred_components=("timers",),
            dspi2_peer=ZeroPeer(),
        )
        try:
            timers = ev["restore_checkpoint_timers"]()
            if timers is None:
                raise ValueError("ready state lacks restored timers")
            if ev["checkpoint_manifest"]["main_sha256"] != main_sha:
                raise ValueError(
                    "restored snapshot MAIN OS differs from checked source"
                )
            if pc != header["regs"]["pc"]:
                raise ValueError("ready snapshot and MSTATE PC differ")
            forcer = FrameForcer(machine, timers, 200_000)
            origin = timers.now

            def on_chunk(pc: int, done: int) -> None:
                forcer.on_chunk(pc, done)
                if forcer.forced:
                    raise _FirstForce

            try:
                spin(machine, pc, 400_000, pits=timers, fast=True, on_chunk=on_chunk)
            except _FirstForce:
                pass
            else:
                raise ValueError(
                    "first forced frame was not reached within 400k instructions"
                )
            force_at = int(timers.now - origin)
            if not 0 < force_at < 400_000:
                raise ValueError("invalid first frame-force clock")
            level = interrupt_level(machine, profile["vector"], respect_mask=False)
            start = machine.uc.reg_read(m68k_const.UC_M68K_REG_PC)
            observed = checkpointcpu.capture_window(
                machine.uc,
                start,
                count,
                1,
                lambda uc: checkpointprep._regs(uc, m68k_const),
            )
        finally:
            machine.close()
    finally:
        if previous is None:
            os.environ.pop("DT2_SECTIONS", None)
        else:
            os.environ["DT2_SECTIONS"] = previous
    reference = {
        "format_version": 1,
        "image_sha256": main_sha,
        "vector": profile["vector"],
        "level": level,
        "force_at": force_at,
        **observed,
    }
    temporary = output.with_suffix(".tmp")
    temporary.write_text(json.dumps(reference, sort_keys=True) + "\n")
    temporary.replace(output)
    image = (sections / "section_3_MAIN_OS.bin").read_bytes()
    profile_data = {
        key: profile[key]
        for key in ("name", "vector", "handler", "driver", "counter", "gate")
    }
    profile_data.update(
        image_sha256=main_sha,
        uart8_ring_ptr=resolve(image).uart8_ring_ptr,
    )
    temporary = profile_output.with_suffix(".tmp")
    temporary.write_text(json.dumps(profile_data, sort_keys=True) + "\n")
    temporary.replace(profile_output)
    print(
        f"{output}: first force at {force_at}; {count + 1} CPU samples,"
        f" {len(observed['effects'])} guest effects"
    )


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    for name in (
        "snapshot",
        "source",
        "sections",
        "card",
        "state",
        "output",
        "profile-output",
    ):
        parser.add_argument(f"--{name}", type=Path, required=True)
    parser.add_argument("--count", type=int, default=32)
    args = parser.parse_args()
    capture(
        args.snapshot,
        args.source,
        args.sections,
        args.card,
        args.state,
        args.output,
        args.profile_output,
        args.count,
    )


if __name__ == "__main__":
    main()
