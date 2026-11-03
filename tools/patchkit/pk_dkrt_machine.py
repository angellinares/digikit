#!/usr/bin/env python3
"""Helpers for the run-time-machine proofs: the panel rig, the PCM comparison and the second implementation of the DSP relocation.

The proof that used to live here (a machine loaded through the mailbox while the firmware ran, against the patched-in one) was
hot-reload only and is archived (tag `digikit-hot-reload-v1`, docs/plans/digikit-hot-reload.md). Its boot-time successor is
tools/patchkit/pk_digikit_scen.py (equivalence scenarios: the same PCM, sample for sample, from a module selected at boot; finding 37),
which uses `Rig`, `pcm_check`, `parse_dsp_section` and `relocate` from here.
"""

from __future__ import annotations

import contextlib
import hashlib
import json
import struct
import sys
import time
from collections.abc import Callable
from pathlib import Path
from typing import Any

import numpy as np

ROOT = Path(__file__).resolve().parents[2]

from tools.coldfire import arena_safety  # noqa: E402
from tools.control.digi_client import Emu  # noqa: E402
from tools.device.dkproto import SETTINGS_SECTORS  # noqa: E402
from tools.patchkit import hook_ab  # noqa: E402
from tools.patchkit import pk_harness as H  # noqa: E402
from tools.patchkit.pk_dkrt import (  # noqa: E402
    ProofError,
)

LOADING = 1
STATE_IDLE, STATE_LOADING = 0, 1
FREE_TYPE = 0xFFFF_FFFF
L2_BYTE_BASE, L2_SW_BASE = 0x2000_0000, 0x00B8_0000
TRACK_OBJ_WORD = 0x4265338C  # the type byte is the third byte of this word (finding 02)
MODULES_STATIC = "core/runtime,core/dynreg,core/dyndsp"
PLUCK_STATIC_TYPE = 9
MODULE = "test/sine"
MACHINE = "sine"  # its machine
# DspState (modules/core/runtime/src/rs/src/dsp.rs), byte offsets of the fields the proof reads
D_MAGIC, D_PHASE, D_STEP, D_TRANSFERS, D_ROW_WRITES, D_FAILURES = 0, 4, 12, 36, 40, 44
D_MOD_OFF, D_MOD_LEN, D_ROW_TYPE, D_ROW_PC, D_ROW_OWNER = 52, 84, 148, 180, 212
# State: last_result at 12; Mailbox: cmd 0, seq 4, result 8, arg 12
ST_LAST = 12
MB_CMD, MB_SEQ, MB_RESULT, MB_ARG = 0, 4, 8, 12
# frame at which the note starts, the same in both runs (dsp_frames counts from the boot)
TRIG_FRAME = 7500
TRIG_FRAME_DN2 = 6000
PRESS_OFFSET = (
    44_000  # ColdFire instructions after the block boundary (a block is 88 000)
)
TRIG1 = "TRIG 1"
PIN_SAMPLES = (
    24000  # interleaved samples a PCM pin covers from the onset (0.25 s of stereo)
)
NOTE_FRAMES = 1500  # 1 s of audio, captured from the trig on


class Fail(RuntimeError):
    pass


def be32(b: bytes, off: int) -> int:
    return struct.unpack_from(">I", b, off)[0]


def machine_hash(module: str, machine: str) -> int:
    """The 24-bit hash of a run-time machine's global id `<module id>:<machine id>`: the low 24 bits of FNV-1a 32 (D7; `register.rs`)."""
    h = 0x811C9DC5
    for b in f"{module}:{machine}".encode():
        h = ((h ^ b) * 0x01000193) & 0xFFFFFFFF
    return h & 0xFFFFFF


def sym(plan: dict, owner: str, name: str) -> int:
    for k, v in plan["symbols"].items():
        if k.startswith(owner + "@") and k.split(":", 1)[1] == name:
            return int(v)
    raise ProofError(f"no symbol {owner}:{name}")


# ---- the second implementation of the DSP relocation -----------------------------------------------------------------------


