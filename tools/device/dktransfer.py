#!/usr/bin/env python3
"""Transfer: the repo's run-time modules on a target (emulator or device), and install of a chosen set.

This is the command line for scripts and the scenarios. The desktop workbench does not use it: its Transfer page speaks the same protocol in
TypeScript (packages/dk-session/src/dkupload-core.ts `DkSession`, dktransfer.py's port in apps/web/src/modules/dktransfer.ts) and reads
built modules from out/modules/<device>/ (`mise run module:pack-all`, tools/device/dkpackall.py). Python is for reverse engineering and
dev tooling; the app starts no process.

    uv run python -m tools.device.dktransfer list --emu [--json]
    uv run python -m tools.device.dktransfer install <author/name> [MOD ...] --emu [--select [--keep PATH ...]] [--restart] [--json]
    uv run python -m tools.device.dktransfer restart --emu [--json]
    uv run python -m tools.device.dktransfer delete modules/<name>.dkm [PATH ...] --emu [--json]
    mise run transfer -- list --emu --json
    mise run transfer -- install <author/name> --port Digitakt

`list` is one merged view for the target's device (INFO): the repo's run-time modules that pack for it (never `core/*`; `test/` ones are
marked), with group (machine, fx or service contract in the packed header: Machines, Effects, Tools), version, sizes, build state
(`unchanged` = the built objects carry the hash of their sources; `needs build`), whether the file is on the card (LIST, same file hash),
whether it is loaded (MODULES), and whether it fits (CAPACITY; a warning only). Card files that match no repo module are listed with
their group, read from the 0xa0-byte header (READ, runtime 0.2.4).

`install MOD...` (it emits `progress` events with the bytes sent of each upload, at most about ten per file) builds only the modules whose objects changed, packs them, uploads the files the card does not have (same file hash),
VERIFYs each file on the card (the device hashes it from a fresh read). That is all: the device's selection is not touched, because
the selection belongs to the device (the user ticks the module in SETTINGS > DIGIKIT > MODULES and applies and restarts there).
`--select` also sends SELECT, which replaces the selection by exactly these modules plus the `--keep` paths; `--restart` implies `--select`,
sends RESTART, waits for the boot and checks that MODULES lists the modules (scripts, the scenarios). `restart [MOD...]` sends RESTART,
waits for the boot and reports which modules MODULES lists. `delete PATH...` sends one DELETE per path and confirms with LIST that each is gone (a path that is not on the card is an error before anything
is sent); the device's selection is not touched. `--json` prints one JSON object per line (`{"event": ...}`); `list --json` prints one JSON document.

Target: `--emu` (the running emulator; `--run-dir DIR` or `--socket PATH` pick another one) or `--port NAME` (python-rtmidi). A running
emulator is driven in real time. A paused emulator is refused (nothing would step it): resume it, or pass `--step` to let this tool step it
(a daemon of `mise run emu:serve`, the test rigs).
"""

from __future__ import annotations

import argparse
import json
import os
import struct
import sys
import time
import tomllib
from collections.abc import Callable
from pathlib import Path
from typing import Any

from tools.device import dkneeds as N
from tools.device import dkrun as R
from tools.device import dkupload as D
from tools.patchkit import modroots

MAGIC = b"DKMF"
GROUPS = ("Machines", "Effects", "Tools")
SELECTION_MAX = 8


class TransferError(RuntimeError):
    pass


# ---- the .dkm header ----------------------------------------------------------------------------------------------------------


def header_info(h: bytes) -> dict[str, Any] | None:
    """The fields of a .dkm header (docs/design/digikit-dkm.md) that `list` shows; None when `h` is not one."""
    if len(h) < D.HEADER_BYTES or h[:4] != MAGIC:
        return None
    size, _hdr = struct.unpack_from(">II", h, 8)
    version = struct.unpack_from(">I", h, 0x34)[0]
    contracts = {
        k: (h[0x38 + 2 * i], h[0x39 + 2 * i])
        for i, k in enumerate(("api", "machine", "fx", "service"))
    }
    image, bss = struct.unpack_from(">II", h, 0x64)
    dsp = struct.unpack_from(">I", h, 0x9C)[0]
    return {
        "name": h[0x40:0x60].split(b"\0")[0].decode("ascii", "replace"),
        "version": D.fmt_runtime(version),
        "contracts": contracts,
        "group": group_of(contracts),
        "size": size,
        "arena": image + bss,
        "dsp": dsp,
    }


