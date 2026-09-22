"""The p-code of the generated SHARC+ language, and tools/sharcpcode.py.

Generates the language from decode_table.json in a temporary directory,
compiles it with the sleigh compiler bundled with pypcode, and lifts
hand-encoded instructions. Needs no firmware and no Ghidra."""

import os
import shutil
import sqlite3
import subprocess
import sys
import tempfile
import unittest

sys.path.insert(
    0,
    os.path.join(os.path.dirname(os.path.dirname(os.path.abspath(__file__))), "tools"),
)

import sharc_disasm  # noqa: E402  # pyright: ignore[reportMissingImports]
import sharc_visa_tables as T  # noqa: E402  # pyright: ignore[reportMissingImports]
import sharcpcode  # noqa: E402  # pyright: ignore[reportMissingImports]
from test_sharc_disasm import encode  # noqa: E402

TARGET_SW = 0x1C1400
COND_EQ = 0x00


def have_pypcode():
    try:
        import pypcode  # noqa: F401  # pyright: ignore[reportMissingImports]
    except ImportError:
        return False
    return True


def field(name, label, value):
    """-> the bits that put value into field `label` of form `name`."""
    hi, lo = T.get_type(name)["fields"][label]
    return (value & ((1 << (hi - lo + 1)) - 1)) << lo


def branch(name, cond=None, b=None, target=None):
    """-> bytes of a form-`name` branch with the given cond, b bit and absolute target."""
    extra = 0
    if cond is not None:
        extra |= field(name, "cond[4:0]", cond)
    if b is not None:
        extra |= field(name, "b", b)
    if target is not None:
        extra |= field(name, "addr[23:16]", target >> 16) | field(
            name, "addr[15:0]", target
        )
    return encode(name, extra)


def type14a(**values):
    """Encode a scalar Type14a direct-DM transfer, defaulting to R4 at 0x254d98."""
    defaults = {
        "g": 0,
        "d": 0,
        "l": 0,
        "ureg[6:0]": 4,
        "addr[31:16]": 0x25,
        "addr[15:0]": 0x4D98,
    }
    defaults.update(values)
    return encode(
        "14a", sum(field("14a", name, value) for name, value in defaults.items())
    )


def type3b(**values):
    """Encode a Type3b instruction, defaulting to raw 493e0e3f's fields."""
    defaults = {
        "u": 0,
        "i[2:0]": 4,
        "m[2:0]": 4,
        "cond[4:0]": sharcpcode.COND_TRUE,
        "g": 0,
        "d": 0,
        "l": 0,
        "ureg[6:0]": 28,
        "w": 1,
        "x": 1,
    }
    defaults.update(values)
    return encode(
        "3b", sum(field("3b", name, value) for name, value in defaults.items())
    )


def type15b(**values):
    """Encode a Type15b indexed DM instruction, defaulting to I3+0 -> R5."""
    defaults = {
        "i[2:0]": 3,
        "d": 0,
        "l": 0,
        "g": 0,
        "ureg[6:0]": 5,
        "data[6:0]": 0,
    }
    defaults.update(values)
    return encode(
        "15b", sum(field("15b", name, value) for name, value in defaults.items())
    )


def type4a(**values):
    """Encode a Type4a indexed DM instruction, defaulting to a TRUE-cond,
    pre-modify (u=0) load: I2+0 -> R5, no compute."""
    defaults = {
        "i[2:0]": 2,
        "g": 0,
        "d": 0,
        "u": 0,
        "cond[4:0]": sharcpcode.COND_TRUE,
        "data[5:5]": 0,
        "data[4:0]": 0,
        "dreg[3:0]": 5,
        "compute[22:16]": 0,
        "compute[15:0]": 0,
    }
    defaults.update(values)
    return encode(
        "4a", sum(field("4a", name, value) for name, value in defaults.items())
    )


def type3a(**values):
    """Encode a Type3a indexed DM instruction, defaulting to a TRUE-cond,
    pre-modify (u=0) load: I2+M3*4 -> R5, no compute."""
    defaults = {
        "u": 0,
        "i": 2,
        "m": 3,
        "cond": sharcpcode.COND_TRUE,
        "g": 0,
        "d": 0,
        "l": 0,
        "ureg": 5,
        "compute": 0,
    }
    defaults.update(values)
    return encode(
        "3a", sum(field("3a", name, value) for name, value in defaults.items())
    )


def immediate_move(name, ureg, data):
    """-> bytes of a Type17 immediate move to UREG code `ureg`."""
    extra = field(name, "ureg[6:0]", ureg)
    if name == "17a":
        extra |= field(name, "data[31:16]", data >> 16)
        extra |= field(name, "data[15:0]", data)
    else:
        extra |= field(name, "data[15:0]", data)
    return encode(name, extra)