def parse_dsp_section(dkm: bytes) -> dict[str, Any]:
    off, size = be32(dkm, 0x98), be32(dkm, 0x9C)
    s = dkm[off : off + size]
    if be32(s, 0) != 0x444B4453:
        raise Fail("no DSP section")
    image_off, reloc_off, nreloc = be32(s, 0x18), be32(s, 0x1C), be32(s, 0x20)
    import_off, nimport = be32(s, 0x24), be32(s, 0x28)
    voice_off, nvoice = be32(s, 0x2C), be32(s, 0x30)
    imports = []
    for i in range(nimport):
        e = s[import_off + 32 * i : import_off + 32 * (i + 1)]
        imports.append(e.split(b"\0")[0].decode())
    relocs = []
    for i in range(nreloc):
        o, w = struct.unpack_from(">II", s, reloc_off + 8 * i)
        relocs.append((o, w >> 28, (w >> 24) & 15, w & 0xFFFF))
    version = be32(s, 4)
    # version 1: type, entry; version 2 adds the eight knob ranges; versions 3 to 5 have voices of 64 bytes
    stride = 16 if version == 1 else 48 if version == 2 else 64
    voices = [
        struct.unpack_from(">II", s, voice_off + stride * i) for i in range(nvoice)
    ]
    # version 4: effect records of 64 bytes (docs/design/digikit-dkm.md): process, desc, state_bytes, n_params, flags, p0 .. p7
    fxs = []
    if version in (4, 5):
        for i in range(be32(s, 0x38)):
            at = be32(s, 0x34) + 64 * i
            fxs.append(
                {
                    "process": be32(s, at),
                    "desc": be32(s, at + 4),
                    "state_bytes": be32(s, at + 8),
                    "n_params": be32(s, at + 12),
                    "defaults": [be32(s, at + 20 + 4 * k) for k in range(8)],
                }
            )
    return {
        "device": be32(s, 8),
        "image": bytes(s[image_off : image_off + be32(s, 0x0C)]),
        "bss": be32(s, 0x10),
        "align": be32(s, 0x14),
        "imports": imports,
        "relocs": relocs,
        "voices": voices,
        "fxs": fxs,
    }


def relocate(sec: dict[str, Any], base: int, imports: dict[str, int]) -> bytes:
    m = bytearray(sec["image"])
    pc_shift, byte_shift = (base - L2_BYTE_BASE) // 2, base - L2_BYTE_BASE
    for o, kind, cls, idx in sec["relocs"]:
        add = (
            pc_shift
            if cls == 0
            else byte_shift
            if cls == 1
            else imports[sec["imports"][idx]]
        )
        le16 = lambda a: m[a] | m[a + 1] << 8  # noqa: E731
        if kind == 0:
            v = ((m[o + 2] << 16) | le16(o + 4)) + add
            assert v < 1 << 24
            m[o + 2] = v >> 16 & 0xFF
            m[o + 4 : o + 6] = (v & 0xFFFF).to_bytes(2, "little")
        else:
            v = ((le16(o + 2) << 16) | le16(o + 4)) + add & 0xFFFFFFFF
            m[o + 2 : o + 4] = (v >> 16).to_bytes(2, "little")
            m[o + 4 : o + 6] = (v & 0xFFFF).to_bytes(2, "little")
    return bytes(m)


