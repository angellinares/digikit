import importlib.util
import sys
import types
from pathlib import Path
from typing import cast

spec = importlib.util.spec_from_file_location(
    "ghidraq", Path(__file__).parents[1] / "tools/ghidraq.py"
)
assert spec is not None and spec.loader is not None
ghidraq = importlib.util.module_from_spec(spec)
spec.loader.exec_module(ghidraq)


class Language:
    def __init__(self, language_id):
        self.language_id = language_id

    def getLanguageID(self):
        return self.language_id


class Factory:
    def __init__(self, space, spaces=None):
        self.space = space
        self.spaces = {s.getSpaceID(): s for s in (spaces or [space])}

    def getDefaultAddressSpace(self):
        return self.space

    def getAddressSpace(self, space_id):
        return self.spaces.get(space_id)


class FunctionManager:
    def getFunctionContaining(self, _address):
        return None

    def getFunctions(self, _forward):
        return []


class Program:
    def __init__(
        self, language_id="SHARC_VISA:LE:32:default", space=None, listing=None
    ):
        self.language = Language(language_id)
        self.factory = Factory(space or Space())
        self.functions = FunctionManager()
        self.listing = listing

    def getName(self):
        return "fake-program"

    def getLanguage(self):
        return self.language

    def getAddressFactory(self):
        return self.factory

    def getFunctionManager(self):
        return self.functions

    def getListing(self):
        return self.listing


class Space:
    def __init__(self, name="ram", wordsize=1, memory=True, space_id=1):
        self.name, self.wordsize, self.memory, self.space_id = (
            name,
            wordsize,
            memory,
            space_id,
        )

    def getSpaceID(self):
        return self.space_id

    def getName(self):
        return self.name

    def getAddressableUnitSize(self):
        return self.wordsize

    def isMemorySpace(self):
        return self.memory

    def getAddress(self, offset):
        return Addr(offset, self)


class Addr:
    def __init__(self, off, space=None):
        self.off, self.space = off, space or Space()

    def getOffset(self):
        return self.off

    def getAddressSpace(self):
        return self.space


class Seq:
    def __init__(self, pc, time=0):
        self.pc, self.time = pc, time

    def getTarget(self):
        return Addr(self.pc)

    def getTime(self):
        return self.time


class Varnode:
    def __init__(self, off, kind="unique", definition=None, size=4, space=None):
        self.off, self.kind, self.definition, self.size, self.space = (
            off,
            kind,
            definition,
            size,
            space,
        )

    def isConstant(self):
        return self.kind == "constant"

    def isRegister(self):
        return self.kind == "register"

    def isUnique(self):
        return self.kind == "unique"

    def getOffset(self):
        return self.off

    def getSize(self):
        return self.size

    def getAddress(self):
        space = self.space or {
            "register": Space("register", memory=False, space_id=2),
            "unique": Space("unique", memory=False, space_id=3),
            "constant": Space("const", memory=False, space_id=4),
            "memory": Space("ram", memory=True),
        }.get(self.kind, Space("unique", memory=False, space_id=3))
        return Addr(self.off, space)

    def getDef(self):
        return self.definition


class Op:
    def __init__(self, pc, name, inputs=(), output=None, time=0):
        self.pc, self.name, self.inputs, self.output, self.time = (
            pc,
            name,
            list(inputs),
            output,
            time,
        )

    def getSeqnum(self):
        return Seq(self.pc, self.time)

    def getMnemonic(self):
        return self.name

    def getInputs(self):
        return self.inputs

    def getOutput(self):
        return self.output


def test_serializers_are_deterministic_and_do_not_use_repr():
    op = Op(0x20, "COPY", [Varnode(2, "constant")], Varnode(3))
    assert ghidraq.serialize_op(op)["seq"] == {"pc": "0x20", "time": 0}
    assert ghidraq.serialize_varnode(Varnode(2, "constant")) == {
        "kind": "constant",
        "value": "0x2",
        "size": 4,
    }
    assert ghidraq.op_sort_key(op) == (0x20, 0, "COPY", 3)


