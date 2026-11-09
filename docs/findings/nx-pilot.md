# Nx host cache and named execution pilot (2026-11-01)

**[D]** Bounded local observations on base `04bfd560` plus the worktree-isolation fix, using
Node 24.15.0, pnpm 12.8.2, Nx/devkit 22.7.12 and `@monodon/rust` 3.0.0. The target was
`pnpm nx:pilot run control-client:test-host --outputStyle=static`. This establishes the exercised
host cache behavior, not exhaustive input closure or a performance improvement.

## Cache isolation

**[D]** Nx 22 uses the main worktree for both its shared cache and task database. A worktree's
`nx.json` `cacheDirectory` alone did not isolate these artifacts. The initial successful host run
and cache hit used hash `14423200522536462197` in the main checkout's `.nx` directory, which appeared
as untracked until moved intact into the pilot's ignored destination.
Inspection of installed Nx sources found `sharedCacheDirectory` in
`dist/src/utils/cache-directory.js` and main-root database selection in
`dist/src/utils/db-connection.js`.

The public pilot script now sets absolute paths from the checkout root:

```sh
NX_CACHE_DIRECTORY="$PWD/.nx/cache"
NX_WORKSPACE_DATA_DIRECTORY="$PWD/.nx/workspace-data"
```

The initial shared Nx artifacts were moved intact into the pilot's ignored
`.nx/pilot-validation/shared-cache-before-isolation`. The main checkout was clean after the isolated
runs. These were Nx cache artifacts; this observation does not indicate a change to PGO artifacts.

## Host input probes

**[D]** After isolation, the following sequence completed locally:

| Input state | Task hash | Cache result | Exit | Test evidence |
| --- | --- | --- | --- | --- |
| Original baseline | `4186974415010007784` | cache-miss | 0 | Six tests passed freshly |
| Same input repeat | `4186974415010007784` | local-cache-hit | 0 | Replayed successful result |
| Unrelated README HTML comment | `4186974415010007784` | local-cache-hit | 0 | Replayed successful result |
| Appended standalone `assert.fail` test | `16930213597592617922` | cache-miss | 1 | Original six passed; intentional probe failed |
| Byte-exact original files restored | `4186974415010007784` | local-cache-hit | 0 | Replayed original successful result |

The README and test file were saved before mutation and restored byte-for-byte. Their final Git
diffs were empty. Local records are `.nx/pilot-validation/02-repeat.json`, `03-unrelated.json`,
`04-changed-input.json` and `05-restored.json`; the baseline was observed in command output and
cached terminal output. The JSON records retain hashes, cache status and exit status; they are ignored
local evidence, not committed artifacts.

The first sandboxed host attempt failed all six tests with Unix socket `EPERM`. Running outside that
restriction passed the six original tests. This is evidence of the sandbox restriction, not an
established product flake.

## Named Rust execution

**[D]** On checkpoint `f3906b02`, `pnpm nx:pilot run dkneeds:test --outputStyle=static` executed
once with `cache: false`. The existing benchlock command ran pinned Rust 1.98.1 with
`cargo test -p dkneeds --lib --locked --offline --target-dir .nx/cargo/dkneeds`. Only dkneeds compiled
in the debug test profile; all ten tests passed, none ignored. The isolated target's rustc information
reported 1.98.1 (Homebrew), host `aarch64-apple-darwin`; its only fingerprint was
`dkneeds-bd5fd01e2cc14e75`. No emulator ran and no generated core compiled. `Cargo.lock` was unchanged.

The local `.nx/pilot-validation/06-dkneeds.json` records hash `16223256972177239252`, exit 0 and
`cache-miss`. That status alone does not establish caching was disabled: the resolved target had
`cache: false`, and no `.nx/cache/16223256972177239252` entry was present.

Execution used pinned Node/pnpm on `PATH`, `RUSTUP_AUTO_INSTALL=0`, a pilot-local `UV_CACHE_DIR`,
and the existing Python environment via `UV_PROJECT_ENVIRONMENT`, `UV_NO_SYNC=1`. An absolute
`BENCHLOCK_PATH` selected the shared repository lock because the pilot's `out` was not linked.
These supplied execution settings allowed reuse without dependency synchronization and coordinated
the existing build lock; they do not establish Cargo cache input closure or a performance result.

## Named runtime execution

**[D]** On checkpoint `d6950feb`, `regression:service-commands` executed once with `cache: false`
under the existing shared benchlock. Only `runtime/service-commands.test.ts` ran: the original DT2
OS 1.17 and DN2 OS 1.12 cases both passed, two tests with no failures, errors or skips.
The local `.nx/pilot-validation/07-service-commands.json` records hash `9331773737677411536`, exit 0
and `cache-miss`; no `.nx/cache/9331773737677411536` entry was present. Logs and JUnit evidence are
`.nx/pilot-runtime/logs/service-commands.log` and `.nx/pilot-runtime/junit/service-commands.xml`.