def runtime_watch_env(
    plan_file: Path, out: Path, *, sequential: bool = False
) -> dict[str, str]:
    """The runtime proof's existing DMA holes and allowed writer PCs, before a daemon starts."""
    plan = json.loads(plan_file.read_text())
    lo, hi = arena_safety.expansion(
        plan["base"]["device"] + "-" + plan["base"]["version"]
    )
    # the card reads land by DMA in the runtime's staging buffer (and the 512-byte record buffer): left out of the watch
    holes = [(sym(plan, "core/runtime", "DKRT_STAGE"), 64 * 1024)]
    with contextlib.suppress(
        ProofError
    ):  # an image of an older runtime has no record buffer
        holes.append((sym(plan, "core/runtime", "DKRT_SECBUF"), 512))
    with contextlib.suppress(
        ProofError
    ):  # nor the upload buffer (36,896 bytes, card reads of the store land in it)
        holes.append((sym(plan, "core/runtime", "DKRT_UPLOAD_BUF"), 36896))
    with contextlib.suppress(
        ProofError
    ):  # nor the store-peek sector (the card read of the store's header DMAs into it)
        holes.append((sym(plan, "core/runtime", "DKRT_STORE_PEEK"), 512))
    with contextlib.suppress(ProofError):  # the fingerprint sector of a store list
        holes.append((sym(plan, "core/runtime", "DKRT_STORE_FP"), 512))
    with contextlib.suppress(
        ProofError
    ):  # the settings record's RAM copy and its header sector: the boot's card read of the settings area lands in them by DMA
        holes.append(
            (sym(plan, "core/runtime", "DKRT_SETTINGS"), SETTINGS_SECTORS * 512)
        )
        holes.append((sym(plan, "core/runtime", "DKRT_SETTINGS_HDR"), 512))
    ranges, at = [], lo
    for start, size in sorted(holes):
        if start > at:  # adjacent holes leave no range between them
            ranges.append("0x%x-0x%x" % (at, start))
        at = start + size
    ranges.append("0x%x-0x%x" % (at, hi))
    env = {
        "DIGI_WRITE_WATCH": ",".join(ranges),
        "DIGI_WRITE_WATCH_AFTER": "cacr",
        "DIGI_WRITE_WATCH_ALLOW_PC": arena_safety.plan_allow_pc(plan_file),
        "DIGI_FLUSH_CHECK": "1",
        "DIGI_WRITE_WATCH_OUT": str(out),
    }
    if sequential:
        env["DIGI_WRITE_WATCH_SEQ"] = "1"
    return env


# ---- sessions --------------------------------------------------------------------------------------------------------------


class FolderSession(H.Session):
    """Session whose card is built from a +Drive folder (the module store is `digikit/modules` in it)."""

    folder: Path
    card_img: Path | None = None

    def open(self, image: Path | None = None) -> None:
        self.syx, self.ready = H.firmware_inputs(self.cell)
        exe = H.daemon_binary(self.cell.device, self.syx)
        if self.run_dir is None:
            self.run_dir = H.temp_run_dir(self.run_name)
            self._temp_run_dir = True
        run_dir = self.run_dir
        if (
            self.cache_dir is None
        ):  # the boot caches outlive the run dir; `load` locks them
            self.cache_dir = H.SHARED_CACHE
        H.link_shared(run_dir, self.cache_dir)
        self.emu = Emu.spawn_coupled(
            self.syx, self.ready, timeout=300, binary=exe, run_dir=run_dir, env=self.env
        )
        self.emu.subscribe()
        # the Digitone II daemon takes a card image built from the folder (the Digitakt II builds it from the folder itself)
        self.card = (
            str(self.card_img)
            if self.card_img
            else self.emu.call("card.load", dir=str(self.folder))["card"]
        )
        self.load(image)


def has_rt(plan: dict) -> bool:
    return any(k.startswith("core/runtime@") for k in plan["symbols"])


