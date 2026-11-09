#!/usr/bin/env python3
"""Boot-time loading of run-time modules in the emulator (docs/findings/37-boot-load.md).

    uv run python -m tools.patchkit.pk_dkrt_boot proof [--image dt2-1.17|dn2-1.12] [--quick] [--keep DIR] [--only a,b]

The runtime loads the modules of the selection record (two sectors at the end of the card's store range) at boot, from a hook just
before the MainScreenView constructor. `proof` builds the images and cards and boots them in the coupled emulator (cold boots of the
patched firmware with the image's own DSP code; a restart is the emulator's RCM soft-reset model):

  equiv    a selection loaded at boot plays like the same module patched in: rows, DSP bytes, voice row, machine row, PCM
  restart  select through the mailbox, `dkrt_restart`, the next boot loads it; select nothing, restart, it is gone
  cut      the card states a power cut can leave between the two writes of a selection (and a torn copy)
  safe     a module that resets the machine in `init`: boots 1 and 2 raise the counter, boot 3 is safe mode; the safe-once flag
  watch    write-watch and flush check over every boot, and the card sectors the runtime wrote

Not covered **[O]**: the safe-mode key (the queue peek is untested on a device and the emulator delivers no key before the main
OS runs), `dkrt_restart` refusing during a card transfer (the emulator's eSDHC never reports one busy), panel edits surviving the
restart (they do not: finding 40; `DKRT_SAVE_MODE` selects the firmware's save sequence).

Python API for the scenario suite (lane C builds on it; everything below is importable, nothing runs at import)::

    from tools.patchkit import pk_dkrt_boot as B
    env = B.Env("dt2-1.17", work_dir)                  # the files of one image: folder, cards, binaries
    img = env.build_image("core/runtime,core/dynreg,core/dyndsp")   # a patched firmware for the static module set S -> Img
    env.pack("test/sine", "sine-d.dkm")           # a .dkm into the card folder (digikit/modules/)
    card = env.card(["modules/sine-d.dkm"], counter=0, seq=1)      # a card image holding that selection record
    with env.boot(img, card, tag="a") as rig:           # a coupled daemon, cold-booted on that card: the boot hook has run
        rig.boot_info()        # dict: safe, reason, counter, seq, copy, count, loaded, results[], ui_reached, dsp_wait
        rig.voice_rows()       # [(type, pc)] of the DSP voice registry (type 0xffffffff: free)
        rig.dyn_type("test/sine", "sine")   # the type the runtime assigned at this boot (from DKRT_ASSIGN)
        rig.machine_listed(16) # is the machine row of that type named
        rig.sel_records()      # the two record copies as the card holds them now: [dict|None, dict|None]
        rig.set_selection(["modules/x.dkm"])   # dkrt_selection_set through the mailbox (returns its result)
        rig.restart()          # dkrt_restart, then the emulator's soft reset; returns when the new boot is ready
        rig.select_type(16, 7) # the machine list on the panel (DT2); rig.note(wav, B.M.TRIG_FRAME) plays TRIG 1, returns the PCM
    B.make_record(paths, seq=1, counter=0, flags=0)     # the 512 bytes of one record copy (the layout of docs/design/digikit-store.md)
    B.write_record_sectors(card, copy_a, copy_b, "dt2")  # raw sector images over the two record sectors of a card file of a device

The record layout is in docs/design/digikit-store.md; the runtime's own view of the same bytes is `dkrt_sdk::sel`.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import shutil
import struct
import sys
import tempfile
import time
import zlib
from dataclasses import dataclass
from pathlib import Path
from typing import Any

ROOT = Path(__file__).resolve().parents[2]

from tools.coldfire import arena_safety  # noqa: E402
from tools.control.digi_client import Emu, EmuError  # noqa: E402
from tools.device.dklog_kinds import RUNTIME_PACKED  # noqa: E402
from tools.firmware import config  # noqa: E402
from tools.patchkit import pk_device  # noqa: E402
from tools.patchkit import pk_dkrt_machine as M  # noqa: E402
from tools.patchkit import pk_harness as H  # noqa: E402
from tools.patchkit.pk_dkrt import (  # noqa: E402
    DEVICE_SYX,
    Tools,
    build_card,
    patch,
)

RUNTIME_VERSION = (
    RUNTIME_PACKED  # packed major << 16 | minor << 8 | patch (sdk/sdk.toml, generated)
)
SEL_MAGIC, SEL_VERSION = 0x444B534C, 1
SEL_MAX, SEL_PATH = pk_device.contract_max()["sel_max"], 56
# The store area differs per device (devices/*.toml [card.store]): `pk_device.store_of(device)` has first, sectors, sel_a, sel_b and header_b (all absolute).
CMD_CARD_PROBE = 7
CMD_SETTINGS_PUT, CMD_SETTINGS_GET, CMD_SETTINGS_REMOVE, CMD_SETTINGS_FLUSH = (
    9,
    10,
    11,
    12,
)
E_RANGE, E_CARD_RO = -46, -47
CMD_LOAD, CMD_SELECT, CMD_RESTART, CMD_SELECT_ONE = (
    1,
    3,
    4,
    5,
)  # command 2 was CMD_UNLOAD (archived)
SAFE_NAMES = {0: "none", 1: "counter", 2: "key", 3: "flag"}
BI_FIELDS = [
    "magic",
    "version",
    "safe",
    "reason",
    "counter",
    "seq",
    "copy",
    "count",
    "loaded",
    "ui_reached",
]
# The stable period the proof's records carry (DSP blocks of 64 samples at 48 kHz; the runtime's default is 30 s): short, so that the
# UI loop clears the boot counter within the settle time.
STABLE_BLOCKS = 300
SETTLE_MS = int(
    os.environ.get("DIGIKIT_SETTLE_MS", "1500")
)  # emulated ms the first UI-loop passes run before a scenario looks (wait_dsp then waits for a module's DSP part)
E_SAFE_MODE = -44
E_LOADING = 1
RT = "core/runtime,core/dynreg,core/dyndsp"
FREE_TYPE = M.FREE_TYPE


class Fail(RuntimeError):
    pass


def map_at(i: int) -> int:
    """Byte offset of sticky-map entry I in a selection record."""
    return 28 if i == 0 else 480 + 4 * (i - 1)


def make_record(
    paths: list[str],
    seq: int = 1,
    counter: int = 0,
    flags: int = 0,
    stable_blocks: int = 0,
    tmap: list[int] | None = None,
) -> bytes:
    """One copy of the selection record (docs/design/digikit-store.md). STABLE_BLOCKS: the stable period of safe mode (0: the default).
    TMAP: the 8 sticky machine-type entries (`hash24 << 8 | type`, 0 = empty), entry 0 at byte 28, entries 1 to 7 at 480."""
    if len(paths) > SEL_MAX or any(len(p) >= SEL_PATH or not p for p in paths):
        raise Fail("selection: at most 8 paths of 1 to 55 characters")
    b = bytearray(512)
    struct.pack_into(
        ">IIIIIII",
        b,
        0,
        SEL_MAGIC,
        SEL_VERSION,
        seq,
        counter,
        flags,
        len(paths),
        stable_blocks,
    )
    for i, p in enumerate(paths):
        b[32 + i * SEL_PATH : 32 + i * SEL_PATH + len(p)] = p.encode()
    for i, e in enumerate(tmap or []):
        struct.pack_into(">I", b, map_at(i), e)
    struct.pack_into(">I", b, 508, zlib.crc32(bytes(b[:508])))
    return bytes(b)


def parse_record(b: bytes) -> dict[str, Any] | None:
    """A valid record as a dict, or None (magic, version, CRC, count, NUL-terminated paths)."""
    if len(b) != 512:
        return None
    magic, ver, seq, counter, flags, count = struct.unpack_from(">IIIIII", b, 0)
    if (
        magic != SEL_MAGIC
        or ver != SEL_VERSION
        or struct.unpack_from(">I", b, 508)[0] != zlib.crc32(b[:508])
        or count > SEL_MAX
    ):
        return None
    paths = []
    for i in range(count):
        raw = b[32 + i * SEL_PATH : 32 + (i + 1) * SEL_PATH]
        if 0 not in raw or raw[0] == 0:
            return None
        paths.append(raw.split(b"\0")[0].decode())
    tmap = [struct.unpack_from(">I", b, map_at(i))[0] for i in range(8)]
    return {
        "seq": seq,
        "counter": counter,
        "flags": flags,
        "paths": paths,
        "map": tmap,
    }


def write_record_sectors(
    card: Path, a: bytes | None, b: bytes | None, device: str
) -> None:
    """Write raw 512-byte sector images over the two record sectors of a card image file of DEVICE (None leaves one alone)."""
    s = pk_device.store_of(device)
    with open(card, "r+b") as f:
        for sector, data in ((s.sel_a, a), (s.sel_b, b)):
            if data is not None:
                f.seek(sector * 512)
                f.write(data)


def current(recs: list[dict[str, Any] | None]) -> dict[str, Any] | None:
    """The record a boot uses: the valid copy with the higher sequence number (A when equal)."""
    a, b = recs
    if a is None or (b is not None and b["seq"] > a["seq"]):
        return b
    return a


def records_in(
    card_file: Path, overlay: bytes, device: str
) -> list[dict[str, Any] | None]:
    """The two record copies of a card image of DEVICE with an overlay (the bytes the guest wrote) laid over it."""
    rec = 8 + 64 + 512
    s = pk_device.store_of(device)
    sel_a, sel_b = s.sel_a, s.sel_b
    secs: dict[int, bytearray] = {}
    with open(card_file, "rb") as f:
        for sector in (sel_a, sel_b):
            f.seek(sector * 512)
            # a card image that ends before the area (no store built into it) reads as zeros there, as the emulated card does
            secs[sector] = bytearray(f.read(512).ljust(512, b"\0"))
    for i in range(0, len(overlay), rec):
        sector = struct.unpack_from("<Q", overlay, i)[0]
        if sector in secs:
            mask, body = overlay[i + 8 : i + 72], overlay[i + 72 : i + rec]
            for j in range(512):
                if mask[j // 8] & (1 << (j % 8)):
                    secs[sector][j] = body[j]
    return [parse_record(bytes(secs[sel_a])), parse_record(bytes(secs[sel_b]))]


@dataclass
class Img:
    """A patched firmware and its plan."""

    syx: Path
    plan: dict
    modules: str


class Env:
    """The files of one device image: the card folder, the patched firmware, cards."""

    def __init__(self, image: str, work: Path, tools: Tools | None = None):
        self.image, self.work = image, work
        self.device = image[:3]
        self.tools = tools or Tools()
        work.mkdir(parents=True, exist_ok=True)
        self.folder = work / "plus_drive"
        (self.folder / "digikit").mkdir(
            parents=True, exist_ok=True
        )  # `digikit/modules` is made by the first pack
        self.stock = config.stock(DEVICE_SYX[image])
        self.cards = 0

    def build_image(self, modules: str, name: str = "img") -> Img:
        syx = self.work / f"{self.image}-{name}.syx"
        return Img(syx, patch(self.tools, self.stock, syx, modules), modules)

    def pack(self, module: str, file: str) -> Path:
        out = self.folder / "digikit" / "modules" / file
        out.parent.mkdir(parents=True, exist_ok=True)
        self.tools.patchkit_run("pack", module, "--device", self.device, "-o", out)
        return out

    def synth(self, spec: str, file: str) -> Path:
        """Pack the synthetic module SPEC (TOML text, keys in packages/patchkit/src/dkmsynth.rs) for this device into the store folder."""
        specs = self.work / "synth-specs"
        specs.mkdir(parents=True, exist_ok=True)
        src = specs / f"{file}.toml"
        src.write_text(spec)
        out = self.folder / "digikit" / "modules" / file
        out.parent.mkdir(parents=True, exist_ok=True)
        self.tools.patchkit_run("synth-dkm", src, "--device", self.device, "-o", out)
        return out

    def card(
        self,
        selection: list[str] | None,
        counter: int = 0,
        seq: int = 1,
        flags: int = 0,
        tag: str | None = None,
        stable_blocks: int = 0,
    ) -> Path:
        """A card image with the folder's store and, when SELECTION is not None, both copies of the record."""
        self.cards += 1
        path = self.work / f"card-{tag or self.cards}.img"
        path.unlink(missing_ok=True)
        build_card(self.tools, self.folder, path, self.stock, self.device)
        if selection is not None:
            rec = make_record(selection, seq, counter, flags, stable_blocks)
            write_record_sectors(path, rec, rec, self.device)
        return path

    def boot(
        self,
        img: Img,
        card: Path,
        tag: str = "boot",
        watch: bool = False,
        extra_env: dict[str, str] | None = None,
        cf_only: bool = False,
        settle_ms: int | None = None,
    ) -> BootRig:
        # SETTLE_MS: emulated ms to run after the boot before the rig is handed out (default SETTLE_MS)
        # CF_ONLY: a daemon without DSP and audio (`CfBootRig`)
        # (the cold boot runs the image's own DSP code from the start, as a device does: a boot-time DSP load needs it)
        # DIGI_WRITE_WATCH_SEQ: one watch summary per emulator (the cold boot, each reset boot, the installed runtime)
        rig = (CfBootRig if cf_only else BootRig)(
            self,
            img,
            tag,
            card,
            watch,
            {
                "DIGI_WRITE_WATCH_SEQ": "1",
                **(extra_env or {}),
            },
        )
        if settle_ms is not None:
            rig.settle_ms = settle_ms
        return rig


