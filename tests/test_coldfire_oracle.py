# pyright: reportMissingImports=false
"""Synthetic GNU/Ghidra checks for the MCF5441x instruction subset.

The fixture is original manual-derived input. GNU Binutils is an optional,
external GPL oracle; no GNU implementation or upstream test text is imported.
"""

import json
import os
import re
import shutil
import subprocess
import tempfile
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
FIXTURES = ROOT / "tests" / "fixtures" / "coldfire"
EXPECTED = FIXTURES / "mcf54415-oracle.json"
LANGUAGES = ROOT / "tools" / "ghidra" / "ColdfireEMAC" / "data" / "languages"


def load_expected():
    with open(EXPECTED, encoding="utf-8") as f:
        return json.load(f)


def have_pypcode():
    try:
        import pypcode  # noqa: F401
    except ImportError:
        return False
    return True


def binutils_tool(name):
    prefix = Path(
        os.environ.get(
            "M68K_ELF_PREFIX",
            ROOT / "out" / "toolchains" / "prefix" / "binutils-2.47",
        )
    )
    local = prefix / "bin" / ("m68k-elf-" + name)
    if local.exists():
        return str(local)
    return shutil.which("m68k-elf-" + name)


class GnuOracleTest(unittest.TestCase):
    def setUp(self):
        self.expected = load_expected()
        assembler = binutils_tool("as")
        objcopy = binutils_tool("objcopy")
        objdump = binutils_tool("objdump")
        if not assembler or not objcopy or not objdump:
            self.skipTest("needs an m68k-elf GNU binutils build")
        self.assembler: str = assembler
        self.objcopy: str = objcopy
        self.objdump: str = objdump

    def assemble_text(self, cpu, directory):
        obj = Path(directory) / ("mcf" + cpu + ".o")
        raw = Path(directory) / ("mcf" + cpu + ".bin")
        subprocess.run(
            [
                self.assembler,
                "-mcpu=" + cpu,
                "-o",
                str(obj),
                str(FIXTURES / self.expected["source"]),
            ],
            check=True,
            capture_output=True,
            text=True,
        )
        subprocess.run(
            [
                self.objcopy,
                "--only-section=.text",
                "-O",
                "binary",
                str(obj),
                str(raw),
            ],
            check=True,
            capture_output=True,
            text=True,
        )
        return raw.read_bytes()

    def disassemble(self, obj):
        proc = subprocess.run(
            [self.objdump, "-dr", str(obj)],
            check=True,
            capture_output=True,
            text=True,
        )
        instructions = []
        pattern = re.compile(
            r"^\s*([0-9a-f]+):\s+((?:(?:[0-9a-f]{4})\s+)+)(\S.*)$"
        )
        for line in proc.stdout.splitlines():
            match = pattern.match(line)
            if match:
                instructions.append(
                    (int(match.group(1), 16), "".join(match.group(2).split()), match.group(3))
                )
        return instructions

    def test_5441x_targets_produce_checked_bytes(self):
        with tempfile.TemporaryDirectory(prefix="coldfire-gnu-") as tmp:
            for cpu in self.expected["gnu_oracle"]["positive_cpus"]:
                with self.subTest(cpu=cpu):
                    actual = self.assemble_text(cpu, tmp)
                    self.assertEqual(actual.hex(), self.expected["code_hex"])

    def test_54415_objdump_text_matches_checked_oracle(self):
        with tempfile.TemporaryDirectory(prefix="coldfire-gnu-disasm-") as tmp:
            self.assemble_text("54415", tmp)
            actual = self.disassemble(Path(tmp) / "mcf54415.o")
        expected = [
            (insn["offset"], insn["bytes"], insn["gnu"])
            for insn in self.expected["instructions"]
        ]
        self.assertEqual(actual, expected)

    def test_5206_rejects_the_mcf5441x_fixture(self):
        with tempfile.TemporaryDirectory(prefix="coldfire-gnu-negative-") as tmp:
            obj = Path(tmp) / "mcf5206.o"
            proc = subprocess.run(
                [
                    self.assembler,
                    "-mcpu=" + self.expected["gnu_oracle"]["negative_cpu"],
                    "-o",
                    str(obj),
                    str(FIXTURES / self.expected["source"]),
                ],
                capture_output=True,
                text=True,
            )
            self.assertNotEqual(proc.returncode, 0)
            self.assertFalse(obj.exists())


@unittest.skipUnless(have_pypcode(), "needs pypcode (pyproject.toml)")
class GhidraOracleTest(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        import pypcode

        cls.expected = load_expected()
        cls.tmp = tempfile.mkdtemp(prefix="coldfire-sleigh-")
        dst = Path(cls.tmp)
        for src in LANGUAGES.iterdir():
            if src.suffix in {
                ".sinc",
                ".slaspec",
                ".ldefs",
                ".pspec",
                ".cspec",
                ".dwarf",
            }:
                shutil.copy(src, dst / src.name)
        assert pypcode.__file__ is not None
        sleigh = Path(pypcode.__file__).resolve().parent / "bin" / "sleigh"
        if not sleigh.exists():
            raise unittest.SkipTest("the installed pypcode package has no sleigh compiler")
        subprocess.run(
            [
                str(sleigh),
                str(dst / "coldfire_emac.slaspec"),
                str(dst / "coldfire_emac.sla"),
            ],
            check=True,
            capture_output=True,
            text=True,
        )
        arch = pypcode.Arch("68000", str(dst / "coldfire_emac.ldefs"))
        cls.ctx = pypcode.Context(arch.languages[0])

    @classmethod
    def tearDownClass(cls):
        shutil.rmtree(cls.tmp, ignore_errors=True)

    def test_current_language_decode_and_lengths_are_pinned(self):
        code = bytes.fromhex(self.expected["code_hex"])
        actual = list(self.ctx.disassemble(code).instructions)
        expected = self.expected["instructions"]
        self.assertEqual(len(actual), len(expected))
        for got, want in zip(actual, expected):
            with self.subTest(offset=want["offset"]):
                text = got.mnem + ((" " + got.body) if got.body else "")
                self.assertEqual(got.addr.offset, want["offset"])
                self.assertEqual(got.length, want["length"])
                self.assertEqual(text, want["ghidra_current"])

    def test_selected_pcode_data_flow_is_pinned(self):
        code = bytes.fromhex(self.expected["code_hex"])
        for insn in self.expected["instructions"]:
            fragments = insn.get("pcode_contains", [])
            if not fragments:
                continue
            start = insn["offset"]
            raw = code[start : start + insn["length"]]
            ops = self.ctx.translate(raw, max_instructions=1).ops
            text = "\n".join(str(op) for op in ops)
            with self.subTest(offset=start):
                for fragment in fragments:
                    self.assertIn(fragment, text)

    def test_only_mcf5441x_control_register_names_are_known_gaps(self):
        gaps = [
            insn
            for insn in self.expected["instructions"]
            if "ghidra_target" in insn
        ]
        self.assertEqual(
            [insn["ghidra_target"].rsplit(",", 1)[1] for insn in gaps],
            ["ACR4", "ACR5", "ACR6", "ACR7", "RGPIOBAR"],
        )
        self.assertTrue(all("UNK_CTL_" in insn["ghidra_current"] for insn in gaps))


if __name__ == "__main__":
    unittest.main()
