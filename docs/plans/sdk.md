# SDK design

Date: 2026-11-12. Base: `main` at 25378434. Targets: DT2 OS 1.17, DN2 OS 1.12.

Status: **design agreed with Em on 2026-11-12. Nothing built.**

This plan replaces sections 7.1 (Cleanup) and 7.2 (Contract) of [runtime-and-services.md](runtime-and-services.md), that is the SDK
parts of its items 1 and 5. The order of its steps is in [sdk-runtime-roadmap.md](sdk-runtime-roadmap.md). It links to
[profiling.md](profiling.md) for the DSP rule check and cycle cost.

## 1. Goal

A stable SDK with which users build their own modules (machines, FX, LFOs, filters, tools) from reusable parts of the stock DT2 and DN2
firmware, as device-agnostic as possible.

- Composable: a machine has ordered pages, a page has params. An FX is routable to send or insert and has pages.
- Clean C functions, few macros.
- A clear boundary between runtime and SDK, and between SDK and modules.
- Easy to navigate for users and LLMs: the directory tree says where a thing belongs and where a new thing goes.
- Extensible for new concepts (a mixer, audio buses) and for services a module asks for at run time.
- Raw modules stay possible and migrate to the SDK step by step.
- Tested on the host where possible. A module change never reruns SDK tests.
- Low churn: move files once, into the final structure.

Versions stay experimental. No version bumps are needed while this lands.

## 2. Today

Facts from the 2026-11-12 review of `main`:

- Declarations are macros (`DK_MACHINE`, `DK_PARAM`, `DK_PAGE`, `DK_FX`, `DK_FX_PARAM`, `DK_NATIVE_PAGE_V1/_V2/_V2_FLAGS`,
  `DK_SAMPLE_TRACKS`, `DK_PARAM_LATCHED`, `DK_PARAM_ACTION`, `DK_PARAM_GLYPH`). Each expands to a const record in `.dk.desc`, in a
  separate ColdFire object that the device never sees. `patchkit` lowers the records into the `.dkm` register section and DSP section.
  Callbacks are name strings, because `.dk.desc` refuses relocations.
- Five of six machines and the one FX are only declarations (11 to 14 lines, no C functions). `shot` is the only machine with C code
  (pages, actions, sample view). The tools are plain C over `dkm_init(const dk_api *api)`.
- Two runtime mechanisms exist: the Api table (`dkm_init(api)`, `api->size`, `DK_API_HAS`, `uses_api`) and named imports.
- Duplication: `put32`/`get32` in three tools; number-to-text and text centring in `shot/src/pages.c`; error constants copied by hand
  into each header (-40 three times); modules compile SDK sources by relative path (`../../../sdk/src/picker_logic.c`).
- Naming: `pk_*` beside `dk_*`; `chooser`, `sample_picker`, `picker_logic`; `sample`, `sample_track`, `sample_region`; `_v1` and
  `_V2_FLAGS` in public names.
- Build: m68k gcc with `-Wall -Wextra -Werror`, no `-std=`. Host tests use `cc -std=c11`. No C formatter or linter.
- Nx: `sdk` is one project. An SDK header is a build input of every module. Device tests key on built bytes, so an SDK change reruns
  only the device tests of modules whose `.dkm` changes.

## 3. Two contracts

- **Binary contract**: what the runtime sees. The `.dkm` header and sections, the imports grouped by service, status codes, record
  kinds. Defined in `sdk/sdk.toml`, generated for both sides. It must stay stable and changes rarely.
- **Source API**: the components (page, machine, fx, sample, later lfo and filter). C code compiled into each module. A change never
  breaks a module that is already built, so it can change faster.

Only the binary contract has to be stable. This is what keeps the SDK stable while the components grow.

## 4. Layers and tree

```text
sdk/
  sdk.toml            binary contract: records, imports grouped by service, status codes, kinds
  README.md           map of the layers and where to add new things
  include/dk/
    core/             module.h (dk_module, dk_ctx, lifecycle), status.h (generated), log.h, trace.h,
                      mem.h, bytes.h (put32/get32), text.h (numbers, centring)
    platform/         what the device provides; one header per service import group:
                      ui.h, draw.h, param.h, settings.h, store.h, files.h, file.h, io.h, job.h,
                      samples.h, input.h, usb_bulk.h, hash32.h, debug.h
    page/             page.h, canvas.h, widgets.h
    machine/          machine.h
    fx/               fx.h
    sample/           picker.h, chooser.h, region.h, bank.h, track.h
    gen/              generated only; included by the headers above, not by modules
  src/                same folders as include/
  dsp/                SHARC code by concept: osc/, filter/, env/, dynamics/, sample/, util/,
                      contract/ (today's sdk/asm)
  templates/          machine/, fx/, tool/
  tests/              same folders as include/
```

