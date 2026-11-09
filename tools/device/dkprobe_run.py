"""`dkprobe run`: one command from a clean device to a probe capture and back (docs/plans/debug-mode-and-probes.md).

    uv run --with python-rtmidi python -m tools.device.dkprobe run --port Digitakt --modules test/dsp-facts --capture out/probe/facts.json
    uv run python -m tools.device.dkprobe run --emu --modules test/sharc-probe --capture OUT.json --compare REF.json [--keep]
    mise run dk:probe -- --port Digitakt --modules test/dsp-facts --capture out/probe/facts.json

Steps, one line each: INFO (the device id picks the pack target), the modules loaded now (the selection to restore), pack (`patchkit pack`) of the given
modules, `digikit/remote`, `digikit/debug` and `digikit/probe`, upload of the files whose size or file hash differ from LIST, SELECT (remote, debug, the modules, the collector),
RESTART, the wait for the new boot, the boot's MOD_LOAD and MOD_DSP results and the loaded modules, the fault check, the capture
(`dkprobe capture`), the fault check again, the comparison with `--compare`, and the restore: SELECT of the previous modules and RESTART, unless `--keep`.
The restore also runs after a failure, once the new selection is saved.

The selection is read back from the loaded modules (the device has no command for the selection record): each loaded module is selected again as
`modules/<last part of its name>.dkm`, which is how every `dk run` and this tool store them. A module that is not in the store any more cannot be
restored; the tool says so. The sequence (`--seq sharc|facts`) follows the probe module by default.

The exit status is 0 when every item was captured (`usable`: a lost word spoils one frame of one run and is only a warning) and the device shows no fault; a comparison that finds differences prints them and does not change it.
"""

from __future__ import annotations

import json
import sys
import time
from collections.abc import Callable
from pathlib import Path
from typing import Any

from tools.device import dklog as L
from tools.device import dkprobe as P
from tools.device import dkprobe_facts as F
from tools.device import dkprobe_seq as S
from tools.device import dkrun as R
from tools.device import dkupload as D

REMOTE = "digikit/remote"
COLLECTOR = "digikit/probe"
DEBUG = "digikit/debug"  # the log (the boot wait and the fault check read it) exists only while it is loaded
# the sequence a probe module speaks
SEQUENCES: dict[str, Any] = {"sharc": S, "facts": F}
MODULE_SEQ = {"test/sharc-probe": "sharc", "test/dsp-facts": "facts"}
FAULT_KINDS = (
    "FAULT_FOUND",
    "CRASH_FOUND",
    "SAFE",
    "DSP_STOPPED",
    "DSP_ABSENT",
    "FX_FAULT",
    "FX_DISABLED",
    "MACHINE_FAULT",
    "TABLE_BAD",
)


class ProbeRunError(RuntimeError):
    pass


def say(msg: str) -> None:
    print(f"dkprobe run: {msg}", flush=True)


def selection_paths(modules: list[str]) -> list[str]:
    """The selection of one run: remote first, the modules, the collector last (the order of the regression test)."""
    out: list[str] = []
    for m in [REMOTE, DEBUG, *modules, COLLECTOR]:
        if m not in out:
            out.append(m)
    return [R.module_path(m) for m in out]


def sequence_for(modules: list[str], name: str | None) -> Any:
    if name:
        return SEQUENCES[name]
    for m in modules:
        if m in MODULE_SEQ:
            return SEQUENCES[MODULE_SEQ[m]]
    raise ProbeRunError(
        f"no probe module among {modules}: give one of {sorted(MODULE_SEQ)} or --seq"
    )


def previous_selection(
    up: Any, store: dict[str, tuple[int, str]], log: Callable[[str], None]
) -> list[str]:
    """The store paths of the modules loaded now, in load order; the ones the store lacks are named and left out."""
    names = [m["name"] for m in up.modules().get("modules", [])]
    paths = [R.module_path(n) for n in names]
    lost = [p for p in paths if p not in store]
    if lost:
        log(f"WARNING loaded now but not in the store, so not restored: {lost}")
    keep = [p for p in paths if p in store]
    log(f"loaded now: {', '.join(names) or 'nothing'} (selected again at the end)")
    return keep


def fault_lines(up: Any) -> list[str]:
    """What the log and INFO say about faults since the boot: one line each."""
    out: list[str] = []
    info = up.info()
    if info.get("safe"):
        out.append("the device is in safe mode")
    if info.get("crash_found"):
        out.append("INFO reports a crash record")
    _, _, evs = L.read_all(up)
    for e in evs:
        if e[2] in {L.K[k] for k in FAULT_KINDS}:
            out.append(L.text_of(L.event_dict(e)).strip())
    return out


