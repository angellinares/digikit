# patchkit: firmware modules for Digitakt II and Digitone II

Status: design agreed 2026-10-13; Phase 0 and Phase 1 done (lane `a`, 2026-10-13); Phase 2 done (lane `a`, 2026-10-13). Branch `work/patchkit`.

## Goal

Patch a stock Elektron `.syx` with composable modules (new machines, parameters, menu items, FX) that target the
ColdFire MAIN OS, the SHARC+ DSP, or both, for every device and firmware the module supports. Two front ends share
one implementation:

- **CLI** (mise tasks): create a module, build it, patch a firmware, inject it into a running emulator, run the
  test matrix. The loop from edit to a running patched emulator should take seconds.
- **Web gallery** (GitHub Pages): the user uploads their own firmware, picks compatible modules, patches in the
  browser, sees the patched firmware boot, and downloads the patched `.syx`. Firmware never leaves the browser and
  the site never hosts firmware.

## State and backlog (2026-10-14, prepared for merge to main)

Read this first; the phase notes below are the history. Handover: `docs/handovers/HANDOVER-2026-10-14-patchkit.md`.
Repository-wide follow-up (structure, TypeScript test runner, collaborator readiness) continues in Em's cleanup plan;
patched-image DSP speed and headroom measurement in the SHARC/ColdFire performance plan.

Done: Phases 0, 1, 2, 3 (DT2 and DN2), 3b (frameworks as modules on generic primitives), 4.1-4.2 (master-gain,
master-drive, sine voice, PLUCK on DT2 and DN2), 5 (gallery), 6 (fw.py, atlas), 7 (PATCHING.md, MODULE-AUTHORING.md,
modules/README.md); permanent machine type numbers; quick proof tier (`mise run patch:proofs`, 387 s); smoke matrix 15
cells; second-agent checks of findings 01/02/04/06/08/17. Nothing flashed.

Backlog for patchkit itself (the rest lives in the plans named above):

- [ ] Gallery: validation surfaced per module (compatible, conflicts, claims, capacity) and a memory and headroom map
      from `plan.json` (container vs the 3 MiB cap, cf.expansion, dsp.l2, boot-stream append) that fills in as modules
      are selected.
- [ ] Per-module budget in manifests (DSP instructions per block, memory), measured in the emulator, shown in the
      gallery.
- [ ] Region guard in the emulator (writes by stock code into allocated module regions fail a run); used by the
      regression suite.
- [ ] Recipes and starter templates: parameter, DSP voice, master-bus effect, ColdFire UI hook (machine exists).
- [ ] Parameter locks on added parameters: test.
- [ ] `fw:onboard` (not built): one pipeline from a new `.syx` to databases, ports and a device-profile skeleton
      (next devices: Syntakt, Analog Rytm MkII).
- [ ] A master-effects framework (several master-bus modules at once).
- [ ] Later: an on-device runtime (module menu, CPU/RAM health) and dynamic module loading from +Drive; granular;
      cross-porting between devices.

## Facts this design rests on

- Container: checksum, HMAC trailer and per-packet checksums are reproduced; `tools.firmware.build.rebuild` repacks and
  `roundtrip.check_authenticity` verifies (finding 01). No patched image has been flashed to a device.
- 1.17 (DT2) and 1.12 (DN2) change only MAIN OS (section 3) and META (section 5). Sections 2 (bootstrap), 4
  (updater), 7 (SHARC blob) and 8 are byte-identical to 1.16 / 1.11. DT2 1.17 MAIN is the same size as 1.16 but
  relinked (2.3 M differing bytes in 9,331 runs), so every ColdFire address moves. Modules must not hold addresses.
- The bootstrap upgrades itself only when the incoming bootstrap version is strictly greater (finding 01). The
  device runs 1.16, whose bootstrap (0x0201) equals the one in 1.17, so installing a patched 1.17 does not rewrite
  the bootstrap.
- Recovery: hold [FUNC] at power-on for the STARTUP menu (bootstrap-owned), [TRIG 4] for OS UPGRADE over **MIDI
  DIN only** (USB MIDI is not available there). The bootstrap checks only the content checksum and HMAC; on
  failure it prints UPGRADE ABORTED and writes nothing [V]. That the menu comes up with a broken MAIN OS is [O].
- The bootstrap's receive and flash-write path has no size limit. NOR flash is 16 MiB; the container is written at
  flash offset 0x80000 and record sectors (tagged fields, serial number) start at 0x380000, so the container must
  stay at or under 0x300000 bytes (finding 01, spike 0.2). DDR is 128 MiB [C] (finding 08, spike 0.3).
- ColdFire space today: `flash_cave` only (DT2 1.16 2,108 B, DN2 1.11 2,136 B). `cave_b` is cleared at reset.
  `tools/cavefind.py` is lost (only its .pyc remains).
- SHARC space: L2 past the loader looks free (about 900 KB DT2, 490 KB DN2), weak evidence. `dsp_patch.py` can only
  overwrite bytes the boot stream already stores.
- Emulator: `firmware.load` (inject) applies a DSP-only change hot in 0.13 s; a MAIN change cold-boots the
  ColdFire in about 4.4 s, cached per MAIN sha, **without DSP audio** (finding 17, [O]). No `fault`/`halt` events
  are emitted and there is no hang watchdog.
- No new DSP code has run and no added machine has made sound. The per-voice hook into the SHARC render path is
  open research (type 7 selector slot collides; render choice is binary).
- Browser: no cold boot from a `.syx`, no in-page extraction, generic core at about 0.17x real time, COI service
  worker not loaded.

## Architecture

### Layers

| Layer | Holds | Example |
|---|---|---|
| Port | Per-firmware facts: named anchors with `expect` hashes, allocatable regions. Generated, keyed by `.syx` sha256. | `devices/ports/dt2-1.17.toml` |
| Framework module | Hooks firmware sites once, exports a registry API, carries per-device differences. | `core.expansion`, `core.machines`, `core.master-fx`, `core.dsp-voice` |
| Content module | Own code plus declarative registry entries. No addresses. | `digikit/master-drive`, `machines/granular` |

A module is compatible with a firmware when its framework dependencies resolve on that firmware's port and its
resources fit. `targets` in the manifest records only test status (`stable`, `experimental`, `untested`).

### Ports

- Live in `devices/ports/<short>-<version>.toml`, next to the device files, referenced by firmware sha256.
- Built by `mise run port:resolve -- FILE.syx` from resolution rules (masked signatures, xref shapes, SHARC
  `reloc_hash`), the same approach as `tools/coldfire/symbols.py` and `tools/sharc_dev/sharc_symbols.py`. Never hand-typed.
- Each anchor stores address, length and the **sha256 of the expected bytes**, not the bytes, so no firmware bytes
  are committed.
- Regions: `flash_cave` (loader only), the ColdFire expansion region, free SHARC L2, each with capacity and
  evidence mark.

### Module layout

```
modules/<category>/<id>/
  module.toml      id, version, category, status, summary, requires/conflicts, resources claimed,
                   registry entries (machines, params, menu items), hooks, per-target test status
  src/cf/*.c|*.s   ColdFire: gcc/binutils m68k-elf, -mcpu=54418, freestanding, -Os, relocatable
  src/dsp/*.s      SHARC+: selache selas; parcel order and relocations handled by patchkit
  tests/*.toml     module scenarios for the harness
  gallery/         card image, long description
  build/*.dko      compiled relocatable objects (committed; a check keeps them current with src/)
```

Source language: C for ColdFire logic, assembly for SHARC kernels (its algebraic syntax reads well), both
commented. Hook stubs and trampolines are generated by patchkit from the manifest, so authors rarely write raw
hook assembly. There is no custom cross-ISA IL: no mature compiler targets both ColdFire EMAC and SHARC+, and
little code is shared between the two CPUs. The readable layer is the manifest plus `patch:explain`.

`.dko`: JSON with sections as hex, symbols, relocations, source hash, toolchain version. Builds are pinned and
reproducible.

### patchkit (Rust, `packages/patchkit`)

One implementation, built as a CLI binary and as `patchkit.wasm` for the gallery. A test asserts the CLI and wasm
produce byte-identical output. It does not depend on the generated SHARC core, so it builds in seconds, and
patching never rebuilds the emulator (patched images boot through `boot_for_base`).

- `.syx` codec: reuse `packages/emulator/loader`; untouched sections keep their original packed bytes verbatim; aPLib packer
  for changed sections; HMAC and per-device packet constant.
- Safety rules, not overridable: refuse any change to sections 2, 4 and 8; cap the packed container size at the
  stock size plus a budget set from the flash map (Phase 0.3); re-decode and verify every output.
- Linker: allocate from port regions first-fit after earlier claims; ColdFire relocations (R_68K_32, PC16, PC32);
  SHARC relocations; symbols from ports, frameworks and the module itself.
- Operations: `write` (with expect), `hook` (trampoline with displaced-instruction relocation, using the native
  ColdFire decoder), `relocate_table`, `repoint_imm`, `dsp_append` (new loader blocks), registry entries.
- Conflicts: refuse overlapping byte ranges after linking; refuse colliding resource claims (machine type ids,
  parameter ids, mirror slots, frame words, DSP selector slots, menu positions); requires/conflicts and framework
  versions. Modules apply in canonical order (dependency topology, then id).
- Patch record: base sha plus module@version list, stored in the image. Patching an already patched image is
  refused; always patch from stock.
- Outputs: patched `.syx`, `plan.json` (every write with provenance, diffable), and `explain` (before/after
  disassembly of each site, allocation map, registry entries).
- `tools/patchkit/machinepatch.py` / `machinebuild.py` become the reference: their XSLICE output and the DN2 master-half DSP
  patch must be reproduced byte-identically before the Python versions are retired.

### Space

- ColdFire expansion segment: append module code and data to MAIN OS; a 34-byte loader in `flash_cave`, called
  from the reset path at 0x4000053e (before the .data copy and RAM clear, before caches are on), copies
  `[len][dest][payload]` from the image end into the region above the DMA arena: DT2 0x47e8b000-0x47fe0000
  (1.33 MiB), DN2 about 24.8 MiB. Canary-checked on all four images (finding 01, spike 0.3). The loader must zero
  module bss itself.
- SHARC: new loader blocks in free L2, verified by canary.
- eMMC/+Drive: not used. The file system starts late in boot, the file can be missing or deleted, and the browser
  flow would need a card file. Revisit only if flash or memory capacity binds.

## Developer loop

