# Instrumentation

Diagnostics are off by default and cost nothing when off. Tuning switches
(which tier runs what) are separate and listed at the end.

## Where output goes

Every run tool writes one directory per run:

```
out/runs/<scenario-or-kind>/<UTC stamp>/
    report.json   coupled-run report (tools/emulator/coupled.py); perf: perf.json + report.md
    audio.wav     with --wav or --diag pcm
    logs/         everything `--diag` records, plus stderr.log
out/runs/<scenario-or-kind>/latest -> <newest stamp>
```

`tools/emulator/coupled.py scenario|replay`, `tools/devtools/perf.py` (`perf`) and
`tools/devtools/fastcov.py` (`fastcov`) use it. `--out` still overrides: coupled.py puts
the report at that path and audio.wav and logs/ next to it; perf.py and
fastcov.py take a run directory. Caches stay outside runs: the perf cost
calibration and ranking (`out/native/perf/`), fastcov entry lists
(`out/native/fastcov/`), bisect references (`out/native/bisect/`).
`tools/emulator/digi_env.py` holds the layout (`run_dir`) and the diagnostics table
(`DIAG`, `diag_env(names, run_dir)`).

## `--diag LIST`

`uv run python -m tools.emulator.coupled scenario NAME --diag dspi,wwatch=1a0:4,hist`.
A name that takes a value is `name=VALUE`; a comma-separated value is fine
(`pcdump=0x100,0x200`). Setting any name also sets `DIGI_DIAG_DIR=<run>/logs`,
the default directory for every native `*_OUT` that is unset.

| name | records | file in logs/ | variables |
|---|---|---|---|
| `dspi` | DSPI traffic (binary, `DSPILOG1`) | `dspi.bin` | `DIGI_DSPI_LOG` |
| `dspwatch=ADDR[,ADDR]` | DSP words, one row per frame | `dspwatch.csv` | `DIGI_DSP_WATCH`, `_OUT` |
| `model` | per-frame DSP tier counters | `model.csv` | `DIGI_DSP_MODEL_LOG` |
| `wwatch=ADDR[:N],..` | changes to DSP words: icount, PC, unit | `wwatch.csv` | `SHARC_WWATCH`, `_OUT` |
| `pcdump=PC,..` | registers at those PCs, the PC and loop stack depth, and a `#stack-max` line (`pcdump=0` records only that; `{pid}` in `_OUT` is the process id) | `pcdump.csv` | `SHARC_PCDUMP`, `_OUT`, `_ICOUNT=lo:hi`, `_LIMIT`, `_MEM=addr:words` |
| `fastcov` | fast-tier coverage, refusals, exits, interpreter histogram | `stderr.log` | `SHARC_FAST_LOG=summary`, `SHARC_FAST_COV`, `SHARC_FAST_HIST` |
| `fastlog` | fast-tier summary | `stderr.log` | `SHARC_FAST_LOG=summary` |
| `fasttime` | per-slot host time of the fast tier | `stderr.log` | `SHARC_FAST_TIME=1`: on engine drop, prints a `fast time` table per fast-tier slot, sorted by host ticks: PC, `auto` or `list`, `loop` or `cfg`, region length, calls, guest instructions, ticks, ticks per guest instruction, and the tick rate |
| `tiertime` | host time per DSP tier and per thread | `audio.report` `dsp_tiers.tier_time`, `link_timing` | `SHARC_TIER_TIME=1` (also turns link timing on): host seconds per audio second in the interpreter, generated blocks, fast tier and idle skip (time between two tier calls counts for the tier before it), the DSP worker's busy, wait and other time, and the ColdFire side's wait for the DSP. `rt_matrix run --tier-time` adds the `host` block (ColdFire thread CPU, waits, DSP tiers) to each cell. Cost: one tick read per change of tier |
| `hist` | interpreted instructions per PC | `stderr.log` | `SHARC_FAST_HIST` |
| `dump` | fast-tier kernels and real calls (wasm gate input) | `fastdump/` | `SHARC_FAST_DUMP`; needs the `fast-dump` feature (coupled.py adds it) |
| `pcm` | the rendered audio | `../audio.wav` | `--wav` |

Not a `spikes` name: `SHARC_UNINIT=<hex seed>` makes DSP bytes that no boot block loaded and no store wrote read as a seeded non-zero pattern
instead of zero (default off, part of the render cache key); `SHARC_UNINIT_LOG=FILE` lists the first reads (finding 49).
`SHARC_UNINIT_TRAP=1` (default off, value unchanged) logs each data read of a never-written byte once per word-aligned address as `uninit-read addr=0x.. w=N pc=0x.. n=COUNT` to `SHARC_UNINIT_LOG` (stderr if unset), again at counts 64, 128, ...; widths 1, 2, 4 and 8, no instruction fetch. Silicon parity (MODE1 bits 27-29) may fault on such reads.

`SHARC_IRQ_PROBE=LO-HI[,LO-HI...]:MASK[:EVERY]` (hex software PCs, an IRPTL bit mask, a decimal divisor; default off, part of the render cache key)
latches the interrupt bits MASK just before the interpreter executes an instruction in a window, once per visit of that PC and for every EVERY-th
visit, so the DSP takes the kernel's tick (`400000`) or context-switch request (`80000000`) between any two instructions of patched code, as silicon can
and the coupled model otherwise does not (it delivers peripheral events between runs of the core). The emulator prints the counters when it exits.
Scenario key `[rungc] irq_probe` (finding 49 section 9).

`SHARC_REPLY_INFLIGHT=1|fail` (default off, no cost when unset; set it before the daemon or coupled run starts) flags core writes into the SPI2 reply
record while it is being sent. The emulator reads the record in one instant when the ColdFire frame arrives; on silicon the transmit DMA reads it while the
ColdFire shifts the frame out (2748 bytes, DSPI2 at 44 MHz: about 0.5 ms of the 0.667 ms block). From each exchange for that time (in DSP instructions) the
check compares the record with a snapshot after every dispatch unit and reports each changed 32-bit word: frame number, word index, PC, unit letter (`i`
interpreter, `b` block, `f` fast tier: only `i` gives the writer's own PC, but patched code appended by the runtime always runs in the interpreter), and
`EARLY` (before the DMA read the word: the wire carries the new value) or `late`. It sees changes of value, not stores of the value already there.
`fail` also halts the engine at the first early change (the daemon reports it as a DSP halt). A summary with the per-PC counts and the record addresses goes to
stderr and to `SHARC_REPLY_INFLIGHT_OUT` (default `$DIGI_DIAG_DIR/reply-inflight.txt`) when the engine is dropped. Knobs: `SHARC_REPLY_INFLIGHT_SCK` (Hz, default
44000000), `SHARC_REPLY_INFLIGHT_IPS` (DSP instructions per second, default 1500 x `DIGI_DSP_PERIOD`), `SHARC_REPLY_INFLIGHT_SKIP=N` (leave out the first N
exchanges: the stock firmware writes a few record words while it boots). The DMA's FIFO read-ahead is not modelled. Finding 31 has the record, finding 78 the results.

`DSP_FRAME_POKE=OFF=VAL[/TRACKS][,...]` sets the big-endian 16-bit word at frame byte OFF (with `/N`, at
OFF + 0x60*t for t < N) in every frame sent to the DSP, to drive a frame word no stock control reaches
(finding 06, "Parameter channel"). `tools/patchkit/hook_ab.py` renders and compares paired runs with a DSP patch.

`wwatch` units: `i` interpreter step, `b` generated block, `f` fast tier,
`s` idle skip; for exact writer PCs also run with `SHARC_AOT=0
SHARC_FAST_REGIONS= SHARC_BLOCKTRANS=`. `fastcov` output is read by
`tools/devtools/fastcov.py` (`Diag`); `perf.py` and `fastcov.py` run it themselves.

DSP memory dumps: `uv run python -m tools.emulator.coupled scenario NAME --dump-dsp` (or
`coupled-run --dump-dsp PATH`) writes the DSP's canonical state blob (`SHRD`,
every byte written since the image, as ranges; the format of
`tools/sharc_dev/sharc_diff.py`) to `<run>/dsp-<scenario>.bin` at the end of the run.
`tools/emulator/dspdiff.py A B [C..] [--image dn2-1.11] [--ignore-volatile --noise X Y]`
lists the differing words per range with float32 values and the functions that
reference them. Runs are deterministic, so two dumps of one scenario are equal
and scenarios with identical event times give dumps at the same DSP instruction
count.

