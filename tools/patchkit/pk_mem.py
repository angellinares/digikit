"""Patch memory readout: per region of a firmware's port, what a set of modules uses and what stays free.

    uv run python -m tools.patchkit.pk_mem [MODULE ...] --device dt2|dn2 [--firmware VER] [--json]

The fit is the patchkit placement's own: the modules are applied to the stock `.syx` (`patchkit apply`, first-fit
allocation per region) and refused when they do not fit (exit 1, the placement's message). The numbers come from the
plan it writes (`regions`: bytes used and capacity per region, `writes`: every placed object, table and hook tail), so
no placement is reimplemented here. Modules pulled in by `requires` are listed too. Without modules the table shows
the free regions of the port (devices/ports/<device>-<firmware>.toml) and needs no firmware.

Used bytes of a region are the placement's: the listed modules' bytes plus alignment padding and zero-initialised
data (`other`). Nothing here is written under out/ except the plan of the applied image (a scratch directory).
"""

from __future__ import annotations

import argparse
import json
import sys
import tempfile
import tomllib
from pathlib import Path
from typing import Any

from tools.firmware import config

HERE = Path(__file__).resolve().parent
ROOT = HERE.parent.parent

DEFAULT_FIRMWARE = {"dt2": "1.17", "dn2": "1.12"}


def port_regions(device: str, firmware: str) -> dict[str, dict[str, Any]]:
    """name -> {cpu, start, end, size} of the port's regions; size is net of the excluded ranges."""
    path = ROOT / "devices" / "ports" / f"{device}-{firmware}.toml"
    return regions_of(tomllib.loads(path.read_text()))


def regions_of(port: dict[str, Any]) -> dict[str, dict[str, Any]]:
    out: dict[str, dict[str, Any]] = {}
    for r in port.get("region", []):
        excluded = sum(
            max(0, min(e, r["end"]) - max(s, r["start"]))
            for s, e in r.get("excludes", [])
        )
        out[r["name"]] = {
            "cpu": r["cpu"],
            "start": r["start"],
            "end": r["end"],
            "size": r["end"] - r["start"] - excluded,
        }
    return out


def usage(
    regions: dict[str, dict[str, Any]], plan: dict[str, Any] | None
) -> dict[str, Any]:
    """The readout. REGIONS from `regions_of`; PLAN is the placement's plan (None: nothing placed).

    A written object belongs to the region its address lies in; the placement's own used-bytes figure is the total, so
    what no module line accounts for (alignment, bss) is `other`."""
    placed = plan["regions"] if plan else {}
    rows = []
    for name, r in regions.items():
        used, cap = placed.get(name, [0, r["size"]])
        by_module: dict[str, int] = {}
        for w in (plan or {}).get("writes", []):
            if w["kind"] == "place" and r["start"] <= w["addr"] < r["end"]:
                by_module[w["owner"]] = by_module.get(w["owner"], 0) + w["len"]
        listed = sum(by_module.values())
        rows.append(
            {
                "region": name,
                "cpu": r["cpu"],
                "size": cap,
                "used": used,
                "free": cap - used,
                "pct": round(100.0 * used / cap, 2) if cap else 0.0,
                "modules": dict(sorted(by_module.items())),
                "other": used - listed,
            }
        )
    return {"modules": (plan or {}).get("modules", []), "regions": rows}


def table(report: dict[str, Any]) -> str:
    lines = []
    if report["modules"]:
        lines.append("modules: " + ", ".join(report["modules"]))
    head = f"{'region':<16}{'size':>10}{'used':>10}{'free':>10}{'used %':>8}"
    lines.append(head)
    for r in report["regions"]:
        lines.append(
            f"{r['region']:<16}{r['size']:>10}{r['used']:>10}{r['free']:>10}{r['pct']:>8.2f}"
        )
        for m, n in r["modules"].items():
            lines.append(f"    {m:<28}{n:>10}")
        if r["used"] and r["other"]:
            lines.append(f"    {'(alignment, bss)':<28}{r['other']:>10}")
    return "\n".join(lines)


def runtime_bytes(device: str, root: Path = ROOT) -> dict[str, int] | None:
    """Bytes of core/runtime for DEVICE from its built objects: code and data (bss is not counted) of the ColdFire objects
    (`cf`) and of the DSP objects (`dsp`). The objects are the ones its module.toml lists for the device. None: not built."""
    base = root / "modules" / "core" / "runtime"
    man = tomllib.loads((base / "module.toml").read_text())
    out = {"cf": 0, "dsp": 0}
    for o in man.get("object", []):
        if o.get("devices") and device not in o["devices"]:
            continue
        p = base / o["path"]
        if not p.is_file():
            return None
        dko = json.loads(p.read_text())
        n = sum(s["size"] for s in dko["sections"] if s["kind"] != "bss")
        out["dsp" if dko["arch"] == "sharc" else "cf"] += n
    return out


def runtime_line(rt: dict[str, int]) -> str:
    return f"runtime core/runtime: {rt['cf']} bytes ColdFire code+data, {rt['dsp']} bytes DSP"


def apply_plan(device: str, firmware: str, modules: list[str]) -> dict[str, Any]:
    """Apply MODULES to the stock image and return the plan; SystemExit(1) with the placement's message when refused."""
    from tools.patchkit import patchkit
    from tools.patchkit import pk_harness as h

    syx = config.stock(h.FIRMWARE[(device, firmware)]["syx"])
    if not syx.is_file():
        raise SystemExit(f"{syx} is missing")
    with tempfile.TemporaryDirectory(prefix="pk-mem-") as tmp:
        out, plan = Path(tmp) / "out.syx", Path(tmp) / "plan.json"
        r = patchkit.run(
            [
                "apply",
                "--firmware",
                str(syx),
                "--modules",
                ",".join(modules),
                "-o",
                str(out),
                "--plan",
                str(plan),
            ],
            capture=True,
        )
        if r.returncode:
            msg = (r.stderr or r.stdout).strip().splitlines()
            raise SystemExit(f"does not fit or is refused: {msg[-1] if msg else '?'}")
        return json.loads(plan.read_text())


def main(argv: list[str]) -> int:
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    ap.add_argument("modules", nargs="*", help="ids under modules/, e.g. core/machines")
    ap.add_argument("--device", default="dt2", choices=("dt2", "dn2"))
    ap.add_argument("--firmware", help="default: 1.17 (dt2), 1.12 (dn2)")
    ap.add_argument("--json", action="store_true", help="JSON only")
    a = ap.parse_args(argv)
    fw = a.firmware or DEFAULT_FIRMWARE[a.device]
    regions = port_regions(a.device, fw)
    plan = apply_plan(a.device, fw, a.modules) if a.modules else None
    report = {"device": a.device, "firmware": fw, **usage(regions, plan)}
    rt = (
        runtime_bytes(a.device)
        if any(m.startswith("core/runtime") for m in report["modules"])
        else None
    )
    if rt is not None:
        report["runtime_bytes"] = rt
    if a.json:
        print(json.dumps(report, indent=1))
    else:
        print(f"{a.device}-{fw}\n{table(report)}")
        if rt is not None:
            print(runtime_line(rt))
    return 0


if __name__ == "__main__":
    raise SystemExit(main(sys.argv[1:]))
