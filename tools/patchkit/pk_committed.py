"""The records a module committed on the emulated device, as `module:check` and conformance use them (docs/plans/sdk.md 6.4).

A module declares its machines, parameters and pages with builder calls in `init`, so nothing in the repository lists them. The
runtime's `MODULE_COMMIT` event names the record block it wrote; `tools/devtools/commit_records.ts` boots a device, reads that block from
emulator memory and decodes it with `patchkit records --json` (the one decoder of the record format). This file runs that tool and turns its
output into what the callers need:

  * `committed(mid, device)`: the decoded records of module MID on DEVICE;
  * `uses(per_device)`: the `[uses.<device>]` counts, one table per device;
  * `gallery(mid, machine_ids, per_device)`: `build/gen/gallery.json` in the schema `patchkit gen` writes for a major-2 module;

Conformance (`pk_conform._params`) reads that gallery.json, so it gets its cases (slot, min, max, default) from the committed rows too.

The rows carry names, not the ids of the old declarations, so a parameter's id here is its name in lower case (`Detune` is `detune`), with
the slot appended when two share a name.
"""

from __future__ import annotations

import json
import re
import subprocess
from pathlib import Path
from typing import Any

ROOT = Path(__file__).resolve().parents[2]
CLI = "tools/devtools/commit_records.ts"


def committed(
    mid: str,
    device: str,
    dkm: Path,
    patchkit: str,
    binary: str | None = None,
    debug_dkm: Path | None = None,
    services: list[Path] | None = None,
) -> dict[str, Any]:
    """The decoded records of module MID (packed as DKM) on DEVICE: `{module, commit, records}` of `commit_records.ts`.
    DEBUG_DKM is the packed debug module the device boots with; without it the tool reads `dist/modules/<device>/debug.dkm`.
    SERVICES are the bound `.dks` files (`patchkit bind`) the module needs: they go on the card as `services/<name>.dks`."""
    cmd = [
        "node", CLI, "--device", device, "--module", mid, "--dkm", str(dkm), "--patchkit", patchkit,
        *(["--binary", binary] if binary else []),
        *(["--debug-dkm", str(debug_dkm)] if debug_dkm else []),
        *[a for s in services or [] for a in ("--service", str(s))],
    ]  # fmt: skip
    r = subprocess.run(cmd, cwd=ROOT, capture_output=True, text=True)
    if r.returncode:
        raise RuntimeError(f"{CLI} {mid}@{device} failed:\n{r.stderr.strip()}")
    return json.loads(r.stdout)


def uses(per_device: dict[str, dict[str, Any]]) -> dict[str, dict[str, int]]:
    """The `[uses.<device>]` counts: what each device committed (the runtime builds different rows per device), and `record_bytes`, the size of the
    record block it keeps in the arena (the `records_size` of the `MODULE_COMMIT` event)."""
    keys = ("machine_rows", "param_rows", "page_rows")
    return {
        dev: {
            **{k: int(d["records"]["counts"][k]) for k in keys},
            "record_bytes": int(d["commit"]["size"]),
        }
        for dev, d in per_device.items()
    }


def _ids(params: list[dict[str, Any]]) -> list[str]:
    base = [
        re.sub(r"[^a-z0-9]+", "_", p["name"].lower()).strip("_") or "param"
        for p in params
    ]
    return [
        b if base.count(b) == 1 else f"{b}_{p['slot']}"
        for b, p in zip(base, params, strict=True)
    ]


PARAM_CODE = 0x80000000  # a knob code with this bit set names parameter row `code & 0x7fffffff`; any other nonzero code is a stock parameter


def _knobs(
    codes: list[int], ids: list[str], base: int, count: int, trim: bool
) -> list[dict[str, Any] | None]:
    out: list[dict[str, Any] | None] = []
    for j in range(count):
        code = codes[j] if j < len(codes) else 0
        if code == 0:
            out.append(None)
        elif code & PARAM_CODE:
            out.append({"slot": base + j, "param": ids[code & ~PARAM_CODE]})
        else:
            out.append({"slot": base + j, "stock": code})
    while trim and out and out[-1] is None:
        out.pop()
    return out


