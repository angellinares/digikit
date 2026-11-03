#!/usr/bin/env python3
"""The `upload` scenario of the digikit runtime that stays in Python: module upload over USB SysEx in the emulator (docs/design/digikit-upload.md,
docs/findings/43-digikit-upload.md). Run by `tools.patchkit.pk_digikit_scen` (kind `upload`); the scenario's `[upload] part` picks one:

  rust      the Rust host `Session` (packages/dkupload example `emu_session`, the code of the desktop Modules panel and the web page)
            against the device side: INFO, LIST, put of a built module and of 70,000 bytes, replace, DELETE, E_HASH, lost replies, CLEAR; each
            step compared with the Python tool's LIST and the card read by plusdrive.

The other upload scenarios are TypeScript tests (modules/core/runtime/tests/upload.device.test.ts and upload-faults.device.test.ts).
`transport` is also used by the other digikit runners.

The through-the-menu proof (an uploaded module appears in DIGIKIT > MODULES, loads after APPLY & RESTART? and plays like the
card-built module) is the `menu` kind with `upload = true` (tests/digikit/scenarios/menu-upload-*.toml).
"""

from __future__ import annotations

import hashlib
import random
from pathlib import Path
from typing import Any

from tools.device import dkupload as D
from tools.patchkit import pk_dkrt_boot as B
from tools.patchkit import pk_dkrt_machine as M

# The delivery model of the emulated USB input: one high-speed microframe (a 512-byte packet carries at most 384 MIDI bytes)
HS: dict[str, int] = {"chunk": 384, "period": 16500}


# The test knob `DKRT_STORE_CAP` of the runtime keeps every sector from here to header B out of the allocations, so the DN2's 20 GiB store can be filled
# (and its free bytes fit INFO's 32 bits).
CAP_SECTORS = 0x40000


def apply_cap(c: Any, rig: Any, img: Any) -> None:
    """Take a store of more than 4 GiB down to `CAP_SECTORS` (128 MiB) on this rig; `free_fresh` follows."""
    if c.store.limit * 512 > 0xFFFFFFFF:
        rig.emu.poke32(M.sym(img.plan, "core/runtime", "DKRT_STORE_CAP"), CAP_SECTORS)
        c.store_cap = CAP_SECTORS


def free_fresh(c: Any) -> int:
    """INFO's free bytes of a card with no store: used 1, room for a one-entry table (the DN2's is cut by the cap)."""
    limit = getattr(c, "store_cap", None) or c.store.limit
    return min((limit - 1 - 1) * 512, 0xFFFFFFFF)


def blob(n: int, seed: int) -> bytes:
    """Deterministic bytes with every value (the 8-in-7 packing needs the high bits)."""
    return random.Random(seed).randbytes(n)


def transport(rig: Any) -> D.EmuTransport:
    return D.EmuTransport(rig.emu, **HS)


def sha(b: bytes) -> bytes:
    return hashlib.sha256(b).digest()


# ---- the card as the plusdrive reader sees it -----------------------------------------------------------------------------------


def card_view(
    c: Any, rig: Any, base: Path, tag: str
) -> tuple[dict[str, bytes], str, set[int]]:
    """The module store of BASE with the guest's writes laid over it: ({path: bytes}, the reader's header line, the sectors the guest
    wrote anywhere). The files are read (and their sha256 checked) by `plusdrive store ls --cat`."""
    from tools.coldfire import arena_safety
    from tools.patchkit.pk_digikit_project import card_after

    ov = rig.work / f"{tag}.ov.bin"
    rig.emu.call("card.overlay", path=str(ov))
    after = card_after(c, base, ov)
    writes = set(arena_safety.parse_overlay(ov.read_bytes()))
    import subprocess

    tools = c.env.tools
    r = tools.run(tools.plusdrive, "store", "ls", after, "--device", c.device)
    if r.returncode:
        return {}, f"error: {r.stderr.strip()}", writes
    files: dict[str, bytes] = {}
    head = ""
    for line in r.stdout.splitlines():
        if line.startswith("#"):
            head = line
            continue
        path = line.split("\t")[0]
        if line.endswith("\tfolder"):  # a folder with nothing in it: no file to read
            continue
        out = rig.work / f"{tag}.cat.bin"  # binary: through a file, not a decoded pipe
        with out.open("wb") as f:
            p = subprocess.run(
                [str(tools.plusdrive), "store", "ls", str(after), "--device", c.device, "--cat", path],
                stdout=f,
                stderr=subprocess.PIPE,
            )  # fmt: skip
        files[path] = out.read_bytes() if p.returncode == 0 else b"<unreadable>"
    return files, head, writes


