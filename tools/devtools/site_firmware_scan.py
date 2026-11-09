"""Scan a static site for byte runs copied from stock Elektron firmware.

    uv run python -m tools.devtools.site_firmware_scan SITE_DIR [--min-run N] [--self-test]
    uv run python -m tools.devtools.site_firmware_scan --self-test

References (never printed or saved; reports carry offsets and lengths only):
the current analysis images DT2 1.17, DN2 1.12 and DN2 1.11 from firmware/, as
  - every decoded section (out/sections/<image>, checked against the .syx hash and
    re-extracted into a temporary directory when missing or stale),
  - the ELE3 container and the raw .syx file,
  - the DSP boot stream (section 7) as stored, and the loaded DSP data of each
    non-fill block chain in these layouts: as stored (16-bit little endian parcels),
    16-bit swapped, 32-bit swapped at phase 0 and 2, 48-bit byte-reversed and
    48-bit parcel-reversed at phase 0, 2 and 4, 32-bit parcel-reversed at phase 0 and 2.
Older images are not used.

Scanned: every site file as raw bytes, plus decoded forms: JSON (recursively, also
JSON inside strings), hex strings, base64 strings, integer arrays and 0x-literal lists
(as bytes, and as 16/32-bit words in both byte orders). A text file gets the same
string and literal extraction.

Detection: every 32-byte (--min-run) window of the scanned bytes is hashed and looked
up in an index of all reference windows (stride 1, 64-bit hash, bytes re-checked).
Windows with fewer than 8 distinct byte values are ignored (padding and fills carry no
information). Consecutive matching windows on one diagonal form a run; the run is then
extended byte by byte to its maximum ("len"); "core" is the length of the run made
only of qualifying windows. Exit code 0 only with zero hits.
"""

import argparse
import base64
import binascii
import hashlib
import json
import re
import struct
import sys
import tempfile
from pathlib import Path

import numpy as np
from numpy.lib.stride_tricks import sliding_window_view

from tools.firmware import config, container, extract
from tools.sharc_dev import sharcldr

REPO = Path(__file__).resolve().parents[2]
MIN_DISTINCT = 8
IMAGES = {
    "dt2-1.17": "Digitakt_II_OS1.17.syx",
    "dn2-1.12": "Digitone_II_OS1.12.syx",
    "dn2-1.11": "Digitone_II_OS1.11.syx",
}
MAX_DEPTH = 8
P = np.uint64(0x9E3779B97F4A7C15)
PINV = np.uint64(pow(0x9E3779B97F4A7C15, -1, 1 << 64))


# ---- window hashing -------------------------------------------------------


def _pows(base, n):
    out = np.empty(n, dtype=np.uint64)
    out[0] = 1
    if n > 1:
        out[1:] = np.cumprod(np.full(n - 1, base, dtype=np.uint64), dtype=np.uint64)
    return out


_POW_CACHE: dict[str, np.ndarray] = {}


def _pow_arrays(n):
    have = _POW_CACHE.get("p")
    if have is None or len(have) < n:
        n2 = max(n, 1 << 16)
        _POW_CACHE["p"] = _pows(P, n2)
        _POW_CACHE["i"] = _pows(PINV, n2)
    return _POW_CACHE["p"], _POW_CACHE["i"]


def window_hashes(arr, k):
    """uint64 hash of every k-byte window of the uint8 array (len - k + 1 of them)."""
    n = len(arr)
    if n < k:
        return np.empty(0, dtype=np.uint64)
    pw, ipw = _pow_arrays(n + 1)
    with np.errstate(over="ignore"):
        terms = (arr.astype(np.uint64) + np.uint64(1)) * pw[:n]
        c = np.concatenate(
            [np.zeros(1, dtype=np.uint64), np.cumsum(terms, dtype=np.uint64)]
        )
        return (c[k:] - c[:-k]) * ipw[: n - k + 1]


def window_ok(arr, k, thr=MIN_DISTINCT):
    """bool per window: at least thr distinct byte values."""
    n = len(arr)
    if n < k:
        return np.zeros(0, dtype=bool)
    view = sliding_window_view(arr, k)
    out = np.empty(len(view), dtype=bool)
    step = 1 << 17
    for i in range(0, len(view), step):
        s = np.sort(view[i : i + step], axis=1)
        out[i : i + step] = ((s[:, 1:] != s[:, :-1]).sum(axis=1) + 1) >= thr
    return out


