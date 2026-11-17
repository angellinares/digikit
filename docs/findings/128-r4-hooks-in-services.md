# R4 hooks-in-services spike, static part: can hooks move into a `.dks`

Roadmap item R4 ([sdk-runtime-roadmap.md](../plans/sdk-runtime-roadmap.md)), static part of the spike in [runtime-and-services.md](../plans/runtime-and-services.md) section 11.1. Static evidence only: module manifests, runtime sources, patchkit, findings 06 and 36. Nothing was run, so the best mark is [D]. DT2 OS 1.17 and DN2 OS 1.12.

## Answer

**SHARC hooks cannot move into a `.dks`. ColdFire hooks can, with open points.** Confidence: about 80% for SHARC, about 70% for ColdFire. [D]

- SHARC: the image keeps each SHARC hook site as a thin stub that jumps through a pointer slot (zero slot means stock). The dispatch code moves into the `machines` and `fx` services; rows and adapter data stay in the image. This is the design for section 6.1 of the plan and [r12-8-sharc-stubs.md](../plans/r12-8-sharc-stubs.md).
- ColdFire: the runtime installs service hooks at the main-OS task entry, after the card is readable.
- Dynamic checks wait for R8 whole-path spike.

## SHARC side

Evidence:

1. Every SHARC hook is an `op = "hook"` entry applied at patch time. Owners:
   - `core/dsp-voice`: `dsp.voice.hook.render_entry`, `dsp.frame.hook.frame_rx.unpack_call_a` and `unpack_call_b` (DT2), the per-stage call hooks, `dsp.voice.hook.pan_slot` and `pan_pre` (`modules/core/adapters/dsp-voice/module.toml:78-467`, 28 hook entries).
   - `core/fx`: `dsp.fx.hook.entry` and `dsp.fx.hook.send` (`modules/core/adapters/fx/module.toml:63,68`).
   - `core/dsp-link`: `dsp.link.hook.block_tail` (`modules/core/adapters/dsp-link/module.toml:79`).
   - `core/sample-pool` on DN2: `dsp.cache.hook.cache_enable` (DSP), `cf.link.hook.boot_wire_size` and `cf.link.hook.reply_copy` (ColdFire) (`modules/core/adapters/sample-pool/module.toml:87,93,106`).
2. The ColdFire boot routine uploads the whole DSP image (section 7) to the SHARC before any of our code can read the card: DT2 1.17 `FUN_400cd0d8`, DN2 1.12 `FUN_400cf4b0`. It sends the depacked section byte by byte, does not parse it, and has no timeout (`docs/findings/06-sharc-engine-and-startup.md:5485`; `docs/findings/36-boot-load-spike.md:18`). The stock DSP code is therefore running before a service can load.
3. A service would need live writes into running stock DSP code. This is unproven:
   - SHARC instructions are 48 bits. A transfer writes 32-bit words, so a write is not atomic against a fetch. [O]
   - The DSP cache invalidate after a write has not run anywhere; the emulator does not model the DSP caches (`modules/core/adapters/dsp-link/README.md:74`). [O]
   - The `dsp-link` area limit: it writes only inside its own area today (same line). Writing stock code is outside what it does.
   - The block-tail hook is the transport itself (`dsp-link` `_dl_back`, `module.toml:66`). The transport cannot install its own hook.
4. Module DSP code already loads at run time and is relocated by the runtime. So the code behind a hook can move; only the hook site cannot.

Design:

- The image keeps every SHARC hook site as a thin stub. The stub loads a pointer slot and jumps through it. A zero slot runs the stock path, so a device with no service behaves as stock.
- The dispatch code moves into the `machines` and `fx` services. They load at run time and write their entry into the slot.
- The `DK_SPAN` mechanism is image-owned; its call sites sit in the service dispatch, so content modules need none. The DSP stack guard is a later item.
- Cost to check in the R12-8 slice: stub overhead per block.

## ColdFire side

Evidence:

1. The first point where the card is readable is the main-OS task entry: `cf.rtos.hook.main_os_task` (`modules/core/fw/inc/dk_fw_rtos.inc:3`, used by the runtime at `modules/core/runtime/module.toml:95`). The block read needs a mutex and a semaphore (finding 36 [D]).
2. Patchkit already records the stock bytes: each patch carries `old_sha256` and the site length (`packages/patchkit/src/apply.rs:1322`). The installer can check them before it writes.
3. The tools for a safe write exist in the runtime: `dkrt_irq_mask` (status register to 0x2700) and `dkrt_flush` (`cpushl` per 16-byte line) in `modules/core/runtime/src/cf/tramp.s:25-56`.

Install sequence per site: check stock bytes against the plan, mask interrupts, write the 6-byte `jmp.l`, flush, restore interrupts. A mismatch refuses the service.

Cannot move:

- The runtime's own hooks: boot, UI loop, MIDI receive, page key and turn.
- The exception hook word.
- The loader and `cf.expansion`.
- Any site that runs before the main-OS task entry.

Open [O]:

- Row reservations in firmware tables (`[[section]]` append): can they be made at run time before the firmware reads the table?
- Tasks already running at install time may execute a site during the write. Interrupt masking does not stop other tasks.
- Hardware cache behaviour (ACR0 copyback): the emulator keeps caches coherent and cannot prove it.

## Deferred to R8 whole-path spike

- A pointer-slot stub reaches a service handler on both devices; golden hashes and PCM pins are unchanged at a zero slot.
- Stub overhead per block (`dsp_thread_insns_per_audio_s`).
- A ColdFire hook table write equals patchkit's bytes; a stock mismatch is refused; safe mode leaves sites stock; a second claimant is refused; no task executes a site during the write.
- Cache flush on hardware.