| Task | Does |
|---|---|
| `mise run module:new -- category/name` | Module from template |
| `mise run module:build [-- MODULE]` | Compile `src/` to `build/*.dko` |
| `mise run patch -- --firmware X.syx --modules a,b [-o OUT]` | Link, apply, verify; writes `out/patch/` |
| `mise run patch:explain -- ...` | Human-readable listing of a patch |
| `mise run patch:inject [-- OUT.syx] [--hot]` | Load into the running emulator (`emu:ctl inject`) |
| `mise run module:dev -- --modules a,b` | Watch, rebuild, patch, inject |
| `mise run patch:test [-- --matrix]` | Harness over the control socket |
| `mise run patch:port -- X.syx` | Generate or check a port file |
| `mise run fw:onboard -- X.syx` | **Not built (backlog)**: extract, databases, names, port for a new firmware |

Inject modes: DSP-only changes hot (exists); ColdFire changes through a coupled cold boot to ready cached by
(MAIN sha, DSP sha) (Phase 0.6); `--hot` for code-only ColdFire edits that a module marks hot-safe.

Harness (pytest over the control socket):
- Smoke tour for every module set: boot to the readiness contract, track and pattern change, play/stop, machine
  select, save and reload a project, seeded random-input stress.
- Module tests from `tests/*.toml`.
- Assertions: no `diagnostics.fault`, no DSP `halted`, main-loop heartbeat advancing, framebuffer goldens (exact
  hashes), PCM oracles where relevant, DSP headroom from idle-loop share within budget.
- Matrix: device x firmware x module set.

## Web gallery

Upload, identify (device-profiles), list `modules/index.json` (generated at build time from manifests and
`.dko`), compatibility and conflicts from `patchkit.wasm` in a worker, patch and verify, boot in the emulator,
download. Routes: emulator and `/patch`. Dev-only pickers move behind a dev flag. Until the browser DSP is fast
enough, audio is an offline render.

## Phase 0 results (wave 1, 2026-10-13)

- 0.1: DT2 1.17 and DN2 1.12 boot to ready; device profiles load at run time; `tools/coldfire/cfmatch.py` matches 97.5-98%
  of functions in about 8 s (99.8-99.9% precision against Version Tracking). Open: 36 of 135 DT2 FX oracles fail
  on 1.17 (encoder read-backs differ); DN2 1.12 keeps 3 of 4 PCM pins.
- 0.2: the real bootstrap runs on the ColdFire core (`packages/emulator/bootstrap`): [FUNC] is tested before MAIN OS is
  touched, the STARTUP menu comes up over a faulting MAIN OS, the bootstrap's checksum/HMAC accepts rebuilt images
  and rejects corrupt ones, recovery to stock works, and a 1.16/1.17 image does not rewrite the bootstrap [V].
  `tools/patchkit/patched_image.py` builds the harmless stock-size test image. Open: MAIN OS validator (0x400d9e4c) not run.
- 0.3: RAM map, expansion region and loader hook as above; `tools/coldfire/regions.py` replaces cavefind; `tools/firmware/aplibpack.py`
  passes the bootstrap's own depacker on all 16 sections; `rebuild` keeps untouched sections verbatim and has a
  `max_container` guard.

- 0.7 (master hook, lane fastweb): the Q31 converter's single CALL (DT2 `0x1c7734`, DN2 `0x1c9fd2`) retargeted to a
  routine in free L2 scales the final master block; gain 1.0 is bit-identical, gain 0.5 is -6.0206 dB on both devices
  (finding 06). The CFADE word (`+0xde`) is read by the DSP only as a flag with no effect on a Sample-machine track,
  so a hook can use it as a parameter channel ([C]: the earlier "discarded" reading missed a flag). Open: the
  UI-knob leg (needs 0.6), DN2's parameter word, the flag's reader; voice hook only proposed.

## Phase 0 results (wave 2, 2026-10-13)

- 0.4: `Stream.append_block` and an `APPEND` patch op (Python and Rust); the ColdFire boot routine sends the whole
  depacked section 7 (limits: stored <= 0x100000, depacked <= 0xfffff). Usable appended payload: 727,540 B (DT2),
  211,600 B (DN2), limited by the stream, not by L2. Free L2 by canary over the scenario suites: 914,564 B (DT2) and
  466,372 B (DN2), below the cache range 0x200fa000 (finding 100). Open: the real SHARC boot ROM with an extra block (hardware);
  L1 PM room.
- 0.5: pinned binutils 2.47 + GCC 16.2.0 for `-mcpu=54418` and selache 926bfac9 (ElekLabs fork) under out/tools
  (`mise run toolchains`); `tools/patchkit/pk_link.py` (JSON object prototype, R_68K_32/PC32/PC16/PC8); call harness on the
  CF core (`Emulator::call_function`, `tools/patchkit/pk_cfcall.py`); `tools/patchkit/pk_sharc.py` assembles, links and re-decodes
  SHARC code and runs it on the core. selcc: glue only. Open: libgcc helpers for 54418 (float, 64-bit).
- 0.6: `firmware.load` of a patched MAIN cold-boots coupled with DSP audio: DN2 about 6-7 s, DT2 about 13 s
  uncached, under 0.5 s cached. The ColdFire expansion canary also passes with the DSP running.
- 0.7: master hook = retarget the single CALL into the Q31 converter (DT2 `PM 1c7734`, DN2 `PM 1c9fd2`); a
  7-instruction routine in L2 gives exactly -6.02 dB, bit-identical at gain 1.0, on both devices. The +0xde
  (CFADE) word is not discarded [C]: it sets a flag; poking it does not change Sample-machine PCM. Headroom: idle
  share about 50% (DN2) and 68% (DT2). Per-voice hook proposal at 0x1c6b00, about 40%. Open: a UI knob that
  reaches a free frame word; DN2's parameter word.
- Hardware smoke test (0.2): ready (`tools/patchkit/patched_image.py`), waiting for Em's decision.

## Phases

Estimates are agent time. Phase 0 spikes run in parallel lanes (`tools/devtools/lane.sh`); each ends with a go/no-go.

### Phase 0: de-risk

- [x] **0.1 Onboard 1.17 and 1.12** (about 1 day, high). Device profile entries (make `devices/*.toml` loadable at
      run time, compiled-in fallback); emulator boots both to ready; sharcdb reuse (blobs identical); Ghidra import
      with seeds and RTTI; cfdb; in-house ColdFire function matcher (address-masked hash plus call-graph
      propagation) to port 1.16/1.11 names; ready snapshots; AOT cores unchanged (same DSP image).
- [x] **0.2 Bricking and recovery** (about 2 days, medium-high). Run the bootstrap (section 2) in the emulator:
      [FUNC] held reaches the STARTUP menu with a MAIN OS that faults at reset; the bootstrap's own checksum and HMAC
      code accepts a patchkit image (DIN MIDI receive or a direct call of its verifier); confirm a patched 1.17 does
      not trigger a bootstrap upgrade on a 1.16 device. Then Em decides on a hardware test: a harmless patch (one
      string), flashed, then recovery to stock through the STARTUP menu. Prerequisite: a MIDI DIN interface.
