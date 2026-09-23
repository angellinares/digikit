"""Contracts for the report-only SHARC discovery comparison."""
import importlib.util
import json
import os
import pathlib
import subprocess
import sys
import tempfile
import unittest


TOOL = pathlib.Path(__file__).parents[1] / "tools" / "sharc_compare.py"
spec = importlib.util.spec_from_file_location("sharc_compare", TOOL)
assert spec is not None and spec.loader is not None
C = importlib.util.module_from_spec(spec)
sys.modules[spec.name] = C
spec.loader.exec_module(C)


def report(digest, *, forms=("9b_abs",), stops=None, functions=None):
    return {
        "schema": "sharc-discovery/v1",
        "provenance": {
            "blob": {"sha256": digest, "size": 10, "final_marker": True},
            "manifest": {"schema": "sharc-discovery-manifest/v1", "sha256": "m" * 64},
        },
        "coverage": {
            "functions": len(functions or []),
            "decoded_instruction_pcs": 12,
            "indirect_sites": len(forms),
            "pointer_table_runs": 2,
            **({"stop_reasons": stops} if stops is not None else {}),
        },
        "functions": functions or [],
        "indirect_sites": [{"form": form} for form in forms],
        "pointer_tables": [{}, {}],
    }


def function(n_insns=4, feature=2):
    features = {key: 0 for key in C.STRUCTURAL_FEATURES}
    features.update({"compute_total": feature, "mem_load": 1})
    return {
        "id": "address-dependent-id",
        "entry_sw": 0x1234,
        "n_insns": n_insns,
        "features": features,
        "callers": ["caller"],
        "callees": [],
        "unresolved_callees": [],
        "label": "must-not-enter-signature",
    }


class CompareTest(unittest.TestCase):
    def test_input_order_and_path_do_not_change_canonical_result(self):
        left = report("b" * 64, forms=("9b_abs", "9a_abs"), stops={"unsupported": 2}, functions=[function()])
        right = report("a" * 64, forms=("9b_abs",), stops={"external": 1}, functions=[function(), function(5)])
        self.assertEqual(C.compare_reports(left, right), C.compare_reports(right, left))
        with tempfile.TemporaryDirectory() as directory:
            directory = pathlib.Path(directory)
            first, second = directory / "one.json", directory / "nested" / "two.json"
            second.parent.mkdir()
            first.write_text(json.dumps(left))
            second.write_text(json.dumps(right))
            one = subprocess.run([sys.executable, str(TOOL), str(first), str(second)], check=True, text=True, capture_output=True).stdout
            two = subprocess.run([sys.executable, str(TOOL), str(second), str(first)], check=True, text=True, capture_output=True).stdout
            seed_one = subprocess.run(
                [sys.executable, str(TOOL), str(first), str(second)], check=True,
                text=True, capture_output=True, env={**os.environ, "PYTHONHASHSEED": "1"},
            ).stdout
            seed_two = subprocess.run(
                [sys.executable, str(TOOL), str(first), str(second)], check=True,
                text=True, capture_output=True, env={**os.environ, "PYTHONHASHSEED": "999"},
            ).stdout
        self.assertEqual(one, two)
        self.assertEqual(seed_one, seed_two)
        result = json.loads(one)
        signatures = result["normalized_function_signatures"]["value"]
        self.assertEqual(len(signatures["common"]), 1)
        self.assertNotIn("label", signatures["common"][0]["signature"])
        self.assertEqual(len(signatures["one_to_one_candidates"]), 1)
        self.assertEqual(
            signatures["one_to_one_candidates"][0]["functions"][0]["label"],
            "must-not-enter-signature",
        )

    def test_missing_optional_evidence_is_explicitly_unavailable(self):
        left = {"schema": "sharc-discovery/v1", "provenance": {}}
        right = report("a" * 64)
        result = C.compare_reports(left, right)
        missing = next(item for item in result["reports"] if item["identity"]["image_sha256"] is None)
        self.assertEqual(missing["counts"]["instructions"]["status"], "unavailable")
        self.assertEqual(missing["indirect_site_form_histogram"]["status"], "unavailable")
        self.assertEqual(missing["stop_reason_histogram"]["status"], "unavailable")
        self.assertEqual(result["normalized_function_signatures"]["status"], "unavailable")

    def test_cli_output_is_stable_and_written_canonically(self):
        with tempfile.TemporaryDirectory() as directory:
            directory = pathlib.Path(directory)
            first, second, output = directory / "a.json", directory / "b.json", directory / "out" / "comparison.json"
            first.write_text(json.dumps(report("a" * 64, functions=[function()])))
            second.write_text(json.dumps(report("b" * 64, functions=[function(6)])))
            subprocess.run([sys.executable, str(TOOL), str(first), str(second), "-o", str(output)], check=True)
            written = output.read_text()
            stdout = subprocess.run([sys.executable, str(TOOL), str(first), str(second)], check=True, text=True, capture_output=True).stdout
        self.assertEqual(written, stdout)
        self.assertEqual(written, json.dumps(json.loads(written), indent=2, sort_keys=True) + "\n")


if __name__ == "__main__":
    unittest.main()
