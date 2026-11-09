// Private source-continuation proof. Original b8251b is loaded at runtime.
// SISD entry; the positive path owns SIMD companion scratch and clears PEYEN.
#include "dsp/call.inc"
.SECTION/PM seg_pmco;
#define SOURCE_STAGE(state, audio, label) \
    R4 = state; R8 = audio; R12 = audio; \
    R0 = 32; DM(I7, M7) = R0; \
    DK_CALL(_source, label); MODIFY(I7, 1) (NW); RTS
.GLOBAL _left_source;
_left_source:
    SOURCE_STAGE(0x20100000, 0x20104000, _lso);
.GLOBAL _right_source;
_right_source:
    SOURCE_STAGE(0x20100400, 0x20104400, _rso);
