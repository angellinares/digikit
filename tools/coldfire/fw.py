#!/usr/bin/env python3
"""fw.py: one compact query CLI over the firmware databases, for agents.

    uv run python -m tools.coldfire.fw fn 0x401be1be -i dt2-1.17
    uv run python -m tools.coldfire.fw batch queries.txt -i dt2-1.17      # or '-' for stdin

IMAGE is dt2-1.17, dn2-1.12, ... : ColdFire (out/cfdb/IMAGE-emac.sqlite) and
SHARC (out/sharcdb/IMAGE.sqlite) are both opened from it. The CPU is chosen
from the address (>= 0x400000 is ColdFire) or with --cpu. Set FW_IMAGE to skip
-i. Text output is capped at --max lines (default 60; 0 = no cap) with a
"... N more" tail. The stores command instead returns uncapped JSON.

Subcommands: fn decomp disasm callers callees xref str class path port sql
search atlas catalog dossier stores batch. In a batch file each line is one command line (without
"fw.py"); lines starting with # are comments; every result block starts with
"## <line>". The call graph is cached on disk (out/cache/fw/), keyed by the
database hash, so a cold process only pays for opening SQLite.

Ghidra's call/data-ref tables miss code outside functions: an empty callers
list is not evidence of dead code (AGENTS.md; tools/coldfire/refscan.py confirms).
"""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import pickle
import re
import shlex
import sqlite3
import sys
from collections import deque
from collections.abc import Iterable, Sequence
from typing import Any

HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.dirname(os.path.dirname(HERE))

CF_DIR = os.path.join(ROOT, "out", "cfdb")
SHARC_DIR = os.path.join(ROOT, "out", "sharcdb")
ATLAS_DIR = os.path.join(ROOT, "out", "atlas")
CACHE_DIR = os.path.join(ROOT, "out", "cache", "fw")
DEFAULT_MAX = 60
CF_BASE = 0x10000000  # ColdFire addresses start at 0x40000000, SHARC words stay below 0x1000000


class FwError(Exception):
    """A usage or lookup problem reported as one line, not a traceback."""


# --- output -----------------------------------------------------------------


def capped(lines: Sequence[str], cap: int) -> str:
    lines = list(lines)
    if cap and len(lines) > cap:
        more = len(lines) - cap
        lines = lines[:cap] + ["... %d more (--max N, 0 = all)" % more]
    return "\n".join(lines)


def clip(text: str, n: int = 100) -> str:
    text = " ".join(text.split())
    return text if len(text) <= n else text[: n - 3] + "..."


# --- shared helpers -----------------------------------------------------------


def short_image(name: str) -> str:
    return name[: -len("-emac")] if name.endswith("-emac") else name


def parse_addr(text: str) -> int | None:
    t = text.strip().lower()
    if t.startswith("fun_") or t.startswith("lab_") or t.startswith("dat_"):
        t = t[4:]
    try:
        if t.startswith("0x"):
            return int(t, 16)
        if re.fullmatch(r"[0-9a-f]{6,8}", t) and re.search(r"[a-f]", t + "a"):
            return int(t, 16)
    except ValueError:
        return None
    return None


def tri_query(term: str) -> str:
    """Trigram FTS query: AND of words, or one phrase when a word is under 3 characters."""
    words = [w.replace('"', "") for w in term.split()]
    if not words:
        raise FwError("empty search term")
    if any(len(w) < 3 for w in words):
        return '"%s"' % " ".join(words)
    return " AND ".join('"%s"' % w for w in words)


def fts_query(term: str, prefix: bool = False) -> str:
    """AND of the quoted words in `term` as an FTS5 query."""
    words = [w.replace('"', "") for w in term.split() if w.replace('"', "")]
    if not words:
        raise FwError("empty search term")
    return " AND ".join('"%s"%s' % (w, "*" if prefix else "") for w in words)


def open_ro(path: str) -> sqlite3.Connection:
    if not os.path.exists(path):
        raise FwError("missing database %s" % os.path.relpath(path, ROOT))
    return sqlite3.connect("file:%s?mode=ro" % path, uri=True)


def db_key(con: sqlite3.Connection, path: str) -> str:
    meta = (
        dict(con.execute("SELECT key, value FROM meta").fetchall())
        if _has(con, "meta")
        else {}
    )
    sha = (meta.get("image_sha256") or "nosha")[:12]
    return "%s-%s-%s-%d" % (
        sha,
        meta.get("db_version", "0"),
        (meta.get("build_time") or "")[-9:-1].replace(":", ""),
        os.path.getsize(path),
    )


def _has(con: sqlite3.Connection, table: str) -> bool:
    return (
        con.execute("SELECT 1 FROM sqlite_master WHERE name=?", (table,)).fetchone()
        is not None
    )


class Graph:
    """Function-level call graph as plain dicts (no networkx import)."""

    def __init__(
        self, succ: dict[int, tuple[int, ...]], pred: dict[int, tuple[int, ...]]
    ):
        self.succ = succ
        self.pred = pred

    @staticmethod
    def load(tag: str, key: str, edges: Iterable[tuple[int, int]]) -> tuple[Graph, str]:
        path = os.path.join(CACHE_DIR, "%s.%s.graph.pkl" % (tag, key))
        try:
            with open(path, "rb") as fh:
                succ, pred = pickle.load(fh)
            return Graph(succ, pred), "cached"
        except (OSError, pickle.UnpicklingError, EOFError, ValueError):
            pass
        s: dict[int, set[int]] = {}
        p: dict[int, set[int]] = {}
        for a, b in edges:
            s.setdefault(a, set()).add(b)
            p.setdefault(b, set()).add(a)
        succ = {k: tuple(sorted(v)) for k, v in s.items()}
        pred = {k: tuple(sorted(v)) for k, v in p.items()}
        try:
            os.makedirs(CACHE_DIR, exist_ok=True)
            for old in os.listdir(CACHE_DIR):
                if old.startswith(tag + ".") and old.endswith(".graph.pkl"):
                    os.remove(os.path.join(CACHE_DIR, old))
            tmp = path + ".%d" % os.getpid()
            with open(tmp, "wb") as fh:
                pickle.dump((succ, pred), fh, protocol=pickle.HIGHEST_PROTOCOL)
            os.replace(tmp, path)
        except OSError:
            pass
        return Graph(succ, pred), "built"

    def shortest(self, a: int, b: int, limit: int = 200000) -> list[int] | None:
        if a == b:
            return [a]
        prev = {a: a}
        q = deque([a])
        seen = 0
        while q:
            n = q.popleft()
            for m in self.succ.get(n, ()):
                if m in prev:
                    continue
                prev[m] = n
                if m == b:
                    out = [b]
                    while out[-1] != a:
                        out.append(prev[out[-1]])
                    return out[::-1]
                q.append(m)
            seen += 1
            if seen > limit:
                break
        return None


# --- ColdFire -----------------------------------------------------------------


class CF:
    cpu = "cf"

    def __init__(self, image: str):
        self.image = short_image(image)
        self.db_name = self.image + "-emac"
        self.path = os.path.join(CF_DIR, self.db_name + ".sqlite")
        self.db = open_ro(self.path)
        self.meta = dict(self.db.execute("SELECT key, value FROM meta").fetchall())
        self._graph: Graph | None = None
        self._fts: sqlite3.Connection | None = None
        self._names: dict[int, str] | None = None
        self._starts: list[int] | None = None
        self.graph_state = ""

    # names and lookup
    def names(self) -> dict[int, str]:
        if self._names is None:
            from tools.coldfire import cfdb

            self._names = cfdb._image_names(self.db_name)
        return self._names

    def func_row(self, addr: int) -> tuple[Any, ...] | None:
        return self.db.execute(
            "SELECT entry, end, name, namespace, size, signature, decomp_path, disasm_path "
            "FROM functions WHERE entry<=? AND end>? ORDER BY entry DESC LIMIT 1",
            (addr, addr),
        ).fetchone()

    def resolve(self, token: str) -> int:
        """Entry of the function named or containing `token`."""
        addr = parse_addr(token)
        if addr is None:
            for k in self.names():
                if token == _cfname(self, k):
                    addr = k
                    break
        if addr is None:
            row = self.db.execute(
                "SELECT entry FROM functions WHERE name=?", (token,)
            ).fetchone()
            if row is None:
                row = self.db.execute(
                    "SELECT entry FROM functions WHERE name LIKE ? ORDER BY entry LIMIT 1",
                    (token + "%",),
                ).fetchone()
            if row is None:
                raise FwError(
                    "no function matches %r (try: search %s)" % (token, token)
                )
            addr = row[0]
        f = self.func_row(addr)
        if f is None:
            raise FwError("0x%x is not inside a known function" % addr)
        return f[0]

    def label(self, entry: int) -> str:
        n = self.names().get(entry)
        row = self.db.execute(
            "SELECT name FROM functions WHERE entry=?", (entry,)
        ).fetchone()
        g = row[0] if row else "FUN_%08x" % entry
        return "%08x %s%s" % (entry, g, " [%s]" % n if n else "")

    def label_any(self, addr: int) -> str:
        f = self.func_row(addr)
        if f is None:
            return "%08x" % addr
        if f[0] == addr:
            return self.label(addr)
        return "%s+0x%x" % (self.label(f[0]), addr - f[0])

    def graph(self) -> Graph:
        if self._graph is None:
            self._graph, self.graph_state = Graph.load(
                self.db_name,
                db_key(self.db, self.path),
                self.db.execute(
                    "SELECT DISTINCT from_func, to_func FROM edges WHERE kind='call' "
                    "AND from_func IS NOT NULL AND to_func IS NOT NULL"
                ),
            )
        return self._graph

    def fts(self) -> sqlite3.Connection:
        if self._fts is None:
            from tools.coldfire import cfdb

            path = cfdb.fts_path(self.path)
            if not os.path.exists(path):
                print("# building full-text index (one time)", file=sys.stderr)
                cfdb.build_fts(self.path)
            self._fts = open_ro(path)
        return self._fts

    def read_dump(self, rel: str | None) -> str | None:
        dump = self.meta.get("ghidra_dump_dir")
        if not rel or not dump:
            return None
        p = os.path.join(dump, rel)
        if not os.path.exists(p):
            p = os.path.join(ROOT, "out", "ghidra", self.db_name, rel)
        try:
            with open(p, errors="replace") as fh:
                return fh.read()
        except OSError:
            return None

    # queries
    def callers(self, entry: int) -> list[tuple[int, int, str]]:
        """(from_func, site, kind) for edges into the function or its entry."""
        return self.db.execute(
            "SELECT from_func, site, kind FROM edges WHERE to_addr=? OR to_func=? ORDER BY from_func, site",
            (entry, entry),
        ).fetchall()

    def callees(self, entry: int) -> list[tuple[int, str]]:
        return self.db.execute(
            "SELECT DISTINCT to_func, kind FROM edges WHERE from_func=? AND to_func IS NOT NULL ORDER BY to_func",
            (entry,),
        ).fetchall()

    def strings_of(self, entry: int, limit: int = 8) -> list[tuple[int, str]]:
        return self.db.execute(
            "SELECT DISTINCT s.addr, s.text FROM datarefs d JOIN strings s ON s.addr=d.to_addr "
            "WHERE d.func=? ORDER BY s.addr LIMIT ?",
            (entry, limit),
        ).fetchall()

    def vtable_slots(self, entry: int) -> list[tuple[str, int]]:
        return self.db.execute(
            "SELECT DISTINCT class, slot FROM vtables WHERE target=? ORDER BY class, slot",
            (entry,),
        ).fetchall()


