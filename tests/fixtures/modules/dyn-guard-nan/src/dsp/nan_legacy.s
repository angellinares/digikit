// Legacy form, kept for the rig test of core/dsp-voice rows without a note_on pc (tests/patchkit/test_dsp_guard.py; not linked into the module, the module links nan.s): a DSP routine of the core/dsp-voice ABI that outputs NaN while its voice is active (fixture of the runtime guard,
// docs/findings/51-pluck-tune-guard.md). An idle voice renders silence. Entry: R4 = voice record (byte +0x1b8 ACTIVE), R8 = output block.

.SECTION/PM seg_pmco;
.GLOBAL _gn_render;

_gn_render:
    I0 = R8;
    I1 = R4;
    MODIFY(I1, 440);                      // record byte +0x1b8: ACTIVE
    R9 = DM(I1, M5);
    R0 = 0xff;
    R0 = R9 AND R0;
    R1 = 0x7fc00000;                      // a quiet NaN
    IF EQ R1 = PASS R0;                   // an idle voice: 0
    LCNTR = 32, DO _gn_loop UNTIL LCE;
_gn_loop: DM(I0, M6) = R1;
    JUMP (M13, I13);
