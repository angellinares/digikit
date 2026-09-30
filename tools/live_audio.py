"""ctypes wrapper for ``native/live``, the live audio output path (a cpal
real-time callback, a lock-free ring and the SPI2 TX frame repeater --
``native/live/src/lib.rs`` module docs; not the SHARC core itself, see
``native/sharc``, a separate lane).

Build the library first::

    cd native/live && cargo build --release

Then::

    from tools.live_audio import LiveAudio

    with LiveAudio() as audio:
        audio.play("out/listen/hat-trig1-emac-voices.wav")
        time.sleep(1)
        print(audio.stats())

The native SHARC core, live, on a capture (the pack comes from
``tools/sharc_transpile_run.py live-pack``, built in Python once and cached
under out/native/live/; nothing in the audio path runs Python)::

    with LiveAudio(capture="out/captures/drive3/dt2-1.16-drive3-trig1-emac.dt2cap") as a:
        time.sleep(10)
        print(a.stats(), a.render_stats())

or ``uv run python tools/live_audio.py --capture CAPTURE [--seconds S]``.

The native SHARC core on frames pushed live (the emulator's DSPI2 frames,
``emu/livesharc.py``): a frameless state pack (``state_pack``) and
``push_frame`` for every TX frame, in wire order::

    pack = state_pack("out/captures/drive3/flexbus-drive3.raw", card_sha256)
    with LiveAudio(frames_pack=pack, card_sha256=card_sha256) as a:
        a.push_frame(tx)          # every frame the ColdFire sends
        print(a.frame_stats(), a.render_stats())

``device=False`` opens it with no output device; ``render(n)`` then pulls
N SHARC frames (a headless check).

``LiveAudio()`` raises ``FileNotFoundError`` if the library has not been
built -- callers that want to skip cleanly (tests, in particular) should
catch that rather than assume the library exists (see ``tests/
test_live_audio.py``, which skips the whole module this way).
"""

from __future__ import annotations

import array
import ctypes
import os
import platform
import sys
from dataclasses import dataclass
from pathlib import Path

_ROOT = Path(__file__).resolve().parent.parent
_CRATE_DIR = _ROOT / "native" / "live"
DEFAULT_SHARC_LIB = Path(
    os.environ.get(
        "SHARC_NATIVE_LIB",
        _ROOT / "out/native/opt/target-final/release/libsharc_native.dylib",
    )
)


def _default_library_path() -> Path:
    system = platform.system()
    if system == "Darwin":
        name = "liblive_audio.dylib"
    elif system == "Linux":
        name = "liblive_audio.so"
    elif system == "Windows":
        name = "live_audio.dll"
    else:
        raise RuntimeError("live_audio: unsupported platform %r" % system)
    return _CRATE_DIR / "target" / "release" / name


class LiveAudioError(RuntimeError):
    """A ``native/live`` C ABI call failed. ``str(exc)`` is the message
    ``live_last_error`` reported, if any."""


@dataclass(frozen=True)
class LiveStats:
    """Mirrors ``native/live/src/abi.rs``'s ``LiveStats`` (repr(C))."""

    sample_rate: int
    requested_sample_rate: int
    channels: int
    used_fallback: bool
    target_latency_frames: int
    ring_capacity_frames: int
    ring_fill_frames: int
    underruns: int
    frames_rendered: int


class _CLiveStats(ctypes.Structure):
    _fields_ = [
        ("sample_rate", ctypes.c_uint32),
        ("requested_sample_rate", ctypes.c_uint32),
        ("channels", ctypes.c_uint16),
        ("used_fallback", ctypes.c_uint8),
        ("_pad", ctypes.c_uint8),
        ("target_latency_frames", ctypes.c_uint32),
        ("ring_capacity_frames", ctypes.c_uint32),
        ("ring_fill_frames", ctypes.c_uint32),
        ("underruns", ctypes.c_uint64),
        ("frames_rendered", ctypes.c_uint64),
    ]