# ---- references -----------------------------------------------------------


def _swap(a, grp, phase):
    """Reverse bytes in each grp-byte group starting at `phase` (ragged ends unchanged)."""
    b = a.copy()
    m = (len(a) - phase) // grp * grp
    if m > 0:
        b[phase : phase + m] = (
            a[phase : phase + m].reshape(-1, grp)[:, ::-1].reshape(-1)
        )
    return b


def _parcel_rev(a, parcels, phase):
    """Reverse the order of 16-bit parcels in each group, keeping bytes in each parcel."""
    b = a.copy()
    g = parcels * 2
    m = (len(a) - phase) // g * g
    if m > 0:
        v = a[phase : phase + m].reshape(-1, parcels, 2)
        b[phase : phase + m] = v[:, ::-1, :].reshape(-1)
    return b


DSP_LAYOUTS = (
    [("stored", lambda a: a), ("swap16", lambda a: _swap(a, 2, 0))]
    + [(f"swap32@{p}", lambda a, p=p: _swap(a, 4, p)) for p in (0, 2)]
    + [(f"rev48@{p}", lambda a, p=p: _swap(a, 6, p)) for p in (0, 2, 4)]
    + [(f"parcelrev3@{p}", lambda a, p=p: _parcel_rev(a, 3, p)) for p in (0, 2, 4)]
    + [(f"parcelrev2@{p}", lambda a, p=p: _parcel_rev(a, 2, p)) for p in (0, 2)]
)


def _sha(path):
    return hashlib.sha256(Path(path).read_bytes()).hexdigest()


def section_files(key, syx, tmp):
    """-> (directory of decoded sections, how it was obtained). Re-extracts on a stale directory."""
    d = Path(config.SECTIONS_ROOT) / key
    marker = d / extract.SOURCE_MARKER
    want = _sha(syx)
    if (
        marker.exists()
        and marker.read_text().strip() == want
        and any(d.glob("section_*.bin"))
    ):
        return d, "out/sections"
    out = Path(tmp) / key
    extract.extract(str(syx), str(out))
    return out, "re-extracted"


def load_references(log):
    """-> list of (label, uint8 array). Labels: '<image> <section/layout>'."""
    refs: list[tuple[str, np.ndarray]] = []
    tmp = tempfile.mkdtemp(prefix="sitefw-")
    for key, name in IMAGES.items():
        syx = config.stock(name)
        if not syx.exists():
            raise SystemExit(f"missing reference image {syx}")
        d, how = section_files(key, syx, tmp)
        log(f"reference {key}: {name} sha256 {_sha(syx)[:16]}.., sections from {how}")

        def add(label, data, key=key):
            refs.append((f"{key} {label}", np.frombuffer(bytes(data), dtype=np.uint8)))

        add("syx-file", syx.read_bytes())
        add("ele3-container", container.container(str(syx)))
        blob7 = b""
        for f in sorted(d.glob("section_*.bin")):
            data = f.read_bytes()
            add(f.stem, data)
            if f.stem.startswith("section_7_"):
                blob7 = data
        if not blob7:
            raise SystemExit(f"{key}: no section 7 (DSP boot stream)")
        blocks = sharcldr.parse_blocks(blob7)
        if not blocks:
            raise SystemExit(f"{key}: section 7 does not parse as a boot stream")
        chains: list[tuple[int, bytearray]] = []
        last_end = None
        for b in blocks:
            if b["fill"] or not b["byte_count"]:
                last_end = None
                continue
            pl = blob7[b["payload_offset"] : b["payload_offset"] + b["payload_len"]]
            if last_end is not None and b["target_address"] == last_end:
                chains[-1][1].extend(pl)
            else:
                chains.append((b["target_address"], bytearray(pl)))
            last_end = b["target_address"] + b["byte_count"]
        for lo, data in chains:
            base = np.frombuffer(bytes(data), dtype=np.uint8)
            for lname, fn in DSP_LAYOUTS:
                if lname == "stored" and len(base) == 0:
                    continue
                add(f"dsp-mem@{lo:#x}+{len(base):#x} {lname}", fn(base).tobytes())
        log(
            f"  {key}: section 7 {len(blob7)} bytes, {len(blocks)} blocks, "
            f"{len(chains)} loaded chains x {len(DSP_LAYOUTS)} layouts"
        )
    return refs