def test_fallback_slice_handles_cycles_roots_and_cap():
    root = Varnode(1, "register")
    a = Varnode(2)
    b = Varnode(3)
    oa = Op(0x10, "COPY", [b], a)
    ob = Op(0x11, "COPY", [a, root], b)
    a.definition, b.definition = oa, ob
    result = ghidraq.backward_slice_fallback([a], node_cap=1)
    assert result["partial"] is True
    assert result["ops"] == [oa]
    result = ghidraq.backward_slice_fallback([a], node_cap=10)
    assert result["partial"] is False
    assert result["ops"] == [oa, ob]
    assert result["roots"] == [root]
    assert ghidraq.backward_slice_fallback([Varnode(4, "constant")]) == {
        "ops": [],
        "roots": [],
        "partial": False,
    }


def test_selector_parse_and_selection_and_store_validation():
    out = Varnode(9)
    op = Op(0x10, "STORE", [Varnode(1), Varnode(2), Varnode(3)], out)
    assert ghidraq.parse_slice_selector("input:2") == ("input", 2)
    assert ghidraq.select_slice_seeds([op], "store:value") == [op.inputs[2]]
    assert ghidraq.select_slice_seeds([op], "output") == [out]
    assert ghidraq.select_slice_seeds([op], "reg:r9") == []
    assert ghidraq.store_operands(op) == (op.inputs[0], op.inputs[1], op.inputs[2])
    assert ghidraq.store_operands(Op(1, "STORE", [Varnode(1)])) is None


def test_sorting_and_address_metadata():
    ops = [Op(0x20, "B"), Op(0x10, "A", time=2), Op(0x10, "A", time=1)]
    assert [x.time for x in ghidraq.sorted_ops(ops)] == [1, 2, 0]
    # Live SHARC uses a default space named ram; language + unit size decide.
    metadata = ghidraq.program_address_metadata(Program(space=Space("ram", 2)))
    assert metadata == {
        "name": "ram",
        "word_size": 2,
        "language": "SHARC_VISA:LE:32:default",
        "short_word_addresses": True,
        "conversion_rule": "displayed_offset = logical_short_word * 2",
        "delay_slot_caveat": "HighFunction p-code does not model SHARC_VISA delay-slot execution.",
    }
    assert (
        ghidraq.address_space_metadata(Space("SHARC_VISA", 2))["short_word_addresses"]
        is False
    )


def test_short_word_coordinates_are_explicit_and_schema_is_stable():
    sharc = Program(space=Space("ram", 2))
    displayed, sw = ghidraq.resolve_coordinate(sharc, "sw:0x1c1928")
    assert displayed == 0x383250
    assert sw == {
        "requested": "sw:0x1c1928",
        "logical": "0x1c1928",
        "displayed": "0x383250",
        "conversion_rule": "displayed_offset = logical_short_word * 2",
    }
    displayed, raw = ghidraq.resolve_coordinate(sharc, "0x383250")
    assert displayed == 0x383250
    assert raw == {**sw, "requested": "0x383250"}
    displayed, odd = ghidraq.resolve_coordinate(sharc, "0x3")
    assert displayed == 3
    assert odd == {
        "requested": "0x3",
        "logical": None,
        "displayed": "0x3",
        "conversion_rule": "displayed_offset = logical_short_word * 2",
    }