`SHARC_PROFILE_PREFIX=PATH` (default off; no cost when unset) turns on the profile maps (option 26) in every SHARC engine of a live
process, for example `digi-emud`. When an engine is dropped (daemon restart or exit) it writes `PATH.e<pid>-<n>` (coverage: `pc mode known count`),
`.entries` (`pc count`) and `.trans` (`from to count`), the inputs of `sharc_rsgen --coverage/--entries/--transitions`. Merge them with
`tools/sharc_dev/aot_fxprof.py --live PATH` (finding 18, "Send-FX coverage"). The daemon runs slower while it records.

To find generated regions that the runtime patches bar (they run in the interpreter), capture with
`SHARC_PROFILE_PREFIX=out/prof/cap SHARC_CODE_LOG=1 mise run wb` (the folder must exist; quit the app normally so the profile is written).
Then run `mise run perf:fast-roots -- rank --prefix out/prof/cap --device dn2`. It ranks the regions by interpreted instructions and prints
roots for `fallback_regions` in `config/emulator.json`. Check each root for exactness. The gate target `fast-roots`
(`mise run perf:fast-roots -- check --device dn2,dt2`) fails when the interpreted work per audio second is over `tools/emulator/fast_budgets.json`.
`mise run perf:rt-matrix` measures RT, tier shares and PCM parity per machine and setting; its `gaps` subcommand ranks interpreted work by function.

Replay fixtures (`node tools/devtools/runtime_fixture.ts --device dn2`) save the state right after the first boot. Benchmarks and PGO training should also use the `reset` state (`--state reset`, default dir `out/fixtures/runtime/dn2-reset/`, DN2 only): the state after a guest soft reset, which is what a device with installed modules runs. The two-core `train` adds it as an extra input when that directory exists.

Not in `--diag` (set the variable by hand): `SHARC_LIVE_AOT_CAPTURE_PC/_DIR/_LIMIT`
(raw block-call snapshots), `SHARC_FAST_DUMP_ALL`, `SHARC_FAST_CL_DISASM`,
`DIGI_LP0_LOG` (stderr), `DN2_PROFILE_LINK=1`,
`DIGI_EMU_SINK_WAV`, `DIGI_EMU_PCM_F32LE` + `DIGI_EMU_PCM_SECONDS` (desktop audio
captures), `DIGI_COUPLED_PCM_F32LE`, `DIGI_COUPLED_DSP_EXPORT` (the pinned test),
`DIGI_PANEL_TX_LOG=PATH` (front-panel LED bytes from UART8: one line `icount hex` per drain), `DIGI_INTC_FORCE=0` (turn off delivery of guest-forced INTC sources, INTFRCH/INTFRCL: the
sequencer step ISRs; on by default since 2026-10-13, always on in wasm; for A/B only; finding 19).

## Write-watch and flush check (ColdFire)

Two instruments for the native ColdFire core, off by default. They extend the write-range guards
(`--guard`, `packages/emulator/boot/src/guard.rs`) and share their state; the code is
`packages/emulator/boot/src/watch.rs`. They are read from the environment when an `Emulator` is built (desktop,
`coupled-run`, `regions_probe`, wasm builds read nothing).