def gallery(
    module: str,
    machine_ids: list[str],
    per_device: dict[str, dict[str, Any]],
    models: dict[str, dict[str, int]],
    fx_ids: list[str] | None = None,
) -> dict[str, Any]:
    """`build/gen/gallery.json` of a module from its committed rows. MACHINE_IDS are the `[[machine]]` ids of module.toml, in the order of the
    machine rows; MODELS gives per device the knob (`slot_base`, `count` and `to`, devices/*.toml `[modules.knobs]`). FX_IDS are the `[[fx]]`
    ids in the order of the effect records; the effects of the first device (a record is the same on both) are listed under `effects`.
    A parameter the machine latches at note-on gets `latched_word` (its word in the machine), from the `latched` masks `commit_records.ts` reads."""
    machines: list[dict[str, Any]] = []
    params_out: dict[str, list[dict[str, Any]]] = {}
    for dev, data in per_device.items():
        rec = data["records"]
        ids = _ids(rec["params"])
        params_out[dev] = [
            {
                "id": i,
                "name": p["name"],
                "short": p["short"],
                "min": p["min"],
                "max": p["max"],
                "default": p["default"],
                "slot": p["slot"],
                "template": p["template"],
            }
            for i, p in zip(ids, rec["params"], strict=True)
        ]
        # a latched parameter's word is its place among its machine's parameters (the order of the file); the machine's latch mask has a bit per word
        for n, m in enumerate(rec["machines"]):
            page = next((q for q in rec["pages"] if q["type"] == m["type"]), None)
            codes = m["knobs"] if any(m["knobs"]) or not page else page["knobs"]
            owned = sorted(c & ~PARAM_CODE for c in codes if c & PARAM_CODE)
            mask = (data.get("latched") or [])[n : n + 1] or [0]
            for word, index in enumerate(owned):
                if mask[0] >> word & 1:
                    params_out[dev][index]["latched_word"] = word
        base, count = models[dev]["slot_base"], models[dev]["count"]
        trim = (
            models[dev]["to"] == "page"
        )  # a page device lists knobs up to the last used one
        for n, m in enumerate(rec["machines"]):
            mid = machine_ids[n] if n < len(machine_ids) else f"machine{n}"
            entry = next((e for e in machines if e["id"] == mid), None)
            if entry is None:
                entry = {
                    "id": mid,
                    "name": m["name"],
                    "short": m["short"],
                    "devices": {},
                }
                machines.append(entry)
            page = next((p for p in rec["pages"] if p["type"] == m["type"]), None)
            entry["devices"][dev] = {
                "name": m["name"],
                "short": m["short"],
                "type": None,
                "clone_of": m["clone_of"],
                "position": None,
                "own_dsp": bool(m["own_dsp"]),
                "level": m["level"],
                "knobs": _knobs(
                    m["knobs"] if any(m["knobs"]) or not page else page["knobs"],
                    ids,
                    base,
                    count,
                    trim,
                ),
                "page": {"name": page["name"], "title": page["title"]}
                if page
                else None,
            }
    out: dict[str, Any] = {"module": module, "machines": machines, "params": params_out}
    first = next(iter(per_device.values()), None)
    if first and first["records"]["effects"]:
        out["effects"] = [
            {
                "id": (fx_ids or [])[n] if n < len(fx_ids or []) else f"fx{n}",
                "tab": e["tab"],
                "title": e["title"],
                "mix": e["mix"],
                "knobs": [
                    {
                        k: q[k]
                        for k in ("name", "short", "min", "max", "default", "display")
                    }
                    for q in e["knobs"]
                ],
            }
            for n, e in enumerate(first["records"]["effects"])
        ]
    return out