def check_loaded(up: Any, wanted: list[str]) -> list[str]:
    """The loaded modules: every wanted name present, no fault counted by the guard. Returns the names."""
    mods = up.modules().get("modules", [])
    names = [m["name"] for m in mods]
    missing = [w for w in wanted if w not in names]
    if missing:
        raise ProbeRunError(f"not loaded after the boot: {missing} (loaded: {names})")
    bad = [m["name"] for m in mods if m["faults"] or m["guard_off"]]
    if bad:
        raise ProbeRunError(f"the guard faulted or switched off: {bad}")
    return names


def summary_line(doc: dict[str, Any]) -> str:
    h = doc["host"]
    return (
        f"{h['items_present']}/{doc['sequence']['items']} items, {h['frames']} frames, seq errors {h['seq_errors']}, "
        f"checksum errors {h['checksum_errors']}, runs {len(h['runs'])}, drops {doc['collector']['drops']}, "
        f"complete {h['complete']}, usable {h['usable']} ({h['capture_seconds']} s)"
    )


def compare_lines(res: dict[str, Any]) -> list[str]:
    if "derived" in res:  # dsp-facts
        return F.compare_lines(res)
    return [
        f"compared {res['compared']} items: {res['differing_items']} differ "
        f"(result {res['field_counts']['result']}, astat {res['field_counts']['astat']}, stky {res['field_counts']['stky']}); "
        f"only in the capture {len(res['only_a'])}, only in the reference {len(res['only_b'])}"
    ]


def run(
    up: D.Uploader,
    modules: list[str],
    capture_path: Path,
    *,
    compare_path: Path | None = None,
    keep: bool = False,
    seq_name: str | None = None,
    device: str | None = None,
    connect: Callable[[], D.Uploader] | None = None,
    wait_boot: Callable[[], None] | None = None,
    sleep: Callable[[float], None] = time.sleep,
    now: Callable[[], float] = time.monotonic,
    emu: Any | None = None,
    step: int = 100_000_000,
    pack: Callable[[str, str], Path] = R.build_pack,
    log: Callable[[str], None] = say,
) -> dict[str, Any]:
    """The whole sequence. UP talks to the device now; CONNECT and WAIT_BOOT as in `dkrun.run`. Returns {"doc": the capture, "compare": ...}."""
    sq = sequence_for(modules, seq_name)
    out: dict[str, Any] = {}
    info = up.info()
    R.require_runtime(info)
    dev = device or info["port"]
    log(
        f"{info['port']}, runtime {D.fmt_runtime(info['runtime'])}, image {info.get('image_id')}, free {info['free_bytes']} bytes"
        + (", safe mode" if info["safe"] else "")
    )
    store = {p: (n, h) for p, n, h in up.ls()}
    before = previous_selection(up, store, log)
    ids = [REMOTE, DEBUG, *[m for m in modules if m not in (REMOTE, DEBUG)], COLLECTOR]
    files: list[tuple[str, bytes]] = []
    for mid in dict.fromkeys(ids):
        t0 = time.monotonic()
        dkm = pack(mid, dev)
        log(f"packed {mid} for {dev}: {dkm.name} ({time.monotonic() - t0:.1f} s)")
        data = dkm.read_bytes()
        why = R.check_fit(up, info, data)
        if why:
            raise ProbeRunError(f"{mid} will not fit this device: {why}")
        files.append((R.module_path(mid), data))
    changed = 0
    for path, data in files:
        changed += R.upload_if_changed(up, path, data, store, log)
    out["uploaded"] = changed
    selection = selection_paths(modules)
    up.select(selection)
    log(f"selected {', '.join(selection)}")
    restore_needed = True
    try:
        up = R.restart_and_wait(up, connect, wait_boot, sleep, now, log)
        res = R.module_results(up, sleep, 60.0, now)
        for d in res:
            log(L.text_of(d).strip())
        R.check_results(res)
        names = check_loaded(up, [*ids])
        log(f"loaded: {', '.join(names)}")
        faults = fault_lines(up)
        if faults:
            raise ProbeRunError("fault after the boot: " + "; ".join(faults))
        log("no fault after the boot; capturing")
        doc = P.capture(up, emu, step, sq)
        capture_path.parent.mkdir(parents=True, exist_ok=True)
        capture_path.write_text(json.dumps(doc, indent=1) + "\n")
        log(f"{capture_path}: {summary_line(doc)}")
        out["doc"] = doc
        faults = fault_lines(up)
        if faults:
            raise ProbeRunError("fault during the capture: " + "; ".join(faults))
        log("no fault during the capture")
        if not doc["host"]["usable"]:
            raise ProbeRunError(
                f"the capture misses {doc['host']['items_missing']} items or the table checksum is wrong"
            )
        if not doc["host"]["complete"]:
            log(
                "WARNING framing errors in the capture (a word was lost): the items are all there from other runs"
            )
        if compare_path is not None:
            ref = json.loads(compare_path.read_text())
            out["compare"] = P.compare(doc, ref)
            for line in compare_lines(out["compare"]):
                log(line)
        if keep:
            restore_needed = False
            log("--keep: the probe modules stay selected and loaded")
    finally:
        if restore_needed:
            try:
                up.select(before)
                log(f"restoring the selection: {', '.join(before) or 'nothing'}")
                up = R.restart_and_wait(up, connect, wait_boot, sleep, now, log)
                log("restored")
            except Exception as e:  # noqa: BLE001  the failure of the run is the one to report
                log(f"WARNING the restore failed: {e}; SELECT {before} by hand")
    return out


