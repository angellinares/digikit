"""Content-addressed cache of coupled-run scenario renders (report entry + WAV).

A render is keyed by a context key (binary, arguments, input files, semantic
environment, device tomls, device, peeks) and the scenario object. Anything
that can change a render must change the key; when unsure, include it (a miss
costs time, a stale hit hides a bug).

    cache = RenderCache()
    hit = cache.load(scenario_key(context_key(...), scenario))
    cache.store(key, top, entry, wav)

Env: DIGI_RENDER_CACHE=0 disables the cache by default.
DIGI_RENDER_CACHE_MAX_GB (default 30) caps the cache size; store() prunes the
least recently used entries.
"""

from __future__ import annotations

import contextlib
import hashlib
import json
import os
import shutil
import tempfile
from collections.abc import Mapping, Sequence
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]

CACHE_VERSION = 1
DEFAULT_ROOT = ROOT / "out" / "cache" / "renders"

# Names that do not change a render: job counts and timeouts, logging and
# capture sinks, diagnostics. Path selectors are excluded because the files
# they select are hashed through argv, the ready inputs or the binary.
# Build-only variables do not affect a run of an existing binary (the binary
# digest covers them).
DENYLIST: frozenset[str] = frozenset(
    {
        "DIGI_JOBS",
        "DIGI_TIER_JOBS",
        "DIGI_PY_JOBS",
        "DIGI_PY_TIMEOUT",
        "DIGI_CONFORM_JOBS",
        "DIGI_PK_JOBS",
        "DIGI_VERIFY_BUDGET",  # CPU budget of a gate step (the Nx gate sets DIGI_PK_JOBS and DIGI_PY_JOBS)
        "DIGI_RENDER_CACHE",
        "DIGI_RENDER_CACHE_MAX_GB",
        "DIGI_RENDER_CACHE_CHECK",  # cache controls, not render inputs
        "DIGI_DIAG_DIR",
        "DIGI_SPIKE_LOG",
        "DIGI_DSPI_LOG",
        "DIGI_DSP_MODEL_LOG",
        "DIGI_LP0_LOG",
        "DIGI_PANEL_TX_LOG",
        "DIGI_EMU_SINK_WAV",
        "DIGI_EMU_PCM_F32LE",
        "DIGI_EMU_PCM_SECONDS",
        "SHARC_FAST_LOG",
        "SHARC_FAST_COV",
        "SHARC_FAST_HIST",
        "SHARC_FAST_CL_DISASM",
        "SHARC_WWATCH",
        "SHARC_UNINIT_LOG",  # diagnostic output of SHARC_UNINIT (SHARC_UNINIT itself is a render input and stays in the key)
        # path selectors covered by hashing the resolved files
        "DIGI_EMU_SYX",
        "DIGI_COUPLED_SYX",
        "DIGI_EMU_READY_DIR",
        "DIGI_EMU_DSP_IMAGE",
        "DIGI_EMU_DSP_STATE",
        "DIGI_EMU_CF_SNAPSHOT",
        "DIGI_DT2_CARD",
        "DIGI_EMU_FAST",
        # build-only
        "SHARC_GEN_DIR",
        "DIGI_ALLOW_STALE_GEN",
        "DIGI_COUPLED_FEATURES",
    }
)

# Prefixes: SHARC_FAST_TL*, SHARC_FAST_DUMP* and SHARC_PCDUMP* are trace and
# dump outputs; DIGI_DSP_WATCH* are watch-only diagnostics; DIGI_EMU_SYX_* and
# DIGI_EMU_READY_DIR_* are per-device path selectors (like their unsuffixed
# forms); DIGI_EMUD* name a daemon binary. DIGI_DEVICES_DIR, DIGI_CARD_IDENTITY, DIGI_INTC_FORCE,
# DIGI_DSP_PERIOD and DIGI_DSP_RX_PHASE change behavior and stay in the key.
DENY_PREFIXES: tuple[str, ...] = (
    "SHARC_FAST_TL",
    "SHARC_FAST_DUMP",
    "SHARC_PCDUMP",
    "DIGI_DSP_WATCH",
    "DIGI_EMU_SYX_",
    "DIGI_EMU_READY_DIR_",
    "DIGI_EMUD",  # the digi-emud binary the patchkit harness runs: DIGI_EMUD, DIGI_EMUD_DN2, DIGI_EMUD_DT2
)

# Environment variables read by native code linked into coupled-run that do not change
# a render (checked by tests/devtools/test_render_inputs.py). Variables matching the
# key's prefixes need no entry here; anything else read by that code must be listed.
IGNORED_ENV: dict[str, str] = {
    "WASM_HELPER_OUT": "read only by a sharc-fast-wasm unit test (debug output)",
    "DT2_SYX": "read only by a plusdrive unit test to find firmware for fixtures",
    "LIVE_AUDIO_DEVICE_TESTS": "gate of an ignored live-audio unit test",
}

_KEEP_PREFIXES = ("SHARC_", "DIGI_", "DSP_")
_OUTPUT_FLAGS = frozenset({"--out", "--wav", "--dump-dsp"})
_SCENARIO_FLAGS = frozenset({"--scenario", "--scenarios"})

_memo: dict[tuple[str, int, int], str] = {}


def default_enabled() -> bool:
    return os.environ.get("DIGI_RENDER_CACHE") != "0"


def max_bytes() -> int:
    return int(float(os.environ.get("DIGI_RENDER_CACHE_MAX_GB", "30")) * (1 << 30))


