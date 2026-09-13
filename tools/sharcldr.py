#!/usr/bin/env python3
"""Parse and characterise the SHARC DSP blob (container section 7).

Section 7 is the SHARC DSP image. The ColdFire MAIN OS makes no audio; it
RPCs parameter changes to this DSP, so anything sound-related lives here.

The blob IS a pure boot-stream, end to end. An earlier reading here said only
the first 11,248 bytes parse and the rest was "a format this tool does not
decode" -- that was this parser desyncing, not a change of format. FILL is
bit 8, not bit 12 (see below), and with it corrected the chain consumes the
whole section exactly.

The blob is an ADI boot-stream: a 16-byte, little-endian, four 32-bit-field
(block_code, target_address, byte_count, argument) header per block. A header
is valid iff the byte-wise XOR of all 16 header bytes is zero -- that is the
format's header checksum and the reliable way to find block boundaries.
block_code bit 8 is FILL: when set, no payload follows the header (the block
is a zero/constant fill of byte_count bytes); otherwise exactly byte_count
payload bytes follow. Other block_code bits are NOT decoded here -- their ADI
semantics have not been verified, so this tool only reports them as bitN
rather than inventing meanings.

Why bit 8 and not bit 12, measured rather than argued. Walking with FILL at
bit 12 stops after 4 blocks at exactly 11,248 bytes on every image tried --
which is where the "prologue" came from. Walking with FILL at bit 8:

    Digitone II 1.11   95 blocks, 836,956 of 836,956 bytes   exact
    Digitone II 1.10E  96 blocks, 833,060 of 833,060 bytes   exact

Both terminate on a block whose flags include bit 15, whose target is then an
entry point rather than a load address, and -- the part that makes this more
than a curve fit -- **every one of those 95/96 headers still passes the XOR
check above**, including the 77 that lie beyond the old 11,248-byte cutoff.
Bit 12 is set on only 2 blocks in the whole image, bit 8 on 37. (Digitakt II
was not re-tested here for want of an image, but its prologue is the same
11,248 bytes, which is the same signature.)

This also resolves the discrepancy flagged below: a wrong FILL bit is exactly
how a walk finds 4 blocks where another tool found 32.

Measured region split, as examples, not universal rules: Digitakt's tail
(everything after the 11,248-byte prologue) is roughly 56 KB of float-like
tables and 244 KB of "other"; Digitone's tail is roughly 72 KB float and
712 KB other -- Digitone's blob is 833,060 bytes total against Digitakt's
320,780. `kind` is a heuristic over bit patterns only: "float" means the
32-bit words decode as plausible little-endian IEEE-754 floats, which is
strong evidence of coefficient or wavetable data; "other" means only "not
obviously float or zero" -- it is NOT evidence of executable code.
Confirming code needs a SHARC disassembler, and stock Ghidra 12.1.3 ships
no SHARC, Blackfin or ADSP processor module (verified by listing its
Processors directory).

A previously circulated figure of "32 blocks, 178,796 of 320,780 bytes
loaded" did not reproduce with this parser, which found 4 blocks and 11,184
payload bytes. **That discrepancy was this parser's FILL bit**, and it goes
away with bit 8: a fill block advances the load address without advancing the
file, so mistaking one for a payload block skips byte_count bytes that were
never there and lands mid-data.

Alignment analysis of the 10,312-byte loader payload (see `alignment()`)
finds repeated code motifs landing on even offsets exclusively but spread
roughly uniformly across mod 4, mod 6 and mod 8. That is 16-bit-granular
variable-length encoding, not fixed 48-bit words, and on SHARC+ that
encoding is VISA. This is an inference from alignment statistics, not a
decoded instruction, but it scopes any future Ghidra SLEIGH work to
SHARC+ VISA rather than classic ADSP-21xx fixed 48-bit. The encoding spec
is the publicly downloadable "SHARC+ Core Programming Reference"
(sc58x-2158x-prm.pdf) from analog.com, no registration required.

Usage:
    uv run python tools/sharcldr.py section_7_digitakt.bin --json out.json
    uv run python tools/sharcldr.py section_7_digitakt.bin --dump-blocks blocks/
"""
import argparse, hashlib, json, math, os, struct, sys
from collections import defaultdict

sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))

HEADER_LEN = 16
# Bit 8, not bit 12 -- see the module docstring. With bit 12 the walk desyncs
# after four blocks and stops at 11,248 bytes on every image tried; with bit 8
# it consumes the whole section exactly and every header still passes the XOR
# check this file already applies.
FILL_BIT = 8

# Measured on this firmware's own aPLib-compressed container streams, which
# is the only honest local definition of "compressed": sections 2/3/7 of
# both shipping images sit at 7.83-7.88 bits/byte. Two known-raw controls
# sit far below it -- section 4 (the updater, documented as stored raw) at
# 6.057, and this blob's own SHARC loader payload at 6.066. So a region
# below roughly 7.5 is not compressed, and the section 7 payload regions
# (Digitakt 6.416, Digitone 7.260) are therefore raw, not packed. This
# matters because a compressed payload would need unpacking before any
# disassembly, and it does not.
APLIB_ENTROPY_BAND = (7.83, 7.88)
ENTROPY_RAW_CEILING = 7.5

# Bits 24-31 of block_code are the header checksum byte, not flags: the whole
# 16-byte header XORs to zero, and that byte is what makes it do so. Reporting
# them as flags is how every block ends up looking like it has eight of them.
# Every shipping block here has 0xad there, which is a property of the other
# fields, not a meaning.
HDRCHK_SHIFT = 24


def _flags(block_code):
    flags = []
    for bit in range(HDRCHK_SHIFT):
        if block_code & (1 << bit):
            flags.append("FILL" if bit == FILL_BIT else "bit%d" % bit)
    return flags


def _header_valid(hdr):
    x = 0
    for b in hdr:
        x ^= b
    return x == 0


def parse_blocks(data):
    """Walk the ADI boot-stream, returning parsed block dicts.

    Stops at the first invalid header (bad checksum) or at any header whose
    payload would overrun the file.
    """
    blocks = []
    offset = 0
    index = 0
    while offset + HEADER_LEN <= len(data):
        hdr = data[offset:offset + HEADER_LEN]
        if not _header_valid(hdr):
            break
        block_code, target_address, byte_count, argument = struct.unpack(
            "<IIII", hdr)
        fill = bool(block_code & (1 << FILL_BIT))
        payload_offset = offset + HEADER_LEN
        payload_len = 0 if fill else byte_count
        if payload_offset + payload_len > len(data):
            break
        blocks.append({
            "index": index,
            "offset": offset,
            "block_code": block_code,
            "target_address": target_address,
            "byte_count": byte_count,
            "argument": argument,
            "fill": fill,
            "flags": _flags(block_code),
            "payload_offset": payload_offset,
            "payload_len": payload_len,
        })
        offset = payload_offset + payload_len
        index += 1
    return blocks


def entropy(data):
    """Shannon entropy in bits/byte over `data`. 0.0 for empty input."""
    if not data:
        return 0.0
    counts = [0] * 256
    for b in data:
        counts[b] += 1
    n = len(data)
    ent = 0.0
    for c in counts:
        if c:
            p = c / n
            ent -= p * math.log2(p)
    return ent


def _entropy_annotation(value):
    lo, hi = APLIB_ENTROPY_BAND
    if lo <= value <= hi:
        return "(compressed?)"
    if value < ENTROPY_RAW_CEILING:
        return "(raw)"
    return ""


