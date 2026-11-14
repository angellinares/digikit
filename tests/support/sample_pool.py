"""PCM planes and pool records for the machine rigs, written the way the runtime does: planes in free L2, then the record, with `ready` last.

Used by modules that play a sample from the pool (a sample machine): their playback tests and the conformance worker."""

import random

from support.machine_rig import nw

PLANE_L = nw(0x200F0000)  # normal-word addresses of the test PCM planes (free L2)
PLANE_R = nw(0x200F8000)
TOKEN = 7


def pcm(seed: int, n: int = 200):
    """n int16 samples of numpy's generator (the playback test compares them); numpy is imported here, as the conformance worker runs without it."""
    import numpy as np

    rng = np.random.default_rng(seed)
    return rng.integers(-32768, 32768, n).astype(np.int16)


def noise(seed: int, n: int) -> list[int]:
    """n int16 samples of noise, in plain Python (the conformance worker has no numpy)."""
    rng = random.Random(seed)
    return [rng.randrange(-32768, 32768) for _ in range(n)]


def pack(plane) -> list[int]:
    """Two int16 samples per word, the first in the low half; an odd tail is padded with zero."""
    p = [int(v) & 0xFFFF for v in plane]
    p += [0] * (len(p) % 2)
    return [p[i] | p[i + 1] << 16 for i in range(0, len(p), 2)]


def write_planes(poke, left, right, plane_l=PLANE_L, plane_r=PLANE_R):
    """Write the packed planes at the two plane addresses."""
    for k, w in enumerate(pack(left)):
        poke(plane_l + k, w)
    for k, w in enumerate(pack(right)):
        poke(plane_r + k, w)


def publish(
    poke,
    base,
    index,
    frames,
    src_rate,
    channels,
    token=TOKEN,
    plane_l=PLANE_L,
    plane_r=PLANE_R,
):
    """Publish record `index` of the table at word address `base` (8 words per record)."""
    rec = base + 8 * index
    base_r = plane_l if channels == 1 else plane_r
    for k, w in enumerate([0, token, frames, src_rate, channels, 1, plane_l, base_r]):
        poke(rec + k, w)
    poke(rec, 1)  # ready last
