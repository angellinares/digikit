#!/usr/bin/env python3
"""Arena and card-store safety runs (finding 25, plan docs/plans/digikit-runtime.md phase 2).

Runs a MAIN OS image in the native emulator (`regions_probe`) with a panel/MIDI script and proves, for that run:

  * the firmware's CPU stores and DMA never touch the module RAM `cf.expansion` (write-watch, from the first CACR
    write; docs/INSTRUMENTATION.md) and the canary pattern filled into it survives;
  * the card sectors the firmware wrote (the copy-on-write overlay) include none of the module store
    (the device's `[card.store]` area, docs/design/digikit-store.md).

    uv run python -m tools.coldfire.arena_safety run IMAGE SCRIPT [--card IMG] [--out DIR] [--watch-limit N]
    uv run python -m tools.coldfire.arena_safety list
    uv run python -m tools.coldfire.arena_safety card-check OVERLAY.bin [--lo SECTOR --hi SECTOR]

`SCRIPT` is a name from `tools.coldfire.panel_script.SCRIPTS` (or `tools.coldfire.regions.scenario_args`). The overlay
format is the control socket's `card.overlay` file: per touched sector `u64 le sector, [u8; 64] mask, [u8; 512] data`.
"""

from __future__ import annotations

import argparse
import contextlib
import json
import os
import pathlib
import struct
import subprocess
import sys
import tomllib

from tools import python_env
from tools.coldfire import panel_script, regions
from tools.device.devrpc import parse_reply
from tools.firmware import config
from tools.patchkit import pk_device

ROOT = pathlib.Path(__file__).resolve().parents[2]
SECTOR_RECORD = 8 + 64 + 512
# The existing firmware kit-library sector ranges (hi exclusive), shared by fixed FX policies.
KIT_AREAS = {"dt2": ((0x16000, 0x17000),), "dn2": ((0x2000, 0x2100),)}


def card_store(device: str) -> tuple[int, int]:
    """The module store area of a device (docs/design/digikit-store.md; devices/*.toml [card.store]): sectors, hi exclusive."""
    s = pk_device.store_of(device)
    return (s.first, s.end)


def expansion(image: str) -> tuple[int, int]:
    """The `cf.expansion` region of an image's port file (the file for IMAGE in devices/ports): (lo, hi exclusive)."""
    port = tomllib.loads((ROOT / "devices" / "ports" / (image + ".toml")).read_text())
    for r in port["region"]:
        if r["name"] == "cf.expansion":
            return r["start"], r["end"]
    raise KeyError("no cf.expansion region in %s" % image)


def plan_allow_pc(plan: pathlib.Path | str) -> str:
    """`allow_pc` for a patched image: the range the patch placed in cf.expansion (its used size from the patch plan: the
    modules' code, data and bss). Only the modules run from there, so their own stores are not reported and nothing
    else is (the PC range is a filter on who stores, not on where)."""
    d = json.loads(pathlib.Path(plan).read_text())
    first, _ = expansion(d["base"]["device"] + "-" + d["base"]["version"])
    return "0x%x-0x%x" % (first, first + d["regions"]["cf.expansion"][0])


def parse_overlay(data: bytes) -> list[int]:
    """The sector numbers in a `card.overlay` file, in file order."""
    if len(data) % SECTOR_RECORD:
        raise ValueError(
            "overlay size %d is not a multiple of %d" % (len(data), SECTOR_RECORD)
        )
    return [
        struct.unpack_from("<Q", data, k)[0] for k in range(0, len(data), SECTOR_RECORD)
    ]


def card_store_hits(
    sectors, device: str = "dt2", lo: int | None = None, hi: int | None = None
) -> list[int]:
    """Sectors of the card overlay that lie in [lo, hi) (default: the module store area of DEVICE). `sectors` is a list of sector
    numbers, bytes of an overlay file, or a path to one. An empty result is the pass."""
    if lo is None or hi is None:
        lo, hi = card_store(device)
    if isinstance(sectors, (str, os.PathLike)):
        sectors = pathlib.Path(sectors).read_bytes()
    if isinstance(sectors, (bytes, bytearray)):
        sectors = parse_overlay(bytes(sectors))
    return sorted(s for s in sectors if lo <= s < hi)


