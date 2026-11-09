# Sample publication proof fixture

`test/sample-proof` is an emulator test machine for the bounded sample-bank API.
It declares one module-owned bank of 8,192 mono float32 frames. Its four-word
initialized descriptor starts at READY=0; the runtime publishes READY after the
payload and immutable view have been acknowledged. The bank stays owned until
restart. This is a transport and playback proof, not a product sampler.

Upload the synthetic mono 48 kHz WAV through the existing Samples backend before
requesting a load. The fixed source is `/proof/test` on DT2 and
`samples/proof/test` on DN2. DT2 acquisition requires no simultaneous Transfer
writes. Native input must be complete canonical mono PCM16 at 48 kHz, nonempty,
and at most 8,192 frames (16,464 bytes including header and trailer).

The fixture registers two test-only service commands with empty request bodies:

- `0x3e`: queue the fixed path; service status is the runtime load result.
- `0x3f`: return one signed big-endian 32-bit sample status in the reply body.
  Service status is zero. Values are UNUSED=0, QUEUED=1, LOADING=2, READY=3, or a
  negative runtime error.

START and END are frame indices from 0 to 127; END=127 selects the file end.
REVERSE values at least 64 select reverse playback. Note-on latches these bounds
and the immutable published source. Playback runs at one source frame per output
frame and ends without looping. Before READY, a note outputs silence and DONE.
The render ceiling is 1,500 instructions; the aggregate note-on plus render
ceiling is 1,700. These are guard budgets, not measured hardware timing.

The consumer bridges the machine adapter's JUMP entry to the SDK block routine's
CJUMP frame: it reserves a word below the adapter's exclusive stack boundary,
saves the module's I12/I13 return registers in kernel-preserved registers, then
restores both registers and the exact stack pointer after the call. The bridge
stays in this test fixture; it does not establish a general module-call API.

`tests/test_sample_proof_playback.py` assembles the actual consumer and checks
forward, trimmed, reverse, empty and unready playback, completion, parameter
latching and retriggering. DT2 uses a 40-word adapter stack frame; DN2 saves its context in static data
and enters the module without a stack frame. Tests use byte-alias stack pointers,
adapter-frame sentinels and sequential keep-register checks without resetting
the context between calls. An additional real-adapter test observes render-entry
I7 and checks the permitted module stack against that measured boundary, then
runs the actual consumer with store-ownership checks enabled. Run it from the repository root:

```sh
uv run pytest -q tests/fixtures/modules/sample-proof/tests/test_sample_proof_playback.py
```

The TypeScript runtime scenario at
[../../../regression/runtime/sample-playback.test.ts](../../../regression/runtime/sample-playback.test.ts)
uses prepared module artifacts and preflighted coupled daemons. It uploads a
synthetic 4,096-frame WAV, checks exact decoded DSP bank bytes and the descriptor,
checks replacement refusal, and captures silence before READY and sound after
READY and retriggering on both emulators. Preparing artifacts and running that
scenario does not authorize hardware use. The scenario's assertions are proof
requirements; their presence alone does not establish a successful device run.