def group_of(contracts: dict[str, tuple[int, int]]) -> str:
    """Machine contract: Machines; fx: Effects; service or api only: Tools."""
    if contracts["machine"][0]:
        return "Machines"
    if contracts["fx"][0]:
        return "Effects"
    return "Tools"


# ---- the repo's modules ---------------------------------------------------------------------------------------------------------


def candidates() -> list[str]:
    """Module ids that can be run-time modules: not `core/*`, not a framework, and `dynamic`, a raw ColdFire module or a service module
    (`type = "service"`, a .dkm with the service contract, like digikit/remote). Machines and effects that are patched in (not `dynamic`)
    and the effects without a ColdFire object do not pack to a .dkm: they stay out. The packer decides the rest (`repo_rows`)."""
    out = []
    for mid, d in sorted(modroots.manifests().items()):
        m = tomllib.loads((d / "module.toml").read_text())["module"]
        if mid.startswith("core/") or m.get("type") == "framework":
            continue
        if m.get("dynamic") or m.get("type") in ("raw", "service"):
            out.append(mid)
    return out


def inputs_mtime(moddir: Path) -> float:
    """The newest modification time among everything a pack of the module reads: its directory, its built objects, the patchkit sources, the
    device files and the SDK numbers."""
    newest = 0.0
    roots = [
        (moddir, "*"),
        (R.ROOT / "packages" / "patchkit" / "src", "*.rs"),
        (R.ROOT / "devices", "*.toml"),
    ]
    mid = modroots.module_id(moddir)
    if mid:  # the built objects of a content module are cached outside its directory
        roots.append((R.ROOT / "out" / "cache" / "dko" / mid, "*"))
    for root, pat in roots:
        for f in root.rglob(pat):
            if f.is_file() and "__pycache__" not in f.parts:
                newest = max(newest, f.stat().st_mtime)
    return max(newest, (R.ROOT / "sdk" / "sdk.toml").stat().st_mtime)


def pack_for_transfer(module: str, device: str) -> Path:
    """Pack into out/patch/transfer/<device>/, so a list or install does not rewrite the files `dk run` leaves in out/patch. A packed file
    newer than everything the pack reads is reused (a pack costs about 0.25 s; a list packs every candidate). A module that did not pack
    is remembered the same way (`_failed.json`), so a list does not try it again until something it reads changes.
    `DIGI_TRANSFER_DIR` replaces out/patch/transfer: out/ is shared by all worktrees and runs, and the reuse rule compares file times,
    so a test run keeps its own folder."""
    name = module.rsplit("/", 1)[-1]
    base = os.environ.get("DIGI_TRANSFER_DIR")
    folder = (Path(base) if base else R.ROOT / "out" / "patch" / "transfer") / device
    out = folder / f"{name}.dkm"
    newest = inputs_mtime(modroots.module_dir(module))
    failed_file = folder / "_failed.json"
    try:
        failed = json.loads(failed_file.read_text())
    except (OSError, ValueError):
        failed = {}
    if out.is_file() and out.stat().st_mtime > newest:
        return out
    if failed.get(module, [0, ""])[0] > newest:
        raise R.RunError(failed[module][1])
    try:
        return R.pack_module(module, device, out)
    except R.RunError as e:
        failed[module] = [time.time(), str(e)]
        folder.mkdir(parents=True, exist_ok=True)
        failed_file.write_text(json.dumps(failed))
        raise