def _cfname(img: CF, entry: int) -> str | None:
    return img.names().get(entry)


# --- SHARC --------------------------------------------------------------------


class Sharc:
    cpu = "sharc"

    def __init__(self, image: str):
        self.image = short_image(image)
        self.path = os.path.join(SHARC_DIR, self.image + ".sqlite")
        self.db = open_ro(self.path)
        self.meta = (
            dict(self.db.execute("SELECT key, value FROM meta").fetchall())
            if _has(self.db, "meta")
            else {}
        )
        self.meta = {k.split("|")[-1]: v for k, v in self.meta.items()} or self.meta
        rows = (
            self.db.execute(
                "SELECT key, value FROM meta WHERE image=?", (self.image,)
            ).fetchall()
            if _has(self.db, "meta")
            else []
        )
        if rows:
            self.meta = dict(rows)
        self._graph: Graph | None = None
        self.graph_state = ""
        notes = os.path.join(SHARC_DIR, self.image + ".notes.sqlite")
        self.notes = open_ro(notes) if os.path.exists(notes) else None

    def func_row(self, sw: int) -> tuple[Any, ...] | None:
        return self.db.execute(
            "SELECT entry_sw, end_sw, n_insns, block, name, label, entry_kind, is_leaf FROM functions "
            "WHERE entry_sw<=? AND end_sw>? ORDER BY entry_sw DESC LIMIT 1",
            (sw, sw),
        ).fetchone()

    def resolve(self, token: str) -> int:
        addr = parse_addr(token)
        if addr is None:
            row = self.db.execute(
                "SELECT entry_sw FROM functions WHERE name=?", (token,)
            ).fetchone()
            if row is None:
                raise FwError(
                    "no SHARC function named %r (try: search %s)" % (token, token)
                )
            addr = row[0]
        f = self.func_row(addr)
        if f is None:
            raise FwError("0x%x is not inside a known SHARC function" % addr)
        return f[0]

    def note(self, entry: int) -> tuple[str, str] | None:
        if self.notes is None:
            return None
        try:
            row = self.notes.execute(
                "SELECT role, summary FROM notes WHERE function_sw=?", (entry,)
            ).fetchone()
        except sqlite3.Error:
            return None
        return row

    def label(self, entry: int) -> str:
        row = self.db.execute(
            "SELECT name, label FROM functions WHERE entry_sw=?", (entry,)
        ).fetchone()
        n = self.note(entry)
        extra = ""
        if n:
            extra = " {%s}" % n[0]
        elif row and row[1]:
            extra = " (%s)" % row[1]
        return "%06x %s%s" % (entry, row[0] if row else "FUN_%06x" % entry, extra)

    def graph(self) -> Graph:
        if self._graph is None:
            self._graph, self.graph_state = Graph.load(
                "sharc-" + self.image,
                db_key(self.db, self.path),
                self.db.execute(
                    "SELECT DISTINCT from_function, to_function FROM edges WHERE kind='call' "
                    "AND from_function IS NOT NULL AND to_function IS NOT NULL"
                ),
            )
        return self._graph


# --- read-only SHARC store inventory ------------------------------------------


def _sha256_file(path: str) -> str:
    digest = hashlib.sha256()
    try:
        with open(path, "rb") as source:
            for chunk in iter(lambda: source.read(1024 * 1024), b""):
                digest.update(chunk)
    except OSError as exc:
        raise FwError(f"cannot read provenance input {path}: {exc}") from exc
    return digest.hexdigest()


def cmd_stores(
    img: Sharc, roots: list[str], expected_sha: str | None, blob: str | None
) -> dict[str, Any]:
    """One read-only transaction; file identity is not a SQLite snapshot hash."""
    img.db.execute("BEGIN")
    try:
        return _store_inventory(img, roots, expected_sha, blob)
    finally:
        img.db.rollback()


