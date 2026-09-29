#!/usr/bin/env python3
"""One queryable SQLite database per ColdFire (MAIN OS) image: functions,
call/jump edges, data references, strings and symbols, imported from a
tools/ghidradump.py dump directory (manifest.json, xrefs.sqlite, decomp/,
disasm/).

    uv run python tools/cfdb.py build out/ghidra/dt2-1.16-emac
        [--name dt2-1.16] [--out out/cfdb/dt2-1.16.sqlite] [--force]

This exists for the same reason tools/sharcdb.py does for the SHARC+ side
(see that file's module docstring): "who calls 0x400caf48" or "what writes
0x80004704" should be a `sqlite3` query in milliseconds against a database
built once, not a fresh xrefs.sqlite scan and a throwaway script every time.
It is a separate tool from tools/sharcdb.py (which also has an
`import-ghidra` subcommand, built for the same xrefs.sqlite/manifest.json
shape) rather than a second image type sharing that file's schema and
DB_VERSION: sharcdb.py's DB_VERSION and code-block bookkeeping exist for the
SHARC+ side's own decode concerns, and a ColdFire image has no `sw` (SHARC
sequencer word) addressing, no code-block selection and no basic-block/
reaching-definitions layer -- keeping this schema and version number
separate means a SHARC-only schema change never forces a ColdFire rebuild
and vice versa. tools/cf.py is the query layer, mirroring tools/sharc.py.

Ghidra's call/data-ref tables miss code outside functions (small trampolines,
raw branch targets never turned into a function) -- an empty `callers()`
here is not evidence of dead code; confirm with tools/refscan.py on the raw
image before recording "no caller" (see CLAUDE.md's Ghidra section and
tools/sharcdb.py's own "empty Ghidra caller list" note).

Decompiled C and disassembly text are NOT copied into the database (60 MB /
72 MB for the current dt2-1.16-emac dump -- see functions.decomp_path/
disasm_path, resolved against meta['ghidra_dump_dir'] by tools/cf.py's
Image.decomp()/disasm() instead of duplicating that text here).
"""

from __future__ import annotations

import argparse
import collections
import json
import os
import sqlite3
import sys
import time

DB_VERSION = 1
DEFAULT_DB_DIR = "out/cfdb"

SCHEMA = """
CREATE TABLE meta (image TEXT, key TEXT, value TEXT, PRIMARY KEY (image, key));

CREATE TABLE functions (
  image TEXT, entry INTEGER, end INTEGER, name TEXT, namespace TEXT,
  size INTEGER, signature TEXT, decomp_path TEXT, disasm_path TEXT,
  decomp_error TEXT,
  PRIMARY KEY (image, entry)
);
CREATE INDEX functions_range ON functions(image, entry, end);
CREATE INDEX functions_name ON functions(image, name);

-- kind: normalized ('call'/'jump'/'cond_jump'); raw_kind: Ghidra's own
-- RefType name (UNCONDITIONAL_CALL, COMPUTED_JUMP, ...).
CREATE TABLE edges (
  image TEXT, site INTEGER, from_func INTEGER, to_func INTEGER,
  to_addr INTEGER, kind TEXT, raw_kind TEXT
);
CREATE INDEX edges_to_func ON edges(image, to_func);
CREATE INDEX edges_to_addr ON edges(image, to_addr);
CREATE INDEX edges_from_func ON edges(image, from_func);

CREATE TABLE datarefs (
  image TEXT, site INTEGER, func INTEGER, to_addr INTEGER, kind TEXT,
  label TEXT, block TEXT
);
CREATE INDEX datarefs_addr ON datarefs(image, to_addr);
CREATE INDEX datarefs_func ON datarefs(image, func);

CREATE TABLE strings (image TEXT, addr INTEGER, text TEXT);
CREATE INDEX strings_addr ON strings(image, addr);

CREATE TABLE symbols (
  image TEXT, addr INTEGER, name TEXT, namespace TEXT, type TEXT,
  is_primary INTEGER
);
CREATE INDEX symbols_addr ON symbols(image, addr);
CREATE INDEX symbols_name ON symbols(image, name);
"""

_CALL_KIND = {
    "UNCONDITIONAL_CALL": "call",
    "COMPUTED_CALL": "call",
    "UNCONDITIONAL_JUMP": "jump",
    "COMPUTED_JUMP": "jump",
    "CONDITIONAL_JUMP": "cond_jump",
}


