"""Private persistent static index for the SHARC discovery tools.

This is intentionally a narrow façade: callers receive canonical records, not
SQLite rows.  The database is a cache of loader-derived facts and is never an
alternate decoder or source of semantic claims.
"""
from __future__ import annotations

import hashlib
import json
import os
import sqlite3
import tempfile
from dataclasses import dataclass
from pathlib import Path
from typing import Any, Mapping, Sequence

HERE = Path(__file__).resolve().parent
SCHEMA_VERSION = 2
DEPENDENCIES = (
    "sharc_index.py", "sharcldr.py", "sharc_disasm.py", "sharcinv.py",
    "sharcflow.py", "sharcfn.py", "sharc_trace.py", "sharcwriters.py",
    "sharc_discover.py", "sharcspec/decode_table.json",
)


def _canonical(value: Any) -> str:
    return json.dumps(value, sort_keys=True, separators=(",", ":"), ensure_ascii=True)


def _digest_file(path: Path) -> str:
    return hashlib.sha256(path.read_bytes()).hexdigest()


@dataclass(frozen=True)
class IndexConfig:
    code_blocks: tuple[int, ...]
    min_depth: int


@dataclass(frozen=True)
class WriterTarget:
    address: int
    width: int = 4


@dataclass(frozen=True)
class RegisterEffectQuery:
    entry_sw: int
    register: str
    max_steps: int = 1000
    max_states: int = 64
    max_call_depth: int = 8


def classify_register_paths(paths: Sequence[Mapping[str, Any]]) -> dict[str, Any]:
    """Conservative tri-state reducer used by generated effect queries.

    A path must explicitly establish a verified return and no uncertainty to
    contribute to preservation.  This small pure seam is also useful for
    synthetic tests without firmware.
    """
    writers: set[int] = set()
    for path in paths:
        for pc in path.get("writer_pcs", ()):
            if isinstance(pc, bool) or not isinstance(pc, int):
                raise ValueError("register-effect writer PC must be an integer")
            writers.add(pc)
    writer_pcs = sorted(writers)
    if writer_pcs:
        return {"status": "written", "quantifier": "exists reachable retained path", "writer_pcs": writer_pcs, "reasons": []}
    bad = sorted({str(reason) for path in paths for reason in path.get("uncertainty", ())})
    if paths and not bad and all(path.get("verified_return") for path in paths):
        return {"status": "preserved", "quantifier": "all retained paths", "writer_pcs": [], "reasons": []}
    if not paths:
        bad.append("no retained trace path")
    if any(not path.get("verified_return") for path in paths):
        bad.append("path does not reach verified return")
    return {"status": "unknown", "quantifier": "not established", "writer_pcs": [], "reasons": sorted(set(bad))}


