"""Play the sequencer headless and list the notes it sends the DSP.

    python tools/seqplay.py Digitone_II_OS1.11.syx [--trigs 1,5,9,13] [--track 2:3,11]
                            [--run 700M] [--against STOCK.syx]

Boots the image from reset with SSI0 paced (`panel_drive --ssi-hz`: the audio-frame
interrupt runs, and with it the two forced interrupts that tick the sequencer, see
docs/findings/04-coldfire-dsp-link.md "The internal step clock"), enters trigs with RECORD and
the trig keys, presses PLAY and keeps the head of the frame the audio ISR sends the DSP on
every send. Prints each note-on (voices and note numbers) and note-off with its time and
checks the note-ons keep to the spacing of the trigs' steps.

`--trigs` is track 1's steps; each `--track N:STEPS` adds a track (TRK held, then trig key N).
`--against IMAGE` plays the same pattern on IMAGE too and compares the two event by event:
kind, voices and notes exactly, the time within `--tolerance` instructions (the capture lands
wherever the ColdFire was when a frame went out, so different code shifts it by a few). That
is the check that a modified image leaves the sequencer alone: IMAGE is the modified one,
`--against` the stock 1.11 reference.

The frame is the 2,688-byte one built at 0x80005e60 and sent through 0x400cf7be. Bytes 34
and 36 are the note-on and note-off voice masks (one bit per voice, so a unison note sets
several); +38 and +40 repeat them. A mask can stay set for two frames, so the same non-zero
mask in consecutive frames is one event. The note is the u16 at byte 2 + 2 * voice,
`note << 8 | fine`, written in the note-on's frame and kept after it, so it is read there.
Holding a trig key plays a preview note, which is an extra note-on: enter trigs with RECORD
and do not hold keys during the run.

Digitone II 1.11 only: the addresses and key codes are that image's. With no `--against` the
file's hash is checked; with it, the reference's is, and IMAGE may be a modified 1.11.
Needs a panel_drive with `service_forced` and a literal `--capture` length.
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
NOTE_WORDS = 2               # byte offset of voice 0's note word
CPU_HZ = 132_000_000         # what --ssi-hz paces against
NO, RECORD, PLAY, STOP, TRK, TRIG1 = 12, 19, 20, 21, 16, 25


@dataclass(frozen=True)
class NoteEvent:
    icount: int
    kind: str                 # "on" or "off"
    voices: tuple[int, ...]
    notes: tuple[float, ...] = ()    # at a note-on: each voice's note, fine as a fraction


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
                notes: tuple[float, ...] = ()
                if kind == "on" and len(frame) >= NOTE_WORDS + 2 * (voices[-1] + 1):
                    notes = tuple(
                        struct.unpack_from(">H", frame, NOTE_WORDS + 2 * v)[0] / 256
                        for v in voices)
                out.append(NoteEvent(capture["icount"], kind, voices, notes))
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


def script(tracks: dict[int, list[int]], run: str) -> list[str]:
    entered: list[str] = []
    for track, trigs in sorted(tracks.items()):
        if track != 1 or len(tracks) > 1:
            entered += [f"press:{TRK}", "wait:10M", f"tap:{TRIG1 + track - 1}",
                        f"release:{TRK}", "wait:30M"]
        entered += [f"tap:{RECORD}"] + [f"tap:{TRIG1 + t - 1}" for t in trigs]
        entered += [f"tap:{RECORD}", "wait:30M"]
    return (["wait:1500M", f"tap:{NO}", "wait:60M"] + entered
            + ["wait:50M", f"tap:{PLAY}", f"wait:{run}", f"tap:{STOP}", "wait:20M"])


def signature(found: list[NoteEvent]) -> list[tuple]:
    """What an image did, with times from its first note-on: comparable between images."""
    ons = [e.icount for e in found if e.kind == "on"]
    t0 = ons[0] if ons else 0
    return [(e.icount - t0, e.kind, e.voices, e.notes) for e in found if e.icount >= t0]


def first_difference(a: list[tuple], b: list[tuple], tolerance: int = 0) -> str | None:
    """None when the two agree; else the first event that does not."""
    if len(a) != len(b):
        return f"{len(b)} events against {len(a)}"
    for i, (x, y) in enumerate(zip(a, b)):
        if x[1:] != y[1:] or abs(x[0] - y[0]) > tolerance:
            return f"event {i}: {y} against {x}"
    return None


def panel_drive() -> pathlib.Path:
    chosen = os.environ.get("PANEL_DRIVE")
    names = ("panel_drive.exe", "panel_drive")
    candidates = ([pathlib.Path(chosen)] if chosen else []) + [
        ROOT / "out/native/target-host/release/examples" / n for n in names]
    for path in candidates:
        if path.exists():
            return path
    raise SystemExit("panel_drive not found: build it (docs/TOOLS.md) or set PANEL_DRIVE")


def play(image: pathlib.Path, tracks: dict[int, list[int]], run: str, ssi_hz: str,
         keep: pathlib.Path | None = None) -> list[NoteEvent]:
    """Boot IMAGE and play the trigs; raises RuntimeError on a fault."""
    cmd = [str(panel_drive()), str(image), "--ssi-hz", ssi_hz,
           "--capture", f"{EXCHANGE}:1:48", "--steps", ",".join(script(tracks, run))]
    done = subprocess.run(cmd, capture_output=True, text=True, timeout=1500)
    if not done.stdout.strip():
        raise RuntimeError(done.stderr[-300:])
    result = json.loads(done.stdout)
    if keep:
        keep.write_text(done.stdout)
    if result["outcome"] != "done":
        raise RuntimeError(f"fault: {result['fault']}")
    return events(result["captures"])


def parse_tracks(trigs: str, extra: list[str]) -> dict[int, list[int]]:
    tracks = {1: [int(t) for t in trigs.split(",")]}
    for spec in extra:
        number, steps = spec.split(":")
        tracks[int(number)] = [int(t) for t in steps.split(",")]
    return tracks


def is_dn2_111(path: pathlib.Path) -> bool:
    if hashlib.sha256(path.read_bytes()).hexdigest() == DN2_111_SHA256:
        return True
    print(f"{path.name}: not Digitone II 1.11 (sha-256 differs); the addresses here are "
          "that image's")
    return False


def main(argv: list[str] | None = None) -> int:
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    ap.add_argument("image", type=pathlib.Path)
    ap.add_argument("--trigs", default="1,5,9,13", help="steps of track 1, 1-based, ascending")
    ap.add_argument("--track", action="append", default=[], metavar="N:STEPS",
                    help="another track's steps, e.g. 2:3,11 (repeatable)")
    ap.add_argument("--run", default="700M", help="instructions to play for")
    ap.add_argument("--ssi-hz", default="96000")
    ap.add_argument("--against", type=pathlib.Path, help="an image to compare the result with")
    ap.add_argument("--tolerance", type=int, default=1000,
                    help="instructions a time may differ by under --against (default 1000)")
    ap.add_argument("--json", type=pathlib.Path, help="keep panel_drive's output here")
    a = ap.parse_args(argv)
    if not is_dn2_111(a.against or a.image):
        return 3
    tracks = parse_tracks(a.trigs, a.track)
    try:
        found = play(a.image, tracks, a.run, a.ssi_hz, a.json)
    except RuntimeError as error:
        print(error)
        return 2

    ons = [e for e in found if e.kind == "on"]
    if not ons:
        print("no note-on reached the DSP")
        return 1
    print(f"{len(ons)} note-ons, {len(found) - len(ons)} note-offs")
    for e in found:
        notes = f"  notes {list(e.notes)}" if e.notes else ""
        print(f"  {(e.icount - ons[0].icount) / CPU_HZ:7.3f} s  {e.kind:3}  "
              f"voices {list(e.voices)}{notes}")

    status = 0
    gaps = note_on_gaps(found)
    if gaps:
        # the trigs repeat every 16 steps; a step two tracks share is one event
        trigs = sorted({t for steps in tracks.values() for t in steps})
        per_step = [g / s for g, s in zip(gaps, step_counts(trigs, len(gaps)))]
        mean = sum(per_step) / len(per_step)
        worst = max(abs(p - mean) / mean for p in per_step)
        print(f"step period {mean / 1e6:.2f} M instructions = {bpm(mean):.1f} BPM; "
              f"worst gap off by {worst * 100:.2f}%")
        status = 0 if worst < 0.02 else 1
    if a.against:
        try:
            other = play(a.against, tracks, a.run, a.ssi_hz)
        except RuntimeError as error:
            print(f"{a.against.name}: {error}")
            return 2
        why = first_difference(signature(other), signature(found), a.tolerance)
        print(f"against {a.against.name}: {'same' if why is None else 'DIFFERS, ' + why}")
        status = status or (0 if why is None else 1)
    return status


if __name__ == "__main__":
    sys.exit(main())