def runtime_card_policy(
    sectors, device: str, firmware_areas=(), store_extra=frozenset()
) -> dict:
    """The scenario common_checks policy, retaining its exact store and low-sector restrictions."""
    store = pk_device.store_of(device)
    allowed = {store.sel_a, store.sel_b}
    return {
        "invalid_store": sorted(
            set(card_store_hits(sectors, device)) - allowed - store_extra
        ),
        "invalid_low": sorted(
            s
            for s in set(sectors)
            if s < 0x50000
            and s not in allowed
            and not any(lo <= s < hi for lo, hi in firmware_areas)
        ),
    }


def sector_ranges(sectors) -> list[tuple[int, int]]:
    """Runs of consecutive sectors as (first, last) pairs."""
    out: list[list[int]] = []
    for s in sorted(sectors):
        if out and s == out[-1][1] + 1:
            out[-1][1] = s
        else:
            out.append([s, s])
    return [(a, b) for a, b in out]


def project_slots(sectors) -> list[int]:
    """MmcFs project slots (0x58000 + 0x8000 * slot, 0x8000 sectors each) that the overlay touched."""
    return sorted(
        {
            (s - 0x58000) // 0x8000
            for s in sectors
            if 0x58000 <= s < 0x58000 + 128 * 0x8000
        }
    )


def _json_lines(text: str) -> list[dict]:
    out = []
    for line in text.splitlines():
        if line.startswith("{"):
            with contextlib.suppress(ValueError):
                out.append(json.loads(line))
    return out