def device_ls(up: D.Uploader) -> dict[str, tuple[int, str]]:
    return {p: (s, h) for p, s, h in up.ls()}


def same(
    c: Any,
    label: str,
    up: D.Uploader,
    files: dict[str, bytes],
    view: dict[str, bytes],
    head: str,
    version: int = 3,
) -> None:
    """The device's LIST, the reader's view and the bytes that were sent all agree."""
    want = {p: (len(b), sha(b).hex()) for p, b in files.items()}
    got = device_ls(up)
    c.check(f"{label}: LIST equals the files sent", got == want, f"{got} want {want}")
    c.check(
        f"{label}: the card, read by plusdrive, holds the same bytes",
        view == files,
        f"{ {p: len(b) for p, b in view.items()} } {head}",
    )
    c.check(f"{label}: store version {version}", f"version={version}" in head, head)


def _selection_untouched(c: Any) -> None:
    c.check(
        "the upload wrote no selection sector",
        not (c.store_writes & {c.store.sel_a, c.store.sel_b}),
        f"{sorted(hex(s) for s in c.store_writes)}",
    )


# ---- the Rust host ---------------------------------------------------------------------------------------------------------------


def rust_binary(c: Any) -> Path:
    """The `emu_session` example of packages/dkupload (the Rust `Session` the desktop Modules panel and the web page use)."""
    import json
    import subprocess

    from tools import REPO_ROOT

    r = subprocess.run(
        ["cargo", "build", "-q", "-p", "dkupload", "--example", "emu_session", "--message-format=json"],
        cwd=REPO_ROOT,
        capture_output=True,
        text=True,
    )  # fmt: skip
    exe = None
    for line in r.stdout.splitlines():
        try:
            m = json.loads(line)
        except ValueError:
            continue
        if m.get("executable") and m.get("target", {}).get("name") == "emu_session":
            exe = Path(m["executable"])
    if r.returncode or exe is None:
        raise RuntimeError(
            f"cargo build of the emu_session example failed: {r.stderr[-400:]}"
        )
    return exe


