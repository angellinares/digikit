# UI surfaces for mods on DT2 1.17

Digitakt II OS 1.17 only. Marks: **[D]** read or measured once, **[O]** open. Nothing here is **[V]**. What a mod with its own
mode (knobs, keys, LEDs, screens, a PERSONALIZE row, per-frame values) needs beyond [finding 71](71-ui-components-1-17.md).
Rows come from a patched-build session (closed PR #215); function sizes, names, call sites and the LED and NameView points
were re-read in the 1.17 database on 2026-11-11. Values marked "session" were measured there and not re-read.

## 1. Calling convention **[D]**

The firmware returns pointers in d0. GCC's m68k ABI reads a pointer result from a0, so a C prototype that returns a pointer
reads a stale a0 (the `UIStates` getter returned the view controller and the UI froze). Declare such routines as returning
a 32-bit integer.

## 2. Input

| Input | Handlers get (session) |
| --- | --- |
| Encoders A-H | ids 1-8 |
| LEVEL/DATA | encoder id 9; pushed, key code 49 |
| Pushes A-H | key codes 41-48 |

Key event flags (event + 16), session: bit 0 is "down". Press `0x01` (`0x03` with FUNC held), repeat `0x09` (`0x0b`),
release has bit 0 clear: `0x10` for most keys, `0x00` for FUNC and PAGE. After FUNC + NO (pattern reload) FUNC's release
is not passed on. The key-down query ([finding 58](58-panel-input-path.md)) knows.

Views on top of the stack (vtable pointer values, session; the database shows the table header 8 below):

| Held or state | View | DT2 vtable |
| --- | --- | --- |
| FUNC | `QuickMuteMenuView` (base `MuteMenuView`) | `0x401f9208` (table `0x401f9200`) **[D]** |
| TRK | `TrackSelectionView` | `0x401f9ec8` (table `0x401f9ec0`) **[D]** |
| main screen, parameter pages | `HelpBubbleView` | `0x40216f14` (table `0x40216f0c`) **[D]** |

A menu, dialog or popup pushes another view.

### Encoder filter **[D]**

Every stock knob turns through one filter: after rest it swallows the first counts, then moves more per count the faster
it turns. [Finding 03](03-ui-and-panel.md) has it on DN2 1.11.

| | DT2 1.17 |
| --- | --- |
| `filter(block, knob, count, cfg, pushed)` (62 call sites) | `0x40111cda` |
| `list_filter(block, event, cfg)`: count x8 when pushed, then `filter` | `0x40111e42` |
| Block constructor `EncoderFilter::ctor_dtor` (starts the refill timer) | `0x40111b68` |
| cfg of the 0-127 parameters (LEV, VOL, sends): raw units, 256 a step, count x2 pushed | `0x44d2e654` |
| cfg of modulation depths: 1/256 steps, count x12 pushed | `0x44d2e670` |
| cfg of MODULATION SETUP destination knobs: steps | `0x44d2e600` |

Encoder event: knob at + 12, count + 16, "pushed" byte + 20. A parameter's cfg is the word at + 16 of its descriptor
(`0x4293c040 + 0x44 id`). Session: a mod knob through `filter` with the parameters' cfg moved by the same steps as the stock
LEV knob over 38 turns.

## 3. Per-frame values **[D]**

The frame interrupt (vector 191) builds a block of per-track values every frame: the glide writes it from the mirror, then
the stock modulators add to it.

| | DT2 1.17 |
| --- | --- |
| Glide `(mirror)` to block (`param_smoother`) | `0x400d9406`; mirror `0x80003340`, block `0x80005b50` |
| Per-track modulators `(block, ...)`, 6-byte `jsr` at `0x4002e90c` | `0x400d9508` |
| Track t, value j in the frame (session) | `0x80005b72 + 142 t + 2 j` |

A hook at the call site adds to the block before the stock velocity, second source and LFOs, without touching the kit or
the mirror. Session: the frame values moved by the expected offsets; kit and mirror did not.

## 4. Key LEDs

The LED frame `0x4003117c` runs when the view controller's byte + `0x21` is set **[D, session]**. It clears every LED with
`jsr 0x4003098e` at `0x400311a2` (6 bytes; the next instruction is `move.l d2,-(sp)`) **[D]**. Then the views set theirs, and
the finish turns off what nobody set. The setter `(mgr, logical, layer, value)` is `0x400308e0` ([finding 19](19-sequencer-and-leds.md)).
It refuses an LED whose layer is already set, so the first setter wins: a hook after the clear sets LEDs over the views'.
Layer 2 lights with the value, layer 1 turns off **[D, session]**.

Value byte, two readings **[O]**:

| | Finding 19 | PR #215 session |
| --- | --- | --- |
| Basis | inference from call-site constants and wire values; no panel firmware | trig key n lit with `(n - 1) << 4 \| 1`, read by eye on a Digitakt II by its owner (not checked by a second agent) |
| High nibble | colour mask: `0x10` red, `0x20` green, `0x40` blue, `0x80` a fourth channel; `0x71` white | palette index: `0x01` off, `0x11` red, `0x21` green, `0x31` deep blue, `0x41` yellow, `0x51` light blue, `0x61` purple, `0x71` white, `0x81` orange |
| Agrees | `0x11` red, `0x21` green, `0x71` white, `0x01` off | same |
| Differs | `0x41` would be blue, `0x31` red + green | `0x31` deep blue, `0x41` yellow |

Finding 19 is not edited. Lighting `0x31` and `0x41` on a real unit, checked by a second person, would settle it.

## 5. Text, fonts, header **[D]**

- `draw_fmt(bitmap, font, x, y, flags, fmt, ...)` is `0x40113dc8`. `y` is the text's bottom row from the screen bottom; flags
  bit 1 centres on x (session).
- Fonts (session): main `0x44d2e70c`; narrow (3-pixel letters) `0x44d2e6f4`. Stock lists and page boxes use the narrow one.
- Header message (finding 71's toast): `UIStates` getter `0x401998c0` (`*0x44f370c0`), message string at + 72 (session).

## 6. PERSONALIZE value rows **[D]**

The builder `PersonalizeMenuView::ctor_dtor` (`0x4009daec`) makes LED INTENSITY as a MenuItem with label, action, draw and adjust:
- Draw invoker `0x4009c722` gets `(functor, item, bitmap, x, y)` and draws the value string with `0x40113fd4` as
  `(bitmap, font 0x402a9be0, x + 19, y + 2, 2, 0, 0, text)`: right-aligned.
- Adjust `0x4009e2ae` gets `(functor, item, delta)`, clamps the value and redraws with `View::invalidate(menu + 0x38)` (`0x4011b126`).

Session: a row of the same shape draws like LED INTENSITY. LEFT and RIGHT call adjust, YES calls the action; the LEVEL
encoder does not reach the rows in this menu.

## 7. NameView constructors **[D]**

Finding 71 gives ctor `0x40117ac2`; the session used `0x40117c70`. Both are right and they are two functions:
- `0x40117ac2` is `NameView::ctor_dtor`, 8 arguments `(this, prompt, initial, number, maxlen, show_number, number_fmt, done)`.
  Callers: `0x4007f210`, `0x40084f50`, `0x400850dc` and the wrapper below.
- `0x40117c70` is a wrapper with 7 arguments `(view, title, name, number, len, show_number, done)`. It builds the number
  format from the string at `0x4023e128` (`%03d`), copies the strings, and calls `0x40117ac2`. 21 call sites, among them
  `MidiParameterPageView::vfunc_2` (`0x4006884a`, the MIDI parameter name).

`done` is a `std::function<void(std::string)>`; its invoker gets a pointer to the string's character pointer. The view is made
as `std::make_shared` makes it: operator new `0x4011ebd0`, 0xb0 bytes, control block vtable `0x401ef190`. Push:
`0x4011c08a(Brain + 0x40, &shared_ptr, 0)` (finding 71: `0x4011c08a` is `ViewController::push`, and `0x4011b22a` is a
thin wrapper over it). A std::string is one pointer: built with `0x401e7e7a`, freed with `0x401e63ca`. Session: a mod's
own title and name round-trip.

## Open **[O]**

- LED palette: finding 19 against the session table (section 4).
- How stock MODULATION SETUP marks the selected column.
- All of this on DN2 1.12 (PR #215 held its addresses; not re-read here).
