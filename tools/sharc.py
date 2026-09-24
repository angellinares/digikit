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

import collections
import os
import sqlite3
import sys
import time

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

# Known DM structures, addresses in this repo's own bare convention (the same
# one literals.value/dataref.value/ptr.address already use -- see
# tools/sharcdb.py's DB_VERSION v7 comment for how that was pinned down
# against IVT_DISPATCH_TABLES). Used by Image.card() to label a resolved
# memory access by structure instead of a bare hex address. Strided arrays
# first (a fixed-size record repeated N times), then flat ranges; the first
# match wins. Track count is a generous upper bound, not a verified count --
# an index past the real number of tracks is still reported (as a high
# index), not silently misclassified as "elsewhere".
_KNOWN_STRIDED = (
    # docs/findings/06: the per-track record array and its "array #2" twin.
    (0x2506EC, 0xDC, 32, "per-track record"),
    (0x2412C8 + 0xD604, 0x1D8, 32, "per-track array#2"),
)
_KNOWN_RANGES = (
    (0x2412C8, 0x2412C8 + 0x10000, "per-frame workspace"),
    (0x261CC8, 0x264200, "rings"),
    (0x252D78, 0x252D78 + 4, "mix table base"),
    # tools/sharcdb.py's IVT_DISPATCH_TABLES, in the same address convention;
    # both are boot-time FILL blocks on every image checked so far (DB_VERSION
    # v7's comment) -- labelled here anyway so a resolved access there (e.g.
    # once startup code is traced) still gets a name instead of a bare hex.
    (0x240948, 0x240948 + 392, "SECI dispatch table"),
    (0x240AD4, 0x240AD4 + 1740, "generic interrupt dispatch table"),
)

# tools/sharc.py's Image.card() pairs an image with its cross-image partner
# (same func_hash convention, see Image.match()) for the "cross-image match"
# section -- the two shipping SHARC+ firmwares this repo builds.
_CROSS_IMAGE_PARTNER = {
    "dt2-1.16": "dn2-1.11", "dt2-1.15C": "dn2-1.10E",
    "dn2-1.11": "dt2-1.16", "dn2-1.10E": "dt2-1.15C",
}