class BootRig(M.Rig):
    """A coupled daemon cold-booted on a card; `Rig` of pk_dkrt_machine plus the boot-time views."""

    def __init__(
        self,
        env: Env,
        img: Img,
        tag: str,
        card: Path,
        watch: bool,
        extra_env: dict[str, str] | None,
    ):
        super().__init__(
            tag,
            env.image,
            img.modules,
            img.plan,
            img.syx,
            env.folder,
            env.work,
            watch,
            card,
            extra_env,
        )
        self.env, self.card_path = env, card
        self._lock: Any = None
        self.settle_ms = SETTLE_MS
        self.boot_sym = (
            M.sym(img.plan, "core/runtime", "DKRT_BOOT") if M.has_rt(img.plan) else 0
        )

    def __enter__(self) -> BootRig:
        # two rigs on one run directory (one firmware on one card) would share a socket and a boot cache: the second waits
        # (scenarios run side by side in threads and in processes)
        import fcntl

        STATE_DIR.mkdir(parents=True, exist_ok=True)
        self._lock = (STATE_DIR / f"{self.s.run_name}.lock").open("w")
        fcntl.flock(self._lock, fcntl.LOCK_EX)
        try:
            self.start()
        except BaseException:
            self._release(False)
            raise
        return self

    def _release(self, ok: bool = True) -> None:
        self.s.close(ok)
        lock, self._lock = self._lock, None
        if lock is not None:
            lock.close()  # closing the file drops the flock

    def __exit__(self, exc_type: object, *exc: object) -> None:
        self._release(exc_type is None)

    def start(self) -> None:  # the cold boot has run the hook; no mailbox traffic yet
        self.s.open(self.syx)
        self.s.segment = "boot"
        # the state at the ready point of the cold boot, before anything else runs: is the DSP part of the selection done?
        self.at_ready = self.boot_info()
        # when the cold boot is ready: DSP blocks since the boot (a block is 64 samples at 48 kHz) and whether the voice row is in
        self.frames_at_ready = self.frames()
        self.voice_at_ready = [r for r in self.voice_rows() if r[0] != FREE_TYPE]
        # DSP blocks from the ready point (the cold boot is ready, the UI loop runs) to the DSP part being done: a saved track plays as
        # type 0 until then. None: no DSP part (or already done at the ready point: 0).
        self.blocks_to_dsp: int | None = (
            0 if E_LOADING not in self.at_ready["results"] else None
        )
        n = 0
        while self.blocks_to_dsp is None and n < 3000:
            self.emu.step(88_000)
            n += 1
            if E_LOADING not in self.boot_info()["results"]:
                self.blocks_to_dsp = self.frames() - self.frames_at_ready
        self.s.reset()
        self.s.run_ms(self.settle_ms)
        self.wait_dsp()

    def wait_dsp(self, ms: int = 5000) -> None:
        """Run until no selected module is still `LOADING` (its DSP part follows the CF part over the first UI-loop passes)."""
        for _ in range(ms // 100):
            if E_LOADING not in self.boot_info()["results"]:
                return
            self.s.run_ms(100)

    def boot_info(self) -> dict[str, Any]:
        d = {k: self.w(self.boot_sym + 4 * i) for i, k in enumerate(BI_FIELDS)}
        res = []
        for i in range(SEL_MAX):
            v = self.w(self.boot_sym + 4 * (len(BI_FIELDS) + i))
            res.append(v - (1 << 32) if v >= 1 << 31 else v)
        d["results"] = res[: d["count"]] if d["count"] else res[:0]
        d["all_results"] = res
        d["dsp_wait"] = self.w(self.boot_sym + 4 * (len(BI_FIELDS) + SEL_MAX))
        d["stable"] = self.w(self.boot_sym + 4 * (len(BI_FIELDS) + SEL_MAX + 1))
        d["reason_name"] = SAFE_NAMES.get(d["reason"], "?")
        return d

    def assign_table(self) -> dict[str, Any]:
        """The boot's machine-type assignment (`DKRT_ASSIGN`, assign.rs): the sticky map and, per machine of the selection, its module's
        selection index, machine key (`hash << 8`) and assigned type (0: none was free)."""
        base = M.sym(self.plan, "core/runtime", "DKRT_ASSIGN")
        w = [self.w(base + 4 * i) for i in range(3 + 8 + 3 * 64)]
        n = w[1]
        return {
            "magic": w[0],
            "dirty": w[2],
            "map": w[3:11],
            "machines": [
                {"index": w[11 + i], "key": w[75 + i], "type": w[139 + i]}
                for i in range(n)
            ],
        }

    def dyn_type(self, module: str, machine: str) -> int:
        """The type number the runtime assigned to `<module>:<machine>` in this boot (read from `DKRT_ASSIGN`)."""
        h = M.machine_hash(module, machine)
        for m in self.assign_table()["machines"]:
            if m["key"] >> 8 == h and m["type"]:
                return m["type"]
        raise Fail(f"{self.tag}: {module}:{machine} has no assigned type")

    def voice_rows(self) -> list[tuple[int, int]]:
        return [self.voice_row(k) for k in range(8)]

    def post_raw(self, cmd: int, data: bytes) -> int:
        """A mailbox command with binary arguments (big-endian words); returns its result."""
        b = data + bytes(-len(data) % 4)
        for i in range(0, len(b), 4):
            self.emu.poke32(
                self.mb + M.MB_ARG + i, struct.unpack(">I", b[i : i + 4])[0]
            )
        seq0 = self.w(self.mb + M.MB_SEQ)
        self.emu.poke32(self.mb + M.MB_CMD, cmd)
        for _ in range(200):
            self.s.run_ms(20)
            if self.w(self.mb + M.MB_CMD) == 0 and self.w(self.mb + M.MB_SEQ) != seq0:
                break
        r = self.w(self.mb + M.MB_RESULT)
        return r - (1 << 32) if r >= 1 << 31 else r

    def probe_write(self, sector: int, nbytes: int) -> int:
        """Ask the runtime's one card-write function to write NBYTES at SECTOR (`CMD_CARD_PROBE`): 0, `E_RANGE` or `E_CARD_RO`.
        Only used with sectors outside the store range or on a card that is not ours: it would write the staging buffer."""
        return self.post_raw(CMD_CARD_PROBE, struct.pack(">II", sector, nbytes))

    def sel_records(self) -> list[dict[str, Any] | None]:
        """The two copies as the card holds them now (the overlay of the running daemon over the image)."""
        ov = self.work / f"{self.tag}.sel-overlay.bin"
        self.emu.call("card.overlay", path=str(ov))
        return records_in(self.card_path, ov.read_bytes(), self.env.device)

    def card_store_writes(self) -> list[int]:
        """The sectors in the store range that the guest has written."""
        ov = self.work / f"{self.tag}.all-overlay.bin"
        self.emu.call("card.overlay", path=str(ov))
        return arena_safety.card_store_hits(ov, self.env.device)

    def set_selection(self, paths: list[str]) -> int:
        return self.post(CMD_SELECT, "\0".join(paths) + "\0\0")

    def restart(self, timeout_s: float = 900.0) -> None:
        """`dkrt_restart` (mailbox command), then the emulator's RCM soft-reset model: the controller cold-boots the same patched
        firmware on the card as the guest left it. Returns when the new boot is ready (the daemon is the same)."""
        self.emu.drain_events()
        self.emu.poke32(self.mb + M.MB_CMD, CMD_RESTART)
        t0 = time.monotonic()
        while time.monotonic() - t0 < timeout_s:
            try:
                self.emu.step(400_000)
            except Exception:  # noqa: BLE001  a cold boot runs on its worker: steps are refused until it is done
                time.sleep(0.5)
            for ev in self.emu.drain_events():
                if ev.get("kind") == "cpu.reset" and ev.get("restart") == "unsupported":
                    raise Fail("the emulator cannot restart this build")
                if ev.get("kind") == "firmware.loaded":
                    if not ev.get("ok"):
                        raise Fail(f"restart: the cold boot failed {ev}")
                    self.s.emu.watchdog(self.s.cell.stall_ms)
                    self.s.run_ms(1500)
                    self.wait_dsp()
                    return
        raise Fail(f"restart: no new boot after {timeout_s:.0f} s")

    def watch_summaries(self) -> dict[str, Any]:
        """Close the daemon and add up the write-watch summaries of every emulator it ran (cold boots included)."""
        self.s.close()
        events = flush = cpushl = n = 0
        for f in sorted(self.work.glob(f"{self.tag}.writewatch-*.summary.json")):
            d = json.loads(f.read_text())
            events += d["events"]
            flush += len(d["flush_hits"])
            cpushl += d["cpushl"]
            n += 1
        return {
            "emulators": n,
            "write_events": events,
            "flush_hits": flush,
            "cpushl": cpushl,
        }


# The ColdFire state of a cold boot at the main panel (`state.save`), shared by the rigs of one key (the daemon binary, the
# image, the card, the environment) and by the later runs: a rig restores it (`state.load`, about 0.3 s) instead of booting (about 6 s).
STATE_DIR = H.SHARED_CACHE / "cf-states"
KEEP_STATES = 64  # newest files kept
_UNKEYED_ENV = (
    "DIGI_WRITE_WATCH",
    "DIGI_FLUSH_CHECK",
)  # the watch looks, it does not change the boot
_EXE_IDS: dict[tuple[str, int, int], str] = {}


def exe_id(exe: Path) -> str:
    """The content hash of a daemon binary (read once per file version): a changed emulator never restores an old state."""
    st = exe.stat()
    key = (str(exe), st.st_size, st.st_mtime_ns)
    if key not in _EXE_IDS:
        _EXE_IDS[key] = hashlib.sha256(exe.read_bytes()).hexdigest()
    return _EXE_IDS[key]


def state_key(exe: Path, image: Path, card: Path, env: dict[str, str]) -> str:
    """The name of the saved boot state of (daemon, image, card, environment). The card is named by path, size and mtime as
    the daemon names a card (the digikit cards are cached by content, so one content is one path)."""
    st = card.stat()
    h = hashlib.sha256()
    h.update(exe_id(exe).encode())
    h.update(hashlib.sha256(image.read_bytes()).digest())
    h.update(f"{card.resolve()}:{st.st_size}:{st.st_mtime_ns}".encode())
    for k, v in sorted(
        {**env, "DIGIKIT_RUN_SALT": os.environ.get("DIGIKIT_RUN_SALT", "")}.items()
    ):
        if not k.startswith(_UNKEYED_ENV):
            h.update(f"{k}={v};".encode())
    return h.hexdigest()[:24]


def prune_states(keep: int = KEEP_STATES) -> None:
    """Delete all but the KEEP newest saved states."""
    files = sorted(
        STATE_DIR.glob("*.snap"), key=lambda p: p.stat().st_mtime, reverse=True
    )
    for old in files[keep:]:
        old.unlink(missing_ok=True)


class CfFolderSession(M.FolderSession):
    """A daemon without DSP and audio: the ColdFire alone. Its cold boot to the main panel takes about 6 s (the coupled one 10 to 20 s)
    and a second of emulated idle time costs almost nothing (the coupled daemon runs at about a quarter of real time). What the
    firmware does on the ColdFire is the same; what needs the DSP (a module's DSP part, voice rows, PCM) is not there.

    The first boot of a key saves its state at the main panel (`state.save`, STATE_DIR); every later `load` of the key, and `reset()`,
    is `firmware.load` then `state.load` of that file."""

    exe: Path | None = None
    state_hit = False  # the last load restored a saved state

    def open(self, image: Path | None = None) -> None:
        exe = H.daemon_binary(self.cell.device, self.syx)
        self.exe = Path(exe)
        if self.run_dir is None:
            self.run_dir = H.temp_run_dir(self.run_name)
            self._temp_run_dir = True
        self.emu = Emu.spawn(
            binary=exe,
            timeout=300,
            run_dir=self.run_dir,
            env=self.env,
        )
        self.emu.subscribe()
        self.card = str(self.card_img)
        assert image is not None
        self.load(image)

    def state_file(self, image: Path) -> Path | None:
        """Where the boot state of this daemon, image and card is kept (None: no cache for this daemon)."""
        if (
            self.exe is None
            or self.card is None
            or os.environ.get("DIGI_CF_STATES") == "0"
        ):
            return None
        return (
            STATE_DIR
            / f"{self.cell.device}-{state_key(self.exe, image, Path(self.card), self.env)}.snap"
        )

    def _firmware_load(self, image: Path) -> dict[str, Any]:
        assert self.emu is not None and self.card is not None
        try:
            return self.emu.call(
                "firmware.load",
                syx=str(image),
                force=True,
                auto_no=True,
                start="auto",
                card=self.card,
            )
        except EmuError as e:
            raise H.LoadFailure(f"firmware.load {image.name}: {e}") from e

    def load(self, image: Path, settle: bool = False) -> dict[str, Any]:
        emu = self.emu
        assert emu is not None and self.card is not None
        started = time.monotonic()
        report = self._firmware_load(image)
        state = self.state_file(image)
        self.state_hit = False
        if state is not None and state.is_file():
            try:
                emu.state_load(state)
                self.state_hit = True
            except (
                EmuError
            ) as e:  # a file the daemon refuses is stale or torn: boot cold, save again
                print(f"cf state {state.name}: {e}; booting cold", file=sys.stderr)
                state.unlink(missing_ok=True)
                report = self._firmware_load(
                    image
                )  # a fresh runtime: the failed restore may have changed this one
        self.image, self.load_report = image, report
        emu.watchdog(self.cell.stall_ms)
        # the boot runs when the daemon is stepped; the panel is up when the main UI has been reached (a used card never runs
        # the DT2 filesystem check the coupled readiness contract waits for). A restored state is at that point already.
        for _ in range(0 if self.state_hit else 400):
            emu.step(20_000_000)
            cf = emu.status()["cf"]
            if cf.get("error") or cf["phase"] == "fault":
                raise H.LoadFailure(f"cold boot: {cf.get('error')} pc={cf['pc']:#x}")
            if cf["main_ui_reached"]:
                break
        else:
            cf = emu.status()["cf"]
            if not cf["main_ui_reached"]:
                raise H.LoadFailure("cold boot: the main panel was not reached")
        emu.drain_events()
        if state is not None and not self.state_hit:
            state.parent.mkdir(parents=True, exist_ok=True)
            emu.state_save(state)
            prune_states()
        self.timings["load_s"] = time.monotonic() - started
        return report

    def reset(self) -> None:
        """Back to the boot state of the loaded image: a fresh runtime, restored (or booted when the key has no state yet)."""
        assert self.image is not None
        self.load(self.image)

    def mark_pcm(self) -> None:  # no PCM tap
        pass


class CfBootRig(BootRig):
    """`BootRig` on a ColdFire-only daemon (`CfFolderSession`): panel scripts, RAM views and the card; no DSP, no PCM, no restart.
    `boot_info()['results']` of a module with a DSP part still reads `LOADING`."""

    session_cls = CfFolderSession

    def start(self) -> None:
        self.s.open(self.syx)
        self.s.segment = "boot"
        self.at_ready = self.boot_info()
        self.s.run_ms(
            self.settle_ms
        )  # the first UI-loop passes, as the coupled rig settles

    def wait_dsp(self, ms: int = 5000) -> None:
        pass

    def voice_rows(self) -> list[tuple[int, int]]:
        raise Fail("no DSP in a ColdFire-only rig")

    def restart(self, timeout_s: float = 900.0) -> None:
        raise Fail("the soft-reset model is coupled only")

    def reset(self) -> None:
        """Back to the state after `start()`: the boot state restored (a fresh daemon runtime) and the settle time run again."""
        self.s.reset()
        self.s.run_ms(self.settle_ms)


# ---- the proof ----------------------------------------------------------------------------------------------------------------


class Proof:
    def __init__(self, image: str, quick: bool, work: Path, only: set[str]):
        self.image, self.quick, self.work, self.only = image, quick, work, only
        self.env = Env(image, work)
        self.checks: list[tuple[str, bool, str]] = []
        self.report: dict[str, Any] = {"image": image, "work": str(work)}
        self.dt2 = image.startswith("dt2")
        self.watch_totals = {"emulators": 0, "write_events": 0, "flush_hits": 0}
        self.store_writes: set[int] = set()
        self.mods: list[str] = []
        # DSP voice rows: test/sine; on DN2 also test/dyn-dn2 (it carries the DSP of test/sine)
        self.voices = 1 if self.dt2 else 2
        self.run_img: Img | None = None

    def check(self, name: str, ok: bool, detail: str = "") -> None:
        self.checks.append((name, bool(ok), detail))
        print(f"{'ok  ' if ok else 'FAIL'} {name} {detail}", flush=True)

    def finish(self, rig: BootRig) -> None:
        """Collect the card-store writes and the watch summaries of a rig that is done."""
        boot = rig.s.load_report.get(
            "boot"
        )  # the cold boot's ColdFire instruction count (absent when the ready pair was cached)
        if boot:
            self.report.setdefault("boot_icount", {})[rig.tag] = boot["icount"]
        try:
            self.store_writes |= set(rig.card_store_writes())
        except Exception as e:  # noqa: BLE001
            print(f"card.overlay: {e}", file=sys.stderr)
        w = rig.watch_summaries()
        for k in self.watch_totals:
            self.watch_totals[k] += w[k]

    def setup(self) -> None:
        e = self.env
        if self.dt2:
            e.pack(M.MODULE, "m1.dkm")
            self.mods = ["modules/m1.dkm"]
        else:
            e.pack("test/dyn-dn2", "m0.dkm")
            e.pack("test/sine", "m1.dkm")
            self.mods = ["modules/m0.dkm", "modules/m1.dkm"]
        e.pack("test/dyn-fault", "fault.dkm")
        self.run_img = e.build_image(RT, "run")

    def rig(
        self,
        img: Img | None,
        card: Path,
        tag: str,
        extra_env: dict[str, str] | None = None,
    ) -> BootRig:
        assert img is not None
        return self.env.boot(img, card, tag, watch=True, extra_env=extra_env)

    # -- scenarios
    def equiv(self) -> None:
        e = self.env
        trig = M.TRIG_FRAME if self.dt2 else M.TRIG_FRAME_DN2
        card = e.card(self.mods, tag="equiv", stable_blocks=STABLE_BLOCKS)
        with self.rig(self.run_img, card, "equiv") as run:
            n = len(self.mods)
            info = run.boot_info()
            self.report["boot_info"] = info
            self.report["at_ready"] = run.at_ready
            self.check(
                "the selection loaded at boot: every module result is 0",
                info["count"] == n
                and info["results"] == [0] * n
                and info["loaded"] == n
                and info["safe"] == 0,
                f"{info['results']} dsp_wait {info['dsp_wait']}",
            )
            rec = current(run.sel_records())
            self.check(
                "the UI loop ran and the counter is back to 0",
                info["ui_reached"] == 1 and rec is not None and rec["counter"] == 0,
                str(rec),
            )
            used = [r for r in run.voice_rows() if r[0] != FREE_TYPE]
            # DT2: the machine type of test/sine, assigned at load; DN2: a voice row for each of test/dyn-dn2 and test/sine
            mach = (
                run.dyn_type(M.MODULE, M.MACHINE)
                if self.dt2
                else run.dyn_type("test/dyn-dn2", "dyn_pluck")
            )
            vtype = mach if self.dt2 else run.dyn_type("test/sine", "sine")
            want_types = {mach} if self.dt2 else {mach, vtype}
            self.check(
                "the voice rows of the DSP modules are claimed",
                len(used) == self.voices and {r[0] for r in used} == want_types,
                str([(hex(a), hex(b)) for a, b in used]),
            )
            idx = self.mods.index("modules/m1.dkm")
            sec = M.parse_dsp_section(
                (e.folder / "digikit/modules/m1.dkm").read_bytes()
            )
            assert self.run_img is not None
            base = run.info_word(3) + run.w(run.dsp + M.D_MOD_OFF + 4 * idx)
            want = M.relocate(sec, base, M.run_imports(run, self.run_img.plan))
            got = run.dsp_bytes(base, len(want))
            self.check(
                "the DSP area holds the relocated image byte for byte",
                got == want,
                f"{len(want)} bytes at {base:#x}",
            )
            self.check("the machine row is named", run.machine_listed(mach))
            if self.dt2:
                downs = run.select_type(mach, 7)
                self.check(
                    f"the stock machine list shows it: the track takes type {mach}",
                    run.type_now() == mach,
                    f"downs {downs}",
                )
            if not self.dt2:  # the sine machine follows MIDI and the first run-time machine in the list
                run.select(6)
            pcm = run.note(self.work / "run.wav", trig)
            peak = float(abs(pcm).max())
            self.check("the run-time note is audible", peak > 0.01, f"peak {peak:.3f}")
            self.finish(run)

    def restart(self) -> None:
        e = self.env
        n = len(self.mods)
        with self.rig(
            self.run_img,
            e.card([], tag="restart", stable_blocks=STABLE_BLOCKS),
            "restart",
        ) as rig:
            b0 = rig.boot_info()
            self.check(
                "a card with an empty selection boots with no module",
                b0["count"] == 0
                and b0["loaded"] == 0
                and b0["safe"] == 0
                and b0["seq"] == 1,
            )
            self.check(
                "the mailbox selection answers 0", rig.set_selection(self.mods) == 0
            )
            recs = rig.sel_records()
            self.check(
                "both copies hold the selection, the same sequence number",
                recs[0] == recs[1]
                and recs[0] is not None
                and recs[0]["paths"] == self.mods
                and recs[0]["seq"] == 2,
                str(recs),
            )
            rig.restart()
            b1 = rig.boot_info()
            self.report["restart_boot"] = b1
            self.check(
                "the next boot loads it",
                b1["loaded"] == n and b1["results"] == [0] * n and b1["seq"] == 2,
                str(b1["results"]),
            )
            rows = [r for r in rig.voice_rows() if r[0] != FREE_TYPE]
            self.check(
                "its voice rows are claimed", len(rows) == self.voices, str(rows)
            )
            rec = current(rig.sel_records())
            self.check(
                "the UI loop of the new boot cleared the counter",
                rec is not None and rec["counter"] == 0,
            )
            if not self.quick:
                self.check("select nothing", rig.set_selection([]) == 0)
                rig.restart()
                b2 = rig.boot_info()
                self.check(
                    "the boot after that loads nothing and the voice rows are free",
                    b2["count"] == 0
                    and all(r[0] == FREE_TYPE for r in rig.voice_rows()),
                )
            self.finish(rig)

    def cut(self) -> None:
        e = self.env
        m = self.mods
        rec_new = make_record(m, seq=2)
        rec_old = make_record([], seq=1)
        torn = bytearray(rec_new)
        torn[100] ^= 0xFF  # a torn write: the CRC fails
        cases = [
            (
                "A new, B old (cut after the first write)",
                rec_new,
                rec_old,
                len(m),
                2,
                0,
            ),
            (
                "A torn, B old (cut inside the first write)",
                bytes(torn),
                rec_old,
                0,
                1,
                1,
            ),
            ("A old, B new", rec_old, rec_new, len(m), 2, 1),
            ("both copies invalid", bytes(torn), bytes(torn), 0, 0, 0),
        ]
        if self.quick:
            cases = cases[:2]
        for name, a, b, want_loaded, want_seq, want_copy in cases:
            card = e.card(None, tag="cut")
            write_record_sectors(card, a, b, e.device)
            with self.rig(self.run_img, card, "cut") as rig:
                info = rig.boot_info()
                self.check(
                    f"power cut, {name}: the boot uses the valid copy with the higher sequence number",
                    info["loaded"] == want_loaded
                    and info["seq"] == want_seq
                    and info["copy"] == want_copy
                    and info["safe"] == 0,
                    f"loaded {info['loaded']} seq {info['seq']} copy {info['copy']}",
                )
                self.finish(rig)
        # the state made by the runtime itself: one copy written (CMD_SELECT_ONE), then the restart
        with self.rig(self.run_img, e.card(None, tag="cut2"), "cut2") as rig:
            r = rig.post(CMD_SELECT_ONE, "\0" + "\0".join(m) + "\0\0")
            recs = rig.sel_records()
            self.check(
                "one copy written by the runtime: A new, B still empty",
                r == 0
                and recs[0] is not None
                and recs[0]["seq"] == 1
                and recs[1] is None,
                str(recs),
            )
            rig.restart()
            info = rig.boot_info()
            self.check(
                "the restart after the cut loads the new copy",
                info["loaded"] == len(m) and info["copy"] == 0,
                str(info["results"]),
            )
            self.finish(rig)

    def safe(self) -> None:
        e = self.env
        with self.rig(
            self.run_img, e.card(["modules/fault.dkm"], tag="safe"), "safe"
        ) as rig:
            info = rig.boot_info()
            self.report["safe_boot"] = info
            self.check(
                "two boots that never reached the UI loop, the third boots in safe mode",
                info["safe"] == 1 and info["reason"] == 1 and info["counter"] == 2,
                f"{info['reason_name']} counter {info['counter']}",
            )
            self.check(
                "safe mode loaded no module",
                info["loaded"] == 0
                and info["all_results"][0] == E_SAFE_MODE
                and all(r[0] == FREE_TYPE for r in rig.voice_rows()),
                str(info["all_results"][:2]),
            )
            rec = current(rig.sel_records())
            self.check(
                "the UI loop ran: the counter is back to 0 and the selection is kept",
                info["ui_reached"] == 1
                and rec is not None
                and rec["counter"] == 0
                and rec["paths"] == ["modules/fault.dkm"],
                str(rec),
            )
            self.finish(rig)
        # safe once: the record asks for it; the boot that honours it clears the flag
        with self.rig(
            self.run_img, e.card(self.mods, flags=1, tag="safe2"), "safe2"
        ) as rig:
            info = rig.boot_info()
            rec = current(rig.sel_records())
            self.check(
                "the safe-once flag: safe mode (flag), no module, flag cleared, selection kept",
                info["safe"] == 1
                and info["reason"] == 3
                and info["loaded"] == 0
                and rec is not None
                and rec["flags"] == 0
                and rec["paths"] == self.mods,
                f"{info['reason_name']} {rec}",
            )
            self.finish(rig)

    def run(self) -> dict[str, Any]:
        t0 = time.monotonic()
        self.setup()
        scenarios = {
            "equiv": self.equiv,
            "restart": self.restart,
            "cut": self.cut,
            "safe": self.safe,
        }
        for name, fn in scenarios.items():
            if self.only and name not in self.only:
                continue
            print(f"--- {name}", flush=True)
            fn()
        if not self.only or "watch" in self.only:
            wt = self.watch_totals
            self.check(
                "write-watch on cf.expansion over every boot: no stray write",
                wt["write_events"] == 0 and wt["emulators"] > 0,
                str(wt),
            )
            self.check(
                "flush check over every boot: no hit", wt["flush_hits"] == 0, str(wt)
            )
            sel = pk_device.store_of(self.image[:3])
            stray = sorted(self.store_writes - {sel.sel_a, sel.sel_b})
            self.check(
                "the only store-range sectors the guest wrote are the two record sectors",
                not stray,
                f"wrote {sorted(hex(s) for s in self.store_writes)}",
            )
        self.report["checks"] = [
            {"name": a, "ok": b, "detail": c} for a, b, c in self.checks
        ]
        self.report["pass"] = all(c[1] for c in self.checks)
        self.report["seconds"] = round(time.monotonic() - t0, 1)
        (self.work / "report.json").write_text(
            json.dumps(self.report, indent=1, default=str)
        )
        return self.report


def main(argv: list[str] | None = None) -> int:
    ap = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    ap.add_argument("cmd", choices=["proof"])
    ap.add_argument("--image", default="dt2-1.17")
    ap.add_argument(
        "--quick", action="store_true", help="fewer cases in restart and cut"
    )
    ap.add_argument("--keep", type=Path)
    ap.add_argument(
        "--only", default="", help="comma separated: equiv,restart,cut,safe,watch"
    )
    a = ap.parse_args(argv)
    work = a.keep or Path(tempfile.mkdtemp(prefix="pk-dkrt-boot-"))
    proof = Proof(a.image, a.quick, work, {x for x in a.only.split(",") if x})
    try:
        rep = proof.run()
    except (Fail, M.Fail) as e:
        print(f"FAIL {e}", file=sys.stderr)
        return 1
    print(
        json.dumps(
            {k: v for k, v in rep.items() if k != "checks"}, indent=1, default=str
        )
    )
    if not a.keep:  # a temp work dir: removed after a clean run
        if rep["pass"]:
            shutil.rmtree(work, ignore_errors=True)
        else:
            print(f"work dir kept: {work}", file=sys.stderr)
    return 0 if rep["pass"] else 1


if __name__ == "__main__":
    sys.exit(main())
