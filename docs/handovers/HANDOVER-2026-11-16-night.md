# Handover 2026-11-16 night: stack A built, two R12 gates, T2 test pyramid started

Start from docs/plans/sdk-runtime-roadmap.md, then this file. The R12 design for every step (R12-2 to R12-7) is in docs/plans/runtime-and-services.md section 13.2; T2 is in docs/plans/test-architecture.md. This file holds state and next steps.

## Merged today (evening)

| PR | What |
| --- | --- |
| 547 | web test-wasm: stale size cap and message removed; test-wasm keys on the built wasm |
| 548 | docs describe the service crates, R11 states and the Storage interface as built |
| 549 | the three full-tier reds: sharc fast-tier test runs the fast tier (option 10, SYSCTL named MMR); wasm CLI tests get a 900 s limit; runtime_fixture drops the wall-clock boot cap |
| 550 | gate throughput (T2 step 1): no phantom device halves; emu-slot admit lock expires after 30 s; test-only Rust files out of `production`, gate roots and the daemon test key; mergeTests = smoke set; `mise run test:subset` |

Batch proof: the full tier on be8098f74 had no device failure (stopped after 48 device tasks + 7 conformance, then the 9 `full: true` tests ran green on both devices). The device phase had run nearly serially: emu-slot's idle-core admission needed 6 idle cores; `DIGI_EMU_SLOTS=4` is the stopgap until T2 makes gates small.

## Branches (not pushed unless noted)

| Branch | Lane | State |
| --- | --- | --- |
| `rt/r12-2-format-v2` | sdk-ui | R12-2 done, host-green |
| `rt/r12-4a-abi-shape` | sdk-design | R12-4a done (C-ABI row, non-generic table; +468 B) |
| `rt/r12-3a-loader-stack` | sdk-design | R12-3a done (Loader, Images/Registry seams, record space, role steps; +336 B) |
| `rt/r12-3-stack` | sdk-design | R12-3 done (providers in the boot, installer, E_HOOK_STOCK -82, provider exports, svc-test hook on SETTINGS list tail, providers device test written; runtime +8.9 KB accepted) |
| `rt/r12-4-stack` | sdk-ui | R12-4 steps 2-6 done on R12-3a (fingerprint H_SVC_ABI/CAP_SVC_ABI/E_SVC_ABI -81, dkc_* imports + dkrt-svc-rt, Registry add/remove, lfo.dks, lfo device test written; +2.9 KB then -0.8 KB) |
| `rt/r12-stack-a` | sdk-design | ASSEMBLED, head 4c23f5c7d: R12-2 to R12-4 incl. lfo.dks + main; host-green (258 runtime tests, patchkit, gen --check, layers); every .dkm byte-identical except lfo-shape*; runtime text 231,298 B. Gate A next (it predates PR 550: merge main again first for `test:subset`) |
| `ui/r12-7-transfer-services` | docs-design | R12-7 code + joins written, not compiled; needs rebase on stack A (RUNTIME_SERVICE_NAMES generator, real binder) |
| `rt/r12-6-stub-tooling` | t1-tests | R12-6 patchkit `op = "stub"`, `[[slot]]`, slot table; tests + docs |
| `fix/rpc-after-install` | docs-plans | FIXED, e41c85dbd: after `restart()` the first stock RPC waits for the stock service (read-only screenshot probe, up to ~30 s; a write is never resent); host test + storage DT2 4/4. Land it if not merged |
| `docs/r12-plan-update` | docs-handover | plan/roadmap/AGENTS.md updates for all of today's decisions; this handover |
| `test/fx-pyramid` | test-fx-pyramid | one-file zero-DSP change; do NOT merge (zero DSP fails E_DSP_XFER) |

## Next (in order)

1. Finish `rt/r12-stack-a` (host-green, byte identity, sizes), then gate A on a quiet machine (no builds): providers test (`core/runtime-provider-tests`), lfo test (`core/runtime-lfo-tests`), refusal tests, plus `mise run test:subset -- <tags of the touched areas>`; DN2 then DT2, `DIGI_EMU_SLOTS=4`. Then merge stack A.
2. Before gate A: the DT2 `clone.6` PC probe (decides whether R12-6 needs a stub there; `coupled-run --pc-probe`, docs/INSTRUMENTATION.md).
3. Land `fix/rpc-after-install` (gate: dk-session host tests, gate-smoke, gate-pins, static) if it is not merged yet. T2 step 3 harness events (FW_READY) can be built with batch BC.
4. Batch BC on stack A: R12-7 integration (rebase, generated service names, wasm pk_bind, screenshot vs mockup), R12-5 fx.dks (design 13.2; FX contract suite on dyn-fx-gain + bipolar fixture variant; fx host tests over FakeCore), R12-6 machines.dks (stub tooling branch; frametype slot proof on DN2). One gate BC, then the full tier and the PGO retrain.
5. T2 step 2 lanes (test-only, independent review for deletions): FX contract suite (ledger done: ~92 → ~30 boots), machine equiv → rig references (new pins, Em reviews once; then the 8 old pins go), usb-fast matrix, card/settings duplicates.
6. After R12: P8c, P8d, P9 (hardware go), R14 (after R12-8, separate go).

## Findings today

- Storage DT2 "no reply to MidiRpc 11": a boot race, not storage code. After a module install restarts the device, the runtime answers at once but the firmware is still on its boot intro, which drops stock MidiRpc requests (emulator frame source "intro"). Fixed in the session (branch above); T2 step 3 adds a firmware-ready event so tests wait on firmware state.
- Zero-DSP FX: FX modules fail E_DSP_XFER (-35) without SHARC; FX device tests keep SHARC; the saving comes from the contract suite.
- The daemon test key hashed `cfg(test)` files (9); fixed in PR 550.
- Runtime image service code still to move: machines 11.9 KB, fx 10.8 KB, samples 17.2 KB text + 35.9 KB data (lfo out on stack A).

## Working rules learned

- Builds and device runs never overlap; at most 2 Rust builds when no emulator runs. Timing-sensitive tests (usb-fast, storage) flake under build load.
- One device gate per stack; named subset via `mise run test:subset`; full tier per batch.
- Do the test pyramid as we go (T2), not after.
- Lane agents edit and compile; blind coders only for small disjoint edits.
- Check a test's DSP mode and timeout basis before calling it timing-sensitive.

## Lanes and cleanup

Lanes in use: sdk-ui, sdk-design, docs-design, t1-tests, docs-plans, docs-handover, test-fx-pyramid, sdk-gen (full-tier lane, detached at main). Em removes merged branches and lanes; the accidental lane `--help` can go: `git -C ~/src/digi/digitakt2 worktree remove ~/src/digi/lanes/--help && git -C ~/src/digi/digitakt2 branch -D lane/--help`.
