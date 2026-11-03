#!/usr/bin/env python3
"""Resolve the anchors and regions of one firmware into its port lockfile.

    uv run python -m tools.patchkit.port_resolve resolve FIRMWARE.syx [--out devices/ports/NAME.toml]
    uv run python -m tools.patchkit.port_resolve resolve-all [--root DIR]      # the supported firmwares: DT2 1.17, DN2 1.12, DN2 1.11
    uv run python -m tools.patchkit.port_resolve rule-sharc IMAGE PC [--len N] # print a devices/fw rule for a SHARC pc
    (for a rule of either CPU and a declaration entry, see tools/firmware/catalog.py: emit-rule, declare)

A port (devices/ports/<short>-<version>.toml) records, for one stock `.syx` (keyed by its sha256), the
named anchors the modules and frameworks need (address, length, sha256 of the bytes there; never the bytes),
the allocatable regions and the container limits. patchkit only reads it and verifies the hashes.

Anchors are declared in devices/fw/<subsystem>.toml (docs/design/firmware-decls.md), not in module manifests. One
entry is one declaration; its `rule` is shared, or `rule.dt2` / `rule.dn2` give one rule per device:

    [[fn]]
    name = "dsp.master.fn.q31_converter"   # <cpu>.<subsystem>.<kind>.<name>; the kind is the table, the
                                           # subsystems are listed in devices/fw/subsystems.toml
    cpu = "sharc"
    devices = ["dt2"]                # the devices that have it; the entry is skipped on the others
    rule = { kind = "sharc_func", reloc_hash = "...", offset = 0x0, len = 6 }   # kind: see below
    mark = "D"

The tables fn, site, data and value hold single anchors and sites holds multi-site rules. The table struct is not
resolved. A `[[hook]]` names a declared `site` (and an optional `offset`) and a `mode`; the port file lists each hook
whose site resolves on that device. An unknown table is an error. A renamed entry lists its former names in
`was = [...]`; they are for reading older docs, never resolve, and must not be a current name.

Rule kinds (SHARC rules use the image's sharcdb through tools/sharc_dev/sharc.py):

  sharc_func     reloc_hash = <hash of the containing function>, offset = <parcels from its entry>, len = <bytes>,
                 ordinal = <index among the functions with that hash, in entry address order>.
                 The function is found by its relocation-insensitive hash (it must be unique in the image unless the
                 rule gives an ordinal; the same holds for the three kinds below); the anchor is the `len` bytes at
                 entry + offset. SHARC `pm` is the entry + offset pc.
  sharc_literal  as sharc_func, but the instruction at entry + offset loads a constant (a literal in the
                 sharcdb): the anchor's value is that constant (a data address); `src` is the instruction.

  sharc_table    as sharc_literal, but the anchor is the table the literal points to: a data anchor of `len` bytes at the
                 software alias 0x28000000 + the literal (`src` = the instruction).

  sharc_mem_addr as sharc_literal, but the instruction at entry + offset is a direct DM access (Type 14d): the
                 anchor's value is its absolute address (a frame word or other fixed data), `src` the instruction.

  cf_words       words = [u32 or "*", ...]: a table row found by a masked signature (big-endian u32 words at 4-byte
                 alignment, exactly one match in the MAIN OS image); a data anchor over the row. With `as = "code"`, `offset` and
                 `len`: a code anchor of `len` bytes at row + offset (a call site found by the words around it, for images
                 where the cfdb has no function there).

  cf_func        hash = <cfdb h_strict of the containing function>, ordinal = <index among the functions with that
                 hash, in address order; default 0>, offset = <bytes from its entry>, len = <bytes>: a code
                 anchor. The hash masks address operands, so one rule serves relinked builds; a hash with fewer
                 functions than the ordinal needs is an error. Needs the image's cfdb (out/cfdb/<image>-emac).

  cf_vtable_slot class = <RTTI class name>, slot = <vtable slot>, offset_to_top = <0 for the primary vtable, default 0;
                 negative for a secondary one>, offset = <bytes from the function entry; default 0>, len = <bytes>: a
                 code anchor on the function in that slot, for virtual functions whose hash is not unique. Exactly
                 one cfdb vtables row must match the class, offset_to_top and slot. Needs the image's cfdb.

  cf_params_t1   the parameter descriptor table (60-byte rows), found by the 36-byte signature of its reader (finding 02,
                 exactly one match): a data anchor over all rows (the row count is the length / 60).
  cf_params_sites  the sites of the code that depend on that table, for core/params: the 2-aligned words that point
                 into it and the clamp bounds (`cmpi.l #N,Dn` / `cmpa.l #N,An` with N the row count or one less,
                 followed by scs/scc/bhi/bls), found by the scan of scan_param_sites below. An anchor of
                 kind "sites": addr = the table, len = number of sites, sha256 = their digest.

  cf_ptr         as cf_func (hash, ordinal, offset), but the 4 bytes at entry + offset are an absolute address literal
                 (a `lea table,An`, a `pea`, an `addi.l #table,Dn`): the anchor is the table the literal points to
                 (kind data, `src` = the literal, `len` bytes of the table hashed).

  cf_reset_init_calls   the two reset-path calls (copy helper, clear helper) at the start of the reset code, found by
                 tools/coldfire/regions.py `reset_facts` (a masked signature of the clear helper plus the call sites):
                 an 8-byte code anchor.
  cf_reset_helper       which = "copy" | "clear": a value anchor, the address of that helper, read from the same
                 8 bytes (the displacement of its jsr).

Regions: dsp.l2 (the stream's loaded end to the end of L2, minus excludes), cf.flash_cave (the zero cave that ends at
the reset clear's start) and cf.expansion (the gap above the DMA arena), from tools/coldfire/regions.py `analyse`.

Every anchor is stamped with the rule that produced it and a mark (D = measured or read once). A rule that
matches nothing or more than one place is an error, so a firmware that moved code fails here, not at patch time.
"""

