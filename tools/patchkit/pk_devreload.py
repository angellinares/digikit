#!/usr/bin/env python3
"""Fast edit loop for module code on the emulator: build the changed objects, reload a kept daemon, press keys, save screenshots.

    mise run dk:dev -- MODULE_OR_SET [--device dn2[,dt2]] [--keys FILE|INLINE] [--store IDS] [--trace] [--out DIR] [--stop]

MODULE_OR_SET is a patch set of devices/<device>.toml (`emulator`), or module ids separated by commas (`test/ui-demo`); the modules
they require are added. Per device the tool

1. builds the objects of every module in the set whose sources changed (`patchkit build`: unchanged objects cost nothing),
2. keeps one daemon per device and module set alive between runs (run directory out/runs/patchkit/run-devreload-<device>-<set>-<dsp>; the first run
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
import hashlib
import json
import os
import re
import shutil
import sys
import tempfile
import time
import tomllib
from collections.abc import Callable, Iterator
from pathlib import Path
from typing import Any

HERE = Path(__file__).resolve().parent
ROOT = HERE.parent.parent

from tools.control.digi_client import Emu, EmuError, oled_png  # noqa: E402
from tools.devtools.portable_flock import LOCK_EX, LOCK_NB, LOCK_UN, flock  # noqa: E402
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


def dsp_mode(
    modules: list[str], store: list[str], *, zero_framework: bool = False
) -> str:
    """Classify both closures; a zero-safe base may have core DSP adapters.

    Store files can contain arbitrary packed modules, so their DSP needs are unknown.
    Synthetic BSS-only modules contain no DSP code. Store closures never inherit the
    base set's permission to run its framework adapters with a zero peer.
    """
    stored = []
    for entry in store:
        mid = entry.rstrip("*")
        if mid.startswith("file:"):
            return "sharc"
        if not mid.startswith("synth:"):
            stored.append(mid)
    try:
        for roots, allow_adapters in ((modules, zero_framework), (stored, False)):
            for mid in closure(roots):
                manifest = tomllib.loads(
                    (modroots.module_dir(mid) / "module.toml").read_text()
                )
                kind = manifest["module"].get("type")
                if kind in ("machine", "fx"):
                    return "sharc"
                for obj in manifest.get("object", []):
                    place = obj.get("place")
                    if place in ("cf.expansion", "cf.flash_cave"):
                        continue
                    if (
                        allow_adapters
                        and mid.startswith("core/")
                        and kind == "framework"
                        and place == "dsp.l2"
                    ):
                        continue
                    return "sharc"
    except (OSError, ValueError, KeyError, SystemExit):
        return "sharc"
    return "zero"


def target_dsp_mode(target: str, device: str, store: list[str]) -> str:
    """Declared zero-safe patch sets may contain optional core DSP adapters."""
    data = tomllib.loads((ROOT / "devices" / DEVICE_FILE[device]).read_text())
    policy = tomllib.loads((HERE / "pk_devreload.toml").read_text())
    hinted = target in policy.get("zero_dsp_patch_sets", []) and target in data.get(
        "patch_sets", {}
    )
    return dsp_mode(resolve_modules(target, device), store, zero_framework=hinted)


def store_ui_preview_mode(target: str, device: str, store: list[str]) -> str:
    """Internal dk:module path: one declarative machine on a zero-safe base."""
    if (
        target != "emulator"
        or len(store) != 1
        or store[0].startswith(("file:", "synth:"))
    ):
        raise H.HarnessError(
            "UI preview requires the emulator set and one machine store id"
        )
    mid = store[0].rstrip("*")
    manifest = tomllib.loads((modroots.module_dir(mid) / "module.toml").read_text())
    if manifest["module"].get("type") != "machine":
        raise H.HarnessError(
            "UI preview store requires a declarative machine; use --dsp"
        )
    if target_dsp_mode(target, device, []) != "zero":
        raise H.HarnessError("UI preview requires a zero-DSP emulator base")
    return "zero"


def assert_mode(emu: Emu, dsp: str) -> None:
    status = emu.status()
    audio = status.get("audio")
    if status.get("dsp") != dsp or (audio is not None) != (dsp == "sharc"):
        raise H.HarnessError(
            f"expected DSP {dsp}, got dsp={status.get('dsp')!r}, audio={audio is not None}"
        )


def build(modules: list[str]) -> tuple[float, list[str]]:
    """Build the objects of the modules whose sources changed; seconds and the modules that were rebuilt."""
    started = time.monotonic()
    rebuilt = []
    dirs = {mid: modroots.module_dir(mid) for mid in modules}
    statuses = patchkit.module_statuses(list(dirs.values())) if dirs else {}
    for mid, folder in dirs.items():
        if statuses[folder] == "unchanged":
            continue
        messages = patchkit.build_module(folder, check=False)
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
    device: str,
    target: str,
    store: list[str] | None = None,
    trace: bool = False,
    dsp: str | None = "sharc",
) -> Path:
    slug = re.sub(
        r"[^A-Za-z0-9.+-]+",
        "_",
        target
        + ("+store-" + ",".join(store) if store else "")
        + ("+trace" if trace else ""),
    )
    # None names the exact pre-mode directory, used only to stop legacy daemons.
    suffix = f"-{dsp}" if dsp is not None else ""
    return H.OUT / f"run-devreload-{device}-{slug}{suffix}"


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


def pack_store(*args: object) -> None:
    """Packing needs only patchkit, not the native proof binaries used to build cards."""
    result = patchkit.run([str(arg) for arg in args], capture=True)
    if result.returncode:
        raise H.HarnessError((result.stderr or result.stdout).strip())


def prepare_store(
    device: str,
    ids: list[str],
    run_dir: Path,
    trace: bool = False,
    *,
    ui_preview: bool = False,
) -> dict[str, Any]:
    """Pack/copy requested content in isolated staging, without touching a mounted card."""
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
            pack_store("synth-dkm", spec, "--device", device, "-o", out)
        else:
            out = folder / "digikit" / "modules" / f"{mid.rpartition('/')[2]}.dkm"
            pack_store(
                "pack",
                mid,
                "--device",
                device,
                "-o",
                out,
                *(["--trace"] if trace else []),
                *(["--ui-preview"] if ui_preview else []),
            )
        if pick:
            selected.append(f"modules/{out.name}")
    files = {}
    for path in sorted(folder.rglob("*")):
        if path.is_file():
            with path.open("rb") as content:
                files[path.relative_to(folder).as_posix()] = hashlib.file_digest(
                    content, "sha256"
                ).hexdigest()
    return {
        "device": device,
        "ids": ids,
        "trace": trace,
        "ui_preview": ui_preview,
        "selected": selected,
        "files": files,
    }


def store_card(
    device: str,
    ids: list[str],
    run_dir: Path,
    trace: bool = False,
    *,
    ui_preview: bool = False,
    prepared: dict[str, Any] | None = None,
) -> Path:
    """Build a card from freshly prepared content, including its initial selection."""
    from tools.firmware import config
    from tools.patchkit import pk_dkrt

    if prepared is None:
        prepared = prepare_store(device, ids, run_dir, trace, ui_preview=ui_preview)
    syx = config.stock(STOCK_SYX[device])
    image = run_dir / "store-card.img"
    pk_dkrt.build_card(pk_dkrt.Tools(), run_dir / "plus_drive", image, syx, device)
    if prepared["selected"]:
        pk_dkrt.write_selection(image, prepared["selected"], device)
    (run_dir / "store-request.json").write_text(json.dumps(prepared, sort_keys=True))
    return image


def stop_connected(emu: Emu, run_dir: Path, timeout: float = 5) -> None:
    """Stop the connected process and await exit before replacing its mounted card."""
    try:
        info = json.loads((run_dir / "current.json").read_text())
        pid = info["pid"]
        if (
            type(pid) is not int
            or pid <= 0
            or info["socket"] != emu.path
            or pid == os.getpid()
        ):
            raise ValueError("daemon identity mismatch")
    except (OSError, ValueError, KeyError) as exc:
        raise H.HarnessError(
            f"cannot identify store daemon in {run_dir}: {exc}"
        ) from exc
    deadline = time.monotonic() + timeout
    try:
        emu._sock.settimeout(timeout)  # noqa: SLF001  quit must also be bounded
        emu.quit()
    except (OSError, EmuError) as exc:
        raise H.HarnessError(
            f"could not stop store daemon in {run_dir}: {exc}"
        ) from exc
    finally:
        release(emu)
    while True:
        try:
            os.kill(pid, 0)
        except ProcessLookupError:
            return
        if time.monotonic() >= deadline:
            raise H.HarnessError(f"store daemon did not stop in {run_dir}")
        time.sleep(0.05)


def upload_store(emu: Emu, candidate: Path, prepared: dict[str, Any]) -> None:
    """Update one preview DKM on the mounted card, then soft-reset the guest."""
    from tools.device import dkrun, dktransfer, dkupload

    def idle() -> None:
        if emu.status().get("job"):
            raise EmuError(3, "a job is running")

    retry_busy(idle)
    previous = retry_busy(lambda: emu.call("run.mode", mode="paused")).get(
        "previous", "paused"
    )
    try:
        emu.subscribe()
        up = dkupload.Uploader(dkupload.EmuTransport(emu))
        dkrun.require_runtime(up.info())
        path = prepared["selected"][0]
        source = candidate / "plus_drive/digikit" / path
        data = source.read_bytes()
        header = dktransfer.header_info(data[: dkupload.HEADER_BYTES])
        if header is None:
            raise H.HarnessError("preview pack has no DKM header")
        dkrun.upload_if_changed(up, path, data)
        if up.verify(path) != dkupload.file_hash(data).hex():
            raise H.HarnessError("preview module did not verify on the mounted card")
        # A preview selects one module explicitly; all other card contents stay.
        print("preview: upload verified; selecting module", flush=True)
        up.select(prepared["selected"])
        emu.drain_events()
        print("preview: restarting firmware", flush=True)
        up.restart()
        # Preview cards need not select the debug module that supplies BOOT_START.
        # The emulator event proves a new boot; MODULES proves the selection loaded.
        dkrun.emu_boot_waiter(emu, timeout_s=90)()
        print("preview: firmware booted; waiting for module", flush=True)
        dktransfer.wait_loaded(
            up,
            [header["name"]],
            sleep=lambda seconds: emu.step(int(seconds * dkrun.EMU_IPS)),
            now=time.monotonic,
            timeout_s=20,
            paths=prepared["selected"],
        )
        print(f"preview: module ready ({header['name']})", flush=True)
    except (
        dkupload.UploadError,
        dkrun.RunError,
        dktransfer.TransferError,
        TimeoutError,
    ) as exc:
        raise H.HarnessError(f"preview upload/restart failed: {exc}") from exc
    finally:
        emu.call("run.mode", mode=previous)


def can_upload_store(prepared: dict[str, Any], baseline: Any) -> bool:
    """Only replace bytes of the same one-module preview, never arbitrary card layouts."""
    if not isinstance(baseline, dict) or len(prepared["ids"]) != 1:
        return False
    mid = prepared["ids"][0]
    if mid.startswith(("file:", "synth:")) or not mid.endswith("*"):
        return False
    same_request = {k: v for k, v in prepared.items() if k != "files"} == {
        k: v for k, v in baseline.items() if k != "files"
    }
    return (
        same_request
        and set(prepared["files"])
        == {"digikit/" + path for path in prepared["selected"]}
        and len(prepared["selected"]) == 1
    )


def refresh_store(
    emu: Emu | None,
    device: str,
    target: str,
    ids: list[str],
    run_dir: Path,
    trace: bool,
    ui_preview: bool,
    *,
    live_update: bool = False,
) -> tuple[Emu | None, Path]:
    """Reuse the initial request baseline, or prepare a replacement before stopping."""
    run_dir.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory(prefix=".dk-store-", dir=run_dir.parent) as tmp:
        candidate = Path(tmp)
        prepared = prepare_store(device, ids, candidate, trace, ui_preview=ui_preview)
        prepared["target"] = target
        try:
            baseline = json.loads((run_dir / "store-request.json").read_text())
        except (OSError, ValueError):
            baseline = None
        image = run_dir / "store-card.img"
        overlay_required = run_dir / "store-overlay-required"
        recover_overlay = (
            emu is None
            and overlay_required.is_file()
            and not (run_dir / "overlay.bin").is_file()
        )
        if prepared == baseline and image.is_file() and not recover_overlay:
            return emu, image
        if (
            emu is not None
            and image.is_file()
            and live_update
            and can_upload_store(prepared, baseline)
        ):
            upload_store(emu, candidate, prepared)
            emu.call("card.overlay", path=str(run_dir / "overlay.bin"))
            # The base card still contains the previous DKM. A future launch
            # without this overlay must rebuild rather than trust the request.
            overlay_required.touch()
            # Commit the requested baseline only after upload, verification and boot.
            (run_dir / "store-request.json").write_text(
                json.dumps(prepared, sort_keys=True)
            )
            return emu, image
        # Card construction and selection validation may fail: keep the daemon alive until both succeed.
        store_card(
            device, ids, candidate, trace, ui_preview=ui_preview, prepared=prepared
        )
        if emu is not None:
            stop_connected(emu, run_dir)
        # A saved overlay belongs to the previous card layout/content. Quit may
        # have written it, so remove it only after the daemon has fully exited.
        (run_dir / "overlay.bin").unlink(missing_ok=True)
        overlay_required.unlink(missing_ok=True)
        shutil.rmtree(run_dir / "plus_drive", ignore_errors=True)
        shutil.move(str(candidate / "plus_drive"), run_dir / "plus_drive")
        for name in ("store-card.img", "store-request.json"):
            os.replace(candidate / name, run_dir / name)
        return None, image


def start(
    cell: H.Cell,
    run_dir: Path,
    store: list[str] | None = None,
    trace: bool = False,
    dsp: str = "sharc",
    *,
    store_ui_preview: bool = False,
    prepared_card: Path | None = None,
    reuse_runtime: bool = False,
) -> tuple[Emu, H.Session]:
    """A new daemon that outlives this process: it boots the patched image of the cell (cached by image)."""
    session = H.Session(
        cell,
        H.Goldens(),
        cache_dir=H.SHARED_CACHE,
        run_name=f"devreload-{cell.device}",
        run_dir=run_dir,
        dsp=dsp,
    )
    if prepared_card is not None:
        session.card_override = str(prepared_card)
    elif store:
        run_dir.mkdir(parents=True, exist_ok=True)
        session.card_override = str(
            store_card(cell.device, store, run_dir, trace, ui_preview=store_ui_preview)
        )
    image = cached_runtime_image(session) if reuse_runtime else None
    session.open(**({"image": image} if image is not None else {}))
    assert session.emu is not None
    emu = session.emu
    if session.card:
        (run_dir / "card.txt").write_text(session.card)
    return emu, session


def release(emu: Emu) -> None:
    """Close the connection and leave the daemon running."""
    emu._proc = None  # noqa: SLF001  Emu.close would wait for the process
    emu.close()


@contextlib.contextmanager
def session_lock(run_dir: Path, timeout: float = 300) -> Iterator[None]:
    """Serialize only this kept session's setup/navigation, never its faceplate lifetime."""
    run_dir.mkdir(parents=True, exist_ok=True)
    deadline = time.monotonic() + timeout
    waiting = False
    with (run_dir / "launcher.lock").open("a+b") as lock:
        while True:
            try:
                flock(lock, LOCK_EX | LOCK_NB)
                break
            except BlockingIOError as exc:
                if not waiting:
                    print(
                        f"Waiting for preview startup in {run_dir.name}...", flush=True
                    )
                    waiting = True
                if time.monotonic() >= deadline:
                    raise H.HarnessError(
                        f"preview startup did not finish in {run_dir}"
                    ) from exc
                time.sleep(0.1)
        try:
            yield
        finally:
            flock(lock, LOCK_UN)