def run(
    image: str,
    script: panel_script.Script | str,
    out: pathlib.Path,
    card: pathlib.Path | None = None,
    extra: list[str] | None = None,
    watch_limit: int = 100000,
    canary: bool = True,
    flush_check: bool = True,
    timeout_s: float = 900.0,
    binary: pathlib.Path | None = None,
    watch: str | None = None,
    syx: pathlib.Path | None = None,
    allow_pc: str | None = None,
    env_extra: dict[str, str] | None = None,
) -> dict:
    """One bounded probe run; returns the facts. `out` is the run directory (created). `watch` replaces the write-watch
    ranges (a control: add a range the firmware does write, such as the display buffer 0x44e4a5b8+0x400, and the event
    count must be above zero). `syx` runs a patched image (built from the stock firmware of `image`; the probe gets the
    stock image's SHA-256 as its base). `allow_pc` (LO-HI[,..]) are PC ranges whose stores are not reported: the
    module's own code, which writes its bss in the module RAM. `env_extra` is added to the probe's environment (for
    example DIGI_STACK_WATCH, docs/INSTRUMENTATION.md)."""
    out = pathlib.Path(out)
    out.mkdir(parents=True, exist_ok=True)
    lo, hi = expansion(image)
    binary = binary or regions.probe_binary()
    _, syx_name, base = regions.IMAGES[image]
    import hashlib

    stock = config.stock(syx_name)
    cmd = [str(binary), str(syx or stock)]
    if syx is not None:
        canary = False  # the fill would overwrite the module's code and data, which sit in the region
        # a patched image: the probe boots it from the stock image's profile (and the stock image of its base, if any)
        cmd += [
            "--base-sha",
            hashlib.sha256(
                config.stock(regions.IMAGES[base or image][1]).read_bytes()
            ).hexdigest(),
        ]
    elif base:
        cmd += [
            "--base-sha",
            hashlib.sha256(
                config.stock(regions.IMAGES[base][1]).read_bytes()
            ).hexdigest(),
        ]
    if card is not None:
        cmd += ["--card", str(card)]
    if canary:
        cmd += [
            "--fill",
            "0x%x:0x%x" % (lo, hi - lo),
            "--fill",
            "0x%x:0x%x" % (lo + regions.ALIAS, hi - lo),
        ]
    if isinstance(script, str):
        script = resolve_script(script)
    cmd += script.args(frame_dir=out)
    cmd += [
        "--overlay-out",
        str(out / "overlay.bin"),
        "--midi-out",
        str(out / "midi.out"),
        "--usb-out",
        str(out / "usb.out"),
    ]
    cmd += list(extra or [])
    env = python_env()
    env.update(
        DIGI_WRITE_WATCH=watch or "0x%x-0x%x" % (lo, hi),
        DIGI_WRITE_WATCH_AFTER="cacr",
        DIGI_WRITE_WATCH_OUT=str(out / "writewatch.jsonl"),
        DIGI_WRITE_WATCH_LIMIT=str(watch_limit),
    )
    if allow_pc:
        env["DIGI_WRITE_WATCH_ALLOW_PC"] = allow_pc
    env.update(env_extra or {})
    if flush_check:
        env["DIGI_FLUSH_CHECK"] = "1"
    for f in ("writewatch.jsonl", "writewatch.summary.json", "overlay.bin"):
        (out / f).unlink(missing_ok=True)
    proc = subprocess.run(
        cmd, cwd=ROOT, env=env, capture_output=True, text=True, timeout=timeout_s
    )
    (out / "probe.stdout").write_text(proc.stdout)
    (out / "probe.stderr").write_text(proc.stderr)
    lines = _json_lines(proc.stdout)
    facts: dict = {
        "image": image,
        "exit": proc.returncode,
        "cmd": " ".join(cmd[:3]) + " ...",
    }
    final = next((r for r in lines if r.get("report") == "final"), {})
    facts["icount"] = final.get("icount")
    facts["ready_at"] = next((r["ready_at"] for r in lines if "ready_at" in r), None)
    facts["canary_bad_words"] = (
        sum(r["bad_words"] for r in final.get("regions", [])) if canary else None
    )
    facts["errors"] = [r for r in lines if r.get("error") or r.get("input_error")]
    midi = next((r["midi"] for r in lines if "midi" in r), {})
    facts["midi"] = midi
    facts["usb_out"] = next((r["usb"]["out"] for r in lines if "usb" in r), "")
    summary = out / "writewatch.summary.json"
    ww = json.loads(summary.read_text()) if summary.exists() else None
    facts["write_events"] = None if ww is None else ww["events"]
    facts["flush_hits"] = None if ww is None else len(ww["flush_hits"])
    facts["stacks"] = [] if ww is None else ww.get("stacks", [])
    facts["stack_epochs"] = (
        []
        if ww is None
        else [
            json.loads(r) if isinstance(r, str) else r
            for r in ww.get("stack_epochs", [])
        ]
    )
    facts["watch_stats"] = (
        None
        if ww is None
        else {
            k: ww[k]
            for k in (
                "cacr_writes",
                "flush_stores_tracked",
                "flush_fetches_checked",
                "cpushl",
            )
        }
    )
    overlay = (
        (out / "overlay.bin").read_bytes() if (out / "overlay.bin").exists() else b""
    )
    sectors = parse_overlay(overlay)
    facts["card_sectors"] = len(sectors)
    facts["card_store_hits"] = card_store_hits(sectors, image[:3])
    facts["card_ranges"] = sector_ranges(sectors)
    facts["project_slots_touched"] = project_slots(sectors)
    facts["pc_hits"] = {r["pc"]: r["hits"] for r in lines if "pc" in r}
    facts["frames"] = []
    for name, _ in script.frames:
        raw = out / (name + ".raw")
        if raw.exists():
            from tools.control.digi_client import oled_png

            png = out / (name + ".png")
            png.write_bytes(oled_png(raw.read_bytes()))
            facts["frames"].append(str(png))
    facts["frame_sha256"] = frame_hashes(out, script)
    replies = [
        parse_reply(m)
        for m in panel_script.split_sysex(bytes.fromhex(facts["usb_out"]))
    ]
    facts["usb_reply_types"] = [r[0] for r in replies if r]
    facts["usb_replies"] = [(r[0], len(r[1])) for r in replies if r]
    facts["pass"] = (
        proc.returncode == 0
        and facts["write_events"] == 0
        and not facts["card_store_hits"]
        and not facts["errors"]
        and facts["canary_bad_words"] in (0, None)
        and facts["flush_hits"] in (0, None)
    )
    (out / "facts.json").write_text(json.dumps(facts, indent=1))
    return facts