def _store_inventory(
    img: Sharc, roots: list[str], expected_sha: str | None, blob: str | None
) -> dict[str, Any]:
    """Inventory recorded operands, not runtime addresses, extents or ownership."""
    import networkx as nx

    from tools.sharc_dev.sharcdb.schema import DB_VERSION

    required = {
        "meta": "image key value",
        "functions": "image entry_sw end_sw name",
        "edges": "image from_sw to_sw kind delayed cond from_function to_function note",
        "insn": "image sw form fields confidence function_sw raw",
        "mem_access": "image sw space direction base_reg modifier u form width abs_address",
        "ptr": "image sw base_reg base_value address direction width",
    }
    for table, columns in required.items():
        found = {r[1] for r in img.db.execute(f"PRAGMA table_info({table})")}
        if not set(columns.split()) <= found:
            raise FwError(
                f"stores: missing/stale schema for {table}; no rebuild attempted"
            )
    meta = dict(
        img.db.execute(
            "SELECT key,value FROM meta WHERE image=? ORDER BY key", (img.image,)
        )
    )
    if any(
        not meta.get(k)
        for k in ("image", "cpu", "db_version", "image_sha256", "blob_path")
    ):
        raise FwError(
            "stores: missing per-image provenance metadata; no rebuild attempted"
        )
    if (
        meta["image"] != img.image
        or meta["cpu"] != "sharc"
        or meta["db_version"] != str(DB_VERSION)
    ):
        raise FwError(
            "stores: stale/mismatched image, CPU or schema version; no rebuild attempted"
        )
    sha = meta["image_sha256"]
    if not re.fullmatch(r"[0-9a-f]{64}", sha):
        raise FwError("stores: malformed image SHA-256 metadata")
    if expected_sha is not None and expected_sha.lower() != sha:
        raise FwError("stores: expected image SHA-256 differs from database metadata")
    selected_blob = os.path.abspath(blob) if blob else None
    if selected_blob is not None and _sha256_file(selected_blob) != sha:
        raise FwError(
            "stores: image bytes differ from database SHA-256; no rebuild attempted"
        )

    def rows(table: str) -> list[dict[str, Any]]:
        cursor = img.db.execute(
            f"SELECT rowid AS evidence_rowid,* FROM {table} WHERE image=? ORDER BY rowid",
            (img.image,),
        )
        names = [c[0] for c in cursor.description]
        return [dict(zip(names, r, strict=True)) for r in cursor]

    functions = sorted(rows("functions"), key=lambda r: r["entry_sw"])
    entries = {f["entry_sw"] for f in functions}
    owner_cache: dict[int, list[int]] = {}

    def owners(pc: int | None) -> list[int]:
        if pc is None:
            return []
        if pc not in owner_cache:
            owner_cache[pc] = [
                f["entry_sw"] for f in functions if f["entry_sw"] <= pc < f["end_sw"]
            ]
        return owner_cache[pc]

    root_rows = []
    for token in roots:
        pc = parse_addr(token)
        if pc is None:
            raise FwError(f"stores: root must be an explicit PC, not {token!r}")
        containing = owners(pc)
        if len(containing) != 1:
            raise FwError(
                f"stores: root 0x{pc:x} has {len(containing)} containing functions"
            )
        root_rows.append(
            {
                "requested_pc": pc,
                "containing_function": containing[0],
                "is_function_entry": pc == containing[0],
            }
        )
    root_rows.sort(key=lambda r: (r["requested_pc"], r["containing_function"]))
    edges = rows("edges")
    graph = nx.DiGraph()
    graph.add_nodes_from(entries)
    for edge in edges:
        edge["source_containing_functions"] = owners(edge["from_sw"])
        edge["target_containing_functions"] = owners(edge["to_sw"])
        edge["target_is_recorded_entry"] = (
            edge["to_sw"] in entries if edge["to_sw"] is not None else None
        )
        eligible = (
            edge["kind"] == "call"
            and edge["from_function"] in entries
            and edge["to_function"] in entries
            and edge["source_containing_functions"] == [edge["from_function"]]
            and edge["target_containing_functions"] == [edge["to_function"]]
        )
        edge["followed_by_policy"] = eligible
        if eligible:
            graph.add_edge(edge["from_function"], edge["to_function"])
    reachable: set[int] = set()
    for root in root_rows:
        entry = root["containing_function"]
        reachable.add(entry)
        reachable.update(nx.descendants(graph, entry))
    selected_edges = []
    for edge in edges:
        if (
            not ({edge["from_function"]} | set(edge["source_containing_functions"]))
            & reachable
        ):
            continue
        if edge["followed_by_policy"]:
            reason = None
        elif edge["source_containing_functions"] != [edge["from_function"]]:
            reason = "source_function_metadata_missing_or_mismatched"
        elif edge["kind"] == "call":
            reason = "unresolved_or_mismatched_call_target"
        elif edge["kind"] == "indirect":
            reason = "unresolved_indirect_target"
        elif edge["kind"] == "return":
            reason = "return_not_followed"
        elif edge["to_function"] not in (None, edge["from_function"]):
            reason = "non_call_cross_function_edge"
        else:
            reason = "non_call_edge_not_followed"
        edge["not_followed_reason"] = reason
        selected_edges.append(edge)
    selected_edges.sort(key=lambda r: (r["from_sw"], r["evidence_rowid"]))

    instructions = {r["sw"]: r for r in rows("insn")}
    pointers: dict[int, list[dict[str, Any]]] = {}
    for pointer in rows("ptr"):
        pointers.setdefault(pointer["sw"], []).append(pointer)
    all_stores = [r for r in rows("mem_access") if r["direction"] == "store"]
    stores: list[dict[str, Any]] = []
    for mem in sorted(all_stores, key=lambda r: (r["sw"], r["evidence_rowid"])):
        insn = instructions.get(mem["sw"])
        scope = set(owners(mem["sw"]))
        if insn and insn["function_sw"] in entries:
            scope.add(insn["function_sw"])
        if not scope & reachable:
            continue
        site_ptr = pointers.get(mem["sw"], [])
        compatible = [
            p
            for p in site_ptr
            if p["direction"] == "store"
            and p["base_reg"] == mem["base_reg"]
            and p["width"] == mem["width"]
        ]
        competing = [
            m
            for m in all_stores
            if m["sw"] == mem["sw"]
            and m["base_reg"] == mem["base_reg"]
            and m["width"] == mem["width"]
        ]
        unknown = []
        if len(scope) != 1 or (insn and insn["function_sw"] not in owners(mem["sw"])):
            unknown.append("function_ownership_metadata_ambiguous")
        if not insn:
            unknown.append("instruction_evidence_missing")
        matched = None
        if mem["abs_address"] is not None:
            resolution = "database_recorded_absolute_operand"
        elif len(compatible) > 1 or (compatible and len(competing) > 1):
            resolution = "unknown"
            unknown.append("ambiguous_pointer_to_operand_matching")
        elif len(compatible) == 1:
            matched = compatible[0]
            resolution = (
                "static_first_arrival_estimate"
                if matched["address"] is not None
                else "base_only_estimate"
            )
            if matched["address"] is None:
                unknown.append("effective_address_unresolved")
        else:
            resolution = "unknown"
            unknown.append(
                "no_compatible_pointer_evidence"
                if not site_ptr
                else "pointer_base_or_width_mismatch_possible_bank_metadata_gap"
            )
        labels = []
        if mem["base_reg"] in ("I6", "I7"):
            labels.append("stack_register_candidate")
        if mem["space"] == "PM":
            labels.append("program_memory_operand")
        if mem["form"] == "dual":
            labels.append("dual_memory_operand")
        stores.append(
            {
                "operand_id": len(stores),
                "function_candidates": sorted(scope),
                "mem_access": mem,
                "instruction": insn,
                "ptr_site_evidence": site_ptr,
                "compatible_ptr_rowids": [p["evidence_rowid"] for p in compatible],
                "matched_ptr_rowid": matched["evidence_rowid"] if matched else None,
                "address_estimate": matched["address"] if matched else None,
                "resolution": resolution,
                "confidence": {
                    "instruction": insn["confidence"] if insn else None,
                    "address": "limited_static_estimate_not_bounds"
                    if matched
                    else resolution,
                },
                "unknown_reasons": unknown,
                "candidate_labels": labels,
            }
        )
    schema = list(
        img.db.execute(
            "SELECT type,name,sql FROM sqlite_master WHERE sql IS NOT NULL ORDER BY type,name"
        )
    )
    return {
        "report_version": 1,
        "provenance": {
            "image": img.image,
            "meta": meta,
            "database_path": os.path.abspath(img.path),
            "database_file_sha256": _sha256_file(img.path),
            "database_file_hash_scope": "main file identity only, not transaction/WAL snapshot or concurrent replacement proof",
            "journal_mode": img.db.execute("PRAGMA journal_mode").fetchone()[0],
            "schema_sha256": hashlib.sha256(
                json.dumps(schema, separators=(",", ":")).encode()
            ).hexdigest(),
            "selected_blob_path": selected_blob,
            "image_verification": "bytes_verified"
            if selected_blob
            else "metadata_only_not_bytes_verified",
            "verified_image_sha256": sha if selected_blob else None,
            "expected_image_sha256": expected_sha,
        },
        "policy": {
            "edges": "recorded CALL only with unambiguous containing target; whole containing functions, including interior targets",
            "roots": "requested PC is retained; traversal starts at its containing function",
            "pointer_evidence": "limited constant propagation; loop first arrival, not runtime bounds; stack labels are candidates, not exclusions",
            "footprint": "raw width tokens only; no byte extent, SIMD or alias inference",
            "coverage": "recorded functions/operands only; unresolved/non-CALL edges prevent full reachability or ownership claims",
        },
        "roots": root_rows,
        "reachable_functions": [f for f in functions if f["entry_sw"] in reachable],
        "edges": selected_edges,
        "stores": stores,
        "counts": {
            "database_store_operands": len(all_stores),
            "reachable_functions": len(reachable),
            "reachable_store_operands": len(stores),
            "reachable_store_sites": len({r["mem_access"]["sw"] for r in stores}),
            "other_store_operands": len(all_stores) - len(stores),
            "edge_rows": len(selected_edges),
            "followed_call_edge_rows": sum(
                e["followed_by_policy"] for e in selected_edges
            ),
            "unresolved_call_edge_rows": sum(
                e["kind"] == "call" and not e["followed_by_policy"]
                for e in selected_edges
            ),
            "unresolved_indirect_edge_rows": sum(
                e["kind"] == "indirect" for e in selected_edges
            ),
            "not_followed_edge_rows": sum(
                not e["followed_by_policy"] for e in selected_edges
            ),
            "stack_candidate_operands": sum(
                "stack_register_candidate" in r["candidate_labels"] for r in stores
            ),
            "pm_operands": sum(r["mem_access"]["space"] == "PM" for r in stores),
            "dual_operands": sum(
                "dual_memory_operand" in r["candidate_labels"] for r in stores
            ),
        },
    }


# --- command implementations ---------------------------------------------------

Image = CF | Sharc


def want_sharc(token: str | None, cpu: str | None) -> bool:
    if cpu:
        return cpu == "sharc"
    if token:
        a = parse_addr(token)
        if a is not None:
            return a < CF_BASE
        if re.match(r"(?i)fun_[0-9a-f]{6}$", token):
            return True
    return False


class Session:
    """Opened databases for one process (batch shares it)."""

    def __init__(self) -> None:
        self.imgs: dict[tuple[str, str], Image] = {}

    def get(
        self, image: str | None, token: str | None = None, cpu: str | None = None
    ) -> Image:
        image = image or os.environ.get("FW_IMAGE")
        if not image:
            raise FwError("no image: pass -i dt2-1.17 or set FW_IMAGE")
        kind = "sharc" if want_sharc(token, cpu) else "cf"
        key = (short_image(image), kind)
        if key not in self.imgs:
            self.imgs[key] = Sharc(image) if kind == "sharc" else CF(image)
        return self.imgs[key]


def cmd_fn(img: Image, token: str, lines_n: int = 12) -> list[str]:
    if isinstance(img, Sharc):
        return sharc_fn(img, token)
    entry = img.resolve(token)
    f = img.func_row(entry)
    assert f is not None
    out = ["%s  size=%d  end=%08x" % (img.label(entry), f[4], f[1])]
    if f[5]:
        out.append("sig: " + clip(f[5]))
    slots = img.vtable_slots(entry)
    if slots:
        out.append("vtable: " + ", ".join("%s#%d" % s for s in slots[:6]))
    callers = img.callers(entry)
    cfuncs = sorted({c[0] for c in callers if c[0] is not None})
    callees = img.callees(entry)
    out.append("callers: %d sites in %d functions" % (len(callers), len(cfuncs)))
    for c in cfuncs[:5]:
        out.append("  <- " + img.label(c))
    if len(cfuncs) > 5:
        out.append("  <- ... %d more" % (len(cfuncs) - 5))
    out.append("callees: %d" % len(callees))
    for c, _k in callees[:6]:
        out.append("  -> " + img.label(c))
    if len(callees) > 6:
        out.append("  -> ... %d more" % (len(callees) - 6))
    strs = img.strings_of(entry)
    if strs:
        out.append("strings:")
        for a, t in strs:
            out.append("  %08x %s" % (a, repr(clip(t, 70))))
    text = img.read_dump(f[6])
    if text:
        body = _decomp_body(text)
        out.append("decomp (%d of %d lines):" % (min(lines_n, len(body)), len(body)))
        out.extend("  " + ln[:110] for ln in body[:lines_n])
    else:
        out.append("decomp: none (%s)" % ("failed" if f[6] is None else "missing file"))
    return out