from __future__ import annotations

import argparse
import functools
import hashlib
import re
import sys
import tomllib
from pathlib import Path

from tools.firmware import config

ROOT = Path(__file__).resolve().parents[2]

RESOLVED_BY = "tools/patchkit/port_resolve.py 1"
# Per device, from devices/*.toml `[patch]`: the port limits, the DSP L2 range and what it leaves out, the MAIN OS load address.
LIMIT_KEYS = (
    "container_max",
    "dsp_stored_max",
    "dsp_depacked_max",
    "main_staged_max",
)


class ResolveError(RuntimeError):
    pass


def sha(b: bytes) -> str:
    return hashlib.sha256(b).hexdigest()


def load_profiles(root: Path = ROOT) -> dict[str, dict]:
    """sha256 -> {device, version, dsp image, filename, port (has a port lockfile), patch (the device's `[patch]`)}"""
    out = {}
    for f in sorted((root / "devices").glob("*.toml")):
        d = tomllib.loads(f.read_text())
        if "device" not in d or "firmware" not in d:
            continue
        for fw in d["firmware"]:
            out[fw["sha256"]] = {
                "device": d["device"]["short"],
                "version": fw["version"],
                "dsp": (fw.get("dsp") or {}).get("image"),
                "filename": fw.get("filename"),
                "port": bool(fw.get("port")),
                "patch": d.get("patch") or {},
                # RAM inside cf.expansion that another patcher's build uses at fixed addresses (`[[firmware]] cf_expansion_excludes`)
                "cf_expansion_excludes": fw.get("cf_expansion_excludes", []),
            }
    return out


def ported_firmware(root: Path = ROOT) -> list[str]:
    """File names of the firmware that have a port lockfile (`port = true` in the device files), in device-file order."""
    return [p["filename"] for p in load_profiles(root).values() if p["port"]]


RULE_TABLES = ("fn", "site", "data", "value", "sites")
UNRESOLVED_TABLES = ("struct",)
HOOK_MODES = ("before", "replace", "retarget")
SUBSYSTEMS = "subsystems.toml"
NAME_SEGMENT = re.compile(r"[a-z0-9_]+")


def load_subsystems(root: Path) -> dict[str, set[str]]:
    """cpu -> the allowed subsystem words, from devices/fw/subsystems.toml."""
    d = tomllib.loads((root / "devices" / "fw" / SUBSYSTEMS).read_text())
    return {cpu: set(words) for cpu, words in d.items()}


def check_name(name: str, table: str, vocab: dict[str, set[str]]) -> None:
    """A declaration name is `<cpu>.<subsystem>.<kind>.<name>`: the cpu and subsystem are in VOCAB, the kind is the
    entry's TABLE, and the rest is dot-separated segments of [a-z0-9_] (so `clone.0.alt` is allowed)."""
    parts = name.split(".")
    if len(parts) < 4:
        raise ResolveError(
            f"{name}: a name has the form <cpu>.<subsystem>.<kind>.<name>"
        )
    cpu, subsystem, kind, *rest = parts
    if cpu not in vocab:
        raise ResolveError(f"{name}: cpu {cpu!r} is not one of {sorted(vocab)}")
    if subsystem not in vocab[cpu]:
        raise ResolveError(
            f"{name}: {subsystem!r} is not a {cpu} subsystem in devices/fw/{SUBSYSTEMS}"
        )
    if kind != table:
        raise ResolveError(
            f"{name}: the kind {kind!r} differs from its table [[{table}]]"
        )
    for segment in rest:
        if not NAME_SEGMENT.fullmatch(segment):
            raise ResolveError(
                f"{name}: segment {segment!r} must be lower case letters, digits and _"
            )


def fw_rules(root: Path) -> list[dict]:
    """The rules of devices/fw/**/*.toml, one dict per device filter, with `_source` = "fw/<file without .toml>".
    Each has the declaration's `was` (former names, never resolved; empty by default)."""
    fw = root / "devices" / "fw"
    vocab = load_subsystems(root)
    former: list[tuple[str, str]] = []  # (a former name, the declaration it was)
    rules = []
    hook_names: set[str] = set()
    for f in sorted(fw.rglob("*.toml")):
        if f == fw / SUBSYSTEMS:
            continue
        source = "fw/" + f.relative_to(fw).with_suffix("").as_posix()
        for table, entries in tomllib.loads(f.read_text()).items():
            if table == "hook":
                # a hook makes no rule; its names take part in the former-name check
                for e in entries:
                    check_name(e["name"], table, vocab)
                    former += [(n, e["name"]) for n in e.get("was", [])]
                    hook_names.add(e["name"])
                continue
            if table in UNRESOLVED_TABLES:
                continue
            if table not in RULE_TABLES:
                raise ResolveError(f"{f}: unknown table [[{table}]]")
            for e in entries:
                check_name(e["name"], table, vocab)
                was = list(e.get("was", []))
                former += [(n, e["name"]) for n in was]
                rule = e["rule"]
                if "kind" in rule:
                    shared = [(e.get("devices"), rule)]
                else:
                    shared = [([dev], r) for dev, r in rule.items()]
                for devices, r in shared:
                    out = {"name": e["name"], "was": was}
                    if "cpu" in e:
                        out["cpu"] = e["cpu"]
                    out.update({k: v for k, v in r.items() if k != "mark"})
                    out["mark"] = r.get("mark", e.get("mark", "D"))
                    if devices:
                        out["devices"] = devices
                    out["_source"] = source
                    rules.append(out)
    current = {r["name"] for r in rules} | hook_names
    for n, name in former:
        if n in current:
            raise ResolveError(f"{n}: a former name of {name} is a current name")
    return rules


