# 112. DT2 1.17: the stock pan stage sums a voice pair unless its gate is 1

Trigger: the SHOT stereo machine on DT2 played a sum of L and R on both outputs, although the adapter wrote R into the odd voice buffer. Marks: **[D]** measured or read once, **[O]** open.

## 1. Measurement **[D]**

SHOT stereo machine in the emulator. Sample L = 440 Hz, R = 660 Hz, held note. DSP peeks and SHARC_WWATCH.

- The even voice buffer `0x8045b3c0` held pure 440 Hz. The odd voice buffer `0x8045b440` (+0x80 bytes) held pure 660 Hz.
- The pan-stage output planes `0x252df8` and `0x252e78` were bit-identical: outA = outB = 0.086 * (inA + inB). Least-squares fit, residual about 1e-19.
- The pointer table `0x24ef2c` holds 32 distinct voice buffers (`0x8045b3c0 + 0x80*v`).

## 2. The pan stage **[D]**

Pan stage `FUN_1c642a`, loop `0x1c7040` over 16 voice pairs. At `0x1c7050` it reads a gate word from table `0x24f16c + 8*pair`:

- Gate 1: stereo-pair path `0x1c705f..0x1c7155`. Plane A = inA * level_A * cos-law(panA) from table `0x8055c440`. Plane B = inB * level_B * sin-law(panB) from `0x8055c640`. No cross term, so pan acts as a balance.
- Gate 0: matrix path `0x1c71ec..0x1c7350`. Audio loop `0x1c7329`: outL = gL(panA)*A + gL(panB)*B, outR = gR(panA)*A + gR(panB)*B. Both voices are panned and summed.
- Other values: the pair is skipped (`0x1c7158`).

## 3. Where the gate comes from **[D]**

- At the end of each pair iteration (`0x1c7158..0x1c7167`) the gate is copied from word +0xd8 of the even voice's param record. Record base `0x2506ec`, stride 0xdc bytes. The even voice of track t is at `0x2506ec + 0xdc*2t + 0xd8 = 0x2507c4 + 0x1b8*t`. The gate lags one frame.
- The only writer of +0xd8 is the stock sample note-on `DM(I5+54) = R2` at `0x1c335b` (block `0x1c3329` in `FUN_1c3289`). It writes 1 for every sample note, mono or stereo.
- The unpack `FUN_1c24e9` never writes it.
- The slot stereo byte `0x405ba368[slot]` is a load-time flag only ([finding 109](109-dt2-stock-pool-borrowing.md)).

## 4. Fix and result **[D]**

Commit 6eb7b8af: the dsp-voice adapter writes 1 to word +0xd8 of the even voice's record (`0x2507c4 + 0x1b8*track`) every block for a machine that returns DK_STEREO. Mono machines leave it. The record base is the anchor `dsp.voice.value.param_record_base`, the literal `I5 = 0x2506ec` of the per-voice loop in `FUN_1c2b24` (`sharc_literal`, offset 0x1bf). The adapter adds the stride and +0xd8 itself.

Result (`modules/machines/shot/tests/shot-dt2.device.test.ts`): L 440 Hz 0.0763, R 660 Hz 0.0763. Cross tones 7e-9 and 2e-8. A second sample (330/495 Hz) is split with the other tone about 55 dB down.

## 5. Open

- **[O]** The conditions that snap the gain ramp when the gate is 1 (`0x1c70e2..0x1c7104`) are not analysed. Observed only on track 0.

[Finding 96](96-stereo-machine-processing.md): "the pan stage scales the pair's two buffers independently" holds only on the gate-1 path.
