#!/usr/bin/env python3
"""Report first ordered ColdFire wire mismatch with Oracle/native host clocks.

Source and native DTFR files must be operator-checked local artifacts; matching
hashes verify integrity, not provenance. Neither matched replay nor clocks
establish autonomous Device policy or SHARC audio parity.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import re
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT))

from tools import wiretrace  # noqa: E402


def _sha(path: Path) -> str:
    with path.open("rb") as src:
        return hashlib.file_digest(src, "sha256").hexdigest()


def first_byte(
    expected: list[bytes], actual: list[bytes]
) -> tuple[int, int | None] | None:
    for index in range(min(len(expected), len(actual))):
        if expected[index] != actual[index]:
            offset = next(
                (
                    i
                    for i, (a, b) in enumerate(
                        zip(expected[index], actual[index], strict=False)
                    )
                    if a != b
                ),
                min(len(expected[index]), len(actual[index])),
            )
            return index, offset
    if len(expected) != len(actual):
        return min(len(expected), len(actual)), None
    return None


def old_probe_events(path: Path) -> list[dict]:
    """Partial historical clock report, NOT a complete replay event stream."""
    events = []
    for line in path.read_text().splitlines():
        force = re.search(r"NATIVE_FORCE (\d+) at (\d+)", line)
        feed = re.search(
            r"panel RX (\d+) at (\d+): pre-PC (0x[\da-f]+), SR (0x[\da-f]+)", line
        )
        if force:
            events.append(
                {"kind": "force", "forced": int(force[1]), "clock": int(force[2])}
            )
        if feed:
            events.append(
                {
                    "kind": "feed",
                    "index": int(feed[1]),
                    "clock": int(feed[2]),
                    "pre_pc": int(feed[3], 16),
                    "pre_sr": int(feed[4], 16),
                }
            )
    return events


def compare(
    source: Path,
    native: Path,
    source_events: Path,
    native_events: Path | None = None,
    native_log: Path | None = None,
) -> tuple[str, bool]:
    if (native_events is None) == (native_log is None):
        raise ValueError("provide exactly one native event JSON or historical text log")
    reference = json.loads(source_events.read_text())
    if reference["format_version"] != 1 or reference["dtfr_sha256"] != _sha(source):
        raise ValueError("source events do not match source DTFR")
    expected, actual = wiretrace.read(source), wiretrace.read(native)
    if len(expected) != reference["frames"]:
        raise ValueError("source frame count disagrees with source events")
    if native_events is not None:
        capture = json.loads(native_events.read_text())
        if capture["format_version"] != 1 or capture["source_dtfr_sha256"] != _sha(
            source
        ):
            raise ValueError("native event log was not replayed from this source DTFR")
        events = capture["events"]
        policy = capture["policy"]
        native_steps: int | str = capture["actual_native_instructions"]
    else:
        assert native_log is not None
        events = old_probe_events(native_log)
        policy = "partial historical native scheduling (not event replay)"
        native_steps = "historical log only"
    differing = first_byte(expected, actual)
    lines = [
        f"Oracle stepping: {reference.get('stepping', 'unknown')} (fast = approximate clock)",
        f"Native policy: {policy}",
        f"Source credited instructions: {reference['actual_credited_instructions']}; "
        f"native actual steps: {native_steps}",
    ]
    source_host = [e for e in reference["events"] if e["kind"] in ("force", "feed")]
    native_host = [e for e in events if e["kind"] in ("force", "feed")]
    # The historical text probe logged only some force ordinals; comparing
    # its positional event list to the complete source log is meaningless.
    first_pc = (
        next(
            (
                (a, b)
                for a, b in zip(source_host, native_host, strict=False)
                if a["kind"] != b["kind"] or a["pre_pc"] != b["pre_pc"]
            ),
            None,
        )
        if native_events is not None
        else None
    )
    if first_pc is not None:
        src, dst = first_pc
        lines.append(
            f"first host pre-PC difference: {src['kind']} at source clock {src['clock']} "
            f"source={src['pre_pc']:#x} native={dst.get('pre_pc', 0):#x}; "
            "wire agreement does not prove CPU/timer parity"
        )
    if differing is None:
        lines.append(
            f"matched {len(expected)} ordered frames (Oracle wire parity only)"
        )
        index = max(0, len(expected) - 1)
    else:
        index, offset = differing
        lines.append(
            wiretrace.first_mismatch(expected, actual) or f"missing frame {index}"
        )
        lines.append(f"first divergence frame {index}, byte {offset}")
    for kind in ("feed", "force"):
        source_kind = [e for e in reference["events"] if e["kind"] == kind]
        native_kind = [e for e in events if e["kind"] == kind]
        if kind == "feed":
            for i, event in enumerate(source_kind):
                other = next((e for e in native_kind if e["index"] == i), None)
                lines.append(
                    f"feed {i}: request={event['request_clock']}, source delivered={event['clock']}, "
                    f"native delivered={other['clock'] if other else 'not observed'}, "
                    f"source pre-PC={event['pre_pc']:#x}, native pre-PC="
                    f"{other.get('pre_pc') if other else 'not observed'}"
                )
        else:
            for e in source_kind:
                n = e["forced"]
                high = index + (1 if differing is None else 3)
                if not max(1, index - 1) <= n <= high:
                    continue
                other = next(
                    (x for x in native_kind if x.get("forced", None) == n), None
                )
                # Structured replay logs carry sequential force events without
                # `forced`; the ordinal is their source correspondence.
                if other is None and native_events is not None:
                    other = native_kind[n - 1] if n <= len(native_kind) else None
                lines.append(
                    f"force {n}: source={e['clock']}, native="
                    f"{other['clock'] if other else 'not observed'}, "
                    f"delta={other['clock'] - e['clock'] if other else 'unknown'}"
                )
    for i in range(max(0, index - 2), min(len(expected), index + 3)):
        src = expected[i]
        dst = actual[i] if i < len(actual) else b""
        lines.append(
            f"frame {i}: source sha256={hashlib.sha256(src).hexdigest()}, "
            f"native sha256={hashlib.sha256(dst).hexdigest() if dst else 'missing'}, "
            f"release[0x24:0x2b]={src[0x24:0x2B].hex()}/"
            f"{dst[0x24:0x2B].hex() if dst else 'missing'}"
        )
    return "\n".join(lines), differing is None


def main() -> int:
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument("source", type=Path)
    p.add_argument("native", type=Path)
    p.add_argument("--source-events", type=Path, required=True)
    g = p.add_mutually_exclusive_group(required=True)
    g.add_argument("--native-events", type=Path)
    g.add_argument("--native-log", type=Path)
    args = p.parse_args()
    text, matched = compare(
        args.source,
        args.native,
        args.source_events,
        args.native_events,
        args.native_log,
    )
    print(text)
    return 0 if matched else 1


if __name__ == "__main__":
    raise SystemExit(main())
