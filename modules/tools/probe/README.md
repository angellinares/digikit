# digikit/probe

A ColdFire service module that collects the words of a DSP probe module (`test/sharc-probe`) and gives them to a computer. It needs `core/dsp-link` and the
per-block callback `dl_set_tick_cb` of the runtime.

`dkm_init` sets the callback, which runs in the block interrupt every block with the copy of the previous DSP reply. When a module ran in that record,
the callback keeps diagnostic word 25 (the module step) in a ring of 24,576 words. Every reply record carries the step word of its block (the block tail of `core/dsp-link` copies the module's `_dk_step` into it), so every block gives a word, 1500 a second; one run of the probe module (23,436 words) takes about 16 s. The ring stops taking words when it is full (overruns are counted). RESET empties it.

| Command | Number | Body | Reply |
| --- | --- | --- | --- |
| STATUS | 0x32 | none | 16 u32 (big endian): format (3), callback calls, new records, records missed, repeats, records with no module call, words in the ring, overruns, ring size, words checked, frames ok, sequence errors, checksum errors, last frame number, last heartbeat, synced |
| READ | 0x33 | u32 first word, u16 count (at most 200) | u32 words in the ring, u32 first, the words |
| RESET | 0x34 | none | status 0 |

The commands lie in the `probe` range of `sdk/sdk.toml`. STATUS checks the frames (header, size, frame number, checksum) a bounded number of words per request.
The host tool is `tools/device/dkprobe.py`. Unloading the module clears the callback.
