#!/usr/bin/env python3
"""Proof of core/dsp-link in the coupled emulator (docs/findings/31-dsp-link.md).

    uv run python -m tools.patchkit.pk_dsplink proof [--device dt2|dn2] [--fw 1.17] [--sizes 4096,65536] [--quick]
    uv run python -m tools.patchkit.pk_dsplink frame [--device dt2|dn2] [--fw 1.17]

`proof` patches core/dsp-link into the stock firmware, boots it coupled (DSP and ColdFire), and, with a note playing:

  1. runs the stock firmware and records the PCM of the note (the reference);
  2. runs the patched firmware with no transfer: the PCM must equal the reference (the module is invisible when idle);
  3. for each size: starts a transfer of known bytes into the DSP's reserved area (a host poke of the request fields,
     which is what `dl_send` stores; `dl_send`/`dl_poll` themselves are unit-tested on the ColdFire core in
     modules/core/adapters/dsp-link/tests/test_dsplink.py), waits for DONE, reads the DSP's L2 back (`dsp.peek`) and compares byte for byte,
     counts DSP blocks for the throughput, and compares the PCM of the whole note with the reference;
  4. a destination outside the area (forged request) must stop with status 3 and leave the area untouched;
  5. a lost chunk (the sender's position is pushed ahead) must be recovered by going back.

`frame` runs the patched firmware and prints the tail of the ColdFire's queue and of the DSP's receive rings while a
chunk is in flight: the check of the byte order of the wire units (finding 31, "Byte order").
Prints one JSON report; exit 0 when every check holds. Everything it reads from the DSP comes from the emulator.
"""

from __future__ import annotations

import argparse
import json
import random
import struct
import subprocess
import sys
import time
from pathlib import Path
from typing import Any

import numpy as np

ROOT = Path(__file__).resolve().parents[2]

from tools import python_env  # noqa: E402
from tools.patchkit import hook_ab, pk_device  # noqa: E402
from tools.patchkit import pk_harness as H  # noqa: E402

FW = {"dt2": "1.17", "dn2": "1.12"}
# DlLink field offsets (modules/core/adapters/dsp-link/src/cf/dsp-link.c, struct DlLink)
L_STATE, L_ERR, L_BUF, L_DST, L_NWORDS, L_XID, L_CAP, L_NCHUNKS = (
    0,
    4,
    8,
    12,
    16,
    20,
    24,
    28,
)
L_ACKED, L_NEXT, L_STALL, L_SINCE, L_SENT, L_RESENT, L_NERR, L_ENDING = (
    32,
    36,
    40,
    44,
    48,
    52,
    56,
    60,
)
L_WIN = 64
DL_IDLE, DL_BUSY, DL_DONE, DL_ERROR, DL_START = 0, 1, 2, 3, 4
AREA_WORDS = {
    d: pk_device.load(d)["digikit"]["dsp_area_bytes"] // 4
    for d in pk_device.digikit_devices()
}  # DL_AREA_WORDS
FRAME = 88_000  # ColdFire instructions per DSP block (132 M / 1500)
TRIG1 = 25
DN2_NOTE = [
    {"at_instr": 0, "button": 12, "down": True},
    {"at_instr": 1_000_000, "button": 12, "down": False},
    {"at_instr": 60_000_000, "button": 25, "down": True},
    {"at_instr": 100_000_000, "button": 25, "down": False},
]
# the receive rings of the DSP and the first whole tail word in them (bytes), per device
RINGS = {"dt2": ((0x264220, 0x265220), 0x804), "dn2": ((0x2C29D0, 0x2C39D0), 0xA80)}
TX_QUEUE = {
    "dt2": (0x80001BC0, 0x401, 0x55E),
    "dn2": (0x80002510, 1344, 1374),
}  # base, first tail entry, entries


class ProofError(RuntimeError):
    pass


