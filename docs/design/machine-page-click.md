# Click on a machine's knob

Status: built in the lane `page-hook`, 2026-11-02; static only, not run in the emulator. Marks: **[D]** read once in the image (DT2 OS 1.17, DN2 OS 1.12), **[O]** open until the emulator proof.

A click on a knob of a digikit machine's page (press and release of the encoder, no turn between) can run a function of the machine's module. Press and turn stays the stock coarse step. The first user is SHOT: its SMP knob opens the sample pool list.

## Contract for a machine module (SHOT)

```c
#include <dk/machine/machine.h>

DK_PARAM(shot, 0, shot_smp, "Sample", "SMP", 0, 15, 0);   // word 0: knob B on both devices
DK_PARAM_ACTION(shot, 0, shot_smp_click);                 // clicking the knob of word 0 calls shot_smp_click

void shot_smp_click(uint32_t track)                       // global, not static
{
    dk_sample_picker_open_pool(track);                           // opens a <dk/platform/ui.h> menu over the page
}
```

- `DK_PARAM_ACTION(MACHINE, WORD, FN)` declares `void FN(uint32_t track)` and one `dk_desc_param_action` record (`kind = DK_DESC_PARAM_ACTION`, `machine`, `word`, `fn` = the symbol). `WORD` must have a `DK_PARAM` of the same machine. A parameter has at most one action.
- `FN` must be a global function of the module's ColdFire object. `patchkit pack` finds it by its symbol; a static function has no symbol and the pack stops with a message.
- The runtime calls `FN(track)` in the UI task, from the UI loop pass that follows the release of the encoder (not inside the key handler). `track` is the page's track, 0 to 15.
- `FN` may open a `dk_ui` menu. The page stays under it: NO, or a pick followed by `dk_ui_close`, returns to the page. `FN` does not return to the page itself.
- A knob without an action, a stock knob and a click with a turn behave as stock. Patched-in modules (not `dynamic`) cannot declare an action: `patchkit` refuses it.
- From the click flow a module can read and set parameters of its own machine: `param_get(track, word)` returns the raw value (unit * 256) and `param_set(track, word, raw)` writes it through the stock setter of a knob turn (API 1.6, `<dk/platform/param.h>`). They work in the UI task from the callback until the page gets its next key or turn, so the list the callback opened can set the value when the user picks. `word` is the n-th `DK_PARAM` row of the module in declaration order (one machine per module for now); a foreign word or a track above 15 is refused; a held TRIG, FUNC or track-mode key (logical keys 0x10, 0x11, 0x16 of `cf.input.fn.key_down`) refuses the write. The runtime keeps the clicked view next to the track (`Held` in `paramapi.rs`). Static source: the stock turn path of `ParameterPageView` (`FUN_4006cb14`): getter = view vtable 0x54 (view, code, -1), parameter object = view vtable 0xc4 (view, code, track), setter = `cf.param.fn.set_value` (DT2 0x4003b570, DN2 0x40037326) **[D]**; the emulator proof is open **[O]**.
- Actions need the runtime with this feature. A module with an action is refused by an older runtime (`E_FORMAT`: it does not know the record kind).
- Which encoder a word sits on follows `param_slots` of the device profile. With DT2 `[1,2,4,5,6]` and DN2 `[1,2,4,5,6,0,3,7]`: word 0 is knob B (encoder 1, key code 0x2a), word 1 C, word 2 E, word 3 F, word 4 G; DN2 word 5 is A, word 6 D, word 7 H. The runtime does not read `param_slots`: it asks the page which parameter code sits under the encoder (below), so a change of the layout needs no change here.

## How it works

```text
 key event / turn event of the machine page
        |  hook at the entry of the page's vfunc_2 and vfunc_17 (both devices), then the stock handler runs unchanged
        v
 dkrt_page_key / dkrt_page_turn          (modules/core/runtime/src/rs/src/pageclick.rs; one test of ARMED when no action exists)
        |  Detector: press of encoder e -> pending; a turn -> turned; release of e, same view, no turn -> click(e)
        v
 code = view.vtable[47](view, e)         the page's own encoder -> parameter code (what the stock list opener calls)
 action = ACTIONS[code - param_first]    set by registry::apply from the ACTION record, cleared by registry::remove
        |  none: stock click, nothing queued
        v
 PENDING = (callback, track)             track = cf.page.fn.view_track(view[0x74])
        |  next UI loop pass (dkrt_tick)
        v
 callback(track)                         in the module
```

- The hooks never consume an event. The stock handler runs after them, so the stock behaviour (also for a click that we use) is unchanged. The page sees the release as it does today.
- Run-time parameter rows have the codes `P_DYN0 + k` (`registry::apply`); a machine row (DT2) or page row (DN2) holds these codes, and the page's `vtable[47]` returns them. So the lookup is by parameter row, not by knob letter, and it is the same on both devices.
- The record: `register` table kind 5, `ACTION`, 0x10 bytes: `u32 param` (index of a PARAM record of the same file), `u32 offset` (image offset of the callback; the runtime adds the load address). Format: `docs/design/digikit-registry.md`. It takes no row (`dkneeds` counts nothing for it). An old file has no such record and loads as before.

