# Address map

Names for code addresses in profiles, crash records and analysis tools. Plan: [profiling.md](plans/profiling.md) section 3.

## Layers

A lookup uses the most specific layer that holds the address and falls back to the raw address.

| Layer | `layer` | Holds | Written by | Ships |
| --- | --- | --- | --- | --- |
| 1 Our code | `our` | The ColdFire code of the runtime, core adapters and apps in a patched image; loaded `.dkm` modules, rebased to their load address; the DSP code of loaded modules, rebased to the start of their DSP image | `patchkit apply --addrmap`, `patchkit pack --addrmap` | yes |
| 2 Anchors | `anchor` | Code anchors of `devices/ports/*.toml` that are at most 4096 bytes long. Region anchors (`cf.main`) are left out: they would hide every firmware name | `patchkit apply --addrmap` | yes |
| 3 Firmware names | `fw` | Functions of the firmware databases (cfdb: ColdFire; sharcdb: SHARC) | `tools/coldfire/addrmap_export.py` | no, developer machines only |

Names of Rust symbols are demangled without the hash (`dkrt::card::gate`). A return address is looked up at the address minus 2: a call to a
function that does not return is often the last instruction of its caller.

SHARC: layer 3 (stock firmware, software PC) and, in layer 1, the DSP code of loaded modules (symbols of the module's DSP object). The patched image has no DSP code of its own in the map.

## Files

JSON, one object. `kind` is `ship`, `module` or `dev`.

```json
{
  "format": "dk-addrmap", "version": 1, "kind": "ship",
  "key": { "device": "dt2", "firmware": "1.17", "image_sha256": "...", "output_sha256": "...", "modules": ["core/runtime@0.6.0"] },
  "entries": [ { "cpu": "cf", "addr": 1076959172, "size": 54, "name": "pk_loader", "layer": "our" } ]
}
```

- `cpu`: `cf` (byte address) or `sharc` (software PC, in words).
- `size`: bytes (words) from `addr`; 0 holds only `addr`.
- `module`: set on the entries of a module map. `addr` is then the offset from the module's load address; the lookup adds the base from the load
  table. A module that is not in the load table has no names. For a `sharc` entry `addr` is the offset in words from the start of the module's DSP
  image and the lookup adds the module's `dsp_base` (below); a module without `dsp_base` has no DSP names.
- `key` of a ship map: the stock image (`image_sha256`), the patched image (`output_sha256`), the modules the patch placed and the `loader` header (below). A module map has
  `module` (the name the runtime reports), `source_sha256` (its object) and `dkm_sha256`. The dev map has the stock image hash from the port file.
  The lookup refuses maps that name different stock images.
- Entries are sorted by (cpu, layer, module, addr, name): equal inputs give equal files.
- Sizes of our code: the `.dko` objects hold no symbol sizes. With one function per section (`-ffunction-sections`) the section size is the
  function size; otherwise the size runs to the next symbol of the section.

Where the files are:

| File | Path |
| --- | --- |
| Ship map | `X.addrmap.json` beside the image `X.syx` (`patchkit patch` writes it with `X.plan.json`) |
| Module map | `M.addrmap.json` beside `M.dkm` (`dist/modules/<device>/` from `nx run <module>:pack`; `patchkit pack --addrmap FILE`) |
| Dev map | `out/addrmap/<device>-<version>.dev.json` (`uv run python -m tools.coldfire.addrmap_export dt2-1.17`); derived from firmware, never committed |

A ship map and a module map hold no firmware bytes and no firmware-derived text beyond the anchor names of the port files (committed).

## Load table

`{"modules": [{"id": "test/dyn-demo", "base": 1196000000, "size": 512, "fini": 1196000064, "dsp_base": 12062720}]}`. `dsp_base` is optional: the software PC
where the module's DSP image starts. A module without a DSP part, or a table written without it, leaves it out. `fini` is optional too.

Hosts do not decode the table. They read raw memory and `dkaddrmap::snapshot` (one Rust decoder, also built for wasm32) turns it into the table:

- A ship map has `key.loader`: `{"modules": slots, "l2_byte_base": ..., "l2_sw_base": ..., "dsp": true}`. `patchkit apply --addrmap` writes it from the device
  file (`[limits] modules`) and the L2 constants of `packages/patchkit/src/dsp.rs`; `dsp` says the image has the run-time DSP loader.
- `patchkit addr --map SHIP --format reads` lists what to read: the plan symbols `DKRT_STATE`, `DKRT_DSP` and `dyndsp_info` with their byte counts.
- `patchkit addr --map SHIP --snapshot SYMBOL=HEX ... [--format loads]` takes the bytes (hex, one flag per symbol) and decodes them; `--format loads` prints
  the table, any other format names addresses with it.

Layout: `DKRT_STATE` (`State` in `modules/core/runtime/src/rs/src/lib.rs`) has ten header words, then one record of 13 words per slot: used, name[32], version,
base, size, fini. `DKRT_DSP` (`DspState` in `.../dsp.rs`) has 13 words, then `mod_off[slots]` and `mod_len[slots]`. The record index of `DKRT_STATE` is the index in
`DKRT_DSP`. A module with `mod_len[i] != 0` starts at the L2 byte address `area + mod_off[i]` (`area` is word 3 of `dyndsp_info`); `dsp_base` is
`l2_sw_base + (that address - l2_byte_base) / 2`. The slot count is read from the length of the `DKRT_STATE` bytes. No firmware or runtime change.

Callers: `tools.devtools.addrnames.AddrNames.for_image(..., peek=)` (Python tools: `dklog`) and `readLoadTable` in `tests/regression/runtime/addrmap.ts`
only peek the symbols and pass the bytes on.

## Lookup

Rust: crate `dkaddrmap` (`packages/dkaddrmap`: format, `Resolver`), the map writers and `patchkit addr` in `packages/patchkit/src/addrmap.rs`.

```sh
patchkit addr --map X.addrmap.json --map M.addrmap.json --map out/addrmap/dt2-1.17.dev.json --load test/dyn-demo=47e10000 [--dsp-base test/dyn-demo=b81000] [--ret] 0x401be1be sharc:0xb80105
patchkit addr ... --stdin          # one address per line
patchkit addr ... --format text    # "0x401be1be name+0x4" per line
```

JSON on stdout: `{"unloaded": [...], "results": [{"cpu", "addr", "name", "text", "layer", "sym_addr", "offset", "size", "module"}]}`. `name` is null
when no layer holds the address. Python tools do not read maps: `tools/devtools/addrnames.py` (`AddrNames`) runs this command once for a batch.

## Usage

Build patchkit once: `pnpm nx:pilot run patchkit:cli` (writes `dist/patchkit/patchkit`). Commands run from the repository root; the outputs below are trimmed.

Write the map of a module while packing it (the DSP symbols of a machine or effect are in it too):

```sh
mkdir -p out/try
dist/patchkit/patchkit --root . pack digikit/pluck -o out/try/pluck.dkm --device dn2 --addrmap out/try/pluck.addrmap.json
```

Name a ColdFire address and a DSP PC of the module. The load table says where the module sits (`--load ID=BASE`, hex); `--dsp-base ID=PC` is the software PC where
its DSP image starts:

```sh
dist/patchkit/patchkit addr --map out/try/pluck.addrmap.json --load digikit/pluck=47e10000 --dsp-base digikit/pluck=b81000 \
  --format text 0x47e10000 sharc:0xb811e7 sharc:0xb811ea
# 0x47e10000 init
# 0xb811e7 _pk_render
# 0xb811ea _pk_render+0x3
```

Without `--dsp-base` the DSP address has no name (`0xb811ea ?`). Without `--format text` the output is JSON (see Lookup).

`wb addrmap lookup` does the same through `dkprof.wasm` (the code the workbench uses; `mise run wasm:dkprof` builds it once, no patchkit binary is needed). It takes the same
maps and load options; the default output is text, `--format json` prints the JSON of Lookup. An address is a number or `cpu:number`; `--image X.syx` adds the ship map `X.addrmap.json`;
`--loads FILE` is a load table, `--snapshot FILE` a JSON object of runtime symbol to hex bytes:

```sh
node tools/devtools/wb.mjs addrmap lookup 0x47e10000 sharc:0xb811e7 sharc:0xb811ea --map out/try/pluck.addrmap.json --load digikit/pluck=47e10000 --dsp-base digikit/pluck=b81000
# 0x47e10000 init
# 0xb811e7 _pk_render
# 0xb811ea _pk_render+0x3
node tools/devtools/wb.mjs --format json addrmap lookup 0x47e10000 --map out/try/pluck.addrmap.json --load digikit/pluck=47e10000
# {"results":[{"addr":1205927936,"cpu":"cf","layer":"our","module":"digikit/pluck","name":"init","offset":0,"size":156,"sym_addr":1205927936,"text":"init"}],"unloaded":[]}
```

Read the load table of a running emulator or device. This needs the stock firmware in `firmware/`. `patchkit apply --addrmap` writes the ship map, which says what to
read; patchkit decodes the bytes:

```sh
dist/patchkit/patchkit --root . apply --firmware firmware/Digitakt_II_OS1.17.syx --set emulator -o out/try/rt.syx --plan out/try/rt.plan.json --addrmap out/try/rt.addrmap.json
dist/patchkit/patchkit addr --map out/try/rt.addrmap.json --format reads
# [{"len":456,"symbol":"DKRT_STATE"},{"len":116,"symbol":"DKRT_DSP"},{"len":16,"symbol":"dyndsp_info"}]
dist/patchkit/patchkit addr --map out/try/rt.addrmap.json --snapshot DKRT_STATE=$(printf '00%.0s' $(seq 456)) --format loads
# {"modules":[]}
```

The second command is an all-zero module table: no module loaded. With real bytes, peek each symbol (its address is in `out/try/rt.plan.json`) for the given length and pass it as
`--snapshot SYMBOL=HEX`; the table lists every module with `base` and `dsp_base`, and the same flags name addresses in the same call. `wb profile record --image X.syx` (emulator) and
`tools/device/dklog.py` (device) do the peeking.

## Workbench

The desktop Log pane names addresses of the **emulator** and of a **device** (`packages/dk-session/src/log-names.ts`, `apps/desktop/ui/src/log-names-backend.ts`, `dkprof-env.ts`); the shared text builders are in
`packages/dk-session/src/runtime-log.ts`. One path serves both origins: the load table is read through the dk-session protocol with PEEK of the debug module
(`DkSession.peek`; the capability PEEK, so `digikit/debug` must be loaded), on the session the follower already uses, and `dkprof.wasm` decodes it. `mem.cf.peek` is not used.
Only the source of the ship map differs. **Hardware proof is a later, explicit step**: the device path runs in the emulator over the same code
(`modules/core/runtime/tests/log-names.device.test.ts`, registered, run in the batched gate), and the host tests use a fake session for both origins.

- Emulator: the controller call `runtime.maps` ([CONTROL.md](CONTROL.md)) returns the ship map of the running image, made from its patch record where `dev.reload` makes its plan, and
  the addresses of `DKRT_STATE`, `DKRT_DSP` and `dyndsp_info`; it follows a module swap.
- Device: `firmware_prepare` keeps the ship map and the same symbol addresses of the image it prepares in `out/addrmap/<device>/<image id>.ship.json` (`library::store_ship`).
  The follower picks the record whose image id equals the id INFO reports. A device whose image was not prepared by this workbench has no names: the Log says why once.
- Modules: `lib_build` writes `M.addrmap.json` beside each `M.dkm` in `out/modules/<device>/`. The Host command `library.readMaps(device)` returns these and the kept ship
  maps; `host.firmware.dkprofWasm()` gives the wasm.
- When the maps are read: before the first entry that names an address (a crash, a fault) and again after the module list changed. Without PEEK, without a map for the image or when the
  maps do not match the running bytes, the pane shows hex and one line `names unavailable: <reason>`.
- Text: a name is appended as ` (module: function+0xNN)` after the `pc` of a crash and the `at` of a stopped DSP, and a stack word that names code is listed as a return
  address in the detail of a fault. An address no map holds stays hex. Records of an earlier boot (crash, fault) mark a module name `unverified`: the module sits at the load
  addresses of the boot that reads the record (above). Whether the DSP's last word of a fault record is a SHARC PC is unverified [O]: it is named only when a module's DSP map holds it.
- The Memory panel lists, under each bar, what each loaded module takes of the resource (`moduleRows`, from CAPACITY and the module files' needs).

## Consumers

| Tool | Use |
| --- | --- |
| `tools/device/dklog.py crash --image X.syx [--module M.dkm]` | pc, registers and return-address-looking stack words of a crash record |
| `wb profile record`, `wb profile report` | `cf.profile` (feature `cf-profile`) or, with `--cpu sharc`, `sharc.profile` (feature `sharc-profile`) of the emulator by function, loop and module ([INSTRUMENTATION.md](INSTRUMENTATION.md#profiles)); `--map out/addrmap/<device>-<version>.dev.json` adds firmware names |
| `tools/devtools/fastcov.py` | SHARC function names of the interpreter report |
| `tools/emulator/snapread.py` | ColdFire names for `regs` and `tasks` (the image comes from the snapshot path or `--image`) |
| `tools/devtools/pcdump_names.py` | a `func` column and names in the `#stack-max` line of a `pcdump.csv` |

Device crash records name modules with the load addresses of the boot that reads the record. A module loaded at another address in the crashed
boot is named wrongly [O]; the record has the module index and name for a check.
