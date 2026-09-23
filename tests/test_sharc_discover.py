"""Deterministic composition tests for tools/sharc_discover.py."""

import importlib
import json
import os
import pathlib
import sqlite3
import struct
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import patch

sys.path.insert(0, str(pathlib.Path(__file__).parents[1] / "tools"))

sharc_disasm = importlib.import_module("sharc_disasm")
D = importlib.import_module("sharc_discover")
T = importlib.import_module("sharc_visa_tables")
sharcldr = importlib.import_module("sharcldr")


BLOB = pathlib.Path("out/sections/dt2-1.16/section_7_BLOB.bin")
MANIFEST = pathlib.Path("tools/sharc-discovery/dt2-1.16.json")


def loader_block(code, address, count, arg=0, payload=b""):
    header = bytearray(struct.pack("<IIII", code | 0xAD000000, address, count, arg))
    header[2] = 0
    checksum = 0
    for byte in header:
        checksum ^= byte
    header[2] = checksum
    return bytes(header) + payload


def instruction(name, **fields):
    description = T.get_type(name)
    value = description["opcode_value"]
    for field, field_value in fields.items():
        high, low = description["fields"][field]
        mask = ((1 << (high - low + 1)) - 1) << low
        value = (value & ~mask) | ((field_value << low) & mask)
    words = [
        (value >> (description["bits"] - 16 * (index + 1))) & 0xFFFF
        for index in range(description["bits"] // 16)
    ]
    encoded = struct.pack("<%dH" % len(words), *words)
    return next(sharc_disasm.disassemble(encoded))


class StaticContextTest(unittest.TestCase):
    def test_enumerates_type9_variants_and_normalizes_dag2_registers(self):
        insns = [
            instruction(
                "9b_abs",
                **{
                    "b": 0,
                    "cond[4:0]": 31,
                    "pmi[2:2]": 1,
                    "pmi[1:0]": 0,
                    "pmm[2:0]": 6,
                    "j": 1,
                },
            ),
            instruction(
                "9b_abs",
                **{
                    "b": 0,
                    "cond[4:0]": 31,
                    "pmi[2:2]": 1,
                    "pmi[1:0]": 0,
                    "pmm[2:0]": 5,
                    "j": 0,
                },
            ),
            instruction(
                "9a_abs",
                **{
                    "b": 1,
                    "cond[4:0]": 7,
                    "pmi[2:2]": 0,
                    "pmi[1:0]": 3,
                    "pmm[2:0]": 2,
                    "j": 1,
                },
            ),
        ]
        offsets = []
        offset = 0
        for item in insns:
            offsets.append((offset, item))
            offset += item.length_bytes
        context = {
            "functions": [
                {
                    "id": "blk1@0x100",
                    "block": 1,
                    "entry": 0x100,
                    "exit": 0x120,
                }
            ],
            "analyzed": {1: {"base_sw": 0x100, "insns": offsets}},
        }
        sites = D.build_static_context(context)["indirect_sites"]
        self.assertEqual([site["kind"] for site in sites], ["return", "jump", "call"])
        self.assertEqual(
            (sites[1]["index_register"], sites[1]["modifier_register"]),
            ("I12", "M13"),
        )
        self.assertTrue(sites[0]["delayed"])
        self.assertFalse(sites[1]["delayed"])
        self.assertEqual(sites[2]["condition"], 7)


class PointerRunTest(unittest.TestCase):
    def test_uses_final_memory_and_keeps_slot_provenance(self):
        stream = b"".join(
            [
                loader_block(1, 0x1000, 4, payload=b"code"),
                loader_block(
                    1,
                    0x80000000,
                    12,
                    payload=struct.pack("<III", 0x100, 0x102, 0x104),
                ),
                loader_block(1, 0x80000004, 4, payload=struct.pack("<I", 0x104)),
                # The final fill contains an otherwise valid pointer but is
                # not payload evidence and must not extend the run.
                loader_block(0x101, 0x80000008, 4, arg=0x100),
            ]
        )
        blocks = sharcldr.parse_blocks(stream)
        context = {
            "blocks": blocks,
            "mem": sharcldr.LoadedMemory.from_stream(stream, blocks),
        }
        instructions = {
            0x100: {"block": 1, "owner": "a"},
            0x102: {"block": 1, "owner": "wrong-overwritten-target"},
            0x104: {"block": 1, "owner": "b"},
        }
        runs, hits = D.find_pointer_runs(context, instructions, code_blocks=(0,))
        self.assertEqual(hits, 2)
        self.assertEqual(len(runs), 1)
        self.assertEqual([entry["target_sw"] for entry in runs[0]["entries"]], [0x100, 0x104])
        self.assertEqual(runs[0]["source_blocks"], [1, 2])
        self.assertEqual(runs[0]["entries"][1]["source_byte_blocks"], [2, 2, 2, 2])

    def test_canonicalizes_low_dm_literals_for_loader_pointer_joins(self):
        sites = [{"pc_sw": 0x120, "block": 1, "owner": "f", "kind": "jump",
                  "index_register": "I12", "modifier_register": "M13"}]
        literals = [{"pc_sw": 0x110, "block": 1, "owner": "f", "value": 0x256C98}]
        table = {"source_byte_address": 0x28256C98, "entry_count": 2,
                 "entries": [{"target_sw": 0x130}, {"target_sw": 0x140}],
                 "target_owners": ["f"]}
        joined = D.join_dispatch_candidates(sites, literals, [table], 64)
        self.assertEqual(joined[0]["literal_loader_alias"], 0x28256C98)
        self.assertEqual(joined[0]["table_byte_address"], 0x28256C98)

    def test_structural_join_is_transparent_and_not_semantic_proof(self):
        sites = [
            {
                "pc_sw": 0x120,
                "block": 1,
                "owner": "f",
                "kind": "jump",
                "index_register": "I12",
                "modifier_register": "M13",
            }
        ]
        literals = [
            {
                "pc_sw": 0x110,
                "block": 1,
                "owner": "f",
                "value": 0x80000000,
            }
        ]
        tables = [
            {
                "source_byte_address": 0x80000000,
                "entry_count": 2,
                "entries": [{"target_sw": 0x130}, {"target_sw": 0x140}],
                "target_owners": ["f"],
            }
        ]
        joined = D.join_dispatch_candidates(sites, literals, tables, 64)
        self.assertEqual(joined[0]["rank_tuple"], [0, -2, 0x10, 0x120])
        self.assertEqual(joined[0]["semantic_status"], "not-proven")


class NaturalSelectorFrontierTest(unittest.TestCase):
    def test_materializes_loader_cells_and_rejects_unbounded_candidates(self):
        stream = b"".join([
            loader_block(1, 0x282560C4, 4, payload=struct.pack("<I", 0)),
            loader_block(1, 0x28256C98, 8, payload=struct.pack("<II", 0x130, 0x140)),
        ])
        blocks = sharcldr.parse_blocks(stream)
        context = {"mem": sharcldr.LoadedMemory.from_stream(stream, blocks), "by_id": {
            "wrap-a": {"callees": ["leaf-a"]}, "wrap-b": {"callees": ["leaf-b"]},
            "leaf-a": {"entry": 0x200}, "leaf-b": {"entry": 0x210},
        }}
        static = {"instructions": {
            0x120: {"form": "9b_abs", "raw_hex": "deadbeef"},
            0x130: {"owner": "wrap-a"}, 0x140: {"owner": "wrap-b"},
        }}
        table = {"source_byte_address": 0x28256C98, "source_blocks": [1], "entry_count": 2,
                 "entries": [{"target_sw": 0x130}, {"target_sw": 0x140}]}
        declaration = {"name": "frontier", "tail_jump_sw": 0x120,
                       "tail_decode": {"form": "9b_abs", "raw_hex": "deadbeef"},
                       "runtime_cells": [{"dm_byte_address": 0x2560C4},
                                         {"dm_byte_address": 0x256C98, "width": 8}],
                       "table_dm_byte_address": 0x256C98,
                       "loaded_target_candidates": [
                           {"entry_index": 0, "target_sw": 0x130, "callee_entry_sw": 0x200},
                           {"entry_index": 1, "target_sw": 0x140, "callee_entry_sw": 0x210}],
                       "unresolved_reasons": []}
        frontier = D.build_natural_selector_frontiers(context, static, [table], [declaration])[0]
        self.assertEqual(frontier["runtime_cells"][0]["loader_byte_address"], 0x282560C4)
        self.assertEqual(frontier["loaded_target_candidates"][1]["target_sw"], 0x140)
        self.assertEqual(
            frontier["loaded_target_candidates"][0]["r6_disposition_evidence_class"],
            "unknown",
        )
        self.assertEqual(frontier["r6_effect_audit"]["evidence_class"], "unknown")
        bad = {**declaration, "loaded_target_candidates": declaration["loaded_target_candidates"][:1]}
        with self.assertRaisesRegex(ValueError, "cover"):
            D.build_natural_selector_frontiers(context, static, [table], [bad])


class GeneratedFrontierEvidenceTest(unittest.TestCase):
    def test_joins_generated_writer_and_r6_results(self):
        frontier: list[dict] = [{"runtime_cells": [{"dm_byte_address": 0x10, "width": 4}],
                     "loaded_target_candidates": [{"callee_entry_sw": 0x20}]}]
        D.attach_generated_frontier_evidence(frontier, {
            "writer_targets": [{"target": 0x10, "target_width": 4, "coverage": "incomplete"}],
            "register_effects": [{"entry_sw": 0x20, "register": "R6", "status": "written"}],
        })
        self.assertEqual(frontier[0]["writer_coverage"]["status"], "unknown")
        self.assertEqual(frontier[0]["loaded_target_candidates"][0]["r6_disposition"], "written")
        self.assertEqual(frontier[0]["r6_effect_audit"]["effects"][0]["entry_sw"], 0x20)


class ParallelTraceCaseTest(unittest.TestCase):
    def test_jobs_two_uses_worker_path_and_merges_stable_order(self):
        probes = [
            {"name": "z", "start_sw": 2, "_cases": [{"sets": {"R1": 1}}, {"sets": {"R1": 2}}]},
            {"name": "a", "start_sw": 1, "_cases": [{"sets": {"R1": 3}}]},
        ]
        def result(item):
            return {"ordinal": item["ordinal"], "case": item["case"],
                    "summary": {"states": [{"stopped": "mock"}]}, "edges": []}
        submitted = []
        class Future:
            def __init__(self, value): self.value = value
            def result(self): return self.value
        class Executor:
            def __init__(self, **kwargs): self.kwargs = kwargs
            def __enter__(self): return self
            def __exit__(self, *args): return False
            def submit(self, function, item):
                submitted.append((function, item["ordinal"]))
                return Future(result(item))
        with patch.object(D, "_run_trace_case", side_effect=lambda memory, item: result(item)):
            serial, _ = D.run_declared_probes(object(), probes, 8, set(), set(), jobs=1)
        with patch.object(D, "ProcessPoolExecutor", Executor), patch.object(D, "as_completed", lambda futures: futures), patch.object(D, "_trace_case_worker", side_effect=result):
            parallel, _ = D.run_declared_probes(object(), probes, 8, set(), set(), blob_path=pathlib.Path("fake"), jobs=2)
        self.assertEqual(serial, parallel)
        self.assertEqual(len(submitted), 3)
        self.assertTrue(all(callable(item[0]) for item in submitted))
        self.assertEqual([item[1] for item in submitted], [[0, 0], [1, 0], [1, 1]])

    def test_real_spawn_worker_matches_serial_synthetic_loader(self):
        stream = loader_block(15, 0, 0)
        memory = sharcldr.LoadedMemory.from_stream(stream, sharcldr.parse_blocks(stream))
        probes = [{"name": "unmapped", "start_sw": 0, "_cases": [{}]}]
        with tempfile.TemporaryDirectory() as directory:
            blob = pathlib.Path(directory) / "loader.bin"
            blob.write_bytes(stream)
            serial, _ = D.run_declared_probes(memory, probes, 2, set(), set(), blob_path=blob, jobs=1)
            parallel, _ = D.run_declared_probes(memory, probes, 2, set(), set(), blob_path=blob, jobs=2)
        self.assertEqual(json.dumps(serial, sort_keys=True), json.dumps(parallel, sort_keys=True))

    def test_r4_frontier_declaration_expands_to_one_case_per_target(self):
        frontier = {"name": "frontier", "loaded_target_candidates": [
            {"entry_index": 0, "target_sw": 0x100}, {"entry_index": 1, "target_sw": 0x120}
        ]}
        plans = D.resolve_r4_frontier_probes([{"name": "r4", "start_sw": 0x80, "frontier": "frontier", "values": [1, 0], "sets": {"M13": 0, "R4": 99}, "assumptions": ["counterfactual"]}], [frontier])
        self.assertEqual([case["sets"] for case in plans[0]["cases"]], [{"M13": 0, "R4": 0}, {"M13": 0, "R4": 1}])
        self.assertEqual([case["breakpoints"] for case in plans[0]["cases"]], [[0x100], [0x120]])
        self.assertNotIn("I12", plans[0]["cases"][0]["sets"])
        self.assertIn("counterfactual", plans[0]["assumptions"])
        with self.assertRaisesRegex(ValueError, "sets must be an object"):
            D.resolve_r4_frontier_probes([{"name": "bad", "start_sw": 0x80, "frontier": "frontier", "values": [0], "sets": []}], [frontier])
        with self.assertRaisesRegex(ValueError, "must not seed I12"):
            D.resolve_r4_frontier_probes([{"name": "bad", "start_sw": 0x80, "frontier": "frontier", "values": [0], "sets": {"I12": 0}}], [frontier])

    def test_parallel_requires_blob_and_keeps_global_cap(self):
        probes = [{"name": "a", "start_sw": 1, "_cases": [{}, {}]}]
        with self.assertRaisesRegex(ValueError, "require blob_path"):
            D.run_declared_probes(object(), probes, 2, set(), set(), jobs=2)
        with self.assertRaisesRegex(ValueError, "exceed total case cap"):
            D.run_declared_probes(object(), probes, 1, set(), set(), blob_path=pathlib.Path("x"), jobs=2)


class ManifestAndDumpTest(unittest.TestCase):
    def test_manifest_rejects_wrong_schema(self):
        with tempfile.TemporaryDirectory() as directory:
            path = pathlib.Path(directory) / "manifest.json"
            path.write_text(json.dumps({"schema": "wrong", "image_sha256": "0" * 64}))
            with self.assertRaisesRegex(ValueError, "manifest schema"):
                D.load_manifest(path)

    def test_trace_matrix_order_is_stable(self):
        cases = D._expand_probe(
            {"sets": {"R8": "0x10"}, "matrix": {"M4": [1, 0], "M13": [0]}},
            10,
        )
        self.assertEqual(
            cases,
            [
                {"M13": 0, "M4": 1, "R8": 0x10},
                {"M13": 0, "M4": 0, "R8": 0x10},
            ],
        )

    def test_trace_probe_rejects_unbounded_step_limit(self):
        with self.assertRaisesRegex(ValueError, "at most 1000"):
            D.run_declared_probes(
                b"", [{"name": "too-long", "start_sw": 0, "max_steps": 1001}], 1, set(), set()
            )

    def test_dispatch_target_expands_all_entries_in_stable_order(self):
        table = {"source_byte_address": 0x8000, "entry_count": 2,
                 "entries": [{"target_sw": 0x100, "target_owner": "a"},
                             {"target_sw": 0x120, "target_owner": "b"}]}
        dispatch = {"site_pc_sw": 0x50, "table_byte_address": 0x8000}
        declaration = {"name": "p", "site_pc_sw": 0x50, "table_byte_address": 0x8000,
                       "selector_register": "M4", "start_sw": 0x40, "sets": {"M13": 0},
                       "entry_indices": "all", "max_steps": 2, "max_states": 1,
                       "assumptions": []}
        plans = D.resolve_dispatch_target_probes([declaration], [dispatch], [table])
        self.assertEqual([case["entry_index"] for case in plans[0]["cases"]], [0, 1])
        self.assertEqual(plans[0]["cases"][1]["sets"], {"M13": 0, "M4": 1})
        self.assertEqual(plans[0]["cases"][1]["breakpoints"], [0x120])

    def test_dispatch_target_rejects_bad_structural_or_indices(self):
        base = {"name": "p", "site_pc_sw": 1, "table_byte_address": 2,
                "selector_register": "M4", "start_sw": 0, "entry_indices": [0], "assumptions": []}
        table = {"source_byte_address": 2, "entry_count": 1,
                 "entries": [{"target_sw": 3, "target_owner": None}]}
        with self.assertRaisesRegex(ValueError, "match count is 0"):
            D.resolve_dispatch_target_probes([base], [], [table])
        with self.assertRaisesRegex(ValueError, "truncated"):
            D.resolve_dispatch_target_probes([base], [{"site_pc_sw": 1, "table_byte_address": 2}], [{**table, "entries_truncated": True}])
        with self.assertRaisesRegex(ValueError, "out-of-range"):
            D.resolve_dispatch_target_probes([{**base, "entry_indices": [1]}], [{"site_pc_sw": 1, "table_byte_address": 2}], [table])
        with self.assertRaisesRegex(ValueError, "duplicates"):
            D.resolve_dispatch_target_probes([{**base, "entry_indices": [0, 0]}], [{"site_pc_sw": 1, "table_byte_address": 2}], [table])
        with self.assertRaisesRegex(ValueError, "supported register"):
            D.resolve_dispatch_target_probes([{**base, "selector_register": "bad"}], [{"site_pc_sw": 1, "table_byte_address": 2}], [table])

    def test_selector_provenance_expansion_and_forced_classification(self):
        table = {"source_byte_address": 0x8000, "entry_count": 2,
                 "entries": [{"target_sw": 0x100}, {"target_sw": 0x120}]}
        declaration = {"name": "r6", "start_sw": 0x40, "copy_pc_sw": 0x41,
                       "table_load_pc_sw": 0x42, "site_pc_sw": 0x50,
                       "table_byte_address": 0x8000, "source_register": "R6",
                       "selector_register": "M4", "sets": {"M13": 0},
                       "seed_values": [1, 0], "assumptions": []}
        plans = D.resolve_selector_provenance_probes(
            [declaration], [{"site_pc_sw": 0x50, "table_byte_address": 0x8000}], [table]
        )
        self.assertEqual([case["seed_value"] for case in plans[0]["cases"]], [0, 1])
        self.assertEqual(plans[0]["cases"][1]["sets"], {"M13": 0, "R6": 1})
        reached = {"stopped": "breakpoint", "stop_pc_sw": 0x120,
                   "selector_transfer_audit": {"copies": [{}], "table_loads": [{}],
                   "later_selector_writes": [], "dispatch_branches": [{"target_sw": 0x120}]}}
        other = {"stopped": "unsupported form"}
        outcome, quantifier, observed = D.classify_selector_provenance_summary(
            [reached, other], 0x120
        )
        self.assertEqual((outcome, quantifier), ("target-reached", "existential"))
        self.assertFalse(observed)  # A manifest-seeded R6 hypothesis is never occurrence evidence.
        missing_load = {**reached, "selector_transfer_audit": {
            **reached["selector_transfer_audit"], "table_loads": []
        }}
        self.assertEqual(
            D.classify_selector_provenance_summary([missing_load], 0x120)[0],
            "ambiguous",
        )
        for boolean in ("concrete_memory", "assume_nw32", "follow_loaded_calls",
                        "continue_external_calls", "core_reset_state"):
            with self.subTest(boolean=boolean):
                with self.assertRaisesRegex(ValueError, f"{boolean} must be boolean"):
                    D.resolve_selector_provenance_probes(
                        [{**declaration, boolean: "false"}],
                        [{"site_pc_sw": 0x50, "table_byte_address": 0x8000}],
                        [table],
                    )

    def test_dispatch_target_summary_requires_exact_branch_evidence(self):
        branch = {"action": "branch", "pc_sw": 0x50, "target_sw": 0x100}
        resolved = {"states": [{"stopped": "breakpoint", "stop_pc_sw": 0x100,
                                 "registers": {"M4": 0}, "last_events": [{"action": "load"}],
                                 "dispatch_branch_audit": [branch]}]}
        outcome, terminals, state = D.classify_dispatch_target_summary(resolved, 0x100, 0x50)
        self.assertEqual((outcome, state["registers"]), ("resolved", {"M4": 0}))
        self.assertEqual(terminals[0]["last_events"], [{"action": "load"}])
        self.assertEqual(terminals[0]["dispatch_branch_audit"], [branch])
        missing = {"states": [{"stopped": "breakpoint", "stop_pc_sw": 0x100,
                               "last_events": [branch], "dispatch_branch_audit": []}]}
        self.assertEqual(D.classify_dispatch_target_summary(missing, 0x100, 0x50)[0], "missing-branch-evidence")
        self.assertEqual(D.classify_dispatch_target_summary({"states": []}, 0x100, 0x50)[0], "unreached")
        self.assertEqual(D.classify_dispatch_target_summary(
            {"states": [{"stopped": "breakpoint", "stop_pc_sw": 0x101}]}, 0x100, 0x50)[0], "mismatch")
        self.assertEqual(D.classify_dispatch_target_summary(
            {"states": [{"stopped": "breakpoint", "stop_pc_sw": 0x100},
                        {"stopped": "limit", "stop_pc_sw": 0x102}]}, 0x100, 0x50)[0], "ambiguous")

    def test_reads_actual_ghidradump_schema_in_read_only_mode(self):
        with tempfile.TemporaryDirectory() as directory:
            path = pathlib.Path(directory) / "xrefs.sqlite"
            connection = sqlite3.connect(path)
            connection.executescript(
                """
                create table functions(entry integer primary key, name text, size integer);
                create table calls(from_func integer, to_func integer, to_addr integer, site integer, kind text);
                create table data_refs(site integer, func integer, to_addr integer, kind text, label text, block text);
                insert into functions values(512, 'root', 12);
                insert into calls values(768, 512, 512, 800, 'CALL');
                insert into calls values(512, 1024, 1024, 520, 'CALL');
                """
            )
            connection.commit()
            connection.close()
            evidence = D.read_ghidradump_evidence_ro(
                path, [{"name": "r", "entry_sw": 0x100}], 2
            )
            self.assertEqual(evidence["evidence_class"], "dump-advisory")
            self.assertEqual(evidence["roots"][0]["function_name"], "root")
            self.assertEqual(evidence["roots"][0]["incoming_direct_edges"], 1)
            self.assertEqual(evidence["roots"][0]["outgoing_direct_edges"], 1)


@unittest.skipUnless(BLOB.exists(), "DT2 1.16 SHARC loader is not available")
class FirmwareIntegrationTest(unittest.TestCase):
    def test_finds_known_dispatch_and_pointer_run(self):
        manifest = D.load_manifest(MANIFEST)
        report = D.discover(BLOB, manifest)
        dispatch = next(
            item for item in report["dispatch_candidates"] if item["site_pc_sw"] == 0x1C6579
        )
        self.assertEqual(dispatch["table_byte_address"], 0x8055C840)
        self.assertEqual(dispatch["target_pcs"][:4], [0x1C65BD, 0x1C6715, 0x1C6782, 0x1C686E])
        site = next(item for item in report["indirect_sites"] if item["pc_sw"] == 0x1C6579)
        self.assertEqual(site["strict_trace_targets"], sorted(dispatch["target_pcs"]))
        contexts = report["dispatch_target_contexts"]
        self.assertEqual([item["entry_index"] for item in contexts], list(range(20)))
        self.assertTrue(all(item["outcome"] == "resolved" for item in contexts))
        self.assertEqual([item["expected_target_sw"] for item in contexts], dispatch["target_pcs"])
        for item in contexts:
            self.assertEqual(item["selector_register"], "M4")
            self.assertEqual(item["selector_value"], item["entry_index"])
            self.assertEqual(len(item["branch_evidence"]), 1)
            self.assertEqual(item["branch_evidence"][0]["action"], "branch")
            self.assertEqual(item["branch_evidence"][0]["pc_sw"], 0x1C6579)
            self.assertEqual(item["branch_evidence"][0]["target_sw"], item["expected_target_sw"])
            self.assertEqual(item["registers"]["I12"], item["expected_target_sw"])
            self.assertEqual(item["registers"]["M4"], item["entry_index"])
            self.assertEqual(item["registers"]["M13"], 0)
            self.assertEqual(item["registers"]["R8"], 0x42700000)
        # This owner cluster is structural (function inventory containment),
        # not a claim that these selectors occur naturally.
        self.assertTrue(all("1c71ec" in (item["target_owner"] or "") for item in contexts[-5:]))
        self.assertEqual(report["coverage"]["dispatch_target_contexts"], 20)
        self.assertEqual(report["coverage"]["dispatch_target_context_outcomes"], {"resolved": 20})
        provenance = report["selector_provenance"]
        self.assertEqual([item["seed_value"] for item in provenance], list(range(20)))
        self.assertTrue(all(item["selector_origin"] == "manifest-seed" for item in provenance))
        self.assertTrue(all(item["runtime_occurrence"] == "not-observed" for item in provenance))
        self.assertTrue(all(not item["natural_runtime_observed"] for item in provenance))
        self.assertEqual([item["trace_outcome"] for item in provenance[15:20]], ["target-reached"] * 5)
        self.assertEqual([item["path_quantifier"] for item in provenance[15:20]], ["existential"] * 5)
        frontier = report["natural_selector_frontiers"][0]
        self.assertEqual(frontier["range_guard_status"].split(":", 1)[0], "unproven")
        self.assertTrue(all(
            item["r6_disposition_evidence_class"] == "unknown"
            for item in frontier["loaded_target_candidates"]
        ))
        self.assertEqual(frontier["r6_effect_audit"]["evidence_class"], "unknown")
        self.assertEqual(report["semantic_status"], "discovery-only")

    def test_indexed_jobs_emit_identical_cli_report(self):
        # Firmware-gated subprocess contract: CLI serialization itself must
        # not leak worker identity, cache path, hash seed, or completion order.
        with tempfile.TemporaryDirectory() as directory:
            directory = pathlib.Path(directory)
            index, first_path, second_path = directory / "index.sqlite", directory / "one.json", directory / "two.json"
            base = [sys.executable, "tools/sharc_discover.py", str(BLOB), str(MANIFEST), "--index", str(index)]
            first_env, second_env = dict(os.environ, PYTHONHASHSEED="1"), dict(os.environ, PYTHONHASHSEED="999")
            subprocess.run([*base, "--jobs", "1", "-o", str(first_path)], check=True, cwd=pathlib.Path(__file__).parents[1], env=first_env)
            mtime = index.stat().st_mtime_ns
            subprocess.run([*base, "--jobs", "2", "-o", str(second_path)], check=True, cwd=pathlib.Path(__file__).parents[1], env=second_env)
            self.assertEqual(mtime, index.stat().st_mtime_ns)
            first, second = first_path.read_bytes(), second_path.read_bytes()
        self.assertEqual(first, second)
        report = json.loads(first)
        self.assertEqual(report["provenance"]["index"]["fingerprint"], report["provenance"]["blob"]["sha256"])
        self.assertEqual(report["provenance"]["tools"]["sharc_static.py"], D._sha256((D._HERE / "sharc_static.py").read_bytes()))
        self.assertTrue(report["ivt_audio_root_scan"]["candidate_scans"])
        self.assertTrue(any(item["pc_sw"] == 0x1C6579 for item in report["indirect_sites"]))
        self.assertEqual(report["natural_selector_frontiers"][0]["tail_jump"]["pc_sw"], 0x1C351A)

    def test_candidate_vector_scan_keeps_block70_identity_unverified(self):
        manifest = D.load_manifest(MANIFEST)
        report = D.discover(BLOB, manifest)
        scan = report["ivt_audio_root_scan"]["candidate_scans"][0]
        self.assertEqual(scan["identity"], "unverified-core-ivt-candidate")
        self.assertEqual(scan["candidate_region"]["start_pc_sw"], 0x120000)
        self.assertEqual(scan["candidate_region"]["loader_block"], 70)
        self.assertEqual(scan["layout"]["offset_unit"], "architectural-instruction")
        self.assertFalse(scan["layout_match"])
        self.assertEqual(scan["failure"], "entry_count and processor-specific vector mapping are unverified")
        self.assertEqual((scan["core_semantics"], scan["sport_semantics"], scan["audio_semantics"]), ("unknown", "unknown", "unknown"))

    def test_r4_counterfactual_tail_cases_retain_return_and_breakpoint_paths(self):
        memory = sharcldr.LoadedMemory.from_stream(BLOB.read_bytes(), sharcldr.parse_blocks(BLOB.read_bytes()))
        for value, target in enumerate((0x1C351C, 0x1C352F, 0x1C353E, 0x1C354D)):
            states = D.trace.trace(memory, None, 0x1C3507, sets={"M13": 0, "R4": value},
                                   max_steps=64, max_states=16, concrete_memory=True,
                                   assume_nw32=True, breakpoints=(target,))
            self.assertEqual([state.stopped for state in states].count("return without followed call"), 1)
            breakpoint = [state for state in states if state.stopped == "breakpoint"]
            self.assertEqual(len(breakpoint), 1)
            self.assertEqual(breakpoint[0].pc_sw, target)
            events = breakpoint[0].trace
            self.assertTrue(any(event.get("action") == "ureg-copy" and event.get("pc_sw") == 0x1C3507 and event.get("source") == "R4" and event.get("destination") == "M4" for event in events))
            self.assertTrue(any(event.get("action") == "load" and event.get("pc_sw") == 0x1C3518 and event.get("ureg") == "I12" for event in events))
            self.assertTrue(any(event.get("action") == "branch" and event.get("pc_sw") == 0x1C351A and event.get("target_sw") == target for event in events))

    def test_wrong_declared_table_load_cannot_reach_selector_provenance(self):
        manifest = D.load_manifest(MANIFEST)
        # This is decoded but is not the I12 table load at 0x1c656c.
        manifest["selector_provenance_probes"][0]["table_load_pc_sw"] = "0x1c656e"
        report = D.discover(BLOB, manifest)
        self.assertTrue(all(
            item["trace_outcome"] == "ambiguous"
            for item in report["selector_provenance"]
        ))


if __name__ == "__main__":
    unittest.main()