@dataclass(frozen=True)
class RenderStats:
    """Mirrors ``native/live/src/abi.rs``'s ``LiveRenderStats``: the SHARC
    source's per-frame render times (µs; frame 0, the cold start, is
    ``first_us`` and left out of the percentiles) and interpreter use."""

    frames: int
    clean: int
    stopped: int
    single_steps: int
    frames_with_single_steps: int
    instructions: int
    median_us: float
    p99_us: float
    max_us: float
    mean_us: float
    first_us: float
    nonzero_frames: int
    first_nonzero: int
    idle_frames: int


class _CRenderStats(ctypes.Structure):
    _fields_ = (
        [
            (name, ctypes.c_uint64)
            for name in (
                "frames",
                "clean",
                "stopped",
                "single_steps",
                "frames_with_single_steps",
                "instructions",
            )
        ]
        + [
            (name, ctypes.c_double)
            for name in ("median_us", "p99_us", "max_us", "mean_us", "first_us")
        ]
        + [
            ("nonzero_frames", ctypes.c_uint64),
            ("first_nonzero", ctypes.c_int64),
            ("idle_frames", ctypes.c_uint64),
        ]
    )


@dataclass(frozen=True)
class FrameStats:
    """Mirrors ``native/live/src/abi.rs``'s ``LiveFrameStats``: the frame
    queue's counters (``native/live/src/repeater.rs``)."""

    pushed: int
    trig_pushed: int
    taken: int
    trig_taken: int
    repeats: int
    merged: int
    max_depth: int
    depth: int


class _CFrameStats(ctypes.Structure):
    _fields_ = [(name, ctypes.c_uint64) for name in FrameStats.__dataclass_fields__]


# (export name, argtypes, restype) -- see native/live/src/abi.rs's own
# per-function doc comments for the full contract each one implements.
_FUNCTIONS = (
    ("live_open", [ctypes.c_uint32], ctypes.c_void_p),
    ("live_close", [ctypes.c_void_p], None),
    (
        "live_play_tone",
        [ctypes.c_void_p, ctypes.c_float, ctypes.c_float],
        ctypes.c_int32,
    ),
    (
        "live_play_wav",
        [ctypes.c_void_p, ctypes.c_char_p, ctypes.c_int32],
        ctypes.c_int32,
    ),
    (
        "live_push_frame",
        [ctypes.c_void_p, ctypes.c_char_p, ctypes.c_size_t],
        ctypes.c_int32,
    ),
    (
        "live_stats",
        [ctypes.c_void_p, ctypes.POINTER(_CLiveStats)],
        ctypes.c_int32,
    ),
    (
        "live_device_name",
        [ctypes.c_void_p, ctypes.c_char_p, ctypes.c_size_t],
        ctypes.c_int32,
    ),
    ("live_last_error", [ctypes.c_char_p, ctypes.c_size_t], ctypes.c_int32),
    (
        "live_open_capture",
        [
            ctypes.c_uint32,
            ctypes.c_char_p,
            ctypes.c_char_p,
            ctypes.c_uint32,
            ctypes.c_int32,
            ctypes.c_float,
        ],
        ctypes.c_void_p,
    ),
    (
        "live_render_stats",
        [ctypes.c_void_p, ctypes.POINTER(_CRenderStats)],
        ctypes.c_int32,
    ),
    (
        "live_first_stop",
        [ctypes.c_void_p, ctypes.c_char_p, ctypes.c_size_t],
        ctypes.c_int32,
    ),
    (
        "live_open_frames",
        [
            ctypes.c_uint32,
            ctypes.c_char_p,
            ctypes.c_char_p,
            ctypes.c_char_p,
            ctypes.c_float,
            ctypes.c_int32,
        ],
        ctypes.c_void_p,
    ),
    (
        "live_render",
        [
            ctypes.c_void_p,
            ctypes.c_uint32,
            ctypes.POINTER(ctypes.c_float),
            ctypes.c_size_t,
        ],
        ctypes.c_int32,
    ),
    (
        "live_frame_stats",
        [ctypes.c_void_p, ctypes.POINTER(_CFrameStats)],
        ctypes.c_int32,
    ),
)