def fw_hooks(root: Path) -> list[dict]:
    """The `[[hook]]` entries of devices/fw/**/*.toml: name, site, offset (default 0), mode, cpu, devices (when the entry has
    them) and `_source` as in fw_rules."""
    fw = root / "devices" / "fw"
    hooks = []
    for f in sorted(fw.rglob("*.toml")):
        if f == fw / SUBSYSTEMS:
            continue
        source = "fw/" + f.relative_to(fw).with_suffix("").as_posix()
        for e in tomllib.loads(f.read_text()).get("hook", []):
            if e["mode"] not in HOOK_MODES:
                raise ResolveError(
                    f"{e['name']}: mode {e['mode']!r} is not one of {list(HOOK_MODES)}"
                )
            hook = {
                "name": e["name"],
                "site": e["site"],
                "offset": int(e.get("offset", 0)),
                "mode": e["mode"],
                "cpu": e.get("cpu", "cf" if e["name"].startswith("cf.") else "sharc"),
            }
            if "devices" in e:
                hook["devices"] = e["devices"]
            hook["_source"] = source
            hooks.append(hook)
    return hooks


def sections_of(syx: Path, only: tuple[int, ...] | None = None) -> dict[int, bytes]:
    """The depacked sections of SYX; with ONLY, just those section ids (depacking is the slow part)."""
    from tools.firmware.container import sections
    from tools.firmware.elz import depack_section
    from tools.firmware.extract import classify

    c, table = sections(str(syx))
    out = {}
    for sid, off, clen, _dest in table:
        if only is not None and sid not in only:
            continue
        kind, payload = classify(bytes(c[off : off + clen]))
        out[sid] = depack_section(payload) if kind == "packed" else bytes(payload)
    return out


class SharcFuncs:
    """The functions of one DSP image, from its sharcdb."""

    def __init__(self, image: str):
        from tools.sharc_dev import sharc

        self.db = sharc.load(image)
        self.image = image

    def funcs_by_hash(self, reloc_hash: str) -> list[int]:
        """Entries of the functions whose relocation-insensitive hash is RELOC_HASH, in address order."""
        rows = self.db.sql(
            "SELECT entry_sw FROM func_hash WHERE image=? AND reloc_hash=? ORDER BY entry_sw",
            self.image,
            reloc_hash,
        )
        return [int(r[0]) for r in rows]

    def func_by_hash(self, reloc_hash: str, ordinal: int | None = None) -> int:
        return select_by_ordinal(
            self.funcs_by_hash(reloc_hash),
            ordinal,
            f"reloc_hash {reloc_hash[:12]}..",
            self.image,
        )

    def func_at(self, pc: int) -> tuple[int, str] | None:
        """(entry, reloc_hash) of the function that contains PC, or None."""
        rows = self.db.sql(
            "SELECT f.entry_sw, h.reloc_hash FROM functions f JOIN func_hash h"
            " ON h.image=f.image AND h.entry_sw=f.entry_sw"
            " WHERE f.image=? AND f.entry_sw<=? AND f.end_sw>?",
            self.image,
            pc,
            pc,
        )
        return (int(rows[0][0]), rows[0][1]) if rows else None


class Dsp(SharcFuncs):
    """The DSP boot stream plus its sharcdb."""

    def __init__(self, blob: bytes, image: str):
        from tools.patchkit import dsp_patch

        super().__init__(image)
        self.stream = dsp_patch.Stream(blob)
        self._dsp_patch = dsp_patch

    def byte_addr(self, pc: int) -> int:
        return self._dsp_patch.pc_address(pc, self.stream.present)


def select_by_ordinal(
    entries: list[int], ordinal: int | None, what: str, where: str
) -> int:
    """The function to use among ENTRIES (address order): the ORDINAL-th, or the only one when ORDINAL is None."""
    if ordinal is None:
        if len(entries) != 1:
            raise ResolveError(f"{what} matches {len(entries)} functions in {where}")
        return entries[0]
    if not 0 <= ordinal < len(entries):
        raise ResolveError(
            f"{what} matches {len(entries)} function(s) in {where} (ordinal {ordinal})"
        )
    return entries[ordinal]


class CfFacts:
    """Reset-path facts of one MAIN OS image (tools/coldfire/regions.py)."""

    def __init__(self, name: str, main: bytes):
        from tools.coldfire import regions

        self.name = name
        self.main = main
        self.base = regions.MAIN_BASE

    @functools.cached_property
    def analysis(self) -> dict:
        """The region analysis of the image (slow: it scans the whole image); only the reset and cave rules need it."""
        from tools.coldfire import regions

        return regions.analyse(self.name)

    @functools.cached_property
    def reset(self) -> dict:
        return self.analysis["reset"]

    @functools.cached_property
    def db(self):
        from tools.coldfire import cf

        return cf.load(f"{self.name}-emac").db

    def bytes_at(self, addr: int, n: int) -> bytes:
        return self.main[addr - self.base : addr - self.base + n]

    def funcs_by_hash(self, h: str) -> list[int]:
        """Entries of the functions whose cfdb h_strict is H, in address order."""
        image = f"{self.name}-emac"
        return [
            r[0]
            for r in self.db.execute(
                "SELECT entry FROM func_hash WHERE image=? AND h_strict=? ORDER BY entry",
                (image, h),
            )
        ]

    def func_at(self, addr: int) -> tuple[int, str | None] | None:
        """(entry, h_strict) of the cfdb function that contains ADDR (h_strict None without a hash), or None."""
        image = f"{self.name}-emac"
        db = self.db
        row = db.execute(
            "SELECT entry FROM functions WHERE image=? AND entry<=? AND end>?",
            (image, addr, addr),
        ).fetchone()
        if row is None:
            return None
        h = db.execute(
            "SELECT h_strict FROM func_hash WHERE image=? AND entry=?",
            (image, row[0]),
        ).fetchone()
        return (int(row[0]), h[0] if h else None)

    def vtable_slot(self, cls: str, slot: int, offset_to_top: int = 0) -> list[int]:
        """Targets of the cfdb vtables rows for CLS, OFFSET_TO_TOP and SLOT."""
        image = f"{self.name}-emac"
        return [
            int(r[0])
            for r in self.db.execute(
                "SELECT target FROM vtables WHERE image=? AND class=? AND offset_to_top=? AND slot=?",
                (image, cls, offset_to_top, slot),
            )
        ]

    def own_vtable_slots(self, entry: int) -> list[tuple[str, int, int]]:
        """(class, offset_to_top, slot) of the vtables rows that point at ENTRY and whose owner is the class itself."""
        image = f"{self.name}-emac"
        return [
            (r[0], int(r[1]), int(r[2]))
            for r in self.db.execute(
                "SELECT class, offset_to_top, slot FROM vtables WHERE image=? AND target=? AND owner=class",
                (image, entry),
            )
        ]

    def cave(self) -> dict:
        return next(
            c
            for c in self.analysis["flash_caves"]
            if c["keep"] and c["ends_at_clear_start"]
        )


