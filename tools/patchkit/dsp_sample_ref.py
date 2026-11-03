"""Numerical reference for sdk/dsp/sample.inc; no storage or resource policy."""

from collections.abc import Mapping, Sequence
from dataclasses import dataclass, replace

import numpy as np


@dataclass
class Cursor:
    """Six integer state words; signed rate is Q16.16, fraction is unsigned16."""

    start: int
    end: int
    index: int
    fraction: int
    rate: int
    done: int = 0


def cursor_step(index: int, fraction: int, rate: int) -> tuple[int, int]:
    """Exact split update, with no conversion of absolute position to float."""
    total = fraction + (rate & 65535)
    return index + (rate >> 16) + (total >> 16), total & 65535


def linear_block(
    source: Sequence[float] | Mapping[int, float], state: Cursor
) -> tuple[np.ndarray, Cursor]:
    """Read 32 samples; return output and copied state, preserving authoritative done."""
    st = replace(state)
    out = np.zeros(32, dtype=np.float32)
    for n in range(32):
        if not st.start <= st.index < st.end:
            st.done = 1
        if st.done:
            break
        a = np.float32(source[st.index])
        b = np.float32(source[min(st.index + 1, st.end - 1)])
        f = np.float32(st.fraction / 65536)
        out[n] = a + f * (b - a)
        st.index, st.fraction = cursor_step(st.index, st.fraction, st.rate)
        if not st.start <= st.index < st.end:
            st.done = 1
    return out, st