class AnalysisIndex:
    def __init__(self, db_path: Path, metadata: Mapping[str, Any]):
        self._path = db_path
        self._metadata = dict(metadata)

    @staticmethod
    def _fingerprint(blob_path: Path, config: IndexConfig) -> dict[str, Any]:
        blob = blob_path.read_bytes()
        dependencies = {name: _digest_file(HERE / name) for name in DEPENDENCIES if (HERE / name).is_file()}
        return {"schema_version": SCHEMA_VERSION, "blob_sha256": hashlib.sha256(blob).hexdigest(),
                "blob_size": len(blob), "config": {"code_blocks": list(config.code_blocks), "min_depth": config.min_depth},
                "dependencies": dependencies}

    @classmethod
    def open_or_build(cls, blob_path: Path, *, path: Path | None = None,
                      config: IndexConfig, readonly: bool = False) -> "AnalysisIndex":
        blob_path = Path(blob_path)
        expected = cls._fingerprint(blob_path, config)
        cache = Path(path) if path is not None else blob_path.with_suffix(blob_path.suffix + ".sharc-index.sqlite")
        if cache.exists():
            try:
                with sqlite3.connect(f"file:{cache}?mode=ro", uri=True) as con:
                    row = con.execute("select value from metadata where key='fingerprint'").fetchone()
                    complete = con.execute("select value from metadata where key='complete'").fetchone()
                    if row and complete == ("1",) and json.loads(row[0]) == expected:
                        return cls(cache, {**expected, "blob_path": str(blob_path)})
            except (sqlite3.Error, json.JSONDecodeError):
                pass
        if readonly:
            raise ValueError("no complete compatible SHARC index at %s" % cache)
        return cls._build(blob_path, cache, config, expected)

    @classmethod
    def _build(cls, blob_path: Path, cache: Path, config: IndexConfig, fingerprint: Mapping[str, Any]) -> "AnalysisIndex":
        # Import lazily to avoid discovery importing its own cache façade.
        import sharcfn
        import sharcwriters
        import sharc_discover
        ctx = sharcfn.load_context(str(blob_path), config.code_blocks, config.min_depth)
        if not ctx["mem"].has_final_marker():
            raise ValueError("loader stream has no final marker")
        static = sharc_discover.build_static_context(ctx)
        tables, pointer_hits = sharc_discover.find_pointer_runs(ctx, static["instructions"], config.code_blocks)
        functions = sharc_discover._compact_functions(ctx["functions"])
        census, orphan = sharcwriters.full_project_census(ctx)
        memory_sites = []
        for fn_id, rows in census.items():
            for row in rows:
                if row["is_dm"]:
                    memory_sites.append({"function_id": fn_id, **row})
        for row in orphan:
            if row["is_dm"]:
                memory_sites.append({"function_id": None, **row})
        snapshot = {"functions": functions,
                    "instructions": [{"pc_sw": pc_sw, **record} for pc_sw, record in sorted(static["instructions"].items())],
                    "instruction_pcs": sorted(static["instructions"]), "literals": static["literals"],
                    "indirect_sites": static["indirect_sites"], "pointer_runs": tables,
                    "pointer_hits": pointer_hits, "memory_sites": sorted(memory_sites, key=lambda x: (x["pc"], x.get("function_id") or ""))}
        cache.parent.mkdir(parents=True, exist_ok=True)
        fd, temporary = tempfile.mkstemp(prefix=cache.name + ".", suffix=".tmp", dir=cache.parent)
        os.close(fd)
        try:
            with sqlite3.connect(temporary) as con:
                con.executescript("create table metadata(key text primary key, value text not null); create table snapshot(id integer primary key check(id=1), payload text not null);")
                con.execute("insert into metadata values (?,?)", ("fingerprint", _canonical(fingerprint)))
                con.execute("insert into snapshot values (1,?)", (_canonical(snapshot),))
                con.execute("insert into metadata values ('complete','1')")
                con.commit()
            with open(temporary, "rb") as handle:
                os.fsync(handle.fileno())
            os.replace(temporary, cache)
            return cls(cache, {**fingerprint, "blob_path": str(blob_path)})
        finally:
            if os.path.exists(temporary):
                os.unlink(temporary)

    def snapshot(self) -> Mapping[str, Any]:
        with sqlite3.connect(f"file:{self._path}?mode=ro", uri=True) as con:
            row = con.execute("select payload from snapshot where id=1").fetchone()
        if row is None:
            raise ValueError("complete index lacks static snapshot")
        try:
            return json.loads(row[0])
        except json.JSONDecodeError as error:
            raise ValueError("complete index has invalid static snapshot JSON") from error

    def query(self, *, writer_targets: Sequence[WriterTarget] = (),
              register_effects: Sequence[RegisterEffectQuery] = (), jobs: int = 1) -> Mapping[str, Any]:
        import sharcwriters
        results: dict[str, Any] = {"writer_targets": [], "register_effects": []}
        if writer_targets:
            config = self._metadata["config"]
            targets = [(item.address, item.width) for item in writer_targets]
            # The blob path is recorded only in this process-local instance.
            # A query needs an explicit source path; index DB deliberately has
            # no absolute paths in identity, so retain it in metadata object.
            blob_path = self._metadata.get("blob_path")
            if blob_path is None:
                raise ValueError("writer queries require an index opened in this process")
            out = sharcwriters.run_many(blob_path, tuple(config["code_blocks"]), config["min_depth"], targets,
                                        4000, 128, 4, sharcwriters.DEFAULT_STACK_LO, sharcwriters.DEFAULT_STACK_HI, jobs=jobs)
            results["writer_targets"] = [out[(address, width)] for address, width in sorted(set(targets))]
        # Effects are generated from tracer events only.  Unknown is the
        # default: a trace is not allowed to turn an incomplete search into a
        # preservation claim.
        if register_effects:
            import sharcfn
            import sharc_trace
            blob_path = self._metadata.get("blob_path")
            if blob_path is None:
                raise ValueError("register-effect queries require an index opened in this process")
            config = self._metadata["config"]
            context = sharcfn.load_context(blob_path, tuple(config["code_blocks"]), config["min_depth"])
            for item in sorted(register_effects, key=lambda q: (q.entry_sw, q.register)):
                if item.entry_sw not in {fn["entry"] for fn in context["functions"]}:
                    effect = classify_register_paths([{"verified_return": False, "uncertainty": ["entry is not a recovered function"]}])
                else:
                    try:
                        states = sharc_trace.trace(context["mem"], None, item.entry_sw,
                            max_steps=item.max_steps, max_states=item.max_states,
                            concrete_memory=True, follow_loaded_calls=True,
                            continue_external_calls=False, max_call_depth=item.max_call_depth)
                    except Exception as error:
                        raise RuntimeError(f"register-effect trace failed at 0x{item.entry_sw:x}") from error
                    paths = []
                    for state in states:
                        writers = []
                        uncertainty = []
                        for event in state.trace:
                            destination = event.get("destination", event.get("ureg"))
                            result = event.get("result_register")
                            if destination == item.register or result == item.register or (isinstance(result, list) and item.register in result):
                                writers.append(event.get("pc_sw", item.entry_sw))
                            if event.get("action") in ("opaque-external-call", "unsupported", "indirect-call"):
                                uncertainty.append(event.get("action"))
                        if state.stopped not in ("return", "returned"):
                            uncertainty.append("trace stop: " + str(state.stopped))
                        paths.append({"verified_return": state.stopped in ("return", "returned"), "writer_pcs": writers, "uncertainty": uncertainty})
                    effect = classify_register_paths(paths)
                results["register_effects"].append({"entry_sw": item.entry_sw, "register": item.register, **effect})
        return results

    def trace(self, probes: Sequence[Mapping[str, Any]], *, jobs: int = 1) -> Mapping[str, Any]:
        # The coordinator owns output.  Discovery supplies the LoadedMemory;
        # this method is intentionally a policy-neutral declaration record.
        try:
            canonical_probes = [json.loads(_canonical(probe)) for probe in sorted(probes, key=_canonical)]
        except (TypeError, ValueError, json.JSONDecodeError) as error:
            raise ValueError("trace probes must be canonical JSON-compatible records") from error
        return {"probes": canonical_probes, "status": "unknown", "reason": "trace requires discovery memory adapter"}
