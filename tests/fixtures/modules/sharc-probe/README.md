# SHARC probe (test)

An effect of the FX contract that measures what the SHARC+ core does for `RECIPS` and `RSQRTS`, and passes the audio unchanged. Each block it
sends one 32-bit word through the module step word (diagnostic word 25 of the DSP reply, `core/dsp-link`), 1500 words a second. The words form frames
(header with a sequence number, 12 payload words, checksum) that carry the result bits and the ALU status and sticky bits of every item of a fixed
input sequence. The sequence repeats from the start, with a run counter.

The inputs come from `tools/device/dkprobe_seq.py` (it writes `src/probe_seq.inc`; the host tool `tools/device/dkprobe.py` expands the same lists). The ColdFire
side, a service module that collects the words with the per-block callback of `core/dsp-link`, is `digikit/probe`. Plan: `docs/plans/debug-mode-and-probes.md`.
It runs on the Digitakt II and the Digitone II. It reads ASTAT and STKY, clears them before each item (ASTAT bits 0 to 13, all of STKY), and puts both back
at the end of every frame. It never changes MODE1.