def eval_pcode(ops, registers=None):
    """Evaluate a straight-line p-code op sequence with no control flow,
    stopping at the first LOAD or STORE. Returns (op, address_value) where
    address_value is that op's memory-offset input (inputs[1]).

    This models exactly the opcodes the DM byte-address -> ram-unit
    translation uses (tools/sharcspec/ghidra/gen_sleigh.py
    dm_byte_addr_to_ram_unit): INT_ZEXT/INT_SEXT, INT_LEFT/INT_RIGHT,
    INT_OR/INT_AND, INT_ADD/INT_SUB/INT_MULT, INT_LESS/INT_LESSEQUAL,
    BOOL_AND/BOOL_NEGATE, COPY. `registers` seeds any register-space input
    (e.g. I4, M4) the sequence reads before writing.
    """
    registers = dict(registers or {})
    unique = {}

    def value(vn):
        space = vn.space.name
        if space == "const":
            return vn.offset
        if space == "register":
            name = vn.getRegisterName()
            if name not in registers:
                raise AssertionError("eval_pcode: no seed value for register %s" % name)
            return registers[name]
        if space == "unique":
            return unique[vn.offset]
        raise AssertionError("eval_pcode: unexpected input space %r" % space)

    def store(op, result):
        mask = (1 << (op.output.size * 8)) - 1
        result &= mask
        space = op.output.space.name
        if space == "unique":
            unique[op.output.offset] = result
        elif space == "register":
            registers[op.output.getRegisterName()] = result
        else:
            raise AssertionError("eval_pcode: unexpected output space %r" % space)

    for op in ops:
        name = op.opcode.name
        if name in ("LOAD", "STORE"):
            return op, value(op.inputs[1])
        ins = [value(i) for i in op.inputs]
        if name == "INT_ZEXT":
            result = ins[0]
        elif name == "INT_SEXT":
            src = op.inputs[0]
            result = ins[0]
            if result & (1 << (src.size * 8 - 1)):
                result -= 1 << (src.size * 8)
        elif name == "COPY":
            result = ins[0]
        elif name == "INT_LEFT":
            result = ins[0] << ins[1]
        elif name == "INT_RIGHT":
            result = ins[0] >> ins[1]
        elif name == "INT_OR":
            result = ins[0] | ins[1]
        elif name == "INT_AND":
            result = ins[0] & ins[1]
        elif name == "INT_ADD":
            result = ins[0] + ins[1]
        elif name == "INT_SUB":
            result = ins[0] - ins[1]
        elif name == "INT_MULT":
            result = ins[0] * ins[1]
        elif name == "INT_LESS":
            result = 1 if ins[0] < ins[1] else 0
        elif name == "INT_LESSEQUAL":
            result = 1 if ins[0] <= ins[1] else 0
        elif name == "BOOL_AND":
            result = 1 if (ins[0] and ins[1]) else 0
        elif name == "BOOL_NEGATE":
            result = 0 if ins[0] else 1
        else:
            raise AssertionError("eval_pcode does not model opcode %s" % name)
        store(op, result)
    raise AssertionError("eval_pcode: no LOAD/STORE op in sequence")


def run_pcode(ops, registers=None, max_steps=1000):
    """Execute a full p-code op list, including intra-instruction branching
    (CBRANCH/BRANCH to a relative op index, exactly what `goto <label>;`
    compiles to in this language's Type15b/4a/3a indexed-memory semantics --
    see tools/sharcspec/ghidra/gen_sleigh.py's direction_branch,
    premodify_or_post/postmodify_update and compute_marker_lines). Unlike
    eval_pcode (which stops at the first LOAD/STORE and has no branch
    support, and stays as-is for the existing Type14a/Type3b tests), this
    walks to the end of the op list, following every branch actually taken,
    and records every LOAD/STORE and CALLOTHER (condition/compute/circular)
    along the way, plus the final register values.

    `registers` seeds initial register values (by name); unseeded registers
    read before being written raise, same as eval_pcode. Returns
    {'registers': {name: value}, 'loads': [{'address','size'}, ...],
    'stores': [{'address','value','size'}, ...],
    'callothers': [{'name','args'}, ...]}."""
    registers = dict(registers or {})
    unique = {}

    def value(vn):
        space = vn.space.name
        if space == "const":
            return vn.offset
        if space == "register":
            name = vn.getRegisterName()
            if name not in registers:
                raise AssertionError("run_pcode: no seed value for register %s" % name)
            return registers[name]
        if space == "unique":
            if vn.offset not in unique:
                raise AssertionError(
                    "run_pcode: unique %#x read before write" % vn.offset
                )
            return unique[vn.offset]
        raise AssertionError("run_pcode: unexpected input space %r" % space)

    def store(op, result):
        mask = (1 << (op.output.size * 8)) - 1
        result &= mask
        space = op.output.space.name
        if space == "unique":
            unique[op.output.offset] = result
        elif space == "register":
            registers[op.output.getRegisterName()] = result
        else:
            raise AssertionError("run_pcode: unexpected output space %r" % space)

    loads, stores, callothers = [], [], []
    idx = 0
    steps = 0
    while idx < len(ops):
        steps += 1
        if steps > max_steps:
            raise AssertionError(
                "run_pcode: exceeded max_steps (%d); infinite loop?" % max_steps
            )
        op = ops[idx]
        name = op.opcode.name
        if name == "CBRANCH":
            taken = value(op.inputs[1])
            idx = idx + op.inputs[0].offset if taken else idx + 1
            continue
        if name == "BRANCH":
            idx = idx + op.inputs[0].offset
            continue
        if name == "CALLOTHER":
            callothers.append(
                {
                    "name": op.inputs[0].getUserDefinedOpName(),
                    "args": [value(i) for i in op.inputs[1:]],
                }
            )
            idx += 1
            continue
        if name == "LOAD":
            loads.append(
                {
                    "address": value(op.inputs[1]),
                    "size": op.output.size if op.output else None,
                }
            )
            if op.output is not None:
                # Loaded content isn't modelled (no backing memory); zero
                # keeps downstream dataflow well-defined without asserting
                # anything about what was "read".
                store(op, 0)
            idx += 1
            continue
        if name == "STORE":
            stores.append(
                {
                    "address": value(op.inputs[1]),
                    "value": value(op.inputs[2]),
                    "size": op.inputs[2].size,
                }
            )
            idx += 1
            continue
        ins = [value(i) for i in op.inputs]
        if name == "INT_ZEXT":
            result = ins[0]
        elif name == "INT_SEXT":
            src = op.inputs[0]
            result = ins[0]
            if result & (1 << (src.size * 8 - 1)):
                result -= 1 << (src.size * 8)
        elif name == "COPY":
            result = ins[0]
        elif name == "INT_LEFT":
            result = ins[0] << ins[1]
        elif name == "INT_RIGHT":
            result = ins[0] >> ins[1]
        elif name == "INT_OR":
            result = ins[0] | ins[1]
        elif name == "INT_AND":
            result = ins[0] & ins[1]
        elif name == "INT_ADD":
            result = ins[0] + ins[1]
        elif name == "INT_SUB":
            result = ins[0] - ins[1]
        elif name == "INT_MULT":
            result = ins[0] * ins[1]
        elif name == "INT_LESS":
            result = 1 if ins[0] < ins[1] else 0
        elif name == "INT_LESSEQUAL":
            result = 1 if ins[0] <= ins[1] else 0
        elif name == "INT_EQUAL":
            result = 1 if ins[0] == ins[1] else 0
        elif name == "INT_NOTEQUAL":
            result = 1 if ins[0] != ins[1] else 0
        elif name == "BOOL_AND":
            result = 1 if (ins[0] and ins[1]) else 0
        elif name == "BOOL_NEGATE":
            result = 0 if ins[0] else 1
        else:
            raise AssertionError("run_pcode does not model opcode %s" % name)
        if op.output is not None:
            store(op, result)
        idx += 1
    return {
        "registers": registers,
        "loads": loads,
        "stores": stores,
        "callothers": callothers,
    }