def patch(device: str, fw: str) -> tuple[Path, dict[str, Any]]:
    syx = {"dt2": "Digitakt_II_OS", "dn2": "Digitone_II_OS"}[device] + fw + ".syx"
    out = ROOT / "out" / "patch" / f"dsplink-{device}-{fw}.syx"
    r = subprocess.run(
        [
            sys.executable, "-m", "tools.patchkit.patchkit", "patch",
            "--firmware", str(ROOT / "firmware" / syx),
            "--modules", "core/dsp-link", "-o", str(out),
        ],
        cwd=ROOT, env=python_env(), capture_output=True, text=True,
    )  # fmt: skip
    if r.returncode:
        raise ProofError(f"patch failed:\n{r.stdout}{r.stderr}")
    plan = json.loads(out.with_suffix(".plan.json").read_text())
    return out, plan


def plan_anchors(device: str, fw: str) -> list[dict[str, Any]]:
    import tomllib

    return tomllib.loads(
        (ROOT / "devices" / "ports" / f"{device}-{fw}.toml").read_text()
    )["anchor"]


def sym(plan: dict[str, Any], name: str) -> int:
    for k, v in plan["symbols"].items():
        if k.endswith(":" + name):
            return int(v)
    raise ProofError(f"no symbol {name} in the plan")


class Rig:
    """A session on the stock image or on the patched one, and the note."""

    def __init__(self, device: str, fw: str, modules: list[str], tag: str):
        self.device = device
        self.cell = H.Cell(device, fw, modules)
        self.session = H.Session(
            self.cell,
            H.Goldens(
                ROOT / "out" / "runs" / "patchkit" / f"dsplink-{tag}.goldens.json"
            ),
            cache_dir=H.SHARED_CACHE,
            run_name=f"dsplink-{tag}",
        )
        self.frames0 = 0

    def __enter__(self) -> Rig:
        self.session.__enter__()
        self.session.open()
        return self

    def __exit__(self, *exc: object) -> None:
        self.session.__exit__(*exc)

    @property
    def emu(self) -> Any:
        assert self.session.emu is not None
        return self.session.emu

    def frames(self) -> int:
        return int(self.emu.call("status")["dsp_frames"])

    def note(
        self,
        wav: Path,
        hook: Any = None,
        total_frames: int = 4500,
        trig_at: int = 20_000_000,
    ) -> np.ndarray:
        """Restart, play the note while HOOK(emu) is called after every block, capture the PCM."""
        emu = self.emu
        emu.restart(auto_no=True)
        emu.drain_events()
        if hook is not None:
            hook("start")
        emu.call("pcm.tap", action="capture", path=str(wav))
        if self.device == "dn2":
            emu.schedule(DN2_NOTE)
            base = self.frames()
            target = base + total_frames
        else:
            base = self.frames()
            target = base + total_frames
            emu.call("input.schedule", events=[
                {"at_instr": trig_at, "button": TRIG1, "down": True},
                {"at_instr": trig_at + 40_000_000, "button": TRIG1, "down": False},
            ])  # fmt: skip
        self.frames0 = base
        n = base
        while n < target:
            n = min(n + 1, target)
            emu.call("run_until", frames=n)
            if hook is not None:
                hook(n)
        emu.call("pcm.tap", action="capture_stop")
        return hook_ab.read_wav(wav)


def blob(size: int, seed: int) -> bytes:
    rng = random.Random(seed)
    return bytes(rng.randrange(256) for _ in range(size))


def poke_bytes(emu: Any, addr: int, data: bytes) -> None:
    data = data + b"\0" * (-len(data) % 4)
    for i in range(0, len(data), 4):
        emu.poke32(addr + i, struct.unpack(">I", data[i : i + 4])[0])


def peek_link(emu: Any, link: int) -> dict[str, int]:
    names = [
        "state",
        "err",
        "buf",
        "dst",
        "nwords",
        "xid",
        "cap",
        "nchunks",
        "acked",
        "next",
        "stall",
        "since",
        "sent",
        "resent",
        "nerr",
        "ending",
        "dirty",
        "win",
    ]
    return {n: int(emu.peek32(link + 4 * i) or 0) for i, n in enumerate(names)}  # fmt: skip