class Rig:
    """One image, one daemon: the helpers both kinds of run share."""

    session_cls: type[FolderSession] = (
        FolderSession  # a subclass picks another kind of daemon (pk_dkrt_boot.CfBootRig)
    )

    def __init__(self, tag: str, image: str, modules: str, plan: dict, syx: Path, folder: Path, work: Path, watch: bool,
                 card_img: Path | None = None, extra_env: dict[str, str] | None = None):  # fmt: skip
        self.tag, self.image, self.plan, self.work = tag, image, plan, work
        self.device, self.fw = image[:3], image[4:]
        cell = H.Cell(self.device, self.fw, modules.split(","))
        env: dict[str, str] = dict(extra_env or {})
        self.summary: Path | None = None
        if watch:
            out = work / f"{tag}.writewatch.jsonl"
            self.summary = out.with_suffix(".summary.json")
            env |= runtime_watch_env(syx.with_suffix(".plan.json"), out)
        self.s = self.session_cls(
            cell,
            H.Goldens(work / f"{tag}.goldens.json"),
            run_name=f"dkm-{tag}-{image}",
            env=env,
        )
        self.s.folder = folder
        self.s.card_img = card_img
        self.syx = syx
        self.checks: list[tuple[str, bool, str]] = []
        # (a reference image without the runtime, `ref_plain` scenarios, has none of these)
        self.mb = sym(plan, "core/runtime", "DKRT_MAILBOX") if has_rt(plan) else 0
        self.state = sym(plan, "core/runtime", "DKRT_STATE") if has_rt(plan) else 0
        self.dsp = sym(plan, "core/runtime", "DKRT_DSP") if has_rt(plan) else 0

    def check(self, name: str, ok: bool, detail: str = "") -> None:
        self.checks.append((f"{self.tag}: {name}", bool(ok), detail))
        print(f"{'ok  ' if ok else 'FAIL'} {self.tag}: {name} {detail}", flush=True)

    @property
    def emu(self) -> Any:
        assert self.s.emu is not None
        return self.s.emu

    def w(self, addr: int) -> int:
        return self.emu.peek32(addr) or 0

    def frames(self) -> int:
        return int(self.emu.call("status")["dsp_frames"])

    def start(self) -> None:
        self.s.open(self.syx)
        self.s.segment = "dkm"
        self.s.reset()
        self.s.run_ms(1500)

    # -- the runtime's mailbox
    def post(self, cmd: int, arg: str) -> int:
        b = arg.encode() + b"\0"
        b += bytes(-len(b) % 4)
        for i in range(0, len(b), 4):
            self.emu.poke32(self.mb + MB_ARG + i, struct.unpack(">I", b[i : i + 4])[0])
        seq0 = self.w(self.mb + MB_SEQ)
        self.emu.poke32(self.mb + MB_CMD, cmd)
        for _ in range(200):
            self.s.run_ms(20)
            if self.w(self.mb + MB_CMD) == 0 and self.w(self.mb + MB_SEQ) != seq0:
                break
        r = self.w(self.mb + MB_RESULT)
        return r - (1 << 32) if r >= 1 << 31 else r

    def last_result(self) -> int:
        r = self.w(self.state + ST_LAST)
        return r - (1 << 32) if r >= 1 << 31 else r

    def dsp_phase(self) -> int:
        return self.w(self.dsp + D_PHASE)

    def dsp_bytes(self, addr: int, n: int) -> bytes:
        out = bytearray()
        for off in range(0, n, 4096):
            h = self.emu.dsp_peek(addr + off, min(4096, n - off))["hex"]
            out += bytes.fromhex(h.replace("??", "00"))
        return bytes(out)

    def info_word(self, k: int) -> int:
        return self.w(sym(self.plan, "core/dyndsp", "dyndsp_info") + 4 * k)

    def voice_row(self, k: int) -> tuple[int, int]:
        """(type, pc) of voice row k; a row is 24 words (core/dsp-voice module.toml), the first two are these."""
        rows = sym(self.plan, "core/dsp-voice", "voices.rows")
        first = self.info_word(6)
        raw = self.dsp_bytes(rows + 96 * (first + k), 8)
        return struct.unpack(
            "<II", raw
        )  # dsp_peek returns memory bytes: DSP words are little endian

    # -- the firmware's machine rows (core/machines): the name pointer marks a row as used
    def machine_listed(self, typ: int) -> bool:
        rows = sym(self.plan, "core/machines", "machines.rows")
        first = sym(self.plan, "core/machines", "machines.first")
        size = sym(self.plan, "core/machines", "machines.row_size")
        name = self.w(rows + (typ - first) * size + 56)
        return bool(name) and (self.w(name) >> 24) != 0

    def type_now(self) -> int:
        return (self.w(TRACK_OBJ_WORD) >> 8) & 0xFF

    def select(self, downs: int, ups: int = 0) -> None:
        """Open the machine list, press UP UPS times (the cursor starts at the track's current machine; UP stops at the top) and DOWN `downs` times, commit."""
        s = self.s
        s.do_tap(["FUNC", "SRC"])
        s.run_ms(200)
        for _ in range(ups):
            s.do_tap("UP")
            s.run_ms(80)
        for _ in range(downs):
            s.do_tap("DOWN")
            s.run_ms(80)
        s.do_tap("YES")
        s.run_ms(300)
        s.do_tap("YES")
        s.run_ms(300)

    def pick(self, want: int, hint: int) -> int:
        """Put the machine of type WANT on track 1. Digitakt II: the list position is tried until the track's type is WANT (the type word is read). Digitone II: the list
        position HINT (finding 33); no type word is read there, the sound proves the choice."""
        if self.device == "dt2":
            return self.select_type(want, hint)
        self.select(hint)
        return hint

    def select_type(self, want: int, hint: int) -> int:
        """Open the list, move DOWN `downs` times and commit; try the hint and its neighbours until the track has WANT."""
        for downs in (hint, hint + 1, hint - 1, hint + 2, hint + 3):
            self.select(downs)
            if self.type_now() == want:
                return downs
        raise Fail(
            f"{self.tag}: no list position gives type {want}; track has {self.type_now()}"
        )

    def note(
        self, wav: Path, trig_frame: int, during: Callable[[], None] | None = None
    ) -> np.ndarray:
        """Play TRIG 1 from the DSP block `trig_frame`, the same instruction after the block's boundary in every run. DURING runs
        once after the key is down, inside the capture (an upload while the note sounds); it must end before frame `trig_frame` + `NOTE_FRAMES`."""
        emu = self.emu
        if self.frames() >= trig_frame - 20:
            raise Fail(
                f"{self.tag}: setup took until frame {self.frames()}; raise TRIG_FRAME"
            )
        emu.call("run_until", frames=trig_frame)
        # half a block on: the key reaches the sequencer well away from a block boundary, so images whose ColdFire code differs by a
        # few hundred instructions on the key path still start the note in the same block
        emu.step(PRESS_OFFSET)
        emu.call("pcm.tap", action="capture", path=str(wav))
        emu.button(H.button(TRIG1), True)
        if during is not None:
            during()
        # the key stays down for the whole capture: the engine's release would reach the two images a block apart (the
        # ColdFire code on the key path differs) and shape the tail by a few 1e-5; nothing time-dependent is left in the comparison
        emu.call("run_until", frames=trig_frame + NOTE_FRAMES)
        emu.call("pcm.tap", action="capture_stop")
        emu.button(H.button(TRIG1), False)
        self.s.run_ms(50)
        self.s.health(f"note {self.tag}")
        return hook_ab.read_wav(wav)

    def finish(self) -> dict[str, Any]:
        """Close the daemon (which writes the write-watch summary) and read what it saw."""
        overlay = self.work / f"{self.tag}.overlay.bin"
        try:
            self.emu.call("card.overlay", path=str(overlay))
        except Exception as e:  # noqa: BLE001
            print(f"card.overlay: {e}", file=sys.stderr)
        self.s.close()
        out: dict[str, Any] = {}
        if self.summary is not None:
            for _ in range(50):
                if self.summary.exists():
                    break
                time.sleep(0.2)
            if self.summary.exists():
                d = json.loads(self.summary.read_text())
                out["write_events"] = d["events"]
                out["flush_hits"] = len(d["flush_hits"])
                out["cpushl"] = d["cpushl"]
                out["per_pc"] = d["per_pc"][:4]
        if overlay.exists():
            out["store_sector_writes"] = len(arena_safety.card_store_hits(overlay))
        return out