SCRIPTS_BY_NAME = panel_script.SCRIPTS


class _ArgsScript:
    """A `regions.scenario_args` scenario (nav, play, boot) as a script: no frames, fixed arguments."""

    frames: list = []

    def __init__(self, name: str) -> None:
        self._args = regions.scenario_args(name)

    def args(self, frame_dir=None, tail: int = 0) -> list[str]:
        return list(self._args)


def resolve_script(name: str):
    """A named panel script, or one of `regions.scenario_args` (boot, nav, play)."""
    if name in SCRIPTS_BY_NAME:
        return SCRIPTS_BY_NAME[name]()
    return _ArgsScript(name)


GOLDEN = ROOT / "tests" / "golden" / "arena_safety_frames.json"


def frame_hashes(out: pathlib.Path, script) -> dict[str, str]:
    """SHA-256 of each dumped OLED frame (the 1024 decoded bytes)."""
    import hashlib

    return {
        name: hashlib.sha256((out / (name + ".raw")).read_bytes()).hexdigest()
        for name, _ in script.frames
        if (out / (name + ".raw")).exists()
    }


def check_reached(
    script_name: str, image: str, facts: dict, goldens: dict | None = None
) -> list[str]:
    """What the run did not reach, as messages (empty: it reached what `panel_script.EXPECT` says).

    Checks: `project_slots` (the MmcFs project slots the card overlay touched, exactly), `frames` (the SHA-256 of the
    named OLED frames against `tests/golden/arena_safety_frames.json`, when that file has them), `usb_replies` (the
    request types the firmware answered over USB, in order), `din_bytes` (bytes the firmware took from MIDI DIN) and `pcs`
    (probe hit counts)."""
    expect = panel_script.EXPECT.get(script_name, {})
    bad: list[str] = []
    if (
        "project_slots" in expect
        and facts["project_slots_touched"] != expect["project_slots"]
    ):
        bad.append(
            "project slots %s, expected %s"
            % (facts["project_slots_touched"], expect["project_slots"])
        )
    if "usb_replies" in expect:
        want = expect["usb_replies"]
        want = want[image[:3]] if isinstance(want, dict) else want
        if facts.get("usb_reply_types") != want:
            bad.append(
                "usb replies %s, expected %s" % (facts.get("usb_reply_types"), want)
            )
    if (
        "din_bytes" in expect
        and facts["midi"].get("status", {}).get("rx_irqs") != expect["din_bytes"]
    ):
        bad.append(
            "din irqs %s, expected %s"
            % (facts["midi"].get("status", {}).get("rx_irqs"), expect["din_bytes"])
        )
    for pc, n in (
        expect.get("pcs", {}).items() if image == "dt2-1.16" else ()
    ):  # addresses of that image
        if facts["pc_hits"].get(pc) != n:
            bad.append(
                "pc %s hit %s times, expected %s" % (pc, facts["pc_hits"].get(pc), n)
            )
    if goldens is None and GOLDEN.exists():
        goldens = json.loads(GOLDEN.read_text())
    for name in expect.get("frames", []):
        want = (goldens or {}).get("%s/%s/%s" % (image, script_name, name))
        got = facts.get("frame_sha256", {}).get(name)
        if got is None:
            bad.append("frame %s was not dumped" % name)
        elif want is not None and want != got:
            bad.append("frame %s differs from the golden" % name)
    return bad