def repo_rows(
    device: str,
    ids: list[str] | None = None,
    pack: Callable[[str, str], Path] = pack_for_transfer,
    status: Callable[[str], str] | None = None,
) -> list[dict[str, Any]]:
    """The repo modules that pack for DEVICE (a dry-run pack of each: the packer is the truth, `ports` is not), one dict per module with its
    packed file (`dkm`), header fields and build state. A module that does not pack for the device is left out."""
    from tools.patchkit import patchkit

    status = status or (lambda mid: patchkit.module_status(modroots.module_dir(mid)))
    rows = []
    for mid in ids if ids is not None else candidates():
        try:
            dkm = pack(mid, device)
        except R.RunError:
            continue
        data = dkm.read_bytes()
        h = header_info(data[: D.HEADER_BYTES])
        if h is None:
            continue
        # what a load takes, inferred from the file (`patchkit needs`); a file the loader would refuse has none and no verdict
        try:
            h["needs"] = N.needs_of(data)
            h["arena"], h["dsp"] = h["needs"]["ARENA"], h["needs"]["DSP_AREA"]
        except N.NeedsError:
            pass
        m = tomllib.loads((modroots.module_dir(mid) / "module.toml").read_text())[
            "module"
        ]
        rows.append(
            {
                **h,
                "id": mid,
                "label": m.get("name", mid),
                "test": mid.startswith("test/"),
                "path": R.module_path(mid),
                "hash": D.file_hash(data).hex(),
                "file_size": len(data),
                "build": status(mid),
                "dkm": str(dkm),
            }
        )
    return rows


# ---- the merged view ----------------------------------------------------------------------------------------------------------


def fit_warnings(
    row: dict[str, Any], cap: dict[str, dict[str, int]] | None
) -> list[str]:
    """What would not fit on the target (CAPACITY): none when the module fits next to what is loaded now (the fit rule, `dkneeds.fit`), else one line
    that says what is short and whether the module fits the device when the loaded ones are left out (the next boot loads the selection into an
    empty runtime). Only a warning: the device refuses a load that does not fit."""
    if cap is None or row["loaded"] or "needs" not in row:
        return []
    short = N.fit(cap, [row["needs"]])["each"][0]
    if short is None:
        return []
    alone = N.fit(cap, [row["needs"]], empty=True)["each"][0] is None
    why = (
        "fits when the modules loaded now are not selected"
        if alone
        else "more than this device has"
    )
    return [f"{short['describe']}; {why}"]


def merge(
    rows: list[dict[str, Any]],
    card: list[tuple[str, int, str]],
    loaded: list[str],
    cap: dict[str, dict[str, int]] | None,
    headers: dict[str, bytes | None],
) -> dict[str, Any]:
    """The `list` result from its parts: the repo rows (`repo_rows`), the card's LIST, the names MODULES reports loaded, CAPACITY (or None),
    and the header bytes of the card files (`headers`: path -> 0xa0 bytes, None when unreadable)."""
    on_card = {p: (n, h) for p, n, h in card}
    mine = []
    seen = set()
    for r in rows:
        r = dict(r)
        have = on_card.get(r["path"])
        seen.add(r["path"])
        r["on_card"] = (
            "no"
            if have is None
            else "yes"
            if have == (r["file_size"], r["hash"])
            else "other version"
        )
        r["loaded"] = r["name"] in loaded
        r["warnings"] = fit_warnings(r, cap)
        r["fits"] = None if cap is None else not r["warnings"]
        mine.append(r)
    mine.sort(key=lambda r: (GROUPS.index(r["group"]), r["test"], r["id"]))
    extra = []
    for path, size, _sha in card:
        if path in seen:
            continue
        h = header_info(headers.get(path) or b"")
        name = h["name"] if h else None
        extra.append(
            {
                "id": None,
                "path": path,
                "name": name or path.rsplit("/", 1)[-1],
                "group": h["group"] if h else "Tools",
                "known": h is not None,
                "version": h["version"] if h else None,
                "file_size": size,
                "test": False,
                "on_card": "yes",
                "loaded": name in loaded if name else False,
            }
        )
    return {"modules": mine, "card_only": extra}


def card_headers(
    up: D.Uploader, card: list[tuple[str, int, str]], skip: set[str]
) -> dict[str, bytes | None]:
    out: dict[str, bytes | None] = {}
    for path, _size, _sha in card:
        if path in skip:
            continue
        try:
            out[path] = up.header(path)
        except D.UploadError:
            out[path] = None
    return out


def list_view(
    up: D.Uploader, rows: list[dict[str, Any]] | None = None
) -> dict[str, Any]:
    """`list` against a connected uploader."""
    info = up.info()
    R.require_runtime(info)
    device = info["port"]
    if rows is None:
        rows = repo_rows(device)
    card = up.ls()
    loaded = (
        [m["name"] for m in up.modules()["modules"]]
        if D.has_cap(info, "MODULES")
        else []
    )
    cap = up.capacity() if D.has_cap(info, "CAPACITY") else None
    mine = {r["path"] for r in rows}
    headers = card_headers(up, card, mine) if D.has_cap(info, "READ") else {}
    out = merge(rows, card, loaded, cap, headers)
    out.update(
        device=device,
        runtime=D.fmt_runtime(info["runtime"]),
        free_bytes=info["free_bytes"],
        loaded=loaded,
    )
    return out


