"""Check two emulator snapshots for exact guest-state equivalence.

    uv run python tools/snapeq.py A.snap B.snap

Compares everything emu/snapshot.py's save() writes EXCEPT `extra` and
`components`/`manifest`: registers, every mapped memory page (byte for
byte, including pages mapped but all-zero and so not individually stored),
mmio, ctlregs, ff1_count and movec_count. `extra` is deliberately excluded:
it carries rung bookkeeping (instruction count, coverage stats, task_create
timestamps) that two ladders built with different `coverage` settings are
expected to differ on by design -- see emu.checkpoint.make's own docstring
-- and comparing it would make an intentional difference look like a guest
state mismatch. Use tools/snapdiff.py instead for a labelled byte-run diff
within a chosen address range, or to also see each snapshot's `extra`.

Reads the pickled blob directly via emu.snapshot._load_blob, so it builds
no Machine and needs no firmware. Exit code 0 and "identical" if every
compared field matches; otherwise prints the FIRST mismatch found (in the
order above) and exits 1.
"""

import os
import sys
import zlib

sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))

from emu.harness import PAGE  # noqa: E402
from emu.snapshot import REGS, _load_blob  # noqa: E402

ZERO_PAGE = b"\x00" * PAGE


def _page(blob, base):
    comp = blob["pages"].get(base)
    return zlib.decompress(comp) if comp is not None else ZERO_PAGE


def compare(a_path, b_path):
    """-> None if `a_path` and `b_path` hold identical guest state, else a
    one-line description of the first difference found."""
    a, b = _load_blob(a_path), _load_blob(b_path)
    for name, _reg in REGS:
        if a["regs"][name] != b["regs"][name]:
            return "register %s: 0x%x != 0x%x" % (
                name,
                a["regs"][name],
                b["regs"][name],
            )
    if a["ff1_count"] != b["ff1_count"]:
        return "ff1_count: %d != %d" % (a["ff1_count"], b["ff1_count"])
    if a["movec_count"] != b["movec_count"]:
        return "movec_count: %d != %d" % (a["movec_count"], b["movec_count"])
    if a["mmio"] != b["mmio"]:
        only = sorted(set(a["mmio"].items()) ^ set(b["mmio"].items()))
        return "mmio differs at %d entries, first %r" % (len(only), only[:1])
    if a["ctlregs"] != b["ctlregs"]:
        only = sorted(set(a["ctlregs"].items()) ^ set(b["ctlregs"].items()))
        return "ctlregs differs at %d entries, first %r" % (len(only), only[:1])
    a_mapped, b_mapped = set(a["all_mapped"]), set(b["all_mapped"])
    if a_mapped != b_mapped:
        only_a = sorted(a_mapped - b_mapped)
        only_b = sorted(b_mapped - a_mapped)
        return "mapped pages differ: %d only in a (e.g. %s), %d only in b (e.g. %s)" % (
            len(only_a),
            hex(only_a[0]) if only_a else None,
            len(only_b),
            hex(only_b[0]) if only_b else None,
        )
    for base in sorted(a_mapped):
        pa, pb = _page(a, base), _page(b, base)
        if pa != pb:
            for i in range(PAGE):
                if pa[i] != pb[i]:
                    return "page 0x%08x+0x%x: 0x%02x != 0x%02x" % (
                        base,
                        i,
                        pa[i],
                        pb[i],
                    )
    return None


def main(argv=None):
    argv = sys.argv[1:] if argv is None else argv
    if len(argv) != 2:
        print(__doc__)
        return 2
    a_path, b_path = argv
    diff = compare(a_path, b_path)
    if diff is None:
        n_pages = len(_load_blob(a_path)["all_mapped"])
        print(
            "identical: %s == %s (regs, %d mapped pages, mmio, ctlregs, "
            "ff1/movec counts)" % (a_path, b_path, n_pages)
        )
        return 0
    print("DIFFERS: %s : %s" % (a_path, b_path))
    print("  " + diff)
    return 1


if __name__ == "__main__":
    sys.exit(main())
