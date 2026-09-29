#!/usr/bin/env python3
"""One small Python API over a tools/cfdb.py database: build/import once,
then query in-process instead of a fresh CLI round trip per question.

    import cf
    img = cf.load("dt2-1.16-emac")
    img.func(0x400caf48)                        # by address
    img.func("machine_descriptor_dispatch")      # by tools/cf_names.py name
    img.func("FUN_400caf48")                     # by Ghidra name
    img.callers(0x400caf48)
    img.reach(0x400caf48)                        # every function reachable by call
    img.paths(0x400337ba, 0x400caf48, max_depth=6)
    img.refs_to(0x80004704)                      # data_refs AND call/jump edges landing on it

Ad hoc SQL from the shell, without writing a script:

    uv run python tools/cf.py dt2-1.16-emac "SELECT kind, count(*) FROM edges GROUP BY kind"

Ghidra's call/data-ref tables miss code outside functions -- an empty
callers()/refs_to() here is not evidence of dead code (CLAUDE.md's Ghidra
section, tools/sharcdb.py's own note); confirm with tools/refscan.py on the
raw image before drawing that conclusion.
"""

from __future__ import annotations

import os
import sqlite3
import sys

_here = os.path.dirname(os.path.abspath(__file__))
if _here not in sys.path:
    sys.path.insert(0, _here)

import networkx as nx  # noqa: E402

import cfdb  # noqa: E402

try:
    import cf_names
except ImportError:  # pragma: no cover
    cf_names = None  # type: ignore[assignment]

GHIDRA_DIR = "out/ghidra"
DB_DIR = cfdb.DEFAULT_DB_DIR

_FUNC_COLUMNS = (
    "entry",
    "end",
    "name",
    "namespace",
    "size",
    "signature",
    "decomp_path",
    "disasm_path",
    "decomp_error",
)


def _hex(v):
    return "0x%x" % v if isinstance(v, int) else v


def load(name, ghidra_dir=None, db_dir=DB_DIR, force=False):
    """Open `db_dir`/<name>.sqlite, building (or rebuilding, if the dump's
    sha256 or tools/cfdb.py's DB_VERSION has moved on) from
    out/ghidra/<name> (or `ghidra_dir`) first when needed. If the Ghidra dump
    is gone but an up-to-date database is already on disk, that's fine --
    only a build or rebuild requires the dump."""
    dump_dir = ghidra_dir or os.path.join(GHIDRA_DIR, name)
    out_path = os.path.join(db_dir, name + ".sqlite")

    stale = force or not os.path.exists(out_path)
    if not stale:
        meta = cfdb.read_meta(out_path)
        manifest_path = os.path.join(dump_dir, "manifest.json")
        if meta.get("db_version") != str(cfdb.DB_VERSION):
            stale = True
        elif os.path.exists(manifest_path):
            import json

            with open(manifest_path) as f:
                sha = json.load(f).get("image_sha256")
            if sha and meta.get("image_sha256") != sha:
                stale = True

    if stale:
        if not os.path.exists(os.path.join(dump_dir, "manifest.json")):
            raise FileNotFoundError(
                "cf.load(%r): %s is missing or stale and %s has no manifest.json to rebuild from"
                % (name, out_path, dump_dir)
            )
        cfdb.build_database(dump_dir, out_path, name=name, force=True)

    return Image(name, out_path)


