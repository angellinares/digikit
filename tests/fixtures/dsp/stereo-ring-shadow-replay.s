// Private synthetic backings; no claim about production ring allocation.
#include "dsp/call.inc"
.SECTION/PM seg_pmco;
.GLOBAL _prepare_ring;
_prepare_ring:
    I4 = 0x08040000; I0 = 0x08040100;
    // Keep history0, coordinates13/14, backing16 and fill17 private.
#define COPY_RING(n) R0 = DM(n, I4); DM(n, I0) = R0
    COPY_RING(1); COPY_RING(2); COPY_RING(3); COPY_RING(4); COPY_RING(5);
    COPY_RING(6); COPY_RING(7); COPY_RING(8); COPY_RING(9); COPY_RING(10);
    COPY_RING(11); COPY_RING(12); COPY_RING(15);
    RTS;
#define RING_INIT(state, label) \
    R4 = state; R8 = 0x473b8000; \
    DK_CALL(_ring_init, label); RTS
.GLOBAL _init_left_ring;
_init_left_ring:
    RING_INIT(0x20100000, _ilr);
.GLOBAL _init_right_ring;
_init_right_ring:
    RING_INIT(0x20100400, _irr);
#define RING_STAGE(state, audio, label) \
    R4 = state; R8 = audio; R12 = audio; \
    R0 = 32; DM(I7, M7) = R0; \
    DK_CALL(_ring, label); MODIFY(I7, 1) (NW); RTS
.GLOBAL _left_ring;
_left_ring:
    RING_STAGE(0x20100000, 0x20104000, _lr);
.GLOBAL _right_ring;
_right_ring:
    RING_STAGE(0x20100400, 0x20104400, _rr);
