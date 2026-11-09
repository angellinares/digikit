# DSP facts (test)

An effect of the FX contract that measures the SHARC+ core it runs on and passes the audio unchanged. Each block it sends one 32-bit word through the module
step word (diagnostic word 25 of the DSP reply, `core/dsp-link`), as `test/sharc-probe` does, and the collector `digikit/probe` keeps it. The words form frames
(header, 12 payload words, checksum); a run is one pass over 143 items (about 0.35 s on a device) and a capture holds about 47 runs.

The items are defined in `tools/device/dkprobe_facts.py`, which also writes `src/facts_seq.inc` (the code of each frame: `uv run python -m tools.device.dkprobe_facts --write PATH`;
`tests/digikit/test_dkprobe_facts.py` checks that the committed include is current) and turns a capture into a table of facts (`derive`, `compare`). `src/dsp/dsp_facts.s` holds
the framing and the call prologue.

| Group | Items | Answers |
| --- | --- | --- |
| block | 8 records of the cycle counter (EMUCLK) at three consecutive module calls | real cycles per block and their jitter; how many blocks the DSP skips |
| cyc | reads of EMUCLK around a snippet, repeated 8 times (min, first, max) | cost of the pair of reads (`empty`), NOP, ALU, multiplier and MAC cycles, 16-bit against 48-bit encodings, hardware loop start and per-pass cost, taken and untaken branches, delayed jump, CALL and RTS, RECIPS with two Newton steps, RSQRTS with three, L2 loads and stores (through the L1 data cache), free L1 SRAM, six lines that conflict in one cache set against six that do not (is L2 data cached), DDR reads |
| val | one instruction on given inputs, with ASTAT and STKY cleared first | FIX of NaN, infinity and overflow with ALUSAT clear and set, denormal and NaN inputs of float add, multiply, MIN, MAX, COMP, ABS (the AI flag), fixed-point overflow of add, subtract, negate, ABS with ALUSAT clear and set, fractional and integer multiply overflow |
| reg | a register read twice, and the cycles of the read | IMASK, IMASKP, IRPTL, LIRPTL, MODE2, MMASK, TPERIOD, TCOUNT, SHDBG_REVID, SHDBG_CORE_ID, SHDBG_OSPID, CMMR_SYSCTL, SHL1C_CFG, SHL1C_CFG2, the 16 SHL1C range registers (RR0 to RR7: start and end), the prefetch no-cache range, CMMR_GPERR_STAT, the three power registers, PFM_CFG |

What the module changes: ASTAT and STKY (saved at the start of each frame, written back at its end), MODE1 for the eight saturation items (ALUSAT set and interrupts off for
a few instructions, then the saved value written back), the module's own state (80 KiB: the cache test buffer) and, as a read and write of the same value, L1 SRAM at `0x2e0000`
(free in both images, finding 100). It reads DDR at the normal-word alias of byte `0x80700000` and never writes it. The DDR reads are the last item of a run: if the DSP stops there,
every other item has arrived, and `dkprobe run` reports the stop.

Run on a device or the emulator: `mise run dk:probe -- --port Digitakt --modules test/dsp-facts --capture out/probe/facts-dt2.json`. Plan: `docs/plans/debug-mode-and-probes.md`.
