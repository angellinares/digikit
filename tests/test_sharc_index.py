"""Synthetic contracts for the persistent SHARC index helpers."""
import os
import pathlib
import sqlite3
import sys
import tempfile
import unittest
from importlib import import_module
from unittest.mock import patch

sys.path.insert(0, str(pathlib.Path(__file__).parents[1] / "tools"))
I = import_module("sharc_index")


class IndexContractTest(unittest.TestCase):
    def test_tri_state_reducer_is_conservative(self):
        self.assertEqual(I.classify_register_paths([{"verified_return": True}])["status"], "preserved")
        self.assertEqual(I.classify_register_paths([{"verified_return": True, "writer_pcs": [9]}])["status"], "written")
        self.assertEqual(I.classify_register_paths([{"verified_return": False, "uncertainty": ["indirect call"]}])["status"], "unknown")

    def test_partial_index_is_rejected_readonly(self):
        with tempfile.TemporaryDirectory() as directory:
            blob = pathlib.Path(directory) / "blob"
            blob.write_bytes(b"synthetic")
            cache = pathlib.Path(directory) / "index.sqlite"
            sqlite3.connect(cache).close()
            with self.assertRaisesRegex(ValueError, "no complete compatible"):
                I.AnalysisIndex.open_or_build(blob, path=cache, readonly=True,
                    config=I.IndexConfig((1,), 8))

    def test_cold_warm_blob_and_dependency_invalidation_rebuild(self):
        with tempfile.TemporaryDirectory() as directory:
            blob = pathlib.Path(directory) / "blob"
            cache = pathlib.Path(directory) / "index.sqlite"
            config = I.IndexConfig((1,), 8)
            blob.write_bytes(b"fixture-a")
            calls = []
            def build(blob_path, cache_path, built_config, fingerprint):
                calls.append((blob_path.read_bytes(), built_config, fingerprint["dependencies"]))
                if cache_path.exists():
                    cache_path.unlink()
                with sqlite3.connect(cache_path) as connection:
                    connection.executescript("create table metadata(key text primary key, value text not null); create table snapshot(id integer primary key, payload text not null);")
                    connection.execute("insert into metadata values (?,?)", ("fingerprint", I._canonical(fingerprint)))
                    connection.execute("insert into metadata values ('complete','1')")
                    connection.execute("insert into snapshot values (1,'{}')")
                return I.AnalysisIndex(cache_path, {**fingerprint, "blob_path": str(blob_path)})
            with patch.object(I.AnalysisIndex, "_build", side_effect=build):
                I.AnalysisIndex.open_or_build(blob, path=cache, config=config)  # cold
                warm_mtime = cache.stat().st_mtime_ns
                I.AnalysisIndex.open_or_build(blob, path=cache, config=config)  # warm
                self.assertEqual(warm_mtime, cache.stat().st_mtime_ns)
                blob.write_bytes(b"fixture-b")
                I.AnalysisIndex.open_or_build(blob, path=cache, config=config)  # blob only
                with patch.object(I, "_digest_file", return_value="0" * 64):
                    I.AnalysisIndex.open_or_build(blob, path=cache, config=config)  # dependency only
                    I.AnalysisIndex.open_or_build(blob, path=cache, config=config)  # warm under same dependency set
            self.assertEqual([item[0] for item in calls], [b"fixture-a", b"fixture-b", b"fixture-b"])
            self.assertEqual([item[1] for item in calls], [config, config, config])

    def test_writer_query_keeps_duplicate_address_widths_distinct(self):
        index = I.AnalysisIndex(pathlib.Path("unused.sqlite"), {
            "config": {"code_blocks": [1], "min_depth": 8}, "blob_path": "blob"
        })
        output = {
            (0x100, 4): {"target": 0x100, "target_width": 4},
            (0x100, 16): {"target": 0x100, "target_width": 16},
        }
        with patch("sharcwriters.run_many", return_value=output):
            result = index.query(writer_targets=[I.WriterTarget(0x100, 16), I.WriterTarget(0x100, 4)])
        self.assertEqual([(item["target"], item["target_width"]) for item in result["writer_targets"]], [(0x100, 4), (0x100, 16)])

    def test_fingerprint_changes_with_config_and_blob(self):
        with tempfile.TemporaryDirectory() as directory:
            blob = pathlib.Path(directory) / "blob"
            blob.write_bytes(b"a")
            first = I.AnalysisIndex._fingerprint(blob, I.IndexConfig((1,), 8))
            self.assertNotEqual(first, I.AnalysisIndex._fingerprint(blob, I.IndexConfig((2,), 8)))
            blob.write_bytes(b"b")
            self.assertNotEqual(first, I.AnalysisIndex._fingerprint(blob, I.IndexConfig((1,), 8)))
            with patch.object(I, "_digest_file", return_value="0" * 64):
                self.assertNotEqual(first, I.AnalysisIndex._fingerprint(blob, I.IndexConfig((1,), 8)))


if __name__ == "__main__":
    unittest.main()
