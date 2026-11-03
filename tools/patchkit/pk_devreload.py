#!/usr/bin/env python3
"""Fast edit loop for module code on the emulator: build the changed objects, reload a kept daemon, press keys, save screenshots.

    mise run dk:dev -- MODULE_OR_SET [--device dn2[,dt2]] [--keys FILE|INLINE] [--store IDS] [--trace] [--out DIR] [--stop]

MODULE_OR_SET is a patch set of devices/<device>.toml (`emulator`), or module ids separated by commas (`test/ui-demo`); the modules
they require are added. Per device the tool

1. builds the objects of every module in the set whose sources changed (`patchkit build`: unchanged objects cost nothing),
2. keeps one daemon per device and module set alive between runs (run directory out/runs/patchkit/run-devreload-<device>-<set>; the first run
   starts it and boots the patched image, `--stop` ends it) and calls `dev.reload` (docs/CONTROL.md): the daemon applies the
   modules again, compares the image with the running one and either swaps the module bytes into a restored main-screen state
   (`swap`: UI layer modules, about a second) or loads the new image (`boot`: the runtime or any other module changed; a cold
   boot unless that image was booted before),
3. runs the key script from the main screen the reload leaves, saves the frames it names, and writes one contact sheet per device.

A key script is a JSON list of harness steps (`{"tap": "SETTINGS"}`, `{"wait_ms": 300}`, `{"turn": "ENC A", "by": 1}`,
`{"shot": "name"}`) or inline text: tokens separated by commas or newlines, `tap:KEY`, `wait:MS`, `turn:ENC A:BY`, `shot:NAME`, each
with an optional `*N` repeat (`tap:DOWN*8`). Without a script the tool takes one frame, `main`. Output: DIR/<device>/NAME.png (4x
OLED = 512x256) and DIR/<device>/sheet.png (2x, four per row, labelled). DIR defaults to out/runs/devreload.

The harness stays the final proof: this loop shows what a change looks like, it does not replace `module:check` or the device suites.
"""

from __future__ import annotations

import argparse
import contextlib
import json
import os
import re
import shutil
import sys
import time
import tomllib
from pathlib import Path
from typing import Any

HERE = Path(__file__).resolve().parent
ROOT = HERE.parent.parent

from tools.control.digi_client import Emu, EmuError, oled_png  # noqa: E402
from tools.patchkit import modroots, patchkit  # noqa: E402
from tools.patchkit import pk_harness as H  # noqa: E402

FIRMWARE = {"dt2": "1.17", "dn2": "1.12"}
STOCK_SYX = {"dt2": "Digitakt_II_OS1.17.syx", "dn2": "Digitone_II_OS1.12.syx"}
DEVICE_FILE = {"dt2": "digitakt-ii.toml", "dn2": "digitone-ii.toml"}
SHEET_COLUMNS = 4


def resolve_modules(target: str, device: str) -> list[str]:
    """The module roots: a patch set of the device file, or ids separated by commas."""
    data = tomllib.loads((ROOT / "devices" / DEVICE_FILE[device]).read_text())
    sets = data.get("patch_sets", {})
    if target in sets:
        return list(sets[target])
    return [m for m in target.split(",") if m]


def closure(roots: list[str]) -> list[str]:
    """The roots and everything they require (module.toml `requires`), each once."""
    seen: list[str] = []
    todo = list(roots)
    while todo:
        mid = todo.pop(0)
        if mid in seen:
            continue
        seen.append(mid)
        manifest = tomllib.loads((modroots.module_dir(mid) / "module.toml").read_text())
        todo += manifest["module"].get("requires", [])
    return seen


def build(modules: list[str]) -> tuple[float, list[str]]:
    """Build the objects of the modules whose sources changed; seconds and the modules that were rebuilt."""
    started = time.monotonic()
    rebuilt = []
    for mid in modules:
        messages = patchkit.build_module(modroots.module_dir(mid), check=False)
        if any("unchanged" not in m for m in messages):
            rebuilt.append(mid)
    return time.monotonic() - started, rebuilt


# ----- key scripts ------------------------------------------------------------------------------------------------