The run used prebuilt stable daemons, copied svc-echo modules and samples, specific firmware symlinks,
and read-only ready-input overrides. `DIGI_REPO_ROOT` selected the pilot; `TMPDIR`, `DIGI_PLUS_DRIVE`,
`DIGI_PLUSDRIVE_OUT` and `REGRESSION_JUNIT_FILE` selected owned local paths. Recorded input SHA-256:

| Input | SHA-256 |
| --- | --- |
| DT2 daemon | `dcc3b4cb73eb4eddb2d5ce440045b7dce48d228b7f32378ce780ec43face4f65` |
| DN2 daemon | `e1d7aae86d3413d7129b1fbb4e54347d8663d8ca8b4cc0e4ce5c7945ae704aae` |
| svc-echo module, both devices | `2f8a742d2447ef92ec2bd0754702974d0d7eb4100b2237b8c1e5502f0940449c` |

Ready `SOURCE.json` labels still named dt2-1.16/dn2-1.11, while their `syx_sha256` values matched the
actual stock 1.17/1.12 inputs and `intc_force` was false; harness validation passed. Ready snapshot
hashes were not independently audited, so this does not establish wholly current provenance metadata.

The existing daemons generated patched firmware in the pilot's `out/runtime-cache` and card images
in owned temporary directories. No daemon/module build target, generated-core compile or plusdrive
CLI ran. No `digi-session` temporary directories remained after exit; pilot Git status was clean and
`Cargo.lock` unchanged. Local `out` contained only `runs` and `runtime-cache`, with no new build-cache
outputs. No output, audio or firmware was staged. This is bounded execution evidence, not a speedup.

## Limits and follow-up

**[D]** Project/target inspection, graph export, targeted configuration formatting and independent
review completed before the probes; `Cargo.lock` remained unchanged. Input coverage was reviewed
statically. Config, lockfile, toolchain and every possible environment input were not each mutated
and tested. The exercised fresh result and cached replay agreed; no timing claim is made.

**[D]** All three pilot targets have completed their bounded execution checks; the host target also
completed its hit/miss probes. No routing migration or gate speedup has been established.

**[O]** Rust and runtime both retain `cache: false`; Rust cache
enablement needs a Cargo configuration/environment audit, and runtime cache enablement needs closure
over ignored and external inputs. The next decision is a first limited replacement of existing host
routing with Nx versus further runtime input modeling, not more repeated runs. Existing verification
routing remains in place. See the
[pilot plan](../plans/nx-pilot.md) for commands, input boundaries and migration conditions.

## Host preflight adoption

**[D]** The subsequent host migration replaces `test:changed`'s full-host/`vitest related` branch
with standard Nx affected selection for all seven registered Vitest projects. Changed paths travel
on stdin, tasks run one at a time, and the six newly configured host targets have caching disabled.
The Rust discovery plugin is unregistered: eleven affected-project probes preserved their selections
with an unavailable Rust toolchain. Shared host config and desktop CSS selected all seven projects;
known dependency, deleted-source, device TOML, module-manifest and distributed-test paths selected
their owners/consumers, while unrelated docs and Python selected no host project. Local records are
`.nx/host-selection-probes.json` and `.nx/host-selection-probes-without-rust.json`.

Focused validation passed all 57 Python routing cases (56 initially, then the corrected budget-mock
test alone) and one new uncached test-support target run, four tests in one file. Static review
reported no issues. No full host sweep or merge gate ran for this migration; input closure and gate
speedup are not established. Lint/format/type selection and merge-gate routing remain unchanged.

## Support and session cache extension

**[D]** On base `8e98b797` plus this extension, support and session joined control-client as cached
host targets; the other four hosts and Rust/runtime targets remain uncached. Support uses standard
`hostTests`, `^hostTests` (its graph dependency on control-client), and the explicit
`config/emulator.json` input. Session retains its project/shared inputs and `sdk/sdk.toml`.
Current tests use local sources, mocks and temporary data; frozen installed dependencies are assumed.
Future helper, environment or external-input changes require another boundary audit.

| Target | Original task hash | Fresh / repeat | Intentional failure hash | Failure result |
| --- | --- | --- | --- | --- |
| `@digi/test-support:test-host` | `4808849853363473802` | Four passed / local-cache-hit, exit 0 | `4851917506349540059` | cache-miss, exit 1 from imported control-client throw |
| `@digi/dk-session:test-host` | `11821132459342275169` | Thirty passed / local-cache-hit, exit 0 | `14052011362760429413` | cache-miss, exit 1; original thirty passed plus one intentional failure |

The support probe appended a top-level throw to control-client's imported index, exercising dependency
invalidation through `^hostTests`. The session probe appended a standalone failing assertion. Both
files were restored byte-exactly, and subsequent runs used their original hashes. Explicit data/SDK
coverage was audited statically rather than separately executed for every input. Local correctness
records are `.nx/cache-extension-validation/01-fresh.json`, `02-repeat.json` and `03-failing-probes.json`.