def test_short_word_coordinates_reject_non_sharc_programs_and_cover_store_positions():
    non_sharc = Program("68000:BE:32:ColdfireEMAC", Space("ram", 2))
    for token in ("sw:0x1", "sw:0x2", "sw:0x3"):
        try:
            ghidraq.resolve_coordinate(non_sharc, token)
        except ValueError as exc:
            assert (
                str(exc)
                == "sw: coordinate %r requires SHARC_VISA language with addressable unit size 2"
                % token
            )
        else:
            raise AssertionError("sw: must be rejected outside SHARC")
    sharc = Program(space=Space("ram", 2))
    captured = []
    ghidraq.q_slice(
        sharc,
        ["sw:0x10", "output"],
        lambda kind, payload: captured.append((kind, payload)),
    )
    assert captured == [
        (
            "slice",
            {
                "schema_version": 1,
                "requested": {"pc": "sw:0x10", "selector": "output"},
                "status": "error",
                "partial": False,
                "errors": ["no containing function"],
                "matching_ops": [],
                "selected_seeds": [],
                "slice_ops": [],
                "roots": [],
                "program": {
                    "name": "fake-program",
                    "language": "SHARC_VISA:LE:32:default",
                },
                "address_space": ghidraq.program_address_metadata(sharc),
                "pc": {
                    "requested": "sw:0x10",
                    "logical": "0x10",
                    "displayed": "0x20",
                    "conversion_rule": "displayed_offset = logical_short_word * 2",
                },
            },
        )
    ]

    class EmptyDecompiler:
        def dispose(self):
            pass

    old_decompiler = ghidraq._decompiler
    ghidraq._decompiler = lambda _program: EmptyDecompiler()  # type: ignore[attr-defined]
    try:
        captured = []
        ghidraq.q_stores(
            sharc,
            ["sw:0x10", "sw:0x20", "sw:0x30"],
            lambda kind, payload: captured.append((kind, payload)),
        )
    finally:
        ghidraq._decompiler = old_decompiler  # type: ignore[attr-defined]
    result = captured[0][1]
    assert (
        captured[0][0],
        result["target"],
        result["target_coordinate"]["displayed"],
        result["bounds"]["lo"]["displayed"],
        result["bounds"]["hi"]["displayed"],
        result["functions"],
    ) == ("stores", "sw:0x10", "0x20", "0x40", "0x60", [])


def test_raw_instruction_pcode_is_deterministic_and_maps_sw_coordinates():
    class Instruction:
        def getMnemonicString(self):
            return "Type14a"

        def getLength(self):
            return 6

        def getPcode(self):
            return [
                Op(0x20, "COPY", [Varnode(2, "constant")], Varnode(3)),
                Op(
                    0x20,
                    "STORE",
                    [Varnode(1, "constant"), Varnode(4), Varnode(5)],
                    time=1,
                ),
            ]

    class Listing:
        def __init__(self, instruction):
            self.instruction, self.requested = instruction, None

        def getInstructionAt(self, address):
            self.requested = address.getOffset()
            return self.instruction

    listing = Listing(Instruction())
    captured = []
    ghidraq.q_pcode(
        Program(space=Space("ram", 2), listing=listing),
        ["sw:0x10"],
        lambda kind, payload: captured.append((kind, payload)),
    )
    kind, payload = captured[0]
    assert (kind, listing.requested, payload["status"], payload["pc"]) == (
        "pcode",
        0x20,
        "ok",
        {
            "requested": "sw:0x10",
            "logical": "0x10",
            "displayed": "0x20",
            "conversion_rule": "displayed_offset = logical_short_word * 2",
        },
    )
    assert payload["instruction"] == {"mnemonic": "Type14a", "length": 6}
    assert payload["operations"] == [
        {
            "seq": {"pc": "0x20", "time": 0},
            "mnemonic": "COPY",
            "inputs": [{"kind": "constant", "value": "0x2", "size": 4}],
            "output": {"kind": "unique", "size": 4, "space": "unique", "offset": "0x3"},
            "order": 0,
        },
        {
            "seq": {"pc": "0x20", "time": 1},
            "mnemonic": "STORE",
            "inputs": [
                {"kind": "constant", "value": "0x1", "size": 4},
                {"kind": "unique", "size": 4, "space": "unique", "offset": "0x4"},
                {"kind": "unique", "size": 4, "space": "unique", "offset": "0x5"},
            ],
            "output": None,
            "order": 1,
        },
    ]
    assert payload["address_space"]["delay_slot_caveat"] == (
        "HighFunction p-code does not model SHARC_VISA delay-slot execution."
    )