def file_digest(path: str | Path) -> str:
    p = Path(path).resolve()
    st = p.stat()
    memo_key = (str(p), st.st_size, st.st_mtime_ns)
    hit = _memo.get(memo_key)
    if hit is not None:
        return hit
    h = hashlib.sha256()
    with p.open("rb") as f:
        for chunk in iter(lambda: f.read(1 << 20), b""):
            h.update(chunk)
    digest = h.hexdigest()
    _memo[memo_key] = digest
    return digest


def _canon(obj: object) -> str:
    return json.dumps(obj, sort_keys=True, separators=(",", ":"))


def _sha(text: str) -> str:
    return hashlib.sha256(text.encode()).hexdigest()


def semantic_env(env: Mapping[str, str]) -> dict[str, str]:
    out: dict[str, str] = {}
    for name in sorted(env):
        if not (name.startswith(_KEEP_PREFIXES) or name == "DN2_PROFILE_LINK"):
            continue
        if name in DENYLIST or name.startswith(DENY_PREFIXES):
            continue
        value = env[name]
        if value.startswith("@") and Path(value[1:]).is_file():
            value = "@sha256:" + file_digest(value[1:])
        out[name] = value
    return out


def devices_digest(env: Mapping[str, str]) -> dict[str, str]:
    d = env.get("DIGI_DEVICES_DIR")
    base = Path(d) if d else ROOT / "devices"
    if not base.is_dir():
        return {}
    return {p.name: file_digest(p) for p in sorted(base.glob("*.toml"))}


def _argv_key(argv: Sequence[str]) -> list[str]:
    out: list[str] = []
    skip = False
    for arg in argv:
        if skip:
            skip = False
            continue
        flag, eq, _ = arg.partition("=")
        if flag in _OUTPUT_FLAGS or flag in _SCENARIO_FLAGS:
            skip = not eq
            continue
        if Path(arg).is_file():
            arg = "file:sha256:" + file_digest(arg)
        out.append(arg)
    return out


def context_key(
    binary: str | Path,
    argv: Sequence[str],
    env: Mapping[str, str],
    device: str,
    peeks: Sequence[str],
) -> str:
    return _sha(
        _canon(
            {
                "v": CACHE_VERSION,
                "binary": file_digest(binary),
                "argv": _argv_key(argv),
                "env": semantic_env(env),
                "devices": devices_digest(env),
                "device": device,
                "peeks": list(peeks),
            }
        )
    )


def scenario_key(context: str, scenario: Mapping[str, object]) -> str:
    return _sha(_canon({"context": context, "scenario": scenario}))


class RenderCache:
    def __init__(self, root: Path = DEFAULT_ROOT, enabled: bool | None = None) -> None:
        self.root = Path(root)
        self.enabled = default_enabled() if enabled is None else enabled

    def _dir(self, key: str) -> Path:
        return self.root / key[:2] / key

    def load(self, key: str) -> tuple[dict, dict, Path] | None:
        if not self.enabled:
            return None
        d = self._dir(key)
        wav = d / "audio.wav"
        try:
            rec = json.loads((d / "record.json").read_text())
        except (OSError, ValueError):
            return None
        if not wav.is_file() or not isinstance(rec, dict):
            return None
        if "top" not in rec or "entry" not in rec:
            return None
        with contextlib.suppress(OSError):
            os.utime(d)  # mtime = last use, for prune
        return rec["top"], rec["entry"], wav

    def store(self, key: str, top: Mapping, entry: Mapping, wav: Path) -> None:
        if not self.enabled or not Path(wav).is_file():
            return
        target = self._dir(key)
        if target.exists():
            return
        target.parent.mkdir(parents=True, exist_ok=True)
        tmp = Path(tempfile.mkdtemp(prefix=".tmp-", dir=target.parent))
        try:
            shutil.copyfile(wav, tmp / "audio.wav")
            record = {
                "v": CACHE_VERSION,
                "top": {k: v for k, v in top.items() if k != "scenarios"},
                "entry": dict(entry),
            }
            (tmp / "record.json").write_text(_canon(record))
            # another writer may win the race; keep theirs
            with contextlib.suppress(OSError):
                os.replace(tmp, target)
        finally:
            if tmp.exists():
                shutil.rmtree(tmp, ignore_errors=True)
        self.prune(max_bytes())

    def prune(self, max_bytes: int) -> int:
        """Remove the least recently used entries until the total is <= max_bytes."""
        sized: list[tuple[float, int, Path]] = []
        for e in self._entries():
            try:
                mtime = e.stat().st_mtime
                size = sum(f.stat().st_size for f in e.glob("*"))
            except OSError:
                continue
            sized.append((mtime, size, e))
        total = sum(s for _, s, _ in sized)
        removed = 0
        for _, size, e in sorted(sized, key=lambda t: t[0]):
            if total <= max_bytes:
                break
            shutil.rmtree(e, ignore_errors=True)
            total -= size
            removed += 1
        return removed

    def _entries(self) -> list[Path]:
        if not self.root.is_dir():
            return []
        return [
            e
            for shard in self.root.iterdir()
            if shard.is_dir() and not shard.name.startswith(".")
            for e in shard.iterdir()
            if e.is_dir() and not e.name.startswith(".")
        ]

    def stats(self) -> dict[str, int]:
        entries = self._entries()
        total = sum(f.stat().st_size for e in entries for f in e.glob("*"))
        return {"entries": len(entries), "bytes": total}

    def clear(self) -> int:
        entries = self._entries()
        for e in entries:
            shutil.rmtree(e, ignore_errors=True)
        return len(entries)
