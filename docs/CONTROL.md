# Control socket

Drive a running emulator from scripts: load a patched image, step the guest, press keys, read the screen. This page
lists what is implemented. The design, including parts that are not built, is in
[design/emulator-control.md](design/emulator-control.md).

## Transport

| Item | Value |
|---|---|
| Protocol | JSON-RPC 2.0, one JSON object per line (newline-delimited) |
| Socket | Unix socket `<run_dir>/<pid>.sock` (Unix only) |
| Discovery | `<run_dir>/current.json`, key `socket` |
| Default run dir | `out/native/run` (mode 0700); `--run-dir DIR` overrides it |
| Events | Pushed on a connection after `events.subscribe`, as notifications `{"method": "event", "params": ...}` |

## Who serves it

| Server | Start | Notes |
|---|---|---|
| Desktop app | `mise run workbench` | On by default. `--no-control` turns it off. Prints `control socket: <path>` to stderr. |
| Daemon `digi-emud` | `mise run emu:serve` | No window. Starts paused: a client steps it or sets `run.mode free`. `--free` runs it live. |

The desktop app free-runs a runtime that has no DSP audio (ColdFire only, as a DT2 opened in a session started with the DN2's audio inputs) at the wall clock
(`Config::pace_free`); the daemon keeps full speed (about 20 times real time). Unpaced, a mouse click on the faceplate lasted seconds of emulated time and the
DT2 firmware read SETTINGS as a long press (it opens only on a press shorter than about 0.5 emulated seconds).

Coupled DSP audio in the daemon needs `--auto-coupled` and the ready inputs; see [PATCHING.md](PATCHING.md).
Zero-DSP mode retains the existing SSI/DMA clock with zero samples, including patched-image loads,
snapshot restores and reset reconstruction; it executes no SHARC engine. Its patched snapshots use a separate
`-zero.snap` identity, so earlier CF-only snapshots cannot silently remove that timing.
Startup flags are shared by both servers (`packages/emulator/control/src/cli.rs`): `--control`, `--no-control`, `--paused`,
`--run-dir DIR`, `--card-image FILE` (with `--card-folder DIR`: the +Drive folder the image was built from; the firmware's changes are exported to it on exit; `--no-card-sync` turns the export off), `--device-folders` (one card folder per device, below), `--mirror-dir DIR` (the folder of the first loaded device), `--scenario NAME_OR_FILE`, `--report PATH`, `--no-audio` and the coupled-audio
flags. `--scenario` needs a coupled-audio build.

## Command line

`mise run emu:ctl -- <subcommand>` runs `uv run python -m tools.control.digi_client <subcommand>`.

| Subcommand | Does |
|---|---|
| `status` | Print the `status` result |
| `load SYX` / `inject SYX` | `firmware.load`; options `--base-sha256`, `--start auto\|snapshot\|cold`, `--dsp-patch`, `--runtime [--patch-set NAME]` (`load` also `--card`) |
| `restart` | Back to the ready state of the running firmware |
| `step N` | Run N guest instructions |
| `tap CODE [--hold N] [--at-instr N]` | Press and release a key (default hold 40000000 instructions) |
| `frame out.png` | Save the OLED as a PNG |
| `call METHOD k=v ...` | Any method; each value is parsed as JSON, else kept as text |
| `events [kinds...]` | Print events as JSON lines until interrupted |
| `quit` | Stop the server |

Global options: `--socket PATH` (default: discover), `--run-dir DIR` (where to look for `current.json`).

`mise run patch:inject -- OUT.syx [--hot]` loads a patched image through `tools/patchkit/patchkit.py inject`;
`mise run patch:inject -- STOCK.syx --runtime` lets the emulator patch the stock image itself (`firmware.load {runtime}`, below).

## Methods

Wrapper is the `Emu` method in `tools/control/digi_client.py`. `hello` reports an incomplete method list (it omits
`mem.cf.peek`, `mem.cf.poke`, `mem.cf.write`, `dev.reload`, `card.*` and `watchdog.config`); use this table.

| Method | Wrapper | CLI |
|---|---|---|
| `hello` | `connect` | (automatic) |
| `status` | `status` | `status` |
| `run.mode` (`ui`, `paused`, `free`) | | `call` |
| `step` | `step` | `step` |
| `run_until` | `run_until` | `call` |
| `firmware.load` | `load`, `inject` | `load`, `inject` |
| `restart` | `restart` | `restart` |
| `input.button`, `input.turn` | `button`, `turn` | `call` |
| `input.tap` | `tap` | `tap` |
| `input.schedule` | `schedule` | `call` |
| `inputs.recorded` | `recorded` | `call` |
| `framebuffer.get` | `frame`, `frame_png`, `frame_hash` | `frame` |
| `leds.get` | `leds` | `call` |
| `mem.cf.peek`, `mem.cf.poke` | `peek32`, `poke32` | `call` |
| `mem.cf.write {addr, hex}` | | `call` |
| `dev.reload {modules_dir?, swap_modules?, auto_no?, build?}` | | `call`; `mise run dk:dev` |
| `card.overlay`, `card.export`, `card.load`, `card.status`, `card.cancel`, `card.folder`, `card.sync` | | `call` |
| `midi.in`, `midi.out` | `midi_in`, `midi_out` | `call` |
| `midi.usb.in`, `midi.usb.out` | `usb_in`, `usb_out` | `call` |
| `flash.status`, `flash.read {offset, length}` | | `call`; `patchkit flash --emulator` |
| `watchdog.config` | `watchdog` | `call` |
| `cpu.exceptions` | `cpu_exceptions` | `call` |
| `diagnostics` | | `call` |
| `audio.report` | `audio` | `call` |
| `state.digest {arch?}` | `state_digest` | `call` |
| `state.save {path}`, `state.load {path}` | `state_save`, `state_load` | `call` |
| `pcm.tap` (`start`, `report`, `capture`, `capture_stop`) | `pcm_start`, `pcm_report` | `call` |
| `cf.profile` | | `call`; only with the `cf-profile` feature |
| `dsp.peek` | `dsp_peek` | `call` |
| `dsp.patch` | `patch` | `call` |
| `guard.set`, `guard.report`, `guard.clear` | | `call` |
| `events.subscribe` | `events`, `subscribe`, `drain_events` | `events` |
| `quit` | `quit` | `quit` |

`firmware.load {syx, runtime?, patch_set?, ...}`: with `runtime: true` the controller patches the stock image in-process with the
device's patch set (`patch_set`, default `emulator`; the roots are in `devices/*.toml` `[patch_sets]`, the modules they require are added)
and loads the result with the stock image as its base. A `syx` that already has a patch record is loaded as it is. The patched image is
cached in `out/runtime-cache/<stock sha256>-<set>-<key>.syx`; the key hashes the set, the modules (manifest and object bytes), the device
and port files and the patchkit sources, so any change to them patches again. The module library and the device files come from the
repository root (`Config.repo_root`, else `$DIGI_REPO_ROOT`, else the repository of the working directory); a build outside a checkout
answers `FIRMWARE_REJECTED` with that reason until a shipped bundle exists. The reply gets `runtime`:
`{patched, set, modules, cache_hit, base_sha256, cache_file, seconds}` (`cache_hit` is the patched image; the snapshot cache keeps
its own `cache_hit` at the top level), or `{patched: false, reason}` for an input with a record. `patch_set` without `runtime` is refused.
`status.firmware.runtime` keeps that report for the running image (null when no runtime was requested; a `restart` keeps it).

`firmware.load {replace: true}` loads the file as the first firmware of the controller: nothing of the running one is reused, and the file
may be another device or version (without `replace` an image is a variant of the running one and is injected). The desktop console's Open
and Reload use it. A load without `card` gets the running DT2's card when the image is a DT2 image, else the card remembered for that
exact stock firmware (desktop only), else none; an image of another device never gets a card.

`mem.cf.write {addr, hex}` writes a byte range (any alignment, hex text) of SDRAM in one call, with one decode-cache invalidation for the range: code
written this way runs as new code. Unmapped pages are mapped zeroed. Like `mem.cf.poke` it is a host write, not part of the replay record.

`dev.reload {modules_dir?, swap_modules?, auto_no?, build?}` brings the running session to the module objects as they are on disk now. Without
`build` it never builds. With `build: true` (the workbench button sets it) it first builds the stale objects of the session's modules in
the daemon's process (`patchkit::build::build_module`; ColdFire C, assembly and Rust crates, and SHARC+ sources; gcc, as, ld and cargo run as child processes, the SHARC+ assembler is the Selache library in the process):
a `core/*` module writes its committed `build/` objects, a content module writes `out/cache/dko`. The reply gets
`build: {built: [{id, objects, ms}], skipped: [id], ms}`. A compiler error is the error message (`dev.reload: build of ID failed:` and the
compiler output; for a SHARC+ source the assembler's or a check's message, e.g. `the ISA does not allow`) and nothing is reloaded.
The build runs on the controller thread, so the session is blocked while it runs.
The module set is the patch record of the running image; the image is made again from the stock image (`modules_dir`: the repository root,
default as for `firmware.load {runtime}`; the stock image is the load's base or the file of `firmware/` with the record's digest) and compared
with the image whose ready state the boot cache holds (the baseline: the running image, or after a swap the image the swap started from).
The reply has `path`:

- `swap`: only ranges of swap modules differ (`swap_modules`; default `core/ui`, `core/digikit-menu` and every module whose manifest requires
  `core/ui`). The baseline's ready state is restored (the warm `firmware.load` path, `start: snapshot`), every changed module is written
  over it whole (code, data, zeroed bss; a changed hook site too) and `loaded.current` becomes the new image. This equals a fresh boot of
  the new image as long as nothing in the ready state holds a pointer into the swapped code: the firmware reaches UI code only through the
  SETTINGS hook and the views opened from it, and the ready state is the main screen, SETTINGS never opened. The patch record is not
  compared. Reply: `{ranges, bytes, owners, ms, apply_ms, restore_ms, write_ms, restore}`. `auto_no` presses NO after the restore (DT2).
- `boot`: any other ColdFire range, a section other than MAIN and DSP, or no cached ready state to restore: the new image is loaded as by
  `firmware.load` (a cold boot unless that image was booted before), with `reason` (`core/runtime changed at 0x4671a000`) and the load's report.
- `dsp`: only the DSP section differs: the hot injection of `firmware.load`.
- `none`: the running image already is the new one.

After a swap the boot cache has no entry for the new image; a plain `firmware.load` or `restart` of it boots cold once. The next
`dev.reload` restores the baseline's state again. Refused while a job runs. The desktop workbench's Reload modules button calls it.

`session` (any method, an integer): the webview's session token (the desktop's `emu_call`). `firmware.load` and `restart` start the session
they carry (it must be newer than the current one; it is taken when the call succeeds); every other method must carry the current one,
else the answer is `STALE_SESSION` ("stale emulator session") and nothing runs. Without `session` nothing is checked (the socket's clients).
Time (release build): the first patch of a 1.17 or 1.12 image takes about 1.3 s, a cached one about 0.08 s.

`framebuffer.get {hash?, data?}`: `hash: true` adds `sha256`, the hex SHA-256 of the 1024 decoded OLED bytes
(the bytes `data_b64` carries; the hash the patchkit smoke goldens use); `data: false` omits `data_b64`.

`cpu.exceptions {deliver?}` (a test aid): `deliver: true` makes a CPU exception run the guest's own handler; the default is a fault of the run
(`step` fails with `exception vector N`), because an exception in the firmware is a bug the harness must see. The setting is kept for every runtime
installed later, so it holds across a soft reset. A soft reset also carries the RAM range `[digikit] keep_base .. + keep_bytes` of the device file into the new
cold boot (a model of a device whose RAM survives a reset; `docs/findings/46-digikit-debug.md`).
The old runtime keeps stepping while the cold boot runs on its worker; when the boot is installed, a `step` or `run_until` job in flight ends at once
(its result has `runtime_replaced: true`), so the new runtime starts at the same instruction in every run and not at a count that depends on the host's speed
(`docs/findings/55-fx-adapter.md`). A harness waits for the `firmware.loaded` event (it is sent before the job's reply).

`leds.get` returns `{led_revision, leds, rgb}`: `leds` is 128 hex digits (64 LED values), `rgb` 384 hex digits
(64 x r, g, b) or null until any is set. It reads the emulator's current state and does not consume it from the UI.

`state.save {path}` writes the ColdFire state (registers, RAM, peripherals, the card's write overlay) to `path` (whole file:
written as `path.part`, then renamed) and restarts the scenario clock there (`elapsed` counts from the save, as after a cold
boot to ready). `state.load {path}` restores such a file into the runtime a `firmware.load` just installed: the runtime must not
have run (`icount` 0; otherwise `firmware.load` again), the MAIN and the card must be those of the save (the snapshot checks the
MAIN digest, the device and the card capacity), and a job must not be running. The restored state has the same
`state.digest` as the saved one and goes on the same way (`packages/emulator/control/tests/state_save.rs`). A write watch
(`DIGI_WRITE_WATCH_AFTER=cacr`) that the snapshot is already past runs from the restore. No DSP or audio state is saved: use it on
a ColdFire-only daemon. The digikit ColdFire-only rigs (`tools/patchkit/pk_dkrt_boot.py` `CfFolderSession`) save the state of
their first cold boot at the main panel, keyed by the daemon binary, the image, the card and the environment, and restore it
(about 0.3 s instead of a 6 s boot) for every later rig of the same key and for `reset()`.

`state.digest {"arch": true}` leaves out the three counters of how the instructions were run (interpreted, idle skipped, RAM clear
skipped): two runs that reach the same guest state by different paths (one instruction at a time, the batched loop, the idle skip) have
the same digest. It is the oracle for a change to those paths: boot paused to a fixed `icount`, queue input, `run_until` fixed counts,
compare the digests of a daemon built before the change with one built after.

`--dsp zero --cf-snapshot FILE` (on `digi-emud` and the app) starts without a DSP: SSI paced, the DSPI2 peer
answers zeros, the ColdFire state restored from the snapshot (about 2.2x real time). `status.dsp` is `"zero"`,
`"sharc"` or `"none"`. `run_until {frames|audio_s}` is refused (no DSP session); `elapsed`, `icount` and `step`
work. No PCM is produced.

`guard.set {protect: [{lo, hi, label}], allow_pc: [{lo, hi}]}` arms write-range guards (addresses are integers or
`"0x..."` strings, `hi` exclusive; an empty `protect` disarms). A ColdFire store (8, 16 or 32 bit) that overlaps a
protected range while the PC is outside every `allow_pc` range is a hit. `guard.report` returns `{total, hits}`
(the first 64 hits: `pc`, `addr`, `size`, `label`, `icount`); `guard.clear` forgets the hits and keeps the guards.
`status.cf.guard_hits` is the total. The `guard` event is sent once per protected range on its first hit (`label`,
`pc`, `addr`, `size`, `hit_icount`, `total`). The run is never stopped. Armed guards turn off the fused-loop and
reset-clear shortcuts. Not checked: eDMA and other peripheral writes, and host writes (`mem.cf.poke`, card load,
`dsp.patch`). `coupled-run --guard FILE.json` takes the same object and adds `guard: {total, hits}` and the gate
`guard_ok` to the scenario report.

`midi.in {bytes | file, period?}` queues bytes on the MIDI DIN input (UART9, the port the firmware's MIDI driver reads: eDMA channel 36
into a 32 KiB ring, one interrupt per byte, vector 156; finding 25). `bytes` is a hex string (spaces allowed) or an
array of numbers, `file` a binary file. The bytes go in one per `period` instructions (default 42,240 = 320 us at
132 M instructions per second: the 31250 baud wire rate; 0 = as fast as the firmware takes its interrupts); the
queue holds 1 Mi bytes. `midi.out {file?}` returns `{bytes, hex, status}` for what the firmware sent on DIN since the last call
(it stores bytes straight into the UART9 data register) and clears it; `status` counts queued bytes, receive interrupts and
sent bytes. Not part of a snapshot. MidiRpc (`F0 00 20 3C 10 00 ...`) is **not** accepted from DIN: the device 0x10 handler
takes only the USB sources. USB MIDI is not modelled, so `midi.usb.in {bytes | file}` hands the bytes to the firmware's USB
receive callback (DT2 1.16 `FUN_4011d76c`, found from the call that registers it; DN2 1.11 `FUN_4011f0e0`) in an interrupt
taken at the next instruction boundary the guest allows, in chunks of at most 64 bytes one USB frame (132,000
instructions) apart (the optional params `chunk`, 1 to 384, and `period` change that: 384 and 16,500 model a high-speed microframe;
the callback is the SysEx assembler, so the firmware's USB packet split is not in the path); `midi.usb.out` returns what the firmware passed to its USB transmit function (`FUN_40004734`, the
MidiRpc replies as plain MIDI bytes). The first `midi.usb.in` or `midi.usb.out` starts the capture. A `.syx` MidiRpc request
from `tools/device/devrpc.py build_request` goes in as it is. Errors: bad params when the image's USB callback cannot be
resolved (known: DT2 1.16 and 1.17, DN2 1.11 and 1.12). While the next call is not due, or is due and waits for the interrupt mask
(the audio interrupt), the batched fast path and the analytic idle skip run as they do with no input queued, up to the instruction at
which the call is handed over (they ran one instruction at a time with any USB input queued, 135 ns an instruction; finding 43, "Upload speed"). The
instruction at which each call is handed over, and the guest state, are the same (`state.digest {"arch": true}` equal at every checked point).
The DIN input (`midi.in`) still keeps the one-instruction loop while bytes are queued.

`flash.status` and `flash.read`: the OS UPGRADE of the firmware writes its container to the flash. The emulator takes over MAIN OS's sector erase and page program
for the container area `0x80000..0x380000` (any other flash write runs the firmware's own code, as before) and keeps the result in a 16 MiB flash model that
`hle_flash_read` also serves. `flash.status` is `{erases, programs, bytes}` since the boot; `flash.read` returns bytes of the model as hex (at most 4 MiB). At the
soft reset after such a write the controller boots the container the guest wrote (`flash.booted` event, or `flash.invalid` when it does not read back as a
firmware) instead of the image it had loaded. `patchkit flash STOCK.syx --emulator SOCKET` drives it (finding 75).

`pcm.tap` needs the coupled-audio build and a DSP session. `capture` and `capture_stop` have no wrapper.

## CPU load (`status.load`)

`status.load` is `null` until a window has closed (at least 1 emulated second; a `status` poll closes it), then:

    {"window_s": 1.2,
     "cf":  {"model_pct", "real_pct", "real_high_pct"},
     "dsp": {"busy_per_block", "model_pct", "real_pct", "real_high_pct", "peak_pct"} | null}

`model_pct` is the load in the emulator's own model. ColdFire: busy / total instructions of the window (busy = the
interpreted ones; the idle fast-forward and the RAM clear do not count). DSP: busy instructions per block over the
666,667 cycles of a block (1 instruction = 1 cycle). `real_*` estimate the chip. ColdFire: busy instructions per
emulated second (total / 132 M) x busy CPI / 264 MHz, CPI nominal in `real_pct`, pessimistic in `real_high_pct`. DSP:
the model load x the device's real-cycles-per-instruction factor, nominal in `real_pct`, high in `real_high_pct`.
`peak_pct` (DSP only, optional) is the largest unsmoothed `real_pct` of any single window since the meter was reset (a
restored or replaced runtime resets it). On the device in the emulator, the runtime's HEALTH page shows the model figure
(its EMUCLK ticks once per instruction), so compare it with `model_pct`, not `real_pct`. The real values are estimates from the public manuals' cycle tables and our instruction counts, not measurements. The
factors are one file, `devices/cycle-factors.json` (cited findings inside; `tools/patchkit/pk_dsp_budget.py` reads the
same numbers). Each closed window is blended 50/50 into the shown value; a restored or replaced runtime clears it.
`dsp` is `null` without a DSP session (`status.dsp` is `"zero"` or `"none"`). The desktop panel gets the same object as
`load` in its audio status. The browser (wasm) build has no controller and shows no meter.

## Events

`halt`, `stall.end`, `underrun`, `session.changed`, `scenario.done`, `firmware.loaded` (`ok`, `seconds`), `mode`,
`guard`, `card.started`, `card.ready`, `card.error`.

## Error codes

| Code | Name | Code | Name |
|---|---|---|---|
| -32700 | parse error | 4 | firmware rejected |
| -32600 | invalid request | 5 | audio unavailable |
| -32601 | method not found | 6 | guest fault |
| -32602 | bad params | 7 | DSP halted |
| 1 | no firmware | 8 | I/O |
| 2 | stale session | 9 | timeout |
| 3 | busy | | |

`card.folder`, `card.folder_set` and `card.sync`: the module store as a folder. **The folder belongs to the device, not to the process.** With
`--device-folders` (the launcher passes it; the desktop app keeps the user's choices in its app data folder, `card-folders.json`) every load
(`firmware.load`, Open, Reload, a device change) sets the folder of the loaded device: the user's choice for it, else `plus_drive/<device>/`
(`$DIGI_PLUS_DRIVE` or the repository's; made if missing). A load without a `card` builds the card from that folder when it holds something the
device's card takes (`plusdrive_format` builder, cached by content under `<run dir>/cards/`; a card the user chose with `card.load` stays). `--mirror-dir DIR`
alone (no `--device-folders`) is the folder of the first loaded device only: a load of another device has no mirror. While the emulator runs the
store files the guest wrote to the card are mirrored into `<folder>/digikit/` about 2 s after the guest's last card write (one way, card to folder; a file
the card no longer has is removed from the folder, moved to `<out>/backup/mirror-<stamp>/`; a session removes only files it saw on the card). A load finishes the
running device's pending mirror pass into its own folder before the next device's folder takes over. `card.folder` returns
`{folder, continuous, files: [{path, bytes, sha256}], last_sync (ms), last, behind, error, device, default_folder, custom, chosen_missing, next_folder, reload_needed}`
(`folder` null and `continuous` false without a mirror). `custom`: the folder is the user's choice; `chosen_missing`: a saved choice whose folder is gone (the default is used);
`next_folder`: what the next load uses; `reload_needed`: `next_folder` is not `folder` yet. `card.folder_set {dir?, device?}` saves the choice for the loaded (or the named)
device and returns `card.folder`'s answer; no `dir` goes back to `plus_drive/<device>/`. A device without a path filesystem (the DN2) mirrors to the new folder at once (files of the
new folder that differ from the card's store are replaced, the old ones go to the backup); a device whose card is built from the folder (the DT2) takes it at the next load
(`restart` builds the card again from the folder in use). `card.sync` mirrors now and returns the same as `card.folder`. Details: docs/plans/card-folder.md.

`card.load {dir, sync?, export_out?}` builds a card from `dir` (a `plus_drive/dt2/`-style folder: docs/SAMPLES.md) and also records `dir` as the card's +Drive folder. The same holds for a card given at start with `--card-image` and `--card-folder` (`mise run workbench` passes both; `sync` is on unless `--no-card-sync`). `card.export` and the quit export use the running device's `[card]` layout (found by the base firmware's hash). `card.export {dir?, out?, prune?}` brings
that folder (or `dir`) up to date with what the firmware wrote to the card: files of the path filesystem by name in the
native format, `.mmcfs/{projects,sounds,kits}/NNNN.bin` slots and `.mmcfs/boot/SSSSSS.bin` regions (finding 24). The tree goes
to `<out>/export/<stamp>/` first, each replaced file to `<out>/backup/<stamp>/` (`out` default `$DIGI_PLUSDRIVE_OUT` or
`out/plusdrive`); files the firmware deleted are listed and kept unless `prune`. With `sync: true` a `quit` (or the controller
stopping) saves `overlay.bin` next to the card image and runs the same export, and logs `card: exported N changed files to <dir> (...)`. The module store files (`digikit/`) are exported too (three-way against the card as built: a new or changed file is added or replaces the folder's; a file the card no longer has is listed in `deleted_by_firmware` and kept unless `prune`); during a session `card.folder` and `card.sync` keep them current (below). From the shell:
`plusdrive export IMAGE [--overlay overlay.bin] -o FOLDER [--out DIR] [--prune]`.

## Examples

```sh
mise run emu:serve                            # headless, paused
mise run emu:ctl status
mise run patch:inject -- out/patch/mg.syx     # cold or snapshot start, as needed
mise run patch:inject -- out/patch/mg.syx --hot # never cold boots; fails if no saved state fits
mise run emu:ctl step 1000000
mise run emu:ctl frame out.png
mise run emu:ctl tap 25 --hold 1000000
mise run emu:ctl events halt underrun
mise run emu:ctl quit
```

Python (`tools/` on `sys.path`):

```python
from digi_client import Emu

emu = Emu.connect()              # or Emu.connect(run_dir="...")
print(emu.status())
emu.step(1_000_000)
print(emu.state_digest())
emu.close()
```

`Emu.spawn(*args)` starts a private daemon (binary from `DIGI_EMUD`) and quits it on exit;
`Emu.spawn_coupled(syx, ready_dir)` adds the coupled-audio flags.

## Not implemented

The design document lists these; no server method or client wrapper exists.

| Design item | Use instead |
|---|---|
| `scenario.run` | Startup flag `--scenario NAME_OR_FILE [--report PATH]`; the `scenario.done` event carries the report |
| replay | `inputs.recorded` then `input.schedule` |
| save / restore, counters | none |
| `cf_read` | `mem.cf.peek` |
| `capture.*` wrappers | `pcm.tap` with `action=capture` through `call` |