Include direction: `core` <- `platform` <- components (`page`, `machine`, `fx`, `sample`, later `lfo`, `filter`, `mixer`).
Components may use other components, without cycles.

- `core/` is the module contract: entry point, lifecycle, status, log, small helpers. No device access.
- `platform/` is the device layer: thin, device-agnostic access to what the firmware and runtime provide. It is the only layer that
  declares imports. Components never declare imports; they call `platform/`.
- There is no vendor directory. Almost everything would end up in it; the layer line does that job.
- A new concept is a new component folder, plus `dsp/<concept>/` for SHARC code. If it needs the device, its imports go in
  `platform/<name>.h`, owned by a service. The README states this rule.
- `sdk/vectors/` (fit and settings expected results) moves next to the runtime; module authors do not need it. Readers to update:
  `patchkit/src/fit.rs:211`, `storedump.rs:444`, `runtime/fit/src/tests.rs:11`, `runtime/sdk/src/settings/tests.rs:234`,
  `apps/web/tests/fit.test.mjs`, `tests/digikit/test_dksettings.py`, `tests/regression/host/settings-record.test.ts:23`,
  `tests/patchkit/test_patchkit_dkrt_boot.py`, `tools/nx/cargo.mjs`.

## 5. Module entry and lifecycle

Every module defines one well-known constant, `dk_module`: its single entry point.

```c
const dk_module dk_module = {
    .init = shot_init,   /* int (dk_ctx *ctx): declares content, opens services */
    .fini = shot_fini,   /* void (dk_ctx *ctx) */
    .tick = shot_tick,   /* UI task, about 120 Hz; today's slot 0 */
    .menu = shot_menu,   /* SETTINGS... in DIGIKIT > MODULES; today's dkm_menu */
};
```

- `dk_module` grows by appended fields: event handlers such as "project changed" or "settings changed" (runtime-and-services section 8,
  ordered event tables).
- Services: in `init`, a module calls an accessor, for example `dk_usb_bulk_open(ctx, &usb)`. It returns an error if the service is
  absent, or a handle that can also report resource limits. Underneath these are named imports, so the packer sees what a module needs.
  The service API itself is designed with the services work.
- Settings: keys are declared in `init` (`dk_setting_add`), values are read and written with `dk_settings_get/put` as today.
- Instrumentation: the SDK and runtime own the dispatch into `init`, `tick` and the kind dispatchers. Debug and profiling hooks (`DK_SPAN`,
  log) attach there. Module code does not change.

## 6. Declarations

### 6.1 What is static

Only what the device needs before any module code runs:

| What | Where | Why |
| --- | --- | --- |
| Module id | `module.toml` | Identity |
| One `[[machine]] id` or `[[fx]] id` line per machine or effect | `module.toml` | Machine type numbers are assigned for all selected modules before any loads (`assign::plan`), and stay the same across boots so projects stay valid. Projects refer to these ids |
| `[uses]`: machine rows, param rows, page rows | `module.toml`, written by `module:check` | The tick check in the device menu (`dkrt_scan`, `dkrt_fit`) reads counts and sizes only |
| DSP section (code, relocations, named imports, voice and FX entries) | Built from `src/dsp/` as today | Voice rows, FX slots, DSP memory |

`module:check` runs the module in the emulator and writes `[uses]` from what `init` declared, like a lock file. Nobody maintains it by
hand. A generated header gives C the ids from `module.toml`, so each id is written once.

```toml
[module]
id = "analog"

[[machine]]
id = "analog"

[uses]
machine_rows = 1
param_rows = 4
page_rows = 1
```

The Transfer tab does not check fit: the card holds as many modules as fit on disk. Limits matter only for the modules ticked to load.

### 6.2 Everything else comes from builder calls in `init`

```c
#include <dk/machine/machine.h>

static int analog_init(dk_ctx *ctx)
{
    dk_machine *m = dk_machine_add(ctx, DK_MACHINE_ANALOG, "Analog", "ANLG");
    dk_machine_dsp(m, "va_render", "va_note_on");

    dk_page *p = dk_machine_page(m, "Analog");
    dk_param_add(p, "Tone",  "TONE",  64);
    dk_param_add(p, "Decay", "DECAY", 80);
    dk_param_add(p, "Pitch", "PITCH", 0);
    dk_param_add(p, "Level", "LEVEL", 127);
    return DK_OK;
}

const dk_module dk_module = { .init = analog_init };
```