def retry_busy[Result](operation: Callable[[], Result], timeout: float = 60) -> Result:
    """BUSY is a refusal before mutation. Wait without cancelling the existing job."""
    deadline = time.monotonic() + timeout
    waiting = False
    while True:
        try:
            return operation()
        except (EmuError, H.LoadFailure) as exc:
            error = exc if isinstance(exc, EmuError) else exc.__cause__
            if not isinstance(error, EmuError) or error.code != 3:
                raise
            if time.monotonic() >= deadline:
                raise H.HarnessError(
                    "preview daemon is still busy; its running job was left intact"
                ) from exc
            if not waiting:
                print("Waiting for the preview daemon's active job...", flush=True)
                waiting = True
            time.sleep(0.1)


def reload_ready(emu: Emu, session: H.Session, timeout: float = 60) -> dict[str, Any]:
    """Recover only the selected preview daemon; preserve/reapply its mounted card."""

    def reload() -> dict[str, Any]:
        return emu.call("dev.reload", auto_no=True)

    try:
        return retry_busy(reload, timeout)
    except EmuError as exc:
        if exc.code != 4 or "the running image has no patch record" not in str(exc):
            raise
    # Build before changing the session; another tuple uses another private path.
    assert session.run_dir is not None
    image = H.patched_image(session.cell, session.syx, session.run_dir / "recovery")
    print("Restoring this preview's patched runtime...", flush=True)
    retry_busy(lambda: session.load(image), timeout)
    return retry_busy(reload, timeout)