- [x] **0.3 Space** (about 2 days, medium). Resolve the DDR map against the reset clear range; choose and canary
      the ColdFire expansion region through long runs; aPLib packer (round trip through our depacker and the
      firmware's); flash layout past the OS container, and the size cap patchkit enforces; rewrite cavefind as a
      port region resolver.
- [x] **0.4 SHARC append** (about 1 day, medium). Append a loader block into free L2; confirm the ColdFire boot
      routine sends the whole section; canary through the scenario suite.
- [x] **0.5 Toolchains** (about 1-2 days, high for ColdFire, medium for SHARC). Assemble or compile, link and run
      one function on each emulated core as a unit test. Every assembled SHARC word is re-decoded by our decoder.
      Pin selache (fetched by a mise task into `out/tools`, not vendored) and binutils/gcc versions.
- [x] **0.6 Coupled cold boot** (about 2 days, medium-high). Patched MAIN with DSP audio, in-process, cached by
      (MAIN sha, DSP sha). Target under 30 s uncached. Also the base for the browser cold boot.
- [x] **0.7 DSP hook point** (3-10 days; master hook about 70%, voice path about 40%). Master-chain hook first
      (`0x1c207b`), then a per-type voice render path. Gate for Phase 4.2 onward.

### Phase 1: patchkit core (about 3 days, high)

Spec (decided 2026-10-13):

- **Split.** Rust `packages/patchkit` (lib + `patchkit` binary + wasm32 cdylib, the packages/emulator/boot pattern; deps:
  `loader`, `coldfire` (std-only), sha2, hmac, serde, serde_json, toml) does everything at patch time: `.syx`
  decode (via `loader`) and encode, aPLib pack, HMAC seal, checksums, 8-in-7 framing with the per-device packet
  constant, port load and verify, link, apply, conflicts, record, plan, explain. Python does everything at
  module-build and port-resolve time (toolchains, cfdb/sharcdb queries): `tools/patchkit/pk_build.py` (src -> `.dko`),
  `tools/patchkit/port_resolve.py` (rules -> port lockfile). `tools/patchkit/patchkit.py` wraps the Rust CLI for mise.
- **Codec.** Untouched sections keep their stored bytes verbatim; changed sections are packed with an aPLib packer
  ported from `tools/firmware/aplibpack.py` that produces the **same bytes** as the Python packer (test on all 16 stock
  sections). Hard limits from the port (`container_max` 0x300000, section 7 stored <= 0x100000 and depacked <=
  0xfffff, MAIN staged image <= 0x3ffc00); sections 2, 4, 8 can never change. Every output is re-decoded and
  checked (checksum, HMAC, packet checksums, limits, record).
- **Port lockfile** `devices/ports/<short>-<version>.toml`:
  - `[port]`: device, version, `syx_sha256`, `main_sha256`, `dsp_sha256`, `resolved_by` (tool + version).
  - `[limits]`: container and section limits above.
  - `[[region]]`: name (`cf.flash_cave`, `cf.expansion`, `dsp.l2`), cpu, start, end, excludes, mark.
  - `[[anchor]]`: name (`cf.reset.init_calls`, `dsp.master.q31_call`, ...), cpu, addr (byte address; SHARC also
    gives the PM word address), len, `sha256` of the bytes, mark, source rule.
  Anchors are declared with resolution rules by the modules and frameworks that need them
  (`[[anchor_rule]]` in module.toml: masked signature, `cfmatch` from a reference port plus offset, or SHARC
  `reloc_hash` function match plus offset). `port:resolve` resolves every declared rule for one firmware and
  rewrites its lockfile. patchkit only reads lockfiles and verifies anchor hashes; a firmware without a lockfile
  has no compatible modules.
- **Objects** `.dko.json`: canonical JSON (sorted keys, no floats) from the `tools/patchkit/pk_link.py` prototype: header
  (arch `cf` | `sharc`, format version, source sha256, toolchain), sections (name, kind text|data|bss, align,
  hex), symbols (native units: bytes, or 16-bit parcels for SHARC), relocs `{section, offset, type, symbol,
  addend}` with explicit addends; types `cf.32`, `cf.pc32`, `cf.pc16`, `cf.pc8`, `sharc.abs24`, `sharc.abs32`.
  The ISA check (our decoders) runs in `pk_build.py`, not at patch time.
- **Module manifest** `module.toml`: `[module]` id, version, name, category, status, summary, `requires`,
  `conflicts`; `[targets]` (`dt2-1.17 = "tested"` ...); `[[object]]` paths; `[[anchor_rule]]`; ops; `[[claim]]`
  resources. v1 ops: `place` (objects into a named region, implied by `[[object]]`), `write` (bytes at an anchor
  plus offset, may carry relocs to module symbols), `retarget` (rewrite the target of a CF `jsr/bsr/jmp` or SHARC
  `CALL/JUMP` at an anchor to a module symbol). Displaced-instruction trampolines wait for Phase 3.
- **Built-in `core.expansion`**: whenever a module places into `cf.expansion`, patchkit appends
  `[len][dest][payload]` to MAIN OS, writes the loader (its own `.dko`, `modules/core/expansion/`) into
  `cf.flash_cave`, and redirects `cf.reset.init_calls`; the loader also zeroes module bss. `dsp.l2` placement uses
  boot-stream `APPEND` blocks.
- **Apply order and conflicts**: dependency topology then id; refuse overlapping bytes after linking, colliding
  claims, unmet requires/conflicts, a region over capacity, or an anchor hash mismatch.
- **Patch record** `DKPR` (format version, base `syx_sha256`, `id@version` list) in `cf.expansion` when the patch
  uses it, else in an appended `dsp.l2` block. patchkit refuses inputs that already carry a record.
- **CLI**: `patchkit inspect X.syx` (device, version, port, record), `check` and `apply --firmware X.syx
  --modules a,b [-o OUT] [--plan plan.json]`, `explain --plan`, `verify OUT.syx`. Modules are found under
  `modules/` by id.

Slice first (one agent), then coverage:

- [x] Slice: codec + port lockfile + `write`/`retarget` + `dsp.l2` placement + record + verify, proven by the
      master-gain module (spike 0.7 routine as SHARC source, one module for both devices) on DT2 1.17 and DN2 1.12:
      -6.02 dB at gain 0.5, bit-identical at 1.0, through `firmware.load`.
- [x] Oracle: DN2 master-half as a one-`write` module: decoded section 7 equals `dsp_patch.py syx`.
- [x] Oracle: XSLICE as a pinned raw-writes module (converted from `machinebuild` plan, dt2-1.16 only): section 3
      sha `cdbd7726...`.
- [x] `core.expansion` and a CF module in `cf.expansion`: boots to ready, function callable (pk_cfcall), region
      content intact.
- [x] Conflicts, canonical order, `plan.json`, `explain`; aPLib byte-equality with Python on all 16 sections.
- [x] wasm build in `tools/emulator/native_wasm.py` (`public/patchkit.wasm`); CLI and wasm outputs byte-identical.

### Phase 1 results (2026-10-13, lane `a`)

Details and numbers: `docs/findings/21-patchkit-core.md`. Built: `packages/patchkit` (lib, `patchkit` binary, wasm
C ABI), `tools/patchkit/pk_build.py`, `tools/patchkit/port_resolve.py`, `tools/patchkit/patchkit.py`, ports for dt2-1.16, dt2-1.17,
dn2-1.11 and dn2-1.12, modules `core/expansion`, `digikit/master-gain`, `test/cf-demo`; the generated (uncommitted, `out/modules/`) oracle modules
`oracle/dn2-master-half` and `oracle/xslice-dt2-1-16`; mise tasks `patch`, `patch:build`, `patch:explain`, `port:resolve`,
`inject`.

- The codec rebuilds all four stock images byte for byte; the Rust aPLib packer equals the Python one on all 16
  packed sections (and is 25 times faster). Stock payloads start at container offset 0x90, not 0x80 as
  `tools/firmware/build.py` writes [C].
- `digikit/master-gain` on DT2 1.17 and DN2 1.12, injected through `firmware.load`: bit-identical at gain 1.0,
  -6.0206 dB at 0.5, from the same ready state.
- Oracles: DN2 master half (all decoded sections equal `dsp_patch.py syx`), XSLICE (section 3 sha256
  `cdbd7726...ecb86a`).
- `core/expansion`: the loader (54 bytes) boots to ready on both devices with DSP audio (DN2 6.4 s, DT2 13.6 s,
  PCM equal to stock); a C module with code, data and bss runs in place.
- Deviations from the spec, all small: `hmac` is a 20-line function over `sha2` instead of a crate; module ids
  are paths (`core/expansion`, `digikit/master-gain`), not dotted; the segment header is `[len][dest][bss]` (the loader
  zeroes bss) with a `0xffffffff` terminator; `anchor` kinds `code`, `data` and `value` (a number read from an
  instruction, for the converter's buffer address); a manifest `ports = [...]` field pins a module to firmware
  (XSLICE); an `--no-record` switch exists for the oracles only; stock containers keep their own first payload
  offset; SHARC `retarget` supports the direct CALL only, and the SHARC listing of `explain` lives in the Python
  front end.

### Phase 4.1 results (2026-10-13, lane `fastweb`)

Details: `docs/findings/22-patchkit-master-drive.md`. `modules/fx/master-drive` patches DT2 1.17 and DN2 1.12 from one
source: a soft-clip saturator at the Q31 converter call and the master page's encoder 7 (the compressor's Sidechain
Filter, fx[66]) relabelled `DRV`, by two strings in `cf.expansion` and the descriptor row's name pointers. Both devices
pass `tools/patchkit/pk_proof_drive.py`: bit-identical to stock at the neutral value, harmonics and level rise with drive, no
clicks while the knob moves, no fault. 20 to 968 DSP instructions per block.

- **Neutral is 0x5000 (UI 80), not 0:** kits do not start from the descriptor default [C]; the module keeps the default.
- New: anchor rule kinds `sharc_mem_addr` and `cf_words` (port_resolve), `(SW)`/`(SWSE)` direct loads in `pk_sharc`
  (Selache ignores the suffix), data-only ColdFire objects.
- Open: hardware; whether R5-R13 are dead at the hook (saved anyway); DT2 1.16 and DN2 1.11 not run; the module's
  emulator test is slow and skips without inputs.

### Phase 2: developer loop and harness (about 3 days, high)

- [x] mise tasks above (`patch:new`, `patch:dev`, `patch:test`); module template; agent skill
      `.agents/skills/digikit-module/`.
- [x] Emulator: emit `fault` and `halt` events; main-loop heartbeat (+ `stall` event); check whether the MCF5441x
      watchdog is modelled (no: and the firmware never configures it, finding 17).
- [x] Harness: smoke tour, goldens, stress, matrix; `patch:dev` watch loop.

### Phase 2 results (2026-10-13, lane `a`)

Details: finding 17 ("Health events, heartbeat and the watchdog") and `tools/patchkit/pk_harness.py` (module docstring).

- Emulator: `fault`, `halt`, `stall`/`stall.end` events, `status.heartbeat`, `mem.cf.peek/poke`, `watchdog.config`
  (`packages/emulator/control/src/health.rs`, 5 unit tests; end to end on DT2 1.17 and DN2 1.12 through `tests/patchkit/smoke`).
  The first real run already caught a DSP halt in stock DT2 1.17 (pattern 3 or 4 plus PLAY, a conditional Type 7a in
  the generated core, finding 17), now a test and a documented limit.
- Harness: `tools/patchkit/pk_harness.py`, `tests/patchkit/smoke/` (tour `tours/smoke.toml`, `matrix.toml`, `goldens.json` with
  SHA-256 only, 9 pytest tests plus 4 slow matrix cells), module tests from `modules/*/*/tests/*.toml`. Tour: boot to
  the readiness contract, track change, pattern change, audition (PCM), play/stop with the sequencer running,
  machine menu open and close, save project and reload, a seeded 40-input stress; every segment starts from `restart`.
  Matrix (device x firmware x module set), one PASS/FAIL line per cell, logs under `out/runs/patchkit/`.
- Matrix result: DT2 1.17 and DN2 1.12, stock and `digikit/master-gain` + `test/cf-demo`: 4 of 4 PASS against the
  recorded goldens, 28 to 35 checks and 28 to 52 s per cell. Frames are deterministic across runs, and the patched
  cells produce the same frame hashes as stock.
- Dev loop: `patch:new` (template builds), `patch:dev`. Edit to running on DT2 1.17: DSP change 2.9 s (cached MAIN),
  ColdFire change 12.6 s (cold boot); edit to quick tests done 4.8 s and 14.5 s.
- Open: ready inputs of 1.16 and 1.11 are not in the matrix (`snapshots/*-ready` built before the INTC-force fix; the
  harness refuses stale ready inputs; DT2 1.16 cells were added 2026-10-13, see "Phase 5 follow-ups"); the `underrun` event was added later (finding 17); the conditional Type 7a halt.

### Phase 3: frameworks

- [x] `core.expansion` (loader, region): done in Phase 1 (finding 21).
- [x] `core.machines` DT2 1.17 (lane `b`, 2026-10-13): generalises machinepatch's nine parts to N types, plus the
      persistence immediate, the list allocation, a seventh clone site, the type argument of the parameter mapping and
      the DSP selector table; XSLICE and a second clone as content modules. See "Phase 3 results".
- [x] `core.params` DT2 1.16 and 1.17 (lane `b`, 2026-10-14): parameter descriptors a content module adds (names, range, default,
      formatter and value class copied from a template row), the table relocated at patch time, every reader repointed
      and bounded. See "Phase 3 results (wave 6)" and finding 02, "core.params".
- [ ] `core.master-fx` (with 0.7 master hook).
- [x] `core.machines` DN2 (lane `fastweb`, 2026-10-13/14): a clone of FM TONE lists, commits, shows FM TONE's pages, sounds
      bit-identical, persists and loads on stock; own parameter pages not built. See "DN2 results" below.
- [x] `core.machines` / `core.params` DN2 own parameter pages (lane `b`, 2026-10-14): a machine with page rows has its own SYN pages
      (names, titles, ranges, defaults), content proof `test/dn2-pages`; see "DN2 pages and PLUCK" below.
- [x] PLUCK on DN2 (lane `b`, 2026-10-14): `digikit/pluck` builds for the Digitone II (DN2 routine on the voice hook, own SYN page).
- [x] Persistence check: project saved on a patched image, loaded on stock (emulator): the track loads as SAMPLE.

### Phase 3 results (2026-10-13, lane `b`)

Details, evidence and the DSP binding contract: `docs/findings/02-machines-and-parameters.md` ("core.machines").
Built: `a1cf1df^:native/patchkit/src/machines.rs` (the generator, removed in a1cf1df; 4 unit tests, `a1cf1df^:native/patchkit/tests/machines.rs` with 6 integration tests on the
real ports and images), `[[machine]]` in the manifest, `modules/core/machines` (24 ColdFire anchor rules of the new kind
`cf_func`, one DSP rule), `tests/fixtures/modules/xslice`, `tests/fixtures/modules/machine-clone`, `c4d86e4e:tools/patchkit/pk_machine_rules.py`,
`tools/patchkit/pk_proof_machines.py`, `tools/emulator/card_overlay.py` and the daemon method `card.overlay`; harness steps `expect_mem`
and `expect_dsp`; two new matrix cells.

- DT2 1.17 and 1.16 (same proof, same results): XSLICE is selectable (type 7), its SRC page shows the CFADE knob, the type reaches the DSP (TX frame word,
  DSP cache), the sound is bit-identical to stock MANUAL SLICE (audition and sequencer), the MOD destination picker lists
  its parameters, a saved project keeps type 7 (control: without the persistence immediate it falls to SAMPLE) and
  loads as SAMPLE on stock firmware without a fault. XSLICE and a second clone together get types 7 and 8 in any
  selection order (same `.syx` bytes) and both sound like MANUAL SLICE.
- Corrections to the old XSLICE: an illegal 8-byte ColdFire instruction in `machinepatch.build_rank_shim` (images from
  `tools/patchkit/machinebuild.py` fault on a real ColdFire), the list builder's fixed seven-entry allocation, a missing
  `type == 6` site in the frame builder.
- Constraint found and **lifted** (lane `a`, 2026-10-13): the emulator fetched ColdFire code only from the MAIN OS image,
  so the framework code lived in the flash cave. The native runtime executes from DDR and SRAM now (finding 07); the shims
  are in `cf.expansion` and the cave holds the 56-byte loader only (was 372 to 452 B).
- DSP binding: extern symbol `machines.<id>.type`; the DSP's selector table is relocated (word per added type, `selector`
  in the manifest picks the render path).