def test_raw_instruction_pcode_reports_absent_empty_and_malformed():
    class Listing:
        def __init__(self, instruction):
            self.instruction = instruction

        def getInstructionAt(self, _address):
            return self.instruction

    class EmptyInstruction:
        def getMnemonicString(self):
            return "nop"

        def getLength(self):
            return 2

        def getPcode(self):
            return []

    class MalformedInstruction(EmptyInstruction):
        def getPcode(self):
            return [object()]

    for instruction, status in (
        (None, "no-instruction"),
        (EmptyInstruction(), "empty-pcode"),
        (MalformedInstruction(), "malformed-pcode"),
    ):
        captured = []
        ghidraq.q_pcode(
            Program(listing=Listing(instruction)),
            ["0x20"],
            lambda kind, payload: captured.append((kind, payload)),
        )
        assert captured[0][0] == "pcode"
        assert captured[0][1]["status"] == status
        if status == "malformed-pcode":
            assert captured[0][1]["errors"][0].startswith(
                "operation 0: AttributeError:"
            )


def test_memory_access_normalization_handles_raw_and_direct_copy_roles():
    address, value = Varnode(0x44, "constant"), Varnode(3)
    raw_store = Op(1, "STORE", [Varnode(0, "constant"), address, value])
    raw_load = Op(2, "LOAD", [Varnode(0, "constant"), address], Varnode(4))
    copy_write = Op(3, "COPY", [value], Varnode(0x4A9B30, "memory"))
    copy_read = Op(4, "COPY", [Varnode(0x254D98, "memory")], Varnode(5))
    assert ghidraq.memory_access_operands(raw_store, "store") == {
        "space": raw_store.inputs[0],
        "memory_space": None,
        "address": address,
        "value": value,
        "representation": "STORE",
        "direct_address": False,
    }
    assert ghidraq.memory_access_operands(raw_load, "load")["representation"] == "LOAD"
    # q_slice uses this same serializer and now receives program, so raw space
    # IDs have the same resolved identity as global loads/stores metadata.
    slice_program = Program(space=Space("ram", space_id=0))
    assert ghidraq.serialize_memory_op(raw_store, "store", slice_program)[
        "memory_space"
    ] == {"status": "resolved", "name": "ram"}
    assert ghidraq.slice_seed_specs(
        [raw_store], "store:value", program=slice_program
    ) == [(value, True)]
    assert (
        ghidraq.memory_access_operands(copy_write, "store")["representation"]
        == "COPY_DIRECT_WRITE"
    )
    assert (
        ghidraq.memory_access_operands(copy_read, "load")["representation"]
        == "COPY_DIRECT_READ"
    )
    assert ghidraq.store_operands(copy_write)[1:] == (copy_write.output, value)
    assert ghidraq.load_operands(copy_read)[1:] == (
        copy_read.inputs[0],
        copy_read.output,
    )


def test_memory_copy_rejects_nonmemory_and_malformed_forms():
    class ExplicitTestMemorySpace:
        _test_is_memory_space = True

    assert ghidraq._is_memory_space(ExplicitTestMemorySpace())
    value = Varnode(1)
    for copy in (
        Op(1, "COPY", [value], Varnode(2, "register")),
        Op(2, "COPY", [Varnode(2, "unique")], Varnode(3)),
        Op(3, "COPY", [value], Varnode(2, "constant")),
        Op(4, "COPY", [], Varnode(2, "memory")),
        Op(5, "COPY", [Varnode(2, "memory")], None),
    ):
        assert ghidraq.memory_access_operands(copy, "store") is None
        assert ghidraq.memory_access_operands(copy, "load") is None