def runtime_fingerprint(modules: list[str], device: str, syx: Path) -> str | None:
    """Hash the patch inputs after incremental builds; missing inputs disable reuse."""
    paths = {
        syx,
        ROOT / "sdk/sdk.toml",
        ROOT / "Cargo.lock",
        ROOT / "packages/patchkit/Cargo.toml",
        Path(patchkit.__file__),
        Path(modroots.__file__),
        patchkit.binary(),
    }
    for tree in (ROOT / "devices", ROOT / "packages/patchkit/src"):
        paths.update(p for p in tree.rglob("*") if p.is_file())
    digest = hashlib.sha256(f"preview-runtime-v1:{device}".encode())
    try:
        for mid in closure(modules):
            folder = modroots.module_dir(mid)
            manifest = folder / "module.toml"
            data = tomllib.loads(manifest.read_text())
            paths.add(manifest)
            paths.update(
                modroots.object_path(data, folder, obj)
                for obj in data.get("object", [])
            )
        for path in sorted(paths):
            digest.update(str(path).encode())
            with path.open("rb") as content:
                digest.update(hashlib.file_digest(content, "sha256").digest())
    except OSError:
        return None
    return digest.hexdigest()


def cached_runtime_image(session: H.Session) -> Path | None:
    """A card-only restart reuses the patched firmware; verify bytes before reuse."""
    fingerprint = runtime_fingerprint(
        session.cell.modules, session.cell.device, session.syx
    )
    if fingerprint is None:
        return None
    assert session.run_dir is not None
    state = session.run_dir / "runtime-image.json"
    try:
        baseline = json.loads(state.read_text())
        image = Path(baseline["path"])
        # Cache paths are private to this session, even if its metadata was edited.
        if image.parent != session.run_dir / "runtime":
            raise ValueError("foreign cached image")
        with image.open("rb") as content:
            digest = hashlib.file_digest(content, "sha256").hexdigest()
        if baseline["inputs"] == fingerprint and baseline["sha256"] == digest:
            return image
    except (OSError, ValueError, KeyError, TypeError):
        pass
    image = H.patched_image(session.cell, session.syx, session.run_dir / "runtime")
    with image.open("rb") as content:
        digest = hashlib.file_digest(content, "sha256").hexdigest()
    state.write_text(
        json.dumps({"inputs": fingerprint, "path": str(image), "sha256": digest})
    )
    return image


