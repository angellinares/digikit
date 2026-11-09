# Private DT2 directory iterator diagnostic

Emulator only, DT2 OS 1.17, explicit opt-in. Requires `core/expansion`, conflicts
with `core/runtime` and the other UI-loop proof adapter. Its hook claims the UI
loop so another owner cannot be combined. It never changes DSP code/wire counts.

The generated UI trampoline runs `catalog_tick` in the actual UI task before the
original queue pop, preserving its call ABI. Each request performs at most one
filesystem call: OPEN=1, NEXT=2, or SIZE=3 after a successful NEXT. These firmware
calls may block; this is no fixed-latency interface. No call runs from MIDI/audio.

Host writes only after an idle/acknowledged result, retains all buffers until
completion, clears `catalog_mail.result`, then publishes `command` last. Tick
clears command and publishes result=2 last. Mailbox words are command, result,
signed return code, diagnostic error, opened, EOF, call count, size. Error1 is an
unterminated 256-byte path, error 2 a sticky guard violation, error 3 invalid state
or command. OPEN failure invalidates the old iterator. NEXT after EOF returns 0
without calling firmware. A timeout or guard failure requires a fresh boot.

`catalog_state` wraps four state words in two canary words; `catalog_output`
wraps the zeroed 0x114-byte result likewise. All guards begin 0xa55a3cc3 and are
checked before and after calls, never repaired by tick. Raw output contains ID+0,
u16 type+4, directory ID+8, page+12, offset+16, NUL-terminated name[256]+20.
No output is valid after failure. This diagnostic claims no immutable snapshot,
filesystem lease, public SDK/API, production scheduling guarantee or hardware use.

The opt-in test requires an explicit prebuilt card image and passes it directly
to patched firmware.load. It never calls card.load or prepares a card. The daemon
uses its private overlay; stock RpcFiles creates a deterministic child and 257-byte file. Stock ReadDir is
the names/types oracle: raw type 1 is a directory, other types are files. SIZE
reads physical size; ReadDir reports max(physical−16,0). The proof explicitly
checks the 16-byte difference for these native files, retaining both observations.
Raw dot entries are recorded and omitted only for the ReadDir comparison.

Prepare with existing tools, with no implicit patchkit/daemon build:

```text
patchkit --root REPO build modules/core/adapters/file-catalog-proof
patchkit --root REPO build modules/core/adapters/file-catalog-proof --check
DIGI_CATALOG_EMUD=ABS/PREBUILT/digi-emud
DIGI_PATCHKIT_BIN=ABS/PREBUILT/patchkit
DIGI_CATALOG_CARD=ABS/PREBUILT/dt2.img
DIGI_PK_READY_DT2_117=ABS/READY-DT2-1.17
```

Run the named Vitest runtime file `tests/regression/runtime/file-catalog.test.ts`
with the repository's runtime configuration. Missing card input fails preflight.
Each case owns its daemon/run directory; 240s test,
120s patched-startup and 20s mailbox waits bound the proof. Existing withSession
startup/cleanup bounds also apply. Artifacts remain under `out/runs/file-catalog`.
The DT2 1.17 live proof passes, including root/child comparison, error/EOF behavior
and unchanged guards. Evidence and remaining limits are in
`docs/findings/93-sdk-file-catalogue.md`. It is not a public SDK service or a
hardware/timing/concurrent-mutation proof.
