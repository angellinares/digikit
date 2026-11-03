# Stock FX audio oracles

`dn2-trackfx.ts`, `dn2-sendfx.ts`, `dt2-trackfx.ts` and `dt2-sendfx.ts` are the canonical scenario sources for
the stock FX oracles: 47 trackFX and 87 sendFX scenarios on DN2 1.12, and 60 scenarios on DT2 1.17. Run a
source with Node >=24 to print its scenario JSON. `tools/emulator/coupled.py` accepts a source through
`--scenarios`, evaluates it, and writes content-addressed JSON under `out/scenarios/` for the native runner.
Fixed instruction slots include zero-detent padding, the routing window and the filter prelude.

The assertions are Vitest tests in the projects `fx-dn2` (`dn2-trackfx.test.ts`, `dn2-sendfx.test.ts`) and
`fx-dt2` (`dt2-fx.test.ts`), sharing `fx-support.ts`. Python owns rendering, the render cache and all NumPy signal
analysis (`tools/emulator/fxcheck.py`, `tools/emulator/audiocheck.py`).

## DN2 coverage

| Test (name prefix) | Quick | Additional sweep |
| --- | ---: | ---: |
| `test_healthy tf-*` | 10 | 37 |
| `test_readback tf-*` | 9 | 0 |
| `test_zero_is_dry tf-*` | 3 | 3 |
| `test_mono_stays_mono tf-*` | 7 | 0 |
| `test_bit_reduction_error_rises metric fast/sweep` | 2 | 2 |
| `test_overdrive_distortion_rises metric fast/sweep` | 2 | 2 |
| `test_srr_rate_falls rate fast/sweep` | 1 | 1 |
| trackFX total (79 identities) | 34 | 45 |

| Test (name prefix) | Quick | Additional sweep |
| --- | ---: | ---: |
| `test_healthy sf-*` | 11 | 76 |
| `test_readback sf-*` | 8 | 0 |
| `test_zero_is_dry sf-*` | 0 | 3 |
| `test_mono_stays_mono sf-del-ref` | 1 | 0 |
| `test_delay_spacing fast/sweep` | 1 | 1 |
| `test_delay_feedback_zero_one_echo` | 1 | 0 |
| `test_delay_default_feedback_minus_6_db` | 1 | 0 |
| `test_delay_wid_min_is_left` | 1 | 0 |
| `test_delay_wid_max_is_right` | 0 | 1 |
| `test_delay_feedback_above_unity_swells` | 0 | 1 |
| `test_reverb_rt60_rises_with_decay fast/sweep` | 1 | 1 |
| `test_chorus_rate_rises_with_speed fast/sweep` | 1 | 1 |
| `test_chorus_decorrelates_with_width fast/sweep` | 1 | 1 |
| sendFX total (112 identities) | 27 | 85 |
| Combined (191 identities, 134 scenarios) | 61 | 130 |

SendFX checks delay spacing within 2 ms, one echo at zero feedback, default feedback within 1 dB of -6 dB per
repeat, left/right width separation (40/30 dB), repeat growth above unity, rising reverb RT60 and chorus rate,
and falling chorus correlation. Readback uses unsigned stored values. Feedback-above-unity exceptions are
`sf-del-fb-u44`, `sf-del-fb-u127` and `sf-rev-decay-u127`; the sparse `sf-del-time-u127` echo has a peak
<=1.5 times dry exception. Master compression has generic health coverage only.

Health: finite samples, gate success, DC <1e-3, peak <=1, no growth except the exceptions above. Readback:
exact changed-parameter membership and UI values. Zero and mono checks use `np.array_equal` observations,
including reference shape equality. BR/OVER need strictly increasing THD and dry-difference metrics; SRR needs a
strictly decreasing effective rate. Series are ordered by unsigned readback values with distinct settings and
finite metrics. Nonfinite analysis values become JSON null at the interchange boundary and fail the finite check.

## DT2 coverage

`mise run test:fx:dt2` selects 135 quick assertions; `DIGI_FX_SWEEP=1` selects all 146 across 60 scenarios.
The scenarios use DT2 1.17 ready inputs and the one-sample card. The DT2 encoder calibration ladder is printed
by `node tests/regression/scenarios/dt2-trackfx.ts --ladder` (46 fixed-slot scenarios, no note played).

## Commands

```sh
mise run test:fx                                          # 61 quick DN2 identities
DIGI_FX_SWEEP=1 mise run test:fx                           # all 191, all 134 scenarios
mise run test:fx -- -t 'test_healthy tf-br-127'             # selected scenario + dry reference
DIGI_FX_NO_AOT_SUBSET=1 mise run test:fx                    # 17 DN2 no-AOT identities
mise run test:fx:dt2                                       # 135 quick DT2 identities
DIGI_FX_SWEEP=1 mise run test:fx:dt2                       # all 146
```

The commands never build. They need the existing stock coupled-run binary for the device, a current generated
core, the ready firmware/DSP/CF inputs, and for DN2 the MAIN OS descriptor image. Preflight fails on missing
or stale inputs; `--no-build` only establishes that the binary exists, so the caller must ensure it is current.
Vitest writes its scenario document to the owned run directory as `scenarios.json`.

One Vitest worker invokes the Python coupled CLI with `DIGI_JOBS` (default 1). It keeps the shared benchlock,
an independent ready-snapshot restore per scenario, the content render cache and budgeted sharding. Each
Vitest test declares the scenarios it needs; one batch adds dry references and routing companions, so a name
filter renders only that closure. The renderer uses `--check-generated` to reject a stale core.

## Runs and artifacts

Runs use `out/regression/{trackfx,sendfx,dt2-fx}/<timestamp>-<pid>/`. Reports, decoded measurements and command
logs survive success and failure. WAVs are pruned after the suite unless `DIGI_FX_KEEP_RUNS=1`, which the
clock-sweep tool uses. Subprocess groups are bounded and killed on timeout, failure or cancellation.

## Gate

- `check` runs the 61 quick DN2 identities; full verification runs all 191.
- `check-no-aot` runs 17 identities even in full verification: trackFX health for dry/br127/over127/srr127,
  readback for br127/over127/srr127 and the quick SRR series; sendFX health/readback for delay-time-u20,
  reverb-decay-u20 and chorus-speed-u20, default delay feedback, and the quick reverb RT60 and chorus speed
  series. `--all-no-aot` selects 61, or 191 with full verification.
- Tier PCM comparisons (AOT against no-AOT) use the same sources and a fixed scenario subset.
- `dt2-check` runs the DT2 quick selection unless `DIGIKIT_TIER=full`; `mise run gate:full` (or `pnpm nx:pilot run gate-tier:dt2-tier-sweep`) also compares
  DT2 FX PCM with and without AOT.

## Known failure

**[O]** `test_chorus_decorrelates_with_width sweep` (DN2, full tier) fails: unsigned width readback
`0, 10752, 27392, 32512` gives wet correlation `1.0, 0.372, -0.469, -0.452`, so the last step breaks the strict
decrease. The same result holds with a fresh binary and with AOT off (identical PCM hashes). DN2 1.11 passed
an earlier run, so it is open whether the oracle or the DSP behavior needs correction. The assertion is
unchanged and not skipped.

## Adding cases

Add scenarios and assertions to the DN2 or DT2 TypeScript source and test file here. Declare the scenario
names beside each Vitest test so selection renders only its closure. Put numerical measurements in
`tools/emulator/fxcheck.py`.