def resolve_cf_words(cf: CfFacts, rule: dict) -> dict:
    """A table row found by a masked signature: `words` are big-endian u32 values (or "*" for any), matched
    at 4-byte alignment over the whole MAIN OS image; exactly one match is required."""
    import struct

    words = rule["words"]
    n = len(words)
    want = [None if w == "*" else int(w) for w in words]
    first = next(i for i, w in enumerate(want) if w is not None)
    anchor_bytes = struct.pack(">I", want[first])
    hits = []
    i = cf.main.find(anchor_bytes)
    while i >= 0:
        row = i - 4 * first
        if row >= 0 and row % 4 == 0 and row + 4 * n <= len(cf.main):
            got = struct.unpack(f">{n}I", cf.main[row : row + 4 * n])
            if all(w is None or w == g for w, g in zip(want, got, strict=True)):
                hits.append(row)
        i = cf.main.find(anchor_bytes, i + 1)
    if len(hits) != 1:
        raise ResolveError(f"{rule['name']}: {len(hits)} rows match the signature")
    row = hits[0]
    if rule.get("as") == "code":
        # a code anchor inside the matched words (for example the `jsr` of a call site): `offset` bytes from the row, `len` bytes
        addr = cf.base + row + int(rule.get("offset", 0))
        n = int(rule["len"])
        return {
            "kind": "code",
            "addr": addr,
            "len": n,
            "sha256": sha(cf.bytes_at(addr, n)),
        }
    return {
        "kind": "data",
        "addr": cf.base + row,
        "len": 4 * n,
        "sha256": sha(cf.main[row : row + 4 * n]),
    }


PARAM_ROW = 60
# `move.l 4(sp),D0; cmpi.l #count,D0; scs D1b; lea table,A0; mvs.b D1b,D1; and.l D1,D0; move.l D0,D1; lsl.l #2,D1;
# lsl.l #6,D0; sub.l D1,D0; move.l (4,A0,D0*1),D0 ...`: the reader of the mirror index (finding 02)
_PARAM_READER = re.compile(
    rb"\x20\x2f\x00\x04\x0c\x80\x00\x00(..)\x55\xc1\x41\xf9(....)\x73\x01\xc0\x81\x22\x00\xe5\x89"
    rb"\xed\x88\x90\x81\x20\x30\x08\x04\x4e\x75",
    re.S,
)


def param_table(cf: CfFacts, rule: dict) -> tuple[int, int]:
    """(table address, row count) from the reader's signature."""
    hits = list(_PARAM_READER.finditer(cf.main))
    if len(hits) != 1:
        raise ResolveError(f"{rule['name']}: {len(hits)} parameter table readers match")
    m = hits[0]
    return int.from_bytes(m.group(2), "big"), int.from_bytes(m.group(1), "big")