def preview_reload(
    emu: Emu, session: H.Session, modules: list[str], started: bool
) -> dict[str, Any]:
    """Reuse only this launcher's verified patched image, waiting out active jobs."""
    if started:
        # Session.open already applied the patch and booted the selected card.
        return {"path": "boot", "reason": "new preview session"}

    def idle_status() -> dict[str, Any]:
        status = emu.status()
        if status.get("job"):
            raise EmuError(3, "a job is running")
        return status

    status = retry_busy(idle_status)
    wanted = cached_runtime_image(session)
    if wanted is None:
        # Missing inputs disable reuse. Rebuild this patch explicitly rather than
        # following an unknown running image's dev.reload record.
        assert session.run_dir is not None
        wanted = H.patched_image(
            session.cell, session.syx, session.run_dir / "recovery"
        )
        retry_busy(lambda: session.load(wanted))
        return {"path": "boot", "reason": "runtime inputs unavailable for reuse"}
    with wanted.open("rb") as content:
        digest = hashlib.file_digest(content, "sha256").hexdigest()
    image = (status.get("firmware") or {}).get("syx_sha256")
    if image == digest:
        return {"path": "kept", "reason": "unchanged runtime and image"}
    # Never follow a patch record left by another image or patch set.
    retry_busy(lambda: session.load(wanted))
    return {"path": "boot", "reason": "restored this preview's runtime image"}


