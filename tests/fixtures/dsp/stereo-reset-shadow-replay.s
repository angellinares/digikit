// Bounded helper reset mirroring, not event-name or global writer ownership.
// b82440 is bound specifically to the b8265b held-sample record here.
#include "dsp/call.inc"
.SECTION/PM seg_pmco;
#define RESET_STAGE(fn, state, label) \
    R4 = state; DK_CALL(fn, label); RTS
.GLOBAL _reset_left_snapshots;
_reset_left_snapshots:
    RESET_STAGE(_reset_snapshots, 0x20100000, _rls);
.GLOBAL _reset_right_snapshots;
_reset_right_snapshots:
    RESET_STAGE(_reset_snapshots, 0x20100400, _rrs);
.GLOBAL _reset_left_nine;
_reset_left_nine:
    RESET_STAGE(_reset_nine, 0x20100000, _rln);
.GLOBAL _reset_right_nine;
_reset_right_nine:
    RESET_STAGE(_reset_nine, 0x20100400, _rrn);
.GLOBAL _reset_left_prepared;
_reset_left_prepared:
    RESET_STAGE(_reset_prepared, 0x20100000, _rlp);
.GLOBAL _reset_right_prepared;
_reset_right_prepared:
    RESET_STAGE(_reset_prepared, 0x20100400, _rrp);
.GLOBAL _reset_left_held;
_reset_left_held:
    RESET_STAGE(_reset_held, 0x20100000, _rlh);
.GLOBAL _reset_right_held;
_reset_right_held:
    RESET_STAGE(_reset_held, 0x20100400, _rrh);