SUITE = [
    "boot",
    "nav",
    "play",
    "save_project",
    "save_quick",
    "load_project",
    "new_project",
    "manage_delete",
    "manage_rename",
    "format_pool",
    "os_update",
    "check_filesystem",
    "pattern_temp",
    "din_midi",
    "midirpc_info",
    "midirpc_files",
    "midirpc_write",
]
# Scripts that only make sense on one device (the menu has no such entry on the other).
ONLY = {"check_filesystem": "dt2"}


def suite(
    image: str,
    names: list[str],
    out_root: pathlib.Path,
    card: pathlib.Path | None,
    jobs: int = 4,
    update_goldens: bool = False,
    run_kw: dict | None = None,
) -> list[dict]:
    """Run scripts (concurrently, `jobs` at a time) and judge each: no write into cf.expansion, no flush hit, canary intact,
    no module-store sector written, and what `check_reached` demands."""
    from concurrent.futures import ThreadPoolExecutor

    names = [n for n in names if ONLY.get(n, image[:3]) == image[:3]]
    binary = regions.probe_binary()
    goldens = json.loads(GOLDEN.read_text()) if GOLDEN.exists() else {}

    def one(name: str) -> dict:
        out = out_root / name
        facts = run(image, name, out, card=card, binary=binary, **(run_kw or {}))
        facts["script"] = name
        if update_goldens:
            for fname, sha in facts["frame_sha256"].items():
                goldens["%s/%s/%s" % (image, name, fname)] = sha
        facts["not_reached"] = check_reached(name, image, facts, goldens)
        facts["pass"] = bool(facts["pass"] and not facts["not_reached"])
        (out / "facts.json").write_text(json.dumps(facts, indent=1))
        return facts

    with ThreadPoolExecutor(max_workers=jobs) as pool:
        results = list(pool.map(one, names))
    if update_goldens:
        GOLDEN.parent.mkdir(parents=True, exist_ok=True)
        GOLDEN.write_text(json.dumps(dict(sorted(goldens.items())), indent=1) + "\n")
    return results


def table(results: list[dict]) -> str:
    rows = [
        "| script | instructions | write events | card sectors | store sectors | canary | flush | reached | pass |",
        "|---|---|---|---|---|---|---|---|---|",
    ]
    for f in results:
        rows.append(
            "| %s | %s | %s | %s | %s | %s | %s | %s | %s |"
            % (
                f["script"],
                f["icount"],
                f["write_events"],
                f["card_sectors"],
                len(f["card_store_hits"]),
                f["canary_bad_words"],
                f["flush_hits"],
                "; ".join(f["not_reached"]) or "yes",
                "PASS" if f["pass"] else "FAIL",
            )
        )
    return "\n".join(rows)


def static_check(image: str, with_cf: bool = True) -> dict:
    """The static arena check of one image: every absolute reference the MAIN OS code makes into `cf.expansion` (cached
    view and the +0x08000000 uncached alias), and every immediate that looks like a sector or byte address of the module
    store (the image's device area, as a sector number and as a byte offset), by a linear sweep (tools/coldfire/refscan.py), plus
    the call and data references into the range that the Ghidra-derived database knows (cf.py refs_to; skipped when the
    database is absent). A sweep cannot see computed addresses: `hits` empty is a lower bound, not a proof."""
    from tools.coldfire import refscan

    lo, hi = expansion(image)
    store_lo, store_hi = card_store(image[:3])
    main = regions.load_main(image)
    base = regions.MAIN_BASE
    out: dict = {"image": image, "range": [lo, hi], "views": {}, "store": {}}
    for name, a, b, minv in (
        ("cached", lo, hi, 0x10000),
        ("uncached", lo + regions.ALIAS, hi + regions.ALIAS, 0x10000),
        ("store_sector", store_lo, store_hi, 0x10000),
    ):
        hits, n, covered, swept = refscan.scan(
            main, base, base, base + len(main), a, b, minv
        )
        d = {
            "range": [a, b],
            "instructions": n,
            "coverage_pct": round(100.0 * covered / swept, 2),
            "hits": [
                {"pc": pc, "ops": "%s %s" % (mn, ops), "target": t}
                for pc, _, mn, ops, t in hits
            ],
        }
        (out["store"] if name.startswith("store") else out["views"])[name] = d
    if with_cf:
        out["cf"] = _cf_refs(image, lo, hi)
        _tag_functions(image, out)
    return out


