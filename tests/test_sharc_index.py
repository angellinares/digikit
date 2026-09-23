"""Synthetic contracts for the persistent SHARC index helpers."""
import hashlib
import pathlib
import sqlite3
import sys
import tempfile
import unittest
from importlib import import_module
from unittest.mock import patch

sys.path.insert(0, str(pathlib.Path(__file__).parents[1] / "tools"))
I = import_module("sharc_index")
Instruction = import_module("sharc_disasm").Instruction


class IndexContractTest(unittest.TestCase):
    def test_finite_domain_guard_derives_values_from_loaded_shift_and_branch(self):
        shift = Instruction(
            0, 6, "6b_shiftimm",
            {"shiftimm[22:16]": 0, "shiftimm[15:0]": 0xFE04},
        )
        branch = Instruction(
            0, 6, "8a_rel",
            {"cond[4:0]": 0x18, "j": 0, "reladdr[23:16]": 0,
             "reladdr[15:0]": 0x11},
        )
        guard = I.FiniteDomainGuard("R4", 0x100, 0x103, 0x109)
        with patch("sharc_disasm.decode_loaded_at", side_effect=[shift, branch]):
            audit = I._audit_finite_domain_guard(object(), guard)
        self.assertEqual(audit["values"], [0, 1, 2, 3])
        self.assertEqual(audit["evidence_class"], "strict-trace-derived-domain")

    def make_index(self, directory, *, readonly=False):
        blob = pathlib.Path(directory) / "blob"
        blob.write_bytes(b"synthetic")
        cache = pathlib.Path(directory) / "index.sqlite"
        with patch.object(I.AnalysisIndex, "_make_snapshot", return_value={"fixture": True}):
            return I.AnalysisIndex.open_or_build(blob, path=cache, config=I.IndexConfig((1,), 8), readonly=readonly)

    def test_tri_state_reducer_is_conservative(self):
        self.assertEqual(I.classify_register_paths([{"verified_return": True}])["status"], "preserved")
        self.assertEqual(I.classify_register_paths([{"verified_return": True, "writer_pcs": [9]}])["status"], "written")
        self.assertEqual(I.classify_register_paths([{"verified_return": False, "uncertainty": ["indirect call"]}])["status"], "unknown")

    def test_incomplete_and_wrong_version_are_rejected_readonly(self):
        with tempfile.TemporaryDirectory() as directory:
            blob = pathlib.Path(directory) / "blob"; blob.write_bytes(b"synthetic")
            cache = pathlib.Path(directory) / "index.sqlite"; sqlite3.connect(cache).close()
            with self.assertRaisesRegex(ValueError, "no complete compatible"):
                I.AnalysisIndex.open_or_build(blob, path=cache, readonly=True, config=I.IndexConfig((1,), 8))
            index = self.make_index(directory)
            with sqlite3.connect(index._path) as con: con.execute("PRAGMA user_version=2")
            with self.assertRaisesRegex(ValueError, "no complete compatible"):
                I.AnalysisIndex.open_or_build(blob, path=index._path, readonly=True, config=I.IndexConfig((1,), 8))

    def test_fingerprint_is_static_only_and_tracks_automatic_dependencies(self):
        with tempfile.TemporaryDirectory() as directory:
            blob = pathlib.Path(directory) / "blob"; blob.write_bytes(b"a")
            first = I.AnalysisIndex._fingerprint(blob, I.IndexConfig((1,), 8))
            self.assertEqual(first["contract"], I.CACHE_CONTRACT)
            paths = {item["path"] for item in first["dependencies"]}
            self.assertTrue({"tools/sharc_isa.py", "tools/sharc_visa_tables.py", "tools/sharcimm.py", "tools/sharcspec/compute_table.json", "tools/sharc_static.py"} <= paths)
            self.assertNotIn("tools/sharc_discover.py", paths)
            self.assertNotIn("tools/sharc_index.py", paths)
            self.assertNotIn("tools/sharc_selache.py", paths)
            with patch.object(I, "_digest_file", return_value="0" * 64):
                self.assertNotEqual(first, I.AnalysisIndex._fingerprint(blob, I.IndexConfig((1,), 8)))

    def test_writer_cache_batches_misses_and_is_jobs_independent(self):
        with tempfile.TemporaryDirectory() as directory:
            index = self.make_index(directory); calls = []
            facts = {"contract": "test-writer-trace-facts/v1"}
            def classify(_facts, targets, **kwargs):
                calls.append((tuple(targets), kwargs)); return {target: {"target": target[0], "target_width": target[1], "coverage": "incomplete"} for target in targets}
            a, b = I.WriterTarget(0x100, 4), I.WriterTarget(0x100, 16)
            with patch("sharcwriters.collect_trace_facts", return_value=facts) as collect, patch(
                "sharcwriters.classify_trace_facts", side_effect=classify
            ):
                cold = index.query(writer_targets=[b, a, a], jobs=1)
                warm = index.query(writer_targets=[a, b], jobs=9)
                index.query(writer_targets=[a, I.WriterTarget(0x104, 4)], jobs=2)
            self.assertEqual(collect.call_count, 1)
            self.assertEqual(collect.call_args.kwargs["jobs"], 1)
            self.assertEqual(calls[0][0], ((0x100, 4), (0x100, 16)))
            self.assertEqual(calls[1][0], ((0x104, 4),))
            self.assertEqual(cold, warm)
            self.assertEqual(I._canonical_bytes(cold), I._canonical_bytes(warm))
            self.assertGreater(index._path.stat().st_size, 0)
            self.assertEqual([x["target_width"] for x in warm["writer_targets"]], [4, 16])

    def test_register_return_without_followed_call_is_verified_at_function_entry(self):
        with tempfile.TemporaryDirectory() as directory:
            index = self.make_index(directory)
            context = {"mem": object(), "functions": [{"entry": 0x20}]}

            class State:
                trace = []
                stopped = "return without followed call"

            with patch("sharcfn.load_context", return_value=context), patch("sharc_trace.trace", return_value=[State()]):
                effect = index.query(register_effects=[I.RegisterEffectQuery(0x20, "R6")])["register_effects"][0]
            self.assertEqual(effect["status"], "preserved")
            self.assertEqual(effect["reasons"], [])

    def test_register_calibration_uses_typed_evidence_without_strengthening_result(self):
        with tempfile.TemporaryDirectory() as directory:
            index = self.make_index(directory)
            context = {"mem": object(), "functions": [{"entry": 0x20}]}

            class State:
                trace = [
                    {"pc_sw": 0x21, "action": "ureg-write", "destination": "R6"}
                ]
                stopped = "return without followed call"
                provisional_used = ("14d",)

            query = I.RegisterEffectQuery(0x20, "R6", calibration_forms=("14d",))
            with patch("sharcfn.load_context", return_value=context), patch(
                "sharc_trace.trace", return_value=[State()]
            ) as trace:
                effect = index.query(register_effects=[query])["register_effects"][0]

            self.assertEqual(trace.call_args.kwargs["provisional_forms"], ("14d",))
            self.assertEqual(effect["status"], "unknown")
            self.assertEqual(effect["writer_pcs"], [])
            self.assertIn("calibration form used: 14d", effect["reasons"])
            self.assertEqual(
                effect["calibration_forms"],
                [{
                    "form": "14d",
                    "evidence": [{
                        "claim_id": "isa.form.14d.encoding",
                        "source": "prm",
                        "status": "unconfirmed",
                    }],
                }],
            )

    def test_register_calibration_rejects_a_documented_form(self):
        with tempfile.TemporaryDirectory() as directory:
            index = self.make_index(directory)
            with self.assertRaisesRegex(ValueError, "does not require calibration"):
                index.query(
                    register_effects=[
                        I.RegisterEffectQuery(0x20, "R6", calibration_forms=("14a",))
                    ]
                )

    def test_register_effect_rejects_non_return_stops_and_event_uncertainty(self):
        with tempfile.TemporaryDirectory() as directory:
            index = self.make_index(directory)
            class State:
                def __init__(self, stopped, trace=()):
                    self.stopped = stopped
                    self.trace = list(trace)

            stops = ("unsupported 7b", "unknown indirect target", "opaque external call",
                     "max steps", "max states", "max call depth", "arbitrary stop")
            states = [State(stop) for stop in stops]
            states.append(State("return without followed call", [{"action": "unsupported"}]))
            context = {"mem": object(), "functions": [{"entry": 0x20 + number} for number in range(len(states))]}
            with patch("sharcfn.load_context", return_value=context), patch("sharc_trace.trace", side_effect=[[state] for state in states]):
                effects = index.query(register_effects=[I.RegisterEffectQuery(0x20 + number, "R6") for number in range(len(states))])["register_effects"]
            self.assertTrue(all(effect["status"] == "unknown" for effect in effects))
            self.assertTrue(any("unsupported" in effect["reasons"] for effect in effects))

    def test_register_effect_writer_beats_verified_return(self):
        with tempfile.TemporaryDirectory() as directory:
            index = self.make_index(directory)

            class State:
                stopped = "return without followed call"
                trace = [{"pc_sw": 0x24, "action": "ureg-write", "destination": "R6"}]

            context = {"mem": object(), "functions": [{"entry": 0x20}]}
            with patch("sharcfn.load_context", return_value=context), patch("sharc_trace.trace", return_value=[State()]):
                effect = index.query(register_effects=[I.RegisterEffectQuery(0x20, "R6")])["register_effects"][0]
            self.assertEqual(effect["status"], "written")
            self.assertEqual(effect["writer_pcs"], [0x24])

    def test_register_cache_deduplicates_and_is_deterministic(self):
        with tempfile.TemporaryDirectory() as directory:
            index = self.make_index(directory)
            context = {"mem": object(), "functions": [{"entry": 2}, {"entry": 1}]}
            class State:
                trace = []; stopped = "return"
            with patch("sharcfn.load_context", return_value=context) as load, patch("sharc_trace.trace", return_value=[State()]) as trace:
                result = index.query(register_effects=[I.RegisterEffectQuery(2, "R7"), I.RegisterEffectQuery(1, "R6"), I.RegisterEffectQuery(2, "R7")])
                again = index.query(register_effects=[I.RegisterEffectQuery(1, "R6"), I.RegisterEffectQuery(2, "R7")])
            self.assertEqual(load.call_count, 1); self.assertEqual(trace.call_count, 2)
            self.assertEqual(result, again)
            self.assertEqual([x["entry_sw"] for x in result["register_effects"]], [1, 2])

    def test_register_misses_use_bounded_batch_and_parent_orders_results(self):
        with tempfile.TemporaryDirectory() as directory:
            index = self.make_index(directory)
            first = I.RegisterEffectQuery(1, "R6")
            second = I.RegisterEffectQuery(2, "R7")

            def effect(item):
                return {
                    "entry_sw": item.entry_sw,
                    "register": item.register,
                    "status": "preserved",
                    "quantifier": "all retained paths",
                    "writer_pcs": [],
                    "reasons": [],
                }

            with patch.object(
                I,
                "_run_register_effect_queries",
                return_value=[(second, effect(second)), (first, effect(first))],
            ) as run:
                result = index.query(
                    register_effects=[second, first],
                    jobs=4,
                )["register_effects"]

            self.assertEqual(run.call_args.kwargs["jobs"], 4)
            self.assertEqual([item["entry_sw"] for item in result], [1, 2])
            self.assertTrue(all(item["status"] == "preserved" for item in result))

    def test_report_only_dependency_change_keeps_static_cache_warm(self):
        with tempfile.TemporaryDirectory() as directory:
            index = self.make_index(directory)
            original = I._digest_file
            def report_only_change(path):
                return "f" * 64 if path.name == "sharc_discover.py" else original(path)
            with patch.object(I.AnalysisIndex, "_build", wraps=I.AnalysisIndex._build) as build, patch.object(I, "_digest_file", side_effect=report_only_change):
                warm = I.AnalysisIndex.open_or_build(index._metadata["blob_path"], path=index._path, config=I.IndexConfig((1,), 8))
            self.assertEqual(build.call_count, 0)
            self.assertEqual(warm._static_key, index._static_key)

    def test_register_effect_contract_invalidates_old_reducer_rows(self):
        with tempfile.TemporaryDirectory() as directory:
            index = self.make_index(directory)
            item = I.RegisterEffectQuery(0x20, "R6")
            old_request = {"contract": "register-effect/v1", "entry_sw": item.entry_sw,
                           "register": item.register, "max_steps": item.max_steps,
                           "max_states": item.max_states, "max_call_depth": item.max_call_depth,
                           "policy": I.REGISTER_POLICY,
                           "reducer": "conservative-register-paths/v1"}
            old_key = index._query_key(old_request)
            old_value = {"entry_sw": item.entry_sw, "register": item.register,
                         "status": "unknown", "quantifier": "not established",
                         "writer_pcs": [], "reasons": ["trace stop: return without followed call"]}
            with sqlite3.connect(index._path) as con:
                con.execute("INSERT INTO register_results VALUES (?,?,?,?,?,?,1)",
                            (old_key, index._static_key, I._payload(old_request),
                             I._payload(old_value), hashlib.sha256(I._payload(old_value)).hexdigest(),
                             len(I._payload(old_value))))

            class State:
                trace = []
                stopped = "return without followed call"

            context = {"mem": object(), "functions": [{"entry": item.entry_sw}]}
            with patch("sharcfn.load_context", return_value=context), patch("sharc_trace.trace", return_value=[State()]) as trace:
                effect = index.query(register_effects=[item])["register_effects"][0]
            self.assertEqual(trace.call_count, 1)
            self.assertEqual(effect["status"], "preserved")

    def test_query_keys_include_semantics_not_jobs(self):
        with tempfile.TemporaryDirectory() as directory:
            index = self.make_index(directory)
            self.assertNotEqual(index._query_key(index._request_writer(I.WriterTarget(1, 4))), index._query_key(index._request_writer(I.WriterTarget(1, 8))))
            self.assertNotEqual(index._query_key(index._request_register(I.RegisterEffectQuery(1, "R6", max_steps=1))), index._query_key(index._request_register(I.RegisterEffectQuery(1, "R6", max_steps=2))))

    def test_malformed_rows_are_misses_and_writable_cache_repairs_them(self):
        with tempfile.TemporaryDirectory() as directory:
            index = self.make_index(directory); item = I.WriterTarget(1); request = index._request_writer(item); key = index._query_key(request)
            with sqlite3.connect(index._path) as con:
                con.execute("INSERT INTO writer_results VALUES (?,?,?,?,?,?,1)", (key, index._static_key, I._payload(request), b"{bad", "0" * 64, 4))
            with patch(
                "sharcwriters.collect_trace_facts", return_value={"contract": "test"}
            ) as collect, patch(
                "sharcwriters.classify_trace_facts", return_value={(1, 4): {"target": 1}}
            ):
                self.assertEqual(index.query(writer_targets=[item])["writer_targets"], [{"target": 1}])
            self.assertEqual(collect.call_count, 1)
            readonly = I.AnalysisIndex(index._path, index._metadata, readonly=True)
            self.assertEqual(readonly.query(writer_targets=[item])["writer_targets"], [{"target": 1}])
            with self.assertRaisesRegex(ValueError, "readonly"):
                readonly.query(writer_targets=[I.WriterTarget(2)])

    def test_old_object_rejects_replaced_static_key(self):
        with tempfile.TemporaryDirectory() as directory:
            index = self.make_index(directory)
            with sqlite3.connect(index._path) as con: con.execute("UPDATE cache_meta SET value='different' WHERE key='static_key'")
            with self.assertRaisesRegex(ValueError, "replaced"):
                index.query()


if __name__ == "__main__":
    unittest.main()