def scan_param_sites(
    main: bytes, main_base: int, t1: int, count: int
) -> tuple[list[int], list[tuple[int, int]]]:
    """The same scan as `params::scan` in packages/patchkit: references into the table and clamp bounds."""
    import numpy as np

    end = min(t1 - main_base, len(main))
    hi = t1 + count * PARAM_ROW
    refs: list[int] = []
    bounds: list[tuple[int, int]] = []
    for start in (0, 2):  # 2-aligned words: two 4-aligned passes
        n = (end - start) // 4
        w = np.frombuffer(main, dtype=">u4", count=n, offset=start)
        for i in np.nonzero((w >= t1) & (w < hi))[0]:
            refs.append(main_base + start + 4 * int(i))
    refs.sort()
    ops = np.frombuffer(main, dtype=">u2", count=end // 2)
    for i in np.nonzero(((ops & 0xFFF8) == 0x0C80) | ((ops & 0xF1FF) == 0xB1FC))[0]:
        o = 2 * int(i)
        if o + 7 > end:
            continue
        imm = int.from_bytes(main[o + 2 : o + 6], "big")
        if imm in (count, count - 1) and main[o + 6] in (0x54, 0x55, 0x62, 0x63):
            bounds.append((main_base + o, imm))
    return refs, bounds


def params_sites_digest(refs: list[int], bounds: list[tuple[int, int]]) -> str:
    b = b"".join(r.to_bytes(4, "big") for r in refs) + b"\xff" * 4
    b += b"".join(a.to_bytes(4, "big") + i.to_bytes(4, "big") for a, i in bounds)
    return sha(b)


# Multi-site rules: one rule, many anchors `<name>.0 .. <name>.N-1` (patchkit reads them from the lockfile and a
# `write` op with `anchors = "<name>.*"` visits each; no scan runs at patch time).
MULTI_KINDS = ("cf_params_refs", "cf_params_bounds")


def resolve_cf_multi(cf: CfFacts, rule: dict, known: dict[str, dict]) -> list[dict]:
    """The sites of `params::scan`, one anchor each, in address order.

    `cf_params_refs`: every 2-aligned big-endian word that points into the first parameter table (4-byte data
    anchors; the operand of the instruction or the table word itself). `cf_params_bounds`: every compare of the
    row count that clamps an index (7-byte code anchors: opcode, immediate, next opcode byte; the immediate is
    `image.u32(this + 2)`). `exclude_anchors = [...]` drops sites inside those (already resolved) anchors, for
    code the module replaces whole.
    """
    t1, count = param_table(cf, rule)
    refs, bounds = scan_param_sites(cf.main, cf.base, t1, count)
    skip = []
    for n in rule.get("exclude_anchors", []):
        if n not in known:
            raise ResolveError(
                f"{rule['name']}: exclude_anchors names {n}, which no earlier rule resolves"
            )
        skip.append((known[n]["addr"], known[n]["addr"] + known[n]["len"]))

    def kept(addr: int) -> bool:
        return not any(lo <= addr < hi for lo, hi in skip)

    if rule["kind"] == "cf_params_refs":
        sites = [(r, 4) for r in refs if kept(r)]
        kind = "data"
    else:
        sites = [(a, 7) for a, _ in bounds if kept(a)]
        kind = "code"
    return [
        {"kind": kind, "addr": a, "len": n, "sha256": sha(cf.bytes_at(a, n))}
        for a, n in sorted(sites)
    ]


def cf_func_addr(cf: CfFacts, rule: dict) -> int:
    """Address `offset` bytes into the function whose cfdb h_strict hash is `hash` (`ordinal` among equals)."""
    hits = cf.funcs_by_hash(rule["hash"])
    ordinal = int(rule.get("ordinal", 0))
    if ordinal >= len(hits):
        raise ResolveError(
            f"{rule['name']}: {len(hits)} function(s) have hash {rule['hash'][:12]}.. (ordinal {ordinal})"
        )
    if "ordinal" not in rule and len(hits) != 1:
        raise ResolveError(
            f"{rule['name']}: {len(hits)} functions share hash {rule['hash'][:12]}.. and the rule gives no ordinal"
        )
    return hits[ordinal] + int(rule["offset"])


def pick_vtable_target(targets: list[int], what: str, where: str) -> int:
    """The one target among the vtables rows TARGETS for WHAT; none or several is an error."""
    if len(targets) != 1:
        raise ResolveError(f"{what} matches {len(targets)} vtables rows in {where}")
    return targets[0]


def cf_vtable_addr(cf: CfFacts, rule: dict) -> int:
    """Address `offset` bytes into the function in slot `slot` of the vtable of `class` (at `offset_to_top`)."""
    ott = int(rule.get("offset_to_top", 0))
    target = pick_vtable_target(
        cf.vtable_slot(rule["class"], int(rule["slot"]), ott),
        f"{rule['name']}: class {rule['class']} slot {rule['slot']} offset_to_top {ott}",
        cf.name,
    )
    return target + int(rule.get("offset", 0))


def resolve_cf_anchor(cf: CfFacts, rule: dict) -> dict:
    kind = rule["kind"]
    if kind == "cf_params_t1":
        t1, count = param_table(cf, rule)
        n = count * PARAM_ROW
        return {"kind": "data", "addr": t1, "len": n, "sha256": sha(cf.bytes_at(t1, n))}
    if kind == "cf_params_sites":
        t1, count = param_table(cf, rule)
        refs, bounds = scan_param_sites(cf.main, cf.base, t1, count)
        return {
            "kind": "sites",
            "addr": t1,
            "len": len(refs) + len(bounds),
            "sha256": params_sites_digest(refs, bounds),
        }
    if kind == "cf_words":
        return resolve_cf_words(cf, rule)
    if kind == "cf_ptr":
        # a table the code reaches through an absolute 32-bit literal: the anchor is the table (`len` bytes of it are
        # hashed), `src` the literal at entry + offset of the function `hash`
        src = cf_func_addr(cf, rule)
        target = int.from_bytes(cf.bytes_at(src, 4), "big")
        n = int(rule["len"])
        if not cf.base <= target < cf.base + len(cf.main) - n:
            raise ResolveError(
                f"{rule['name']}: the literal {target:#x} is not an address in the MAIN OS image"
            )
        return {
            "kind": "data",
            "addr": target,
            "len": n,
            "src": src,
            "sha256": sha(cf.bytes_at(target, n)),
        }
    if kind in ("cf_func", "cf_vtable_slot"):
        addr = cf_func_addr(cf, rule) if kind == "cf_func" else cf_vtable_addr(cf, rule)
        n = int(rule["len"])
        return {
            "kind": "code",
            "addr": addr,
            "len": n,
            "sha256": sha(cf.bytes_at(addr, n)),
        }
    hook = cf.reset["calls"]["copy"]
    if cf.reset["calls"]["clear"] != hook + 4:
        raise ResolveError("the reset calls are not adjacent")
    code = cf.bytes_at(hook, 8)
    if code[:2] != b"\x4e\xba" or code[4:6] != b"\x4e\xba":
        raise ResolveError("the reset calls are not two jsr d16(pc)")
    if kind == "cf_reset_init_calls":
        return {"kind": "code", "addr": hook, "len": 8, "sha256": sha(code)}
    if kind == "cf_reset_helper":
        which = rule["which"]
        return {
            "kind": "value",
            "addr": cf.reset[f"{which}_helper"],
            "src": hook,
            "len": 8,
            "sha256": sha(code),
        }
    raise ResolveError(f"{rule['name']}: unknown rule kind {kind!r}")


def rule_ordinal(rule: dict) -> int | None:
    return int(rule["ordinal"]) if "ordinal" in rule else None


def resolve_anchor(dsp: Dsp, rule: dict) -> dict:
    kind = rule["kind"]
    if kind in ("sharc_func", "sharc_literal"):
        entry = dsp.func_by_hash(rule["reloc_hash"], rule_ordinal(rule))
        pc = entry + int(rule.get("offset", 0))
        addr = dsp.byte_addr(pc)
        if kind == "sharc_func":
            n = int(rule["len"])
            data = dsp.stream.read(addr, n)
            return {
                "kind": "code",
                "addr": addr,
                "pm": pc,
                "len": n,
                "sha256": sha(data),
            }
        rows = dsp.db.sql(
            "SELECT value FROM literals WHERE image=? AND sw=?", dsp.image, pc
        )
        if len(rows) != 1:
            raise ResolveError(
                f"{rule['name']}: {len(rows)} literals at pc {pc:#x} in {dsp.image}"
            )
        width = dsp.db.sql(
            "SELECT width FROM insn WHERE image=? AND sw=?", dsp.image, pc
        )[0][0]
        data = dsp.stream.read(addr, int(width))
        return {
            "kind": "value",
            # a literal is a 32-bit immediate: the DSP's DDR addresses (0x8422b9c8) read as negative in the database
            "addr": int(rows[0][0]) & 0xFFFF_FFFF,
            "src": addr,
            "len": int(width),
            "sha256": sha(data),
        }
    if kind == "sharc_table":
        # a table the code reaches through a literal: the instruction at entry + offset loads its address; the anchor is
        # the table in the DSP stream (`len` bytes, at the software alias 0x28000000 + the literal)
        entry = dsp.func_by_hash(rule["reloc_hash"], rule_ordinal(rule))
        pc = entry + int(rule.get("offset", 0))
        rows = dsp.db.sql(
            "SELECT value FROM literals WHERE image=? AND sw=?", dsp.image, pc
        )
        if len(rows) != 1:
            raise ResolveError(
                f"{rule['name']}: {len(rows)} literals at pc {pc:#x} in {dsp.image}"
            )
        addr = 0x2800_0000 + int(rows[0][0])
        n = int(rule["len"])
        return {
            "kind": "data",
            "addr": addr,
            "len": n,
            "src": dsp.byte_addr(pc),
            "sha256": sha(dsp.stream.read(addr, n)),
        }
    if kind == "sharc_mem_addr":
        entry = dsp.func_by_hash(rule["reloc_hash"], rule_ordinal(rule))
        pc = entry + int(rule.get("offset", 0))
        rows = dsp.db.sql(
            "SELECT abs_address FROM mem_access WHERE image=? AND sw=? AND abs_address IS NOT NULL",
            dsp.image,
            pc,
        )
        if len(rows) != 1:
            raise ResolveError(
                f"{rule['name']}: {len(rows)} direct DM accesses at pc {pc:#x} in {dsp.image}"
            )
        width = dsp.db.sql(
            "SELECT width FROM insn WHERE image=? AND sw=?", dsp.image, pc
        )[0][0]
        addr = dsp.byte_addr(pc)
        data = dsp.stream.read(addr, int(width))
        return {
            "kind": "value",
            "addr": int(rows[0][0]),
            "src": addr,
            "len": int(width),
            "sha256": sha(data),
        }
    raise ResolveError(f"{rule['name']}: unknown rule kind {kind!r}")


def expansion_excludes(excludes: list, start: int, end: int) -> list:
    """The firmware entry's `cf_expansion_excludes`, checked: inside the region, each non-empty, in order, not overlapping."""
    out = sorted([int(a), int(b)] for a, b in excludes)
    for i, (a, b) in enumerate(out):
        if not start <= a < b <= end:
            raise ResolveError(
                f"cf_expansion_excludes [{a:#x}, {b:#x}] is not inside cf.expansion [{start:#x}, {end:#x}]"
            )
        if i and a < out[i - 1][1]:
            raise ResolveError(
                f"cf_expansion_excludes [{a:#x}, {b:#x}] overlaps the range before it"
            )
    return out


def dsp_l2_region(stream, patch: dict) -> dict:
    lo, hi = patch["dsp_l2_lo"], patch["dsp_l2_hi"]
    end = max(
        (b["target_address"] + b["byte_count"])
        for b in stream.mem.blocks
        if lo <= b["target_address"] < hi
    )
    start = (end + 3) // 4 * 4
    return {
        "name": "dsp.l2",
        "cpu": "sharc",
        "start": start,
        "end": hi,
        "excludes": patch.get("dsp_l2_excludes", []),
        "mark": "D",
    }


def toml_int(v: int) -> str:
    return f"0x{v:x}" if v >= 10 else str(v)


def render(
    port: dict,
    limits: dict,
    regions: list[dict],
    anchors: list[dict],
    hooks: list[dict],
) -> str:
    """The port file text: header, limits, regions, the anchors and then the hooks (both sorted by name)."""
    o = [
        "# Generated by tools/patchkit/port_resolve.py: do not edit by hand. Addresses and sha256 of bytes only.",
        "",
        "[port]",
    ]
    for k in (
        "device",
        "version",
        "syx_sha256",
        "main_sha256",
        "dsp_sha256",
        "resolved_by",
    ):
        o.append(f'{k} = "{port[k]}"')
    o += ["", "[limits]"]
    for k, v in limits.items():
        o.append(f"{k} = {toml_int(v)}")
    for r in regions:
        o += ["", "[[region]]", f'name = "{r["name"]}"', f'cpu = "{r["cpu"]}"']
        o += [f"start = {toml_int(r['start'])}", f"end = {toml_int(r['end'])}"]
        ex = ", ".join(f"[{toml_int(a)}, {toml_int(b)}]" for a, b in r["excludes"])
        o.append(f"excludes = [{ex}]")
        o.append(f'mark = "{r["mark"]}"')
    for a in sorted(anchors, key=lambda x: x["name"]):
        o += ["", "[[anchor]]", f'name = "{a["name"]}"', f'cpu = "{a["cpu"]}"']
        o.append(f'kind = "{a["kind"]}"')
        o.append(f"addr = {toml_int(a['addr'])}")
        if "pm" in a:
            o.append(f"pm = {toml_int(a['pm'])}")
        if "src" in a:
            o.append(f"src = {toml_int(a['src'])}")
        o.append(f"len = {a['len']}")
        o.append(f'sha256 = "{a["sha256"]}"')
        o.append(f'mark = "{a["mark"]}"')
        o.append(f'rule = "{a["rule"]}"')
    for h in sorted(hooks, key=lambda x: x["name"]):
        o += ["", "[[hook]]", f'name = "{h["name"]}"', f'site = "{h["site"]}"']
        if h["offset"]:
            o.append(f"offset = {toml_int(h['offset'])}")
        o.append(f'mode = "{h["mode"]}"')
    return "\n".join(o) + "\n"


def resolve(syx: Path, root: Path = ROOT) -> tuple[str, str]:
    """(port name, port file text) of SYX: its anchors, then the hooks whose site resolves on its device."""
    raw = syx.read_bytes()
    digest = sha(raw)
    prof = load_profiles(root).get(digest)
    if prof is None:
        raise ResolveError(f"{syx}: not a firmware listed in devices/*.toml ({digest})")
    secs = sections_of(syx)
    device, version = prof["device"], prof["version"]
    dsp = Dsp(secs[7], prof["dsp"])
    cf = CfFacts(f"{device}-{version}", secs[3])
    anchors = []
    pending = []
    for rule in fw_rules(root):
        if rule.get("devices") and device not in rule["devices"]:
            continue
        if rule.get("kind") in MULTI_KINDS:
            pending.append(rule)  # after the single anchors they may exclude
            continue
        a = (
            resolve_cf_anchor(cf, rule)
            if rule.get("cpu") == "cf"
            else resolve_anchor(dsp, rule)
        )
        anchor = {
            "name": rule["name"],
            "cpu": rule.get("cpu", "sharc"),
            "mark": rule.get("mark", "D"),
            "rule": f"{rule['_source']}:{rule['kind']}",
            **a,
        }
        anchors.append(anchor)
    known = {a["name"]: a for a in anchors}
    for rule in pending:
        for i, a in enumerate(resolve_cf_multi(cf, rule, known)):
            anchor = {
                "name": f"{rule['name']}.{i}",
                "cpu": "cf",
                "mark": rule.get("mark", "D"),
                "rule": f"{rule['_source']}:{rule['kind']}",
                **a,
            }
            anchors.append(anchor)
    # one anchor name may be declared more than once; the resolved anchors must agree
    seen: dict[str, dict] = {}
    unique = []
    for a in anchors:
        prev = seen.get(a["name"])
        if prev is None:
            seen[a["name"]] = a
            unique.append(a)
        elif {k: v for k, v in prev.items() if k != "rule"} != {
            k: v for k, v in a.items() if k != "rule"
        }:
            raise ResolveError(f"anchor {a['name']} resolves differently for two rules")
    # Built-in anchor: the whole MAIN OS image, for modules that write at fixed offsets of one pinned firmware.
    main_dest = prof["patch"]["main_dest"]
    anchors.append(
        {
            "name": "cf.main",
            "cpu": "cf",
            "kind": "code",
            "addr": main_dest,
            "len": len(secs[3]),
            "sha256": sha(secs[3]),
            "mark": "V",
            "rule": "builtin",
        }
    )
    unique.append(anchors[-1])
    by_name = {a["name"]: a for a in unique}
    hooks = []
    for h in fw_hooks(root):
        if h.get("devices") and device not in h["devices"]:
            continue
        site = by_name.get(h["site"])
        if site is None:
            raise ResolveError(
                f"{h['name']}: site {h['site']} does not resolve on {device}"
            )
        if site["cpu"] != h["cpu"]:
            raise ResolveError(
                f"{h['name']}: cpu {h['cpu']} differs from the cpu {site['cpu']} of site {h['site']}"
            )
        hooks.append({k: h[k] for k in ("name", "site", "offset", "mode")})
    cave = cf.cave()
    exp = cf.analysis["expansion"]
    regions = [
        {
            "name": "cf.flash_cave",
            "cpu": "cf",
            "start": cave["start"],
            "end": cave["start"] + cave["size"],
            "excludes": [],
            "mark": "D",
        },
        {
            "name": "cf.expansion",
            "cpu": "cf",
            "start": exp["start"],
            "end": exp["end"],
            "excludes": expansion_excludes(
                prof["cf_expansion_excludes"], exp["start"], exp["end"]
            ),
            "mark": "D",
        },
        dsp_l2_region(dsp.stream, prof["patch"]),
    ]
    port = {
        "device": device,
        "version": version,
        "syx_sha256": digest,
        "main_sha256": sha(secs[3]),
        "dsp_sha256": sha(secs[7]),
        "resolved_by": RESOLVED_BY,
    }
    limits = {k: prof["patch"][k] for k in LIMIT_KEYS}
    return f"{device}-{version}", render(port, limits, regions, unique, hooks)


def stock_profile(name: str, root: Path = ROOT) -> tuple[Path, dict]:
    """(stock file, profile) of the firmware called NAME (`<device>-<version>`, e.g. "dt2-1.17"), found through the
    `[[firmware]]` entries of devices/*.toml."""
    for prof in load_profiles(root).values():
        if f"{prof['device']}-{prof['version']}" == name:
            f = prof["filename"]
            return (Path(f) if Path(f).exists() else config.stock(f)), prof
    raise ResolveError(f"{name}: not a firmware listed in devices/*.toml")


@functools.cache
def load_cf(name: str, root: Path = ROOT) -> CfFacts:
    """The ColdFire facts of the stock firmware NAME; depacks only the MAIN OS section."""
    syx, _prof = stock_profile(name, root)
    return CfFacts(name, sections_of(syx, (3,))[3])


@functools.cache
def load_dsp(name: str, root: Path = ROOT) -> Dsp:
    """The DSP of the stock firmware NAME; depacks only the DSP section."""
    syx, prof = stock_profile(name, root)
    return Dsp(sections_of(syx, (7,))[7], prof["dsp"])


def load_image(name: str, root: Path = ROOT) -> tuple[CfFacts, Dsp]:
    """The ColdFire facts and the DSP of the stock firmware called NAME."""
    return load_cf(name, root), load_dsp(name, root)


def cf_rule_at(cf: CfFacts, addr: int, length: int) -> dict:
    """A `cf_func` rule for the LENGTH bytes at ADDR: the cfdb function that contains ADDR, found by its h_strict
    hash (with `ordinal` when other functions share the hash) and ADDR's offset from the entry. When more than
    MAX_SAME_HASH functions share the hash, a `cf_vtable_slot` rule on a class whose own virtual function it is."""
    found = cf.func_at(addr)
    if found is None:
        raise ResolveError(f"no function contains {addr:#x} in {cf.name}")
    entry, h = found
    if h is None:
        raise ResolveError(f"the function at {entry:#x} in {cf.name} has no hash")
    hits = cf.funcs_by_hash(h)
    from tools.firmware.catalog import MAX_SAME_HASH

    if len(hits) > MAX_SAME_HASH:
        own = sorted(
            cf.own_vtable_slots(entry), key=lambda s: (s[1] != 0, abs(s[1]), s[0])
        )
        if not own:
            raise ResolveError(
                f"the function at {entry:#x} in {cf.name}: hash shared by {len(hits)} functions"
                " and not a class's own virtual function"
            )
        cls, ott, slot = own[0]
        rule = {"kind": "cf_vtable_slot", "class": cls, "slot": slot}
        if ott:
            rule["offset_to_top"] = ott
        rule["offset"] = addr - entry
        rule["len"] = length
        return rule
    rule = {"kind": "cf_func", "hash": h}
    if len(hits) > 1:
        rule["ordinal"] = hits.index(entry)
    rule["offset"] = addr - entry
    rule["len"] = length
    return rule


def sharc_rule_at(dsp: SharcFuncs, pc: int, length: int) -> dict:
    """A `sharc_func` rule for the LENGTH bytes at the pc PC: the function that contains PC, found by its reloc_hash
    (with `ordinal` when other functions share it) and PC's offset in parcels from the entry. LENGTH 0 omits `len`."""
    found = dsp.func_at(pc)
    if found is None:
        raise ResolveError(f"no function contains pc {pc:#x} in {dsp.image}")
    entry, h = found
    rule: dict = {"kind": "sharc_func", "reloc_hash": h}
    hits = dsp.funcs_by_hash(h)
    if len(hits) > 1:
        rule["ordinal"] = hits.index(entry)
    rule["offset"] = pc - entry
    if length:
        rule["len"] = length
    return rule


def check_rule(image_name: str, rule: dict, cpu: str, root: Path = ROOT) -> int:
    """Resolve RULE on the stock image IMAGE_NAME and return the address it finds: the entry + offset for a ColdFire
    rule, the pc for a SHARC `sharc_func` rule (any other SHARC kind: the resolved address). Raises ResolveError."""
    named = {"name": "rule", **rule}
    if cpu == "cf":
        return resolve_cf_anchor(load_cf(image_name, root), named)["addr"]
    a = resolve_anchor(load_dsp(image_name, root), named)
    return a.get("pm", a["addr"])


def format_rule(rule: dict) -> str:
    """RULE as a TOML inline table: offset in hex, other integers in decimal."""
    parts = []
    for k, v in rule.items():
        if isinstance(v, str):
            parts.append(f'{k} = "{v}"')
        elif k == "offset":
            parts.append(f"{k} = {v:#x}")
        else:
            parts.append(f"{k} = {v}")
    return "{ " + ", ".join(parts) + " }"


def cmd_rule_sharc(a: argparse.Namespace) -> int:
    dsp = SharcFuncs(a.image)
    rule = sharc_rule_at(dsp, a.pc, a.len or 0)
    n = len(dsp.funcs_by_hash(rule["reloc_hash"]))
    entry = a.pc - rule["offset"]
    print(f"# {a.image}: function at {entry:#x}; {n} function(s) share this hash")
    print(f"rule = {format_rule(rule)}")
    return 0


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    sub = ap.add_subparsers(dest="cmd", required=True)
    r = sub.add_parser("resolve")
    r.add_argument("syx", type=Path)
    r.add_argument("--out", type=Path)
    r.add_argument("--root", type=Path, default=ROOT)
    ra = sub.add_parser("resolve-all")
    ra.add_argument("--root", type=Path, default=ROOT)
    rs = sub.add_parser("rule-sharc")
    rs.add_argument("image")
    rs.add_argument("pc", type=lambda s: int(s, 0))
    rs.add_argument("--len", type=int)
    argv = sys.argv[1:]
    # `mise run patch:port [-- X.syx]`: a firmware resolves that one, nothing resolves them all
    if not argv:
        argv = ["resolve-all"]
    elif argv[0] not in ("resolve", "resolve-all", "rule-sharc") and not argv[
        0
    ].startswith("-"):
        argv = ["resolve", *argv]
    a = ap.parse_args(argv)
    try:
        if a.cmd == "rule-sharc":
            return cmd_rule_sharc(a)
        files = (
            [
                Path(f) if Path(f).exists() else config.stock(f)
                for f in ported_firmware(a.root)
            ]
            if a.cmd == "resolve-all"
            else [a.syx]
        )
        for f in files:
            name, text = resolve(f, a.root)
            out = (
                a.out
                if getattr(a, "out", None)
                else a.root / "devices" / "ports" / f"{name}.toml"
            )
            out.parent.mkdir(parents=True, exist_ok=True)
            out.write_text(text)
            print(f"{out}: {text.count('[[anchor]]')} anchors")
    except ResolveError as e:
        print(f"port_resolve: {e}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
