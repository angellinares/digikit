#!/usr/bin/env python3
"""One small Python API over a tools/sharcdb.py database: build/analyze once,
then query in-process instead of a fresh CLI round trip per question.

    import sharc
    img = sharc.load("dt2-1.16")
    img.func(0x1c642a)
    img.reach(0x1c642a, 0x1c7053)

Ad hoc SQL from the shell, without writing a script:

    uv run python tools/sharc.py dt2-1.16 "SELECT kind, count(*) FROM roots GROUP BY kind"
"""

from __future__ import annotations

import os
import sqlite3
import sys

_here = os.path.dirname(os.path.abspath(__file__))
if _here not in sys.path:
    sys.path.insert(0, _here)

import networkx as nx  # noqa: E402
import sharcdb  # noqa: E402
import sharcfn  # noqa: E402
import sharcldr  # noqa: E402
import sharc_trace  # noqa: E402

SECTIONS_DIR = "out/sections"
DB_DIR = "out/sharcdb"

# The firmware's own hardware DAG-modify reset values (docs/findings/06),
# seeded by default so a trace() caller doesn't have to remember them.
_DEFAULT_TRACE_REGS = {"M5": 0, "M6": 1, "M7": -1, "M13": 0, "M14": 1, "M15": -1}

# tools/sharcdb.sql's "last writer of REG before sw S" recursive CTE, kept
# here verbatim rather than re-derived: walk basic blocks backwards over
# succ, stopping expansion at the first block on each path that already has
# a regdef of REG before the search's upper bound there.
_LAST_DEF_SQL = """
WITH RECURSIVE walk(block_sw, upper_sw) AS (
  SELECT b0.start_sw, ?
  FROM bblocks b0 WHERE b0.image = ? AND b0.start_sw <= ? AND b0.end_sw > ?
  UNION
  SELECT s.from_block, b.end_sw
  FROM walk w
  JOIN bblocks b ON b.image = ? AND b.start_sw = w.block_sw
  JOIN succ s ON s.image = ? AND s.to_block = w.block_sw
  WHERE NOT EXISTS (
    SELECT 1 FROM regdef d
    WHERE d.image = ? AND d.reg = ? AND d.sw >= b.start_sw AND d.sw < w.upper_sw
  )
)
SELECT DISTINCT writer_sw FROM (
  SELECT MAX(d.sw) AS writer_sw
  FROM walk w
  JOIN bblocks b ON b.image = ? AND b.start_sw = w.block_sw
  JOIN regdef d ON d.image = ? AND d.reg = ? AND d.sw >= b.start_sw AND d.sw < w.upper_sw
  GROUP BY w.block_sw, w.upper_sw
)"""

_FUNC_COLUMNS = (
    "entry_sw", "end_sw", "name", "n_insns", "block", "label", "entry_kind",
    "has_static_caller", "is_leaf",
)


def _hex(value):
    return "0x%x" % value if isinstance(value, int) else value


def load(name, sections_dir=SECTIONS_DIR, db_dir=DB_DIR, min_depth=8, blocks=None, force=False):
    """Open `db_dir`/<name>.sqlite, building (or rebuilding, if the blob's
    sha256 or tools/sharcdb.py's DB_VERSION has moved on) from
    `sections_dir`/<name>/section_7_BLOB.bin first when needed. If the blob
    is gone but an up-to-date database is already on disk, that's fine --
    only a build or rebuild requires the blob."""
    blob_path = os.path.join(sections_dir, name, "section_7_BLOB.bin")
    out_path = os.path.join(db_dir, name + ".sqlite")

    stale = force or not os.path.exists(out_path)
    if not stale:
        meta = sharcdb.read_meta(out_path)
        if meta.get("db_version") != str(sharcdb.DB_VERSION):
            stale = True
        elif os.path.exists(blob_path) and meta.get("image_sha256") != sharcfn.sha256_of(blob_path):
            stale = True

    if stale:
        if not os.path.exists(blob_path):
            raise FileNotFoundError(
                "sharc.load(%r): %s is missing or stale and %s does not exist to rebuild it"
                % (name, out_path, blob_path)
            )
        sharcdb.build_database(blob_path, out_path, name=name, min_depth=min_depth, blocks=blocks, force=True)

    return Image(name, out_path, blob_path)