- Not done: DN2, hardware, p-lock and parameter-effect checks of a new machine, types above 8.

**DN2 1.12 assessment (not built).** The dispatch, list, name table, permission check, persistence immediate and the group
helper exist (finding 02, "Digitone II 1.11 has the same machine machinery"): five types, type byte at `+0xde`, name
rows ordered `{hint, name, abbrev}`, no sort comparator and no per-type table, list builder allocation of five. Missing
for a framework port: anchors for the permit bound, the group bound and the clone sites (`type == k` tests of the
machine cloned: to be searched), the DN2 DSP selector table (a different function, same idea) and, the large unknown,
the parameter-page layer (`MachineParameterPageView`, `SrcMachineParamPageCopy`) that gives each synthesis engine its
own pages. Steps: (1) per-device layouts in `machines.rs` and anchors on 1.11 and 1.12 for a clone of FM Tone (1 day);
(2) read how a type picks its page layout and whether a descriptor row is enough (1 to 1.5 days, the risk); (3) DSP selector
and proof tool on the ready inputs (1 day). Estimate 3 to 4 days of agent time, confidence about 50 % for a clone that
shows its own name and sounds like its original, lower for a new parameter page. Payoff is moderate: the framework is
the same, but DN2's machines are FM engines, so new DN2 machines need the DSP voice lane first.

### Phase 4.2 results (2026-10-14, lane `fastweb`)

- Hook: the first instruction of the sample renderer `0x1c4ecf` (DT2 1.16/1.17, one DSP blob) is overwritten with a
  `JUMP` (new op `jump`); a call-site retarget of the voice loop would bar the AOT region of the engine's busiest
  function (4 frames/s). Types register with the new op `register` (a row `{type, pc}` in a table another module
  declares with `[[table]]`); an unregistered type falls through, PCM bit-identical to stock, +1,120 DSP
  instructions per block. Spec and evidence: findings 06, "Per-voice render hook: built". Assembler facts: findings 20.
- `test/sine-voice` (type 8 forced on the wire): 261.626 / 523.252 / 277.184 Hz for note 60 / 72 / tune +1, the
  level, reverb-send and filter stages act on it. `digikit/pluck` (type 9): Karplus-Strong, one string per track,
  fundamental within 0.0003 cents at notes 60 and 72, T60 1.02 s for a requested 1.02 s; analytic tuning error
  under 0.01 cents from note 7 to 95, 0.13 at 104, 0.42 at 108.
- **Contract with `core.machines` (lane b):** a machine module gives `core.dsp-voice` its type number and routine
  with `[[op]] op = "register", table = "_dv_table", row = ["<type>", "<routine symbol>"]`; today the type is a literal in the manifest
  and should become the symbol `machines.<id>.type` that `core.machines` assigns (patchkit resolves a row word that
  is not a number as a global symbol). The ColdFire side must put the type in frame word `0x94 + 2t` (finding 04),
  keep types below 24 (the selector table was read to 23, findings 06), and map the descriptor fields to the
  engine slots listed there; Pluck's table: slot 0 TUNE (25), 1 DECAY (26), 2 PICK (27), 3 unused (28 stays 0),
  4 BRIGHT (31), 5 COLOR (32), 6 unused (33), 7 LEV (34).
- Open: DN2 (a different, FM voice engine; see the assessment in findings 06), hardware, DT2 1.16 not run.

### Phase 3 results (wave 6, 2026-10-14, lane `b`): core.params and PLUCK end to end

Built: `a1cf1df^:native/patchkit/src/params.rs` (3 unit tests, `a1cf1df^:native/patchkit/tests/params.rs` 5 integration tests), `[[param]]` and `fields` that
name parameters in the manifest, `modules/core/params`, three port rule kinds in `tools/patchkit/port_resolve.py` (`cf_params_t1`,
`cf_params_sites`: count and hash of 111 sites, plus a `cf_func` for the second table), `modules/machines/pluck` (machine,
four parameters, the DSP row now registers `machines.pluck.type`), `tools/patchkit/pk_proof_pluck.py`, harness helpers
(`Session.mirror`, `tx_word`, `turn_burst`, `turn_to`), two matrix cells.

- **Knob problem solved:** not a bug. Slow single events change nothing (the encoder accelerates by event spacing);
  bursts of events 2 ms apart move values. Proven on stock SAMPLE: encoders A, E, F, G, H change slots 25, 31, 32, 33, 34
  in the Sound object and in the TX frame words. Details in finding 02, "Corrections to core.machines".
- **core.params:** table relocated (rows copied from the image), 56 references repointed, 54 clamp bounds raised, the
  second per-code table's accessor answers added codes with a template's object. Loops that visit every code are left
  at the stock count on purpose. A correction to core.machines came with it: the type-argument shim now answers an added
  type with its own descriptor fields (defaults, ranges and names of the machine's own rows).
- **PLUCK on DT2 1.17 and 1.16:** selectable, SRC page with its own names, defaults, fundamental within 0.002 cents of the
  note, DECAY/BRIGHT/COLOR/PICK act on the sound, sequencer plays it, project save and reload keep type and values, stock
  loads the project as SAMPLE. Numbers in finding 02, "PLUCK on DT2".
- **Not done:** MIDI CC/NRPN for added types (ignored, no fault), p-lock in the emulator (gesture not harnessed even for
  stock), a row of a different value class than its template, DN2.

### DN2 results (2026-10-14, lane `fastweb`)

Details and evidence: findings 02 ("core.machines on Digitone II 1.12") and 06 ("Per-voice hook on DN2 1.11/1.12: built").
Emulator only; nothing flashed; one agent.

- **core.machines on DN2 1.12** (took about a day, not the assessed 3 to 4): `a1cf1df^:native/patchkit/src/machines/dn2.rs` is the
  Digitone part of the generator (the Digitakt-only parts are skipped on `dn2`; the Digitakt output is byte-identical to
  before for `core/machines` + `test/xslice` + `test/machine-clone` on 1.16 and 1.17); 31 DN2 anchors by function hash
  (`c4d86e4e:tools/patchkit/pk_machine_rules_dn2.py`, 1.11 and 1.12 resolve). The parameter-page layer needs no table growth for a clone: three
  per-type table functions get the type mapped to `clone_of` on entry, and the type reader (41 callers) returns the clone's
  original except for the identity code. `test/dn2-clone` (type 5) passes `tools/patchkit/pk_proof_machines_dn2.py` (select, pages,
  PCM bit-identical to FM TONE, parameters, save/load, load on stock, control, sequencer) and the harness smoke tour.
  Manifest additions: `own_dsp` (the DSP is told the machine's own type), `devices` on `[[machine]]`; a `register` row may
  name `machines.<id>.type`.
- **DN2 voice hook**: the Digitone II engine is a stage pipeline over 16 voice slots with a 32-float 48 kHz buffer per slot;
  the hook is `FUN_b8251b` (called once per buffer after the FM synthesis), so a registered routine's signal passes the
  engine's gain, amplitude envelope and gate, filter, overdrive, sends and master. `test/dn2-sine`: pitch exact
  (261.626 / 523.252 Hz), LEVEL -31.8 dB (stock FM TONE -28.8), envelope release, filter -28 dB, overdrive adds
  harmonics, reverb tail; no registered type: bit-identical to stock. Routine ABI: R4 = buffer, R0 = type, R2 = slot,
  R12 = 32 (finding 06). **No note-on edge or gate is available to a routine** and the machine parameters of a slot's
  track were not mapped (they are per track at frame `0xda + 0x92 t`; the slot to track map is unknown).
- **PLUCK on DN2: not built** (**[C]** built later the same day, see "DN2 pages and PLUCK"). Needs the slot to track map and the parameter words (DECAY, PICK, BRIGHT, COLOR would use
  FM TONE's knobs through the clone's pages, labelled as FM TONE's), a note-on edge (a change of the slot's pitch or
  velocity word works for 16 notes, then fails for a repeated note), `pluck.s` rewritten for a 32-float buffer without
  a voice record, and per-device `requires` in the manifest (core/machines only on the Digitone). Estimate 1 to 2 days,
  confidence about 50 % for a string that retriggers reliably; payoff: the same module on a second device.
- Open: hardware; own parameter pages for a DN2 machine (**[C]** built, see "DN2 pages and PLUCK"); DN2 1.11 not run.

### DN2 research (wave 7, lane `fastweb`)

Findings: 02 ("DN2 parameter pages") and 06 ("DN2 voice slots"). Static plus emulator, one agent, [D]. No framework code
was touched; new: `tools/coldfire/dn2_probe.py`, `tools/coldfire/dn2_pages.py`, `tools/coldfire/cf_wordscan.py`, `c4d86e4e:tools/patchkit/pk_params_rules_dn2.py`,
`tests/fixtures/modules/dn2-edge` (a research machine).

- **Own SYN pages for a DN2 machine** need no new builder hook: constant data plus shims at three existing sites. T1 has
  321 rows and the Digitakt's `cf_params_t1` / `cf_params_sites` rules resolve unchanged (56 refs + 55 bounds, 1.11 and
  1.12); the slot table (5 x 40, `0x42c65d18`) and the page table (`0x42433ad4`, rows `{count, begin}`, pages of 0x2c bytes
  with nine parameter codes) are read by only three functions, which `core/machines` already shims for the clone map
  (`cf.machines.typemap.0..2`). The page and draw callers (about 17 reader call sites) must see the own type; the generic
  eight-knob page is the draw function's default for any type but 0..3. Estimate 1 to 2 days, confidence 70 %.
- **Voice slot to track:** slots are voices (16-voice polyphony, round robin); the map is the ColdFire table
  `0x40288306 + 0x14 v`, and the DSP does not need it: slot v's engine words are the frame block `0xda + 0x92 v` (written at
  the note-on from the owning track, updated while the voice lives). **[C]** per voice, not per track.