### Click rules (`Detector`)

| Event | Effect |
| --- | --- |
| press (flags 0x01, no repeat 0x08, no chord 0x02) of key 0x29 + e | pending = e on this view, turned = false |
| press with chord | no pending |
| repeat (0x08) | ignored |
| turn of any encoder on the view while pending | turned = true |
| press of any other key | pending cleared |
| release (0x10) of 0x29 + e, no chord | click(e) if pending == e, same view, not turned; pending cleared |
| release of another encoder, or on another view | no click; pending cleared |

Host tests: `modules/core/runtime/src/rs/src/pageclick.rs` (`tests`); packer side: `desc.rs`, `register.rs`, `dkm.rs`, `typed.rs` (unit tests) and `packages/dkneeds`.

## Static answers (what the hooks rely on)

**Handlers [D].**

| | DT2 1.17 | DN2 1.12 |
| --- | --- | --- |
| class | `SourcePageView` | `MultiSourcePageView` (base `MachineParameterPageView`) |
| key handler, vfunc_2 | 0x400181fe | 0x40016480 |
| turn handler, vfunc_17 | 0x400184ea | 0x4001704e |
| entry (8 bytes, displaced by the hook) | `link.w a6,#-0x34; movem.l ...` | `lea (-0x24|-0x30,sp),sp; movem.l ...` |
| arguments at entry | 4(sp) = view, 8(sp) = event | the same |
| track getter | `FUN_4004abb8(view[0x1d])` = 0x4004abb8 | `FUN_40046bfe(view[0x1d])` = 0x40046bfe (same body) |
| anchors | `cf.page.fn.source_key`, `cf.page.fn.source_turn`, `cf.page.fn.view_track`; hooks `cf.page.hook.source_key`, `cf.page.hook.source_turn` (`devices/fw/page.toml`) | |

- Key event record: key code at +0xc (`FUN_40114984` on DT2, `FUN_401162f8` on DN2), flags at +0x10: 0x01 press (`FUN_401149dc`, `FUN_40116350`), 0x02 chord, 0x08 repeat (`FUN_401149e8`, `FUN_4011635c`), 0x10 release (`FUN_401149ac`). Encoder click keys are codes 0x29 to 0x30 (the DT2 handler computes `code - 0x29` and calls vtable +0xbc with it) **[D]**.
- Turn event: `FUN_40111a12(ev, -3)` (DT2) and `FUN_40113386(ev, -3)` (DN2) test `event[3] - 1 <= 7`; the encoder is `event[3] - 1` (`FUN_40111a3a`, `FUN_401133ae`) **[D]**.
- The page's `encoder -> parameter code` function is vtable slot 47 (offset 0xbc): `i32 (view, encoder)`. On DT2 it feeds the SAMP test (codes 0xcd, 0xf1, 0xd6, 0xe8, 0xdf, 0xfb open the stock sample list from a non-repeat press, `FUN_40017ed0`); on DN2 the turn handler calls it the same way (0x400170d8) **[D]**.
- The track: `FUN_4004abb8` returns `*(ref->vfunc_0x28()+8)` for the reference at `view+0x74`, or 0. The stock code passes it to `FUN_40035cca` (DN2 `FUN_40030e66`), which clamps it to 0 to 15 and uses it as the track index (stride 0x3c0): it is the track number **[D]**. The runtime passes it unchanged to the callback.
- DN2's `MultiSourcePageView::vfunc_2` handles key code 2 only (SRC, the preset name toast); every other key, the encoder clicks too, goes to the base `vfunc_2` (0x4005a214) and on to `ParameterPageView::vfunc_2` (0x4006787c). The hook at the entry of `MultiSourcePageView::vfunc_2` sees all of them first **[D]**.
- The hook entry has no live register: d0, d1, a0, a1 are free, the stack holds the stock arguments. `dkrt_pagekey_hook` (src/cf/tramp.s) pushes (view, event), calls the C function, pops, and continues in the displaced tail.

## What the emulator proof must check

1. Load SHOT (with the action declared) on DT2 and DN2, select it on a track, open its page.
2. Press the SMP encoder through core/input (`input_key` 0x2a: encoder 1 = knob B; with `param_slots` as above), then release it: the pool list opens over the page; NO returns to the page.
3. Press, turn the same encoder, release: no list; the value moves by the stock coarse step.
4. Press and release another knob of SHOT, a knob of a stock machine, and the SMP knob of a stock machine page: stock behaviour (DT2 SAMP of a stock sample machine still opens the stock list).
5. A turn of SMP alone (no press) does what it did before.
6. Repeat 2 on a second track and check the callback's `track` argument.
7. With no module that declares an action in the image: both hooks return at the `ARMED` test (no visible change).

Open **[O]**: the release flag on the device (0x10 in the emulator's input model); the vtable +0xbc call for an added machine returns the module's parameter code on both devices (the row fields are set by `registry::apply`, but nothing has called it yet); the click of an encoder reaches the page while the TRIG or FUNC key is held (chord: ignored by design).