def parse_keys(text: str) -> list[dict[str, Any]]:
    """Harness steps from a JSON list or the inline syntax (see the module docstring)."""
    stripped = text.strip()
    if stripped.startswith("["):
        steps = json.loads(stripped)
        if not isinstance(steps, list):
            raise H.HarnessError("a key script in JSON is a list of steps")
        return steps
    steps = []
    for token in re.split(r"[,\n]", text):
        token = token.split("#")[0].strip()
        if not token:
            continue
        token, _, repeat = token.partition("*")
        kind, _, arg = token.partition(":")
        kind, arg = kind.strip(), arg.strip()
        if kind == "tap" and arg:
            step: dict[str, Any] = {"tap": arg}
        elif kind == "wait" and arg.isdigit():
            step = {"wait_ms": int(arg)}
        elif kind == "turn" and arg:
            name, _, by = arg.rpartition(":")
            step = {"turn": name, "by": int(by)} if name else {"turn": arg, "by": 1}
        elif kind == "shot" and arg:
            step = {"shot": arg}
        else:
            raise H.HarnessError(
                f"key script: cannot read {token!r} (tap:KEY wait:MS turn:ENC:BY shot:NAME)"
            )
        steps += [dict(step) for _ in range(int(repeat or 1))]
    return steps


def load_keys(keys: str | None) -> list[dict[str, Any]]:
    if not keys:
        return [{"shot": "main"}]
    try:
        path = Path(keys)
        is_file = path.is_file()
    except OSError:  # an inline script longer than a file name
        is_file = False
    return parse_keys(path.read_text() if is_file else keys)


