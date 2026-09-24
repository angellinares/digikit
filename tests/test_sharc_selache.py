"""Selache remains an optional, pinned differential oracle."""

import os
import pathlib
import sys
import unittest

sys.path.insert(0, str(pathlib.Path(__file__).parents[1] / "tools"))

import sharc_selache as S  # noqa: E402  # pyright: ignore[reportMissingImports]


class SelacheAdapterTest(unittest.TestCase):
    def test_parcel_order_transform_is_involutive(self):
        data = bytes.fromhex("023e38108022")
        swapped = bytes.fromhex("3e0210382280")
        self.assertEqual(S.swap_parcels(data), swapped)
        self.assertEqual(S.swap_parcels(swapped), data)

    def test_parse_listing_retains_external_text_and_extent(self):
        listing = """
---- Section: seg_pmco [1] ----

  00000000   c001   r0=r0+r1
  00000001   0f0212345678   r2=0x12345678
"""
        rows = S.parse_listing(listing)
        self.assertEqual(
            [(row.parcel_address, row.raw.hex(), row.text) for row in rows],
            [(0, "c001", "r0=r0+r1"), (1, "0f0212345678", "r2=0x12345678")],
        )

    def test_compare_listing_exposes_an_extent_disagreement(self):
        comparison = S.compare_listing("  00000000   c001   r0=r0+r1\n")
        self.assertEqual(len(comparison.instructions), 1)
        row = comparison.instructions[0]
        self.assertEqual(row.external.raw, bytes.fromhex("c001"))
        self.assertEqual(row.boot_bytes, bytes.fromhex("01c0"))
        self.assertIsNone(row.native_form_id)
        self.assertEqual(row.native_candidates, ("2b",))
        self.assertFalse(row.extent_agrees)

    def test_known_disagreements_are_native_regression_fixtures(self):
        fixtures = {fixture.id: fixture for fixture in S.KNOWN_DISAGREEMENTS}
        pair = fixtures["adjacent-type3c-width"]
        comparison = S.compare_external_bytes(pair.external_bytes)
        self.assertEqual(
            tuple(item.native_form_id for item in comparison.instructions),
            pair.expected_native_forms,
        )

        fused = fixtures["type4a-width"]
        comparison = S.compare_external_bytes(fused.external_bytes)
        self.assertEqual(
            tuple(item.native_form_id for item in comparison.instructions),
            fused.expected_native_forms,
        )

    @unittest.skipUnless(os.environ.get("SELACHE_DIR"), "SELACHE_DIR not set")
    def test_pinned_external_round_trip(self):
        oracle = S.SelacheOracle.open(pathlib.Path(os.environ["SELACHE_DIR"]))
        result = oracle.assemble_text(
            ".section/pm seg_pmco;\nNOP;\n"
        )
        self.assertEqual(result.revision, S.PINNED_REVISION)
        self.assertEqual({item.name for item in result.tool_fingerprints}, {"selas", "seldump"})
        self.assertTrue(all(len(item.sha256) == 64 for item in result.tool_fingerprints))
        self.assertEqual(
            tuple(item.native_form_id for item in result.comparison.instructions),
            ("21c",),
        )
        self.assertTrue(all(item.extent_agrees for item in result.comparison.instructions))


if __name__ == "__main__":
    unittest.main()