class GeneratorSource(unittest.TestCase):
    def test_type14a_and_type3b_specializations_coexist(self):
        """Pure generator-output regression: neither form may erase the other."""
        tmp = tempfile.mkdtemp(prefix="sharcspec-source-")
        self.addCleanup(shutil.rmtree, tmp, True)
        spec = os.path.join(tmp, "sharcspec")
        shutil.copytree(
            os.path.join(sharcpcode.TOOLS, "sharcspec"),
            spec,
            ignore=shutil.ignore_patterns("SHARC_VISA", "__pycache__"),
        )
        subprocess.run(
            [sys.executable, os.path.join(spec, "ghidra", "gen_sleigh.py")],
            check=True,
            capture_output=True,
        )
        with open(
            os.path.join(
                spec, "ghidra", "SHARC_VISA", "data", "languages", "sharc_visa.slaspec"
            )
        ) as f:
            generated = f.read()
        self.assertIn("type14a_scalar_w0_9_9=0x0", generated)
        self.assertIn("type14a_scalar_w0_8_8=0x0", generated)
        self.assertIn("type14a_scalar_w0_8_8=0x1", generated)
        self.assertIn("ureg_w0_6_0 = *[ram]:4 unit;", generated)
        self.assertIn("*[ram]:4 unit = ureg_w0_6_0;", generated)
        self.assertIn("type3b_exact_w0_12_12=0x0", generated)
        self.assertIn("I12 = *[ram]:4 unit;", generated)
        self.assertGreaterEqual(generated.count(":Type14a "), 3)
        self.assertGreaterEqual(generated.count(":Type3b "), 2)