# Samples per SHARC frame (native/live's FRAME_LEN).
FRAME_SAMPLES = 32


def capture_pack(
    capture: str | os.PathLike[str],
    *,
    lp0: str | os.PathLike[str] | None = None,
    image: str = "dt2-1.16",
    start_frame: int = 74,
) -> str:
    """The live pack for CAPTURE (``sharc_transpile_run.live_pack``: built
    once, about 2 min of Python, then cached by image, capture, LP0 log and
    core hashes). LP0 defaults to the one ``flexbus*.raw`` next to the
    capture, if there is exactly one."""
    if lp0 is None:
        found = sorted(Path(capture).resolve().parent.glob("flexbus*.raw"))
        lp0 = found[0] if len(found) == 1 else None
    tools = str(_ROOT / "tools")
    if tools not in sys.path:
        sys.path.insert(0, tools)
    import sharc_transpile_run

    info = sharc_transpile_run.live_pack(
        image,
        os.fspath(capture),
        os.fspath(lp0) if lp0 is not None else None,
        start_frame=start_frame,
    )
    return str(info["path"])


def state_pack(
    lp0: str | os.PathLike[str] | None,
    card_sha256: str | None,
    *,
    image: str = "dt2-1.16",
) -> str:
    """The frameless live pack for frames pushed live
    (``sharc_transpile_run.state_pack``: the start state with the LP0 feed
    of LP0, the FlexBus log recorded from the card image CARD_SHA256; built
    once, about 1.5-2 min of Python, then cached by image, LP0 log, card and
    core hashes)."""
    tools = str(_ROOT / "tools")
    if tools not in sys.path:
        sys.path.insert(0, tools)
    import sharc_transpile_run

    info = sharc_transpile_run.state_pack(
        image, os.fspath(lp0) if lp0 is not None else None, card_sha256
    )
    return str(info["path"])


_ERROR_BUF_SIZE = 512
_NAME_BUF_SIZE = 256