_DECL = re.compile(r"^\s+[A-Za-z_][\w ]*[ *]\**\w+(\[\w+\])?;$")
_STMT = ("return", "goto", "break", "continue", "else", "case", "default")


def _decomp_body(text: str) -> list[str]:
    """Decompiled C without comments, blank lines and the local declarations."""
    out: list[str] = []
    header = True
    for ln in text.splitlines():
        if not ln.strip() or ln.lstrip().startswith(("/*", "*", "//")):
            continue
        if header:
            st = ln.strip()
            if st == "{" or (_DECL.match(ln) and not st.startswith(_STMT)):
                continue
            if ln[:1] not in (" ", "\t"):
                out.append(ln)
                continue
            header = False
        out.append(ln)
    return out


def sharc_fn(img: Sharc, token: str) -> list[str]:
    entry = img.resolve(token)
    f = img.func_row(entry)
    assert f is not None
    out = [
        "%s  insns=%d end=%06x block=%s %s%s"
        % (
            img.label(entry),
            f[2],
            f[1],
            f[3],
            "leaf" if f[7] else "non-leaf",
            " " + f[6] if f[6] else "",
        )
    ]
    n = img.note(entry)
    if n:
        out.append("note: %s: %s" % (n[0], clip(n[1] or "", 140)))
    callers = img.db.execute(
        "SELECT DISTINCT from_function FROM edges WHERE to_function=? AND from_function IS NOT NULL "
        "AND from_function<>? AND kind IN ('call','jump','indirect')",
        (entry, entry),
    ).fetchall()
    callees = img.db.execute(
        "SELECT DISTINCT to_function FROM edges WHERE from_function=? AND kind='call' AND to_function IS NOT NULL",
        (entry,),
    ).fetchall()
    out.append("callers: %d" % len(callers))
    out.extend("  <- " + img.label(c[0]) for c in callers[:5])
    out.append("callees: %d" % len(callees))
    out.extend("  -> " + img.label(c[0]) for c in callees[:6])
    lit = img.db.execute(
        "SELECT sw, value, form FROM literals WHERE sw>=? AND sw<? ORDER BY sw LIMIT 6",
        (entry, f[1]),
    ).fetchall()
    if lit:
        out.append("literals: " + ", ".join("%x:%#x" % (a, b) for a, b, _ in lit))
    out.extend(sharc_disasm(img, entry, 8)[:9])
    return out


def sharc_disasm(img: Sharc, entry: int, n: int | None = None) -> list[str]:
    f = img.func_row(entry)
    if f is None:
        raise FwError("no SHARC function at 0x%x" % entry)
    rows = img.db.execute(
        "SELECT sw, mnemonic FROM insn WHERE aligned=1 AND sw>=? AND sw<? ORDER BY sw",
        (entry, f[1]),
    ).fetchall()
    out = ["  %06x  %s" % (sw, m) for sw, m in rows]
    return out[:n] if n else out


def cmd_decomp(img: Image, token: str) -> list[str]:
    if isinstance(img, Sharc):
        raise FwError(
            "no decompiler for SHARC; use: disasm %s (or sharc.py card)" % token
        )
    entry = img.resolve(token)
    f = img.func_row(entry)
    assert f is not None
    text = img.read_dump(f[6])
    if text is None:
        raise FwError(
            "no decompiled C for %s (decompile failed); use disasm" % img.label(entry)
        )
    return [img.label(entry)] + text.splitlines()


def cmd_disasm(img: Image, token: str) -> list[str]:
    if isinstance(img, Sharc):
        entry = img.resolve(token)
        return [img.label(entry)] + sharc_disasm(img, entry)
    entry = img.resolve(token)
    f = img.func_row(entry)
    assert f is not None
    text = img.read_dump(f[7])
    if text is None:
        raise FwError("no disassembly file for %s" % img.label(entry))
    return [img.label(entry)] + [
        ln for ln in text.splitlines() if not ln.startswith(";")
    ][1:]


def cmd_callers(img: Image, token: str) -> list[str]:
    entry = img.resolve(token)
    if isinstance(img, Sharc):
        rows = img.db.execute(
            "SELECT from_function, from_sw, kind FROM edges WHERE (to_sw=? OR to_function=?) "
            "AND kind IN ('call','jump','indirect') AND IFNULL(from_function,0)<>? ORDER BY 1, 2",
            (entry, entry, entry),
        ).fetchall()
    else:
        rows = img.callers(entry)
    out = ["%s  %d call sites" % (img.label(entry), len(rows))]
    last = None
    for fn, site, kind in rows:
        tag = img.label(fn) if fn is not None else "(no function)"
        fmt = "%06x" if isinstance(img, Sharc) else "%08x"
        out.append(
            "  %s %s @%s" % (kind[:4], tag if tag != last else '  "', fmt % site)
        )
        last = tag
    return out


def cmd_callees(img: Image, token: str) -> list[str]:
    entry = img.resolve(token)
    if isinstance(img, Sharc):
        rows = [
            (r[0], "call")
            for r in img.db.execute(
                "SELECT DISTINCT to_function FROM edges WHERE from_function=? AND kind='call' "
                "AND to_function IS NOT NULL ORDER BY 1",
                (entry,),
            )
        ]
    else:
        rows = img.callees(entry)
    out = ["%s  %d callees" % (img.label(entry), len(rows))]
    out.extend("  %s %s" % (k[:4], img.label(c)) for c, k in rows)
    return out


def cmd_xref(img: Image, token: str) -> list[str]:
    addr = parse_addr(token)
    if addr is None:
        addr = img.resolve(token)
    if isinstance(img, Sharc):
        out = ["xref %06x" % addr]
        for fn, site, kind in img.db.execute(
            "SELECT from_function, from_sw, kind FROM edges WHERE to_sw=? AND kind IN ('call','jump','indirect') "
            "ORDER BY from_sw",
            (addr,),
        ):
            out.append(
                "  code %-9s %06x in %s" % (kind, site, img.label(fn) if fn else "?")
            )
        for sw, direction, base in img.db.execute(
            "SELECT sw, direction, base_reg FROM ptr WHERE address=? ORDER BY sw LIMIT 200",
            (addr,),
        ):
            f = img.func_row(sw)
            out.append(
                "  data %-9s %06x in %s (%s)"
                % (direction, sw, img.label(f[0]) if f else "?", base)
            )
        for sw, form in img.db.execute(
            "SELECT sw, form FROM literals WHERE value=? ORDER BY sw LIMIT 100", (addr,)
        ):
            f = img.func_row(sw)
            out.append(
                "  literal %s %06x in %s" % (form, sw, img.label(f[0]) if f else "?")
            )
        if len(out) == 1:
            out.append("  no references recorded (indirect calls are not edges)")
        return out
    out = ["xref %08x  %s" % (addr, img.label_any(addr))]
    s = img.db.execute("SELECT text FROM strings WHERE addr=?", (addr,)).fetchone()
    if s:
        out.append("  string %r" % clip(s[0], 80))
    for site, fn, kind, _label in img.db.execute(
        "SELECT site, func, kind, label FROM datarefs WHERE to_addr=? ORDER BY site",
        (addr,),
    ):
        out.append(
            "  data %-10s %08x in %s"
            % (kind, site, img.label(fn) if fn else "(outside functions)")
        )
    for site, fn, kind in img.db.execute(
        "SELECT site, from_func, kind FROM edges WHERE to_addr=? ORDER BY site", (addr,)
    ):
        out.append(
            "  code %-10s %08x in %s"
            % (kind, site, img.label(fn) if fn else "(outside functions)")
        )
    for cls, slot, vt in img.db.execute(
        "SELECT class, slot, addr FROM vtables WHERE target=? ORDER BY class, slot",
        (addr,),
    ):
        out.append("  vtable %s slot %d (vtable %08x)" % (cls, slot, vt))
    if len(out) == 1:
        out.append(
            "  no references recorded (Ghidra misses code outside functions; try uv run python -m tools.coldfire.refscan)"
        )
    return out


def cmd_str(img: Image, term: str) -> list[str]:
    if isinstance(img, Sharc):
        raise FwError(
            "strings are a ColdFire concept; use -i with a ColdFire address or search"
        )
    rows = _find_strings(img, term, 200)
    out = ["%d strings match %r" % (len(rows), term)]
    for addr, text in rows:
        users = img.db.execute(
            "SELECT DISTINCT func FROM datarefs WHERE to_addr=? AND func IS NOT NULL ORDER BY func",
            (addr,),
        ).fetchall()
        out.append("%08x %s" % (addr, repr(clip(text, 70))))
        for (u,) in users[:3]:
            out.append("    used by " + img.label(u))
        if len(users) > 3:
            out.append("    used by ... %d more" % (len(users) - 3))
        if not users:
            out.append("    no direct users recorded")
    return out