def session_on(emu: Emu, cell: H.Cell, run_dir: Path, dsp: str = "sharc") -> H.Session:
    session = H.Session(
        cell, H.Goldens(), cache_dir=H.SHARED_CACHE, run_dir=run_dir, dsp=dsp
    )
    session.emu = emu
    card = run_dir / "card.txt"
    session.card = card.read_text() if card.is_file() else None
    return session


def stop(
    device: str, target: str, store: list[str] | None = None, trace: bool = False
) -> None:
    stopped = False
    for dsp in ("zero", "sharc", None):
        run_dir = run_dir_of(device, target, store, trace, dsp)
        emu = connect(run_dir)
        if emu is None:
            continue
        with contextlib.suppress(OSError, EmuError):
            emu.quit()
        emu.close()
        stopped = True
        print(f"{device}: {dsp or 'legacy'} daemon stopped")
    if not stopped:
        print(f"{device}: no daemon for {target}")


def run_device(device: str, a: argparse.Namespace) -> dict[str, Any]:
    modules = resolve_modules(a.target, device)
    for m in modules:
        modroots.module_dir(m)
    store = [m for m in (a.store or "").split(",") if m]
    ui_preview = a.store_ui_preview
    dsp = (
        store_ui_preview_mode(a.target, device, store)
        if ui_preview
        else target_dsp_mode(a.target, device, store)
    )
    run_dir = run_dir_of(device, a.target, store, a.trace, dsp)
    with session_lock(run_dir):
        return run_device_locked(device, a, modules, store, dsp, run_dir)


