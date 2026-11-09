# Stereo PCM16 kernel and the adapter's byte-alias frame

**[D]** Found while building `digikit/shot` on the concrete SHARC+ core through `core/dsp-voice` (model tests, no emulator). Fixed in the kernel.

- The adapters enter a module with a byte-alias `I6` (byte addresses, `0x2009...`; see `tests/fixtures/modules/sample-proof`, "actual adapters
  enter through JUMP with byte-alias I6"). A `DM(-n, I6)` offset counts words (4 bytes each); pointer arithmetic on `I6` or on a state pointer
  counts bytes.
- `_dk_sample_pcm16_stereo_linear` (`sdk/dsp/sample_pcm16_stereo.inc`) keeps the right channel's copy of the cursor on the stack, at word slot 11.
  It first computed the address as `I6 - 11`. Under the real adapters that is 11 bytes below the frame, not 11 words: the right call read a
  misplaced cursor, finished at once, and R was silent. L was correct. The kernel test used a normal-word frame, so it passed.
- Fix: the kernel computes `I6 - 44` (11 words in bytes), the convention of the real adapters. A normal-word `I6` is no longer supported. The
  rigs match the adapters: `tests/patchkit/test_dsp_sample_stereo_kernels.py` and the private exclusion and frame-fetch tests enter with a
  byte-alias `I6` and `I7` (`4 * normal-word address`).
- Guard: `test_right_channel_is_not_silent_under_the_adapter_byte_alias_frame` and the parametrised kernel test fail on the old `I6 - 11`
  (40 of 47 tests fail with it).
- SHOT uses this kernel for every sample. A mono sample plays with `base_r = base_l`. The earlier workaround (two mono calls over a state copy of
  the cursor) is removed.
- Related adapter facts used by SHOT: the state pointer (`R4`) and `in->out` are byte addresses; a `dk_strack` record and the PCM planes are
  normal-word addresses (`LSHIFT BY -2` of a data symbol gives one).