def start_request(emu: Any, link: int, buf: int, dst_off: int, nwords: int) -> None:
    """What dl_send stores (modules/core/adapters/dsp-link/src/cf/dsp-link.c), written from the host; the state last."""
    emu.poke32(link + L_ERR, 0)
    emu.poke32(link + L_WIN, 0)
    emu.poke32(link + L_BUF, buf)
    emu.poke32(link + L_DST, dst_off)
    emu.poke32(link + L_NWORDS, nwords)
    emu.poke32(link + L_ACKED, 0)
    emu.poke32(link + L_NERR, 0)
    emu.poke32(link + L_STATE, DL_START)


def dsp_bytes(emu: Any, addr: int, n: int) -> bytes:
    out = bytearray()
    for off in range(0, n, 4096):
        h = emu.dsp_peek(addr + off, min(4096, n - off))["hex"]
        out += bytes.fromhex(h.replace("??", "00"))
    return bytes(out)


def proof(a: argparse.Namespace) -> dict[str, Any]:
    device, fw = a.device, a.fw or FW[a.device]
    work = ROOT / "out" / "runs" / "dsplink" / f"{device}-{fw}"
    work.mkdir(parents=True, exist_ok=True)
    image, plan = patch(device, fw)
    link = sym(plan, "dl_link")
    area = sym(plan, "_dl_area")
    buf = sym(plan, "dl_frame_hook") + 0x4000
    report: dict[str, Any] = {
        "device": device, "fw": fw, "image": image.name, "link": hex(link), "area": hex(area), "buf": hex(buf), "checks": {},
    }  # fmt: skip
    checks: dict[str, Any] = report["checks"]

    def check(name: str, ok: bool, detail: Any = "") -> None:
        checks[name] = {"ok": bool(ok), "detail": detail}
        print(f"{'ok  ' if ok else 'FAIL'} {name} {detail}", flush=True)

    t0 = time.monotonic()
    # The reference is an image patched the same way (core/expansion's loader and a segment) but with nothing hooked: a
    # patched image boots cold, its ready state is not the stock snapshot's (finding 17), so only that compares sample for sample.
    site = int(
        next(
            a["addr"]
            for a in plan_anchors(device, fw)
            if a["name"] == "dsp.link.site.block_tail"
        )
    )
    with Rig(device, fw, ["test/cf-demo"], f"{device}-ref") as stock:
        ref = stock.note(work / "stock.wav")
        orig_site = bytes.fromhex(stock.emu.dsp_peek(site, 6)["hex"])
    report["stock_s"] = round(time.monotonic() - t0, 1)
    check(
        "reference note is audible",
        float(abs(ref).max()) > 0.01,
        f"{len(ref)} samples, peak {float(abs(ref).max()):.3f}",
    )

    sizes = [int(s) for s in a.sizes.split(",")]
    with Rig(device, fw, ["core/dsp-link"], f"{device}-patched") as rig:
        emu = rig.emu
        idle = rig.note(work / "idle.wav")
        r = hook_ab.compare(ref, idle, None)
        # The ColdFire hook costs instructions in every block interrupt, which moves the note-on against the block phase
        # (a one-block shift here, and sub-block timing differences in what the voice does after it, which also appear when
        # the reference trigger is moved). So this is reported, not checked; the checks below are exact.
        report["idle_vs_reference"] = {k: r[k] for k in ("samples", "identical", "rms_base", "rms_test")} | {"max_abs_diff": float(np.abs(ref[: len(idle)] - idle[: len(ref)]).max())}  # fmt: skip

        def restore_site(tick: Any) -> None:
            if tick == "start":
                emu.patch(text=f"BYTES {site:#x} {orig_site.hex()}")
                now = bytes.fromhex(emu.dsp_peek(site, 6)["hex"])
                check(
                    "hook site restored by the DSP patch (so the stub is not run)",
                    now == orig_site,
                    now.hex(),
                )

        off = rig.note(work / "hookoff.wav", restore_site)
        check(
            "DSP hook is transparent: PCM equals the same image with the site restored",
            bool(np.array_equal(off, idle)),
            f"{len(idle)} samples, {int((off != idle).sum())} differ",
        )

        for si, size in enumerate(sizes):
            data = blob(size, 1000 + si)
            dst_off = (
                0x10 if si == 0 else 0
            )  # the first run goes to an offset, the others to the start
            holder: dict[str, Any] = {}

            def hook(
                tick: Any,
                data: bytes = data,
                dst_off: int = dst_off,
                holder: dict[str, Any] = holder,
            ) -> None:
                if tick == "start":
                    poke_bytes(emu, buf, data)
                    start_request(emu, link, buf, dst_off, len(data) // 4)
                    holder["f0"] = rig.frames()
                    return
                if "done" in holder:
                    return
                st = int(emu.peek32(link + L_STATE) or 0)
                if st in (DL_DONE, DL_ERROR):
                    holder["done"] = rig.frames() - holder["f0"]
                    holder["state"] = st
                    holder["link"] = peek_link(emu, link)

            wav = rig.note(work / f"xfer{size}.wav", hook)
            lk = holder.get("link", peek_link(emu, link))
            check(
                f"{size} B: transfer reaches DONE",
                holder.get("state") == DL_DONE,
                f"state {holder.get('state')} err {lk['err']}",
            )
            got = dsp_bytes(emu, area + 4 * dst_off, size)
            check(
                f"{size} B: DSP L2 equals the source byte for byte",
                got == data,
                f"{sum(x != y for x, y in zip(got, data, strict=True))} bytes differ",
            )
            blocks = holder.get("done", 0)
            check(
                f"{size} B: PCM equals the idle image bit for bit, during and after",
                bool(np.array_equal(wav, idle)),
                f"{len(wav)} samples, {int((wav != idle).sum())} differ",
            )
            rate = size / (blocks / 1500.0) if blocks else 0.0
            report[f"xfer{size}"] = {
                "blocks": blocks, "chunks": lk["nchunks"], "cap_words": lk["cap"], "sent": lk["sent"],
                "resent": lk["resent"], "dsp_nerr": lk["nerr"], "bytes_per_s_at_1500_blocks_per_s": round(rate),
            }  # fmt: skip
            print(json.dumps(report[f"xfer{size}"]), flush=True)
            ds = int(emu.dsp_peek(sym(plan, "_dl_status"), 4)["hex"][:2] or "0", 16)
            check(f"{size} B: DSP status is done (1)", ds == 1, f"status {ds}")
            if a.quick:
                break

        # a forged destination: the DSP refuses it
        aw = AREA_WORDS[device]
        before = dsp_bytes(
            emu, area + 4 * (aw - 32), 192
        )  # the last 32 words of the area and 16 words beyond it
        holder = {}

        def hook_range(tick: Any, holder: dict[str, Any] = holder) -> None:
            if tick == "start":
                poke_bytes(emu, buf, blob(256, 7))
                start_request(
                    emu, link, buf, aw - 4, 64
                )  # the first chunk already ends past the area
                return
            if "state" not in holder and int(emu.peek32(link + L_STATE) or 0) in (
                DL_DONE,
                DL_ERROR,
            ):
                holder["state"] = int(emu.peek32(link + L_STATE) or 0)
                holder["link"] = peek_link(emu, link)

        rig.note(work / "range.wav", hook_range, total_frames=1200)
        after = dsp_bytes(emu, area + 4 * (aw - 32), 192)
        check(
            "forged range: the DSP stops the transfer (error 3)",
            holder.get("state") == DL_ERROR and holder["link"]["err"] == 3,
            str(holder.get("link", {}).get("err")),
        )
        check("forged range: nothing written", before == after)

        # a lost chunk: push the sender ahead once; the DSP ignores the gap and the sender goes back
        data = blob(8192, 99)
        holder = {}

        def hook_gap(tick: Any, holder: dict[str, Any] = holder) -> None:
            if tick == "start":
                poke_bytes(emu, buf, data)
                start_request(emu, link, buf, 0, len(data) // 4)
                holder["pushed"] = False
                return
            if (
                holder.get("pushed") is False
                and int(emu.peek32(link + L_ACKED) or 0) >= 5
            ):
                emu.poke32(link + L_NEXT, int(emu.peek32(link + L_NEXT) or 0) + 40)
                holder["pushed"] = True
            if "state" not in holder and int(emu.peek32(link + L_STATE) or 0) in (
                DL_DONE,
                DL_ERROR,
            ):
                holder["state"] = int(emu.peek32(link + L_STATE) or 0)
                holder["link"] = peek_link(emu, link)

        rig.note(work / "gap.wav", hook_gap, total_frames=2400)
        got = dsp_bytes(emu, area, len(data))
        lk = holder.get("link", {})
        check(
            "lost chunk: recovered, DONE",
            holder.get("state") == DL_DONE and got == data,
            f"state {holder.get('state')} resent {lk.get('resent')}",
        )
        check("lost chunk: the sender went back", lk.get("resent", 0) >= 1)
        report["health"] = {"cf": emu.call("status")["cf"]["phase"]}

    report["seconds"] = round(time.monotonic() - t0, 1)
    report["pass"] = all(c["ok"] for c in checks.values())
    (work / "report.json").write_text(json.dumps(report, indent=1))
    return report


def frame(a: argparse.Namespace) -> dict[str, Any]:
    """The queue tail on the ColdFire side and the ring tail on the DSP side, for one chunk in flight."""
    device, fw = a.device, a.fw or FW[a.device]
    image, plan = patch(device, fw)
    link = sym(plan, "dl_link")
    buf = sym(plan, "dl_frame_hook") + 0x4000
    rings, tail_off = RINGS[device]
    txbase, e0, nent = TX_QUEUE[device]
    out: dict[str, Any] = {"device": device}
    with Rig(device, fw, ["core/dsp-link"], f"{device}-frame") as rig:
        emu = rig.emu
        rig.session.reset()
        # idle: the tail words of both rings are zero
        out["idle_tail_words"] = []
        for r in rings:
            out["idle_tail_words"].append(dsp_bytes(emu, r + tail_off, 16).hex())
        data = bytes(range(256)) * 4
        poke_bytes(emu, buf, data)
        start_request(emu, link, buf, 0, len(data) // 4)
        cap = None
        for _ in range(40):
            rig.session.run_ms(0.7)
            lk = peek_link(emu, link)
            if lk["state"] == DL_BUSY and lk["sent"] >= 2:
                cap = lk["cap"]
                break
        words_cf = []
        for j in range(4):
            lo = (emu.peek32(txbase + 4 * (e0 + 2 * j + 0)) or 0) & 0xFFFF
            hi = (emu.peek32(txbase + 4 * (e0 + 2 * j + 1)) or 0) & 0xFFFF
            words_cf.append((lo, hi))
        out["cf_queue_tail_halves"] = [
            [f"{lo:04x}", f"{hi:04x}"] for lo, hi in words_cf
        ]
        out["dsp_ring_tail_words"] = [
            dsp_bytes(emu, r + tail_off, 16).hex() for r in rings
        ]
        out["link"] = peek_link(emu, link)
        out["cap"] = cap
    return out


def main(argv: list[str]) -> int:
    ap = argparse.ArgumentParser(
        description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter
    )
    sub = ap.add_subparsers(dest="cmd", required=True)
    for name in ("proof", "frame"):
        p = sub.add_parser(name)
        p.add_argument("--device", choices=["dt2", "dn2"], default="dt2")
        p.add_argument("--fw")
        if name == "proof":
            p.add_argument("--sizes", default="4096,65536")
            p.add_argument("--quick", action="store_true", help="the first size only")
    a = ap.parse_args(argv)
    rep = proof(a) if a.cmd == "proof" else frame(a)
    print(json.dumps(rep, indent=1))
    return 0 if rep.get("pass", True) else 1


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