def _find_strings(img: CF, term: str, limit: int) -> list[tuple[int, str]]:
    if len(term) >= 3:
        fts = img.fts()
        ids = fts.execute(
            "SELECT rowid FROM fts_strings WHERE fts_strings MATCH ? ORDER BY rank LIMIT ?",
            (tri_query(term), limit),
        ).fetchall()
        if ids:
            marks = ",".join(str(i[0]) for i in ids)
            addrs = fts.execute(
                "SELECT id, addr FROM string_rows WHERE id IN (%s)" % marks
            ).fetchall()
            out = []
            for _id, a in sorted(addrs, key=lambda r: r[1]):
                row = img.db.execute(
                    "SELECT text FROM strings WHERE addr=? LIMIT 1", (a,)
                ).fetchone()
                if row:
                    out.append((a, row[0]))
            return out
        return []
    return img.db.execute(
        "SELECT addr, text FROM strings WHERE text LIKE ? ORDER BY addr LIMIT ?",
        ("%" + term + "%", limit),
    ).fetchall()


def _bases(img: CF, bases: str | None) -> str:
    if not bases:
        return "-"
    names = []
    for tok in re.findall(r"\d+", bases):
        r = img.db.execute(
            "SELECT name FROM typeinfos WHERE addr=?", (int(tok),)
        ).fetchone()
        names.append(r[0] if r else tok)
    return ", ".join(names) or "-"


def cmd_class(img: Image, name: str) -> list[str]:
    if isinstance(img, Sharc):
        raise FwError("RTTI classes exist on the ColdFire side only")
    rows = img.db.execute(
        "SELECT name, kind, bases, addr FROM typeinfos WHERE name=? ORDER BY addr",
        (name,),
    ).fetchall()
    if not rows:
        rows = img.db.execute(
            "SELECT name, kind, bases, addr FROM typeinfos WHERE name LIKE ? AND kind NOT IN ('pointer','fundamental','function') "
            "AND name NOT LIKE 'std::_Sp%' ORDER BY name",
            ("%" + name + "%",),
        ).fetchall()
        if not rows:
            raise FwError("no class like %r" % name)
        if len(rows) > 1:
            return ["%d classes match %r (use the exact name):" % (len(rows), name)] + [
                "  %s  (%s)" % (r[0], r[1]) for r in rows
            ]
    cname, kind, bases, taddr = rows[0]
    out = ["%s  %s  typeinfo=%08x  bases=%s" % (cname, kind, taddr, _bases(img, bases))]
    derived = img.db.execute(
        "SELECT name FROM typeinfos WHERE bases LIKE ? ORDER BY name",
        ("%" + cname + "%",),
    ).fetchall()
    if derived:
        out.append(
            "derived: "
            + ", ".join(d[0] for d in derived[:12])
            + (" ..." if len(derived) > 12 else "")
        )
    vts = img.db.execute(
        "SELECT addr, offset_to_top, slot, target, owner FROM vtables WHERE class=? ORDER BY addr, slot",
        (cname,),
    ).fetchall()
    if not vts:
        out.append("no vtable")
        return out
    cur = None
    for addr, ott, slot, target, owner in vts:
        if addr != cur:
            out.append("vtable %08x (offset_to_top %d)" % (addr, ott))
            cur = addr
        n = img.names().get(target)
        out.append(
            "  #%-3d %s%s%s"
            % (
                slot,
                img.label(target) if target else "0",
                "" if owner == cname else "  <" + str(owner) + ">",
                "",
            )
        )
        del n
    return out


def cmd_path(img: Image, a: str, b: str) -> list[str]:
    ea, eb = img.resolve(a), img.resolve(b)
    g = img.graph()
    p = g.shortest(ea, eb)
    out = ["path %s -> %s (graph %s)" % (img.label(ea), img.label(eb), img.graph_state)]
    if p is None:
        out.append("  no call path (indirect calls through vtables are not edges)")
        return out
    out.append("  %d hops" % (len(p) - 1))
    out.extend(
        "  %s%s" % ("" if i == 0 else "-> ", img.label(n)) for i, n in enumerate(p)
    )
    return out


def cmd_port(sess: Session, name: str, src: str, dst: str) -> list[str]:
    a = sess.get(src, name)
    b = sess.get(dst, name)
    if isinstance(a, Sharc) and isinstance(b, Sharc):
        entry = a.resolve(name)
        h = a.db.execute(
            "SELECT reloc_hash FROM func_hash WHERE entry_sw=?", (entry,)
        ).fetchone()
        out = ["%s  (%s) -> %s" % (a.label(entry), short_image(src), short_image(dst))]
        if h:
            for (e,) in b.db.execute(
                "SELECT entry_sw FROM func_hash WHERE reloc_hash=?", (h[0],)
            ):
                out.append("  match (reloc hash) " + b.label(e))
        if len(out) == 1:
            out.append("  no match")
        return out
    assert isinstance(a, CF) and isinstance(b, CF)
    entry = a.resolve(name)
    out = ["%s  (%s) -> %s" % (a.label(entry), a.image, b.image)]
    rows = b.db.execute(
        "SELECT dst_entry, method FROM matches WHERE src_image=? AND src_entry=?",
        (a.db_name, entry),
    ).fetchall()
    for dst_entry, method in rows:
        out.append("  %s  [%s]" % (b.label(dst_entry), method))
    if not rows:
        rev = b.db.execute(
            "SELECT src_entry, method FROM matches WHERE src_image=? AND dst_entry=?",
            (a.db_name, entry),
        ).fetchall()
        for e, method in rev:
            out.append("  (reverse) %s [%s]" % (b.label(e), method))
    if len(out) == 1:
        out.append(
            "  no recorded match (uv run python -m tools.coldfire.cfmatch run %s %s writes them)"
            % (a.db_name, b.db_name)
        )
    return out


def cmd_sql(img: Image, query: str) -> list[str]:
    if not re.match(r"(?is)\s*(select|with|pragma|explain)\b", query):
        raise FwError("sql is read-only: SELECT/WITH only")
    cur = img.db.execute(query)
    cols = [d[0] for d in cur.description or []]
    out = ["\t".join(cols)]
    for row in cur:
        out.append("\t".join(_cell(v) for v in row))
    return out


def _cell(v: Any) -> str:
    if v is None:
        return "-"
    if isinstance(v, int) and v >= 0x100000:
        return "0x%x" % v
    return clip(str(v), 120)