- **Note-on edge exists:** frame halfwords `0x11` (and `0x13`) bit v = trig, `0x12` / `0x14` bit v = release, one frame
  long; proven with `test/dn2-edge` (24 of 24 and 40 of 40 notes, also past 16 slots, also on track 6). The earlier "no
  edge available" is **[C]**.
- **For PLUCK on DN2:** `_dv_frame_hw` reads only halfwords below 512, so slots 0..5 can reach their knob words: widen it
  (mask `0xffe`); then PLUCK needs the edge (above), the pitch and velocity words (as the sine), 4 knob words through
  the clone's pages (own pages later), `pluck.s` for a 32-float buffer with per-slot state, `devices` filters, and the
  engine's amplitude envelope as the string's gate. Estimate 1 to 2 days, confidence 60 %.

### DN2 pages and PLUCK (2026-10-14, lane `b`)

Details and evidence: findings 02 ("DN2 own parameter pages: built", "PLUCK on Digitone II") and 06 ("DN2 voice routine: PLUCK").
Emulator only; nothing flashed; one agent; no change in `packages/patchkit` (no primitive was missing).

- **Own pages**: `core/machines` gets a registry `pages` (a row is a page descriptor plus its machine and the sound slots of
  its knobs) and the DN2 hooks `typemap.0/1/2` answer page count, descriptor and slot to code from it; `core/params` now has
  the Digitone II side (the same table copy, 56 references and 55 bounds, T2 shim). A machine with page rows is its own type for
  every caller of the type reader (the research's list of about 17 sites was incomplete: the defaults on selection call the reader
  too). `test/dn2-pages` + `tools/patchkit/pk_proof_pages_dn2.py` (quick about 1 min, full about 6 min): own names and titles on screen, the
  knobs move exactly the mapped slots (A..D on 25 26 27 30, page 2 on 40 41), ranges clamp, defaults are its own, save and reload keep
  type and values, stock loads the project, stock machines' pages equal the stock firmware's. The clone (`test/dn2-clone`) is unchanged.
- **PLUCK on DN2**: `pluck_dn2.s` (per-slot state, note-on = rising edge of frame halfword 0x11 bit v, one zero block then the
  excitation, slot words at `0x6d + 0x49 v + (s - 25)`), own page DECAY PICK BRIGHT COLOR (slots 25..28). The `dsp-voice` DN2 frame
  helpers already had the 16-slot mask (0xffe). `tools/patchkit/pk_proof_pluck_dn2.py` (quick about 6 min, full about 15 min): pitch within
  0.005 cents on six notes, T60 0.23 / 0.96 / 2.16 s at DECAY 0x2000 / 0x4000 / 0x5000, BRIGHT +25 dB HF/LF, COLOR -22.6 dB, the
  excitation (PICK comb, COLOR, amplitude) bit-identical to `tools/patchkit/pluck_ref.py`, 20 and 40 repeated notes of one key give 20 and
  40 plucks (past 16 slots), the sequencer plays both passes, save and reload keep type and the four values, stock loads the project.
  Matrix cells `dn2-1.12` PLUCK set and PAGES set added (goldens recorded).
- **Not done:** MIDI CC/NRPN and p-lock for these machines (not tried); hardware; DN2 1.11 not run (rules resolve there).

### Phase 3b: frameworks as modules (refactor, decided 2026-10-14)

Problem: `core/machines` and `core/params` were built as Rust generators inside patchkit (`machines.rs`,
`machines/dn2.rs`, `params.rs`: about 2,070 of 6,520 lines). They emit ColdFire words by hand, copy firmware
tables and unroll code per added type. That breaks the layer rule ("patchkit is generic; frameworks are modules")
and means every framework change is a Rust and wasm rebuild. Fix: give patchkit the generic primitives the
generators needed, then rewrite the frameworks as ordinary modules (manifest + assembly). The Rust generators stay
as the reference until every proof passes on the module version, then they are deleted.

Primitives (patchkit, Rust, wasm parity; every one usable by any module):

- **P1 Image-copy sections.** `[[section]] name, region, from = { anchor, offset = 0, len = EXPR }, append = {
  registry, row = [EXPR, ...] }` (append optional). At apply time the section's bytes are copied from the user's
  image, then one row per registry entry is appended (each EXPR emits a 32-bit word unless typed `u8`/`u16`). The
  section name is a symbol. Firmware bytes are never stored in the repo.
- **P2 Registries.** A framework declares `[[registry]] name, first = EXPR, max, region` and
  `[[registry.field]] name, kind = u8|u16|u32|cstr|sym, max_len?, default?`. Content modules add
  `[[entry]] registry = "machines", id = "pluck", <field> = value`. patchkit validates fields, assigns indices in
  canonical module order, emits a row table (cstr fields become pointers to strings placed in the same region) and
  the symbols `<reg>.first`, `<reg>.count`, `<reg>.rows`, `<reg>.row_size`, `<reg>.<id>.index` (= first + k), plus
  an automatic claim `<reg>.<id>`. This replaces `[[machine]]`, `[[param]]`, `[[table]]` and `op = "register"`.
  Fields of kind `sym` hold another module's symbol or another registry's `<reg>.<id>.index` (e.g. a machine's
  descriptor fields name `params.pluck_decay.index`).
- **P3 Expressions.** Wherever a number or symbol is accepted (write bytes, relocs, section lengths, registry
  `first`, row words): integers, symbols, `+ - * / & | << >>`, parentheses, and `image.u8|u16|u32(anchor [+ off])`
  (a value read from the image at apply time, e.g. a stock bound). `write` gains typed fields:
  `{ op = "write", anchor, offset, value = EXPR, width = 1|2|4 }` with range checks.
- **P4 Hooks.** `{ op = "hook", anchor, offset = 0, to = SYMBOL, displaced = "run"|"drop", cpu = "cf"|"sharc" }`.
  ColdFire: the site gets `jmp.l to` (or `jsr.l`, `kind = "call"`), NOP-padded to whole instructions using the
  native decoder; with `displaced = "run"` patchkit emits `hook.<anchor>.tail` = the displaced instructions copied
  from the image (PC-relative forms re-encoded or refused) + `jmp.l <site end>`, and `hook.<anchor>.return` = site
  end. SHARC: same with a 48-bit JUMP (generalises the `jump` op of core/dsp-voice). Calling convention is the
  module's business (documented per framework).
- **P5 Device filters.** `devices = [...]` on `[[object]]`, `[[section]]`, `[[op]]`, `[[entry]]`,
  `[[anchor_rule]]`, `[[registry]]`, so one framework holds DT2 and DN2 sources side by side
  (`src/cf/dt2/*.s`, `src/cf/dn2/*.s`).
- **P6 Sites as anchors.** Run-time scans in patchkit (e.g. `params::scan` for bound immediates) become anchor
  rules resolved by `port_resolve` into the lockfile (`cf.params.bound.0..N`); patchkit only reads lockfiles.

Frameworks after the refactor: `modules/core/machines/` (DT2 and DN2 asm: list/rank/group/dispatch/name/permit/
pertype/clone/typearg/persist hooks that look up `machines.rows` at run time for `type >= machines.first`, plus a
P1 copy of the DSP selector table with appended words), `modules/core/params/` (P1 copies of the two parameter
tables with appended rows, bound writes via P3, reader hooks), `modules/core/dsp-voice/` (registry `voices`).
Content modules use `[[entry]]` only.

- [x] 3b.1 Primitives P1-P6 in patchkit with unit tests and wasm parity (lane a, 2026-10-14; results below). Old
      generator path keeps working until 3b.3.
- [x] 3b.2 Frameworks rewritten as modules against this spec; routines unit-tested on the ColdFire call harness with
      hand-built tables (lane b), then content modules migrated to `[[entry]]`. Done 2026-10-14 (lane `b`); results below.
- [x] 3b.3 Integration: every proof passes on the module version (pk_proof_machines DT2 1.16/1.17, pk_proof_pluck
      DT2 1.16/1.17, DN2 machine and voice proofs, smoke matrix, wasm = CLI); then delete `machines.rs`,
      `machines/dn2.rs`, `params.rs` and the old manifest keys. Done 2026-10-14 (lane b `a1cf1df`, merged): all seven
      proofs PASS, the smoke matrix passes 13 of 13 cells (DT2 1.17 and 1.16, DN2 1.12), verify passes. patchkit
      has no framework-specific code left (6,257 lines, of which the primitives are `expr.rs` and `prim.rs`).

### Phase 3b.2 results (2026-10-14, lane `b`)

