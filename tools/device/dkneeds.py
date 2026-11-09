"""What a .dkm takes of the device (arena, DSP area, rows), inferred from the file's own contents.

There is one implementation, in Rust (`packages/dkneeds`): the DIGIKIT menu runs it on the card's files, patchkit and its wasm run it on the host.
This module asks the patchkit command line (`patchkit needs --json FILE...`); it computes nothing itself. The fields are `file_size` and the names
of CAPACITY's kinds (`ARENA`, `ARENA_XFER`, `DSP_AREA`, `DSP_ALIGN`, `MACHINE_ROWS`, `VOICE_ROWS`, `PARAM_ROWS`, `PAGE_ROWS`, `FX_SLOTS`), the
input of the fit rule. The fit rule is in Rust too (`modules/core/runtime/fit`, the code the DIGIKIT menu runs): `fit` asks `patchkit fit --json -`.

    needs = dkneeds.needs_of(data)          # raises NeedsError: the loader would refuse the file
    many = dkneeds.needs_many([a, b, c])    # one process for all: each a dict, or None for a file with no needs
    got = dkneeds.fit(caps, [needs])        # {"each": [short or None], "set": ..., "line": ...}; short["describe"] is the line for a person
"""

from __future__ import annotations

import json
import subprocess
import tempfile
from pathlib import Path
from typing import Any

Needs = dict[str, int]
Caps = dict[str, dict[str, int]]


class NeedsError(Exception):
    """A file that has no needs: the runtime would refuse it (not a .dkm of this format, or a table that does not fit)."""


def needs_many(files: list[bytes]) -> list[Needs | None]:
    """The needs of each file, from one `patchkit needs --json` run; None for a file that has none."""
    if not files:
        return []
    from tools.patchkit import patchkit

    with tempfile.TemporaryDirectory(prefix="dkneeds-") as d:
        paths = []
        for i, data in enumerate(files):
            p = Path(d) / f"{i}.dkm"
            p.write_bytes(data)
            paths.append(str(p))
        r = subprocess.run(
            [str(patchkit.binary()), "needs", "--json", *paths],
            capture_output=True,
            text=True,
        )
    try:
        out = json.loads(r.stdout)
    except json.JSONDecodeError as e:
        raise NeedsError(f"patchkit needs: {r.stderr.strip() or e}") from e
    return [None if "error" in out[p] else out[p] for p in paths]


def needs_of(data: bytes) -> Needs:
    """The needs of the file DATA (a whole .dkm). Raises `NeedsError` with the loader-side reason for a file that has none."""
    from tools.patchkit import patchkit

    r = subprocess.run(
        [str(patchkit.binary()), "needs", "--json", "-"],
        input=data,
        capture_output=True,
    )
    try:
        out = json.loads(r.stdout)["-"]
    except (json.JSONDecodeError, KeyError) as e:
        raise NeedsError(f"patchkit needs: {r.stderr.decode().strip() or e}") from e
    if "error" in out:
        raise NeedsError(out["error"])
    return out


def fit(caps: Caps, needs: list[Needs], empty: bool = False) -> dict[str, Any]:
    """The fit rule (`patchkit fit --json -`, modules/core/runtime/fit) for NEEDS against CAPS (kind name -> {limit, used, largest}, as
    `Uploader.capacity()` returns it). EMPTY first makes CAPS the capacity of a runtime that has loaded nothing (the next boot).

    Returns `{"each": [...], "set": ..., "line": ...}`: `each` is per need, None or the short resource when that need alone does not fit; `set` is
    None when the needs fit in turn (the sum rule: fragmentation is not modelled, so it is conservative), else the first short resource with its
    `index`; `line` is the load line (`DSP 52% CPU 1%`). A short resource is `{resource, need, free, text, brief, describe, describe_by}`: `text`
    is what the device shows, `describe` is `DSP memory 400 KiB needed, 254 KiB free`. A kind that CAPS lacks is not judged."""
    from tools.patchkit import patchkit

    req = {"caps": caps, "needs": needs, "empty": empty}
    r = subprocess.run(
        [str(patchkit.binary()), "fit", "--json", "-"],
        input=json.dumps(req).encode(),
        capture_output=True,
    )
    try:
        out = json.loads(r.stdout)
    except json.JSONDecodeError as e:
        raise NeedsError(f"patchkit fit: {r.stderr.decode().strip() or e}") from e
    return out