def pcm_check(
    run: Rig,
    ref_pcm: np.ndarray,
    run_pcm: np.ndarray,
    report: dict[str, Any],
    what: str,
    until: int | None = None,
    max_shift: int = 128,
) -> None:
    """The run-time PCM equals the reference's: exactly, or sample for sample from the note's onset on. The two images do not
    reach the audition at the same ColdFire instruction (the run-time image has loaded a module, the Digitone II reference has
    a machine module's code on the key path) and the PCM tap starts at the block in progress, so the capture may start up to a
    few frames apart; the sound, from the onset on, must not differ in a single sample. UNTIL (interleaved samples after the onset)
    cuts the comparison before the key is released: on the Digitone II the release reaches the engine a block apart in the two images.
    MAX_SHIFT is the largest onset shift (samples) that is accepted."""
    exact = len(run_pcm) == len(ref_pcm) and bool(np.array_equal(run_pcm, ref_pcm))
    shift, aligned_differ = aligned_compare(ref_pcm, run_pcm, until)
    report["pcm"] = {
        "samples": len(run_pcm),
        "exact": exact,
        "onset_shift_samples": shift,
        "differ_after_alignment": aligned_differ,
        "compared_samples_after_onset": until,
        "peak": float(abs(run_pcm).max()),
    }
    run.check(
        f"PCM of the run-time {what} equals the patched-in {what}'s (exact, or sample for sample from the onset on)",
        (exact and until is None)
        or (
            shift is not None
            and shift % 2 == 0
            and abs(shift) <= max_shift
            and aligned_differ == 0
        ),
        f"{len(run_pcm)} samples, exact {exact}, onset shift {shift}, {aligned_differ} differ after alignment",
    )


