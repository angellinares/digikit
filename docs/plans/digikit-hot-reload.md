# digikit hot reload

Status: active plan. S0 starts after the R12 merge; S1 to S5 follow R12-8 (docs/plans/sdk-runtime-roadmap.md, item H). Firmware facts: [finding 135](../findings/135-hot-reload-firmware-facts.md).

## Goal

Replace or remove a service or module while the device runs, with no restart. It applies to the existing services (`lfo`, `fx`, `machines`, `samples`) and to future services and modules.

## Status today

- Nothing is freed at run time. Uninstall is a file delete plus a restart.
- Install records keep the stock bytes of every patched site, and the restore code is kept.
- The runtime loads modules and services at boot only (R12 as built: `fx.dks` and `machines.dks` loaded as services, raw ColdFire hooks installed with `bind_raw`, bind-made tails, DN2 frametype image stub with a slot, one image info table).

## Verdict per mechanism

| Mechanism | Hot reload as built | Needed |
| --- | --- | --- |
| ColdFire raw and handler hooks | Not possible. The site `jmp` goes into `.dks` memory; the tails are in the `.dks`; hooks call firmware that may yield, so a task can be paused with a return address in `.dks` code. | Gate with slot and entry/exit counter (decision 1). |
| Restore of stock bytes | Safe. A new stock-to-`jmp` write while running is unsafe. | Write the `jmp` only through a gate built at boot. |
| Interrupt slots (DN2 frametype) | Safe now: clear the slot under the interrupt mask, then free. No task switch starts from an interrupt at IPL >= 2 (finding 132). | None. |
| SHARC slots (R12-8) | Safe with rules: the stub reads the slot once per call; free after two confirmed blocks, not one; invalidate the DSP instruction cache before reuse **[O]**. | Rules in the R12-8 design. |
| `ServiceRow` Registry add and remove | Unsafe with concurrent readers: remove shifts pointers. | Publish by one pointer swap, plus a per-record counter around thunk calls. |
| Machine, param and page rows | Missing: in-use scan, selection-view rebuild, MISSING placeholder. Today an empty row plays as type 0. | S4. |
| Module DSP code | Quiesce exists. | Add the two-block wait. |
| Arena | Free exists. A swap needs memory twice. | Fit check with old and new counted (decision 3). |
| Sinks | Safe, except the span clock pointer is called unmasked. | Mask it. |
| Module SysEx commands | Calls can be in flight. | Count calls in flight. |
| Worker, files, tick | Safe. | None. |
| Event tables | Unsafe while edited in place. | Publish by pointer swap. |
| Info table | Safe. | None. |
| Providers and dependents | Dependents' imports are absolute provider addresses. | Dependents go first. |

## Decisions

1. Every ColdFire hook goes through a runtime-owned gate with a slot and an entry/exit counter. Cost: about 5 instructions on entry and 2 on exit per hook call. Unload clears the slots, then waits until the counters are 0, checked once per UI pass. On timeout the result is E_BUSY and the memory is kept. There is no opt-out switch: one path. Interrupt sites keep the plain slot stub without a counter. A per-hook counter skip is allowed only if a measured hot hook needs it (not built). S3 starts with a ceiling (hook calls per second from hook hit counts) and one instruction-ratio A/B.
2. Hot reload is refused while the transport plays or records.
3. A swap is refused when old and new do not fit in memory together. Replace loads the new part beside the old, switches the slots under one mask, then drains and frees the old. Rollback switches back.
4. A service whose hook sites were not gated at boot installs on the next restart. The runtime builds the gates at boot from the BIND stock bytes, before tasks run. It never needs a reflash.
5. Timing: S0 (exit convention) now, with the bind-time firmware addresses item right after the R12 merge. S1 to S5 after R12-8.

## Where the code lives

| Part | Place |
| --- | --- |
| Gate builder and exit stubs `dk_tail`, `dk_exit`, `dk_ret` | Runtime `Installer` (`loader/install.rs`). The Installer becomes a trait with a ColdFire and a DSP backend. |
| Slot and counter words | Runtime memory. |
| Quiesce and unload protocol | `Loader`. Services and modules share one lifecycle: clear slots and row table, drain counters, `fini`, registry remove, release DSP, free. |
| Pointer-swap tables | `Registry`. |
| DSP backend: zero the slot, wait two blocks, free | With R12-8. |