| variable | effect |
|---|---|
| `DIGI_WRITE_WATCH=LO-HI[,..]` | report every write into these guest ranges (hex; `LO+LEN` also works; HI exclusive). Alias addresses `0x48000000..0x4fffffff` fold onto DDR, so a range is given once, in the cached view. |
| `DIGI_WRITE_WATCH_ALLOW_PC=LO-HI[,..]` | stores from these PCs are not reported (the runtime's own code). DMA has no PC and is always reported. |
| `DIGI_WRITE_WATCH_AFTER=cacr\|ICOUNT` | start reporting at the first CACR write that enables a cache, or at an instruction count. Without it the reset-path RAM clear is reported too. |
| `DIGI_FLUSH_CHECK=1` | the flush check (below). Independent of `DIGI_WRITE_WATCH`. |
| `DIGI_WRITE_WATCH_OUT=PATH` | JSONL log; default `$DIGI_DIAG_DIR/writewatch.jsonl`, else stderr. The summary goes to `PATH` with the extension `summary.json`. |
| `DIGI_WRITE_WATCH_LIMIT=N` | log at most N events (default 100000); the counts go on. |
| `DIGI_READ_WATCH=LO-HI[,..]` | the read-watch: report every CPU load from these ranges (same syntax and alias folding as `DIGI_WRITE_WATCH`), and count per byte the loads that read it and the first 8 PCs. It starts with `DIGI_WRITE_WATCH_AFTER` and logs to the same file. The summary adds `read_events`, `loads_checked` (every load the watch was offered) and `reads` (per byte: `addr`, `loads`, `pcs`). Each load then takes one more branch, as each store does. |
| `DIGI_CARD_TRACE=1` | log one `card` event per card command: CMD8, CMD18 and CMD25 at the eDMA59 service (`arg` = sector, `bytes` = transfer) and CMD12 and CMD23 from the eSDHC command decode (`bytes` 0). It counts as an instrument on its own (no range needed), starts with `DIGI_WRITE_WATCH_AFTER`, and logs to the same file and under the same `DIGI_WRITE_WATCH_LIMIT`. Off: no buffer, no cost. |

Events (JSONL), `ic` = guest instruction count:

```
{"t":"w","ic":19811920,"pc":"0x400004d2","addr":"0x44e4a5b8","size":4,"value":"0x0"}   CPU store
{"t":"r","ic":..,"pc":"0x..","addr":"0x..","size":2}                                    CPU load (read-watch)
{"t":"dma","ic":..,"src":"esdhc-dma59","addr":"0x..","size":512}                       DMA into RAM
{"t":"card","ic":..,"cmd":25,"arg":1234,"bytes":65536}                                  card command (DIGI_CARD_TRACE)
{"t":"flush","ic":..,"fetch_pc":"0x..","line":"0x..","writer":{"pc":"0x.."},"write_ic":..,"dcache_pending":true,"mask":"0x000f"}
```

DMA sources are the paths that report to `Board::take_dma_written_ranges`: `esdhc-dma59`, `dspi1-rx`, `dspi2`.
A DMA event is reported when the runtime drains the board (after the instruction), so its `ic` can be a
few instructions late, and it has no value. The summary has the events, a count per PC (most frequent
first), a count per DMA source, the flush hits and work counters (`cacr_writes`, `flush_stores_tracked`,
`flush_fetches_checked`, `flush_same_line_clean` (fetches on a dirty line that did not overlap the stored
bytes), `cpushl`) that show the check saw traffic. A flush hit in the log and summary has `mask`, the dirty
bytes of the line (bit n is byte n). `Emulator::finish_write_watch()` writes
the summary at once; it is also written when the emulator is dropped.

Flush check. V4 caches are not coherent on hardware (copyback data cache, separate instruction cache); the
emulator is, so code that runs after being written without a flush works in the emulator and fails on the
device. A store or DMA write marks its bytes dirty in each 16-byte DDR line it touches while CACR[IEC] is
set. A fetch whose instruction bytes overlap dirty bytes of a line that is not yet cleaned is reported once
per line, with the fetch PC and the last writer. Cleaning works per line. A line is cleaned by both of:

- data side: a CPUSHL with dc or bc (push) after a cached store; not needed after an uncached-alias store
  or DMA; CACR[DCINVA] counts;
- instruction side: a CPUSHL with ic or bc that invalidates (CACR[IDPI] = 0 or IVO = 1), or CACR[ICINVA].

CPUSHL is modelled as the hardware does: with CACR[SPA] set (the stock firmware's `0xa50ce100`) the address
register is a physical address and cleans that line; with SPA clear it names a set (bits 10:4) and a way (bits
1:0), and a line is cleaned when all four ways of its set were done since the last store to the set. Stores
while the instruction cache is off do not count (the reset-path loader runs before CACR is written).
Limits: all bytes of an instruction are checked, also when it crosses into the next line; only the DDR window `0x40000000..0x47ffffff` is tracked; stores made by host code (`poke32`, HLE) are
not seen; the emulator itself does not alias the uncached view (a store through `0x48...` lands in separate
memory, as `tools/coldfire/regions.py` already notes).

With either instrument armed the bus takes the single-step path: fused loops and the reset RAM-clear batch
are off (as with `--guard`), so a run is slower: DT2 1.16 boots to ready in about 15 s of wall time with
both on (about 1 billion instructions).

`packages/emulator/boot/examples/cf_script.rs` runs a patched image with a card through a JSON script of runs, pokes, peeks, waits and direct calls with these variables set; `tools/patchkit/pk_dkrt.py proof` and `tools/patchkit/pk_cdemo.py proof` are its users (findings 29 and 35; the hot-load `pk_registry.py proof`, which called the firmware's own list getter through a stub it poked into free RAM, is archived with hot reload).

## Task stack high-water (ColdFire)

`DIGI_STACK_WATCH=LO-HI[,..]` (hex, HI exclusive) names task stacks. Every CPU store into one is tracked after the write-watch
start (`DIGI_WRITE_WATCH_AFTER`: the boot-time RAM clear writes the whole bss, stacks included, so set it to `cacr`). A stack grows
down, so the lowest address a store reached is the high-water use; a frame that was reserved but never written is not seen (the
reserve is small next to the stacks measured). Interrupt handlers push on the interrupted task's stack and are counted. The
instrument needs `DIGI_WRITE_WATCH` or `DIGI_FLUSH_CHECK` only for the bus to take the single-step path; it works alone too.

| variable | effect |
|---|---|
| `DIGI_STACK_WATCH=LO-HI[,..]` | the stacks to track |
| `DIGI_STACK_WATCH_EPOCH=LO-HI[,..]` | a store into these ranges ends an epoch: the low-water mark is kept per epoch, so a phase of the run is measured by writing a marker word at its start and its end (the menu has `MARK`, written at the start and the end of a LOAD or UNLOAD) |

Output: JSONL lines `{"t":"stack","ic","pc","addr","depth"}` for each new low that is 64 bytes deeper than the last one logged, and
`{"t":"stack_epoch","row":{..}}` for each closed epoch; the summary (`PATH.summary.json`) has `stacks` (`lo`, `hi`, `size`, `low`, `depth`,
`pc` and `ic` of the deepest store) and `stack_epochs` (`ic0`, `ic1`, `trigger` = the address that ended it, `low`, `depth`, `pc`,
`low_ic`, `writes`; at most 512). `tools/coldfire/arena_safety.py run(..., env_extra=...)` passes the variables and returns `stacks`
and `stack_epochs` in the facts.

Where the stack of a task is: `uv run python -m tools.coldfire.task_stacks --image dt2-1.16` lists every RTOS task (entry, priority,
stack base and size) from the calls of the firmware's one task-create function and marks the UI task (the one whose loop contains
the queue pop that the runtime hooks); it reads the Ghidra dump of the image. `uv run python -m tools.coldfire.digikit_menu_proof run
--image IMAGE --quick --stack` measures the UI stack during the DIGIKIT proof scripts and prints the high-water outside requests and
inside LOAD/UNLOAD requests; `--stack-stock` does it for the stock SETTINGS and project scripts. `uv run python -m
tools.coldfire.frame_depth modules/core/runtime/build/runtime.dko.json dkrt_load` adds the other side: the static worst-case depth of a
module's functions from their frame sizes and calls (findings 34).

## PC probe (ColdFire)

The PC probe records what the guest does at chosen PCs. It is off by default and costs nothing then: the bus takes no extra
path, and the probe adds only its own PCs to the observed set (`boot/src/pcprobe.rs`, `Emulator::set_pc_probe`). It changes no
guest state. A probe made for a task switch or a hook site settles questions that static reach cannot, such as whether an
interrupt can enter the code of a patched site (R12 design, `docs/plans/runtime-and-services.md` section 13.2).

For each hit of a probed PC, before its instruction runs, the probe counts the hit per (task, IPL) and, for the first `keep`
hits (default 8), records: the instruction count (`icount`), SR and its IPL, SP, the task word, `words` (guest longs at given
addresses; null when unmapped) and `stack` (longs from SP: at a function's first instruction, the return address and the
arguments). "Task" is the long at the address `task_ptr`, the current-task pointer, which the caller gives (DT2 1.17
`0x47db3f64`, DN2 1.12 `0x4664bcdc`; anchor `cf.rtos.value.current_tcb`). With `frame: true` the probe also decodes the exception
frame at SP and counts per (vector, stacked IPL). That is meaningful only at the first instruction of exception code: the long at
SP holds the vector in `(long >> 18) & 0xff` and the stacked SR in the low 16 bits. Spec:

```json
{"task_ptr": "0x47db3f64", "keep": 8,
 "probes": [{"pc": "0x40000410", "label": "task_switch", "words": ["0x47db3f64"], "stack": 2, "frame": true}]}
```

Addresses are integers or `"0x..."` strings. `stack` is at most 64. A spec with no probes removes the probe.

Report (`pc_probe`; the `pc.probe.report` answer):

```json
{"task_ptr": "0x47db3f64", "keep": 8, "probes": [{
  "pc": "0x40000410", "label": "task_switch", "count": 1234,
  "by_task_ipl": [{"task": "0x47dc1000", "ipl": 0, "count": 600}],
  "by_frame": [{"vector": 32, "stacked_ipl": 0, "count": 600}],
  "hits": [{"n": 1, "icount": 851000000, "sr": "0x2000", "ipl": 0, "sp": "0x47dc0ff8", "task": "0x47dc1000",
            "words": [], "stack": ["0x40362000", "0x00002000"],
            "frame": {"long": "0x40802000", "vector": 32, "stacked_sr": "0x2000", "stacked_ipl": 0}}]}]}
```

(`by_frame` and `frame` are there only with `frame: true`; the numbers above show the shape, not a measurement.)

Two ways to run it:

- `coupled-run --pc-probe FILE[:NAME]` (FILE is a spec, or an object of specs with NAME picking one). With `--cold-base` and
  `--dsp zero` the probe is armed before the cold boot, so it is active from reset and keeps counting through the scenarios.
  Every other start arms it at the ready point. The report has `pc_probe`.
- The control daemon: RPC `pc.probe.set {...spec}` or `{file, name?}`, `pc.probe.report`, `pc.probe.clear` (hits only)
  ([CONTROL.md](CONTROL.md)); the TypeScript client has `pcProbeSet`, `pcProbeReport` and `pcProbeClear`. The spec is kept
  and armed in every runtime installed later. A runtime that a cold boot worker builds and snapshots (a patched load, a
  snapshot restore) never sees it: use `coupled-run` for a probe from reset.

### R12 checks 1 and 2

The inputs are `tools/emulator/pc_probe_r12.json` (four specs: `dt2-check1`, `dn2-check1`, `dt2-check2`, `dn2-check2`; the PCs
are the hook sites of `devices/ports/dt2-1.17.toml` and `dn2-1.12.toml`, listed in each spec's `note`) and
`tests/scenarios/r12-probe.json` (`r12-check1-*` and `r12-check2-*`). Build `coupled-run` once for each device, then run the
four commands from the repository root. The stock images are `firmware/Digitakt_II_OS1.17.syx` and
`firmware/Digitone_II_OS1.12.syx`; `--cold-base` takes the image's own SHA-256.

```text
DT2=$(uv run python -m tools.emulator.coupled build --device dt2 | tail -1)
DN2=$(uv run python -m tools.emulator.coupled build --device dn2 | tail -1)
D=firmware/Digitakt_II_OS1.17.syx; N=firmware/Digitone_II_OS1.12.syx
SC="--scenarios tests/scenarios/r12-probe.json"; PP=tools/emulator/pc_probe_r12.json

# check 1: stacked IPL of every task switch
$DT2 --syx $D --dsp zero --cold-base $(shasum -a 256 $D | cut -d' ' -f1) $SC --scenario r12-check1-dt2 \
     --pc-probe $PP:dt2-check1 --out out/runs/r12-probe/dt2-check1.json
$DN2 --syx $N --dsp zero --cold-base $(shasum -a 256 $N | cut -d' ' -f1) $SC --scenario r12-check1-dn2 \
     --pc-probe $PP:dn2-check1 --out out/runs/r12-probe/dn2-check1.json

# check 2: first fire of the vtable-only hooks, value_class, main-OS entry, UI sites, param_code callers
$DT2 --syx $D --dsp zero --cold-base $(shasum -a 256 $D | cut -d' ' -f1) $SC --scenario r12-check2-dt2 \
     --pc-probe $PP:dt2-check2 --out out/runs/r12-probe/dt2-check2.json
$DN2 --syx $N --dsp zero --cold-base $(shasum -a 256 $N | cut -d' ' -f1) $SC --scenario r12-check2-dn2 \
     --pc-probe $PP:dn2-check2 --out out/runs/r12-probe/dn2-check2.json
```

Each run boots the stock image from reset with SSI paced (about 850 million instructions to ready), then plays its scenario.
Estimate, not measured: a zero-DSP run is about 2.2 times real time, so a run takes 1 to 2 minutes (the boot about 15 s, check 1
about 15 s of play, check 2 about 14 s of play, 30 to 40 s wall), and the build of `coupled-run` comes first when it is not built. A probe on a PC
that is hit about 1,500 times per second (check 1) costs one observed-PC call per hit and keeps the fast paths. Read the result in `pc_probe` of each report:

- Check 1: `probes[0].by_frame` is the histogram by (vector, stacked IPL). Controls: `count` above 0, and vector 191 present.
  A row with `stacked_ipl` of 2 or more means a task switch can enter from an interrupt handler (design outcome in section
  13.2). No such row: the stub is load-test-jump and the unload rule holds.
- Check 2: per probe, `hits[].icount` against the `main_os_task` probe's first `icount` (before or after main-OS entry), and
  `by_task_ipl` for the task and IPL of each hit. `param_code.entry` has the return address in `hits[].stack[0]` of each of the
  first 8 callers.

Scenario limits against the design text: the scenarios drive the panel and MIDI DIN. USB MIDI and a card load after ready are
not scenario events, so check 1 has the MIDI DIN note and the card of the boot only (add `--card-image FILE` for a card).
Check 2 uses FUNC+SRC for the machine list and FUNC+FX for the FX view (the FX key alone does not open it on DN2); on a device where that is not the list, the SRC page and the hooks of the list
still fire from the page, and the report shows which probes had no hit.

## Instruction spans (ColdFire)

`coupled-run --span NAME=ENTRY:EXIT` (repeatable) counts the ColdFire instructions from the PC ENTRY to the PC EXIT on each pass.
It also counts the instructions from one entry to the next. The count starts with the sample phase. The exit instruction is not
counted. An exit with no open entry is ignored, and a second entry before the exit counts as `nested`. The instrument
(`boot/src/spans.rs`, `Emulator::add_span`) reads no guest state.

The report has `spans`: one object per span, with `length` and `gap` (`count`, `min`, `mean`, `p50`, `p99`, `max`,
`over_period`). `period` is one DSPI2 frame in instructions at `DIGI_CF_IPS` (88,000 at 132e6). A length or a gap above
it counts in `over_period`. `length` also has `over_unit_period`: the lengths above the period when multiplied by
`unit_scale` (`--unit-scale`, default 1.21). The object gives `unit_scale`.

DN2 1.11 and 1.12 have the vector-191 handler and evaluator A at the same PCs ([finding 04](findings/04-coldfire-dsp-link.md)):

| span | PCs | what it counts |
|---|---|---|
| `isr` | `0x40025e36:0x40027bc6` | the whole handler, from its first instruction to its only `rte` |
| `evala` | `0x400272d4:0x400272da` | the call of evaluator A and its return |

Stock DN2 1.12, `--dsp zero`, 10 s of `idle`: `isr` 49,541 to 50,069 instructions, `evala` 7,841 to 8,369. With `kb-chord16`
(16 trig keys held) the `isr` maximum is 54,560, 62 % of the period. In the emulator the
handler starts every 88,000 instructions, give or take a few. So an entry is late only after a handler that ran past its period.

Three options go with the spans:

| option | effect |
|---|---|
| `--span-log PATH` | writes every pass as a JSON line `{"span", "at", "len"}`, `at` being the instruction count at the entry, with `-<scenario>` before the extension. Each span's report also gives `worst`, the index and the entry count of its longest pass. A short call span can then be placed inside the handler pass it ran in. |
| `--cold-base SHA256` | with `--dsp zero`, boots `--syx` from reset to the ready state instead of loading `--cf-snapshot`. SHA256 names the stock firmware of a patched build. A build with no ready snapshot of its own can then run the scenarios. Without `--card-image` the runtime gives the firmware an empty +Drive. Stock DN2 1.11 reaches ready at 851,236,338 instructions. With `--dsp sharc`, see "A patched build with the SHARC". |
| `--call PC:ARG[,ARG...]` | calls a guest function at the ready point, before the scenario (`Emulator::call_function`, gcc m68k ABI, interrupts masked, a scratch stack 1 KiB below the guest's). ARG is a number, or `*ADDR+OFF`: the long at ADDR, read at the call, plus OFF. The report lists each call's arguments, D0 and instructions. A call that does not return stops the run. |

To compare a patched build with stock, run the same scenario and spans on both with `--cold-base` and compare `length` per span.
On stock DN2 1.11 with `kb-chord16` the `evala` passes also show a stock job every 1,000 frames: +416 to +528 instructions,
in a cycle of four.

### A patched build with the SHARC

`--cold-base` with `--dsp sharc` runs a patched build with the SHARC on the DSPI2 link. The run boots the build once from
reset, and each scenario then starts from that ready state.

1. Make the DSP inputs from the build's own section 7. `tools/emulator/ready.py` writes them in its steps 1 to 3:

   ```text
   uv run python -m tools.emulator.ready dn2 --syx BUILD.syx --out DIR --base-sha256 STOCK_SHA256
   ```

   - `DIR/digi-audio-dn2-image.bin`: the packed image.
   - `DIR/cold-idle.dsp`: the DSP's first idle rest.
   - With `--base-sha256`, step 4 also boots the build (`sharc_live`, `BASE_SHA256`) and saves the ready pair in `DIR`.

2. Boot the build and run the scenarios:

   ```text
   coupled-run --syx BUILD.syx --cold-base STOCK_SHA256 --dsp sharc \
     --dsp-image DIR/digi-audio-dn2-image.bin --dsp-cold-idle DIR/cold-idle.dsp \
     [--card-image CARD.img] [--cold-save PAIR] --scenario NAME ...
   ```

   - The boot is `cold::boot_patched_to_ready`. Each scenario restores its pair (`CoupledRuntime::new_patched_audio`).
   - `--cold-save PAIR` keeps the pair. Without it the pair goes to a temporary folder, and the run removes it at the end.
   - A boot that faults writes `boot-fault.json` in the pair folder. Give `--cold-save` to keep that file.
   - The report's `cold_boot` gives the boot's instructions, DSP frames, wall time and the saved folder.

3. Run again from a saved pair, without a boot. Give the pair in place of `--dsp-cold-idle`:

   ```text
   coupled-run --syx BUILD.syx --cold-base STOCK_SHA256 --dsp sharc --dsp-image DIR/digi-audio-dn2-image.bin \
     --cf-snapshot PAIR/digi-audio-m5.snap --dsp-state PAIR/digi-audio-m5.snap.dsp [--card-image CARD.img] ...
   ```

   The pair of step 1 (`--base-sha256`) works here too.

Use the same `--card-image` for the boot and for each run from its pair. The ColdFire snapshot holds the card's
sample list in RAM.

`--dsp-patch` does not go with `--cold-base` and `--dsp sharc`. The image gives the build's DSP code. Nothing checks that
the image and `cold-idle.dsp` come from the section 7 of `--syx`.

The boot runs with the process's `DIGI_INTC_FORCE` (default on). `ready.py` boots with `DIGI_INTC_FORCE=0`, as for the
stock ready states.

Measured: a local patched build of stock DN2 1.11 with a changed section 7, the shared core (no AOT blocks), in WSL:

| step | result |
|---|---|
| boot to ready | 854,728,145 ColdFire instructions, 4,718 DSP frames, 830 s |
| 1 s of `idle` after the boot | 307 s, PCM `f1c0c5c0…`, RMS 0 |
| 1 s of `idle` from the saved pair | 300 s, the same PCM |
| 1.5 s of `trig1-taps` from the saved pair | 387 s, RMS 0.0154, peak 0.056, sound from the 0.4 s window to the 1.2 s window |
| `ready.py --base-sha256`, step 4 only | 713 s, 4,744 DSP frames, every reply nonzero. 1 s of `idle` from its pair runs in 235 s |

The `trig1-taps` run fails only its PCM pin, which is for stock DN2 1.12.

The DSP started at about 600 M instructions: the boot ran 414 M instructions in 13 s, and the next 205 M took 352 s.

### Gate against stock

`--span-baseline STOCK.json` gates each scenario's spans against an earlier stock report. A span fails when its `p50` or its
`max` is more than its limit above stock. The limit is 1,000 instructions per frame. `--span-max NAME=N` (repeatable) sets
another limit for one span. The result is `gates.span_gate` in each scenario's report. A failure fails the run (exit 2), and
the console shows one line per span.

The gate also checks late entries. A pass that runs past its period (`overrun`, from `length.over_period`) makes the next
entry late. A late entry makes `gap.max` longer, the longest time from one entry to the next. A span fails when its
`gap.max` is more than its limit above stock's, or when it has more overruns than stock. In the emulator a handler entry
drifts by a few dozen instructions around its period (stock `isr` `gap.max` is 88,013 to 88,056). So the gate does not count
the gaps over the period.

The gate also counts the passes over the period at the unit's speed (`unit_overrun`, from `length.over_unit_period`). A
pass counts when its length times `--unit-scale` is above the period. The default scale is 1.21, from a unit (see
"Calibration against a unit"). A span fails when it has more of these passes than stock. The console line gives them as
`unit N/STOCK`.

A run stops at the first chunk boundary past its length. So two builds can end the same scenario up to one chunk (250,000
instructions) apart. The gate accepts a difference of up to one chunk and one period.

The gate set is twelve scenarios: `idle`, `kb-chord16` and `dn2-rec-play` from `tests/scenarios/dn2.json`, and nine load
scenarios from `tests/scenarios/dn2-load.json`:

| scenario | load | checked on stock 1.11 |
|---|---|---|
| `load-seq16` | a trig on all 16 steps of all 16 tracks, then PLAY for 8 s | 64 frames start a note on all 16 tracks (mask `0xffff`), one every 0.125 s |
| `load-turns` | the SRC page, Trig 1 held, then each encoder A to H 40 detents up and 40 down, one detent every 1M instructions | the screen changes 120 times |
| `load-ptn-switch` | 8 pattern changes while stopped, one every 0.5 s | the kit pointer `0x800052a0` takes 8 values, each written twice, then pattern 1's kit again |
| `load-kb-roll` | keyboard mode, a key down every 1M instructions over the 13 trig keys that play a note, 792 notes | 792 frames each start one note, from 0.457 s to 6.45 s, on all 16 voices in turn |
| `load-midi-flood` | MIDI DIN input at the full wire rate for about 2 s on channel 10: 417 events of 15 bytes (note on, CC 48, note off, CC 49, note on) | 6,255 bytes, each taken by the receive interrupt. A 4 s version started 1,666 notes and 833 note offs |
| `load-lfo3` | each MOD page: SPD and MULT to their maximum, DEST one step and YES, DEPTH to its maximum, then Trig 1 held for 3 s | 15 bytes of track 1's sound change, in three LFO blocks 0x10 apart |
| `load-midi-out` | SETTINGS > MIDI CONFIG > PORT CONFIG, ENCODER DEST to INT+EXT, then encoders A and B 40 detents up and 40 down | ENCODER DEST reads 1. The turns send 84 bytes on MIDI DIN (CC 48 and CC 49 on channel 10), in 28 channel-37 transfers |
| `load-sysex-send` | SETTINGS > SYSEX DUMP > SYSEX SEND, the active project to USB+MIDI, then 4 s | 16,128 bytes of SysEx on MIDI DIN (`f0 00 20 3c …`) in 8 transfers. The dump is longer than the 4 KiB ring, so the sender waits for space |
| `load-sysex-keys` | TRIG KEY DEST to INT+EXT, three trig keys, the SysEx project send, then 12 trig keys one every 0.5 s while it streams | 15 note ons and 15 note offs on MIDI DIN. The 12 during the SysEx are inside the SysEx stream |

In keyboard mode trig keys 1, 4 and 8 start no note. So `kb-chord16`, with all 16 keys held, starts 13 voices.

Stock DN2 1.11 on the gate set (`--cold-base`, `--dsp zero`, instructions per pass):

| scenario | `isr` p50 | `isr` max | `evala` p50 | `evala` max |
|---|---|---|---|---|
| `idle` | 49,553 | 50,075 | 7,841 | 8,369 |
| `kb-chord16` | 49,860 | 54,566 | 7,841 | 8,369 |
| `dn2-rec-play` | 49,553 | 52,567 | 7,841 | 8,369 |
| `load-seq16` | 49,643 | 82,102 | 7,905 | 8,913 |
| `load-turns` | 49,553 | 50,762 | 7,841 | 8,369 |
| `load-ptn-switch` | 49,611 | 50,075 | 7,841 | 8,369 |
| `load-kb-roll` | 50,024 | 51,507 | 7,904 | 8,369 |
| `load-midi-flood` | 49,960 | 51,394 | 7,841 | 8,369 |
| `load-lfo3` | 49,579 | 50,826 | 7,873 | 8,193 |
| `load-midi-out` | 49,547 | 50,826 | 7,841 | 8,369 |
| `load-sysex-send` | 49,553 | 50,075 | 7,841 | 8,369 |
| `load-sysex-keys` | 49,553 | 51,985 | 7,841 | 8,369 |

On `load-seq16` the longest stock `isr` pass is 82,102 instructions, 93 % of the period. On the other scenarios it is 62 % at most.

The stock LFOs add no ColdFire work. On `load-lfo3`, `evala` over the last 2.5 s has a median of 7,905, the same as a
control that only opens the MOD pages and holds Trig 1. With `--dsp zero` the DSP does not run, so the stock LFOs run on the
DSP. Use `load-lfo3` with `--dsp sharc` for the DSP load.

MIDI DIN input keeps the fast paths off while bytes wait. So `load-midi-flood` takes about 57 s of wall time for 3.5 s.

The report of each scenario has `midi`. It gives the bytes and interrupts of the DIN input. It also gives the bytes, transfers
and completion interrupts of the DIN output, with the first 48 output bytes.

`midi.out_events` lists the channel messages of the DIN output (status 0x80 to 0xEF and their data bytes), at most 2,000.
Each has `at`, the instruction count from ready when its status byte is on the wire, within one chunk (250,000
instructions). A message that the sender put inside a SysEx message has `in_sysex`. A receiver ends the SysEx at its status
byte.

On `load-sysex-keys` a note on leaves 3.8 to 5.6 ms after its key before the SysEx. During the SysEx it waits behind the
SysEx bytes in the ring: 82 to 1,169 ms, 626 ms on average. The notes leave in groups about every 1.29 s.

On `load-sysex-send` the longest `gap` of `isr` is 88,891 instructions, against 88,056 on `idle`. So a handler entry starts up
to 835 instructions later than on `idle`. The cause is not traced.

1. Run stock once with the scenarios and spans. Keep its report:

   ```text
   coupled-run --syx Digitone_II_OS1.11.syx --dsp zero --cold-base 2af43e65… \
     --scenarios tests/scenarios/dn2.json --scenarios tests/scenarios/dn2-load.json \
     --scenario idle --scenario kb-chord16 --scenario dn2-rec-play --scenario load-seq16 \
     --scenario load-turns --scenario load-ptn-switch --scenario load-kb-roll \
     --span isr=0x40025e36:0x40027bc6 --span evala=0x400272d4:0x400272da --out stock-111.json
   ```

2. Run the patched build with the same options and `--span-baseline stock-111.json`.

The gate fails, and does not pass in silence, when it cannot compare the two reports:

- the stock report has no scenario or no span of the same name
- the span PCs, the period or the unit scale are different. A stock report made before the unit count has no scale: run
  stock again
- the DSP mode is different
- the two scenario lengths are more than one chunk and one period apart

The gate compares instruction counts, not cycles. A limit in instructions is a first step. The limit can change when a
measurement in cycles is available.

#### Calibration against a unit

A DN2 unit with stock 1.11 and the USB probe mod timed its audio handler with DTCN0. DTCN0 ran at
132.00 MHz on that unit (finding 07). The unit ran a new project, read for 10 s or more in each state. The emulator ran the
same states on stock 1.11 (`--cold-base`, `--dsp zero`):

| state | unit, % of a frame | emulator, % of a period | unit ticks per instruction |
|---|---|---|---|
| stopped, average | 56.7 | 56.3 (`idle`) | 1.007 |
| playing, average | 54.6 | 56.6 (`load-seq16`) | 0.96 |
| playing, frames with no step, worst per 100 ms | 61 to 63 | 56.6 | about 1.1 |
| playing, step frames | 91 to 100, about 95 typical | 78 to 80 (68,000 to 70,000 instructions) | about 1.21 |
| the first step after PLAY | 115, 1 frame over | 93 (82,102 instructions) | 1.21 gives 113 |

- On average frames an instruction is one tick, to within 1 %. So emulated time is the unit's time on average.
- A heavy frame costs the unit more. The probe times the handler from entry to exit, so its peak includes the interrupts
  that preempt the handler. The core also has 8 KB caches only, and the handler's code alone is 7,568 bytes. Which of the
  two adds the time is not measured.
- At 1.21, a pass of more than about 72,700 instructions runs past the frame on a unit. Stock's step frames take 68,000 to
  70,000, about 5 % below that.
- Stock has 1 such pass on `load-seq16`, the first step after PLAY. The unit had 1 frame over at PLAY.
- A local patched build has 0 passes over the period on `load-seq16` in the emulator, and 33 at the unit's speed.

The scale is from one unit and one pattern. `--unit-scale X` sets another.

So `--late-send-window` cannot take the unit's window as it is. In the emulator a send is late only when a pass is longer
than the full period. On the unit the same pass is late at about 72,700 instructions.

### Late frame sends

`--late-send-window LO,HI` times each DSPI2 exchange, when a frame goes to the DSP side. It measures the gap from the
previous exchange. A gap of LO to HI ms flags the frame. The ms are emulated time at `DIGI_CF_IPS`: one period of 88,000
instructions is 0.667 ms. The report gives `late_send`: the number of frames and of flagged frames. It also lists the first
1,000 flagged frames, each with its instruction count and gap.

The count is at the transfer, not at a PC. So a build that moves the send call, or holds a frame in a guard, cannot hide a
transfer from it.

`--drop-late-sends` also drops each flagged frame. The DSP side gets the previous frame again, and the ColdFire gets the
DSP's reply to it. This is a model. How a unit loses a frame is not known (finding 04).

Stock DN2 1.11, `--cold-base`, `--dsp zero`:

| run | window | frames | flagged |
|---|---|---|---|
| `idle`, 2 s | 0.66 to 0.67 ms (around one period) | 3,002 | 3,001, each 88,000 instructions after the previous send |
| `load-seq16` | 0.70 to 0.80 ms | 19,684 | 0 |

A local patched build that runs one handler pass past its period on `load-seq16` has 1 flagged frame in 0.70 to 0.80 ms. Its
gap is 95,080 instructions (0.720 ms). On a unit, frames sent 0.739 to 0.772 ms after the previous one were lost. Emulated
time is not the unit's time, so a window from the unit needs a calibration first.

#### Drop a chosen frame

On stock every gap is one period, so a window cannot pick one frame. `--drop-sends N[,N...]` drops the chosen exchanges by
number, whatever their gap. Exchange 1 is the first of the run. It needs no window. The report's `late_send.dropped_at`
gives each dropped exchange with its instruction count.

To find a frame, log the frames with `DIGI_DSPI_LOG` and read them
with `tools/emulator/dspi_log.py`. Log frame N-1 is exchange N.

Stock DN2 1.12, `trig1-tap-release` from the ready snapshot, `--dsp sharc`, `--seconds 3`:

- Frame 682 carries the note-on mask (byte 35) and frame 1137 the note-off mask (byte 37). The masks are one byte later
  than in the 1.11 frame of finding 04.
- **Note-off dropped** (`--drop-sends 1138`). The DSP gets frame 1136 again, so it never gets the note-off. Its replies
  differ from frame 1140 to the end. The voice keeps sounding: the 100 ms RMS stays at 0.029 from 0.8 s to the end of the
  run. Without the drop it falls to 0.000 by 1.1 s. This is the stuck note of finding 04.
- **Control: a frame with no note event dropped** (`--drop-sends 2001`). That frame is the same as the frame before it.
  The PCM is the same as without the drop (`e029639a…`).

So a test can check that a build survives a lost note-off. It drops the frame with the note-off, and checks that no
voice sounds at the end.

## Sink channels of the runtime (a sink owner)

The runtime has two numbered sink channels, `DK_SINK_LOG` (0) and `DK_SINK_SPAN` (1) (`<dk/core/sink.h>`). A module or service becomes the owner of
a channel by calling `dk_sink_set(channel, sink)` (an import, runtime 0.7.1). The runtime names no owner and no producer kind.

- `log`: the sink gets `void sink(const dk_log_ev *e)` for every event the runtime logs, before any filter. The old `dk_debug_log_sink_set` still works and
  is independent; the debug module may move to this channel later (it is not changed now).
- `span`: the sink gets `void sink(const dk_span *s)`, a record `{owner, cycles}`. The runtime emits it from its ColdFire call sites (module ticks, service phases, polls), and the debug module owns the channel
  (clock: the stock DTIM0 counter). The host reads the debug module's span table with the debug commands PROFILE_STATE (0x1e) and PROFILE_READ (0x1f), next to
  LOG, PEEK and LOGCFG.
- A sink runs in the producer's task with interrupts masked: short, no waiting, no logging, no imports.
- One owner per channel. `dk_sink_set` returns 0, `DK_E_BUSY` (another module owns it, nothing changes), `DK_E_RANGE` (no such channel) or `DK_E_CONTEXT`
  (called outside module code). Setting 0 clears the channel (only its owner may).
- The runtime clears a channel after its owner's `fini` (and after a failed `init`), so an unload leaves no pointer into freed code. A module that
  sets a sink in `init` may still clear it in `fini` itself.
- Cost with no sink: a producer tests one word per channel and calls nothing.

## Cost when off

- Interpreter loop: `cov`, `hist`, `wwatch` and `pcdump` are tested through one
  flag (`Engine::diag_on`) and handled in a cold function. `wwatch` is also
  tested once per dispatched block.
- Fast tier call path: the `dump` hooks exist only with the `fast-dump` cargo
  feature (without it `dump::enabled()` is constant false). Environment
  lookups on that path are read once (`OnceLock`). The other env lookups in `fast/` run at build or
  decline-reporting time only.
- Boot: `sharc_peer` reads its diagnostics once at start.
- Instruction spans: with no span the runtime tests one `is_empty` at each PC it already observes. A span adds its two PCs to
  the observed set.
- Write-watch and flush check: the store path of `LoggingBus` tests one `bool` (`WriteGuards::armed`), the
  same single predictable branch the write guards already had (they tested `protect.is_empty()`); the work is
  in a cold out-of-line function. The decode-cache-miss fetch path tests the same flag. `Bus::cpushl` and
  `Bus::cacr_written` are called from CPUSHL and MOVEC-to-CACR only; their default is empty. The environment
  is read once, when an `Emulator` is built.

## MIDI input and output, panel scripts, safety runs (ColdFire, native)

Finding 25. `regions_probe` (`packages/emulator/boot/examples/regions_probe.rs`, built by
`tools/coldfire/regions.py probe_binary`) takes, besides the options of `regions.py confirm`:

| option | effect |
|---|---|
| `--midi T:HEX`, `--midi-file T:PATH` | queue bytes on MIDI DIN T instructions after ready, at `--midi-period N` instructions per byte (default 42240, the wire rate) |
| `--usb T:HEX`, `--usb-file T:PATH` | hand bytes to the firmware's USB MIDI receive callback (MidiRpc requests) |
| `--midi-out PATH`, `--usb-out PATH` | write the bytes the firmware sent on DIN / to the USB transmit function (also in the `midi` and `usb` JSON lines) |
| `--frame T:PATH` | write the raw 1024-byte OLED frame at T |
| `--overlay-out PATH` | write the card writes (`card.overlay` format) |
| `--pc ADDR`, `--peek ADDR` | count executions of a PC; print a longword at the end |

`uv run python -m tools.coldfire.arena_safety run IMAGE SCRIPT` runs one panel script (`tools/coldfire/panel_script.py`;
also `boot`, `nav`, `play`) with `DIGI_WRITE_WATCH` on `cf.expansion` from the first CACR write, the flush check, a canary
pattern in the region and the card overlay check, and judges it; `suite IMAGE` runs them all (a few minutes with
`--jobs 6`); `card-check OVERLAY.bin [--device dt2|dn2]` tests a saved overlay for sectors in the device's module store area (`devices/*.toml [card.store]`);
`list` names the scripts. Each run leaves `facts.json`, the frames (PNG) and `overlay.bin` in its directory
(default `out/runs/arena/IMAGE-SCRIPT`). `contact_sheet()` joins frames into one picture for reading a script's path.
Frame SHA-256 goldens: `tests/golden/arena_safety_frames.json` (`suite --update-goldens` rewrites them after a firmware or
script change; check the frames by eye first).

## Names for addresses

Profiles, crash records and PC dumps name their addresses through the address map ([ADDRMAP.md](ADDRMAP.md)): `tools/devtools/addrnames.py` for tools, `patchkit addr` for everything else. `wb profile report` prints a `cf.profile` recording by function ([Profiles](#profiles)), `tools/devtools/pcdump_names.py` adds a `func` column to `pcdump.csv`, `fastcov` and `snapread` use the same lookup.

## Profiles

A profile is one JSON file, schema `dk.profile/1` (design: [profiling.md](plans/profiling.md) section 9; model: `packages/dkprof/src/profile.rs`). JSON is the artifact; the text of a report is a view of it.

- `run`: id, target (`dt2`, `dn2`), firmware, runtime and module hashes, source revision, binary hash, and the load table of the run (where each module sat).
- `clock`: what the counts measure (`emu.cf.insn`), their unit and how they were sampled.
- `raw`: counters `{cpu, addr, count, series}`. `series` is `pc` or a producer's own (`idle_pc`, `exit_pc`). Raw counters are kept as taken.
- `metrics` and `overhead`: `{name, value, unit}`, or `{name, unit, na_reason}` for what a target cannot provide. N/A is never a zero.
- `markers` (`{name, at}`) and `loss` (what the producer could not keep).

`wb profile report FILE` names the raw counters through address maps and adds up self counts per function and per owner (a module id, `runtime`, `firmware`, `unnamed`) for each series and CPU.
Callers are not credited with their callees. `--map F` (several times) and `--image X.syx` give the maps, `--loads F` a load table (the one in `run.loads` is used too), `--top N` the
length of the function lists (default 30), `--format json` prints the report as JSON. From a synthetic file (the pluck module, loaded at `0x47e10000`, DSP image at `0xb81000`):

```sh
node tools/devtools/wb.mjs profile report out/try/synthetic.profile.json --map out/try/pluck.addrmap.json --top 5
# run synthetic-1 target dn2 clock emu.cf.insn (insn)
# metrics:
#   path.run_fast.instructions         812345 insn
#   host.cpu                           N/A (the device has no host CPU)
# pc cf by function (top 5; 3 rows, total 175):
#              160  91.4%  init
#               15   8.6%  0x40000000
# pc cf by owner:
#              160  91.4%  digikit/pluck
#               15   8.6%  unnamed
# pc sharc by function (top 5; 2 rows, total 400):
#              400 100.0%  _pk_render
# pc sharc by owner:
#              400 100.0%  digikit/pluck
```

`wb profile record --seconds N --out FILE [--image X.syx] [--run-dir DIR]` reads `cf.profile` of the running emulator (the daemon needs the `cf-profile` feature, below) before and after N seconds
and writes the difference as a profile; with `--image` it also reads the load table of the running image. `--since-boot` keeps the cumulative counters instead of the difference (read a paused emulator to get a stable state). A real run, 4 s of a DT2 1.17 image with the runtime (no modules loaded; the dev map adds firmware names):

```sh
cargo build --release -p digi-control --bin digi-emud --features cf-profile   # a daemon with the profile
target/release/digi-emud out/try/rt.syx --free     # from the repository root: its socket is out/native/run/current.json
node tools/devtools/wb.mjs profile record --seconds 4 --image out/try/rt.syx --out out/try/real.profile.json
# out/try/real.profile.json: 371 counters over 4 s
```

`wb profile spans [--start|--stop|--reset] [--out FILE]` reads the runtime span counters of a device (or an emulator with a MIDI port) over the dk-session link: the flag sets the counting first (`--start` resets the counters, then starts), then it reads the spans (modules, the phases `early`, `late`, `last`, and polls) and prints one row each. With `--out` it writes a `dk.profile/1` with `spans` (`owner`, `kind`, `name`, `calls`, `sum_ticks`, `max_ticks`) `span_clock_hz` and `span_elapsed_ticks` (the ticks the counters were gathered over: `sum_ticks / span_elapsed_ticks` is the share of the CPU); `raw` stays empty and `wb profile report` ignores the spans. It takes `--port` and `--bin` like the other device commands. The numbers below are an example:

```sh
node tools/devtools/wb.mjs profile spans --start                    # start counting
node tools/devtools/wb.mjs profile spans --out out/try/spans.profile.json
# running, 132000000 Hz
# name         calls  total ms  mean us  max us
# early        812    41.200    50.7     91.0
# digikit/pluck 812   120.500   148.4    210.0
# out/try/spans.profile.json: 2 spans
```

`cf.profile` keeps the 400 heaviest rows of its cumulative tables, which the artifact records under `loss`.

**Profile pane (desktop workbench).** The Profile tab of the right-hand column (Emulator page and Remote page; `apps/desktop/ui/src/ProfilePanel.tsx`) shows the same span counters live. Start resets the counters and starts the spans (`PROFILE_STATE`); on the emulator it also starts `sharc.profile` and takes a `cf.profile` baseline, so one button records spans and instruction profiles, and on a device it records spans only. While the tab is shown and the target records, the pane reads once per second (`profileAll`, one read at a time). Shares are `sum_ticks / elapsed`: `PROFILE_STATE` reports the elapsed ticks of the span clock (a u64 after the row count; the module adds the 32-bit counter difference at every UI pass and every `PROFILE_STATE`, so one read per wrap period, about 32.5 s at 132 MHz, keeps it exact). The graph keeps the last 60 readings, coloured by the owner type (machine, effect, tool, runtime). A maximum of at least 100 µs and more than 4 times the mean shows red. The DSP section on the emulator lists the modules' instructions per DSP block against the device headroom (`DSP_COST` of CAPACITY), from `sharc.profile` named with the maps of the running image; on a device it shows the whole-block load of the newest HEALTH event. Export writes one `dk.profile/1` (spans, `cf.profile`, `sharc.profile`) with the same builder as `wb profile` (`packages/workbench-core/src/profile/profile-artifact.ts`); Open profile… shows the shares of an exported file as one more column. Phase names come from `[spans]` of `sdk/sdk.toml`. The poll set changes at run time (`dkrt_poll_add`), so the runtime records the owner of each slot (the module record whose code holds the function, or the firmware image) and `PROFILE_STATE` reports it with the function address (`dk_debug_poll`); a row reads `<module> poll` or `runtime poll`, with `: <function>` when the address maps name the function (the Log pane's maps), and `poll N` only when the debug module is too old to say. Without the debug module the pane offers to add it (Transfer upload, one restart). `node tools/devtools/ui_shots.mjs profile-emulator profile-device profile-nodebug` draws the three states over the preview (`preview/profile-backend.mock.ts`; the first two take a minute for the 60 s graph).

**DSP profile (`--cpu sharc`).** A daemon built with `--features sharc-profile` (a DSP core, as for `coupled-audio`) answers `sharc.profile` ([CONTROL.md](CONTROL.md)).
`wb profile record --cpu sharc --seconds N --out FILE [--image X.syx] [--run-dir DIR]` starts the counting, waits N seconds (a live emulator) and writes the counts as a profile
(`clock.unit` `insn`, metrics `sharc.blocks`, `sharc.insn.*`, `sharc.coverage`); `--since-boot` skips the start and the wait and reads the counts since an earlier
`sharc.profile {enable: true}` (a paused emulator). `wb profile report FILE --map SHIP.addrmap.json --map MODULE.addrmap.json --loads LOADS.json` names the DSP functions of a module and, with
`--analyse ID=MODULE.dkm:DSP.dko.json`, adds the counts inside each DO loop of its DSP code (`dksharc`): a counter belongs to the innermost loop that holds it, so a loop's row
does not include the loops inside it. With the `sharc.blocks` metric every row also shows instructions per DSP block. A fast-tier or block-tier dispatch is credited whole to its
entry PC, so a count says which region the instructions belong to, not which instruction in it.
A real run (DN2 1.12 with digikit/pluck, 5487 DSP blocks, one note from the module; the daemon with `sharc-profile`, the report with the ship map and the module's map from `patchkit pack --addrmap`): the whole DSP
takes 332,039 instructions per block (the stock firmware, near the 334,529 of `stock_busy`), the module 247.4 per block; its `_pk_render` loop (32 passes) takes 200.7 per block, its `_pk_coef` 33.5.
The map holds assembler labels as functions (`_pk_run` inside the render routine), so the function rows follow the map and the loop rows follow `dksharc`.
The SHARC-only rig is a second producer of `dk.profile/1` (`tools/devtools/rig_profile.ts`: one conformance case, exact instructions per pc, no emulator; [TESTING.md](TESTING.md), "SHARC-only rig").
`wb addrmap lookup` and `wb module analyse` ([ADDRMAP.md](ADDRMAP.md#usage), [dksharc](../packages/dksharc/README.md#usage)) use the same `dkprof.wasm`.

## Cargo features

| crate | feature | effect |
|---|---|---|
| `packages/emulator/sharc` (`coupled`: same names) | `fast-dump` | `SHARC_FAST_DUMP` capture hooks |
| | `region-profile` | per-entry wall time (`perf.py --regions`; needs `DIGI_COUPLED_FEATURES=coupled-audio,region-profile`) |
| `packages/emulator/boot` | `cf-profile` | host-time and PC profile of ColdFire |
| `packages/emulator/control` | `sharc-profile` | `sharc.profile`: DSP instruction counts per PC (turns on the engine's `region-profile`) |
| | `diagnostic-profile`, `-events` | boot event records and profile |
| `packages/emulator/periph` | `trace` (default) | DT2MMIO trace reader and `mmio-replay`; no runtime hook. Every product crate (`boot`, `card`, `host`, `machine`) takes periph with `default-features = false`, so a product build does not compile it. Turning the default off changes only a direct `cargo build -p periph`. Left as is. |

## Tuning switches (not diagnostics)

Read at start-up; defaults are what `tools/emulator/emulator_config.py` and the shipped
`config/emulator.json` set. Status **[D]**: read from the code, not re-run.

| variable | effect |
|---|---|
| `SHARC_AOT=0`, `SHARC_AOT_DENY` | generated block tier off / blocks denied |
| `SHARC_FAST_REGIONS`, `SHARC_BLOCKTRANS` | fast region entries, block-translator roots (`@FILE` allowed) |
| `SHARC_FAST_FALLBACK_REGIONS` | fast roots eligible only when code verification bars their generated entry; DT2 1.17 defaults to `0x1c2607`; an explicit empty value disables the default (`_2` selects the second core) |
| `SHARC_FAST_AUTO=a,b`, `SHARC_FAST_FOLLOW`, `SHARC_FAST_ONLY` | run-time region selection, extra entries, restrict to listed. `mise run wb` defaults `SHARC_FAST_AUTO` to `4,2,0`; an explicit value wins, and an empty value or `0` turns auto-arming off; Nx tests and `emu:serve` do not get this default |
| `SHARC_FAST_GEN` | generated lowering; default on; `0` = hand lowering only |
| `SHARC_FAST_HELPER` | generated helper calls |
| `SHARC_FAST_NOBACKOFF`, `_NOGUARD` | switch one fast-tier mechanism off (A/B controls) |
| `SHARC_FAST_CL_NOVERIFY`, `SHARC_FAST_BACKEND` | backend checks and plug-in path |
| `DIGI_DSP_PERIOD`, `DIGI_DSP_RX_PHASE` | DSP scheduling in the coupled run |
| `DIGI_EMU_FAST=0` | tools/emulator/digi_env.py: no fast regions |
| `SHARC_FAST_SHAPE=cfg`, `SHARC_FAST_CHAIN=0` | legacy variants **[O]** likely obsolete; check before use |

## Spike log (`DIGI_SPIKE_LOG`)

Per-window timing for real-time dropouts, in the app and in `coupled-run`.
Set `DIGI_SPIKE_LOG=DIR`. The DSP worker writes `DIR/dsp.jsonl` (one record
per 0.1 audio s: busy time, worst period, time spent waiting for frames,
fast-tier build and compile time). The coupled run loop writes `DIR/cf.jsonl`
(one record per ~20 ms of wall time: `step_chunk` time, the gap between calls,
the wait for DSP replies, the audio queue). Setting it turns link timing on.
Read it with `uv run python -m tools.devtools.spikes DIR`: it names the largest cost
of every window that fell behind real time (dsp, compile, cf, ui-gap).
`tools/devtools/app_soak.py` drives the running app over its control socket (Trig 1 taps and page changes) for an unattended run.

## DSP thread hardware counters

`coupled-run` report.json has `cpu.dsp_thread_cycles_per_audio_s` and
`cpu.dsp_thread_insns_per_audio_s` (cycles and retired instructions of the DSP
worker thread; macOS only, 0 elsewhere). `sharc_matrix` summaries give
`cycle_ratios_B_over_A` and `insn_ratios_B_over_A`. Instructions per audio second
repeat within about 0.02% for the same binary and workload, also on a busy host
(A/A control). Cycles spread like CPU time (about 1-10% between runs) because
contention and core type change them. Instruction ratios therefore show the path
length a change removes; they do not show stall changes, so a timing A/B on a
quiet host stays the final check.

## Browser bench (`mise run web:bench`)

Headless Chrome/Chromium throughput of the coupled browser core (DN2, local
inputs): `mise run web:bench -- --seconds 10 --runs 3 [--js-flags "..."]
[--profile]` (exclusive bench lock, own `astro dev` on port 4399;
`tools/devtools/browser_bench.mjs`, page mode `/emulator/?bench=1&seconds=N` in
`apps/web/src/runtime/bench.ts`). Prints JSON: audio s per wall s (whole
run, first and last half, per audio second), worker busy time inside the step
call, DSP and ColdFire instructions per audio s. `--profile` adds a CDP CPU
profile of the worker (DSP / ColdFire / idle split by wasm function name, top
functions). Needs the firmware at the repo root or `DIGI_EMU_SYX`, and
`apps/web/.local/emulator-core-sharc.wasm`. Chrome is used if installed,
else Chromium or Edge (`--chrome PATH`).

The page runs the ColdFire and the DSP in two workers (SharedArrayBuffer link,
dev server sends COOP/COEP) when cross-origin isolated; `--split 0` forces the
single worker. The JSON has `mode` (`single`/`split`) and, in split mode,
`cf_worker_busy_per_audio_s` (step time minus time blocked on the DSP),
`cf_worker_blocked_per_audio_s` and `dsp_worker_busy_per_audio_s`; `pcm_peak`
and `missing_blocks` check that audio is produced. Exactness of the split:
`node tools/devtools/wasm_parity.mjs --scenario NAME --split 1` (two instances in two
threads) must print the same PCM hash as without `--split`.


## Read-only SHARC recorded-store inventory

The existing firmware query surface can report explicit-root CALL reachability
and recorded store operands as deterministic, uncapped JSON:

```sh
uv run python -m tools.coldfire.fw stores 0x1c9fe7 -i dn2-1.12 \
  --expected-image-sha256 336e340aa0cdcd34e314cfa44849f709a3134f6bd4cd57dfc7e15702c83115e2 \
  --image-blob out/sections/dn2-1.12/section_7_BLOB.bin \
  > out/runs/dn2-store-inventory/report.json
```

Supply one or more root PCs; each requested PC and its containing function remain
separate. CALL targets retain actual target PCs and recorded/containing functions,
including interior targets. Reach covers whole containing functions, not proven
execution from that PC. Known unfollowed jumps, indirects and metadata gaps stay
in the report. It uses a fresh in-memory NetworkX graph without graph-cache writes.

SQLite opens read-only and report queries share one read transaction. Missing or
stale schema/provenance fails without rebuilding. The expected hash checks database
metadata; `--image-blob` additionally verifies the supplied bytes. Without it the
report is explicitly metadata-only and does not follow the recorded blob path.
Database-file SHA is main-file identity, not a WAL/transaction snapshot or proof
against concurrent replacement; journal mode and metadata are recorded separately.

Every selected recorded store row remains an operand, including stack candidates,
PM and dual stores. Raw instruction, memory-access and pointer-site evidence is
retained; competing/base/width mismatches stay unknown. Pointer values are limited
constant-propagation/loop-first-arrival estimates, never runtime bounds. Width
stays a raw token: no byte footprint, SIMD or alias mapping is inferred. This is
neither decoder-complete store coverage nor ownership/reservation proof.


## Device probe (USB audio + remote)

`tools/device/devprobe.py` presses keys on a real device through the digikit remote module, records its USB audio with PortAudio (the `sounddevice` package) and measures each hit (pitch, length, level). The capture does not use ffmpeg: its avfoundation input dropped about 13% of the samples from the Digitakt II (a gap every ~70 ms), which made sweeps look too fast. The probe prints the captured seconds, the wall-clock seconds and the overflow count, and prints a warning when the captured length differs from wall time by more than 1% or an overflow happened.

Setup:

- On the device: SETTINGS > SYSTEM > USB CONFIG = USB AUDIO/MIDI; USB OUT = MAIN (or one track); tick `digikit/remote` in DIGIKIT > MODULES and restart.
- On the Mac: nothing to install; `mise run device:probe` fetches `python-rtmidi` and `sounddevice` through `uv`. `--list-audio` shows the input devices; `--audio NAME` picks the first input whose name contains NAME (default Digitakt or Digitone).

```sh
mise run device:probe -- --list-audio
mise run device:probe -- --steps 'trig1*10@500'
```

Results are in `out/runs/devprobe/<time>/`: `capture.wav`, `events.json`, `hits.json` and screenshots. The table shows one line per hit and whether the hits are consistent.

- The remote presses keys only. It does not restart the device.
- On hardware the panel scan may override remote keys (finding 58). The first run shows it.
- `--analyse WAV` analyses any WAV, for example an emulator capture, to compare device and emulator.