class Image:
    """A queryable image: a handle on out/sharcdb/<name>.sqlite plus,
    lazily, the LoadedMemory needed for trace()/xref_table()."""

    def __init__(self, name, db_path, blob_path):
        self.name = name
        self.db_path = db_path
        self.blob_path = blob_path
        self.db = sqlite3.connect(db_path)
        self.meta = dict(self.db.execute("SELECT key, value FROM meta WHERE image=?", (name,)).fetchall())
        self._mem_cache = None
        self._succ_cache = None

    def close(self):
        self.db.close()

    # --- raw SQL ------------------------------------------------------------

    def sql(self, query, *args):
        return self.db.execute(query, args).fetchall()

    # --- functions and listings ----------------------------------------------

    def func(self, sw):
        """The function whose [entry_sw, end_sw) span contains sw, or None."""
        row = self.db.execute(
            "SELECT %s FROM functions WHERE image=? AND entry_sw<=? AND end_sw>?" % ",".join(_FUNC_COLUMNS),
            (self.name, sw, sw),
        ).fetchone()
        if row is None:
            return None
        d = dict(zip(_FUNC_COLUMNS, row))
        d["entry_sw"], d["end_sw"] = _hex(d["entry_sw"]), _hex(d["end_sw"])
        return d

    def listing(self, sw_or_func, n=None):
        """Aligned (sw, mnemonic) pairs from sw_or_func to the end of its
        function, capped at n when given."""
        fn = self.func(sw_or_func)
        if fn is None:
            return []
        rows = self.db.execute(
            "SELECT sw, mnemonic FROM insn WHERE image=? AND aligned=1 AND sw>=? AND sw<? ORDER BY sw",
            (self.name, sw_or_func, int(fn["end_sw"], 16)),
        ).fetchall()
        if n is not None:
            rows = rows[:n]
        return [(_hex(sw), mnemonic) for sw, mnemonic in rows]

    # --- call graph -----------------------------------------------------------

    def callers(self, sw, kinds=None):
        """Every edge (of any kind, or only `kinds`) targeting sw -- a CALL,
        JUMP or COND_JUMP into the middle of a function all count, not just
        CALL (see tools/sharcdb.py's module docstring)."""
        query = "SELECT kind, from_sw, from_function, cond, delayed, note FROM edges WHERE image=? AND to_sw=?"
        args = [self.name, sw]
        if kinds:
            query += " AND kind IN (%s)" % ",".join("?" * len(kinds))
            args += list(kinds)
        rows = self.db.execute(query, args).fetchall()
        return [
            {
                "kind": kind, "from_sw": _hex(from_sw),
                "from_function": _hex(from_function) if from_function is not None else None,
                "cond": cond, "delayed": delayed, "note": note,
            }
            for kind, from_sw, from_function, cond, delayed, note in rows
        ]

    def callees(self, func):
        rows = self.db.execute(
            "SELECT DISTINCT to_function FROM edges WHERE image=? AND from_function=? "
            "AND kind='call' AND to_function IS NOT NULL",
            (self.name, func),
        ).fetchall()
        return [_hex(r[0]) for r in rows]

    def roots(self):
        rows = self.db.execute(
            "SELECT sw, kind, note FROM roots WHERE image=? ORDER BY kind, sw", (self.name,)
        ).fetchall()
        return [{"sw": _hex(sw), "kind": kind, "note": note} for sw, kind, note in rows]

    # --- basic-block reachability ----------------------------------------------

    def _succ_graph(self):
        if self._succ_cache is None:
            g = nx.DiGraph()
            g.add_edges_from(self.db.execute(
                "SELECT from_block, to_block FROM succ WHERE image=? AND to_block IS NOT NULL", (self.name,)
            ).fetchall())
            self._succ_cache = g
        return self._succ_cache

    def _block_at(self, sw):
        row = self.db.execute(
            "SELECT start_sw FROM bblocks WHERE image=? AND start_sw<=? AND end_sw>?", (self.name, sw, sw)
        ).fetchone()
        return row[0] if row else None

    def reach(self, src, dst):
        """(bool, path) over succ alone (bblocks/succ, no call-crossing --
        see sharcdb.sql's "across calls" query for that superset), path a
        list of hex block-start addresses or None."""
        g = self._succ_graph()
        b1, b2 = self._block_at(src), self._block_at(dst)
        if b1 is None or b2 is None or not nx.has_path(g, b1, b2):
            return False, None
        return True, [_hex(b) for b in nx.shortest_path(g, b1, b2)]

    # --- registers and data references ------------------------------------------

    def last_def(self, reg, sw):
        """The last writer(s) of reg before sw. A bare int when there is one
        unambiguous writer, a list when several distinct blocks disagree,
        None when there is none."""
        rows = self.db.execute(
            _LAST_DEF_SQL,
            (sw, self.name, sw, sw, self.name, self.name, self.name, reg, self.name, self.name, reg),
        ).fetchall()
        writers = sorted(r[0] for r in rows if r[0] is not None)
        if not writers:
            return None
        return writers[0] if len(writers) == 1 else writers

    def uses(self, reg, func):
        fn = self.func(func)
        if fn is None:
            return []
        rows = self.db.execute(
            "SELECT sw FROM reguse WHERE image=? AND reg=? AND sw>=? AND sw<? ORDER BY sw",
            (self.name, reg, int(fn["entry_sw"], 16), int(fn["end_sw"], 16)),
        ).fetchall()
        return [_hex(r[0]) for r in rows]

    def refs(self, addr):
        rows = self.db.execute(
            "SELECT sw, form, role FROM dataref WHERE image=? AND value=? ORDER BY sw", (self.name, addr)
        ).fetchall()
        return [{"sw": _hex(sw), "form": form, "role": role} for sw, form, role in rows]

    def xref_table(self, addr, n):
        """Read n consecutive 32-bit little-endian loader words from addr as
        code pointers (the pattern behind roots.kind='code_pointer_array'),
        resolving each to its owning function when there is one."""
        mem = self._mem()
        out = []
        for i in range(n):
            raw = mem.read(addr + i * 4, 4)
            if raw is None:
                break
            value = int.from_bytes(raw, "little")
            fn = self.func(value)
            out.append({
                "index": i, "addr": _hex(addr + i * 4), "value": _hex(value),
                "function": fn["entry_sw"] if fn else None,
            })
        return out

    # --- cross-image ------------------------------------------------------------

    def match(self, other_img, func):
        """Functions in other_img matching func by relocation-tolerant
        hash, with whether the match is exact-byte too."""
        row = self.db.execute(
            "SELECT exact_hash, reloc_hash FROM func_hash WHERE image=? AND entry_sw=?", (self.name, func)
        ).fetchone()
        if row is None:
            return []
        exact_hash, reloc_hash = row
        rows = other_img.db.execute(
            "SELECT entry_sw, exact_hash FROM func_hash WHERE image=? AND reloc_hash=?",
            (other_img.name, reloc_hash),
        ).fetchall()
        return [{"entry_sw": _hex(entry_sw), "exact_match": exact_hash == other_exact}
                for entry_sw, other_exact in rows]

    # --- symbolic trace -----------------------------------------------------------

    def _mem(self):
        if self._mem_cache is None:
            with open(self.blob_path, "rb") as fh:
                data = fh.read()
            blocks = sharcldr.parse_blocks(data)
            self._mem_cache = sharcldr.LoadedMemory.from_stream(data, blocks)
        return self._mem_cache

    def trace(self, start, max_steps=100, max_states=32, pokes=None, provisional_forms=(), **regs):
        """tools/sharc_trace.py's trace(), in-process against this image's
        loaded memory: firmware DAG-modify constants seeded by default
        (override any of them, or add more, via **regs), concrete memory
        and 32-bit-normal-word addressing on by default. provisional_forms
        is passed straight through (e.g. ("14d",) to trace through Type14d).
        Raises NotImplementedError for a ColdFire image (see sharc.load's
        module docstring): sharc_trace only symbolically executes SHARC+."""
        if self.meta.get("cpu") == "coldfire":
            raise NotImplementedError(
                "Image.trace(): %r is a ColdFire image; sharc_trace only symbolically "
                "executes SHARC+ code, not m68k/ColdFire" % self.name
            )
        sets = dict(_DEFAULT_TRACE_REGS)
        sets.update(regs)
        return sharc_trace.trace(
            self._mem(), None, start, sets, max_steps, max_states,
            concrete_memory=True, assume_nw32=True, pokes=pokes, provisional_forms=provisional_forms,
        )


def main(argv=None):
    argv = sys.argv[1:] if argv is None else argv
    if len(argv) != 2:
        print('usage: sharc.py IMAGE "SQL"', file=sys.stderr)
        return 1
    img = load(argv[0])
    for row in img.sql(argv[1]):
        print(row)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