def run_rust(c: Any) -> None:
    """The Rust host `Session` (packages/dkupload) against the real device side in the emulator, one process per step through the
    control socket (`midi.usb.in` / `midi.usb.out` / `step`, as modules.rs does). After each step the Python tool's view (LIST) and
    the card, read by plusdrive, are compared with what the Rust host did."""
    import json
    import subprocess

    exe = rust_binary(c)
    mod = c.pack("test/sine", "rust.dkm")
    dkm_file = c.env.folder / "digikit" / mod
    dkm = dkm_file.read_bytes()
    dkm_file.unlink()
    img = c.image_for(B.RT, "run")
    base = c.card(None)
    work = c.work
    files: dict[str, bytes] = {}

    HSF = ("--chunk", "384", "--period", "16500")

    def host(rig: Any, *args: str, flags: tuple[str, ...] = HSF) -> dict[str, Any]:
        r = subprocess.run(
            [str(exe), rig.emu.path, *flags, *args],
            capture_output=True,
            text=True,
            timeout=900,
        )
        try:
            return dict(json.loads(r.stdout.strip().splitlines()[-1]))
        except (ValueError, IndexError):
            return {"ok": False, "failure": {"error": f"no output: {r.stderr[-300:]}"}}

    def put(
        rig: Any, data: bytes, path: str, tag: str, flags: tuple[str, ...] = HSF
    ) -> dict[str, Any]:
        f = work / f"rust-{tag}.bin"
        f.write_bytes(data)
        return host(rig, "put", str(f), path, flags=flags)

    with c.rig(img, base, "rust", cf_only=True) as rig:
        apply_cap(c, rig, img)
        up = D.Uploader(transport(rig))

        def check(label: str) -> None:
            view, head, writes = card_view(
                c, rig, base, f"rust-{label.replace(' ', '-')}"
            )
            same(c, label, up, files, view, head)
            c.store_writes |= {w for w in writes if c.store.first <= w < c.store.end}
            c.all_writes |= writes

        def stat(label: str, r: dict[str, Any]) -> None:
            c.check(f"Rust host: {label}", bool(r.get("ok")), str(r.get("failure")))

        py = up.info()
        r = host(rig, "info")
        i = r.get("result", {})
        c.check(
            f"Rust INFO: the device id is {c.device!r}, as the Python tool reads it, the free bytes of an empty store, 32 KiB chunks",
            bool(r.get("ok"))
            and i["port"] == c.device == py["port"]
            and i["protocol"] == py["protocol"] == 1
            and i["runtime_version"] == py["runtime"]
            and i["free_bytes"] == py["free_bytes"] == free_fresh(c)
            and i["max_chunk"] == 32768
            and not i["safe_mode"]
            and not i["upload_open"],
            f"{r} vs {py}",
        )
        r = host(rig, "ls")
        c.check(
            "Rust LIST of an empty store", r.get("ok") and r["result"] == [], str(r)
        )

        # a real module, one chunk
        f = work / "rust-dkm.bin"
        f.write_bytes(dkm)
        r = host(rig, "put", str(f), mod)
        stat(f"put {mod} ({len(dkm)} bytes, a built module)", r)
        files[mod] = dkm
        got = host(rig, "ls")
        c.check(
            "Rust LIST shows the module with the right size and sha256",
            got.get("ok")
            and got["result"]
            == [{"path": mod, "size": len(dkm), "sha256": sha(dkm).hex()}],
            str(got),
        )
        check("Rust put of a module")

        # three chunks, two DATA in flight
        a = blob(70_000, 41)
        files["modules/a.dkm"] = a
        r = put(rig, a, "modules/a.dkm", "a")
        stat("put 70,000 bytes (three chunks, two in flight)", r)
        check("Rust put, three chunks")
        c.report["rust_put_70000_stepped"] = r.get("stepped")

        # the Python tool and the Rust host agree on the same bytes
        up.put("modules/py.dkm", dkm)
        files["modules/py.dkm"] = dkm
        rl = {e["path"]: (e["size"], e["sha256"]) for e in host(rig, "ls")["result"]}
        c.check(
            "the Rust LIST equals the Python LIST (the same module put by either has the same sha256)",
            rl == device_ls(up) and rl[mod] == rl["modules/py.dkm"],
            str(rl),
        )
        check("Python put next to the Rust ones")

        # folders and ranges through the Rust host: LIST by prefix, MKDIR, VERIFY of a range
        r = host(rig, "mkdir", "samples/drums")
        c.check("the Rust host: MKDIR", r.get("ok") is True, str(r))
        c.check(
            "the Rust host: LIST by prefix lists the folder entry and nothing else; the Python tool sees the same",
            [e["path"] for e in host(rig, "ls", "samples/")["result"]]
            == ["samples/drums"]
            and [p for p, _, _ in up.ls("samples/")] == ["samples/drums"],
        )
        ln = min(1000, len(files[mod]) - 512)
        r = host(rig, "verify", mod, "512", str(ln))
        c.check(
            "the Rust host: VERIFY of a range equals the Python tool's and the bytes sent",
            r.get("ok") is True
            and r["result"]
            == up.verify(mod, 512, ln)
            == sha(files[mod][512 : 512 + ln]).hex(),
            str(r),
        )
        up.rm("samples/drums")

        # replace
        a2 = blob(1000, 42)
        files["modules/a.dkm"] = a2
        stat("replace modules/a.dkm", put(rig, a2, "modules/a.dkm", "a2"))
        check("Rust replace")

        # delete (and a delete of a missing path)
        stat("delete modules/py.dkm", host(rig, "rm", "modules/py.dkm"))
        del files["modules/py.dkm"]
        r = host(rig, "rm", "modules/py.dkm")
        c.check(
            "Rust delete of a missing path: E_NOFILE (-59)",
            not r["ok"] and r["failure"].get("status") == -59,
            str(r),
        )
        check("Rust delete")

        # E_HASH: nothing committed, no upload left open
        bad = blob(40_000, 43)
        f = work / "rust-bad.bin"
        f.write_bytes(bad)
        r = host(rig, "badhash", str(f), "modules/bad.dkm")
        c.check(
            "Rust wrong sha256: END answers E_HASH (-56)",
            not r["ok"] and r["failure"].get("status") == -56,
            str(r),
        )
        c.check("no upload is left open after E_HASH", not up.info()["upload_open"])
        check("Rust E_HASH (nothing committed)")

        # a lost reply to INFO, to a DATA, to END (the END retry gets E_STATE; the host looks the file up)
        r = host(rig, "--drop", "1", "info", flags=(*HSF, "--budget", "150000000"))
        c.check(
            "a lost INFO reply: the host retries and gets it", bool(r.get("ok")), str(r)
        )
        d = blob(1024, 44)
        files["modules/drop-data.dkm"] = d
        f = work / "rust-dd.bin"
        f.write_bytes(d)
        r = host(rig, "--drop", "2", "put", str(f), "modules/drop-data.dkm",
                 flags=(*HSF, "--budget", "150000000"))  # fmt: skip
        c.check(
            "a lost DATA reply: the chunk is sent again (retry by offset) and the put ends ok",
            bool(r.get("ok")),
            str(r),
        )
        check("Rust lost DATA reply")
        e = blob(1024, 45)
        files["modules/drop-end.dkm"] = e
        f = work / "rust-de.bin"
        f.write_bytes(e)
        r = host(rig, "--drop", "3", "put", str(f), "modules/drop-end.dkm",
                 flags=(*HSF, "--budget", "150000000"))  # fmt: skip
        c.check(
            "a lost END reply: the second END gets E_STATE, the host finds the file in LIST and ends ok",
            bool(r.get("ok")),
            str(r),
        )
        check("Rust lost END reply")

        # the timing of one 32 KiB chunk, default USB delivery (64 bytes / 132,000 instructions) against the high-speed model
        big = blob(32768, 46)
        files["modules/big.dkm"] = big
        slow = put(rig, big, "modules/big.dkm", "big-slow", flags=())
        stat("put 32 KiB with the default delivery", slow)
        fast = put(rig, big, "modules/big.dkm", "big-fast")
        stat("put the same 32 KiB with chunk 384 / period 16,500", fast)
        c.report["rust_32k_stepped_default"] = slow.get("stepped")
        c.report["rust_32k_stepped_hs"] = fast.get("stepped")
        c.report["rust_32k_wall_default_s"] = slow.get("wall_s")
        c.report["rust_32k_wall_hs_s"] = fast.get("wall_s")
        check("Rust 32 KiB puts")

        # CLEAR
        stat("clear", host(rig, "clear"))
        files.clear()
        check("Rust clear")
        c.check("CLEAR gives the space back", up.info()["free_bytes"] == free_fresh(c))
        c.done(rig)
    _selection_untouched(c)
    c.common_checks_upload()


PARTS = {
    "rust": run_rust,
}


def run_upload(c: Any) -> None:
    PARTS[c.table()["part"]](c)