def cmd_search(
    sess: Session, image: str | None, term: str, where: str, cap: int, cpu: str | None
) -> list[str]:
    img = sess.get(image, None, cpu)
    parts = set(where.split(","))
    out: list[str] = []
    per = max(6, cap // 4) if cap else 50
    if isinstance(img, Sharc):
        like = "%" + term + "%"
        rows = img.db.execute(
            "SELECT entry_sw FROM functions WHERE name LIKE ? OR label LIKE ? ORDER BY entry_sw LIMIT ?",
            (like, like, per * 2),
        ).fetchall()
        out.append("functions: %d" % len(rows))
        out.extend("  " + img.label(r[0]) for r in rows)
        if img.notes is not None:
            for fn, role, summ in img.notes.execute(
                "SELECT function_sw, role, summary FROM notes WHERE summary LIKE ? OR role LIKE ? LIMIT ?",
                (like, like, per),
            ):
                out.append("  note %06x %s: %s" % (fn, role, clip(summ or "", 90)))
        return out
    assert isinstance(img, CF)
    fts = img.fts()
    if "names" in parts:
        if len(term) >= 3:
            rows = fts.execute(
                "SELECT rowid, name, cf_name, klass FROM fts_names WHERE fts_names MATCH ? ORDER BY rank LIMIT ?",
                (tri_query(term), per * 2),
            ).fetchall()
        else:
            rows = []
        out.append("names: %d" % len(rows))
        for rowid, name, cfn, _k in rows:
            out.append("  %08x %s%s" % (rowid, name, " [%s]" % cfn if cfn else ""))
    if "strings" in parts:
        rows2 = _find_strings(img, term, per)
        out.append("strings: %d" % len(rows2))
        for a, t in rows2:
            users = img.db.execute(
                "SELECT DISTINCT func FROM datarefs WHERE to_addr=? AND func IS NOT NULL LIMIT 2",
                (a,),
            ).fetchall()
            out.append(
                "  %08x %s%s"
                % (
                    a,
                    repr(clip(t, 60)),
                    "  <- " + ", ".join("%08x" % u[0] for u in users) if users else "",
                )
            )
    if "decomp" in parts:
        hits = fts.execute(
            "SELECT rowid FROM fts_decomp WHERE fts_decomp MATCH ? ORDER BY rank LIMIT ?",
            (fts_query(term, prefix=True), per),
        ).fetchall()
        out.append(
            "decomp: %d functions (best %d)" % (_count_decomp(fts, term), len(hits))
        )
        words = [w.lower() for w in term.split()]
        for (rowid,) in hits:
            f = img.func_row(rowid)
            line = ""
            if f:
                text = img.read_dump(f[6]) or ""
                for ln in text.splitlines():
                    if any(w in ln.lower() for w in words):
                        line = ln
                        break
            out.append("  %s : %s" % (img.label(rowid), clip(line, 70)))
    if "atlas" in parts and os.path.exists(
        os.path.join(ATLAS_DIR, img.image + ".sqlite")
    ):
        out.append("atlas:")
        out.extend("  " + ln for ln in cmd_atlas(img.image, term, 8))
    return out


def _count_decomp(fts: sqlite3.Connection, term: str) -> int:
    return fts.execute(
        "SELECT count(*) FROM fts_decomp WHERE fts_decomp MATCH ?",
        (fts_query(term, prefix=True),),
    ).fetchone()[0]


def cmd_atlas(image: str, term: str, per: int = 15) -> list[str]:
    out = _atlas_one(image, term, per)
    words = term.split()
    if len(words) > 1 and out and out[0].startswith("atlas has nothing"):
        out = []
        for w in words:
            out.append("-- %s" % w)
            out.extend(_atlas_one(image, w, max(4, per // len(words))))
    return out


def _atlas_one(image: str, term: str, per: int) -> list[str]:
    path = os.path.join(ATLAS_DIR, short_image(image) + ".sqlite")
    if not os.path.exists(path):
        raise FwError(
            "no atlas for %s: run uv run python -m tools.coldfire.atlas build %s"
            % (short_image(image), short_image(image))
        )
    con = open_ro(path)
    like = "%" + term + "%"
    out: list[str] = []
    g = con.execute(
        "SELECT term, kind, ref, detail, confidence FROM glossary WHERE term LIKE ? ORDER BY term, instr('function class struct descriptor param string manual', kind), score DESC",
        (like,),
    ).fetchall()
    if g:
        out.append("glossary %r: %d rows" % (term, len(g)))
        kinds: dict[str, int] = {}
        for _t, kind, ref, detail, conf in g:
            kinds[kind] = kinds.get(kind, 0) + 1
            if kinds[kind] <= per // 3 + 2:
                out.append(
                    "  %-9s %s  %s (%s)" % (kind, ref, clip(detail or "", 70), conf)
                )
    for table, sql, fmt in (
        (
            "descriptors",
            "SELECT idx, role, name1, name2, addr FROM descriptors WHERE name1 LIKE ?1 OR name2 LIKE ?1 OR role LIKE ?1 LIMIT ?2",
            "  descriptor #%s %s %r/%r @%#x",
        ),
        (
            "params",
            "SELECT code, long_name, short_name, owner_name, addr FROM params WHERE long_name LIKE ?1 OR short_name LIKE ?1 OR owner_name LIKE ?1 LIMIT ?2",
            "  param %s %r %r owner=%s @%#x",
        ),
        (
            "classes",
            "SELECT name, role, vtable, nslots FROM classes WHERE name LIKE ?1 OR role LIKE ?1 ORDER BY name LIMIT ?2",
            "  class %s role=%s vtable=%#x slots=%s",
        ),
        (
            "ui_strings",
            "SELECT text, view_class, user_func, confidence FROM ui_strings WHERE text LIKE ?1 OR view_class LIKE ?1 LIMIT ?2",
            "  ui %r in %s fn=%#x (%s)",
        ),
    ):
        try:
            rows = con.execute(sql, (like, per)).fetchall()
        except sqlite3.Error:
            continue
        if rows:
            out.append("%s: %d shown" % (table, len(rows)))
            for r in rows:
                try:
                    out.append(fmt % tuple(r))
                except (TypeError, ValueError):
                    out.append("  " + " ".join(str(x) for x in r))
    if not out:
        out.append("atlas has nothing for %r" % term)
    return out


def cmd_catalog(
    image: str,
    term: str,
    tag: str | None,
    top: int,
    grade: bool,
    group: int | None = None,
    groups: bool = False,
    cpu: str = "cf",
) -> list[str]:
    """Rows of the atlas `catalog` table (candidate anchor functions), best score first.

    With cpu="sharc" the rows come from out/atlas/<image>-dsp.sqlite (tools/sharc_dev/sharc_catalog.py).
    """
    short = short_image(image)
    if cpu == "sharc":
        path = os.path.join(ATLAS_DIR, short + "-dsp.sqlite")
        build = "uv run python -m tools.sharc_dev.sharc_catalog %s OTHER" % short
        if not os.path.exists(path):
            raise FwError(
                "no DSP catalog for %s: run %s (e.g. dt2-1.17 dn2-1.12)"
                % (short, build)
            )
    else:
        path = os.path.join(ATLAS_DIR, short + ".sqlite")
        build = "uv run python -m tools.coldfire.atlas catalog %s OTHER" % short
        if not os.path.exists(path):
            raise FwError(
                "no atlas for %s: run uv run python -m tools.coldfire.atlas build %s"
                % (short, short)
            )
    con = open_ro(path)
    if not _has(con, "catalog"):
        raise FwError(
            "no catalog for %s: run %s (e.g. dt2-1.17 dn2-1.12)" % (short, build)
        )
    if (groups or group is not None) and not _has(con, "catalog_groups"):
        raise FwError(
            "no catalog groups for %s: re-run %s (e.g. dt2-1.17 dn2-1.12)"
            % (short, build)
        )
    if groups:
        grows = con.execute(
            "SELECT id, size, anchored, grade, top_classes, top_strings, top_tags, best "
            "FROM catalog_groups ORDER BY size DESC, id LIMIT %d" % top
        ).fetchall()
        out = ["catalog groups %s: %d shown" % (short, len(grows))]
        for gid, size, anc, gr, tcl, tst, ttg, best in grows:
            out.append(
                "grp=%d size=%d anchored=%d grade=%d best=%08x classes=%s tags=%s%s"
                % (
                    gid,
                    size,
                    anc,
                    gr,
                    best,
                    tcl or "-",
                    ttg or "-",
                    " strings=" + repr(tst) if tst else "",
                )
            )
        return out
    where = ["1"]
    args: list[Any] = []
    if group is not None:
        where.append('"group"=?')
        args.append(group)
    if term:
        cls_col, hint_col = ("role", "hints") if cpu == "sharc" else ("class", "hint")
        where.append(
            "(name LIKE ?%d OR %s LIKE ?%d OR %s LIKE ?%d OR tags LIKE ?%d)"
            % (
                len(args) + 1,
                cls_col,
                len(args) + 1,
                hint_col,
                len(args) + 1,
                len(args) + 1,
            )
        )
        args.append("%" + term + "%")
    if tag:
        where.append("(',' || tags) LIKE ?%d" % (len(args) + 1))
        args.append("%," + tag + "%")
    if grade:
        where.append("anchor_grade=1")
    sql = "FROM catalog WHERE " + " AND ".join(where)
    total = con.execute("SELECT count(*) " + sql, args).fetchone()[0]
    if cpu == "sharc":
        drows = con.execute(
            "SELECT addr, score, anchor_grade, name, role, tags, callers, hints, pair_addr, "
            'pair_method, rule_kind, rule_ordinal, anchors, "group" '
            + sql
            + " ORDER BY score DESC, addr LIMIT %d" % top,
            args,
        ).fetchall()
        out = [
            "catalog %s (sharc): %d rows match, %d shown" % (short, total, len(drows))
        ]
        for a, sc, g, name, role, tags, nc, hints, pa, pm, rk, ro, anch, grp in drows:
            rule = rk if ro is None else "%s#%d" % (rk, ro)
            out.append(
                "%06x %5.2f %s %s (%s) grp=%d  %s  callers=%d rule=%s pair=%s%s%s"
                % (
                    a,
                    sc,
                    "G" if g else "-",
                    name,
                    role,
                    grp,
                    tags,
                    nc,
                    rule,
                    "%06x/%s" % (pa, pm) if pa is not None else "-",
                    " " + repr(hints) if hints else "",
                    " anchored=" + anch if anch else "",
                )
            )
        return out
    rows = con.execute(
        "SELECT addr, score, anchor_grade, name, class, tags, ui_callers, hint, pair_addr, "
        'pair_method, rule_kind, rule_ordinal, anchors, work_name, "group", caller_classes '
        + sql
        + " ORDER BY score DESC, addr LIMIT %d" % top,
        args,
    ).fetchall()
    out = ["catalog %s: %d rows match, %d shown" % (short, total, len(rows))]
    for a, sc, g, name, cls, tags, uic, hint, pa, pm, rk, ro, anch, wn, grp, by in rows:
        rule = rk if ro is None else "%s#%d" % (rk, ro)
        out.append(
            "%08x %5.2f %s %s%s%s grp=%d  %s  ui=%d rule=%s pair=%s%s%s%s"
            % (
                a,
                sc,
                "G" if g else "-",
                name,
                " [%s]" % wn if wn else "",
                " (%s)" % cls if cls else "",
                grp,
                tags,
                uic,
                rule,
                "%08x/%s" % (pa, pm) if pa is not None else "-",
                " " + repr(hint) if hint else "",
                " anchored=" + anch if anch else "",
                " by=" + by if by and not cls else "",
            )
        )
    return out


# --- dossier: everything about one function, optionally with its pair on another image ----

DOSSIER_MAX = 90
REFERENCE = os.path.join(ROOT, "modules", "core", "fw", "reference.json")
_DIS = re.compile(r"^([0-9a-f]{8})  [0-9a-f ]+?\s{2,}(\S.*)$")
_A0_LOAD = re.compile(r"^(movea\.l|move\.l|lea)\s.*,A0$")


def parse_disasm(text: str) -> list[tuple[int, str]]:
    """(address, instruction text) of the instruction lines of a Ghidra disassembly dump."""
    out = []
    for ln in text.splitlines():
        m = _DIS.match(ln)
        if m:
            out.append((int(m.group(1), 16), m.group(2).strip()))
    return out


def hidden_return_ptr(insn: str) -> bool:
    """True for a load of A0 (the register that carries a hidden struct return pointer)."""
    return bool(_A0_LOAD.match(insn))


def call_setup(insns: Sequence[tuple[int, str]], site: int, n: int) -> list[str]:
    """The N instructions before the call at SITE and the call itself ('>'), A0 loads marked."""
    before = [i for i in insns if i[0] < site][-n:]
    lines = [
        "    %08x  %s%s"
        % (a, t, "   ; A0: hidden return ptr?" if hidden_return_ptr(t) else "")
        for a, t in before
    ]
    lines += ["  > %08x  %s" % (a, t) for a, t in insns if a == site]
    return lines


def anchors_for(
    entries: Sequence[dict[str, Any]], image: str, entry: int, end: int
) -> list[str]:
    """Declared ColdFire anchors on IMAGE that point into [entry, end) or whose doc names the entry address."""
    out = []
    for e in entries:
        if e.get("cpu") != "cf":
            continue
        spots = [int(s["addr"], 16) for s in e.get("at", {}).get(image, [])]
        inside = [a for a in spots if entry <= a < end]
        if inside or "0x%x" % entry in (e.get("doc") or ""):
            where = "+0x%x" % (inside[0] - entry) if inside else " (doc)"
            out.append("%s [%s]%s" % (e["name"], e.get("mark", "?"), where))
    return out


def anchor_addrs(entries: Sequence[dict[str, Any]], image: str) -> dict[int, str]:
    out: dict[int, str] = {}
    for e in entries:
        if e.get("cpu") == "cf":
            for s in e.get("at", {}).get(image, []):
                out.setdefault(int(s["addr"], 16), e["name"])
    return out


def load_reference() -> list[dict[str, Any]]:
    import json

    try:
        with open(REFERENCE) as fh:
            return json.load(fh)["entries"]
    except (OSError, ValueError, KeyError):
        return []


def catalog_row(image: str, entry: int) -> dict[str, Any] | None:
    path = os.path.join(ATLAS_DIR, short_image(image) + ".sqlite")
    if not os.path.exists(path):
        return None
    con = open_ro(path)
    cols = (
        "h_strict",
        "hash_count",
        "pair_addr",
        "pair_method",
        "rule_kind",
        "rule_ordinal",
        "rule_both",
    )
    try:
        if not _has(con, "catalog"):
            return None
        row = con.execute(
            "SELECT %s FROM catalog WHERE addr=?" % ", ".join(cols), (entry,)
        ).fetchone()
    except sqlite3.OperationalError:
        return None
    finally:
        con.close()
    return dict(zip(cols, row, strict=True)) if row else None


def hash_entries(img: CF, h: str | None) -> list[int]:
    if h is None:
        return []
    return [
        r[0]
        for r in img.db.execute(
            "SELECT entry FROM func_hash WHERE h_strict=? ORDER BY entry", (h,)
        )
    ]


def own_slot(img: CF, entry: int) -> tuple[str, int, int] | None:
    """(class, offset_to_top, slot) of a vtable of its own class that holds ENTRY, or None."""
    return img.db.execute(
        "SELECT class, offset_to_top, slot FROM vtables WHERE target=? AND owner=class "
        "ORDER BY offset_to_top<>0, abs(offset_to_top), class LIMIT 1",
        (entry,),
    ).fetchone()


def rule_targets(
    img: CF,
    kind: str,
    ordinal: int | None,
    h: str | None,
    slot: tuple[str, int, int] | None,
) -> list[int]:
    """Where the rule (kind, ordinal, hash or vtable slot) resolves on IMG."""
    if kind == "cf_func":
        hits = hash_entries(img, h)
        return hits if ordinal is None else hits[ordinal : ordinal + 1]
    if kind == "cf_vtable_slot" and slot:
        return [
            r[0]
            for r in img.db.execute(
                "SELECT DISTINCT target FROM vtables WHERE class=? AND offset_to_top=? AND slot=?",
                slot,
            )
        ]
    return []


def _caller_lines(img: CF, entry: int, n_callers: int, n_insn: int) -> list[str]:
    rows = [r for r in img.callers(entry) if r[0] is not None and r[0] != entry]
    funcs = sorted({r[0] for r in rows})
    sites = len(img.callers(entry))
    out = ["callers: %d sites in %d functions" % (sites, len(funcs))]
    seen: set[int] = set()
    for fn, site, kind in rows:
        if fn in seen:
            continue
        if len(seen) >= n_callers:
            break
        seen.add(fn)
        out.append("  <- %s @%08x %s" % (img.label(fn), site, kind[:4]))
        frow = img.func_row(fn)
        text = img.read_dump(frow[7]) if frow else None
        if text:
            out.extend(call_setup(parse_disasm(text), site, n_insn))
    if len(funcs) > len(seen):
        out.append("  ... %d more callers (callers ADDR)" % (len(funcs) - len(seen)))
    return out


def find_pair(
    img: CF,
    other: CF,
    entry: int,
    row: dict[str, Any] | None,
    rule: tuple[str, int | None, str | None, tuple[str, int, int] | None],
) -> tuple[int | None, str]:
    """The function on OTHER that matches ENTRY and how it was found."""
    if row and row["pair_addr"] is not None:
        return row["pair_addr"], "catalog pair_addr (%s)" % row["pair_method"]
    kind, ordinal, h, slot = rule
    hits = rule_targets(other, kind, ordinal, h, slot)
    if len(hits) == 1:
        return hits[0], "rule %s resolves uniquely" % kind
    rows = other.db.execute(
        "SELECT dst_entry, method FROM matches WHERE src_image=? AND src_entry=?",
        (img.db_name, entry),
    ).fetchall()
    if rows:
        return rows[0][0], "cfmatch %s%s" % (
            rows[0][1],
            " (%d candidates)" % len(rows) if len(rows) > 1 else "",
        )
    rev = other.db.execute(
        "SELECT src_entry, method FROM matches WHERE src_image=? AND dst_entry=?",
        (img.db_name, entry),
    ).fetchall()
    if rev:
        return rev[0][0], "cfmatch %s (reverse)" % rev[0][1]
    return None, "no pair found (try: port %s --from %s --to %s)" % (
        "0x%x" % entry,
        img.image,
        other.image,
    )


def cmd_dossier(
    sess: Session,
    image: str | None,
    token: str,
    other_image: str | None,
    n_callers: int = 3,
    n_decomp: int = 12,
) -> list[str]:
    """One block about the function containing TOKEN; with OTHER_IMAGE also its pair there."""
    img = sess.get(image, token, "cf")
    assert isinstance(img, CF)
    other = sess.get(other_image, None, "cf") if other_image else None
    assert other is None or isinstance(other, CF)
    entry = img.resolve(token)
    f = img.func_row(entry)
    assert f is not None
    from tools.firmware.catalog import suggest_rule

    refs = load_reference()
    hrow = img.db.execute(
        "SELECT h_strict FROM func_hash WHERE entry=?", (entry,)
    ).fetchone()
    h = hrow[0] if hrow else None
    same = hash_entries(img, h)
    how = ""
    out = [
        "dossier %s %s" % (img.image, img.label(entry)),
        "entry=%08x size=%d end=%08x hash=%s (%d with this hash)"
        % (entry, f[4], f[1], h[:12] if h else "none", len(same)),
    ]
    if f[5]:
        out.append("sig: " + clip(f[5]))
    slot = own_slot(img, entry)
    slots = img.vtable_slots(entry)
    if slots:
        more = " ..." if len(slots) > 6 else ""
        out.append("vfunc: " + ", ".join("%s#%d" % s for s in slots[:6]) + more)
    else:
        out.append("vfunc: no")
    strs = img.strings_of(entry, 6)
    if strs:
        out.append("strings: " + "; ".join(repr(clip(t, 40)) for _a, t in strs))
    anchors = anchors_for(refs, img.image, entry, f[1])
    out.append("anchors: " + ("; ".join(anchors) if anchors else "none"))
    row = catalog_row(img.image, entry)
    if row and row["rule_kind"]:
        kind, ordinal, source = row["rule_kind"], row["rule_ordinal"], "catalog"
    else:
        kind, ordinal = suggest_rule(entry, same, slot is not None)
        source = "computed"
    ord_txt = " ordinal=%d" % ordinal if ordinal is not None else ""
    slot_txt = (
        " class=%s slot=%d" % (slot[0], slot[2])
        if kind == "cf_vtable_slot" and slot
        else ""
    )
    both_txt = (
        " both_devices=%s" % row["rule_both"]
        if row and row["rule_both"] is not None
        else ""
    )
    out.append("rule (%s): %s%s%s%s" % (source, kind, ord_txt, slot_txt, both_txt))
    rule = (kind, ordinal, h, slot)
    pair: int | None = None
    if other is not None:
        got = rule_targets(other, kind, ordinal, h, slot)
        pair, how = find_pair(img, other, entry, row, rule)
        out.append(
            "on %s the rule resolves to: %s%s"
            % (
                other.image,
                ", ".join("%08x" % a for a in got) if got else "nothing",
                "  (the pair)" if pair is not None and got == [pair] else "",
            )
        )
    out.extend(_caller_lines(img, entry, n_callers, 8))
    callees = img.callees(entry)
    named = anchor_addrs(refs, img.image)
    ranked = sorted(
        callees,
        key=lambda c: (c[0] not in named, img.names().get(c[0]) is None, c[0]),
    )
    out.append("callees: %d" % len(callees))
    for c, _k in ranked[:8]:
        tag = "  {%s}" % named[c] if c in named else ""
        out.append("  -> %s%s" % (img.label(c), tag))
    if len(ranked) > 8:
        out.append("  -> ... %d more" % (len(ranked) - 8))
    text = img.read_dump(f[6])
    if text:
        body = _decomp_body(text)
        out.append("decomp (%d of %d lines):" % (min(n_decomp, len(body)), len(body)))
        out.extend("  " + ln[:110] for ln in body[:n_decomp])
    else:
        out.append("decomp: none")
    if other is not None:
        out.append("")
        if pair is None:
            out.append("pair on %s: %s" % (other.image, how))
        else:
            pf = other.func_row(pair)
            ph = other.db.execute(
                "SELECT h_strict FROM func_hash WHERE entry=?", (pair,)
            ).fetchone()
            out.append("pair on %s: %s" % (other.image, other.label(pair)))
            out.append("  found by: " + how)
            if pf:
                sig = "  sig: " + clip(pf[5], 80) if pf[5] else ""
                out.append(
                    "  size=%d same strict hash=%s%s"
                    % (pf[4], "yes" if ph and ph[0] == h else "no", sig)
                )
            out.extend("  " + ln for ln in _caller_lines(other, pair, 2, 4))
    return out


# --- argument parsing and dispatch -----------------------------------------------


def build_parser() -> argparse.ArgumentParser:
    p = argparse.ArgumentParser(
        prog="fw.py", description=__doc__.splitlines()[0], add_help=True
    )
    sub = p.add_subparsers(dest="cmd", required=True)

    def common(sp: argparse.ArgumentParser) -> argparse.ArgumentParser:
        sp.add_argument(
            "-i", "--image", help="e.g. dt2-1.17, dn2-1.12 (default $FW_IMAGE)"
        )
        sp.add_argument(
            "--cpu",
            choices=("cf", "sharc"),
            help="override the address-based CPU choice",
        )
        sp.add_argument(
            "--max",
            type=int,
            default=None,
            help="line cap (default %d, 0 = none)" % DEFAULT_MAX,
        )
        return sp

    for name, helptext in (
        ("fn", "function summary"),
        ("decomp", "decompiled C"),
        ("disasm", "disassembly"),
        ("callers", "call sites into a function"),
        ("callees", "functions a function calls"),
        ("xref", "everything that references an address"),
    ):
        sp = common(sub.add_parser(name, help=helptext))
        sp.add_argument("target", help="address (hex), Ghidra name or cf_names name")
    sp = common(
        sub.add_parser(
            "stores",
            help="read-only SHARC CALL-reachable store operands as uncapped JSON",
        )
    )
    sp.add_argument(
        "roots", nargs="+", help="explicit root PCs (hex), not inferred task roots"
    )
    sp.add_argument("--expected-image-sha256")
    sp.add_argument(
        "--image-blob", help="explicit blob path override; bytes must match DB SHA-256"
    )
    sp = common(sub.add_parser("str", help="strings containing TEXT and who uses them"))
    sp.add_argument("text", nargs="+")
    sp = common(sub.add_parser("class", help="RTTI class: bases, derived, vtable"))
    sp.add_argument("name")
    sp = common(sub.add_parser("path", help="shortest call path A -> B"))
    sp.add_argument("a")
    sp.add_argument("b")
    sp = common(sub.add_parser("port", help="carry a function between versions"))
    sp.add_argument("name")
    sp.add_argument("--from", dest="src", required=True)
    sp.add_argument("--to", dest="dst", required=True)
    sp = common(sub.add_parser("sql", help="read-only SQL"))
    sp.add_argument("query")
    sp = common(
        sub.add_parser(
            "search", help="full-text search over names, strings, decompiled C, atlas"
        )
    )
    sp.add_argument("term", nargs="+")
    sp.add_argument("--in", dest="where", default="names,strings,decomp,atlas")
    sp = common(
        sub.add_parser("atlas", help="semantic atlas lookup (tools/coldfire/atlas.py)")
    )
    sp.add_argument("term", nargs="+")
    sp = common(
        sub.add_parser(
            "catalog", help="candidate anchor functions (atlas.py catalog), best first"
        )
    )
    sp.add_argument("term", nargs="*", help="substring of name, class, hint or tags")
    sp.add_argument("--tag", help="tag prefix, e.g. near:cf.menu, role:model, term:fx")
    sp.add_argument("--top", type=int, default=30)
    sp.add_argument("--grade", action="store_true", help="only anchor_grade=1")
    sp.add_argument("--group", type=int, help="only rows of this group id")
    sp.add_argument(
        "--groups", action="store_true", help="list the groups (top N by size)"
    )
    sp = common(
        sub.add_parser(
            "dossier",
            help="one block on a function (signature, class, strings, callers with argument setup, callees, "
            "decomp, anchors, rule) and with --other its pair",
        )
    )
    sp.add_argument("target", help="address (hex), Ghidra name or cf_names name")
    sp.add_argument(
        "--other", help="the other image, e.g. dn2-1.12: find the pair there"
    )
    sp.add_argument("--callers", type=int, default=3, help="callers to show")
    sp.add_argument("--decomp", type=int, default=12, help="decompiled lines to show")
    sp = common(sub.add_parser("batch", help="run many queries from FILE or - (stdin)"))
    sp.add_argument("file")
    return p


def run(
    sess: Session, args: argparse.Namespace, default_image: str | None = None
) -> str:
    c = args.cmd
    cap = (
        (DOSSIER_MAX if c == "dossier" else DEFAULT_MAX)
        if args.max is None
        else args.max
    )
    image = args.image or default_image
    if c == "batch":
        raise FwError("batch cannot be nested")
    if c == "dossier":
        return capped(
            cmd_dossier(
                sess, image, args.target, args.other, args.callers, args.decomp
            ),
            cap,
        )
    if c == "stores":
        if args.cpu not in (None, "sharc"):
            raise FwError("stores supports SHARC databases only")
        img = sess.get(image, None, "sharc")
        assert isinstance(img, Sharc)
        return json.dumps(
            cmd_stores(img, args.roots, args.expected_image_sha256, args.image_blob),
            sort_keys=True,
            indent=2,
        )
    if c == "sql":
        return capped(cmd_sql(sess.get(image, None, args.cpu), args.query), cap)
    if c == "search":
        return capped(
            cmd_search(sess, image, " ".join(args.term), args.where, cap, args.cpu), cap
        )
    if c == "atlas":
        image = image or os.environ.get("FW_IMAGE")
        if not image:
            raise FwError("no image: pass -i dt2-1.17 or set FW_IMAGE")
        return capped(cmd_atlas(image, " ".join(args.term)), cap)
    if c == "catalog":
        image = image or os.environ.get("FW_IMAGE")
        if not image:
            raise FwError("no image: pass -i dt2-1.17 or set FW_IMAGE")
        return capped(
            cmd_catalog(
                image,
                " ".join(args.term),
                args.tag,
                args.top,
                args.grade,
                args.group,
                args.groups,
                args.cpu or "cf",
            ),
            cap,
        )
    if c == "port":
        return capped(cmd_port(sess, args.name, args.src, args.dst), cap)
    if c == "str":
        return capped(cmd_str(sess.get(image, None, "cf"), " ".join(args.text)), cap)
    if c == "class":
        return capped(cmd_class(sess.get(image, None, "cf"), args.name), cap)
    if c == "path":
        return capped(cmd_path(sess.get(image, args.a, args.cpu), args.a, args.b), cap)
    img = sess.get(image, args.target, args.cpu)
    fn = {
        "fn": cmd_fn,
        "decomp": cmd_decomp,
        "disasm": cmd_disasm,
        "callers": cmd_callers,
        "callees": cmd_callees,
        "xref": cmd_xref,
    }[c]
    return capped(fn(img, args.target), cap)


def run_line(
    sess: Session, argv: list[str], default_image: str | None, default_max: int | None
) -> str:
    parser = build_parser()
    try:
        args = parser.parse_args(argv)
    except SystemExit:
        return "error: bad arguments: " + " ".join(argv)
    if args.max is None and args.cmd != "dossier":
        args.max = default_max
    try:
        return run(sess, args, default_image)
    except FwError as e:
        return "error: %s" % e
    except sqlite3.Error as e:
        return "sql error: %s" % e


def cmd_batch(sess: Session, args: argparse.Namespace) -> str:
    if args.file == "-":
        text = sys.stdin.read()
    else:
        with open(args.file) as fh:
            text = fh.read()
    blocks = []
    cap = args.max if args.max is not None else 40
    for raw in text.splitlines():
        line = raw.strip()
        if not line or line.startswith("#"):
            continue
        try:
            argv = shlex.split(line)
        except ValueError as e:
            blocks.append("## %s\nerror: %s" % (line, e))
            continue
        if argv and argv[0] in ("fw.py", "fw"):
            argv = argv[1:]
        blocks.append("## %s\n%s" % (line, run_line(sess, argv, args.image, cap)))
    return "\n".join(blocks)


def main(argv: list[str] | None = None) -> int:
    parser = build_parser()
    args = parser.parse_args(argv)
    sess = Session()
    try:
        if args.cmd == "batch":
            print(cmd_batch(sess, args))
        else:
            print(run(sess, args))
    except FwError as e:
        print("error: %s" % e, file=sys.stderr)
        return 1
    except sqlite3.Error as e:
        print("sql error: %s" % e, file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