Optional properties are setters with sensible defaults (range 0 to 127, no glyph, not latched):

```c
dk_param *play = dk_param_add(p, "Play", "PLAY", 0);
dk_param_range(play, 0, 3);
dk_param_glyph(play, DK_GLYPH_PLAY);
dk_param_latched(play);

dk_param *smp = dk_param_add(p, "Sample", "SMP", 0);
dk_param_range(smp, 0, 15);
dk_param_on_click(smp, smp_click);               /* a real function; the compiler checks the type */

dk_page *wave = dk_machine_page(m, "Shot wave");
dk_page_canvas(wave, DK_PAGE_WIDE_CANVAS, range_draw, range_event);

if (dk_device_has(ctx, DK_CAP_PAGE_ROWS))        /* device-aware: DN2 has page rows, DT2 has none */
    dk_machine_page(m, "Extra");
```

An FX:

```c
dk_fx *fx = dk_fx_add(ctx, DK_FX_FSHIFT, "Freq shift", "FSHF");
dk_fx_dsp(fx, "fs_process");
dk_fx_routes(fx, DK_ROUTE_SEND | DK_ROUTE_INSERT, DK_ROUTE_SEND);   /* allowed, default */
dk_page *p = dk_fx_page(fx, "Freq shift");
dk_fx_param_add(p, "Shift", "SHFT", DK_BIPOLAR, -1.0f, 1.0f, 0.0f);
```

A tool or raw module declares nothing:

```c
static int init(dk_ctx *ctx)
{
    dk_usb_bulk *usb;
    if (dk_usb_bulk_open(ctx, &usb) != DK_OK)
        return DK_E_MISSING;
    return DK_OK;
}

const dk_module dk_module = { .init = init, .fini = fini, .tick = tick, .menu = menu };
```

Builder calls never fail one by one. They record what was asked; errors are reported at commit (6.3). On overflow a call returns a
placeholder, so chained calls stay safe.

### 6.3 Commit

The runtime collects the builder calls during `init`. When `init` returns `DK_OK`:

1. Check the collected records with the existing `registry::check` rules, against the device limits and against `[uses]`.
2. Reserve DSP rows (`dsp::reserve`) and bind each machine to its voice row and each effect to its FX row. A voice row's 8 ranges are the
   min and max of the knob params (a clamp guard, today from `typed.rs:755-771`), so they come from the collected params.
3. Apply the records (`registry::apply`). FX pages need their FX row, so step 2 comes first.
4. Start the DSP upload (`dsp::launch`) as today.

If any step fails, nothing is written, the runtime undoes the DSP reservation, and the module is refused with a reason. A refused module
changes no byte of the firmware tables, as today.

Facts behind this (review of 2026-11-12):

- Nothing requires DSP check and reserve before `init`. The only order rule is reserve before FX page apply (`fxpage.rs:309`).
- The CF image holds no DSP addresses; DSP addresses bind through named DSP imports (separate relocation table).
- Type numbers need every selected module's machine keys up front only for the eviction rule (`sticky.rs:173`). Static machine ids keep
  `assign.rs` unchanged.
- The runtime's firmware views are built after the boot hook, so rows written at commit land before the firmware reads them.

### 6.4 Users of declaration content off the device

- Conformance (`pk_conform`) builds its cases from params (slot, min, max, default). It reads the committed rows from the emulated device
  after load, instead of from records. This tests what the device actually has.
- The site gallery takes its card text from `module.toml`. The "Knobs:" line (`ModuleCard.tsx:28-29`) is dropped or moves to
  `module.toml`; decide when the gallery is touched.

### 6.5 Rejected

- Static const records (nested or as statement macros): no runtime change, but Em prefers builder functions for readability and
  composability.
- A builder run by the packer in the ColdFire CPU core at pack time: adds the CPU core to the packer and the browser build.
- Builder calls plus a hand-written copy of the full content as metadata: the same content in two places.

## 7. Boundaries

- **Runtime and SDK**: only `sdk.toml` crosses. Records, imports, status codes and limits are generated for both sides. One generated
  `core/status.h` replaces the error constants copied into each header.
- **SDK and modules**: modules include only public `<dk/...>` headers, never `sdk/src` paths or `gen/`. SDK component code is linked
  into the module automatically; `--gc-sections` (already on) drops what a module does not use. Modules stop listing SDK sources in
  `module.toml`.