def pcm_pin(
    run: Rig,
    run_pcm: np.ndarray,
    pin: str,
    report: dict[str, Any],
    what: str,
    n: int = PIN_SAMPLES,
) -> None:
    """The run-time PCM from the note's onset on, N interleaved samples as little-endian float32, has the SHA-256 PIN.

    The onset is the first sample above 1e-6 (as `aligned_compare`); it must lie on a whole stereo frame, and the N samples must
    end before the key is released (the release reaches the engine a block apart in two images). PIN "record" fails and reports
    the hash, to record a pin: the value is approved by Em, not by a run."""
    nz = np.flatnonzero(np.abs(run_pcm) > 1e-6)
    onset = int(nz[0]) if len(nz) else -1
    window = run_pcm[onset : onset + n] if onset >= 0 else run_pcm[:0]
    got = hashlib.sha256(
        np.ascontiguousarray(window, dtype="<f4").tobytes()
    ).hexdigest()
    report["pcm_pin"] = {
        "sha256": got,
        "onset": onset,
        "samples": int(len(window)),
        "peak": float(abs(window).max()) if len(window) else 0.0,
    }
    run.check(
        f"PCM of the run-time {what} from the onset ({onset}) over {n} samples has the pinned SHA-256",
        onset >= 0 and onset % 2 == 0 and len(window) == n and got == pin,
        f"sha256 {got}, onset {onset}, {len(window)} samples",
    )


def aligned_compare(
    ref: np.ndarray, run: np.ndarray, until: int | None = None
) -> tuple[int | None, int]:
    """(onset of RUN minus onset of REF in samples, samples that differ over the common length from the onsets on)."""

    def onset(x: np.ndarray) -> int:
        nz = np.flatnonzero(np.abs(x) > 1e-6)
        return int(nz[0]) if len(nz) else -1

    a, b = onset(ref), onset(run)
    if a < 0 or b < 0:
        return None, -1
    n = min(len(ref) - a, len(run) - b)
    if until is not None:
        n = min(n, until)
    return b - a, int((ref[a : a + n] != run[b : b + n]).sum())


def run_imports(run: Rig, plan: dict) -> dict[str, int]:
    """The DSP symbols `dyndsp_info` exports (name, value), as the ColdFire sees them."""
    out = {}
    info = sym(plan, "core/dyndsp", "dyndsp_info")
    for i in range(run.info_word(9)):
        raw = b"".join(
            run.w(info + 40 + 32 * i + 4 * k).to_bytes(4, "big") for k in range(8)
        )
        out[raw[:28].split(b"\0")[0].decode()] = int.from_bytes(raw[28:32], "big")
    return out