def test_memory_to_memory_copy_is_both_a_store_and_a_load():
    # Ghidra folds "R2 = DM(0x252658); DM(0x254d9c) = R2" into one COPY with a
    # direct memory varnode on both ends. Classifying only one end silently hid
    # a byte-proven store from `stores`.
    source = Varnode(0x4A4CB0, "memory")
    destination = Varnode(0x4A9B38, "memory")
    copy = Op(0x20, "COPY", [source], destination)
    store = ghidraq.memory_access_operands(copy, "store")
    assert store["representation"] == "COPY_MEM_TO_MEM_WRITE"
    assert (store["address"], store["value"]) == (destination, source)
    assert store["direct_address"] is True
    load = ghidraq.memory_access_operands(copy, "load")
    assert load["representation"] == "COPY_MEM_TO_MEM_READ"
    assert (load["address"], load["value"]) == (source, destination)
    assert load["direct_address"] is True
    # The destination stays an unfollowed root; the value keeps its definition,
    # which is what walks back to whoever wrote the source location.
    assert ghidraq.slice_seed_specs([copy], "store:address") == [(destination, False)]
    assert ghidraq.slice_seed_specs([copy], "store:value") == [(source, True)]


def test_memory_copy_selectors_keep_static_address_as_a_root():
    stored = Varnode(7)
    direct_address = Varnode(0x4A9B30, "memory")
    copy_write = Op(0x10, "COPY", [stored], direct_address)
    direct_address.definition = copy_write
    specs = ghidraq.slice_seed_specs([copy_write], "store:address")
    assert specs == [(direct_address, False)]
    result = ghidraq._slice([], [direct_address])
    assert result["ops"] == []
    assert result["roots"] == [direct_address]
    assert ghidraq.slice_seed_specs([copy_write], "store:value") == [(stored, True)]

    loaded = Varnode(0x254D98, "memory")
    result_value = Varnode(8)
    copy_read = Op(0x11, "COPY", [loaded], result_value)
    assert ghidraq.slice_seed_specs([copy_read], "load:address") == [(loaded, False)]
    assert ghidraq.slice_seed_specs([copy_read], "load:value") == [(result_value, True)]


def test_loads_and_stores_scan_direct_copy_targets(monkeypatch):
    class Iterator:
        def __init__(self, items):
            self.items = iter(items)

        def hasNext(self):
            try:
                self.next_item = next(self.items)
            except StopIteration:
                return False
            return True

        def next(self):
            return self.next_item

    class High:
        def __init__(self, ops):
            self.ops = ops

        def getPcodeOps(self):
            return Iterator(self.ops)

    class Result:
        def __init__(self, ops):
            self.ops = ops

        def decompileCompleted(self):
            return True

        def getHighFunction(self):
            return High(self.ops)

    class Function:
        def getName(self):
            return "dispatch_reader"

        def getEntryPoint(self):
            return Addr(0x100)

    class Decompiler:
        def __init__(self, ops):
            self.ops = ops

        def decompileFunction(self, *_args):
            return Result(self.ops)

        def dispose(self):
            pass

    class TaskMonitor:
        DUMMY = object()

    task_module = types.ModuleType("ghidra.util.task")
    task_module.TaskMonitor = TaskMonitor  # type: ignore[attr-defined]
    monkeypatch.setitem(sys.modules, "ghidra.util.task", task_module)
    function = Function()

    functions = type(
        "Functions",
        (),
        {
            "getFunctionContaining": lambda _self, _address: function,
            "getFunctions": lambda _self, _forward: [function],
        },
    )()
    program = Program()
    program.functions = cast(FunctionManager, functions)
    write = Op(
        0x20,
        "COPY",
        [Varnode(1)],
        Varnode(0x254D98, "memory", space=program.factory.space),
    )
    read = Op(
        0x10,
        "COPY",
        [Varnode(0x254D98, "memory", space=program.factory.space)],
        Varnode(2),
    )
    monkeypatch.setattr(
        ghidraq, "_decompiler", lambda _program: Decompiler([write, read])
    )

    captured = []
    ghidraq.q_stores(
        program, ["0x254d98"], lambda kind, payload: captured.append((kind, payload))
    )
    ghidraq.q_loads(
        program, ["0x254d98"], lambda kind, payload: captured.append((kind, payload))
    )
    write_match = captured[0][1]["functions"][0]["matches"][0]
    read_match = captured[1][1]["functions"][0]["matches"][0]
    assert (
        captured[0][0],
        write_match["representation"],
        write_match["classification"],
    ) == (
        "stores",
        "COPY_DIRECT_WRITE",
        "exact-constant-target",
    )
    assert (
        captured[1][0],
        read_match["representation"],
        read_match["classification"],
    ) == (
        "loads",
        "COPY_DIRECT_READ",
        "exact-constant-target",
    )
    assert write_match["address_slice"]["roots"][0]["offset"] == "0x254d98"
    assert read_match["address_slice"]["roots"][0]["offset"] == "0x254d98"


