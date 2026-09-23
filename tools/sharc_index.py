"""Private persistent static index for the SHARC discovery tools.

The public surface deliberately remains small: an immutable static snapshot and
final, conservatively-produced query results.  SQLite rows are cache material,
never independent semantic evidence.
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
DB_SCHEMA_VERSION = 3
CACHE_CONTRACT = "sharc-analysis-index/v3"
WRITER_POLICY = {"max_steps": 4000, "max_states": 128, "fallback_width": 4,
                 "stack_lo": 0x26F000, "stack_hi": 0x2C0000,
                 "seed_global_constants": True}
REGISTER_POLICY = {"concrete_memory": True, "follow_loaded_calls": True,
                   "continue_external_calls": False}
# This is deliberately a register-effect query rule, not a tracer stop-rule
# change: the trace began at a recovered architectural function entry.
REGISTER_EFFECT_VERIFIED_RETURNS = frozenset(("return", "returned", "return without followed call"))

_QUERY_SQL = {
    "writer_results": {
        "select": "SELECT request,payload,payload_sha256,payload_size,complete,static_key FROM writer_results WHERE query_key=?",
        "insert_ignore": "INSERT OR IGNORE INTO writer_results VALUES (?,?,?,?,?,?,1)",
        "delete": "DELETE FROM writer_results WHERE query_key=?",
        "insert": "INSERT INTO writer_results VALUES (?,?,?,?,?,?,1)",
    },
    "register_results": {
        "select": "SELECT request,payload,payload_sha256,payload_size,complete,static_key FROM register_results WHERE query_key=?",
        "insert_ignore": "INSERT OR IGNORE INTO register_results VALUES (?,?,?,?,?,?,1)",
        "delete": "DELETE FROM register_results WHERE query_key=?",
        "insert": "INSERT INTO register_results VALUES (?,?,?,?,?,?,1)",
    },
}


def _canonical(value: Any) -> str:
    return json.dumps(value, sort_keys=True, separators=(",", ":"), ensure_ascii=True)


def _canonical_bytes(value: Any) -> bytes:
    return _canonical(value).encode("ascii")


def _digest_file(path: Path) -> str:
    return hashlib.sha256(path.read_bytes()).hexdigest()


def _payload(value: Mapping[str, Any]) -> bytes:
    return _canonical_bytes(value)


def _valid_json_payload(raw: Any, digest: Any, size: Any) -> Mapping[str, Any] | None:
    """Return a canonical object only when all on-disk integrity checks pass."""
    if not isinstance(raw, bytes) or not isinstance(digest, str) or not isinstance(size, int):
        return None
    if len(raw) != size or hashlib.sha256(raw).hexdigest() != digest:
        return None
    try:
        value = json.loads(raw.decode("ascii"))
    except (UnicodeDecodeError, json.JSONDecodeError):
        return None
    if not isinstance(value, dict) or _payload(value) != raw:
        return None
    return value


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
    """Conservative tri-state reducer used by generated effect queries."""
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
    def __init__(self, db_path: Path, metadata: Mapping[str, Any], readonly: bool = False):
        self._path = Path(db_path)
        self._metadata = dict(metadata)
        self._readonly = readonly
        self._static_key = str(metadata.get("static_key", ""))

    @staticmethod
    def _fingerprint(blob_path: Path, config: IndexConfig) -> dict[str, Any]:
        blob = blob_path.read_bytes()
        dependencies = []
        # Discovery/report composition and query-cache code do not produce the
        # immutable snapshot.  Keeping them out avoids rebuilding static facts for
        # report-only or result-cache changes; sharc_static owns the extracted
        # static builders.
        static_sources = [path for path in HERE.glob("sharc*.py")
                          if path.name not in {
                              "sharc_discover.py", "sharc_index.py", "sharc_selache.py"
                          }]
        for path in sorted([*static_sources, *HERE.joinpath("sharcspec").glob("*.json")],
                           key=lambda item: item.relative_to(HERE).as_posix()):
            if path.is_file():
                dependencies.append({"path": path.relative_to(HERE.parent).as_posix(),
                                     "size": path.stat().st_size, "sha256": _digest_file(path)})
        return {"contract": CACHE_CONTRACT,
                "blob": {"sha256": hashlib.sha256(blob).hexdigest(), "size": len(blob)},
                "config": {"code_blocks": list(config.code_blocks), "min_depth": config.min_depth},
                "dependencies": dependencies}

    @staticmethod
    def _schema(con: sqlite3.Connection) -> None:
        con.executescript("""
            PRAGMA foreign_keys=ON;
            CREATE TABLE cache_meta (key TEXT PRIMARY KEY, value BLOB NOT NULL) WITHOUT ROWID;
            CREATE TABLE static_snapshot (
              id INTEGER PRIMARY KEY CHECK (id = 1), static_key TEXT NOT NULL UNIQUE,
              payload BLOB NOT NULL, payload_sha256 TEXT NOT NULL CHECK(length(payload_sha256)=64),
              payload_size INTEGER NOT NULL CHECK(payload_size>=0));
            CREATE TABLE writer_results (
              query_key TEXT PRIMARY KEY, static_key TEXT NOT NULL, request BLOB NOT NULL, payload BLOB NOT NULL,
              payload_sha256 TEXT NOT NULL CHECK(length(payload_sha256)=64), payload_size INTEGER NOT NULL CHECK(payload_size>=0),
              complete INTEGER NOT NULL CHECK(complete=1), FOREIGN KEY(static_key) REFERENCES static_snapshot(static_key)) WITHOUT ROWID;
            CREATE TABLE register_results (
              query_key TEXT PRIMARY KEY, static_key TEXT NOT NULL, request BLOB NOT NULL, payload BLOB NOT NULL,
              payload_sha256 TEXT NOT NULL CHECK(length(payload_sha256)=64), payload_size INTEGER NOT NULL CHECK(payload_size>=0),
              complete INTEGER NOT NULL CHECK(complete=1), FOREIGN KEY(static_key) REFERENCES static_snapshot(static_key)) WITHOUT ROWID;
            PRAGMA user_version=3;
        """)

    @classmethod
    def _open_complete(cls, cache: Path, fingerprint: Mapping[str, Any]) -> bool:
        try:
            with sqlite3.connect(f"file:{cache}?mode=ro", uri=True) as con:
                con.execute("PRAGMA foreign_keys=ON")
                if con.execute("PRAGMA user_version").fetchone() != (DB_SCHEMA_VERSION,):
                    return False
                rows = dict(con.execute("SELECT key, value FROM cache_meta"))
                expected_key = hashlib.sha256(_canonical_bytes(fingerprint)).hexdigest()
                if set(rows) != {"cache_contract", "fingerprint", "static_key", "complete"} or rows.get("cache_contract") != CACHE_CONTRACT or rows.get("static_key") != expected_key or rows.get("complete") != "1" or rows.get("fingerprint") != _canonical_bytes(fingerprint):
                    return False
                row = con.execute("SELECT static_key,payload,payload_sha256,payload_size FROM static_snapshot WHERE id=1").fetchone()
                return bool(row and row[0] == expected_key and _valid_json_payload(*row[1:]) is not None)
        except (sqlite3.Error, TypeError):
            return False

    @classmethod
    def open_or_build(cls, blob_path: Path, *, path: Path | None = None, config: IndexConfig,
                      readonly: bool = False) -> "AnalysisIndex":
        blob_path = Path(blob_path)
        fingerprint = cls._fingerprint(blob_path, config)
        static_key = hashlib.sha256(_canonical_bytes(fingerprint)).hexdigest()
        cache = Path(path) if path is not None else blob_path.with_suffix(blob_path.suffix + ".sharc-index.sqlite")
        metadata = {**fingerprint, "static_key": static_key, "blob_path": str(blob_path)}
        if cache.exists() and cls._open_complete(cache, fingerprint):
            return cls(cache, metadata, readonly)
        if readonly:
            raise ValueError("no complete compatible SHARC index at %s" % cache)
        return cls._build(blob_path, cache, config, fingerprint)

    @classmethod
    def _make_snapshot(cls, blob_path: Path, config: IndexConfig) -> Mapping[str, Any]:
        # Keep all immutable-payload producers in sharc_static, which is part
        # of the static dependency fingerprint rather than query-cache code.
        from importlib import import_module
        sharc_static = import_module("sharc_static")
        return sharc_static.build_snapshot(str(blob_path), config.code_blocks, config.min_depth)

    @classmethod
    def _build(cls, blob_path: Path, cache: Path, config: IndexConfig, fingerprint: Mapping[str, Any]) -> "AnalysisIndex":
        snapshot = cls._make_snapshot(blob_path, config)
        static_key = hashlib.sha256(_canonical_bytes(fingerprint)).hexdigest()
        raw = _payload(snapshot)
        cache.parent.mkdir(parents=True, exist_ok=True)
        fd, temporary = tempfile.mkstemp(prefix=cache.name + ".", suffix=".tmp", dir=cache.parent)
        os.close(fd)
        try:
            with sqlite3.connect(temporary) as con:
                cls._schema(con)
                con.execute("INSERT INTO static_snapshot VALUES (1,?,?,?,?)", (static_key, raw, hashlib.sha256(raw).hexdigest(), len(raw)))
                con.execute("INSERT INTO cache_meta VALUES (?,?)", ("cache_contract", CACHE_CONTRACT))
                con.execute("INSERT INTO cache_meta VALUES (?,?)", ("fingerprint", _canonical_bytes(fingerprint)))
                con.execute("INSERT INTO cache_meta VALUES (?,?)", ("static_key", static_key))
                con.execute("INSERT INTO cache_meta VALUES (?,?)", ("complete", "1"))
                con.commit()
            with open(temporary, "rb") as handle:
                os.fsync(handle.fileno())
            os.replace(temporary, cache)
            try:
                directory = os.open(cache.parent, os.O_RDONLY)
                try: os.fsync(directory)
                finally: os.close(directory)
            except OSError:
                pass
            return cls(cache, {**fingerprint, "static_key": static_key, "blob_path": str(blob_path)})
        finally:
            if os.path.exists(temporary): os.unlink(temporary)

    def _checked_connection(self, *, write: bool = False) -> sqlite3.Connection:
        if write and self._readonly:
            raise ValueError("readonly SHARC index has no cached query result")
        con = sqlite3.connect(self._path if write else f"file:{self._path}?mode=ro", uri=not write)
        con.execute("PRAGMA foreign_keys=ON")
        try:
            if (con.execute("PRAGMA user_version").fetchone() != (DB_SCHEMA_VERSION,)
                    or con.execute("SELECT value FROM cache_meta WHERE key='complete'").fetchone() != ("1",)
                    or con.execute("SELECT value FROM cache_meta WHERE key='static_key'").fetchone() != (self._static_key,)
                    or con.execute("SELECT static_key FROM static_snapshot WHERE id=1").fetchone() != (self._static_key,)):
                raise ValueError("index was replaced or is incomplete")
        except Exception:
            con.close()
            raise
        return con

    def snapshot(self) -> Mapping[str, Any]:
        with self._checked_connection() as con:
            row = con.execute("SELECT payload,payload_sha256,payload_size FROM static_snapshot WHERE id=1 AND static_key=?", (self._static_key,)).fetchone()
        value = _valid_json_payload(*row) if row else None
        if value is None: raise ValueError("complete index lacks valid static snapshot")
        return value

    def _request_writer(self, target: WriterTarget) -> dict[str, Any]:
        return {"contract": "writer-target/v1", "target": {"address": target.address, "width": target.width}, "policy": WRITER_POLICY}

    def _request_register(self, query: RegisterEffectQuery) -> dict[str, Any]:
        return {"contract": "register-effect/v2", "entry_sw": query.entry_sw, "register": query.register,
                "max_steps": query.max_steps, "max_states": query.max_states, "max_call_depth": query.max_call_depth,
                "policy": REGISTER_POLICY, "reducer": "conservative-register-paths/v2"}

    def _query_key(self, request: Mapping[str, Any]) -> str:
        return hashlib.sha256(_canonical_bytes({"static_key": self._static_key, "request": request})).hexdigest()

    def _lookup(self, table: str, key: str, request: Mapping[str, Any]) -> Mapping[str, Any] | None:
        sql = _QUERY_SQL[table]
        with self._checked_connection() as con:
            row = con.execute(sql["select"], (key,)).fetchone()
        if not row or row[4] != 1 or row[5] != self._static_key or row[0] != _payload(request): return None
        return _valid_json_payload(*row[1:4])

    def _publish(self, table: str, key: str, request: Mapping[str, Any], value: Mapping[str, Any]) -> Mapping[str, Any]:
        sql = _QUERY_SQL[table]
        raw_request, raw = _payload(request), _payload(value)
        digest = hashlib.sha256(raw).hexdigest()
        with self._checked_connection(write=True) as con:
            con.execute("BEGIN IMMEDIATE")
            con.execute(sql["insert_ignore"], (key, self._static_key, raw_request, raw, digest, len(raw)))
            row = con.execute(sql["select"], (key,)).fetchone()
            valid = row and row[4] == 1 and row[5] == self._static_key and row[0] == raw_request and _valid_json_payload(*row[1:4])
            if not valid:
                con.execute(sql["delete"], (key,))
                con.execute(sql["insert"], (key, self._static_key, raw_request, raw, digest, len(raw)))
                valid = value
            con.commit()
        return valid

    def query(self, *, writer_targets: Sequence[WriterTarget] = (), register_effects: Sequence[RegisterEffectQuery] = (), jobs: int = 1) -> Mapping[str, Any]:
        # Checking up front also makes an empty query reject an incompatible replacement.
        with self._checked_connection(): pass
        writer_items = sorted({(item.address, item.width): item for item in writer_targets}.values(), key=lambda item: (item.address, item.width))
        register_items = sorted({(item.entry_sw, item.register, item.max_steps, item.max_states, item.max_call_depth): item for item in register_effects}.values(), key=lambda item: (item.entry_sw, item.register, item.max_steps, item.max_states, item.max_call_depth))
        writer_values: dict[tuple[int, int], Mapping[str, Any]] = {}
        missing_writers = []
        for item in writer_items:
            request, key = self._request_writer(item), self._query_key(self._request_writer(item))
            value = self._lookup("writer_results", key, request)
            if value is None: missing_writers.append((item, request, key))
            else: writer_values[(item.address, item.width)] = value
        if missing_writers:
            if self._readonly: raise ValueError("readonly SHARC index has no cached query result")
            import sharcwriters
            config, blob = self._metadata["config"], self._metadata.get("blob_path")
            if not blob: raise ValueError("writer queries require an index opened in this process")
            try:
                out = sharcwriters.run_many(blob, tuple(config["code_blocks"]), config["min_depth"], [(i.address, i.width) for i, _, _ in missing_writers], **WRITER_POLICY, jobs=jobs)
            except Exception as error: raise RuntimeError("writer query batch failed") from error
            for item, request, key in missing_writers:
                writer_values[(item.address, item.width)] = self._publish("writer_results", key, request, out[(item.address, item.width)])
        register_values: dict[tuple[Any, ...], Mapping[str, Any]] = {}
        missing_registers = []
        for item in register_items:
            request, key = self._request_register(item), self._query_key(self._request_register(item))
            value = self._lookup("register_results", key, request)
            ident = (item.entry_sw, item.register, item.max_steps, item.max_states, item.max_call_depth)
            if value is None: missing_registers.append((item, request, key, ident))
            else: register_values[ident] = value
        if missing_registers:
            if self._readonly: raise ValueError("readonly SHARC index has no cached query result")
            import sharcfn
            import sharc_trace
            blob, config = self._metadata.get("blob_path"), self._metadata["config"]
            if not blob: raise ValueError("register-effect queries require an index opened in this process")
            context = sharcfn.load_context(blob, tuple(config["code_blocks"]), config["min_depth"])
            entries = {fn["entry"] for fn in context["functions"]}
            for item, request, key, ident in missing_registers:
                if item.entry_sw not in entries:
                    effect = classify_register_paths([{"verified_return": False, "uncertainty": ["entry is not a recovered function"]}])
                else:
                    try: states = sharc_trace.trace(context["mem"], None, item.entry_sw, max_steps=item.max_steps, max_states=item.max_states, max_call_depth=item.max_call_depth, concrete_memory=True, follow_loaded_calls=True, continue_external_calls=False)
                    except Exception as error: raise RuntimeError(f"register-effect trace failed at 0x{item.entry_sw:x}") from error
                    paths = []
                    for state in states:
                        writers, uncertainty = [], []
                        for event in state.trace:
                            result = event.get("result_register"); destination = event.get("destination", event.get("ureg"))
                            if destination == item.register or result == item.register or isinstance(result, list) and item.register in result: writers.append(event.get("pc_sw", item.entry_sw))
                            if event.get("action") in ("opaque-external-call", "unsupported", "indirect-call"): uncertainty.append(event["action"])
                        if state.stopped not in REGISTER_EFFECT_VERIFIED_RETURNS: uncertainty.append("trace stop: " + str(state.stopped))
                        paths.append({"verified_return": state.stopped in REGISTER_EFFECT_VERIFIED_RETURNS, "writer_pcs": writers, "uncertainty": uncertainty})
                    effect = classify_register_paths(paths)
                register_values[ident] = self._publish("register_results", key, request, {"entry_sw": item.entry_sw, "register": item.register, **effect})
        return {"writer_targets": [writer_values[(i.address, i.width)] for i in writer_items], "register_effects": [register_values[(i.entry_sw, i.register, i.max_steps, i.max_states, i.max_call_depth)] for i in register_items]}

    def trace(self, probes: Sequence[Mapping[str, Any]], *, jobs: int = 1) -> Mapping[str, Any]:
        try: canonical_probes = [json.loads(_canonical(probe)) for probe in sorted(probes, key=_canonical)]
        except (TypeError, ValueError, json.JSONDecodeError) as error: raise ValueError("trace probes must be canonical JSON-compatible records") from error
        return {"probes": canonical_probes, "status": "unknown", "reason": "trace requires discovery memory adapter"}
