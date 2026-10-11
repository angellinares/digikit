"""Play the sequencer headless and list the notes it sends the DSP.

    python tools/seqplay.py Digitone_II_OS1.11.syx [--trigs 1,5,9,13] [--run 700M]

Boots the image from reset with SSI0 paced (`panel_drive --ssi-hz`: the audio-frame
interrupt runs, and with it the two forced interrupts that tick the sequencer, see
docs/findings/04-coldfire-dsp-link.md "The internal step clock"), enters trigs on track 1
with RECORD and the trig keys, presses PLAY and keeps the head of the frame the audio ISR
sends the DSP on every send. Prints each note-on and note-off with its time and checks the
note-ons keep to the trigs' spacing.

The frame is the 2,688-byte one built at 0x80005e60 and sent through 0x400cf7be. Bytes 34
and 36 are the note-on and note-off voice masks (one bit per voice, so a unison note sets
several); +38 and +40 repeat them. A mask can stay set for two frames, so the same non-zero
mask in consecutive frames is one event.

Digitone II 1.11 only: the addresses and key codes are that image's, and the file's hash is
checked. Needs a panel_drive with `service_forced` and a literal `--capture` length.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import pathlib
import struct
import subprocess
import sys
from dataclasses import dataclass

ROOT = pathlib.Path(__file__).resolve().parent.parent
DN2_111_SHA256 = "2af43e65e3d8390b41c9f66222620f8cce027d73ed87db00c80b440f628472e0"
EXCHANGE = "0x400cf7be"      # send to and receive from the DSP; stack argument 1 is the frame
NOTE_ON, NOTE_OFF = 34, 36
CPU_HZ = 132_000_000         # what --ssi-hz paces against
NO, RECORD, PLAY, STOP, TRIG1 = 12, 19, 20, 21, 25


@dataclass(frozen=True)
class NoteEvent:
    icount: int
    kind: str                 # "on" or "off"
    voices: tuple[int, ...]


def events(captures: list[dict]) -> list[NoteEvent]:
    """panel_drive's captures ({"icount", "hex"}, in send order) as note events."""
    out: list[NoteEvent] = []
    previous = {"on": 0, "off": 0}
    for capture in captures:
        frame = bytes.fromhex(capture["hex"])
        if len(frame) < NOTE_OFF + 2:
            continue
        masks = {"on": struct.unpack_from(">H", frame, NOTE_ON)[0],
                 "off": struct.unpack_from(">H", frame, NOTE_OFF)[0]}
        for kind, mask in masks.items():
            if mask and mask != previous[kind]:
                voices = tuple(v for v in range(16) if mask >> v & 1)
                out.append(NoteEvent(capture["icount"], kind, voices))
            previous[kind] = mask
    return out


def note_on_gaps(found: list[NoteEvent]) -> list[int]:
    """Instructions between consecutive note-ons."""
    ons = [e.icount for e in found if e.kind == "on"]
    return [b - a for a, b in zip(ons, ons[1:])]


def bpm(instructions_per_step: float) -> float:
    """The step is a sixteenth: four to the beat."""
    return 60.0 * CPU_HZ / (instructions_per_step * 4)


def step_counts(trigs: list[int], gaps: int) -> list[int]:
    """Steps between successive note-ons of a 16-step pattern, wrapping after the last."""
    between = [b - a for a, b in zip(trigs, trigs[1:])] + [16 - trigs[-1] + trigs[0]]
    return [between[i % len(between)] for i in range(gaps)]


def script(trigs: list[int], run: str) -> list[str]:
    taps = [f"tap:{TRIG1 + t - 1}" for t in trigs]
    return (["wait:1500M", f"tap:{NO}", "wait:60M", f"tap:{RECORD}"] + taps
            + [f"tap:{RECORD}", "wait:50M", f"tap:{PLAY}", f"wait:{run}", f"tap:{STOP}",
               "wait:20M"])


def panel_drive() -> pathlib.Path:
    chosen = os.environ.get("PANEL_DRIVE")
    names = ("panel_drive.exe", "panel_drive")
    candidates = ([pathlib.Path(chosen)] if chosen else []) + [
        ROOT / "out/native/target-host/release/examples" / n for n in names]
    for path in candidates:
        if path.exists():
            return path
    raise SystemExit("panel_drive not found: build it (docs/TOOLS.md) or set PANEL_DRIVE")


def main(argv: list[str] | None = None) -> int:
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    ap.add_argument("image", type=pathlib.Path)
    ap.add_argument("--trigs", default="1,5,9,13", help="steps of track 1, 1-based, ascending")
    ap.add_argument("--run", default="700M", help="instructions to play for")
    ap.add_argument("--ssi-hz", default="96000")
    ap.add_argument("--json", type=pathlib.Path, help="keep panel_drive's output here")
    a = ap.parse_args(argv)
    if hashlib.sha256(a.image.read_bytes()).hexdigest() != DN2_111_SHA256:
        print("not Digitone II 1.11 (sha-256 differs): the addresses here are that image's")
        return 3
    trigs = [int(t) for t in a.trigs.split(",")]

    cmd = [str(panel_drive()), str(a.image), "--ssi-hz", a.ssi_hz,
           "--capture", f"{EXCHANGE}:1:48", "--steps", ",".join(script(trigs, a.run))]
    done = subprocess.run(cmd, capture_output=True, text=True, timeout=1500)
    if not done.stdout.strip():
        print(done.stderr[-600:])
        return 2
    result = json.loads(done.stdout)
    if a.json:
        a.json.write_text(done.stdout)
    if result["outcome"] != "done":
        print("fault:", result["fault"])
        return 2

    found = events(result["captures"])
    ons = [e for e in found if e.kind == "on"]
    if not ons:
        print("no note-on reached the DSP")
        return 1
    print(f"{len(ons)} note-ons, {len(found) - len(ons)} note-offs")
    for e in found:
        print(f"  {(e.icount - ons[0].icount) / CPU_HZ:7.3f} s  {e.kind:3}  "
              f"voices {list(e.voices)}")

    gaps = note_on_gaps(found)
    if gaps:
        per_step = [g / s for g, s in zip(gaps, step_counts(trigs, len(gaps)))]
        mean = sum(per_step) / len(per_step)
        worst = max(abs(p - mean) / mean for p in per_step)
        print(f"step period {mean / 1e6:.2f} M instructions = {bpm(mean):.1f} BPM; "
              f"worst gap off by {worst * 100:.2f}%")
        return 0 if worst < 0.02 else 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