def run_device_locked(
    device: str,
    a: argparse.Namespace,
    modules: list[str],
    store: list[str],
    dsp: str,
    run_dir: Path,
) -> dict[str, Any]:
    t_all = time.monotonic()
    cell = H.Cell(device, FIRMWARE[device], modules)
    ui_preview = a.store_ui_preview
    t_build, rebuilt = build(closure(modules))
    print(f"{device}: auto DSP {dsp}", file=sys.stderr, flush=True)
    t_reload_started = time.monotonic()
    emu = connect(run_dir)
    prepared_card = None
    runtime_report = None
    if emu is not None and getattr(a, "reuse", False):
        try:
            assert_mode(emu, dsp)
            runtime_report = preview_reload(
                emu, session_on(emu, cell, run_dir, dsp), modules, False
            )
        except BaseException:
            release(emu)
            raise
    baseline = run_dir / "store-request.json"
    before_store = baseline.read_bytes() if baseline.is_file() else None
    if store:
        try:
            emu, prepared_card = refresh_store(
                emu,
                device,
                a.target,
                store,
                run_dir,
                a.trace,
                ui_preview,
                **({"live_update": True} if getattr(a, "reuse", False) else {}),
            )
        except BaseException:
            if emu is not None:
                release(emu)
            raise
    started = False
    t_start = 0.0
    if emu is None:
        t = time.monotonic()
        emu, session = start(
            cell,
            run_dir,
            store,
            a.trace,
            dsp,
            store_ui_preview=ui_preview,
            **({"reuse_runtime": True} if getattr(a, "reuse", False) else {}),
            **({"prepared_card": prepared_card} if prepared_card is not None else {}),
        )
        t_start, started = time.monotonic() - t, True
        runtime_report = None
    else:
        session = session_on(emu, cell, run_dir, dsp)
        if prepared_card is not None and not session.card:
            session.card = str(prepared_card)
    try:
        assert_mode(emu, dsp)
        # auto_no: the DT2's FILE SYSTEM OK modal is closed once the state is restored, as the harness does after a restart.
        report = (
            (runtime_report or preview_reload(emu, session, modules, started))
            if getattr(a, "reuse", False)
            else reload_ready(emu, session)
        )
        if (
            not started
            and runtime_report is not None
            and baseline.is_file()
            and baseline.read_bytes() != before_store
        ):
            report = {
                "path": "restart",
                "reason": "module uploaded; guest restarted on mounted card",
            }
        assert_mode(emu, dsp)
        t_reload = time.monotonic() - t_reload_started - t_start
        out = Path(a.out) / device
        out.mkdir(parents=True, exist_ok=True)
        t = time.monotonic()
        frames: list[tuple[str, bytes]] = []
        if getattr(a, "reuse", False):
            print("preview: navigating to module", flush=True)
        session.run_ms(300)
        if dsp == "sharc":
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
        "dsp": dsp,
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
        f"{r['device']}: DSP {r['dsp']} (auto); {path}{detail}\n"
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
        help="module ids separated by commas: packed into the module store of the card the daemon boots with; an id ending in * is also in the saved selection, synth:NAME:BSS is a module that needs BSS bytes of arena, file:SRC=DEST puts a host file or folder at digikit/DEST of the store, e.g. out/smp=samples (unchanged content keeps its card; changed content restarts its daemon)",
    )
    ap.add_argument(
        "--trace",
        action="store_true",
        help="build the --store modules with DK_TRACE (patchkit pack --trace): their DK_TRACE_LOG and DK_STEP marks exist; the kept daemon is its own (the card differs)",
    )
    ap.add_argument("--reuse", action="store_true", help=argparse.SUPPRESS)
    ap.add_argument("--store-ui-preview", action="store_true", help=argparse.SUPPRESS)
    ap.add_argument("--out", default=str(ROOT / "out" / "runs" / "devreload"))
    ap.add_argument(
        "--stop", action="store_true", help="end the kept daemons of this module set"
    )
    ap.add_argument("--json", action="store_true", help="print the reports as JSON")
    a = ap.parse_args(argv)
    if a.store_ui_preview and a.trace:
        ap.error("--store-ui-preview cannot be combined with --trace")
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