def contact_sheet(frames: list[tuple[str, bytes]], dest: Path) -> None:
    """One PNG: the frames at 2x (256x128), four per row, each with its name under it."""
    from PIL import Image, ImageDraw

    cell_w, cell_h, label_h, gap = 256, 128, 14, 6
    rows = (len(frames) + SHEET_COLUMNS - 1) // SHEET_COLUMNS
    sheet = Image.new(
        "L",
        (SHEET_COLUMNS * (cell_w + gap) + gap, rows * (cell_h + label_h + gap) + gap),
        40,
    )
    draw = ImageDraw.Draw(sheet)
    for i, (name, raw) in enumerate(frames):
        x = gap + (i % SHEET_COLUMNS) * (cell_w + gap)
        y = gap + (i // SHEET_COLUMNS) * (cell_h + label_h + gap)
        png = dest.parent / f".{name}.tmp.png"
        png.write_bytes(oled_png(raw, scale=2))
        with Image.open(png) as im:
            sheet.paste(im.convert("L"), (x, y))
        png.unlink()
        draw.text((x + 2, y + cell_h + 1), name, fill=255)
    sheet.save(dest)


# ----- one device -------------------------------------------------------------------------------------------------


def run_dir_of(
    device: str, target: str, store: list[str] | None = None, trace: bool = False
) -> Path:
    slug = re.sub(
        r"[^A-Za-z0-9.+-]+",
        "_",
        target
        + ("+store-" + ",".join(store) if store else "")
        + ("+trace" if trace else ""),
    )
    return H.OUT / f"run-devreload-{device}-{slug}"


def connect(run_dir: Path) -> Emu | None:
    """The kept daemon of the run directory, or None when none answers."""
    try:
        emu = Emu.connect(run_dir=run_dir)
    except (OSError, EmuError):
        return None
    try:
        emu.status()
    except (OSError, EmuError):
        emu.close()
        return None
    return emu


def store_card(device: str, ids: list[str], run_dir: Path, trace: bool = False) -> Path:
    """A card image whose module store holds the modules `ids`, packed for the device (`<leaf>.dkm` under digikit/modules);
    with `trace` they are built with DK_TRACE (`patchkit pack --trace`)."""
    from tools.firmware import config
    from tools.patchkit import pk_dkrt

    tools = pk_dkrt.Tools()
    folder = run_dir / "plus_drive"
    shutil.rmtree(folder, ignore_errors=True)
    (folder / "digikit" / "modules").mkdir(parents=True)
    selected = []
    for mid in ids:
        pick = mid.endswith(
            "*"
        )  # a trailing * puts the module in the card's saved selection (it loads at the boot)
        mid = mid.rstrip("*")
        if mid.startswith(
            "file:"
        ):  # file:SRC=DEST, a host file or folder copied to digikit/DEST of the store (samples/...)
            src, _, dest = mid[5:].partition("=")
            to = folder / "digikit" / dest
            to.parent.mkdir(parents=True, exist_ok=True)
            if Path(src).is_dir():
                shutil.copytree(src, to, dirs_exist_ok=True)
            else:
                shutil.copyfile(src, to)
            continue
        if mid.startswith(
            "synth:"
        ):  # synth:NAME:BSS, a module that only needs BSS bytes of arena (dkmsynth)
            _, name, bss = mid.split(":")
            out = folder / "digikit" / "modules" / f"{name}.dkm"
            spec = run_dir / f"{name}.toml"
            spec.write_text(f'name = "dev/{name}"\nbss = {int(bss) // 4 * 4}\n')
            tools.patchkit_run("synth-dkm", spec, "--device", device, "-o", out)
        else:
            out = folder / "digikit" / "modules" / f"{mid.rpartition('/')[2]}.dkm"
            tools.patchkit_run(
                "pack",
                mid,
                "--device",
                device,
                "-o",
                out,
                *(["--trace"] if trace else []),
            )
        if pick:
            selected.append(f"modules/{out.name}")
    syx = config.stock(STOCK_SYX[device])
    image = run_dir / "store-card.img"
    pk_dkrt.build_card(tools, folder, image, syx, device)
    if selected:
        pk_dkrt.write_selection(image, selected, device)
    return image


def start(
    cell: H.Cell, run_dir: Path, store: list[str] | None = None, trace: bool = False
) -> tuple[Emu, H.Session]:
    """A new daemon that outlives this process: it boots the patched image of the cell (cached by image)."""
    session = H.Session(
        cell,
        H.Goldens(),
        cache_dir=H.SHARED_CACHE,
        run_name=f"devreload-{cell.device}",
        run_dir=run_dir,
    )
    if store:
        run_dir.mkdir(parents=True, exist_ok=True)
        session.card_override = str(store_card(cell.device, store, run_dir, trace))
    session.open()
    assert session.emu is not None
    emu = session.emu
    if session.card:
        (run_dir / "card.txt").write_text(session.card)
    return emu, session


def release(emu: Emu) -> None:
    """Close the connection and leave the daemon running."""
    emu._proc = None  # noqa: SLF001  Emu.close would wait for the process
    emu.close()


def session_on(emu: Emu, cell: H.Cell, run_dir: Path) -> H.Session:
    session = H.Session(cell, H.Goldens(), cache_dir=H.SHARED_CACHE, run_dir=run_dir)
    session.emu = emu
    card = run_dir / "card.txt"
    session.card = card.read_text() if card.is_file() else None
    return session


def stop(
    device: str, target: str, store: list[str] | None = None, trace: bool = False
) -> None:
    run_dir = run_dir_of(device, target, store, trace)
    emu = connect(run_dir)
    if emu is None:
        print(f"{device}: no daemon in {run_dir}")
        return
    with contextlib.suppress(OSError, EmuError):
        emu.quit()
    emu.close()
    print(f"{device}: daemon stopped")


def run_device(device: str, a: argparse.Namespace) -> dict[str, Any]:
    t_all = time.monotonic()
    modules = resolve_modules(a.target, device)
    for m in modules:
        modroots.module_dir(m)
    cell = H.Cell(device, FIRMWARE[device], modules)
    t_build, rebuilt = build(closure(modules))
    store = [m for m in (a.store or "").split(",") if m]
    run_dir = run_dir_of(device, a.target, store, a.trace)
    emu = connect(run_dir)
    started = False
    t_start = 0.0
    if emu is None:
        t = time.monotonic()
        emu, session = start(cell, run_dir, store, a.trace)
        t_start, started = time.monotonic() - t, True
    else:
        session = session_on(emu, cell, run_dir)
    try:
        t = time.monotonic()
        # auto_no: the DT2's FILE SYSTEM OK modal is closed once the state is restored, as the harness does after a restart.
        report = emu.call("dev.reload", auto_no=True)
        t_reload = time.monotonic() - t
        out = Path(a.out) / device
        out.mkdir(parents=True, exist_ok=True)
        t = time.monotonic()
        frames: list[tuple[str, bytes]] = []
        session.run_ms(300)
        emu.pcm_start()  # the harness' key actions mark the PCM window
        session.pcm_mark = 0
        for step in load_keys(a.keys):
            if "shot" in step:
                name = str(step["shot"])
                _digest, raw = session.frame()
                (out / f"{name}.png").write_bytes(oled_png(raw))
                frames.append((name, raw))
            else:
                session.run_steps([step])
        t_keys = time.monotonic() - t
        if frames:
            contact_sheet(frames, out / "sheet.png")
    finally:
        release(emu)
    return {
        "device": device,
        "path": report.get("path"),
        "reason": report.get("reason"),
        "report": report,
        "rebuilt": rebuilt,
        "started_daemon": started,
        "out": str(out),
        "seconds": {
            "build": round(t_build, 2),
            "daemon_start": round(t_start, 2),
            "reload": round(t_reload, 2),
            "keys": round(t_keys, 2),
            "total": round(time.monotonic() - t_all, 2),
        },
    }


def describe(r: dict[str, Any]) -> str:
    rep = r["report"]
    path = r["path"]
    detail = ""
    if path == "swap":
        detail = (
            f" {rep['ranges']} ranges {rep['bytes']} bytes ({', '.join(rep['owners'])});"
            f" apply {rep['apply_ms']:.0f} ms, restore {rep['restore_ms']:.0f} ms, write {rep['write_ms']:.0f} ms"
        )
    elif path in ("boot", "dsp"):
        detail = (
            f" ({r['reason']}); {rep.get('start_used')}, {rep.get('seconds', 0):.1f} s"
        )
    s = r["seconds"]
    return (
        f"{r['device']}: {path}{detail}\n"
        f"    rebuilt {', '.join(r['rebuilt']) or 'nothing'}; daemon {'started' if r['started_daemon'] else 'kept'};"
        f" build {s['build']} s, start {s['daemon_start']} s, reload {s['reload']} s, keys {s['keys']} s,"
        f" total {s['total']} s\n    frames {r['out']}/"
    )


def main(argv: list[str]) -> int:
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    ap.add_argument(
        "target", help="a patch set (emulator) or module ids separated by commas"
    )
    ap.add_argument(
        "--device", default="dn2", help="dn2, dt2 or dn2,dt2 (default dn2: the fastest)"
    )
    ap.add_argument(
        "--keys", help="a key script file (JSON or inline text) or the inline text"
    )
    ap.add_argument(
        "--store",
        help="module ids separated by commas: packed into the module store of the card the daemon boots with; an id ending in * is also in the saved selection, synth:NAME:BSS is a module that needs BSS bytes of arena, file:SRC=DEST puts a host file or folder at digikit/DEST of the store, e.g. out/smp=samples (a kept daemon keeps its card; each list has its own daemon)",
    )
    ap.add_argument(
        "--trace",
        action="store_true",
        help="build the --store modules with DK_TRACE (patchkit pack --trace): their DK_TRACE_LOG and DK_STEP marks exist; the kept daemon is its own (the card differs)",
    )
    ap.add_argument("--out", default=str(ROOT / "out" / "runs" / "devreload"))
    ap.add_argument(
        "--stop", action="store_true", help="end the kept daemons of this module set"
    )
    ap.add_argument("--json", action="store_true", help="print the reports as JSON")
    a = ap.parse_args(argv)
    devices = [d for d in a.device.split(",") if d]
    for d in devices:
        if d not in FIRMWARE:
            ap.error(f"unknown device {d!r}")
    if a.stop:
        for d in devices:
            stop(d, a.target, [m for m in (a.store or "").split(",") if m], a.trace)
        return 0
    results = []
    try:
        for d in devices:
            results.append(run_device(d, a))
    except (
        H.HarnessError,
        H.LoadFailure,
        H.Failure,
        patchkit.BuildError,
        EmuError,
    ) as e:
        print(f"dk:dev: {e}", file=sys.stderr)
        return 2
    for r in results:
        print(json.dumps(r) if a.json else describe(r), flush=True)
    return 0


if __name__ == "__main__":
    os.chdir(ROOT)
    sys.exit(main(sys.argv[1:]))