class LiveAudio:
    """One open output device plus its producer/ring/repeater, driven
    through ``native/live``'s C ABI. Not thread-safe to call from more
    than one Python thread at a time (the native side has its own
    real-time thread and producer thread; this wrapper's calls are all
    plain, quick ABI calls, never on the audio path itself)."""

    def __init__(
        self,
        target_latency_frames: int = 512,
        library_path: str | os.PathLike[str] | None = None,
        *,
        capture: str | os.PathLike[str] | None = None,
        pack: str | os.PathLike[str] | None = None,
        sharc_lib: str | os.PathLike[str] | None = None,
        gap_frames: int = 0,
        hold: bool = False,
        gain: float = 1.0,
        frames_pack: str | os.PathLike[str] | None = None,
        card_sha256: str | None = None,
        device: bool = True,
    ) -> None:
        """Open the default output device on silence, or, with CAPTURE (a
        .dt2cap, see ``capture_pack``) or PACK (a built live pack), on the
        native SHARC core playing it live (``live_open_capture``: loops
        after GAP_FRAMES held frames, or HOLD the last frame), or, with
        FRAMES_PACK (a state pack, see ``state_pack``), on the native SHARC
        core rendering the frames ``push_frame`` queues
        (``live_open_frames``; CARD_SHA256, when given, must be the card
        the pack was built for). DEVICE=False (FRAMES_PACK only): no
        output device; ``render`` pulls frames."""
        path = (
            Path(library_path) if library_path is not None else _default_library_path()
        )
        if not path.is_file():
            raise FileNotFoundError(
                "no native/live library at %r -- build it first: "
                "cd native/live && cargo build --release" % str(path)
            )
        self._lib = ctypes.CDLL(str(path))
        for name, argtypes, restype in _FUNCTIONS:
            try:
                fn = getattr(self._lib, name)
            except AttributeError as exc:
                raise AttributeError(
                    "%r is missing expected export %r" % (str(path), name)
                ) from exc
            fn.argtypes = argtypes
            fn.restype = restype

        self._handle = None
        if capture is not None and pack is None:
            pack = capture_pack(capture)
        if frames_pack is not None:
            lib = Path(sharc_lib) if sharc_lib is not None else DEFAULT_SHARC_LIB
            if not lib.is_file():
                raise FileNotFoundError("no native SHARC core library at %r" % str(lib))
            self._handle = self._lib.live_open_frames(
                target_latency_frames,
                os.fspath(lib).encode("utf-8"),
                os.fspath(frames_pack).encode("utf-8"),
                card_sha256.encode("ascii") if card_sha256 else None,
                float(gain),
                1 if device else 0,
            )
        elif pack is not None:
            lib = Path(sharc_lib) if sharc_lib is not None else DEFAULT_SHARC_LIB
            if not lib.is_file():
                raise FileNotFoundError("no native SHARC core library at %r" % str(lib))
            self._handle = self._lib.live_open_capture(
                target_latency_frames,
                os.fspath(lib).encode("utf-8"),
                os.fspath(pack).encode("utf-8"),
                gap_frames,
                1 if hold else 0,
                float(gain),
            )
        else:
            self._handle = self._lib.live_open(target_latency_frames)
        if not self._handle:
            raise LiveAudioError(self._last_error() or "live_open failed")

    # -- lifecycle ---------------------------------------------------

    def close(self) -> None:
        if self._handle:
            self._lib.live_close(self._handle)
            self._handle = None

    def __enter__(self) -> LiveAudio:
        return self

    def __exit__(self, *exc_info: object) -> None:
        self.close()

    def __del__(self) -> None:
        # Best-effort: __init__ may have raised before self._lib/_handle
        # were set, or the interpreter may be tearing down.
        lib = getattr(self, "_lib", None)
        handle = getattr(self, "_handle", None)
        if lib is not None and handle:
            lib.live_close(handle)
            self._handle = None

    def _check_open(self) -> None:
        if not self._handle:
            raise LiveAudioError("LiveAudio is closed")

    def _last_error(self) -> str:
        buf = ctypes.create_string_buffer(_ERROR_BUF_SIZE)
        n = self._lib.live_last_error(buf, _ERROR_BUF_SIZE)
        return buf.raw[: max(n, 0)].decode("utf-8", "replace")

    # -- playback ------------------------------------------------------

    def play(
        self,
        path: str | os.PathLike[str] | None = None,
        *,
        looping: bool = False,
        tone_hz: float = 440.0,
        amplitude: float = 0.3,
    ) -> None:
        """Swap in a new frame source: a WAV/PCM file if PATH is given,
        else a sine test tone at TONE_HZ/AMPLITUDE."""
        self._check_open()
        if path is not None:
            encoded = os.fspath(path).encode("utf-8")
            rc = self._lib.live_play_wav(self._handle, encoded, 1 if looping else 0)
            if rc != 0:
                raise LiveAudioError(
                    self._last_error() or "live_play_wav failed (rc=%d)" % rc
                )
        else:
            rc = self._lib.live_play_tone(
                self._handle, float(tone_hz), float(amplitude)
            )
            if rc != 0:
                raise LiveAudioError(
                    self._last_error() or "live_play_tone failed (rc=%d)" % rc
                )

    def push_frame(self, frame: bytes) -> None:
        """Queue one SPI2 TX frame, wire order (big-endian halfwords, as
        ``emu/dspi2.py`` hands it over; see ``native/live/src/repeater.rs``)."""
        self._check_open()
        rc = self._lib.live_push_frame(self._handle, frame, len(frame))
        if rc != 0:
            raise LiveAudioError(
                self._last_error() or "live_push_frame failed (rc=%d)" % rc
            )

    def render(self, frames: int) -> array.array:
        """No-device player only: render FRAMES SHARC frames (the queued
        frames, else repeats) -> interleaved L, R float32 samples."""
        self._check_open()
        out = array.array("f", bytes(4 * 2 * FRAME_SAMPLES * frames))
        buf = (ctypes.c_float * len(out)).from_buffer(out)
        rc = self._lib.live_render(self._handle, frames, buf, len(out))
        del buf
        if rc < 0:
            raise LiveAudioError(
                self._last_error() or "live_render failed (rc=%d)" % rc
            )
        return out

    def frame_stats(self) -> FrameStats:
        """The frame queue's counters (frames pushed, rendered, repeated,
        merged; see ``native/live/src/repeater.rs``)."""
        self._check_open()
        c = _CFrameStats()
        rc = self._lib.live_frame_stats(self._handle, ctypes.byref(c))
        if rc != 0:
            raise LiveAudioError("live_frame_stats failed (rc=%d)" % rc)
        names = FrameStats.__dataclass_fields__
        return FrameStats(**{name: getattr(c, name) for name in names})

    def stats(self) -> LiveStats:
        self._check_open()
        c_stats = _CLiveStats()
        rc = self._lib.live_stats(self._handle, ctypes.byref(c_stats))
        if rc != 0:
            raise LiveAudioError(self._last_error() or "live_stats failed (rc=%d)" % rc)
        return LiveStats(
            sample_rate=c_stats.sample_rate,
            requested_sample_rate=c_stats.requested_sample_rate,
            channels=c_stats.channels,
            used_fallback=bool(c_stats.used_fallback),
            target_latency_frames=c_stats.target_latency_frames,
            ring_capacity_frames=c_stats.ring_capacity_frames,
            ring_fill_frames=c_stats.ring_fill_frames,
            underruns=c_stats.underruns,
            frames_rendered=c_stats.frames_rendered,
        )

    def render_stats(self) -> RenderStats:
        """The SHARC source's render summary (capture and frames players)."""
        self._check_open()
        c = _CRenderStats()
        rc = self._lib.live_render_stats(self._handle, ctypes.byref(c))
        if rc != 0:
            raise LiveAudioError("live_render_stats failed (rc=%d)" % rc)
        names = RenderStats.__dataclass_fields__
        return RenderStats(**{name: getattr(c, name) for name in names})

    def first_stop(self) -> str | None:
        """The first SHARC frame stop's index and halt reason, when present."""
        self._check_open()
        buf = ctypes.create_string_buffer(_ERROR_BUF_SIZE)
        n = self._lib.live_first_stop(self._handle, buf, len(buf))
        if n < 0:
            raise LiveAudioError("live_first_stop failed (rc=%d)" % n)
        return buf.raw[:n].decode("utf-8", "replace") if n else None

    def device_name(self) -> str:
        self._check_open()
        buf = ctypes.create_string_buffer(_NAME_BUF_SIZE)
        n = self._lib.live_device_name(self._handle, buf, _NAME_BUF_SIZE)
        if n < 0:
            raise LiveAudioError(
                self._last_error() or "live_device_name failed (rc=%d)" % n
            )
        return buf.raw[:n].decode("utf-8", "replace")


def _main(argv: list[str]) -> int:
    """Minimal manual smoke check: `uv run python tools/live_audio.py FILE`,
    or `... --capture CAPTURE [--seconds S]` for the SHARC core live."""
    import time

    if len(argv) > 2 and argv[1] == "--capture":
        seconds = (
            float(argv[argv.index("--seconds") + 1]) if "--seconds" in argv else 10.0
        )
        with LiveAudio(capture=argv[2]) as audio:
            print("device:", audio.device_name())
            time.sleep(seconds)
            print(audio.stats())
            print(audio.render_stats())
        return 0

    with LiveAudio() as audio:
        if len(argv) > 1:
            audio.play(argv[1])
            run_seconds = 2.0
        else:
            audio.play(tone_hz=440.0, amplitude=0.3)
            run_seconds = 1.0
        print("device:", audio.device_name())
        time.sleep(run_seconds)
        print(audio.stats())
    return 0


if __name__ == "__main__":
    raise SystemExit(_main(sys.argv))