- **Firmware**: modules never reach firmware directly. Firmware access goes through `platform/`, implemented by adapters and services.
- `module.toml` holds identity: id, version, name, category, `[targets]`, plus the static lines of 6.1. `type`, `status`, `ports`,
  `conflicts`, `place` and `[[object]]` leave content manifests. The packer infers machine, effect or tool from what the module declares.
  `requires` goes once derived from imports (real today in `remote` and `probe`). Core modules keep `role`.

## 8. Raw modules

A raw module is a `dk_module` with behaviour only (`init`, `fini`, `tick`, `menu`), using `core/` and `platform/` and no components. The
`tool` template makes one. It migrates by adopting components one at a time. The binary contract stays stable, so it keeps working while
the SDK grows. Today `type = "raw"` exists only in test fixtures.

## 9. DSP contract

These come from the profiling work and land after the steps in section 12 that they depend on.

- **DSP rules.** One rules section in `sdk/` for machine and FX entry points: registers a module may clobber, registers and mode bits it
  must restore, memory it may touch. Parts exist today in `sdk/dsp/README.md` "Block routines" and `sdk/dsp/delay.inc` (MODE1.CBUFEN).
  The stock side comes from sharcdb (`regdef`/`reguse` of the interrupt context save at `0x1c02a4`, image and port to confirm, and of the
  adapter dispatchers; `mem_access` for memory stock code owns). No finding records the stock save set yet [O]; record it in
  `docs/findings/` and have a second agent check it against image bytes before [V]. `pk_conform` checks each module against the rules,
  using sharcdb tables built for the module's own DSP section ([profiling.md](profiling.md) 3.3).
- **Cycle cost.** `max_cycles_*` stops being written by hand. The packer writes a DSP cycles-per-block use into the header and the report,
  from the `module:check` emulator measurement at the device's voice count or a static estimate over the module's sharcdb tables, scaled
  by the emulator-to-device calibration ([profiling.md](profiling.md) 6 and 7.3). The broker checks it (runtime-and-services 5.3).
- **`DK_SPAN`.** `DK_SPAN_BEGIN(owner)` and `DK_SPAN_END(owner)` in `core/trace.h` and `dsp/contract/trace.inc`. They read the cycle
  counter and emit `(owner, cycles)` to the `span` sink (runtime-and-services 5.11). With no sink set they cost one flag test. Only
  service dispatchers use them (runtime-and-services 6.1). In the emulator a counter read can change the execution tier
  (`UNSAFE_READ_NAMES` in `tools/sharc_dev/sharc_rsgen.py`), so the reads run only while a sink is set [O: measure the tier effect].

## 10. Style and enforcement

- C11: `-std=c11` for ColdFire builds, as the host tests already use.
- A repository `.clang-format` and a format check target for C. `clang-tidy` later.
- Names: functions `dk_<area>_<verb>`, types `dk_<thing>`, constants `DK_<AREA>_<NAME>`. No version suffixes (`_v1`, `_V2`); versions
  live in `sdk.toml`.
- Macros only where a function cannot do the job: `DK_TRACE_*` and `DK_SPAN_*` (arguments must not be evaluated when off). Error checks
  such as `DK_PARAM_IS_ERROR` become `static inline` functions.
- `layercheck` rules: the include direction between layers; imports declared only in `platform/`; modules include only public headers;
  no relative SDK paths.
- The README says, for each layer, what belongs there. A new SDK feature goes in the layer and folder its concept names.

## 11. Tests and Nx

- SDK tests mirror the include tree (`sdk/tests/page/`, ...) and use the Vitest and C pattern (`tests/pages.test.ts`).
- Components are tested on the host against a small fake `platform`.
- A module change never selects SDK tests. An SDK change reruns only device tests whose module bytes change (true today). Split `sdk` into
  per-folder Nx projects only if its host tests grow slow.
- Python-compiled host tests of other modules move to the Vitest and C pattern.

## 12. Steps

One PR per step. The roadmap names them S1 to S5.

### S1 SDK restructure (byte-identical)

Rule: every built `.dkm` stays byte-identical.

- Hash task: record the sha256 that `patchkit pack` and `dkm-info` print for every module and every fixture, and compare before and
  after. First answer: do two builds of the same source match; does `module:pack-all` include `tests/fixtures/modules` (if not, the task
  must); how to check adapters that include SDK headers (`adapters/machines/src/cf/native-page.c`).