class Index:
    def __init__(self, refs, k):
        self.k = k
        self.refs = refs
        hs, bs, ps = [], [], []
        for i, (_label, arr) in enumerate(refs):
            ok = window_ok(arr, k)
            h = window_hashes(arr, k)
            pos = np.nonzero(ok)[0]
            hs.append(h[pos])
            bs.append(np.full(len(pos), i, dtype=np.uint32))
            ps.append(pos.astype(np.uint32))
        h = np.concatenate(hs)
        order = np.argsort(h, kind="stable")
        self.h = h[order]
        self.buf = np.concatenate(bs)[order]
        self.pos = np.concatenate(ps)[order]
        self.total = sum(len(a) for _l, a in refs)

    def runs(self, arr):
        """-> [(site pos, ref idx, ref pos, nwindows)] of runs of matching qualifying windows."""
        k = self.k
        ok = window_ok(arr, k)
        spos = np.nonzero(ok)[0]
        if not len(spos):
            return []
        hq = window_hashes(arr, k)[spos]
        lo = np.searchsorted(self.h, hq, "left")
        hi = np.searchsorted(self.h, hq, "right")
        cnt = hi - lo
        sel = cnt > 0
        if not sel.any():
            return []
        lo, cnt, spos = lo[sel], cnt[sel], spos[sel]
        site = np.repeat(spos, cnt)
        start = np.repeat(lo, cnt)
        within = np.arange(int(cnt.sum())) - np.repeat(np.cumsum(cnt) - cnt, cnt)
        j = start + within
        buf = self.buf[j].astype(np.int64)
        rpos = self.pos[j].astype(np.int64)
        diag = rpos - site
        order = np.lexsort((site, diag, buf))
        site, buf, rpos, diag = site[order], buf[order], rpos[order], diag[order]
        brk = np.ones(len(site), dtype=bool)
        brk[1:] = (
            (buf[1:] != buf[:-1])
            | (diag[1:] != diag[:-1])
            | (site[1:] != site[:-1] + 1)
        )
        starts = np.nonzero(brk)[0]
        ends = np.append(starts[1:], len(site))
        return [
            (int(site[s]), int(buf[s]), int(rpos[s]), int(e - s))
            for s, e in zip(starts, ends, strict=True)
        ]