Written: `modules/core/machines/` (`src/cf/dt2/machines.s`, `src/cf/dn2/machines.s`, manifest with registry `machines`, six
`[[section]]` copies of stock tables, hook and bound-write ops), `modules/core/params/` (`src/cf/dt2/params.s`, registry
`params`, `pf_table` section, glob writes for the 56 references and 54 bounds), `modules/core/dsp-voice/` (registry
`voices`, hook op, `_dv_table = voices.rows`); content modules (xslice, machine-clone, pluck, dn2-clone, sine-voice,
dn2-sine) on `[[entry]]`. New anchors in `tools/patchkit/port_resolve.py`: rule kind `cf_ptr` (a table behind an address literal),
`cf_params_refs` and `cf_params_bounds` (many anchors per rule, `<name>.N`); ports re-resolved. Run-time tables (displayed
list, rank pairs, std::string descriptors) are built once in bss by `mf_ensure`; the clone tests hook the compare and set
the Z flag (hooks cannot restore a register after the stock branch); the Digitone II type maps change the stack
argument in place. Tests: `tools/patchkit/pk_cfunit.py` (hook routine on the native core with registers, flags and stack set by the
case; stubs for tails and exits), `tests/patchkit/test_pk_machines_fw.py`, `_dn2_fw.py`, `test_pk_params_fw.py` (19 tests; the
manifest's row expressions are checked against the Rust row builders with `tools/patchkit/pk_expr.py`). Waits for lane a: linking,
apply, every proof (3b.3).

### Stable machine type numbers and CLI requires (2026-10-14, lane `b`)

Problem: `core/machines` numbered machines `first + k` in module order, and a project stores only the number (findings 02,
"Persistence"): the same project could load as another machine after the module set changed. Built:

- **Permanent numbers.** `modules/core/machines/types.toml` per device (`"<module id>:<entry id>" = n`, `last`). Generic
  registry keys in patchkit: `index_table = "types.toml"` and `[registry.gap]`; the registry then spans `first ..= highest
  selected number`, numbers nobody selected are gap rows (clone of type 0, `position = 0xffffffff`, hidden by the list builder
  and the rank pairs; a stored type that names one plays as type 0). New symbols `<reg>.entries`; plan.json lists `gaps`.
  Refused: an entry with no number, a duplicate number (when the table loads), a number below `first` or above `last`.
  DT2 `last` = 23 (the DSP selector table was read to 23); DN2 `last` = 23 chosen, no firmware cap found. Allocated: DT2 xslice 7,
  xclone 8, pluck 9; DN2 fmclone 5, pages 6, dn2sine 7, dn2edge 8, pluck 9 (PLUCK alone on the Digitakt is now type 9, rows 7
  and 8 are gaps). `mise run patch:new -- machines/NAME --machine [--devices dt2,dn2]` appends the next free number and says so.
  Rule and table in modules/README.md; the [C] and the evidence in findings 02, "Stable machine type numbers".
- **CLI requires.** `patchkit apply|check|explain` (and so `mise run patch`) add the requirements of the selection and print
  `added by requires: ...`; `patchkit.wasm` does the same (byte-identical output to the CLI for `digikit/pluck` alone on both
  devices); an unknown module, a missing requirement and a `requires` cycle are refused with the module names. `Library::with_requires`.
- Small: the PLUCK gallery README uses lists (the gallery renders no tables); `fw:onboard` is marked not built (backlog).

Verified (quick proof tier, no full tiers): `cargo test -p patchkit` (all; new: allocation and gap tests in `packages/patchkit/tests/prims.rs`,
requires/cycle test, PLUCK-is-9 in `packages/patchkit/tests/frameworks.rs`), 82 Python tests of the patchkit files (new `tests/patchkit/test_pk_types.py`,
a gap-row case in `tests/patchkit/test_pk_machines_fw.py`), `pnpm --dir apps/web test` 29 of 29, `mise run patch:proofs` 10 of 10
(gain dt2/dn2, drive, voice dt2/dn2, machines dt2/dn2, pages dn2, pluck dt2/dn2; the PLUCK and PAGES proofs now expect types 9 and 6,
the others did not change), smoke matrix 15 of 15 cells (DT2 1.17 and 1.16, DN2 1.12; 11 on the first run, the other 4 after the `expect_mem` / `expect_dsp` type
constants of the select tests changed to the permanent numbers: machine-clone 8, pluck 9 on both devices, dn2-sine 7, dn2-edge 8). No frame golden
changed and none was re-recorded: the machine list shows the same entries in the same order, gap rows are hidden.

### Phase 3b.1 results (2026-10-14, lane `a`)

Built: `packages/patchkit/src/expr.rs` (P3), `prim.rs` (manifest checks, registry and section builders, ColdFire site
decoding and tail re-encoding, plan records), wiring in `apply.rs` and `module.rs`, `explain` sections for registries,
copied sections and hooks, `cf_params_refs` / `cf_params_bounds` in `tools/patchkit/port_resolve.py`. Tests: 7 unit tests
(`expr`, `prim`), `packages/patchkit/tests/prims.rs` (18 integration tests over the real ports and stock images: typed writes and their
errors, multi-site writes, registry rows/symbols/strings, canonical order, entry errors, sections on ColdFire and SHARC,
hook on ColdFire and SHARC, non-boundary, re-encoded PC-relative instructions, device filters, explain, plan.json),
`tests/patchkit/test_pk_prims.py` and `tests/fixtures/modules/registry-demo` (DT2 1.17, 1.16 anchor resolves too): boots to ready on the
ColdFire core, the hooked stock setter `Kit::vfunc_12` is called three times and behaves as stock while the hook counts,
the registry rows and the copied section read back, and `apps/web/tests/patchkit.test.mjs` (wasm = CLI, plan.json
included). The old `[[machine]]`/`[[param]]`/`[[table]]`/`register` path is untouched; its tests still pass.

Manifest syntax (all expressions are strings or integers; symbols are `[A-Za-z_.][A-Za-z0-9_.]*`):

```toml
[[registry]]                      # P2 (framework)
name = "machines"; first = "image.u8(cf.x + 1) + 7"; max = 16; region = "cf.expansion"; devices = ["dt2"]
[[registry.field]]
name = "routine"; kind = "sym"    # u8 | u16 | u32 | cstr (max_len?) | sym ; default?
[[entry]]                         # P2 (content module)
registry = "machines"; id = "pluck"; routine = "pluck_render"; devices = ["dt2"]
[[section]]                       # P1
name = "sel"; region = "cf.expansion"
from = { anchor = "cf.x", offset = 0, len = "image.u32(cf.x) * 4", expect_sha256 = "..." }   # len, expect optional
append = { registry = "machines", row = ["$selector", "u16:$index", "machines.first"] }
[[op]]                            # P3 + P6
op = "write"; anchors = "cf.params.bound.*"; offset = 2; value = "image.u32(this + 2) + params.count"; width = 4
[[op]]                            # P4
op = "hook"; anchor = "cf.x"; offset = 0; to = "my_routine"; displaced = "run"; kind = "jump"   # kind = call: jsr.l
```

Symbols: `<reg>.first .count .rows .row_size`, `<reg>.<id>.index`, `<section>` and `<section>.size`,
`hook.<anchor>.tail` and `hook.<anchor>.return` (`hook.<anchor>.<offset>.*` when offset is not 0). All are visible to
objects as undefined symbols (the usual `[object.externs]` maps a name like `hook.cf.x.tail`) and to expressions.

Deviations and readings of the spec (simplest consistent choice):

- **Registry rows** use natural field alignment, row size rounded to the widest field (u8/u16/u32/cstr pointer/sym
  are 1/2/4/4/4 bytes). Endianness is the region's CPU (ColdFire big, SHARC stream little). The table is rows, then the
  strings, in the registry's region; an empty registry still takes 4 bytes so `<reg>.rows` is valid.
- **Section row words** refer to the entry as `$field` and `$index`; a word is `EXPR`, `u8:EXPR`, `u16:EXPR` or
  `u32:EXPR`. A cstr field's `$name` is the string's address.
- **Stock reads are verified reads:** `image.uN(anchor + off)` must lie inside the anchor (covered by the port's
  sha256) and its cpu decides the byte order. `from` ranges beyond the anchor need `expect_sha256`, as a write does.
  `first`, `len` and table sizes are static: anchors and image reads only, no module symbols.
- **Hook sites** are decoded from the anchor's first byte so an offset inside an instruction is refused. A site past
  the anchor needs `expect_sha256`. `jmp.l` is 6 bytes; the site grows by whole instructions and is NOP-padded.
  Re-encoding in the tail: `(d16,PC)` operands become `abs.l` with the same target (opcode mode 7/2 -> 7/1), `bra/bsr/
  bcc` become the 32-bit form; `(d8,PC,Xn)` and any other PC-relative form are refused. A displaced `jsr` pushes the
  tail address instead of the site address (a callee that reads its return address would see the difference).
- **`kind = "call"` defaults to `displaced = "drop"` and refuses `run`:** the routine returns to the end of the site,
  where the displaced instructions no longer exist. SHARC hooks are jumps only. The SHARC site is one 48-bit
  instruction at a parcel-aligned offset; patchkit does not decode it, so a displaced control-flow instruction or the
  last instruction of a loop is the author's problem (the existing `jump` op had the same limit and stays).
- **P5:** `devices` is accepted on object, section, registry, entry, op, claim and anchor_rule; unknown device names
  are refused at load (`dt2`, `dn2`). A registry with the same name may be declared once per device.
- **P6:** `tools/patchkit/port_resolve.py` rules `cf_params_refs` and `cf_params_bounds` (optional `exclude_anchors`) emit
  `<name>.0 .. <name>.N-1` (DT2 1.17: 56 references as 4-byte data anchors, 54 bounds as 7-byte code anchors, the
  numbers of the old scan). A `write` with `anchors = "PREFIX.*"` visits each in numeric order, `this` names the
  current one. `params::scan` and the `cf.params.sites` anchor stay until 3b.3 deletes the old path; lane b adds the
  two rules to `modules/core/params` and drops the old one then.
- **Plan:** new keys `registries`, `sections`, `hooks` (absent when empty, so old plans are byte-identical);
  registry/section/hook symbols appear in `symbols` as `module@version:name`.
- `patchkit.wasm` grew from 638 KB to 1.05 MB (the ColdFire decoder tables now link in); still under the 2 MB check.
- Not done: the `explain` SHARC listing of a hook tail (bytes only), moving `jump` on the new `hook` op (dsp-voice
  keeps `jump`; lane b may switch it).

### Wave 7 (2026-10-14): parallel lanes

| Lane | Work |
|---|---|
| a | 3b.1 primitives |
| b | 3b.2 framework modules, then 3b.3 with lane a's work merged in |
| fastweb | DN2 research: RAM tables behind DN2 parameter pages, a note-on edge for DN2 voice routines, voice slot to track map, per-track parameter words; findings and anchor rules only |
| read-only checkers | second-agent checks of [D] results in findings 01, 02, 06, 17 (promote to [V] or correct) |

Proof speed (added 2026-10-14): the pk_proof_* tools take up to an hour each (one emulator process per part, a
cold boot per part, seconds of rendered audio per check, single-core, non-PGO lane builds, parts run in sequence).

- [ ] Quick tier: `--quick` on every pk_proof_* tool (fewer notes, short renders, the checks that catch breakage);
      full tier only at integration. Target: a few minutes per tool.
- [ ] Snapshot restore between parts instead of a cold boot per part (one coupled cold boot per image, cached).
- [ ] Independent parts in parallel processes (`--jobs`), bounded to the free cores.
- [ ] Proof runs use a PGO build of digi-emud when one exists for the device.

- [x] Quick tier budget (done 2026-10-14, lane `a`): about 60 s wall per proof tool. Wall seconds before -> after
      (default `--jobs`; "before" from the merged branch, DN2 tools measured on the same machine at the start):

      | tool | before | after |
      |---|---|---|
      | gain (dt2 / dn2) | not measured (positional arguments only) | 11 / 16 |
      | drive dt2 | not measured | 35 |
      | voice dt2 | 25-30 | 24 |
      | machines dt2 | 200-515 (63 with 4 jobs on a quiet machine) | 45 |
      | pluck dt2 | 346-362 (227 with 4 jobs) | 76 |
      | voice dn2 | 74 | 50 |
      | machines dn2 | 60 | 57 |
      | pages dn2 | 39 (in sequence) | 18 |
      | pluck dn2 | 231-236 (in sequence) | 55 |

      What made the difference: (1) a sounding PLUCK note costs about 35 s of wall per emulated second (the patched DSP
      runs interpreted; an idle second costs 3 s), so the quick parts turn DECAY down right after the first trig, play
      0.33 s notes, measure windows of 0.1-0.2 s and compare the decay *slope* at two settings instead of T60 over seconds;
      (2) the PICK comb check resets the excitation noise (`_pk_rng`) before each note, so two notes differ only by
      the parameter (the single-note comb statistic scattered by +-5 dB between noise states); (3) `default_jobs` never
      runs fewer than 4 parts side by side (bounded by cores / 2), even under load; (4) pages_dn2 and pluck_dn2 moved
      onto `tools/patchkit/pk_proofkit.py` (parts in parallel, `--jobs`, `--out`); (5) the persistence part polls (the save
      until the card writes stop growing, the patched reload until the type is back) and waits 3 s instead of 6 s for
      the stock load; (6) gain and drive take no positional arguments (DT2 1.17, stock inputs of the harness, the
      patch built on the fly). Merged or dropped in the quick tier of PLUCK DT2: the separate select, knob, tuning
      parts (a, bq, cq) are one part `aq`; PICK is one deterministic pair of notes; the full tier is unchanged.
      `mise run patch:proofs` runs every tool's quick tier in sequence (`tools/patchkit/pk_proofs.py`): see "Quick tier
      results" below.
Quick tier results (2026-10-14, lane `a`): `mise run patch:proofs`, ten tools in sequence, all PASS, total 387 s
(gain dt2 11, gain dn2 16, drive dt2 35, voice dt2 24, machines dt2 45, pluck dt2 76, voice dn2 50, machines dn2 57,
pages dn2 18, pluck dn2 55). PLUCK DT2 is above 60 s (four parts in parallel; the slowest, the persistence part, is
bound by a 3 s save, two project loads and a second daemon for the stock firmware, about 17 emulated seconds at 3 s of
wall each); nothing is above 90 s.

Teeth (broken images from `tools/patchkit/pk_break.py`, loaded with `PK_PROOF_IMAGE=IMAGE`; built from a copy of the module
sources under out/broken/, never committed):

| tool, broken input | result | the failing checks |
|---|---|---|
| machines dt2, core/machines persistence immediate left at the stock value | FAIL, 43 s | project reload brings type 7 back: 0 |
| pluck dt2, the same break | FAIL, 81 s | project reload brings type 7 back: 0 after 6.25 s |
| pluck dt2, digikit/pluck lists DECAY and PICK in each other's descriptor fields | FAIL, 82 s | descriptor names, engine defaults (slot 26 and 27 exchanged), TX frame, and dq1/dq2 cannot set DECAY (encoder 2 moves the PICK slot) |

The real images pass (the table above). Margins of the quick thresholds, measured on the real image:

| check | measured | limit | broken or ignored |
|---|---|---|---|
| tuning, TRIG 1 and keyboard TRIG 5 (0.1 s windows) | +0.20 and -0.02 cents | 3 cents (the full tier's) | an off-pitch voice moves it by tens of cents |
| DECAY slope, 0x2800 against 0x1800 | ratio 0.44 | below 0.75 | ratio 1.0 when DECAY does nothing |
| PICK comb, same noise state, half against none | +10.5 dB (8.3 .. 10.9 over three seeds) | above 4 dB | 0 dB: the two notes are bit-identical |
| DN2 DECAY slope ratio | 0.42 | below 0.75 | 1.0 |
| DN2 BRIGHT, HF/LF bright against dark (0.125 s in) | +15.3 dB | above 6 dB | 0 dB |
| DN2 PICK excitation against the numpy model | max difference 0 | 0 | any difference |

Findings on the way: a sounding PLUCK note costs 35 s of wall per emulated second only while the voice rings (the
voice ends after 8 blocks below 2^-16; DECAY 0x1000 notes cost about 1.5 s), so ring time, not emulated time, sets the
cost of a PLUCK proof. The engine slots of a PLUCK track read 0 until the first trig.

- Integration rule: the coordinator checks a merge with verify plus the smoke matrix (`--jobs`), not every proof tool;
  full-tier proofs only before a merge to `main` (AGENTS.md).

Patched DSP speed in the emulator (2026-10-14, lane b, findings 18): a ringing PLUCK cost 42 DSP s per audio s.
Cause: code-watch false sharing (module stores on the 64 KiB page of stock L2 code re-hashed all code ranges up to
49 times per block), fixed by watching exact code spans: 4.5 s, PCM bit-identical, stock +0.7% (coupled_ab). What
remains (about 3 s on PLUCK; stock non-PGO is 1.31): the patch bars whole stock AOT regions around a changed word
(the voice hook in the sample renderer, the selector-table literal in the hot per-frame function); appended module
code itself is interpreted but small (0.1-0.25 s).

- [ ] Exact barring: per-region code extents in `tools/sharc_dev/sharc_rsgen.py` so only regions containing a changed word are
      barred; regenerate the DT2/DN2/shared cores (about half a day + verify; measured ceiling PLUCK 4.5 -> 2.76,
      dsp-voice 2.0 -> 1.52; about 70%).
- [ ] Avoid patching hot functions where a cold site does the same job (e.g. the selector-table literal).
- [ ] Real-time play of patched DSP modules in the desktop app (stock is about 1.0 with PGO): needs the above plus
      compiled appended code (fast tier for L2 module code, or an AOT core regenerated for a patched image in dev).
- [ ] DN2: put module data on its own 64 KiB page (fast-tier kernels decline stores into code-watched pages).

After wave 7: DN2 parameter pages and PLUCK on DN2 (as modules), Phase 7 docs, PGO retrain at the wave end,
browser DSP audio (separate sharc-jit plan) when chosen.

### Phase 4: proof-of-concept modules

- [x] 4.1 `digikit/master-drive` (or compressor), DT2 and DN2: a new parameter carried in a frame word the DSP
      discards today, DSP code at the master hook. First module that changes both CPUs. Done 2026-10-13 (lane
      fastweb), see "Phase 4.1 results".
- [x] 4.2 Simple voice (sine or noise) on DT2 through `core.dsp-voice`. Done 2026-10-14 (lane fastweb), see
      "Phase 4.2 results". The Pluck DSP side (`digikit/pluck`) is done too; its ColdFire entry is in, see "Phase 3 results (wave 6)".
- [ ] 4.3 `machines/granular` DT2 (grains from sample memory), 3-5 days after 4.2.
- [ ] 4.4 `machines/granular` DN2. **Open decision:** grain source (track oscillator or wavetable, audio input, or
      a resample buffer).

### Phase 5: web (about 4-5 days, can start after Phase 2)

- [x] `patchkit.wasm` worker; `modules/index.json` build step.
- [ ] Browser cold boot from `.syx` and in-page extraction (open: the patched image boots in the plain core, with
      screen and controls but no DSP audio).
- [x] Load the COI service worker (both pages, through the shared layout).
- [x] `/patch` gallery (categories, status and compatibility badges, dependency and conflict explanations),
      patch, verify, boot, download.
- [x] UI cleanup: dev-only controls behind a flag, device follows the uploaded firmware.

### Phase 5 results (2026-10-13, lane `b`)

- Build step `apps/web/scripts/gen-modules.mjs` (run by `predev` and `prebuild`) writes
  `public/modules/index.json` (cards: id, name, category, status, summary, targets, requires, conflicts, claims,
  `gallery/README.md` as data blocks, never HTML) and `public/modules/bundle.json` (ports and modules as texts, equal
  to `tools/patchkit/patchkit.py bundle`). Committed `modules/` and `devices/ports/` only; both outputs are gitignored. The
  build guard now also rejects `.syx`, `.snap` and `.mstate` files.
- `patchkit.wasm` gained one export, `pk_check` (per-module compatibility with an image, `wasm.rs`). The worker
  (`src/patch/patch-worker.ts`, wrapper `patchkit-core.ts` shared with the node tests) checks, applies in
  canonical order and verifies against the stock image; it fetches only the site's own wasm and bundle.
- `/patch` (`src/components/PatchGallery.tsx`): drop zone, identification against the device profiles and ports,
  category filter, status and per-firmware badges, automatic `requires`, conflicts and shared claims explained,
  patchkit's own error text for incompatibility, result panel, download (`Digitakt_II_OS1.17+digikit-master-gain.syx`),
  "Boot in emulator" through IndexedDB (`src/patch/handoff.ts`, `/emulator/?handoff=1`), safety notes.
- Emulator page: shared layout with a top nav and theme tokens (`styles/site.css`, light and dark); dev-only
  controls (device select, export diagnostics, DSP image/state/snapshot pickers, bench, `?leds=`) show on the dev
  server or with `?dev=1` (`src/dev.ts`); the device follows the firmware's hash. The one pre-existing oxlint
  error (early return in `Faceplate`) is fixed.
- Checked with `tools/devtools/web_shots.mjs` (headless Chrome): on the dev server and on `astro preview` (no headers), DT2
  1.17 and DN2 1.12 patched with `digikit/master-gain` in the page are byte-identical to the command line, and "Boot in
  emulator" boots the patched image in the plain core to the startup display.
- Open: no DSP audio and no cold boot in the browser; the gallery has one real module; hardware untested; the
  emulator page's own `fetch('/emulator-core.wasm')` is root-absolute (breaks under a Pages sub-path, as before).

### Fidelity follow-ups (2026-10-13, lane `a`)

- ColdFire code outside the MAIN image runs in the native runtime (DDR and SRAM, decode cache evicted by CPU stores and
  by DMA ranges; fused loops, HLE and watch bits stay MAIN-only). Unit test with run-time-written and self-modified code;
  paired A/B on the DN2 image: ColdFire thread CPU 0.5221 vs 0.5204 s per audio second, PCM identical. Finding 07.
- `core/machines` shims moved to `cf.expansion`: the cave holds the 56-byte loader only (was 372 to 452 B). The XSLICE
  and XClone goldens on 1.17 and 1.16 are unchanged. One pre-existing failure of the proof (XClone MOD destination
  frames), finding 02.
- `underrun` event and `status.heartbeat.underrun_count` (finding 17).
- DT2 1.16 in the matrix (stock, `digikit/master-gain` + `test/cf-demo`, `digikit/master-drive`, `core/machines` + `test/xslice`),
  fresh ready inputs (`tools/emulator/ready.py dt2 --syx Digitakt_II_OS1.16.syx --out out/ready-pk-dt2-116 --force`), goldens
  recorded: 4 of 4 PASS (29 to 36 checks, 45 to 98 s per cell). This closes the pre-flash item 11 for the emulator side.

### Phase 6: agent tooling (about 2-3 days, parallel with Phases 1-3)

- [x] `tools/coldfire/fw.py`: compact query CLI with `batch` mode and cached graphs; skill file.
- [x] Atlas `out/atlas/<image>.sqlite`: machines, parameters, FX, page views, strings, sequencer structures; FTS5
      over strings and decompiled C; glossary from the manuals in `out/refs/`. Extractor rules committed, data
      derived.

### Phase 7: documentation

- [x] Rewrite `docs/PATCHING.md` (done 2026-10-14, lane `fastweb`): quick usage first (create, build, patch, inject, test, flash and recover), then
      how it works (ports, frameworks, linker, regions, conflicts, safety rules).
- [x] `modules/README.md`: authoring guide and manifest reference (done 2026-10-14, lane `fastweb`; worked examples
      `digikit/master-gain` and `digikit/pluck`). `README.md` has a "Patching" section; the old notes are in
      `docs/archive/PATCHING-pre-patchkit.md`; the `digikit-module` skill is current.
- [ ] Findings for every [V]/[D] result from Phase 0.

### Later

- Cross-porting between devices (for example DN2 FM Drum onto DT2): a module section sourced from the user's second
  firmware at patch time, so no vendor bytes are stored. Research only; the `.dko` format leaves room for it.

## Emulator confidence, wave 3 (2026-10-13, lane `a`)

- Conditional Type 7a is modelled (finding 05): DT2 1.17 and 1.16 PLAY in patterns 3 and 4 runs clean; DN2 pins and the
  61 oracles, DT2 tone, oracles and tier-quick pass against regenerated cores (`gen-next` dirs, coordinator swaps them
  in at merge).
- Boot ROM rules (finding 06, "Boot ROM conformance"): `tools/sharc_dev/sharc_bootcheck.py`, `packages/patchkit/src/bootrom.rs`;
  stock streams, `digikit/master-gain`, `digikit/master-drive` and the L2 canary pass. Two defects fixed: the FIRST block's
  next-application pointer was not grown by `append_block`, and the append window included the ROM's 8 KB workspace.
  The emulator now refuses such streams (`tools/emulator/ready.py`, `firmware.load`, `patchkit verify`).
- selcc pointers: a normal-word access through a byte-address pointer matches the manual (finding 05).

## Pre-flash review (what no emulator run or public manual closes)

Em flashes nothing until everything works in the emulator; these are the risks that remain after that, each with the
best mitigation available. Ranked by what a failure costs.

| # | Risk | Evidence now | Mitigation |
|---|---|---|---|
| 1 | The boot ROM rejects the patched DSP stream (it applies a rule the manual does not state) | All stated rules checked on stock, gain, drive and canary streams (finding 06). A rejected stream is a **boot hang**: the ColdFire polls the SHARC handshake with no timeout | First hardware step is the image that changes nothing (`tools/patchkit/patched_image.py`), then a DSP change with no appended block (`digikit/master-gain` is a 3-byte in-place edit plus a 148-byte record block: take the smallest first), then the append. Keep the stock `.syx` and the STARTUP-menu recovery (0.2) at hand |
| 2 | Cycle cost: the emulator counts instructions, silicon counts cycles (L2 fetch and data wait states, cache misses on first execution, bank conflicts, SIMD memory pairs) | `digikit/master-drive`: 20 to 968 DSP instructions per block; idle share 50 % (DN2), 68 % (DT2) measured in instructions (finding 22, 0.7) | Budget a module at no more than a quarter of the measured idle instructions per block; keep the hot loop short enough to stay in the cache; the neutral setting bypasses the heavy path where the module allows it. Listen for clicks at the first run; the `underrun` event (finding 17) reports the host audio sink only, not the hardware's DSP load |
| 3 | Real DDR contents and timing: the emulator fills DDR from the stream's FILL blocks and skips the initcode's clock and DMC calibration | No patch writes DDR; sample RAM use is stock | None needed while modules stay out of DDR; keep it that way in v1 |
| 4 | Boot time grows with the stream: the ColdFire sends one byte per DSPI frame and handshake poll; the canary doubles the section | Gain +148 B, drive +620 B on 321 KB; the transfer time per byte was not measured against hardware | Ship modules small; use the canary only on the emulator |
| 5 | SPI slave boot expects multiples of 1024 bytes (HWR 40-21); the stock section is not | Stock boots with a short tail; patched tails differ (finding 06 (d)) | If the first appended image hangs while the in-place-only image boots, pad the appended block so that the section length keeps the stock value mod 1024 (505 including the command byte); a one-line change in `append_block` |
| 6 | L1 targets and L1 PM room (appends refuse L1); the datasheet map is not in `out/refs` | No L1 appends | None; L2 only |
| 7 | Rules and state of the ROM-prepared environment: L2 ECC initialised, caches off at handover, `RCU_BCODE` bits | Stock code already runs from L2 loaded this way (DT2: the 109 KB block at `0x20000000` holds functions at pc `0xb80000...`, 24,070 instructions in sharcdb) | Same path as stock; nothing to do |
| 8 | Float behaviour on silicon against the core: denormal flush, saturation mode, reciprocal seed (RECIPS, RSQRTS) in module code | Seven DN2 PCM pins and 61 oracles exact against the core; module code unit-run and PCM-proved in the emulator only | Prefer plain add, multiply and compare in module code; check a module's output against its emulator render by ear and by the harness PCM test before trusting a new operation |
| 9 | Interrupts, DMA and ColdFire timing under real load (the DSPI frame cadence, SSI underruns, hot sample loading): the emulator is deterministic | Coupled runs and 35 s tours pass; 40-input stress in the harness | Hardware soak after the first success |
| 10 | The MAIN OS image validator (`0x400d9e4c`) and the real flash writer were not run; flash cap 3 MiB and the HMAC trailer are checked by the emulated bootstrap | 0.2 [V] for the bootstrap; patched containers rebuild stock byte for byte | The harmless image first; the updater's own checks reject a bad container before it writes (the bootstrap runs from section 2, which patchkit never touches) |
| 11 | Firmware version: the device runs 1.16; most proofs ran on 1.17 (and DN2 1.12). 1.16 has the same DSP section; its MAIN differs, `digikit/master-drive` now passes the 1.16 smoke cell, and 36 of 135 DT2 FX oracles differ on 1.17 | Port files for 1.16 exist; 1.16 PLAY in patterns 3 and 4 passes | Done in the emulator for `digikit/master-gain`, `digikit/master-drive`, `core/machines` + `test/xslice` on 1.16 (matrix cells, "Fidelity follow-ups"); flash the 1.16 port |
| 12 | DN2 patches cannot be tested on hardware at all (no device on the desk) | Emulator only | Say so on the gallery page: DN2 modules are emulator-tested only |
| 13 | Saved projects and kits made on patched firmware read back on stock firmware (Phase 3 not done) | Not tested | Do not save over a stock project while testing; back up the card |
| 14 | Analog level: a drive or gain module can raise the output above the converter's range | Q31 clip in the emulator, neutral default is bit-exact | Start at the neutral setting with the monitor volume low |

## Rules

- Never change sections 2, 4 or 8. patchkit enforces this with no override.
- Flashing hardware is Em's decision, every time.
- No firmware bytes in the repository: ports store hashes, modules store our own code.
- Patched images show a version marker so issues are not reported to Elektron.
- No user-provided firmware is ever uploaded or hosted by the site.

## Risk register

| Risk | Mitigation |
|---|---|
| Device rejects image or fails to boot | 0.2 emulated bootstrap acceptance and STARTUP-menu recovery; harmless first hardware test; DIN MIDI interface ready |
| Flash overwrite past the OS container | 0.3 flash layout; aPLib packer; packed size cap |
| Expansion region in use | 0.3 memory map, canary over long runs |
| Free SHARC L2 not free | 0.4 canary over scenarios |
| No DSP voice hook | 0.7 gate; master hook first; Phase 4 order |
| Toolchain encoding bugs | Re-decode every SHARC word; unit-run functions on the emulated cores |
| Projects saved on patched firmware break stock firmware | Phase 3 persistence check; machines reuse the 8 engine slots |
| New parameters not storable or p-lockable | Prefer the 8 positional engine slots and discarded frame words in v1 |
| DSP overrun on hardware | Idle-loop headroom budget in the harness |
| New Elektron OS | Signature-based ports; `fw:onboard` (not built, backlog); 1.17 is the first test |