def test_memory_target_matching_keeps_address_space_identity(monkeypatch):
    class Iterator:
        def __init__(self, items):
            self.items = iter(items)

        def hasNext(self):
            try:
                self.item = next(self.items)
            except StopIteration:
                return False
            return True

        def next(self):
            return self.item

    class High:
        def getPcodeOps(self):
            return Iterator(ops)

    class Result:
        def decompileCompleted(self):
            return True

        def getHighFunction(self):
            return High()

    class Function:
        def getName(self):
            return "reader"

        def getEntryPoint(self):
            return Addr(0x100)

    class Decompiler:
        def decompileFunction(self, *_args):
            return Result()

        def dispose(self):
            pass

    class TaskMonitor:
        DUMMY = object()

    task_module = types.ModuleType("ghidra.util.task")
    task_module.TaskMonitor = TaskMonitor  # type: ignore[attr-defined]
    monkeypatch.setitem(sys.modules, "ghidra.util.task", task_module)
    ram, pm = Space("ram", space_id=1), Space("pm", space_id=7)
    program = Program(space=ram)
    program.factory = Factory(ram, [ram, pm])
    function = Function()
    program.functions = cast(
        FunctionManager,
        type(
            "Functions",
            (),
            {
                "getFunctionContaining": lambda _self, _address: function,
                "getFunctions": lambda _self, _forward: [function],
            },
        )(),
    )
    target = 0x1234
    ops = [
        Op(
            1, "LOAD", [Varnode(1, "constant"), Varnode(target, "constant")], Varnode(2)
        ),
        Op(
            2, "LOAD", [Varnode(7, "constant"), Varnode(target, "constant")], Varnode(3)
        ),
        Op(
            3,
            "LOAD",
            [Varnode(99, "constant"), Varnode(target, "constant")],
            Varnode(4),
        ),
        Op(4, "COPY", [Varnode(target, "memory", space=ram)], Varnode(5)),
        Op(5, "COPY", [Varnode(target, "memory", space=pm)], Varnode(6)),
    ]
    monkeypatch.setattr(ghidraq, "_decompiler", lambda _program: Decompiler())

    captured = []
    ghidraq.q_loads(
        program, [hex(target)], lambda kind, payload: captured.append(payload)
    )
    matches = captured[0]["functions"][0]["matches"]
    assert [
        (match["representation"], match["classification"]) for match in matches
    ] == [
        ("LOAD", "exact-constant-target"),
        ("LOAD", "computed-or-unresolved"),
        ("COPY_DIRECT_READ", "exact-constant-target"),
    ]
    assert matches[1]["memory_space"] == {"status": "unresolved", "space_id": "0x63"}
    assert matches[1]["partial"] is True
    assert captured[0]["partial"] is True


def test_query_registration_includes_existing_and_new_commands():
    assert ghidraq.SUBCOMMANDS == (
        "strings",
        "symbols",
        "xrefs",
        "func",
        "callers",
        "decompile",
        "read",
        "range",
        "pcode",
        "slice",
        "stores",
        "loads",
    )
    assert ghidraq.HANDLERS["pcode"] is ghidraq.q_pcode
    assert ghidraq.HANDLERS["slice"] is ghidraq.q_slice
    assert ghidraq.HANDLERS["stores"] is ghidraq.q_stores
    assert ghidraq.HANDLERS["loads"] is ghidraq.q_loads
    assert "pcode" in ghidraq.SUBCOMMANDS