def open_db(path):
    db = sqlite3.connect(path)
    db.executescript(SCHEMA)
    return db


def read_meta(path):
    if not os.path.exists(path):
        return {}
    db = sqlite3.connect(path)
    try:
        image = db.execute("SELECT DISTINCT image FROM meta").fetchone()
        if image is None:
            return {}
        return dict(
            db.execute("SELECT key, value FROM meta WHERE image=?", image).fetchall()
        )
    finally:
        db.close()


def _table_exists(db, name):
    return (
        db.execute(
            "SELECT 1 FROM sqlite_master WHERE type='table' AND name=?", (name,)
        ).fetchone()
        is not None
    )


def _function_end(entry, size, ranges_by_func):
    """The entry's own contiguous end address: the range whose lo == entry
    when the dump recorded one (a function can have disjoint chunks, e.g. a
    switch's case bodies far from the entry -- see tools/sharcdb.py's
    build_ghidra_database for the same situation on the SHARC side), else a
    size-based fallback."""
    spans = ranges_by_func.get(entry)
    if not spans:
        return entry + max(size or 1, 1)
    for lo, hi in spans:
        if lo == entry:
            return hi + 1
    return max(hi for _lo, hi in spans) + 1


def build_database(dump_dir, out_path, name=None, force=False):
    """Import a tools/ghidradump.py dump directory into out_path. Returns a
    stats dict with skipped=True (and no other keys but name/path/seconds/
    size) when the existing database's image_sha256+DB_VERSION already match
    the dump's manifest and `force` wasn't given."""
    t0 = time.time()
    manifest_path = os.path.join(dump_dir, "manifest.json")
    if not os.path.exists(manifest_path):
        raise SystemExit("cfdb build: no manifest.json in %s" % dump_dir)
    with open(manifest_path) as f:
        manifest = json.load(f)
    if not manifest.get("complete"):
        raise SystemExit(
            "cfdb build: %s is not a complete dump (manifest 'complete' is false)"
            % dump_dir
        )
    sha = manifest.get("image_sha256")
    if not sha:
        raise SystemExit(
            "cfdb build: %s has no image_sha256 in its manifest" % dump_dir
        )
    if name is None:
        name = os.path.basename(os.path.normpath(dump_dir))

    if not force and os.path.exists(out_path):
        existing = read_meta(out_path)
        if existing.get("image_sha256") == sha and existing.get("db_version") == str(
            DB_VERSION
        ):
            return {
                "name": name,
                "path": out_path,
                "skipped": True,
                "seconds": time.time() - t0,
                "size": os.path.getsize(out_path),
            }

    xrefs_path = os.path.join(dump_dir, "xrefs.sqlite")
    if not os.path.exists(xrefs_path):
        raise SystemExit("cfdb build: no xrefs.sqlite in %s" % dump_dir)
    src = sqlite3.connect(xrefs_path)
    try:
        functions = src.execute(
            "SELECT entry, name, namespace, size, signature, decomp, disasm, decomp_error "
            "FROM functions ORDER BY entry"
        ).fetchall()
        ranges_by_func = collections.defaultdict(list)
        for func, lo, hi in src.execute("SELECT func, lo, hi FROM function_ranges"):
            ranges_by_func[func].append((lo, hi))
        calls = src.execute(
            "SELECT from_func, to_func, to_addr, site, kind FROM calls"
        ).fetchall()
        data_refs = src.execute(
            "SELECT site, func, to_addr, kind, label, block FROM data_refs"
        ).fetchall()
        strings = (
            src.execute("SELECT addr, text FROM strings").fetchall()
            if _table_exists(src, "strings")
            else []
        )
        symbols = (
            src.execute(
                "SELECT addr, name, namespace, type, is_primary FROM symbols"
            ).fetchall()
            if _table_exists(src, "symbols")
            else []
        )
    finally:
        src.close()

    out_dir = os.path.dirname(os.path.abspath(out_path)) or "."
    os.makedirs(out_dir, exist_ok=True)
    tmp_path = os.path.join(
        out_dir,
        ".%s.tmp-%d-%d"
        % (os.path.basename(out_path), os.getpid(), int(time.time() * 1e6)),
    )
    try:
        db = open_db(tmp_path)
        try:
            db.executemany(
                "INSERT INTO meta VALUES (?,?,?)",
                [
                    (name, "image", name),
                    (name, "image_sha256", sha),
                    (name, "db_version", str(DB_VERSION)),
                    (
                        name,
                        "build_time",
                        time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()),
                    ),
                    (name, "ghidra_dump_dir", os.path.abspath(dump_dir)),
                    (name, "ghidra_project", manifest.get("project") or ""),
                    (name, "ghidra_program", manifest.get("program") or ""),
                    (name, "language", manifest.get("language") or ""),
                ],
            )

            func_rows = [
                (
                    name,
                    entry,
                    _function_end(entry, size, ranges_by_func),
                    fname,
                    namespace,
                    size,
                    signature,
                    decomp,
                    disasm,
                    decomp_error,
                )
                for entry, fname, namespace, size, signature, decomp, disasm, decomp_error in functions
            ]
            db.executemany(
                "INSERT INTO functions VALUES (?,?,?,?,?,?,?,?,?,?)", func_rows
            )

            edge_rows = [
                (
                    name,
                    site,
                    from_func,
                    to_func,
                    to_addr,
                    _CALL_KIND.get(kind, "jump"),
                    kind,
                )
                for from_func, to_func, to_addr, site, kind in calls
            ]
            db.executemany("INSERT INTO edges VALUES (?,?,?,?,?,?,?)", edge_rows)

            dataref_rows = [
                (name, site, func, to_addr, kind, label, block)
                for site, func, to_addr, kind, label, block in data_refs
            ]
            db.executemany("INSERT INTO datarefs VALUES (?,?,?,?,?,?,?)", dataref_rows)

            string_rows = [(name, addr, text) for addr, text in strings]
            db.executemany("INSERT INTO strings VALUES (?,?,?)", string_rows)

            symbol_rows = [
                (name, addr, sname, namespace, typ, is_primary)
                for addr, sname, namespace, typ, is_primary in symbols
            ]
            db.executemany("INSERT INTO symbols VALUES (?,?,?,?,?,?)", symbol_rows)

            db.commit()
        finally:
            db.close()
        os.replace(tmp_path, out_path)
    except BaseException:
        if os.path.exists(tmp_path):
            os.remove(tmp_path)
        raise

    return {
        "name": name,
        "path": out_path,
        "skipped": False,
        "seconds": time.time() - t0,
        "size": os.path.getsize(out_path),
        "n_functions": len(func_rows),
        "n_edges": len(edge_rows),
        "n_datarefs": len(dataref_rows),
        "n_strings": len(string_rows),
        "n_symbols": len(symbol_rows),
    }