@unittest.skipUnless(have_pypcode(), "needs pypcode (pyproject.toml)")
class GeneratedLanguage(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.tmp = tempfile.mkdtemp(prefix="sharcpcode-")
        spec = os.path.join(cls.tmp, "sharcspec")
        shutil.copytree(
            os.path.join(sharcpcode.TOOLS, "sharcspec"),
            spec,
            ignore=shutil.ignore_patterns("SHARC_VISA", "__pycache__"),
        )
        subprocess.run(
            [sys.executable, os.path.join(spec, "ghidra", "gen_sleigh.py")],
            check=True,
            capture_output=True,
        )
        src = os.path.join(spec, "ghidra", "SHARC_VISA", "data", "languages")
        cls.lint, ldefs = sharcpcode.build_language(
            src,
            os.path.join(cls.tmp, "lang"),
            sharcpcode.find_sleigh(prefer_ghidra=False),
        )
        cls.ctx = (
            sharcpcode.load_context(ldefs)
            if cls.lint["compile"]["returncode"] == 0
            else None
        )
        cls.temp_slaspec = os.path.join(src, "sharc_visa.slaspec")
        cls.temp_sla = os.path.join(cls.tmp, "lang", "sharc_visa.sla")
        cls.artifact_diagnostics = {
            "temporary_slaspec": cls.temp_slaspec,
            "temporary_slaspec_sha256": sharcpcode.sha256_file(cls.temp_slaspec),
            "temporary_sla": cls.temp_sla,
            "temporary_sla_sha256": sharcpcode.sha256_file(cls.temp_sla)
            if os.path.exists(cls.temp_sla)
            else None,
            "in_tree_slaspec": os.path.join(sharcpcode.LANG_DIR, "sharc_visa.slaspec"),
            "in_tree_sla": os.path.join(sharcpcode.LANG_DIR, "sharc_visa.sla"),
        }
        for kind in ("slaspec", "sla"):
            path = cls.artifact_diagnostics[f"in_tree_{kind}"]
            cls.artifact_diagnostics[f"in_tree_{kind}_sha256"] = (
                sharcpcode.sha256_file(path) if os.path.exists(path) else None
            )

    @classmethod
    def tearDownClass(cls):
        shutil.rmtree(cls.tmp, ignore_errors=True)

    def setUp(self):
        if self.ctx is None:
            self.fail("the generated language does not compile: %s" % self.lint)

    def lift(self, buf, sw=0x1000):
        length, ops = sharcpcode.lift_one(self.ctx, buf, 2 * sw)
        self.assertIsNotNone(length, "the language does not decode %s" % buf.hex())
        return [op.opcode.name for op in ops], ops

    def test_compiles_without_bad_pcode(self):
        """No errors, pattern conflicts, unnecessary extensions, dead or colliding temporaries."""
        self.assertEqual(self.lint["returncode"], 0, self.lint)
        bad = {
            k: v
            for k, v in self.lint["counts"].items()
            if k in sharcpcode.REGRESSING_LINT
        }
        self.assertEqual(bad, {}, self.lint["examples"])

    def test_dreg_attachment_maps_zero_and_fifteen_to_data_registers(self):
        """The generated dreg/cdreg fields attach to the R0-R15 table."""
        self.assertEqual(self.lint["returncode"], 0, self.lint)
        slaspec = os.path.join(
            self.tmp,
            "sharcspec",
            "ghidra",
            "SHARC_VISA",
            "data",
            "languages",
            "sharc_visa.slaspec",
        )
        with open(slaspec) as f:
            attach = next(line for line in f if line.startswith("attach variables"))
        fields, registers = (
            attach.removeprefix("attach variables [ ")
            .removesuffix(" ];\n")
            .split(" ] [ ")
        )
        self.assertEqual(
            set(fields.split()), {"cdreg_w0_9_6", "dreg_w0_3_0", "dreg_w1_10_7"}
        )
        for encoded, register in ((0, "R0"), (15, "R15")):
            with self.subTest(encoded=encoded):
                self.assertEqual(registers.split()[encoded], register)

    def test_type17a_writes_full_32_bit_immediate_to_attached_ureg(self):
        """Type17a combines both data halves before its explicit UREG write."""
        for ureg, register in ((0x07, "R7"), (0x72, "MODE1")):
            with self.subTest(register=register):
                names, ops = self.lift(immediate_move("17a", ureg, 0x89ABCDEF))
                self.assertTrue(names)
                writes = [op for op in ops if op.output and op.output.getRegisterName()]
                self.assertEqual(
                    [op.output.getRegisterName() for op in writes], [register]
                )
                self.assertEqual(names, ["INT_ZEXT", "INT_LEFT", "INT_ZEXT", "INT_OR"])
                self.assertEqual(ops[0].inputs[0].offset, 0x89AB)
                self.assertEqual(ops[1].inputs[1].offset, 16)
                self.assertEqual(ops[2].inputs[0].offset, 0xCDEF)

    def test_type17b_sign_extends_without_simd_complementary_write(self):
        """Type17b writes only the explicit UREG; SIMD CUREG is deferred."""
        for data, expected in ((0x0ABC, 0x00000ABC), (0xFFFF, 0xFFFFFFFF)):
            with self.subTest(data=f"0x{data:04x}"):
                names, ops = self.lift(immediate_move("17b", 0x05, data))
                self.assertTrue(names)
                writes = [op for op in ops if op.output and op.output.getRegisterName()]
                self.assertEqual([op.output.getRegisterName() for op in writes], ["R5"])
                self.assertEqual(writes[0].opcode.name, "INT_SEXT")
                source = writes[0].inputs[0]
                self.assertEqual(source.offset, data)
                self.assertEqual(writes[0].output.size, 4)
                lifted_value = source.offset
                if lifted_value & (1 << (source.size * 8 - 1)):
                    lifted_value |= 0xFFFFFFFF << (source.size * 8)
                self.assertEqual(lifted_value & 0xFFFFFFFF, expected)

    def test_temporary_artifacts_match_in_tree_when_present(self):
        """Report both paths/hashes if a generated or compiled artifact goes stale."""
        if self.artifact_diagnostics["in_tree_slaspec_sha256"] is not None:
            self.assertEqual(
                self.artifact_diagnostics["temporary_slaspec_sha256"],
                self.artifact_diagnostics["in_tree_slaspec_sha256"],
                self.artifact_diagnostics,
            )
        if self.artifact_diagnostics["in_tree_sla_sha256"] is not None:
            self.assertEqual(
                self.artifact_diagnostics["temporary_sla_sha256"],
                self.artifact_diagnostics["in_tree_sla_sha256"],
                self.artifact_diagnostics,
            )

    def test_type14a_scalar_direct_dm_load_and_store(self):
        # Capture words are big-endian; pypcode receives little-endian words.
        raw_store = bytes.fromhex("110400254d98")
        observed_store = b"".join(
            raw_store[i : i + 2][::-1] for i in range(0, len(raw_store), 2)
        )
        self.assertEqual(observed_store, type14a(d=1))
        raw_load = bytes.fromhex("100400254d98")
        observed_load = b"".join(
            raw_load[i : i + 2][::-1] for i in range(0, len(raw_load), 2)
        )
        self.assertEqual(observed_load, type14a(d=0))
        # high16/low16 assemble the raw DM BYTE address; the isl2/unit chain
        # then translates it into a ram-space unit (addr >> 1, with an
        # L2-window correction) so Ghidra's own wordsize-2 LOAD/STORE scaling
        # (byte offset = 2*unit) recovers the original byte address.
        expected_ops = [
            "INT_ZEXT", "INT_LEFT", "INT_ZEXT", "INT_OR",
            "INT_LESSEQUAL", "INT_LESS", "BOOL_AND", "INT_ZEXT",
            "INT_RIGHT", "INT_MULT", "INT_ADD",
        ]
        for direction, buf, expected_name in (
            ("load", observed_load, "LOAD"),
            ("store", observed_store, "STORE"),
        ):
            with self.subTest(direction=direction):
                names, ops = self.lift(buf)
                self.assertEqual(names, expected_ops + [expected_name])
                access = ops[-1]
                self.assertEqual(access.inputs[0].getSpaceFromConst().name, "ram")
                self.assertEqual(ops[0].inputs[0].offset, 0x25)
                self.assertEqual(ops[2].inputs[0].offset, 0x4D98)
                _op, unit = eval_pcode(ops)
                self.assertEqual(unit, 0x254D98 >> 1)
                self.assertEqual(access.inputs[1].size, 4)
                if direction == "load":
                    self.assertEqual(access.output.getRegisterName(), "R4")
                    self.assertEqual(access.output.size, 4)
                else:
                    self.assertEqual(access.inputs[2].getRegisterName(), "R4")
                    self.assertEqual(access.inputs[2].size, 4)

    def test_type14a_scalar_dm_byte_address_translates_to_ram_unit(self):
        """The DM byte-address -> ram-unit translation for an on-chip literal,
        the L2 byte-window alias, and an external (non-aliased) literal."""
        cases = (
            ("on-chip", 0x254D98, 0x254D98 >> 1),
            ("L2 alias", 0x20000010, 0xB80008),
            ("external", 0x82A00008, 0x82A00008 >> 1),
        )
        for label, addr, expected_unit in cases:
            with self.subTest(label=label):
                buf = type14a(
                    d=0,
                    **{"addr[31:16]": (addr >> 16) & 0xFFFF, "addr[15:0]": addr & 0xFFFF},
                )
                _names, ops = self.lift(buf)
                _op, unit = eval_pcode(ops)
                self.assertEqual(unit, expected_unit)

    def test_type14a_pm_or_lw_selector_does_not_inherit_scalar_dm_pcode(self):
        for selector, buf in (("PM", type14a(g=1)), ("LW", type14a(l=1))):
            with self.subTest(selector=selector):
                names, _ops = self.lift(buf)
                self.assertNotIn("LOAD", names)
                self.assertNotIn("STORE", names)

    def test_type3b_exact_reader_loads_i12_from_i4_plus_m4(self):
        # Capture words are big-endian; pypcode receives little-endian words.
        raw = bytes.fromhex("493e0e3f")
        observed = b"".join(raw[i : i + 2][::-1] for i in range(0, len(raw), 2))
        self.assertEqual(observed, type3b())
        names, ops = self.lift(observed, sw=0x1C6C19)
        loads = [op for op in ops if op.opcode.name == "LOAD"]
        self.assertEqual(names.count("LOAD"), 1)
        self.assertEqual(len(loads), 1)
        load = loads[0]
        self.assertEqual(load.inputs[0].getSpaceFromConst().name, "ram")
        self.assertEqual(load.inputs[1].size, 4)
        self.assertEqual(load.output.getRegisterName(), "I12")
        self.assertEqual(load.output.size, 4)
        # I4 + M4 is the raw DM BYTE address; the isl2/unit chain then
        # translates it into a ram-space unit, same as Type14a's literal.
        self.assertEqual(
            names,
            [
                "INT_ADD",
                "INT_LESSEQUAL", "INT_LESS", "BOOL_AND", "INT_ZEXT",
                "INT_RIGHT", "INT_MULT", "INT_ADD",
                "LOAD",
            ],
        )
        self.assertEqual(ops[0].inputs[0].getRegisterName(), "I4")
        self.assertEqual(ops[0].inputs[1].getRegisterName(), "M4")
        self.assertFalse(
            any(op.output and op.output.getRegisterName() == "I4" for op in ops)
        )
        for i4, m4, expected_unit in (
            (0x254D98, 0, 0x254D98 >> 1),
            (0x20000010, 0, 0xB80008),
        ):
            with self.subTest(i4=hex(i4), m4=hex(m4)):
                _op, unit = eval_pcode(ops, registers={"I4": i4, "M4": m4})
                self.assertEqual(unit, expected_unit)

    def test_type3b_nearby_selector_does_not_inherit_exact_reader_pcode(self):
        names, _ops = self.lift(type3b(g=1))
        self.assertNotIn("LOAD", names)

    # --- Type15b / Type4a / Type3a indexed DM forms -----------------------

    def test_type15b_load_and_store_address_and_index_unchanged(self):
        # data[6:0]=0x7f: sign bit set, magnitude 0x3f -> two's-complement
        # -1, so off = -1*4 = -4 (a negative offset).
        i3 = 0x00254D9C
        for direction, expected_role in ((0, "load"), (1, "store")):
            with self.subTest(direction=expected_role):
                buf = type15b(d=direction, **{"i[2:0]": 3, "data[6:0]": 0x7F})
                names, ops = self.lift(buf)
                self.assertEqual(names.count("LOAD" if direction == 0 else "STORE"), 1)
                r = run_pcode(ops, registers={"I3": i3, "R5": 0xAABBCCDD})
                self.assertEqual(r["registers"]["I3"], i3, "I must not be updated")
                if direction == 0:
                    self.assertEqual(len(r["loads"]), 1)
                    self.assertEqual(r["loads"][0]["address"], (i3 - 4) >> 1)
                else:
                    self.assertEqual(len(r["stores"]), 1)
                    self.assertEqual(r["stores"][0]["address"], (i3 - 4) >> 1)
                    self.assertEqual(r["stores"][0]["value"], 0xAABBCCDD)

    def test_type15b_pm_or_lw_selector_produces_no_pcode(self):
        for selector, buf in (("PM", type15b(g=1)), ("LW", type15b(l=1))):
            with self.subTest(selector=selector):
                names, _ops = self.lift(buf)
                self.assertEqual(names, [])

    def test_type4a_pm_selector_produces_no_pcode(self):
        names, _ops = self.lift(type4a(g=1))
        self.assertEqual(names, [])

    def test_type4a_premodify_negative_offset_no_update_no_compute(self):
        # data[5:5]=1, data[4:0]=0x1f -> magnitude 31, sign 1 -> -1*4 = -4.
        buf = type4a(u=0, **{"data[5:5]": 1, "data[4:0]": 0x1F})
        names, ops = self.lift(buf)
        self.assertEqual(names.count("LOAD"), 1)
        i2 = 0x00300000
        r = run_pcode(ops, registers={"I2": i2, "L2": 0})
        self.assertEqual(r["loads"][0]["address"], (i2 - 4) >> 1)
        self.assertEqual(r["registers"]["I2"], i2, "u=0 must never update I")
        self.assertEqual(r["callothers"], [], "compute=0 and u=0: no markers at all")

    def test_type4a_postmodify_plain_update_when_l_zero(self):
        buf = type4a(u=1, **{"data[5:5]": 1, "data[4:0]": 0x1F})  # off = -4
        _names, ops = self.lift(buf)
        i2 = 0x00300000
        r = run_pcode(ops, registers={"I2": i2, "L2": 0})
        self.assertEqual(r["loads"][0]["address"], i2 >> 1, "address uses the OLD I")
        self.assertEqual(r["registers"]["I2"], i2 - 4, "post-modify: I += off")
        self.assertEqual([c for c in r["callothers"] if c["name"] == "circular"], [])

    def test_type4a_postmodify_circular_guard_skips_plain_update(self):
        buf = type4a(u=1, **{"i[2:0]": 2, "data[5:5]": 1, "data[4:0]": 0x1F})
        _names, ops = self.lift(buf)
        i2 = 0x00300000
        r = run_pcode(ops, registers={"I2": i2, "L2": 0x40})
        self.assertEqual(r["loads"][0]["address"], i2 >> 1)
        self.assertEqual(r["registers"]["I2"], i2, "circular buffering: no plain update")
        circular = [c for c in r["callothers"] if c["name"] == "circular"]
        self.assertEqual(len(circular), 1)
        self.assertEqual(circular[0]["args"], [2], "argument is the raw index register")

    def test_type4a_compute_marker_only_when_nonzero(self):
        zero = type4a(**{"compute[22:16]": 0, "compute[15:0]": 0})
        _names, ops = self.lift(zero)
        r = run_pcode(ops, registers={"I2": 0x1000, "L2": 0})
        self.assertEqual([c for c in r["callothers"] if c["name"] == "compute"], [])

        nonzero = type4a(**{"compute[22:16]": 0x05, "compute[15:0]": 0x1234})
        _names, ops = self.lift(nonzero)
        r = run_pcode(ops, registers={"I2": 0x1000, "L2": 0})
        compute = [c for c in r["callothers"] if c["name"] == "compute"]
        self.assertEqual(len(compute), 1)
        self.assertEqual(compute[0]["args"], [(0x05 << 16) | 0x1234])

    def test_type4a_cond_true_has_no_condition_callother(self):
        buf = type4a(**{"cond[4:0]": sharcpcode.COND_TRUE})
        _names, ops = self.lift(buf)
        self.assertFalse(
            any(
                op.opcode.name == "CALLOTHER"
                and op.inputs[0].getUserDefinedOpName() == "condition"
                for op in ops
            )
        )

    def test_type4a_non_true_cond_calls_condition_and_falls_through(self):
        cond_code = 0x00
        buf = type4a(**{"cond[4:0]": cond_code})
        names, ops = self.lift(buf)
        self.assertIn("CALLOTHER", names)
        self.assertIn("CBRANCH", names)
        condition_calls = [
            op
            for op in ops
            if op.opcode.name == "CALLOTHER"
            and op.inputs[0].getUserDefinedOpName() == "condition"
        ]
        self.assertEqual(len(condition_calls), 1)
        # The cond field is a compile-time constant of THIS instruction, so
        # sleigh folds "local code:4 = cond;" into the literal argument.
        self.assertEqual(condition_calls[0].inputs[1].offset, cond_code)

    def test_type3a_premodify_scaled_by_m_no_update(self):
        buf = type3a(u=0, i=2, m=3)
        names, ops = self.lift(buf)
        self.assertEqual(names.count("LOAD"), 1)
        # M3 = -1 (two's complement) -> sm = M[m]*4 = -4 (a negative offset).
        i2, m3 = 0x00300000, 0xFFFFFFFF
        r = run_pcode(ops, registers={"I2": i2, "M3": m3, "L2": 0})
        self.assertEqual(r["loads"][0]["address"], (i2 - 4) >> 1)
        self.assertEqual(r["registers"]["I2"], i2)

    def test_type3a_postmodify_plain_update_when_l_zero(self):
        buf = type3a(u=1, i=2, m=3)
        _names, ops = self.lift(buf)
        i2, m3 = 0x00300000, 1  # sm = M[m]*4 = 4
        r = run_pcode(ops, registers={"I2": i2, "M3": m3, "L2": 0})
        self.assertEqual(r["loads"][0]["address"], i2 >> 1)
        self.assertEqual(r["registers"]["I2"], i2 + m3 * 4)
        self.assertEqual([c for c in r["callothers"] if c["name"] == "circular"], [])

    def test_type3a_postmodify_circular_guard_skips_plain_update(self):
        buf = type3a(u=1, i=2, m=3)
        _names, ops = self.lift(buf)
        i2, m3 = 0x00300000, 1
        r = run_pcode(ops, registers={"I2": i2, "M3": m3, "L2": 0x40})
        self.assertEqual(r["registers"]["I2"], i2)
        circular = [c for c in r["callothers"] if c["name"] == "circular"]
        self.assertEqual(len(circular), 1)
        self.assertEqual(circular[0]["args"], [2])

    def test_type3a_compute_marker_only_when_nonzero(self):
        # Type3a's `compute` is one 23-bit field, split across two words by
        # the generator; field()/encode() take it as a single value.
        zero = type3a(compute=0)
        _names, ops = self.lift(zero)
        r = run_pcode(ops, registers={"I2": 0x1000, "M3": 0, "L2": 0})
        self.assertEqual([c for c in r["callothers"] if c["name"] == "compute"], [])

        nonzero = type3a(compute=0x51234)
        _names, ops = self.lift(nonzero)
        r = run_pcode(ops, registers={"I2": 0x1000, "M3": 0, "L2": 0})
        compute = [c for c in r["callothers"] if c["name"] == "compute"]
        self.assertEqual(len(compute), 1)
        self.assertEqual(compute[0]["args"], [0x51234])

    def test_type3a_cond_true_vs_gated(self):
        true_buf = type3a(cond=sharcpcode.COND_TRUE)
        _names, ops = self.lift(true_buf)
        self.assertFalse(
            any(
                op.opcode.name == "CALLOTHER"
                and op.inputs[0].getUserDefinedOpName() == "condition"
                for op in ops
            )
        )

        cond_code = 0x08
        gated_buf = type3a(cond=cond_code)
        names, ops = self.lift(gated_buf)
        self.assertIn("CALLOTHER", names)
        condition_calls = [
            op
            for op in ops
            if op.opcode.name == "CALLOTHER"
            and op.inputs[0].getUserDefinedOpName() == "condition"
        ]
        self.assertEqual(len(condition_calls), 1)
        self.assertEqual(condition_calls[0].inputs[1].offset, cond_code)

    def test_type3a_pm_or_lw_selector_produces_no_pcode(self):
        for selector, buf in (("PM", type3a(g=1)), ("LW", type3a(l=1))):
            with self.subTest(selector=selector):
                names, _ops = self.lift(buf)
                self.assertEqual(names, [])

    def test_ureg_attachment_uses_complete_manual_code_table(self):
        """Unsplit 7-bit UREG fields attach to all PRM UREG/SYSREG entries."""
        self.assertEqual(self.lint["returncode"], 0, self.lint)
        slaspec = os.path.join(
            self.tmp,
            "sharcspec",
            "ghidra",
            "SHARC_VISA",
            "data",
            "languages",
            "sharc_visa.slaspec",
        )
        with open(slaspec) as f:
            attachments = [
                line.removeprefix("attach variables [ ").removesuffix(" ];\n")
                for line in f
                if line.startswith("attach variables")
            ]
        fields, registers = next(
            attachment.split(" ] [ ")
            for attachment in attachments
            if any("ureg_" in field for field in attachment.split(" ] [ ")[0].split())
        )
        self.assertEqual(set(fields.split()), {"ureg_w0_6_0", "ureg_w1_13_7"})
        expected = {
            0x00: "R0",
            0x0F: "R15",
            0x10: "I0",
            0x2F: "M15",
            0x30: "L0",
            0x4F: "B15",
            0x50: "S0",
            0x5F: "S15",
            0x60: "FADDR",
            0x62: "UREG_RESERVED_62",
            0x63: "PC",
            0x6B: "PX",
            0x6F: "TCOUNT",
            0x70: "USTAT1",
            0x71: "USTAT2",
            0x72: "MODE1",
            0x7D: "MODE1STK",
            0x7F: "USTAT4",
        }
        for code, register in expected.items():
            with self.subTest(code=f"0x{code:02x}"):
                self.assertEqual(registers.split()[code], register)

    def test_every_form_has_our_decoders_length(self):
        """Each form, encoded with only its fixed bits, has the length tools/sharc_disasm.py gives it.
        Branch forms change here on purpose once delay slots join the branch (handover stage 3)."""
        missing, wrong = [], []
        for name in sorted(t["name"] for t in T.TYPES):
            buf = encode(name)
            ours = next(iter(sharc_disasm.disassemble(buf, count=1)), None)
            if ours is None or ours.type_name != name:
                continue
            length, _ = sharcpcode.lift_one(self.ctx, buf, 0x2000)
            if length is None:
                missing.append(name)
            elif length != ours.length_bytes:
                wrong.append((name, length, ours.length_bytes))
        self.assertEqual(wrong, [])
        self.assertEqual(missing, [])

    def test_unconditional_jump_and_call(self):
        for name, b, opname in (("8a_abs", 0, "BRANCH"), ("8a_abs", 1, "CALL")):
            with self.subTest(name=name, b=b):
                names, ops = self.lift(
                    branch(name, cond=sharcpcode.COND_TRUE, b=b, target=TARGET_SW)
                )
                self.assertIn(opname, names)
                self.assertEqual(
                    ops[names.index(opname)].inputs[0].offset, 2 * TARGET_SW
                )
        names, ops = self.lift(branch("25a_direct", target=TARGET_SW))
        self.assertEqual(names, ["CALL"])
        self.assertEqual(ops[0].inputs[0].offset, 2 * TARGET_SW)

    def test_conditional_flow_falls_through(self):
        """A jump, call or return whose condition is not TRUE keeps a fall-through."""
        cases = {
            "8a_abs jump": branch("8a_abs", cond=COND_EQ, b=0, target=TARGET_SW),
            "8a_abs call": branch("8a_abs", cond=COND_EQ, b=1, target=TARGET_SW),
            "8a_rel jump": branch("8a_rel", cond=COND_EQ, b=0),
            "9b_abs jump": branch("9b_abs", cond=COND_EQ, b=0),
            "11a return": branch("11a", cond=COND_EQ),
            "11c return": branch("11c", cond=COND_EQ),
        }
        for label, buf in cases.items():
            with self.subTest(label):
                names, _ = self.lift(buf)
                self.assertIn("CALLOTHER", names)
                self.assertIn("CBRANCH", names)
                self.assertFalse(sharcpcode.lifts_without_fallthrough(names))

    def test_conditional_jump_branches_to_its_target(self):
        names, ops = self.lift(branch("8a_abs", cond=COND_EQ, b=0, target=TARGET_SW))
        self.assertNotIn("BRANCH", names)
        self.assertEqual(ops[names.index("CBRANCH")].inputs[0].offset, 2 * TARGET_SW)

    def test_true_condition_stays_unconditional(self):
        """With cond TRUE there is no condition and no fall-through."""
        cases = {
            "8a_abs jump": branch(
                "8a_abs", cond=sharcpcode.COND_TRUE, b=0, target=TARGET_SW
            ),
            "8a_rel jump": branch("8a_rel", cond=sharcpcode.COND_TRUE, b=0),
            "9b_abs jump": branch("9b_abs", cond=sharcpcode.COND_TRUE, b=0),
            "11a return": branch("11a", cond=sharcpcode.COND_TRUE),
            "11c return": branch("11c", cond=sharcpcode.COND_TRUE),
        }
        for label, buf in cases.items():
            with self.subTest(label):
                names, _ = self.lift(buf)
                self.assertNotIn("CALLOTHER", names)
                self.assertTrue(sharcpcode.lifts_without_fallthrough(names))

    def test_lift_region_counts(self):
        jump_eq = branch("8a_abs", cond=COND_EQ, b=0, target=TARGET_SW)
        call = branch("8a_abs", cond=sharcpcode.COND_TRUE, b=1, target=TARGET_SW)
        data = encode("17b") + jump_eq + call + encode("17b")
        r = sharcpcode.lift_region(self.ctx, data, 0x1000, min_depth=1)
        self.assertEqual((r["aligned"], r["decoded"]), (4, 4))
        self.assertEqual(r["length_mismatch"], {})
        self.assertEqual(r["forms"]["8a_abs"]["n"], 2)
        self.assertEqual(r["forms"]["17b"]["n"], 2)
        expected = int(sharcpcode.lifts_without_fallthrough(self.lift(jump_eq)[0]))
        self.assertEqual(
            sharcpcode._total(r["conditional_without_fallthrough"]), expected
        )


def image_record(**lift):
    rec = {
        "region_sha256": "a",
        "lift": {
            "aligned": 10,
            "decoded": 10,
            "undecoded": {},
            "length_mismatch": {},
            "conditional_without_fallthrough": {},
            "forms": {},
            "ops_total": 0,
            "instructions_per_second": 1000,
        },
    }
    rec["lift"].update(lift)
    return rec


class Compare(unittest.TestCase):
    def test_same_run_has_no_regression(self):
        self.assertEqual(
            sharcpcode.compare_image(image_record(), image_record(), 0.25)[0], []
        )

    def test_fewer_decoded_instructions(self):
        reg, _ = sharcpcode.compare_image(image_record(), image_record(decoded=9), 0.25)
        self.assertEqual(len(reg), 1)

    def test_new_conditional_flow_without_fallthrough(self):
        new = image_record(
            conditional_without_fallthrough={"11c": {"count": 2, "examples": []}}
        )
        reg, _ = sharcpcode.compare_image(image_record(), new, 0.25)
        self.assertEqual(len(reg), 1)

    def test_slower_lifting_beyond_tolerance(self):
        reg, _ = sharcpcode.compare_image(
            image_record(), image_record(instructions_per_second=700), 0.25
        )
        self.assertEqual(len(reg), 1)
        reg, notes = sharcpcode.compare_image(
            image_record(), image_record(instructions_per_second=900), 0.25
        )
        self.assertEqual(reg, [])

    def test_different_image_counts_are_notes(self):
        new = image_record(decoded=5)
        new["region_sha256"] = "b"
        self.assertEqual(sharcpcode.compare_image(image_record(), new, 0.25)[0], [])

    def test_probe_that_stops_passing(self):
        g = {
            "probes": [{"name": "p", "ok": True, "instructions": 3}],
            "decompile_failed": {},
        }
        old, new = image_record(), image_record()
        old["ghidra"] = g
        new["ghidra"] = dict(g, probes=[{"name": "p", "ok": False, "instructions": 3}])
        reg, _ = sharcpcode.compare_image(old, new, 0.25)
        self.assertEqual(reg, ['probe "p" stopped passing'])

    def test_lint_regressions(self):
        old = {
            "compiler": "pypcode",
            "returncode": 0,
            "compile": {"returncode": 0},
            "counts": {"nop": 47},
            "slaspec_sha256": "x",
        }
        new = dict(old, counts={"nop": 40, "dead_temp": 1}, slaspec_sha256="y")
        reg, notes = sharcpcode.compare_lint(old, new, 0.25)
        self.assertEqual(reg, ["dead_temp 0 -> 1"])
        self.assertIn("nop 47 -> 40", notes)


class Dump(unittest.TestCase):
    def test_field_value_assembles_and_sign_extends(self):
        self.assertEqual(
            sharcpcode.field_value({"addr[23:16]": 0x1C, "addr[15:0]": 0x1400}, "addr"),
            0x1C1400,
        )
        self.assertEqual(
            sharcpcode.field_value(
                {"reladdr[5:5]": 1, "reladdr[4:0]": 0x1F}, "reladdr", signed=True
            ),
            -1,
        )
        self.assertEqual(sharcpcode.field_value({"cond[4:0]": 8}, "cond"), 8)
        self.assertIsNone(sharcpcode.field_value({"b": 1}, "addr"))

    def test_decoder_rows(self):
        data = (
            encode("17b")
            + branch("8a_abs", cond=sharcpcode.COND_TRUE, b=0, target=0x1002)
            + encode("17b")
        )
        db = sqlite3.connect(":memory:")
        db.executescript(sharcpcode.SCHEMA)
        sharcpcode.write_decoder(db, None, data, 0x1000, min_depth=1)
        rows = dict(db.execute("SELECT sw, form FROM decoder WHERE aligned = 1"))
        self.assertEqual(rows, {0x1000: "17b", 0x1002: "8a_abs", 0x1005: "17b"})
        target, target_aligned, cond = db.execute(
            "SELECT target_sw, target_aligned, cond FROM decoder WHERE sw = 0x1002"
        ).fetchone()
        self.assertEqual(
            (target, target_aligned, cond), (0x1002, 1, sharcpcode.COND_TRUE)
        )


if __name__ == "__main__":
    unittest.main()