class Image:
    """A queryable ColdFire image: a handle on out/cfdb/<name>.sqlite."""

    def __init__(self, name, db_path):
        self.name = name
        self.db_path = db_path
        self.db = sqlite3.connect(db_path)
        self.meta = dict(
            self.db.execute(
                "SELECT key, value FROM meta WHERE image=?", (name,)
            ).fetchall()
        )
        self._callgraph_cache = None

    def close(self):
        self.db.close()

    # --- raw SQL -----------------------------------------------------------

    def sql(self, query, *args):
        return self.db.execute(query, args).fetchall()

    # --- functions -----------------------------------------------------------

    def _resolve(self, addr_or_name):
        """int address, or None if `addr_or_name` doesn't resolve to one:
        a bare int/hex string, a tools/cf_names.py name, or a Ghidra function
        name (functions.name)."""
        if isinstance(addr_or_name, int):
            return addr_or_name
        text = addr_or_name
        if text.startswith("0x") or text.startswith("0X"):
            try:
                return int(text, 16)
            except ValueError:
                pass
        if cf_names is not None and text in cf_names.ADDRS:
            return cf_names.ADDRS[text][0]
        row = self.db.execute(
            "SELECT entry FROM functions WHERE image=? AND name=?", (self.name, text)
        ).fetchone()
        return row[0] if row else None

    def func(self, addr_or_name):
        """The function whose [entry, end) span contains addr, or the
        function named addr_or_name (a tools/cf_names.py name or a Ghidra
        functions.name) -- see _resolve(). None if nothing matches."""
        addr = self._resolve(addr_or_name)
        if addr is None:
            return None
        row = self.db.execute(
            "SELECT %s FROM functions WHERE image=? AND entry<=? AND end>? "
            "ORDER BY entry DESC LIMIT 1" % ",".join(_FUNC_COLUMNS),
            (self.name, addr, addr),
        ).fetchone()
        if row is None:
            return None
        d = dict(zip(_FUNC_COLUMNS, row, strict=True))
        d["entry"], d["end"] = _hex(d["entry"]), _hex(d["end"])
        cf_name = cf_names.NAME_BY_ADDR.get(addr) if cf_names is not None else None
        if cf_name is None and cf_names is not None:
            cf_name = cf_names.NAME_BY_ADDR.get(int(d["entry"], 16))
        d["cf_name"] = cf_name
        return d

    def decomp(self, addr_or_name):
        """The decompiled C for one function, read live from the Ghidra dump
        directory (meta['ghidra_dump_dir']) -- never copied into the
        database itself (see tools/cfdb.py's module docstring). None if the
        function is unknown or has no decomp file."""
        f = self.func(addr_or_name)
        if f is None or not f["decomp_path"]:
            return None
        dump_dir = self.meta.get("ghidra_dump_dir")
        if not dump_dir:
            return None
        path = os.path.join(dump_dir, f["decomp_path"])
        if not os.path.exists(path):
            return None
        with open(path) as fh:
            return fh.read()

    def disasm(self, addr_or_name):
        """The disassembly listing text for one function, same caveats as
        decomp()."""
        f = self.func(addr_or_name)
        if f is None or not f["disasm_path"]:
            return None
        dump_dir = self.meta.get("ghidra_dump_dir")
        if not dump_dir:
            return None
        path = os.path.join(dump_dir, f["disasm_path"])
        if not os.path.exists(path):
            return None
        with open(path) as fh:
            return fh.read()

    # --- call graph -----------------------------------------------------------

    def callers(self, addr_or_name, kinds=None):
        """Every edge (of any kind, or only `kinds` -- 'call'/'jump'/
        'cond_jump') targeting addr's owning function's entry OR addr
        itself: a JUMP into the middle of a function is a real caller too
        (see tools/sharcdb.py's own note on this -- Ghidra's own CALL-only
        xref view misses it), not just CALL."""
        addr = self._resolve(addr_or_name)
        if addr is None:
            return []
        f = self.func(addr)
        targets = {addr}
        if f is not None:
            targets.add(int(f["entry"], 16))
        query = (
            "SELECT kind, raw_kind, site, from_func, to_func, to_addr FROM edges "
            "WHERE image=? AND to_addr IN (%s)" % ",".join("?" * len(targets))
        )
        args = [self.name, *targets]
        if kinds:
            query += " AND kind IN (%s)" % ",".join("?" * len(kinds))
            args += list(kinds)
        rows = self.db.execute(query, args).fetchall()
        return [
            {
                "kind": kind,
                "raw_kind": raw_kind,
                "site": _hex(site),
                "from_func": _hex(from_func) if from_func is not None else None,
                "to_func": _hex(to_func) if to_func is not None else None,
                "to_addr": _hex(to_addr) if to_addr is not None else None,
            }
            for kind, raw_kind, site, from_func, to_func, to_addr in rows
        ]

    def callees(self, addr_or_name, kinds=None):
        addr = self._resolve(addr_or_name)
        if addr is None:
            return []
        f = self.func(addr)
        entry = int(f["entry"], 16) if f is not None else addr
        query = (
            "SELECT DISTINCT kind, to_func, to_addr FROM edges "
            "WHERE image=? AND from_func=? AND to_func IS NOT NULL"
        )
        args = [self.name, entry]
        if kinds:
            query += " AND kind IN (%s)" % ",".join("?" * len(kinds))
            args += list(kinds)
        rows = self.db.execute(query, args).fetchall()
        return [
            {"kind": kind, "to_func": _hex(to_func), "to_addr": _hex(to_addr)}
            for kind, to_func, to_addr in rows
        ]

    def callgraph(self):
        """The whole image's function-level call graph (edges.kind='call',
        to_func resolved), built once and cached."""
        if self._callgraph_cache is not None:
            return self._callgraph_cache
        g = nx.DiGraph()
        g.add_nodes_from(
            r[0]
            for r in self.db.execute(
                "SELECT entry FROM functions WHERE image=?", (self.name,)
            )
        )
        g.add_edges_from(
            self.db.execute(
                "SELECT DISTINCT from_func, to_func FROM edges WHERE image=? AND kind='call' "
                "AND from_func IS NOT NULL AND to_func IS NOT NULL",
                (self.name,),
            ).fetchall()
        )
        self._callgraph_cache = g
        return g

    def reach(self, addr_or_name, max_depth=None):
        """Every function transitively reachable from addr_or_name by a call
        edge (BFS over callgraph()), as a {entry_hex: depth} dict. Bounded by
        `max_depth` when given (None: unbounded)."""
        addr = self._resolve(addr_or_name)
        if addr is None:
            return {}
        f = self.func(addr)
        entry = int(f["entry"], 16) if f is not None else addr
        g = self.callgraph()
        if entry not in g:
            return {}
        out = {}
        for node, depth in nx.single_source_shortest_path_length(
            g, entry, cutoff=max_depth
        ).items():
            out[_hex(node)] = depth
        return out

    def paths(self, a, b, max_depth=None):
        """Every simple call-graph path from a to b (each a hex address int,
        a tools/cf_names.py name or a Ghidra function name), each a list of
        hex entry addresses, shortest first. `max_depth` caps the number of
        call edges in a path (nx.all_simple_paths' own `cutoff`); omit it on
        a large graph only once you know the two functions are close, or
        this can be slow."""
        addr_a, addr_b = self._resolve(a), self._resolve(b)
        if addr_a is None or addr_b is None:
            return []
        fa, fb = self.func(addr_a), self.func(addr_b)
        entry_a = int(fa["entry"], 16) if fa is not None else addr_a
        entry_b = int(fb["entry"], 16) if fb is not None else addr_b
        g = self.callgraph()
        if entry_a not in g or entry_b not in g:
            return []
        try:
            paths = nx.all_simple_paths(g, entry_a, entry_b, cutoff=max_depth)
            return sorted(([_hex(n) for n in p] for p in paths), key=len)
        except nx.NodeNotFound:
            return []

    # --- data references --------------------------------------------------------

    def refs_to(self, addr):
        """Every datarefs row AND every edge whose to_addr is `addr`: a data
        access and a code (call/jump) reference both count, since a function
        pointer stored in a table shows up as an edge here, not a dataref."""
        data_rows = self.db.execute(
            "SELECT site, func, kind, label, block FROM datarefs WHERE image=? AND to_addr=? ORDER BY site",
            (self.name, addr),
        ).fetchall()
        edge_rows = self.db.execute(
            "SELECT site, from_func, kind, raw_kind FROM edges WHERE image=? AND to_addr=? ORDER BY site",
            (self.name, addr),
        ).fetchall()
        return {
            "data": [
                {
                    "site": _hex(site),
                    "func": _hex(func) if func is not None else None,
                    "kind": kind,
                    "label": label,
                    "block": block,
                }
                for site, func, kind, label, block in data_rows
            ],
            "code": [
                {
                    "site": _hex(site),
                    "from_func": _hex(from_func) if from_func is not None else None,
                    "kind": kind,
                    "raw_kind": raw_kind,
                }
                for site, from_func, kind, raw_kind in edge_rows
            ],
        }

    def strings_at(self, addr):
        rows = self.db.execute(
            "SELECT text FROM strings WHERE image=? AND addr=?", (self.name, addr)
        ).fetchall()
        return [r[0] for r in rows]

    def find_string(self, substring):
        rows = self.db.execute(
            "SELECT addr, text FROM strings WHERE image=? AND text LIKE ? ORDER BY addr",
            (self.name, "%" + substring + "%"),
        ).fetchall()
        return [{"addr": _hex(addr), "text": text} for addr, text in rows]


def main(argv=None):
    argv = sys.argv[1:] if argv is None else argv
    if len(argv) != 2:
        print('usage: tools/cf.py IMAGE "SQL"', file=sys.stderr)
        return 2
    name, query = argv
    img = load(name)
    for row in img.sql(query):
        print(row)
    return 0


if __name__ == "__main__":
    sys.exit(main())