def _icbrt(n):
    x = 1 << ((n.bit_length() + 2) // 3)
    while True:
        y = (2 * x + n // (x * x)) // 3
        if y >= x:
            return x
        x = y


def _primes(n):
    out: list[int] = []
    c = 2
    while len(out) < n:
        if all(c % q for q in out if q * q <= c):
            out.append(c)
        c += 1
    return out


def _known_tables():
    """Public constants any implementation contains: SHA-256 K, CRC-32 (IEEE) table. -> {name: bytes}."""
    k = [_icbrt(p << 96) & 0xFFFFFFFF for p in _primes(64)]
    crc = []
    for i in range(256):
        c = i
        for _ in range(8):
            c = (c >> 1) ^ (0xEDB88320 if c & 1 else 0)
        crc.append(c)
    out = {}
    for name, vals in (("sha256-K", k), ("crc32-ieee-table", crc)):
        out[name + "/u32be"] = b"".join(struct.pack(">I", v) for v in vals)
        out[name + "/u32le"] = b"".join(struct.pack("<I", v) for v in vals)
    return out


KNOWN = _known_tables()


def classify(run):
    """A label for a run that is plainly generic, else ''. Only what can be shown from the run itself."""
    for name, tbl in KNOWN.items():
        if run in tbl:
            return f"public-constant {name}"
    for w in (4, 2, 1):
        for order in ("big", "little"):
            for phase in range(w):
                body = run[phase:]
                vals = [
                    int.from_bytes(body[i : i + w], order)
                    for i in range(0, len(body) - w + 1, w)
                ]
                if len(vals) < 6:
                    continue
                m = (1 << (8 * w)) - 1
                d = {(vals[i + 1] - vals[i]) & m for i in range(len(vals) - 1)}
                if len(d) == 1 and d != {0}:
                    st = d.pop()
                    st = st - (m + 1) if st > m // 2 else st
                    return f"arithmetic-sequence u{8 * w}{order[:2]}e stride {st} ({len(vals)} values)"
    return ""


def scan_buffer(index, data):
    """-> [(site off, length, core, [(ref label, ref off)])] for one decoded buffer."""
    arr = np.frombuffer(data, dtype=np.uint8)
    k = index.k
    found: dict[tuple[int, int], list] = {}
    for s, bi, r, nwin in index.runs(arr):
        ref = index.refs[bi][1]
        core = nwin + k - 1
        if arr[s : s + core].tobytes() != ref[r : r + core].tobytes():
            continue  # hash collision
        a, b = s, r
        while a > 0 and b > 0 and arr[a - 1] == ref[b - 1]:
            a, b = a - 1, b - 1
        e, f = s + core, r + core
        while e < len(arr) and f < len(ref) and arr[e] == ref[f]:
            e, f = e + 1, f + 1
        found.setdefault((a, e - a), []).append((index.refs[bi][0], b, core))
    out: list[tuple] = []
    for (a, ln), refs in sorted(found.items()):
        core = max(c for _l, _o, c in refs)
        out.append(
            (a, ln, core, [(lb, o) for lb, o, _c in refs], classify(data[a : a + ln]))
        )
    return out


# ---- decoding the site ----------------------------------------------------

HEX_RE = re.compile(rb"(?<![0-9A-Za-z])(?:[0-9a-fA-F]{2}){%d,}(?![0-9A-Za-z])")
B64_RE = re.compile(rb"[A-Za-z0-9+/_-]{%d,}={0,2}")
LIT_RE = re.compile(rb"(?:0[xX][0-9a-fA-F]+\s*,?\s*){%d,}")
DEC_RE = re.compile(rb"(?:-?\d{1,10}\s*,\s*){%d,}-?\d{1,10}")


def _words(vals, width):
    """Integer list -> bytes variants (label suffix, bytes) for 8/16/32-bit values, both orders."""
    out = []
    mx = max(vals) if vals else 0
    mn = min(vals) if vals else 0
    if mn >= 0 and mx < 256:
        out.append(("u8", bytes(vals)))
    if mn >= -(1 << 15) and mx < (1 << 16):
        v = [x & 0xFFFF for x in vals]
        out.append(("u16le", b"".join(struct.pack("<H", x) for x in v)))
        out.append(("u16be", b"".join(struct.pack(">H", x) for x in v)))
    if mn >= -(1 << 31) and mx < (1 << 32):
        v = [x & 0xFFFFFFFF for x in vals]
        out.append(("u32le", b"".join(struct.pack("<I", x) for x in v)))
        out.append(("u32be", b"".join(struct.pack(">I", x) for x in v)))
    return out


def decode_text(label, data, n, depth):
    """Yield decoded byte forms found inside a text (hex, base64, literal lists, JSON)."""
    if depth > MAX_DEPTH:
        return
    hex_re = re.compile(HEX_RE.pattern % (n,))
    b64_re = re.compile(B64_RE.pattern % (max(8, n * 4 // 3),))
    lit_re = re.compile(LIT_RE.pattern % (max(4, n // 4),))
    dec_re = re.compile(DEC_RE.pattern % (max(4, n // 4),))
    for m in hex_re.finditer(data):
        yield from emit(
            f"{label}~hex@{m.start()}", binascii.unhexlify(m.group(0)), n, depth + 1
        )
    for m in b64_re.finditer(data):
        s = m.group(0)
        try:
            raw = base64.b64decode(
                s + b"=" * (-len(s) % 4),
                altchars=b"-_" if b"-" in s or b"_" in s else None,
            )
        except (binascii.Error, ValueError):
            continue
        if len(raw) >= n:
            yield from emit(f"{label}~b64@{m.start()}", raw, n, depth + 1)
    for m in lit_re.finditer(data):
        toks = re.findall(rb"0[xX]([0-9a-fA-F]+)", m.group(0))
        vals = [int(t, 16) for t in toks]
        for suf, raw in _words(vals, 0):
            yield (f"{label}~lit@{m.start()}:{suf}", raw)
    for m in dec_re.finditer(data):
        vals = [int(t) for t in re.findall(rb"-?\d+", m.group(0))]
        for suf, raw in _words(vals, 0):
            yield (f"{label}~dec@{m.start()}:{suf}", raw)


def walk_json(label, obj, n, depth):
    if depth > MAX_DEPTH:
        return
    if isinstance(obj, dict):
        for k, v in obj.items():
            yield from walk_json(f"{label}.{k}", v, n, depth)
    elif isinstance(obj, list):
        if obj and all(isinstance(x, int) and not isinstance(x, bool) for x in obj):
            if len(obj) >= max(4, n // 4):
                for suf, raw in _words(obj, 0):
                    yield (f"{label}[ints:{suf}]", raw)
        else:
            for i, v in enumerate(obj):
                yield from walk_json(f"{label}[{i}]", v, n, depth)
    elif isinstance(obj, str):
        yield from emit(
            label, obj.encode("utf-8", "surrogatepass"), n, depth + 1, jsonstr=True
        )


def emit(label, data, n, depth, jsonstr=False):
    """Yield (label, bytes) for data and everything decodable inside it."""
    yield (label, data)
    if depth > MAX_DEPTH or len(data) < n:
        return
    head = data.lstrip()[:1]
    if head in (b"{", b"["):
        try:
            obj = json.loads(data)
        except (ValueError, UnicodeDecodeError, RecursionError):
            obj = None
        if obj is not None:
            yield from walk_json(label, obj, n, depth)
            return
    if jsonstr or label.rsplit(".", 1)[-1] in ("txt",) or _texty(data):
        yield from decode_text(label, data, n, depth)


def _texty(data):
    s = data[:4096]
    if not s:
        return False
    return (
        sum(1 for b in s if b >= 32 or b in (9, 10, 13)) / len(s) > 0.97
        and b"\x00" not in s
    )


def site_items(site, n):
    """Yield (label, bytes, is_site_raw) for every file and decoded form."""
    for p in sorted(Path(site).rglob("*")):
        if not p.is_file():
            continue
        rel = p.relative_to(site).as_posix()
        data = p.read_bytes()
        first = True
        for label, raw in emit(rel, data, n, 0):
            yield label, raw, first
            first = False


# ---- reporting ------------------------------------------------------------


def scan_site(index, site, n, out=print):
    hits = 0
    raw_bytes = 0
    decoded_bytes = 0
    items = 0
    for label, data, is_raw in site_items(site, n):
        items += 1
        if is_raw:
            raw_bytes += len(data)
        else:
            decoded_bytes += len(data)
        for off, ln, core, refs, cls in scan_buffer(index, data):
            hits += 1
            shown = "; ".join(f"{lb} @{o:#x}" for lb, o in refs[:4])
            more = f" (+{len(refs) - 4} more refs)" if len(refs) > 4 else ""
            out(
                f"HIT {label} off={off:#x} len={ln} core={core} <- {shown}{more}"
                + (f" [{cls}]" if cls else "")
            )
    return hits, raw_bytes, decoded_bytes, items


def self_test(index, n, log):
    """Plant real firmware slices in a temporary site and demand that the scan finds them."""
    import random

    rng = random.Random(1234)
    tmp = Path(tempfile.mkdtemp(prefix="sitefw-selftest-"))
    by = {lb: a for lb, a in index.refs}
    cf = next(lb for lb in by if lb == "dt2-1.17 section_3_MAIN_OS")
    dsp = next(
        lb for lb in by if lb.startswith("dn2-1.12 dsp-mem@") and lb.endswith(" stored")
    )
    dsp_swapped = dsp.replace(" stored", " swap32@0")

    def pick(label, size, start):
        a = by[label]
        i = start
        while i + size < len(a):
            if window_ok(a[i : i + size], n).all():
                return i, a[i : i + size].tobytes()
            i += 17
        raise SystemExit(f"self-test: no plantable slice in {label}")

    def noise(sz):
        return bytes(rng.getrandbits(8) for _ in range(sz))

    plants = []  # (site file label, expected offset, length, expected ref label, expected ref offset)

    def plant(name, label, size, start, at):
        r, piece = pick(label, size, start)
        buf = noise(at) + piece + noise(2000)
        plants.append((name, at, size, label, r))
        return buf, piece, at

    buf, piece, at = plant("raw-cf.bin", cf, 300, 0x100000, 3001)
    (tmp / "raw-cf.bin").write_bytes(buf)
    buf, piece2, at2 = plant("raw-dsp-swapped.bin", dsp_swapped, 200, 0, 777)
    (tmp / "raw-dsp-swapped.bin").write_bytes(buf)
    r3, piece3 = pick(cf, 256, 0x200000)
    (tmp / "embedded.json").write_text(
        json.dumps(
            {
                "files": {
                    "x.dko.json": json.dumps(
                        {
                            "sections": [
                                {"name": ".text", "hex": (noise(40) + piece3).hex()}
                            ]
                        }
                    ),
                    "y.b64": base64.b64encode(noise(10) + piece3[::1]).decode(),
                    "z.ints": list(noise(13) + piece3),
                }
            }
        )
    )
    expect_json = {
        "embedded.json.files.x.dko.json.sections[0].hex~hex@0": 40,
        "embedded.json.files.y.b64~b64@0": 10,
        "embedded.json.files.z.ints[ints:u8]": 13,
    }
    (tmp / "clean.bin").write_bytes(noise(50000))
    got: dict[str, list] = {}
    hits_clean = 0
    for label, data, _raw in site_items(tmp, n):
        for off, ln, _core, refs, _cls in scan_buffer(index, data):
            got.setdefault(label, []).append((off, ln, refs))
            if label.startswith("clean"):
                hits_clean += 1
    fail = []
    for name, at, size, rl, ro in plants:
        g = got.get(name, [])
        ok = any(
            o == at and ln >= size and any(lb == rl and off == ro for lb, off in refs)
            for o, ln, refs in g
        )
        log(
            f"  plant {name}: {'found' if ok else 'MISSING'} (expected off={at:#x} len>={size} ref {rl} @{ro:#x})"
        )
        if not ok:
            fail.append(name)
    for sub, at in expect_json.items():
        g = got.get(sub, [])
        ok = any(
            o == at and ln >= 256 and any(lb == cf and off == r3 for lb, off in refs)
            for o, ln, refs in g
        )
        log(
            f"  plant {sub}: {'found' if ok else 'MISSING'} (expected off={at} len>=256 ref {cf} @{r3:#x})"
        )
        if not ok:
            fail.append(sub)
    if hits_clean:
        fail.append("clean control reported hits")
    log(f"  clean random control: {hits_clean} hits")
    if fail:
        print(f"SELF-TEST FAILED: {fail}", file=sys.stderr)
        raise SystemExit(2)
    log("self-test passed")


def main(argv=None):
    ap = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    ap.add_argument("site", nargs="?")
    ap.add_argument("--min-run", type=int, default=32)
    ap.add_argument("--self-test", action="store_true")
    a = ap.parse_args(argv)
    if a.min_run < MIN_DISTINCT:
        raise SystemExit(f"--min-run must be at least {MIN_DISTINCT}")
    if not a.site and not a.self_test:
        ap.error("give SITE_DIR or --self-test")

    def log(s):
        print(s, flush=True)

    log(
        f"rule: windows of {a.min_run} bytes with fewer than {MIN_DISTINCT} distinct byte values are ignored; "
        "offsets and lengths only, no firmware bytes are printed"
    )
    refs = load_references(log)
    index = Index(refs, a.min_run)
    log(
        f"indexed {len(refs)} reference buffers, {index.total} reference bytes, "
        f"{len(index.h)} qualifying windows"
    )
    if a.self_test:
        self_test(index, a.min_run, log)
    if not a.site:
        return 0
    hits, raw, dec, items = scan_site(index, a.site, a.min_run, log)
    log(
        f"SUMMARY scanned {raw} raw site bytes + {dec} decoded bytes in {items} buffers; "
        f"reference bytes indexed {index.total}; hits {hits}"
    )
    return 0 if hits == 0 else 1


if __name__ == "__main__":
    sys.exit(main())