# ---- install ------------------------------------------------------------------------------------------------------------------


def plan_install(
    rows: list[dict[str, Any]],
    wanted: list[str],
    card: list[tuple[str, int, str]],
    keep: list[str] | None = None,
    selecting: bool = True,
) -> dict[str, Any]:
    """What an install does: `build` (ids whose objects need a build), `upload` (ids whose packed file the card lacks, by sha256 -- decided after
    the pack: here by the rows' own hash, which a build can change), `select` (the paths: the wanted modules, then `keep`)."""
    by_id = {r["id"]: r for r in rows}
    missing = [m for m in wanted if m not in by_id]
    if missing:
        raise TransferError(
            f"not a run-time module that packs for this device: {', '.join(missing)}"
        )
    on_card = {p: (n, h) for p, n, h in card}
    select = []
    for m in wanted:
        select.append(by_id[m]["path"])
    for p in keep or []:
        if p not in select:
            select.append(p)
    if selecting and len(select) > SELECTION_MAX:
        raise TransferError(
            f"a selection holds at most {SELECTION_MAX} modules, this one has {len(select)}"
        )
    return {
        "build": [m for m in wanted if by_id[m]["build"] != "unchanged"],
        "upload": [
            m
            for m in wanted
            if on_card.get(by_id[m]["path"])
            != (by_id[m]["file_size"], by_id[m]["hash"])
        ],
        "select": select,
    }


def wait_loaded(
    up: D.Uploader,
    names: list[str],
    sleep: Callable[[float], None],
    now: Callable[[], float],
    timeout_s: float = 60.0,
    paths: list[str] | None = None,
) -> list[str]:
    """Poll MODULES until every name is listed; returns the loaded names. Raises TransferError at the timeout, saying what is
    wrong when it can tell: files that are not on the card after the restart (the restart lost the card's writes), or on the card
    but not loaded (see the log)."""
    end = now() + timeout_s
    got: list[str] = []
    while True:
        try:
            got = [m["name"] for m in up.modules()["modules"]]
        except (TimeoutError, D.UploadError):
            got = []
        if all(n in got for n in names):
            return got
        if now() >= end:
            miss = [n for n in names if n not in got]
            why = "see the log: dk:log"
            try:
                on_card = {p for p, _, _ in up.ls()}
                lost = [p for p in paths or [] if p not in on_card]
                if lost:
                    why = (
                        f"the card no longer holds {', '.join(lost)}: the restart lost the card's writes "
                        "(an emulator started without a card file needs the soft-reset fix)"
                    )
                elif paths:
                    why = "the files are on the card but did not load: see the log, dk:log"
            except (TimeoutError, D.UploadError):
                pass
            raise TransferError(
                f"MODULES does not list {', '.join(miss)} after the restart ({why})"
            )
        sleep(0.5)


def poll_loaded(
    up: D.Uploader,
    names: list[str],
    sleep: Callable[[float], None],
    now: Callable[[], float],
    timeout_s: float = 20.0,
) -> list[str]:
    """MODULES' names after a restart: polled until every name is listed or the timeout; returns what is listed (no error)."""
    end = now() + timeout_s
    got: list[str] = []
    while True:
        try:
            got = [m["name"] for m in up.modules()["modules"]]
        except (TimeoutError, D.UploadError):
            got = []
        if all(n in got for n in names) or now() >= end:
            return got
        sleep(0.5)


def restart_device(
    up: D.Uploader,
    names: list[str],
    emit: Callable[[dict[str, Any]], None] = lambda e: None,
    connect: Callable[[], D.Uploader] | None = None,
    wait_boot: Callable[[], None] | None = None,
    sleep: Callable[[float], None] = time.sleep,
    now: Callable[[], float] = time.monotonic,
    paths: list[str] | None = None,
) -> dict[str, Any]:
    """RESTART, wait for the new boot, then report which of NAMES MODULES lists. `missing` names the others, with the reason when the
    store can tell (the file is not on the card)."""
    quiet = lambda _m: None  # noqa: E731
    emit({"event": "restart"})
    up = R.restart_and_wait(up, connect, wait_boot, sleep, now, quiet)
    emit({"event": "wait", "modules": names})
    got = poll_loaded(up, names, sleep, now)
    missing = [n for n in names if n not in got]
    out: dict[str, Any] = {"loaded": got, "missing": missing}
    if missing:
        try:
            on_card = {p for p, _, _ in up.ls()}
            out["not_on_card"] = [p for p in paths or [] if p not in on_card]
        except (TimeoutError, D.UploadError):
            pass
    emit({"event": "done", "restarted": True, **out})
    return out