def _tag_functions(image: str, res: dict) -> None:
    """Add `func` (entry, name) to each hit whose pc lies in a function that the Ghidra dump found, else None: a hit
    outside every function is data (strings, tables) that the linear sweep decoded as code."""
    from tools.coldfire import cf

    try:
        img = cf.load(image + "-emac")
    except FileNotFoundError:
        return
    for grp in ("views", "store"):
        for d in res[grp].values():
            for h in d["hits"]:
                row = img.sql(
                    'SELECT entry, name FROM functions WHERE image=? AND entry<=? AND "end">=? ORDER BY entry DESC LIMIT 1',
                    img.name,
                    h["pc"],
                    h["pc"],
                )
                h["func"] = [row[0][0], row[0][1]] if row else None


def _cf_refs(image: str, lo: int, hi: int) -> dict:
    """Rows of the call/data reference tables whose target lies in the range (either view)."""
    from tools.coldfire import cf

    try:
        img = cf.load(image + "-emac")
    except FileNotFoundError:
        return {"available": False}
    rows: list = []
    for a, b in ((lo, hi), (lo + regions.ALIAS, hi + regions.ALIAS)):
        rows += img.sql(
            "SELECT 'data', site, to_addr FROM datarefs WHERE image=? AND to_addr>=? AND to_addr<? "
            "UNION ALL SELECT 'edge', site, to_addr FROM edges WHERE image=? AND to_addr>=? AND to_addr<?",
            img.name,
            a,
            b,
            img.name,
            a,
            b,
        )
    return {"available": True, "rows": [list(r) for r in rows]}


def _static_report(r: dict) -> str:
    lines = ["%s cf.expansion %#x-%#x" % (r["image"], *r["range"])]
    for grp in ("views", "store"):
        for name, d in r[grp].items():
            lines.append(
                "  %-13s %#x-%#x  %.2f%% coverage (%d instructions)  %d hits"
                % (
                    name,
                    *d["range"],
                    d["coverage_pct"],
                    d["instructions"],
                    len(d["hits"]),
                )
            )
            for h in d["hits"]:
                f = h.get("func")
                lines.append(
                    "    %#x  %s -> %#x  %s"
                    % (
                        h["pc"],
                        h["ops"],
                        h["target"],
                        "in %s %#x" % (f[1], f[0]) if f else "outside every function",
                    )
                )
    cfr = r.get("cf", {})
    lines.append(
        "  cf tables      %s"
        % ("%d rows" % len(cfr["rows"]) if cfr.get("available") else "no database")
    )
    for kind, site, to in cfr.get("rows", []):
        lines.append("    %s %#x -> %#x" % (kind, site, to))
    return "\n".join(lines)


def _patched_kw(a) -> dict:
    if not a.syx:
        return {}
    kw: dict = {"syx": pathlib.Path(a.syx)}
    if a.plan:
        kw["allow_pc"] = plan_allow_pc(a.plan)
    return kw