def _describe_address(addr):
    """A known-structure label for a DM address (see _KNOWN_STRIDED/
    _KNOWN_RANGES above), or a bare hex string when nothing matches."""
    if addr is None:
        return "unresolved"
    for base, stride, count, label in _KNOWN_STRIDED:
        if base <= addr < base + stride * count:
            idx, off = divmod(addr - base, stride)
            return "%s[%d]%s" % (label, idx, ("+0x%x" % off) if off else "")
    for lo, hi, label in _KNOWN_RANGES:
        if lo <= addr < hi:
            off = addr - lo
            return "%s%s" % (label, ("+0x%x" % off) if off else "")
    return "0x%x" % addr


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
        self._notes_attached = False
        self._cross_cache = {}

    def close(self):
        for other in self._cross_cache.values():
            if other is not None:
                other.close()
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

    def _ptr_hits(self, addr, size, direction):
        """Every tools/sharcdb.py `ptr` row for `direction` whose resolved
        [address, address+width) range overlaps [addr, addr+size) ('kind':
        'resolved'), plus every row whose base is known but whose address
        isn't (an indexed I,M form with an untracked modifier) and whose
        base sits within 0x10000 bytes below addr ('kind': 'base_only') --
        a plausible hit for a runtime-indexed struct/array write this static
        pass can't fully resolve, not a proof. See tools/sharcdb.py's
        _build_ptr_rows module note for what the `ptr` table does and does
        not cover."""
        hi = addr + size
        rows = self.db.execute(
            "SELECT sw, base_reg, base_value, address, width, "
            "  CASE WHEN address IS NOT NULL THEN 'resolved' ELSE 'base_only' END kind "
            "FROM ptr WHERE image=? AND direction=? AND ("
            "  (address IS NOT NULL AND address < ? AND address + (CASE width WHEN 'long' THEN 8 ELSE 4 END) > ?)"
            "  OR (address IS NULL AND base_value IS NOT NULL AND base_value <= ? AND ? - base_value < 0x10000)"
            ") ORDER BY sw",
            (self.name, direction, hi, addr, addr, addr),
        ).fetchall()
        return [
            {
                "sw": _hex(sw), "base_reg": base_reg,
                "base_value": _hex(base_value) if base_value is not None else None,
                "address": _hex(address) if address is not None else None,
                "width": width, "kind": kind,
            }
            for sw, base_reg, base_value, address, width, kind in rows
        ]

    def writers(self, addr, size=4):
        """Every store tools/sharcdb.py's constant-pointer propagation (the
        `ptr` table) can place at or partly inside [addr, addr+size)."""
        return self._ptr_hits(addr, size, "store")

    def readers(self, addr, size=4):
        """The same, for a load."""
        return self._ptr_hits(addr, size, "load")

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

    # --- notes ----------------------------------------------------------------
    #
    # A human/agent-written note per function, kept in a sidecar database
    # (out/sharcdb/<name>.notes.sqlite, ATTACHed here on first use) rather
    # than in the main .sqlite: tools/sharcdb.py's build_database() always
    # replaces out_path wholesale (see its own "build into a temp file" note),
    # so a note living there would be wiped by the next `sharcdb build
    # --force`. This file never touches the sidecar's path.

    def _ensure_notes_db(self):
        if self._notes_attached:
            return
        notes_path = os.path.join(os.path.dirname(os.path.abspath(self.db_path)), self.name + ".notes.sqlite")
        self.db.execute("ATTACH DATABASE ? AS notesdb", (notes_path,))
        self.db.execute("""CREATE TABLE IF NOT EXISTS notesdb.notes (
            image TEXT, function_sw INTEGER, role TEXT, summary TEXT, reads TEXT,
            writes TEXT, confidence TEXT, evidence TEXT, author TEXT, created TEXT,
            PRIMARY KEY (image, function_sw))""")
        self._notes_attached = True

    _NOTE_FIELDS = ("role", "summary", "reads", "writes", "confidence", "evidence", "author")

    def note(self, fn, **fields):
        """Upsert this image's note for function `fn` in the sidecar
        database. fields: any of role/summary/reads/writes/confidence/
        evidence/author; a field left out keeps its previous value (or NULL,
        for a brand new row). `created` is set to now on first write and left
        alone on an update, unless passed explicitly."""
        self._ensure_notes_db()
        existing = self.notes(fn)
        prior = existing[0] if existing else {}
        row = {f: fields.get(f, prior.get(f)) for f in self._NOTE_FIELDS}
        created = fields.get("created") or prior.get("created") or time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime())
        self.db.execute(
            "INSERT INTO notesdb.notes (image, function_sw, role, summary, reads, writes, confidence, "
            "evidence, author, created) VALUES (?,?,?,?,?,?,?,?,?,?) "
            "ON CONFLICT(image, function_sw) DO UPDATE SET role=excluded.role, summary=excluded.summary, "
            "reads=excluded.reads, writes=excluded.writes, confidence=excluded.confidence, "
            "evidence=excluded.evidence, author=excluded.author, created=excluded.created",
            (self.name, fn, row["role"], row["summary"], row["reads"], row["writes"],
             row["confidence"], row["evidence"], row["author"], created),
        )
        self.db.commit()

    def notes(self, fn=None):
        """This image's note for `fn` (a single-element list, or [] when
        none exists), or every note in the image when fn is None."""
        self._ensure_notes_db()
        cols = ("function_sw", "role", "summary", "reads", "writes", "confidence", "evidence", "author", "created")
        query = "SELECT %s FROM notesdb.notes WHERE image=?" % ",".join(cols)
        args = [self.name]
        if fn is not None:
            query += " AND function_sw=?"
            args.append(fn)
        query += " ORDER BY function_sw"
        rows = self.db.execute(query, args).fetchall()
        out = [dict(zip(cols, r)) for r in rows]
        for d in out:
            d["function_sw"] = _hex(d["function_sw"])
        return out

    # --- function cards ---------------------------------------------------------

    def _cross_partner(self):
        """The Image for this one's cross-image partner (see
        _CROSS_IMAGE_PARTNER), lazily loaded and cached, or None when there
        is no known partner or its database isn't built."""
        if self.name in self._cross_cache:
            return self._cross_cache[self.name]
        other_name = _CROSS_IMAGE_PARTNER.get(self.name)
        other = None
        if other_name:
            try:
                other = load(other_name)
            except Exception:
                other = None
        self._cross_cache[self.name] = other
        return other

    def card(self, fn, listing_n=8):
        """A deterministic, compact text card for one function: header,
        roots reaching it, callers/callees (with any note's role/summary),
        literals and datarefs, resolved memory reads/writes grouped by
        known structure (see _describe_address), loops, float-compute
        density, a cross-image match, and a listing (full when short,
        otherwise the first/last `listing_n` instructions plus every
        branch/call/return/indirect/store line in between)."""
        f = self.func(fn)
        if f is None:
            return "no function at 0x%x" % fn
        entry, end = int(f["entry_sw"], 16), int(f["end_sw"], 16)
        L = []
        L.append("FUN_%06x  entry=0x%x end=0x%x  n_insns=%d  bytes=%d" %
                  (entry, entry, end, f["n_insns"], (end - entry) * 2))
        bits = ["block=%s" % f["block"], "leaf" if f["is_leaf"] else "non-leaf"]
        if f["name"] and f["name"] != "FUN_%06x" % entry:
            bits.append("name=%s" % f["name"])
        if f["label"]:
            bits.append("label=%s" % f["label"])
        if f["entry_kind"]:
            bits.append("entry_kind=%s" % f["entry_kind"])
        L.append("  " + " ".join(bits))

        own_note = self.notes(entry)
        if own_note:
            n = own_note[0]
            L.append("note: role=%s  %s" % (n.get("role"), n.get("summary") or ""))

        root_rows = self.db.execute(
            "SELECT r.kind, MIN(rc.depth) FROM reach rc JOIN roots r "
            "ON r.image = rc.image AND r.sw = rc.root_sw "
            "WHERE rc.image=? AND rc.function_sw=? GROUP BY r.kind ORDER BY 2",
            (self.name, entry),
        ).fetchall()
        L.append("roots: " + (", ".join("%s(depth=%s)" % (k, d) for k, d in root_rows) if root_rows else "none reach this function"))

        def _role_suffix(fn_hex):
            if fn_hex is None:
                return ""
            rows = self.notes(int(fn_hex, 16))
            if not rows or not rows[0].get("summary"):
                return ""
            return " -- %s: %s" % (rows[0].get("role") or "?", rows[0]["summary"])

        callers = self.callers(entry)
        seen, caller_bits = set(), []
        for c in callers:
            key = (c["from_function"], c["kind"])
            if key in seen:
                continue
            seen.add(key)
            caller_bits.append("%s(%s)%s" % (c["from_function"] or c["from_sw"], c["kind"], _role_suffix(c["from_function"])))
        L.append("callers (%d): %s" % (len(caller_bits), "; ".join(caller_bits) or "none"))

        callees = self.callees(entry)
        L.append("callees (%d): %s" % (
            len(callees), "; ".join("%s%s" % (c, _role_suffix(c)) for c in callees) or "none"))

        lit_rows = self.db.execute(
            "SELECT DISTINCT l.value, l.form, l.dest_reg FROM literals l JOIN insn i "
            "ON i.image = l.image AND i.sw = l.sw WHERE l.image=? AND i.function_sw=? ORDER BY l.value",
            (self.name, entry),
        ).fetchall()
        if lit_rows:
            shown = ["0x%x(%s%s)" % (v, form, "->" + dr if dr else "") for v, form, dr in lit_rows[:20]]
            more = "" if len(lit_rows) <= 20 else " ... +%d more" % (len(lit_rows) - 20)
            L.append("literals (%d): %s%s" % (len(lit_rows), ", ".join(shown), more))

        dataref_rows = self.db.execute(
            "SELECT DISTINCT d.value, d.role FROM dataref d WHERE d.image=? AND d.sw>=? AND d.sw<? ORDER BY d.value",
            (self.name, entry, end),
        ).fetchall()
        if dataref_rows:
            L.append("datarefs: " + ", ".join("%s(%s)" % (_describe_address(v), role) for v, role in dataref_rows[:20]))

        mem_rows = self.db.execute(
            "SELECT direction, address, base_value FROM ptr WHERE image=? AND sw>=? AND sw<?",
            (self.name, entry, end),
        ).fetchall()
        grouped = collections.defaultdict(lambda: [0, 0])
        for direction, address, base_value in mem_rows:
            label = _describe_address(address if address is not None else base_value)
            grouped[label][0 if direction == "load" else 1] += 1
        if grouped:
            L.append("memory: " + "; ".join(
                "%s(r=%d,w=%d)" % (label, r, w) for label, (r, w) in sorted(grouped.items())))

        loop_rows = self.db.execute(
            "SELECT header_block, n_blocks, kind, depth FROM loops WHERE image=? AND function_sw=? ORDER BY header_block",
            (self.name, entry),
        ).fetchall()
        if loop_rows:
            L.append("loops: " + "; ".join("0x%x(%s,n_blocks=%d,depth=%d)" % (h, k, n, d) for h, n, k, d in loop_rows))

        total = self.db.execute(
            "SELECT COUNT(*) FROM regdef WHERE image=? AND sw>=? AND sw<? AND kind='compute'",
            (self.name, entry, end),
        ).fetchone()[0]
        if total:
            floaty = self.db.execute(
                "SELECT COUNT(*) FROM regdef WHERE image=? AND sw>=? AND sw<? AND kind='compute' AND reg LIKE 'F%'",
                (self.name, entry, end),
            ).fetchone()[0]
            L.append("float-compute density: %.0f%% (%d/%d compute regdefs)" % (100.0 * floaty / total, floaty, total))

        other = self._cross_partner()
        if other is not None:
            matches = self.match(other, entry)
            L.append("cross-image (%s): %s" % (
                other.name,
                ", ".join("%s%s" % (m["entry_sw"], "" if m["exact_match"] else "(reloc-only)") for m in matches)
                if matches else "no match",
            ))

        all_rows = self.db.execute(
            "SELECT sw, mnemonic FROM insn WHERE image=? AND function_sw=? AND aligned=1 ORDER BY sw",
            (self.name, entry),
        ).fetchall()
        n = listing_n
        L.append("listing (%d insns%s):" % (len(all_rows), "" if len(all_rows) <= 2 * n + 4 else ", truncated"))
        if len(all_rows) <= 2 * n + 4:
            L += ["  0x%x  %s" % (sw, m) for sw, m in all_rows]
        else:
            # Branch/call/return/indirect sites are kept in full (there are
            # rarely more than a handful); store sites are capped so one
            # store-heavy dispatcher (e.g. FUN_1c642a) doesn't blow the
            # card's whole size budget -- see the module note on card()'s
            # 1-3k token target.
            control_flow = set(r[0] for r in self.db.execute(
                "SELECT DISTINCT from_sw FROM edges WHERE image=? AND from_sw>=? AND from_sw<? "
                "AND kind IN ('call','jump','cond_jump','indirect','return')", (self.name, entry, end),
            ).fetchall())
            stores = set(r[0] for r in self.db.execute(
                "SELECT DISTINCT sw FROM mem_access WHERE image=? AND sw>=? AND sw<? AND direction='store'",
                (self.name, entry, end),
            ).fetchall())
            head, tail = all_rows[:n], all_rows[-n:]
            edge_sws = {sw for sw, _ in head} | {sw for sw, _ in tail}
            store_cap = 40
            mid_stores = [(sw, m) for sw, m in all_rows if sw not in edge_sws and sw in stores]
            omitted_stores = max(0, len(mid_stores) - store_cap)
            mid_stores = mid_stores[:store_cap]
            mid = sorted(
                {(sw, m) for sw, m in all_rows if sw not in edge_sws and sw in control_flow} | set(mid_stores)
            )
            L += ["  0x%x  %s" % (sw, m) for sw, m in head]
            L.append("  ...")
            L += ["  0x%x  %s" % (sw, m) for sw, m in mid]
            if omitted_stores:
                L.append("  ... (+%d more store lines omitted)" % omitted_stores)
            L.append("  ...")
            L += ["  0x%x  %s" % (sw, m) for sw, m in tail]

        return "\n".join(L)

    def cards(self, order="bottom_up", root=None, limit=None):
        """Yield card() text for every function in the image (or, with
        `root`, only those in `reach` from that root sw or sws), in
        callee-first ('bottom_up', the default) or caller-first
        ('top_down') topological order over the CALL-edge graph (SCC-
        condensed, same construction as tools/sharcdb.py's
        _detect_callgraph, so a recursive cluster is yielded as one group)."""
        if order not in ("bottom_up", "top_down"):
            raise ValueError("cards(): order must be 'bottom_up' or 'top_down', got %r" % order)
        fn_rows = self.db.execute("SELECT entry_sw FROM functions WHERE image=?", (self.name,)).fetchall()
        G = nx.DiGraph()
        G.add_nodes_from(r[0] for r in fn_rows)
        G.add_edges_from(self.db.execute(
            "SELECT DISTINCT from_function, to_function FROM edges WHERE image=? AND kind='call' "
            "AND from_function IS NOT NULL AND to_function IS NOT NULL", (self.name,),
        ).fetchall())
        if root is not None:
            roots = root if isinstance(root, (list, tuple, set)) else [root]
            keep = set()
            for r in roots:
                keep |= {row[0] for row in self.db.execute(
                    "SELECT function_sw FROM reach WHERE image=? AND root_sw=?", (self.name, r),
                ).fetchall()}
            G = G.subgraph(keep).copy()

        C = nx.condensation(G)
        topo = list(nx.topological_sort(C))
        if order == "bottom_up":
            topo.reverse()
        count = 0
        for c in topo:
            for entry in sorted(C.nodes[c]["members"]):
                if limit is not None and count >= limit:
                    return
                yield self.card(entry)
                count += 1

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