def alignment(data, min_count=8, gram=4, strides=(2, 4, 6, 8)):
    """Answer "what instruction width is this code?" from repeat offsets.

    Finds every `gram`-byte sequence occurring at least `min_count` times,
    collects all of its occurrence offsets, and for each stride in
    `strides` builds a histogram of `offset % stride` across all of those
    occurrences. Returns a dict mapping stride -> {residue: count}, plus a
    "total" key with the count of occurrences considered.

    A fixed-width instruction set concentrates repeated code motifs at ONE
    residue for its width: 48-bit SHARC code would be constant mod 6,
    32-bit code constant mod 4. Measured on the 10,312-byte loader payload
    of both images, repeated motifs land on EVEN offsets exclusively (the
    6-byte motif f29fc09f1200 occurs 56 times, all at offset % 2 == 0;
    3f083f34089c 38 times; an 11-byte motif 6 times) but spread roughly
    uniformly across mod 4, mod 6 and mod 8. That is 16-bit-granular
    variable-length encoding, which on SHARC+ is VISA. This is an
    inference from alignment statistics, NOT a decoded instruction -- no
    instruction has been decoded, and confirming it needs the SHARC+ Core
    Programming Reference.
    """
    seen = defaultdict(list)
    for i in range(len(data) - gram + 1):
        seen[data[i:i + gram]].append(i)
    occurrences = []
    for offs in seen.values():
        if len(offs) >= min_count:
            occurrences.extend(offs)
    result = {}
    for stride in strides:
        hist = {r: 0 for r in range(stride)}
        for off in occurrences:
            hist[off % stride] += 1
        result[stride] = hist
    result["total"] = len(occurrences)
    return result


def _format_hist(hist, stride):
    return "{" + ",".join("%d:%d" % (r, hist[r]) for r in range(stride)) + "}"


def _is_float_like(word):
    try:
        f = struct.unpack("<f", struct.pack("<I", word))[0]
    except struct.error:
        return False
    if f != f:  # NaN
        return False
    af = abs(f)
    return 1e-6 < af < 1e6


def _classify_window(chunk):
    words = [struct.unpack("<I", chunk[i:i + 4])[0]
              for i in range(0, len(chunk) - (len(chunk) % 4), 4)]
    n = len(words)
    if n == 0:
        return "other", 0.0, 0.0
    zeros = sum(1 for w in words if w == 0)
    floats = sum(1 for w in words if _is_float_like(w))
    pct_zero = 100.0 * zeros / n
    pct_float = 100.0 * floats / n
    if pct_zero > 90.0:
        kind = "zero"
    elif pct_float > 90.0:
        kind = "float"
    else:
        kind = "other"
    return kind, pct_float, pct_zero


def characterise(data, start, window):
    """Classify data[start:] in fixed-size windows, then coalesce runs."""
    windows = []
    offset = start
    while offset < len(data):
        chunk = data[offset:offset + window]
        kind, pct_float, pct_zero = _classify_window(chunk)
        windows.append({
            "offset": offset,
            "length": len(chunk),
            "kind": kind,
            "pct_float": round(pct_float, 2),
            "pct_zero": round(pct_zero, 2),
        })
        offset += len(chunk)

    regions = []
    for w in windows:
        if regions and regions[-1]["kind"] == w["kind"] \
                and regions[-1]["end"] == w["offset"]:
            regions[-1]["end"] = w["offset"] + w["length"]
            regions[-1]["length"] = regions[-1]["end"] - regions[-1]["offset"]
        else:
            regions.append({
                "offset": w["offset"],
                "end": w["offset"] + w["length"],
                "length": w["length"],
                "kind": w["kind"],
            })
    for r in regions:
        r["entropy"] = round(entropy(data[r["offset"]:r["end"]]), 3)
    return windows, regions


def summarise(path, data, blocks, stop_offset):
    header_bytes = len(blocks) * HEADER_LEN
    payload_bytes = sum(b["payload_len"] for b in blocks if not b["fill"])
    fill_bytes = sum(b["byte_count"] for b in blocks if b["fill"])
    return {
        "path": path,
        "size": len(data),
        "sha256": hashlib.sha256(data).hexdigest(),
        "entropy": round(entropy(data), 3),
        "block_count": len(blocks),
        "header_bytes": header_bytes,
        "payload_bytes": payload_bytes,
        "fill_bytes": fill_bytes,
        "parse_stopped_at": stop_offset,
        "unparsed_bytes": len(data) - stop_offset,
    }