Services and modules only declare hooks, slots and rows. Content modules do not hook firmware.

## Live state

- Sounding voices end at the swap.
- Kits and projects that name a removed type need the MISSING placeholder first.
- Settings keyed by module id stay.
- Open listings use generation counters and the selection-view rebuild.
- Pending DSP transfers are drained by quiesce. Worker and files calls return E_BUSY.

## Protocol

- Refuse during transport or card operations, and in safe mode.
- Unload order: dependents, then the service.
- Replace: stage; check role, ABI and stock bytes; fit against the live set with old and new counted; load beside; switch under one mask; drain; free.
- A service replace cascades to its dependents.
- A fault goes to crash capture, and the next boot is safe mode.

## Rules for work before hot reload

- Hook asm leaves only through the `dk_tail`, `dk_exit` and `dk_ret` imports: no raw `rts`, no `jmp` through table words.
- Keep the restore code.
- No image or runtime word points into a `.dks`, except slots and the row table.
- Bind-time addresses keep firmware addresses in the service's own data, and keep each site's stock bytes in BIND.
- R12-8 follows the SHARC slot rules above.

## Stages

| Stage | Content | Host tests | Device proof | Estimate | Payoff | Confidence |
| --- | --- | --- | --- | --- | --- | --- |
| S0 | Exit convention, with the bind-time addresses item | `objdiff` explained | PCM unchanged | 0.5 day | Hooks can be gated later without edits | High |
| S1 | Code-only modules: revert parts of a12048c46; MIDI-command counter; masked span clock; live-set fit | Loader fakes for order and E_BUSY | 20 load and unload cycles with `DIGI_FLUSH_CHECK=1` | 2 days | First live unload | High |
| S2 | Interrupt-slot service and row-table counter | Registry swap tests | DN2 frametype | 2 days | Interrupt services reload | High |
| S3 | Gates for all ColdFire hooks and service replace | `FakePlatform` gate and counter tests | fx swap during a key loop | 4 days | Services replace live | Medium (hook cost not yet measured) |
| S4 | Machine and fx modules: MISSING placeholder, whole-project scan, view rebuild, DSP grace wait | Fakes for scan and rebuild | Machine swap with a project open | 4 to 5 days | Content modules reload | Medium |
| S5 | SHARC slots | Rig tests of the slot rules | DSP swap with a voice held | 2 days, after R12-8 | DSP code reloads | Medium |

S3 starts with the hook-call ceiling and the paired A/B (`tools/devtools/coupled_ab.py`); stop if the cost is weak against the payoff.

## Reuse from the archive (tag digikit-hot-reload-v1, revert of commit a12048c46 "digikit runtime: archive hot reload")

An earlier hot reload was built and proven on the emulator only, then archived. The new plan reuses:

- Runtime: `dkrt_unload`, `loader::unload`, `CMD_UNLOAD`; the in-use scan `registry::in_use` with its `dynreg_info` words (`F_PROJECT`, `F_KIT`, `F_TRACK`, `TRACKS`); the machine-view rebuild (`registry::view_now`, `registry::refresh_view`); the run-time DSP load path and the load queue (S4, S5).
- `core/dynreg`: the view hook op and the anchors for project, kit and track type (regenerate port anchors with `port_resolve resolve-all`).
- Proofs: the unload order (machine row, voice row, range), voice rows written atomically, unload refusal while a track of the active kit uses the machine (finding 33), DT2 pluck PCM bit-identical with the key held (finding 32).
- Findings 28, 29, 30, 32, 33, 34 and 36.

Still open from the old risk review: a project round trip (save, unload, reopen, save, reload in another order, reopen: byte-identical), a load and unload soak with write-watch and flush check, and a stress test of unload while a DSP voice releases. The old menu showed a finished background load only after the next key press.