PROGRESS_EVENTS = (
    10  # `progress` events per uploaded file, at most (plus the last one at 100 %)
)


def upload_progress(
    emit: Callable[[dict[str, Any]], None], mid: str, total: int
) -> Callable[[int, int], None]:
    """The `progress` callback of one file's upload: emits `{"event": "progress", "id", "sent", "bytes"}` when at least a tenth of the file
    went since the last one, and at 100 %; not once per DATA chunk (a 64 KiB file is two chunks, but a slow link must not flood the page)."""
    last = 0
    step = max(1, total // PROGRESS_EVENTS)

    def cb(sent: int, size: int) -> None:
        nonlocal last
        if sent >= size or sent - last >= step:
            last = sent
            emit({"event": "progress", "id": mid, "sent": sent, "bytes": size})

    return cb


def install(
    up: D.Uploader,
    wanted: list[str],
    keep: list[str] | None = None,
    emit: Callable[[dict[str, Any]], None] = lambda e: None,
    connect: Callable[[], D.Uploader] | None = None,
    wait_boot: Callable[[], None] | None = None,
    sleep: Callable[[float], None] = time.sleep,
    now: Callable[[], float] = time.monotonic,
    rows: list[dict[str, Any]] | None = None,
    build: Callable[[str], None] = R.build_objects,
    pack: Callable[[str, str], Path] = pack_for_transfer,
    restart: bool = False,
    select: bool = False,
) -> dict[str, Any]:
    """For each of WANTED, in order: build when its objects changed, pack, upload unless the card has the same sha256, VERIFY (the device
    reads the file back from the card and hashes it; a mismatch is an error). That is all by default: the device's selection is left as it
    is. With `select` (implied by `restart`) one SELECT of the wanted modules and the `keep` paths follows (it replaces the selection);
    with `restart` a RESTART and the wait for MODULES (`restart_device`). `selected` in the result and in the `done` event is the list of
    paths selected (empty when nothing was). EMIT gets one dict per step; a module's steps end with `verify` (ok true or false)."""
    quiet = lambda _m: None  # noqa: E731
    info = up.info()
    R.require_runtime(info)
    device = info["port"]
    emit({"event": "start", "device": device, "modules": wanted})
    if rows is None:
        rows = repo_rows(device, pack=pack, ids=wanted)
    plan = plan_install(rows, wanted, up.ls(), keep, select or restart)
    by_id = {r["id"]: r for r in rows}
    have = {p: (n, h) for p, n, h in up.ls()}
    can_verify = D.has_cap(info, "VERIFY")
    uploaded = []
    for mid in wanted:
        if mid in plan["build"]:
            emit({"event": "build", "id": mid})
            build(mid)
            by_id[mid] = repo_rows(device, [mid], pack)[0]  # the packed file changed
        r = by_id[mid]
        data = Path(r["dkm"]).read_bytes()
        sha = D.file_hash(data).hex()
        emit({"event": "upload", "id": mid, "path": r["path"], "bytes": len(data)})
        if R.upload_if_changed(
            up, r["path"], data, have, quiet, upload_progress(emit, mid, len(data))
        ):
            uploaded.append(mid)
        else:
            emit({"event": "skip", "id": mid, "reason": "same hash on the card"})
        if can_verify:
            got = up.verify(r["path"])
            ok = got == sha
            emit({"event": "verify", "id": mid, "path": r["path"], "ok": ok})
            if not ok:
                raise TransferError(
                    f"{r['path']}: the device reads back hash {got[:8]}, expected {sha[:8]}: the card did not keep the file"
                )
    selected: list[str] = []
    if select or restart:
        selected = plan["select"]
        emit({"event": "select", "paths": selected})
        up.select(selected)
    out: dict[str, Any] = {
        "built": plan["build"],
        "uploaded": uploaded,
        "selected": selected,
        "restarted": False,
    }
    if not restart:
        emit({"event": "done", "restarted": False, "selected": selected})
        return out
    names = [by_id[m]["name"] for m in wanted]
    res = restart_device(up, names, emit, connect, wait_boot, sleep, now, selected)
    if res["missing"]:
        lost = res.get("not_on_card")
        why = (
            f"the card no longer holds {', '.join(lost)}: the restart lost the card's writes "
            "(an emulator started without a card file needs the soft-reset fix)"
            if lost
            else "the files are on the card but did not load: see the log, dk:log"
        )
        raise TransferError(
            f"MODULES does not list {', '.join(res['missing'])} after the restart ({why})"
        )
    return {**out, "restarted": True, "loaded": res["loaded"]}


def delete(
    up: D.Uploader,
    paths: list[str],
    emit: Callable[[dict[str, Any]], None] = lambda e: None,
) -> dict[str, Any]:
    """DELETE each path from the card (one request each: the messages are tiny), then LIST to confirm that each is gone. A path that is
    not on the card is an error, reported before anything is sent. The device's selection is not touched: a deleted file that was
    selected or loaded is skipped by the next boot (a MOD_LOAD error line, E_NOT_FOUND), and a loaded module stays loaded until then."""
    info = up.info()
    R.require_runtime(info)
    paths = list(dict.fromkeys(paths))
    on_card = {p for p, _, _ in up.ls()}
    missing = [p for p in paths if p not in on_card]
    if missing:
        raise TransferError(f"not on the card: {', '.join(missing)}")
    emit({"event": "start", "device": info["port"], "paths": paths})
    for path in paths:
        emit({"event": "delete", "path": path})
        up.rm(path)
    left = {p for p, _, _ in up.ls()}
    still = [p for p in paths if p in left]
    if still:
        raise TransferError(f"still on the card after DELETE: {', '.join(still)}")
    emit({"event": "done", "deleted": paths})
    return {"deleted": paths}


# ---- the command line ---------------------------------------------------------------------------------------------------------


class LiveTransport(D.EmuTransport):
    """A running emulator: replies are polled in real time, nothing steps the guest."""

    def recv(self, timeout_s: float = 5.0) -> bytes:
        end = time.monotonic() + timeout_s
        while True:
            self.buf += self.emu.usb_out()
            self.ready += [m for m in D.split_messages(self.buf) if D.parse_reply(m)]
            if self.ready:
                return self.ready.pop(0)
            if time.monotonic() >= end:
                raise TimeoutError("no reply from the emulated device")
            time.sleep(0.005)


def connect_target(args: argparse.Namespace) -> tuple[D.Uploader, dict[str, Any]]:
    """The uploader and the install wiring (`connect`, `wait_boot`, `sleep`, `now`) for the target of the command line."""
    if args.emu:
        emu = D._emu(args.run_dir) if not args.socket else _emu_at(args.socket)
        mode = emu.status().get("mode")
        if mode == "paused" and not args.step:
            raise TransferError(
                "the emulator is paused: nothing would run the transfer. Resume it (the Run button of the workbench), "
                "or pass --step to let this tool step it"
            )
        if args.step:
            emu.subscribe()
            steps = {"n": 0}

            def sleep(s: float) -> None:
                n = int(s * R.EMU_IPS)
                emu.step(n)
                steps["n"] += n

            wiring = {
                "connect": None,
                "wait_boot": R.emu_boot_waiter(emu),
                "sleep": sleep,
                "now": lambda: steps["n"] / R.EMU_IPS,
            }
            return D.Uploader(D.EmuTransport(emu)), wiring
        wiring = {
            "connect": None,
            "wait_boot": None,
            "sleep": time.sleep,
            "now": time.monotonic,
        }
        return D.Uploader(LiveTransport(emu)), wiring
    port = args.port
    wiring = {
        "connect": lambda: D.Uploader(D.RtMidiTransport(port)),
        "wait_boot": None,
        "sleep": time.sleep,
        "now": time.monotonic,
    }
    return D.Uploader(D.RtMidiTransport(port)), wiring


def _emu_at(socket_path: str) -> Any:
    from tools.control.digi_client import Emu

    return Emu.connect(socket_path=socket_path)


def text_row(r: dict[str, Any]) -> str:
    flags = [f"card {r['on_card']}"]
    if r["loaded"]:
        flags.append("loaded")
    if r.get("build"):
        flags.append(r["build"])
    if r.get("warnings"):
        flags.append("WARNING " + "; ".join(r["warnings"]))
    size = f"{r['file_size']} B" + (
        f" (arena {r['arena']}, dsp {r['dsp']})" if "arena" in r else ""
    )
    name = r["id"] or r["path"]
    return f"  {name:<28} {r.get('version') or '-':<8} {size:<34} {', '.join(flags)}"


def print_list(v: dict[str, Any]) -> None:
    print(f"{v['device']}, runtime {v['runtime']}, free {v['free_bytes']} bytes")
    for g in GROUPS:
        rows = [r for r in v["modules"] if r["group"] == g]
        print(f"{g}:" + ("" if rows else "  (none)"))
        for r in rows:
            print(text_row(r) + ("  [test]" if r["test"] else ""))
    if v["card_only"]:
        print("On the card, not from this repo:")
        for r in v["card_only"]:
            print(text_row(r) + f"  [{r['group']}]")


def main(argv: list[str] | None = None) -> int:
    ap = argparse.ArgumentParser(
        description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter
    )
    where = ap.add_mutually_exclusive_group(required=True)
    where.add_argument("--emu", action="store_true", help="the running emulator")
    where.add_argument("--port", help="MIDI port name substring (a real device)")
    ap.add_argument("--run-dir", type=Path, help="with --emu: a run directory")
    ap.add_argument("--socket", help="with --emu: the control socket path")
    ap.add_argument(
        "--step",
        action="store_true",
        help="with --emu: step the emulator from here (a paused daemon); default: real time, a paused emulator is refused",
    )
    ap.add_argument("--json", action="store_true", help="machine-readable output")
    sub = ap.add_subparsers(dest="cmd", required=True)
    sub.add_parser("list")
    p = sub.add_parser("install")
    p.add_argument("modules", nargs="+", help="module ids, e.g. <author/name>")
    p.add_argument(
        "--select",
        action="store_true",
        help="also SELECT these modules (replaces the device's selection; default: the selection is left alone)",
    )
    p.add_argument(
        "--restart",
        action="store_true",
        help="implies --select; then RESTART and wait until MODULES lists the modules",
    )
    p.add_argument(
        "--keep",
        nargs="*",
        default=[],
        metavar="PATH",
        help="with --select: store paths of loaded modules to keep selected",
    )
    p = sub.add_parser(
        "delete", help="delete files from the card (the selection is not touched)"
    )
    p.add_argument("paths", nargs="+", help="store paths, e.g. modules/<name>.dkm")
    p = sub.add_parser(
        "restart", help="RESTART, wait for the boot, report which modules loaded"
    )
    p.add_argument(
        "modules", nargs="*", help="module ids that should be listed after the boot"
    )
    args = ap.parse_args(argv)

    def emit(e: dict[str, Any]) -> None:
        if args.json:
            print(json.dumps(e), flush=True)
        else:
            rest = {k: v for k, v in e.items() if k != "event"}
            print(
                f"transfer: {e['event']} {json.dumps(rest) if rest else ''}".rstrip(),
                flush=True,
            )

    try:
        up, wiring = connect_target(args)
        if args.cmd == "list":
            v = list_view(up)
            if args.json:
                print(json.dumps(v))
            else:
                print_list(v)
        elif args.cmd == "delete":
            out = delete(up, args.paths, emit)
            if args.json:
                print(json.dumps({"event": "result", **out}), flush=True)
        elif args.cmd == "restart":
            out = restart_device(
                up,
                args.modules,
                emit,
                paths=[R.module_path(m) for m in args.modules],
                **wiring,
            )
            if args.json:
                print(json.dumps({"event": "result", **out}), flush=True)
        else:
            out = install(
                up,
                args.modules,
                args.keep,
                emit,
                restart=args.restart,
                select=args.select,
                **wiring,
            )
            if args.json:
                print(json.dumps({"event": "result", **out}), flush=True)
    except (TransferError, R.RunError, D.UploadError, TimeoutError, OSError) as e:
        if args.json:
            print(json.dumps({"event": "error", "message": str(e)}), flush=True)
        else:
            print(f"error: {e}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