def main():
    ap = argparse.ArgumentParser(description=__doc__,
                                  formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("blob", help="path to a section_7_*.bin")
    ap.add_argument("--window", type=int, default=4096,
                    help="characterisation window size in bytes")
    ap.add_argument("--json", help="write the full result dict as JSON")
    ap.add_argument("--dump-blocks", help="write each block's payload here")
    ap.add_argument("--align", action="store_true",
                    help="run alignment() over regions and block payloads")
    args = ap.parse_args()

    data = open(args.blob, "rb").read()
    blocks = parse_blocks(data)
    stop_offset = blocks[-1]["payload_offset"] + blocks[-1]["payload_len"] \
        if blocks else 0
    windows, regions = characterise(data, stop_offset, args.window)
    summary = summarise(args.blob, data, blocks, stop_offset)

    if args.dump_blocks:
        os.makedirs(args.dump_blocks, exist_ok=True)
        for b in blocks:
            if b["fill"]:
                continue
            name = "blk%02d_%08x.bin" % (b["index"], b["target_address"])
            with open(os.path.join(args.dump_blocks, name), "wb") as f:
                f.write(data[b["payload_offset"]:
                             b["payload_offset"] + b["payload_len"]])

    result = {
        "summary": summary,
        "blocks": blocks,
        "windows": windows,
        "regions": regions,
    }

    alignment_results = None
    if args.align:
        alignment_results = {"regions": [], "blocks": []}
        for r in regions:
            if r["kind"] == "zero":
                continue
            span = data[r["offset"]:r["end"]]
            alignment_results["regions"].append({
                "label": "%s@%d..%d" % (r["kind"], r["offset"], r["end"]),
                "length": len(span),
                "alignment": alignment(span),
            })
        for b in blocks:
            if b["fill"]:
                continue
            span = data[b["payload_offset"]:
                        b["payload_offset"] + b["payload_len"]]
            alignment_results["blocks"].append({
                "label": "blk%02d" % b["index"],
                "length": len(span),
                "alignment": alignment(span),
            })
        result["alignment"] = alignment_results

    if args.json:
        json.dump(result, open(args.json, "w"), indent=2)

    print("=== %s ===" % args.blob)
    print("size=%d  sha256=%s  entropy=%.3f %s" % (
        summary["size"], summary["sha256"], summary["entropy"],
        _entropy_annotation(summary["entropy"])))
    print("blocks=%d  header_bytes=%d  payload_bytes=%d  fill_bytes=%d" % (
        summary["block_count"], summary["header_bytes"],
        summary["payload_bytes"], summary["fill_bytes"]))
    print("parse_stopped_at=%d  unparsed_bytes=%d" % (
        summary["parse_stopped_at"], summary["unparsed_bytes"]))

    print("\n--- blocks ---")
    for b in blocks:
        print("blk%02d  @%-6d  code=0x%08x  addr=0x%08x  cnt=%-6d  arg=0x%08x  %s" % (
            b["index"], b["offset"], b["block_code"], b["target_address"],
            b["byte_count"], b["argument"], ",".join(b["flags"]) or "-"))

    print("\n--- regions ---")
    for r in regions:
        print("%-6s %8d..%-8d  (%d KB)  entropy=%.3f %s" % (
            r["kind"], r["offset"], r["end"], r["length"] // 1024,
            r["entropy"], _entropy_annotation(r["entropy"])))

    if args.align:
        print("\n--- alignment ---")
        for item in alignment_results["regions"] + alignment_results["blocks"]:
            a = item["alignment"]
            hist_str = "  ".join(
                "mod%d=%s" % (stride, _format_hist(a[stride], stride))
                for stride in sorted(k for k in a if k != "total"))
            print("%-20s len=%-8d n=%-6d %s" % (
                item["label"], item["length"], a["total"], hist_str))

    return 0


if __name__ == "__main__":
    sys.exit(main())
