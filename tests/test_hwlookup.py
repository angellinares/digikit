# pyright: reportMissingImports=false, reportAttributeAccessIssue=false
import contextlib
import io
import unittest

from emu.hwref import (
    decode_register_value,
    load_reference,
    resolve_address,
    resolve_dma_channel,
    resolve_vector,
)
from tools import hwlookup


class AddressLookupTest(unittest.TestCase):
    def test_resolves_channel_50_citer_using_coldfire_layout(self):
        result = resolve_address(0xFC045654)
        self.assertEqual(result["name"], "EDMA.TCD50.CITER")
        self.assertEqual(result["channel"], 50)
        self.assertEqual(result["width"], 2)

    def test_decodes_channel_50_scatter_gather_csr(self):
        resolved = resolve_address(0xFC04565E)
        self.assertEqual(resolved["name"], "EDMA.TCD50.CSR")
        decoded = decode_register_value(resolved, 0x0012)
        self.assertEqual(decoded["set_fields"], ["E_SG", "INT_MAJOR"])

    def test_resolves_intc1_force_register_and_value(self):
        result = resolve_address(0xFC04C010)
        self.assertEqual(result["name"], "INTC1.INTFRCH")
        decoded = decode_register_value(result, 0x80000000)
        self.assertEqual(decoded["forced_sources"], [63])

    def test_reports_memory_range_for_internal_sram_alias(self):
        result = resolve_address(0x80003CD0)
        self.assertEqual(result["memory_range"]["name"], "internal_sram_backdoor")

    def test_resolves_ssi0_transmit_register(self):
        result = resolve_address(0xFC0BC000)
        self.assertEqual(result["name"], "SSI0.TX0")
        self.assertEqual(result["width"], 4)


class RoutingLookupTest(unittest.TestCase):
    def test_vector_170_is_dma_50_completion(self):
        result = resolve_vector(170)
        self.assertEqual((result["controller"], result["source"]), ("INTC1", 42))
        self.assertEqual(result["assignment"]["description"], "DMA channel 50 transfer complete")
        self.assertEqual(result["assignment"]["clear"], "write EDMA_CINT=50")

    def test_vector_191_is_unassigned_hardware_and_software_forceable(self):
        result = resolve_vector(191)
        self.assertIsNone(result["assignment"]["module"])
        self.assertEqual(result["force_address"], 0xFC04C010)
        self.assertEqual(result["force_bit"], 31)

    def test_low_interrupt_source_uses_low_force_register(self):
        result = resolve_vector(64)
        self.assertEqual((result["controller"], result["source"]), ("INTC0", 0))
        self.assertEqual(result["force_address"], 0xFC048014)
        self.assertEqual(result["force_bit"], 0)

    def test_dma_50_links_ssi0_request_to_vector_170(self):
        result = resolve_dma_channel(50)
        self.assertEqual(result["tcd_address"], 0xFC045640)
        self.assertEqual(result["routing"]["request"], "SSI0_SR[TFE0]")
        self.assertEqual(result["routing"]["completion_vector"], 170)

    def test_invalid_dma_channel_does_not_invent_tcd_address(self):
        result = resolve_dma_channel(64)
        self.assertFalse(result["valid"])
        self.assertNotIn("tcd_address", result)

    def test_manual_facts_retain_provenance(self):
        reference = load_reference()
        source63 = next(
            item
            for item in reference["intc"]["sources"]
            if item["controller"] == "INTC1" and item["source"] == 63
        )
        self.assertIn("pages/p0349.txt", source63["source_ref"])
        self.assertIn("mcf5441x_rm", reference["sources"])


class CliTest(unittest.TestCase):
    def test_human_dma_lookup(self):
        output = io.StringIO()
        with contextlib.redirect_stdout(output):
            hwlookup.main(["dma", "50"])
        text = output.getvalue()
        self.assertIn("SSI0_SR[TFE0]", text)
        self.assertIn("vector 170", text)


if __name__ == "__main__":
    unittest.main()
