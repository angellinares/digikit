# R12-0 hook timing probe

Stock DT2 1.17 and DN2 1.12 unless noted. Tool: the ColdFire PC probe (`coupled-run --pc-probe`, documented in `docs/INSTRUMENTATION.md` "PC probe (ColdFire)"; inputs `tools/emulator/pc_probe_r12*.json`, `tests/scenarios/r12-probe.json`). Raw reports are in `out/runs/r12-probe/` of lane sdk-gen and are not committed. All results are **[D]** unless marked.

## Check 1: task switches through `vector_32_handler` 0x40000410

| | DT2 | DN2 |
| --- | --- | --- |
| entries | 20760 | 17778 |
| vector 205 (interrupt-entered switches) | 12117 | 10578 |
| vector 32 (`trap #0`, a task yields) | 8642 | 7199 |

- Vector 205 entries all come from stacked IPL 0: a task was running.
- Vector 32 entries all have stacked SR 0x2700: the task had interrupts masked.
- Vector 191, named in the R12 design, does not occur. The interrupt-driven switch is vector 205.
- No task switch is entered from an interrupt handler running at IPL >= 2.

Decision:

- Interrupt-only service code uses load-test-jump stubs. It can be unloaded after masking interrupts and clearing the slot.
- Task-reachable service code needs the entry/exit counter for hot reload, because tasks yield while service code is on their stack.

## Check 2: first fire of the hook sites

Main-OS entry: DT2 icount 299258200, DN2 icount 298782809.

After main-OS entry, in one task (DT2 0x40966ee8, DN2 0x4059e1c0), at IPL 0. These hooks install directly:

- DT2: `machine.dispatch` (first hit 827461270), `page_desc`, `param.value_class`.
- DN2: `page_desc`, `param.value_class`, `page.source_key`, `source_code`, `source_draw`, `source_turn` (needs a knob turn on the SRC page), `tab_label_test` (needs FUNC+FX).

Not reached after three scenario attempts; these stay patched in the image:

- DT2 `machine.clone.4`: called only from the KeyboardView PAGE handler in trig mode 3 or 4.
- DN2 `machine.dispatch`: its only caller is FUN_4003e69e, from parameter page views.

## `param_code` before main-OS entry

`param_code` (`param_code.args` on DT2, `param_code.map` on DN2) runs before main-OS entry, from the boot project decode loop, in boot tasks:

- DT2 caller FUN_400e64dc, return 0x400e6546.
- DN2 caller FUN_400e7eb0, return 0x400e7f1a.
- 8 iterations per call group.

Run on a patched image booted from a card whose saved project uses digikit machines (types 16 and 17):

| | hits | type 0 | type -1 |
| --- | --- | --- | --- |
| DT2 | 1207 | 1136 | 71 |
| DN2 | 1717 | 1616 | 101 |

- No digikit type and nothing outside the stock range was seen.
- The saved project is not loaded before main-OS entry.
- Direct install at main-OS entry is safe; the claim of finding 131 holds.

## Open

- **[O]** Project loads after main-OS entry were not in the 10 s window. They run with the hook installed.