def main(argv=None) -> int:
    ap = argparse.ArgumentParser(description=(__doc__ or "").splitlines()[0])
    sub = ap.add_subparsers(dest="cmd", required=True)
    r = sub.add_parser("run")
    r.add_argument("image")
    r.add_argument("script")
    r.add_argument("--card")
    r.add_argument("--out", default=None)
    r.add_argument("--watch-limit", type=int, default=100000)
    r.add_argument(
        "--syx", help="a patched image built from the stock firmware of IMAGE"
    )
    r.add_argument(
        "--plan",
        help="its patch plan (gives the module's own PC range, whose stores are not reported)",
    )
    sub.add_parser("list")
    su = sub.add_parser("suite")
    su.add_argument("image")
    su.add_argument("scripts", nargs="*", help="default: the whole SUITE")
    su.add_argument("--card")
    su.add_argument("--out", default=None)
    su.add_argument("--jobs", type=int, default=4)
    su.add_argument("--update-goldens", action="store_true")
    su.add_argument(
        "--syx", help="a patched image built from the stock firmware of IMAGE"
    )
    su.add_argument("--plan", help="its patch plan")
    st = sub.add_parser("static")
    st.add_argument("images", nargs="+")
    st.add_argument("--json")
    st.add_argument("--no-cf", action="store_true")
    c = sub.add_parser("card-check")
    c.add_argument("overlay")
    c.add_argument(
        "--device",
        default="dt2",
        help="the device whose store area is checked (default dt2)",
    )
    c.add_argument("--lo", type=lambda s: int(s, 0), default=None)
    c.add_argument("--hi", type=lambda s: int(s, 0), default=None)
    a = ap.parse_args(argv)
    if a.cmd == "list":
        for n in sorted(SCRIPTS_BY_NAME):
            print(n)
        return 0
    if a.cmd == "static":
        res = [static_check(i, not a.no_cf) for i in a.images]
        for r in res:
            print(_static_report(r))
        if a.json:
            pathlib.Path(a.json).write_text(json.dumps(res, indent=1))
        return 0
    if a.cmd == "card-check":
        lo, hi = card_store(a.device)
        lo, hi = lo if a.lo is None else a.lo, hi if a.hi is None else a.hi
        hits = card_store_hits(a.overlay, a.device, lo, hi)
        print(
            "sectors in [%#x, %#x): %d %s"
            % (lo, hi, len(hits), "PASS" if not hits else "FAIL")
        )
        return 1 if hits else 0
    if a.cmd == "suite":
        out = pathlib.Path(a.out or ROOT / "out" / "runs" / "arena" / a.image)
        results = suite(
            a.image,
            a.scripts or SUITE,
            out,
            pathlib.Path(a.card) if a.card else None,
            a.jobs,
            a.update_goldens,
            run_kw=_patched_kw(a),
        )
        text = table(results)
        (out / "summary.md").write_text(text + "\n")
        print(text)
        return 0 if all(f["pass"] for f in results) else 1
    out = pathlib.Path(
        a.out or ROOT / "out" / "runs" / "arena" / ("%s-%s" % (a.image, a.script))
    )
    facts = run(
        a.image,
        a.script,
        out,
        card=pathlib.Path(a.card) if a.card else None,
        watch_limit=a.watch_limit,
        **_patched_kw(a),
    )
    print(
        json.dumps(
            {k: v for k, v in facts.items() if k not in ("card_ranges",)}, indent=1
        )
    )
    return 0 if facts["pass"] else 1


if __name__ == "__main__":
    sys.exit(main())


def contact_sheet(pngs, out, cols: int = 3) -> pathlib.Path:
    """The OLED frames of a run side by side with their names (for reading a script's path through the menus)."""
    from PIL import Image, ImageDraw

    pngs = [pathlib.Path(p) for p in pngs]
    ims = [Image.open(p).convert("RGB") for p in pngs]
    w, h = ims[0].size
    rows = (len(ims) + cols - 1) // cols
    sheet = Image.new("RGB", (cols * (w + 8), rows * (h + 22)), (40, 40, 40))
    d = ImageDraw.Draw(sheet)
    for k, (p, im) in enumerate(zip(pngs, ims, strict=True)):
        x, y = (k % cols) * (w + 8), (k // cols) * (h + 22)
        d.text((x + 2, y + 2), p.stem, fill=(255, 255, 0))
        sheet.paste(im, (x, y + 18))
    out = pathlib.Path(out)
    sheet.save(out)
    return out