**[D]** Ceiling: a cache hit can skip target process/test work; Nx overhead, other static checks and
the merge gate remain. A single warm-graph pair for only these two targets ran under one existing
exclusive benchlock against the shared repository lock: `--skipNxCache` took **4.96 s real**, then
normal cached execution took **1.52 s real**, an observed **3.44 s reduction**. Both uncached targets
passed freshly; both cached results were successful local hits with the original hashes. The user
approved `/usr/bin/time` for this host-only comparison because `coupled_ab` is an emulator measurement
tool. This is one local pair, not evidence of a whole-preflight, whole-suite or gate speedup.
Initial unguarded fresh/repeat timings are excluded from performance evidence.

The locked pair's records are `.nx/cache-extension-validation/04-uncached.json` and `05-cached.json`,
with corresponding `.log` and `.time` files. The unused Rust plugin dependency was removed offline
with scripts disabled, deleting 1,171 lockfile lines without version updates; `@nx/devkit` remains.
The frozen scripts-disabled install reconciliation fetched policy metadata, reused 576 packages and
downloaded none. Static review reported no issues. No probe files or generated inputs were committed.

## Desktop and regression cache extension

**[D]** On base `2b6ceffc` plus this extension, desktop-host and regression-host enabled caching
with standard `^hostTests` dependency inputs and their explicit tracked data inputs retained.
Fresh execution passed 99 desktop tests across 13 files and 41 regression tests across five files.

| Target | Original hash: fresh pass, repeat hit, restored hit | Dependency-probe hash: miss, exit 1 |
| --- | --- | --- |
| `@digi/desktop:test-host` | `13066639354666026902` | `9304580470596112005` |
| `regression:test-host` | `11137627996833255939` | `2770010914756785979` |

Throws in imported web `runtime/reload.ts` and support `keys.ts` caused the respective intentional
import failures; byte-exact restoration recovered successful local hits at both original hashes.
Evidence is `.nx/desktop-regression-validation/01-fresh.json`, `02-repeat.json`, `03-failing-probes.json`
and `04-restored.json`, with logs. Targeted config formatting passed; static review reported no issues.
No new timing or whole-suite claim follows. Five host groups now cache results; web-transfer,
empty module-tests, Rust and runtime remain uncached.

**[O]** At this checkpoint, gate explanation selected 19 steps and 57 Python files; setup was pending.
The isolated worktree's native build and ready-state inputs were incomplete for that gate and could be prepared
through the documented lane and owner workflows. Earlier explicit runtime overrides are not full-gate
setup. No merge or gate performance claim is established.

## Integration gate checkpoint

**[D]** The warm lane attempted `out/runs/verify/20261101-043906`. The parent stopped our runtime
process group when the user challenged broad coverage: about 4,035 s had elapsed, including about
2,483 s of runtime execution. This was an aborted, unsuccessful gate, not a full pass or suite-speed
measurement. Build, native, quick-tier, module-load, smoke, stock, card and Python checks passed.

Host formatting and Rust launcher failures were repaired in source. The Rust builder now uses
explicit pinned `rustup run` invocations and removes inherited stable compiler overrides; focused
validation was still pending at this checkpoint. Two runtime sample cases failed with `ENOENT` for the lane's
`apps/web/public/dksample.wasm`; that prerequisite was subsequently built, with no device rerun.
Focused host follow-up initially passed 244 tests with 17 skipped; the previously blocked transfer
file then passed all 14 tests. Formatting and workspace typechecks passed.
The web Node owner check was attempted, but subprocess CLI comparisons hit a sandbox denial at the
default uv source cache because the launch omitted the lane's uv environment. That initial attempt
did not establish a passing web Node suite.

**[D]** At interruption, [PR #237](https://github.com/ElekLabs/digikit/pull/237) remained draft with
incomplete gate coverage. Remaining work was focused host and web checks, validation of the Rust fix,
and a gate-coverage decision. The broad gate was not repeated for observation or timing. Prioritize tighter static
dependency ownership using existing phase evidence; fixed runtime waits are already documented.
No additional cache expansion, custom tracing or whole-suite speed claim follows from this checkpoint.

### Focused verification completed

**[D]** The final Rust fix resolves the pinned compiler with `rustup which --toolchain` and supplies
its absolute path as Cargo's `RUSTC`: `rustup run` alone leaves the gate's real stable compiler first
on inherited `PATH`. One focused test forced compilation under inherited stable `RUSTC`, `RUSTDOC`
and `RUSTUP_TOOLCHAIN` and passed in 19.26 s, without an emulator. Static review reported no issues.
Targeted web Node retries passed with the correct uv environment; the runner reported 21 entries,
including matching cases and empty-file entries, not 21 scenario tests.

The focused verification is complete. The user explicitly authorized a one-time full-gate exception
for PR #237 after these checks passed; merge remains pending. The interrupted gate above remains
unsuccessful, and the runtime sample cases were not rerun. No full-pass or new speed claim is made.