def cmd_build(args):
    name = args.name or os.path.basename(os.path.normpath(args.dump_dir))
    out = args.out or os.path.join(args.out_dir, name + ".sqlite")
    r = build_database(args.dump_dir, out, name=name, force=args.force)
    if r["skipped"]:
        print(
            "%-16s SKIPPED (up to date)  %s  %.1fs"
            % (r["name"], r["path"], r["seconds"])
        )
        return 0
    print(
        "%-16s built  %s  %.1fs  %.1f KB  functions=%d edges=%d datarefs=%d strings=%d symbols=%d"
        % (
            r["name"],
            r["path"],
            r["seconds"],
            r["size"] / 1024,
            r["n_functions"],
            r["n_edges"],
            r["n_datarefs"],
            r["n_strings"],
            r["n_symbols"],
        )
    )
    return 0


def main(argv=None):
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    sub = ap.add_subparsers(dest="cmd", required=True)

    b = sub.add_parser(
        "build", help="build a ColdFire image database from a Ghidra dump"
    )
    b.add_argument("dump_dir", help="e.g. out/ghidra/dt2-1.16-emac")
    b.add_argument(
        "--name", help="image name; default is the dump directory's own name"
    )
    b.add_argument(
        "--out", help="output .sqlite path (default: --out-dir/<name>.sqlite)"
    )
    b.add_argument("--out-dir", default=DEFAULT_DB_DIR)
    b.add_argument(
        "--force",
        action="store_true",
        help="rebuild even if sha256+DB_VERSION already match",
    )

    args = ap.parse_args(argv)
    if args.cmd == "build":
        return cmd_build(args)
    return 1


if __name__ == "__main__":
    sys.exit(main())