- Move files into the tree of section 4, including `sdk/dsp/` by concept. SHARC include paths come from
  `tools/patchkit/toolchain/pins.toml:17`; check what a change there invalidates.
- `pk_`/`PK_` to `dk_` (about 107 uses). Read `chooser`, `sample_picker` and `picker_logic` before choosing names; they may be one flow.
  Settle `sample`, `sample_track` and `sample_region`.
- Generate status codes into `core/status.h` with the same values. One definition of -40 (`sample.h:21`, `sample_track.h:21`,
  `chooser.h:31`). Generate `DK_STRACK_W_*` (`sdk/dsp/sample_track.inc:14-23`), `DK_SAMPLE_PEAK_BINS` and `DK_SAMPLE_WINDOW_MAX` from
  `sdk.toml`.
- Delete the test-only `dk_sample_region*` wrappers (`sample_region.h:241-256`); move the tests to the draw functions.
- `sdk/project.json`: add `src`, `dsp` and `asm` to the inputs (cache correctness bug).
- `sdk/vectors/` moves next to the runtime (section 4). The `service` template becomes `tool`.
- Update every include directly. No forwarding headers.
- Check: all hashes unchanged, plus `mise run test:changed`.

### S2 Entry point and imports

- `dk_module` and `dk_ctx` (section 5). `DK_MENU`/`dkm_menu` and slot 0 become `.menu` and `.tick`. Slots 1 to 7 are bound but never
  called (`lib.rs:1155-1157`); delete them.
- The Api table becomes named imports grouped by service in `sdk.toml`, with `platform/` accessors. `uses_api`, `api->size` and
  `DK_API_HAS` go. `fw_alloc`, `fw_free` and `fw_block_read` go (no content module, template or SDK source uses them; check core users).
  The runtime stops naming samples and params once the Api thunks are gone (`lib.rs:521-579`).
- Move duplicated helpers into `core/` (`put32`/`get32`, number-to-text, centring). Turn on `-std=c11` and the format check. These can
  change bytes, so they land here, not in S1.
- Migrate the tools (`usb-fast`, `debug`, `remote`, `probe`, `bootscreen`) and `shot`'s init.
- Check: the tools' device tests on both devices; record new hashes.

### S3 Builders and the runtime commit path

- Runtime: collect builder calls during `init`; commit as in 6.3; DSP binding at commit. Bounds: the collected list lives in the module's
  arena share and keeps today's `MAX_RECORDS` (32).
- DSP section: an export table (entry name to address) so `dk_machine_dsp` and `dk_fx_dsp` can name entry points. Sample tracks name
  their DSP symbols the same way.
- Packer: writes the `module.toml` ids and `[uses]` into the header; stops reading `.dk.desc`.
- `dkneeds::needs_of` and the device menu read `[uses]` from the header.
- `module:check` writes `[uses]`. Conformance reads committed rows from the emulated device.
- Migrate the machines, the FX and the fixtures (scripted for the fixtures). Delete the declaration macros, the declaration object and the
  `.dk.desc` path in `patchkit` (`desc.rs`).
- Check: every module's and fixture's device tests on both devices; SHARC golden hashes and PCM pins unchanged; refusal cases for "more
  than `[uses]`", "unknown machine id" and "no free row".
- Size: about 4 to 6 agent days. Risk: medium, because it changes the boot path.

### S4 Templates and docs

- Rewrite the `machine`, `fx` and `tool` templates on the new API, ready for Nx generators.
- Update `sdk/README.md`, `MODULE-AUTHORING.md` and the `digikit-module` skill.

### S5 DSP contract

- DSP rules, cycle cost and `DK_SPAN` (section 9). Depends on profiling P3 sharcdb tables for module DSP sections, P4 per-module cost,
  and runtime-and-services R4 spike (static part).

## 13. Open items

- [O] Where the generated id header is written and how its build phase works. The build is per object today, with no second phase
  (`build/mod.rs:343-405`).
- [O] `init` reuses the staging buffer for `store_read` (`loader.rs:416`) while `dsp::check` keeps pointers into the staged file
  (`dsp.rs:816-824`). Copy what commit needs out of the staging buffer before `init`, or re-stage after.
- [O] Whether two modules may share an FX `id` (saved with the kit, `fxpage.rs:16,190`). No cross-module check was found.
- [O] The gallery "Knobs:" line (6.4).
- [O] `state_bytes` and `stack_bytes` stay in the DSP descriptor block (asm); the packer reads them as today.