def spawn_emulator(device: str) -> Any:
    """A private paused daemon on the stock firmware of DEVICE (dt2 1.17, dn2 1.12) with the digikit runtime patched in (`firmware.load {runtime}`), as the
    regression tests start it. The binary is DIGI_EMUD_<DEV> or DIGI_EMUD, else the build of tools.patchkit.pk_harness. Close it with `emu.__exit__`."""
    from tools.control.digi_client import Emu
    from tools.emulator import card_check
    from tools.patchkit import pk_harness as H

    cell = H.Cell(device, {"dt2": "1.17", "dn2": "1.12"}[device])
    syx, ready = H.firmware_inputs(cell)
    emu = Emu.spawn_coupled(
        syx, ready, timeout=300, binary=H.daemon_binary(device, syx)
    )
    try:
        params: dict[str, Any] = {
            "syx": str(syx),
            "runtime": True,
            "force": True,
            "auto_no": True,
            "start": "auto",
        }
        if device == "dt2":
            samples = H.OUT / "samples"
            card_check.make_samples(samples)
            params["card"] = emu.call("card.load", dir=str(samples))["card"]
        emu.call("firmware.load", **params)
    except BaseException:
        emu.__exit__(None, None, None)
        raise
    return emu


def main(args: Any) -> int:
    """The `run` subcommand of dkprobe (args: the parsed namespace)."""
    modules = [m for m in args.modules.split(",") if m]
    emu = None
    connect: Callable[[], D.Uploader] | None = None
    wait_boot: Callable[[], None] | None = None
    sleep: Callable[[float], None] = time.sleep
    now: Callable[[], float] = time.monotonic
    if args.emu or args.spawn:
        emu = spawn_emulator(args.spawn) if args.spawn else D._emu(args.run_dir)
        emu.subscribe()
        up = D.Uploader(D.EmuTransport(emu))
        steps = {"n": 0}

        def emu_sleep(s: float) -> None:
            n = int(s * R.EMU_IPS)
            emu.step(n)
            steps["n"] += n

        def emu_now() -> float:
            return steps["n"] / R.EMU_IPS

        sleep, now = emu_sleep, emu_now
        wait_boot = R.emu_boot_waiter(emu)
    else:
        up = D.Uploader(D.RtMidiTransport(args.port), skip_foreign=True)

        def connect() -> D.Uploader:
            return D.Uploader(D.RtMidiTransport(args.port), skip_foreign=True)

    try:
        res = run(
            up,
            modules,
            args.capture,
            compare_path=args.compare,
            keep=args.keep,
            seq_name=args.seq,
            device=args.device,
            connect=connect,
            wait_boot=wait_boot,
            sleep=sleep,
            now=now,
            emu=emu,
            step=args.step,
        )
    except (ProbeRunError, R.RunError, P.ProbeError, D.UploadError, TimeoutError) as e:
        print(f"error: {e}", file=sys.stderr)
        return 1
    finally:
        if args.spawn and emu is not None:
            emu.__exit__(None, None, None)
    return 0 if res["doc"]["host"]["usable"] else 1
