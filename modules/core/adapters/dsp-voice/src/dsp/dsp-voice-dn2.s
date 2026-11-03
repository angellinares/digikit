// core/dsp-voice on the Digitone II: render hook for machine types the FM engine does not know.
//
// The Digitone II engine (FUN_1c8ef1) is a stage pipeline over 16 voice slots: slot v has a 32-float voice buffer
// (48 kHz, one DSP block) at `0x804acf90 + 0x80 v` and a frame slot (pitch halfword 1 + v = note << 8, velocity 26 + v
// = velocity << 8, machine type 74 + v). The FM synthesis of each voice writes its buffer; the stages that follow
// (gain, amplitude envelope and its gate, filter, overdrive and the rest of the track chain) process the mono buffer.
// FUN_1c9aea then applies independent pan gains to produce two 32-sample channel planes for the sends and master mix;
// the configured rate/block is 48000 Hz / 32 samples (finding 96). A slot whose machine type is
// above 4 is skipped by the synthesis like MIDI and its buffer holds nothing, so a routine that fills it makes the
// slot sound through everything after the synthesis (findings 06, "Per-voice hook on DN2").
//
// The hook is FUN_b8251b, a 55-instruction helper the engine calls once per voice buffer after the synthesis, with
// R15 = 16 - v (a count that runs 16, 15, ... 1), R8 = R12 = the voice buffer, R4 = the voice's control block. patchkit
// overwrites its first instruction (`I7 = MODIFY(I7, -18)`, 48 bits) with `JUMP _dv_entry`. This file saves every
// register it touches in `_dv_save`, runs the routine registered for the slot's type, restores the registers, executes
// the displaced instruction and continues in the stock helper at its second instruction. A patched image with no
// registered type renders exactly the stock audio. The helper has a second user on another path (a tail jump from
// FUN_b8258f); that call has R8 != R12 and is passed through.
//
// Routine ABI (entered with JUMP (M13, I12); I13 holds the return address, the routine ends with JUMP (M13, I13);
// ordinary CALL/RTS pairs for its own subroutines are fine):
//   in   R4 = the slot's voice buffer (32 floats, written in full), R0 = type, R2 = slot v (0..15), R12 = 32
//   may clobber  R0-R15, I0-I5, I12, I13, M1-M4 (saved and restored here); must keep I6-I11, I14, I15, M0, M5-M15,
//        the L registers and MODE1 (SIMD off).
//   The context registers are SET by this wrapper, never assumed. The firmware restores M5, M6, M7, M13, M14, M15, the L
//   registers and MODE1 per DSP context (blk69 context restore, finding 45), so their values at the hook are the context's,
//   not constants of the machine. Before a registered type runs, the wrapper saves them (`_dv_ctx`) and pins
//        M5 = 0, M6 = 1, M7 = -1, M13 = 0, M14 = 1, M15 = -1, L0-L5 = 0 (linear addressing),
//        MODE1: SIMD (PEYEN), ALUSAT and TRUNCATE clear, RND32 set (the startup value; round to nearest, 32-bit floats),
//   and restores the saved values when the routine returns. So a routine may use M5/M6/M7 as the constants above and
//   JUMP (M13, I13) as written, and it must leave them pinned. The wrapper's own code before the pin (the type read and the
//   registry scan) uses immediate offsets and no modify register; L4 (the register the wrapper's I4 pointer would wrap by)
//   is zeroed for the whole call and put back at the end.
//   frame words (`_dv_frame_hw`, halfword index k of the frame copy, big-endian values): pitch 1 + v (note << 8),
//   velocity 26 + v (velocity << 8), type 74 + v. There is no voice record to mark: the routine keeps its own
//   per-slot state. The voice's amplitude envelope, gate and level are the engine's: what the routine writes is the
//   signal before them.
//
// Called once per DSP block and slot. A slot with the same type is called with the same arguments every block.
//
// Two kinds of registered row (docs/design/module-sdk.md 4.1 and 5): a row whose word 16 (note_on) is 0 is a routine of the form above (the raw row of a `raw` module, module-sdk.md 11.5 D12: no contract). A row
// with a note_on pc is a machine of the SDK contract (<dk/machine.h>) and runs through `_dv_contract` below: the adapter finds the note-on
// (the rising edge of bit v of frame halfword ADM_TRIG_HW: the edge block is silent, the next block runs note_on and render), zeroes the first
// `clear` bytes of the slot's state (row words 17 state base, 18 stride, 19 clear), builds the `dk_voice_in` and the parameter words (the knobs
// of `ADM_PARAM_SLOT*`, clamped to the row's ranges, in a buffer of its own), calls the module, ends the voice on DK_DONE or a fault, and
// checks the block (the guard of the Digitakt II file: finite samples of at most 8.0, the fault counts in row words 10 to 13, a type
// disabled after 8 faults in 1500 blocks). An idle slot is silent.

#include "adapter-dn2.inc"
#include "dk_machine.inc"

.SECTION/PM seg_pmco;
.GLOBAL _dv_entry;
.EXTERN _fw_frame;
.EXTERN _fw_render_next;
.EXTERN _dv_table;
.EXTERN _gs_rbuf;
.EXTERN _gs_post;
.EXTERN _gs_block;

_dv_entry:
    DM(_dv_s_r0) = R0;
    // The cheap path for a slot that no module renders (every slot of a patched image with no registered type): `_dv_mask` has bit t set when a
    // row of type t is registered (bit 31: a type of 31 or more), and slot 0 rebuilds it from the table once per block below. Slot 0 always
    // takes the full path (the heartbeat). With a registered type, a slot looks its own type up in the mask and takes the full path only on a hit.
    // A row written by the runtime is seen from the next block; a row freed after the mask was built still ends in the scan, which finds no match.
    R0 = 16;
    COMP(R0, R15);
    IF EQ JUMP _dv_full;                  // slot 0
    R0 = DM(_dv_mask);
    R0 = PASS R0;
    IF EQ JUMP _dv_out;                   // no registered type
    DM(_dv_q1) = R1;
    DM(_dv_q2) = R2;
    R0 = I4;
    DM(_dv_q4) = R0;
    R0 = L4;
    DM(_dv_ql4) = R0;
    L4 = 0;
    R0 = 90;                              // 74 + 16: the slot's type halfword is 74 + v = 90 - R15 (as in the full path; the full path checks the caller)
    R0 = R0 - R15;
    R1 = 0xffe;
    R1 = R0 AND R1;
    R1 = LSHIFT R1 BY 1;
    R2 = _fw_frame;
    R1 = R2 + R1;
    I4 = R1;
    R1 = DM(0, I4);
    R2 = FEXT R0 BY 0:1;
    R2 = LSHIFT R2 BY 4;
    R0 = 0x400;
    R2 = R2 OR R0;
    R1 = FEXT R1 BY R2;                   // the type
    R2 = 31;
    COMP(R1, R2);
    IF GT R1 = PASS R2;
    R2 = 0;
    R1 = R2 - R1;
    R2 = DM(_dv_mask);
    R2 = LSHIFT R2 BY R1;                 // a shift by a negative count moves bit t down to bit 0
    R1 = 1;
    R2 = R2 AND R1;
    R0 = DM(_dv_ql4);
    L4 = R0;
    R0 = DM(_dv_q4);
    I4 = R0;
    R1 = DM(_dv_q1);
    R2 = DM(_dv_q2);                      // (the loads leave the flags of the AND above)
    IF NE JUMP _dv_full;
_dv_out:
    R0 = DM(_dv_s_r0);
    MODIFY(I7, -18) (NW);                 // the displaced first instruction of the stock helper
    JUMP _fw_render_next;
_dv_full:
    R0 = I4;
    DM(_dv_s_i4) = R0;
    R0 = L4;
    DM(_dv_s_l4) = R0;                    // I4 is a linear pointer for the whole call; put back at the end
    L4 = 0;
    I4 = _dv_save;
    DM(0, I4) = R1;
    DM(1, I4) = R2;
    DM(2, I4) = R3;
    DM(3, I4) = R4;
    DM(4, I4) = R5;
    DM(5, I4) = R6;
    DM(6, I4) = R7;
    DM(7, I4) = R8;
    DM(8, I4) = R9;
    DM(9, I4) = R10;
    DM(10, I4) = R11;
    DM(11, I4) = R12;
    DM(12, I4) = R13;
    DM(13, I4) = R14;
    DM(14, I4) = R15;
    R0 = I0;                              // a DM store cannot take a DAG1 register (UREGXDAG1, PRM p.15-7): through R0 (free: saved in _dv_s_r0)
    DM(15, I4) = R0;
    R0 = I1;
    DM(16, I4) = R0;
    R0 = I2;
    DM(17, I4) = R0;
    R0 = I3;
    DM(18, I4) = R0;
    R0 = I5;
    DM(19, I4) = R0;
    DM(20, I4) = I12;
    DM(21, I4) = I13;
    R0 = M1;
    DM(22, I4) = R0;
    R0 = M2;
    DM(23, I4) = R0;
    R0 = M3;
    DM(24, I4) = R0;
    R0 = M4;
    DM(25, I4) = R0;
    // the engine's per-voice call: R8 = R12 = the voice buffer (its low bits 0x10 + 0x80 k), R15 = 16 - v in 1..16
    COMP(R8, R12);
    IF NE JUMP _dv_done;
    R0 = 0x7f;
    R1 = R12 AND R0;
    R0 = 0x10;
    COMP(R1, R0);
    IF NE JUMP _dv_done;
    R0 = 16;
    R1 = R0 - R15;                        // v
    IF LT JUMP _dv_done;
    R0 = 15;
    COMP(R1, R0);
    IF GT JUMP _dv_done;
    DM(_dv_t) = R1;
    DM(_dv_buf) = R12;
    R1 = PASS R1;
    IF NE JUMP _dv_nodiag;
    // once per block (slot 0): the heartbeat and the context registers as the firmware left them (finding 45, dl_diag)
    I4 = _dv_diag;
    R1 = DM(0, I4);
    R1 = R1 + 1;
    DM(0, I4) = R1;
    R1 = M5;
    DM(1, I4) = R1;
    R1 = M6;
    DM(2, I4) = R1;
    R1 = M7;
    DM(3, I4) = R1;
    R1 = M13;
    DM(4, I4) = R1;
    R1 = M14;
    DM(5, I4) = R1;
    R1 = M15;
    DM(6, I4) = R1;
    R1 = I6;
    DM(7, I4) = R1;
    R1 = I7;
    DM(8, I4) = R1;
    R1 = B7;
    DM(9, I4) = R1;
    R1 = L7;
    DM(10, I4) = R1;
    R1 = MODE1;
    DM(11, I4) = R1;
    CALL _gs_block;                       // publishes the stereo events of the block that has ended (dsp-voice-dn2-stereo.s); clobbers R0 and R1 only
    // the mask of the registered types: R0-R3 are saved above and free (a free row holds 0xffffffff, the zero row ends the table)
    I4 = _dv_table;
    R3 = 0;
_dv_mkl:
    R0 = DM(0, I4);
    MODIFY(I4, 24) (NW);
    R0 = PASS R0;
    IF EQ JUMP _dv_mke;
    IF LT JUMP _dv_mkl;
    R2 = 31;
    COMP(R0, R2);
    IF GT R0 = PASS R2;
    R1 = 1;
    R1 = LSHIFT R1 BY R0;
    R3 = R3 OR R1;
    JUMP _dv_mkl;
_dv_mke:
    DM(_dv_mask) = R3;
    R1 = DM(_dv_t);
_dv_nodiag:
    R0 = 74;
    R0 = R1 + R0;                         // halfword 74 + v of the frame copy: the slot's machine type
    R1 = 0xffe;
    R1 = R0 AND R1;
    R1 = LSHIFT R1 BY 1;
    R2 = _fw_frame;
    R1 = R2 + R1;
    I4 = R1;
    R1 = DM(0, I4);
    R2 = FEXT R0 BY 0:1;
    R2 = LSHIFT R2 BY 4;
    R0 = 0x400;
    R2 = R2 OR R0;
    R1 = FEXT R1 BY R2;                   // the type
    I4 = _dv_table;
_dv_scan:
    R0 = DM(0, I4);                       // row type; 0 ends the table (the last row is always empty)
    R2 = DM(1, I4);                       // row pc
    MODIFY(I4, 24) (NW);                  // a row is 24 words (core/dsp-voice's module.toml)
    R0 = PASS R0;
    IF EQ JUMP _dv_done;
    COMP(R0, R1);
    IF NE JUMP _dv_scan;
    R0 = I4;
    DM(_dv_row) = R0;                     // the row's end: the contract adapter reads its other words
    R3 = DM(-8, I4);                      // row word 16: the note_on pc; 0 is a routine of the old form
    R3 = PASS R3;
    R0 = _dv_contract;
    IF NE R2 = PASS R0;
    I12 = R2;                             // the routine's pc (or the contract adapter)
    R0 = R1;                              // type
    R2 = DM(_dv_t);
    R4 = DM(_dv_buf);
    R12 = 32;
    // save the context registers, then pin them to the ABI's values (see the header)
    I4 = _dv_ctx;
    R3 = M5;                              // as above, through R3 (free here; MODE1 goes through it too)
    DM(0, I4) = R3;
    R3 = M6;
    DM(1, I4) = R3;
    R3 = M7;
    DM(2, I4) = R3;
    DM(3, I4) = M13;
    DM(4, I4) = M14;
    DM(5, I4) = M15;
    R3 = L0;
    DM(6, I4) = R3;
    R3 = L1;
    DM(7, I4) = R3;
    R3 = L2;
    DM(8, I4) = R3;
    R3 = L3;
    DM(9, I4) = R3;
    R3 = L5;
    DM(10, I4) = R3;
    R3 = MODE1;
    DM(11, I4) = R3;
    M5 = 0;
    M6 = 1;
    M7 = -1;
    M13 = 0;
    M14 = 1;
    M15 = -1;
    L0 = 0;
    L1 = 0;
    L2 = 0;
    L3 = 0;
    L5 = 0;
    BIT CLR MODE1 0x20A000;               // PEYEN, TRUNCATE, ALUSAT
    BIT SET MODE1 0x10000;                // RND32
    NOP;
    DM(_dv_typ) = R0;
    R6 = 4;                               // `last`: the core is about to call the module; the module step is cleared (fault table, dsp-link.inc)
    R7 = 0;
    CALL _dv_mark;
    I13 = _dv_back;
    JUMP (M13, I12);                      // the routine returns with JUMP (M13, I13)
_dv_back:
    R6 = 5;                               // `last`: the module returned (its step stays)
    R7 = -1;
    CALL _dv_mark;
    I4 = _dv_ctx;
    R0 = DM(11, I4);
    MODE1 = R0;
    NOP;
    M5 = DM(0, I4);
    M6 = DM(1, I4);
    M7 = DM(2, I4);
    M13 = DM(3, I4);
    M14 = DM(4, I4);
    M15 = DM(5, I4);
    L0 = DM(6, I4);
    L1 = DM(7, I4);
    L2 = DM(8, I4);
    L3 = DM(9, I4);
    L5 = DM(10, I4);
_dv_done:
    I4 = _dv_save;
    R1 = DM(0, I4);
    R2 = DM(1, I4);
    R3 = DM(2, I4);
    R4 = DM(3, I4);
    R5 = DM(4, I4);
    R6 = DM(5, I4);
    R7 = DM(6, I4);
    R8 = DM(7, I4);
    R9 = DM(8, I4);
    R10 = DM(9, I4);
    R11 = DM(10, I4);
    R12 = DM(11, I4);
    R13 = DM(12, I4);
    R14 = DM(13, I4);
    R15 = DM(14, I4);
    I0 = DM(15, I4);
    I1 = DM(16, I4);
    I2 = DM(17, I4);
    I3 = DM(18, I4);
    I5 = DM(19, I4);
    I12 = DM(20, I4);
    I13 = DM(21, I4);
    M1 = DM(22, I4);
    M2 = DM(23, I4);
    M3 = DM(24, I4);
    M4 = DM(25, I4);
    R0 = DM(_dv_s_l4);
    L4 = R0;
    R0 = DM(_dv_s_i4);
    I4 = R0;
    R0 = DM(_dv_s_r0);
    MODIFY(I7, -18) (NW);                 // the displaced first instruction of the stock helper
    JUMP _fw_render_next;

// The core's mark of a module call (the breadcrumb of the Digitakt II wrapper, reduced to the call and the return). In: R6 = stage (4 the module is about to be
// called, 5 it returned), R7 = the mask for the module step (0 clears it, -1 keeps it): the module step is the static word `_dk_step`, whose address is _dv_rec[2]
// (written by core/dsp-link; 0 without it), and the block tail copies it into the block's record; the slot is `_dv_t`, the type `_dv_typ`. Writes stage | slot << 8 |
// type << 16 into _dv_diag word 12 (core/dsp-link copies it into its word 14) and at once into both reply records (_dv_rec) with the header word, so that it is
// there when the DSP stops inside the module (finding 48). Clobbers R1, R3, I1.
_dv_mark:
    R1 = DM(_dv_t);
    R1 = LSHIFT R1 BY 8;
    R3 = DM(_dv_typ);
    R3 = LSHIFT R3 BY 16;
    R1 = R1 OR R3;
    R1 = R1 OR R6;
    DM(_dv_diag + 48) = R1;
    I1 = _dv_rec;
    R3 = DM(2, I1);
    R3 = PASS R3;
    IF EQ JUMP _dv_mk0;
    I1 = R3;
    R3 = DM(0, I1);
    R3 = R3 AND R7;
    DM(0, I1) = R3;
    I1 = _dv_rec;
_dv_mk0:
    R3 = DM(0, I1);
    R3 = PASS R3;
    IF EQ JUMP _dv_mk1;
    I1 = R3;
    DM(192, I1) = R1;
    R3 = 0x20D1A6;                        // DL_DG_MAGIC | 32 << 16
    DM(45, I1) = R3;
    I1 = _dv_rec;
_dv_mk1:
    R3 = DM(1, I1);
    R3 = PASS R3;
    IF EQ RTS;
    I1 = R3;
    DM(192, I1) = R1;
    R3 = 0x20D1A6;
    DM(45, I1) = R3;
    RTS;

// ---- a machine of the SDK contract (the row has a note_on pc), slot v ----
// Entered with JUMP (M13, I12) from the code above, the context pinned: R0 = type, R2 = v, R4 = the voice buffer, DM(_dv_row) = the row's end
// (word k of the row is DM(k - 24, end)). The adapter's scalars are _dv_c: 0 type, 1 v, 2 buffer, 3 slot record, 4 note-on, 5 parameter index,
// 6 knob slot, 7 state, 8 status, 9 fault kind, 10 faults so far, 11 types disabled. The slot's record (_dv_cst + 16 v): 0 phase (0 silent,
// 1 armed by the edge, 2 running), 1 previous trig bit, 2 blocks since the note-on, 3 latched source track.
_dv_contract:
    DM(_dv_c + 0) = R0;
    DM(_dv_c + 4) = R2;
    DM(_dv_c + 8) = R4;
    R3 = DM(_dv_row);
    I4 = R3;
    R3 = DM(-14, I4);                     // the row's disabled type
    COMP(R3, R0);
    IF EQ JUMP _dv_cz;
    R3 = LSHIFT R2 BY 4;
    R5 = _dv_cst;
    R5 = R5 + R3;
    DM(_dv_c + 12) = R5;
    I3 = R5;
    R0 = ADM_TRIG_HW;
    CALL _dv_frame_hw;
    R2 = DM(_dv_c + 4);
    R1 = 0;
    R1 = R1 - R2;
    R0 = LSHIFT R0 BY R1;
    R1 = 1;
    R9 = R0 AND R1;                       // cur
    R12 = DM(0, I3);                      // phase
    R13 = DM(1, I3);                      // previous trig bit
    DM(1, I3) = R9;
    COMP(R9, R13);
    IF GT JUMP _dv_cedge;
    R12 = PASS R12;
    IF EQ JUMP _dv_cz;
    R11 = 0;
    R1 = 1;
    COMP(R12, R1);
    IF EQ R11 = PASS R1;                  // phase 1: the note-on block
    R1 = 2;
    DM(0, I3) = R1;
    DM(_dv_c + 16) = R11;
    // Latch the source track at the delayed note_on, not the silent edge.
    // The unsigned frame halfword is zero-extended by _dv_frame_hw; no masking
    // can turn an invalid owner into a valid track (finding 90).
    R11 = PASS R11;
    IF EQ JUMP _dv_cowner;
    R2 = DM(_dv_c + 4);
    R0 = 0x52d;
    R0 = R0 + R2;
    CALL _dv_frame_hw;
    R1 = 16;
    COMP(R0, R1);
    IF GE JUMP _dv_cbadowner;
    DM(3, I3) = R0;
_dv_cowner:
    R0 = DM(3, I3);
    DM(_dv_in + DK_IN_TRACK) = R0;
    R11 = DM(_dv_c + 16);
    // the dk_voice_in: blocks since the note-on, pitch, velocity, the output block
    R0 = DM(2, I3);
    R1 = 0;
    R11 = PASS R11;
    IF NE R0 = PASS R1;
    DM(_dv_in + DK_IN_BLOCK) = R0;
    R0 = R0 + 1;
    DM(2, I3) = R0;
    R2 = DM(_dv_c + 4);
    R0 = ADM_PITCH_HW;
    R0 = R0 + R2;
    CALL _dv_frame_hw;
    DM(_dv_in + DK_IN_PITCH) = R0;
    R2 = DM(_dv_c + 4);
    R0 = ADM_VELOCITY_HW;
    R0 = R0 + R2;
    CALL _dv_frame_hw;
    DM(_dv_in + DK_IN_VELOCITY) = R0;
    R4 = DM(_dv_c + 8);
    DM(_dv_in + DK_IN_OUT) = R4;
    // out_r: the slot's R buffer (`_gs_rbuf`, dsp-voice-dn2-stereo.s). A machine that returns DK_STEREO has its R stamped after the guard (`_gs_post`)
    // and run through the R twins of the stages that follow; a mono machine leaves it alone.
    R2 = DM(_dv_c + 4);
    R0 = LSHIFT R2 BY 7;
    R1 = _gs_rbuf;
    R0 = R0 + R1;
    DM(_dv_in + DK_IN_OUT_R) = R0;
    // the parameter words: knob ADM_PARAM_SLOTi of the slot, until a slot of 255 (the buffer's other words stay 0)
    R0 = 0;
    DM(_dv_c + 20) = R0;
_dv_cpl:
    R0 = DM(_dv_c + 20);
    R1 = LSHIFT R0 BY 2;
    R2 = _dv_slots;
    R2 = R2 + R1;
    I1 = R2;
    R6 = DM(0, I1);
    R2 = 255;
    COMP(R6, R2);
    IF EQ JUMP _dv_cpe;
    DM(_dv_c + 24) = R6;
    R2 = DM(_dv_c + 4);
    R3 = ADM_PARAM_STRIDE;
    R3 = R3 * R2 (SSI);
    R0 = ADM_PARAM_BASE;
    R0 = R0 + R3;
    R0 = R0 + R6;                         // the knob's halfword: base + stride v + slot (the Digitone II frame has no groups)
    CALL _dv_frame_hw;
    R6 = DM(_dv_c + 24);
    R13 = DM(_dv_row);
    R14 = LSHIFT R6 BY 2;
    R13 = R13 + R14;
    R14 = 88;
    R13 = R13 - R14;                      // the row's word 2 + slot: the range of the knob, min | max << 16 (0: 0 .. 0x7f00)
    I3 = R13;
    R13 = DM(0, I3);
    R14 = 0x7f000000;
    R13 = PASS R13;
    IF EQ R13 = PASS R14;
    R14 = FEXT R13 BY 16:16;
    R13 = FEXT R13 BY 0:16;
    R0 = MIN(R0, R14);
    R0 = MAX(R0, R13);
    R1 = DM(_dv_c + 20);
    R2 = LSHIFT R1 BY 2;
    R3 = _dv_params;
    R3 = R3 + R2;
    I1 = R3;
    DM(0, I1) = R0;
    R1 = R1 + 1;
    DM(_dv_c + 20) = R1;
    R2 = DK_MAX_PARAMS;
    COMP(R1, R2);
    IF LT JUMP _dv_cpl;
_dv_cpe:
    // the slot's state, cleared at the note-on
    R2 = DM(_dv_c + 4);
    R3 = DM(_dv_row);
    I4 = R3;
    R1 = DM(-7, I4);                      // state base
    R3 = DM(-6, I4);                      // stride
    R3 = R3 * R2 (SSI);
    R4 = R1 + R3;
    DM(_dv_c + 28) = R4;
    R11 = DM(_dv_c + 16);
    R11 = PASS R11;
    IF EQ JUMP _dv_crender;
    R5 = DM(-5, I4);                      // clear bytes
    R6 = -2;
    R5 = LSHIFT R5 BY R6;                 // clear bytes -> words
    R5 = PASS R5;                         // (a shift sets SZ, the condition tests AZ)
    IF EQ JUMP _dv_cnoclr;
    I2 = R4;
    R0 = 0;
    LCNTR = R5, DO _dv_czero UNTIL LCE;
_dv_czero: DM(I2, M6) = R0;
_dv_cnoclr:
    R0 = DM(-8, I4);
    I12 = R0;                             // the note_on pc
    R8 = _dv_in;
    R12 = _dv_params;
    I13 = _dv_nback;
    JUMP (M13, I12);
_dv_nback:
    R0 = PASS R0;
    IF LT JUMP _dv_nfault;
_dv_crender:
    R3 = DM(_dv_row);
    I4 = R3;
    R0 = DM(-23, I4);
    I12 = R0;                             // the render pc
    R4 = DM(_dv_c + 28);
    R8 = _dv_in;
    R12 = _dv_params;
    I13 = _dv_rback;
    JUMP (M13, I12);
_dv_rback:
    R1 = DK_STEREO;                       // contract 1.2: the stereo bit goes to _gs_post, the rest is the status
    R2 = R0 AND R1;
    DM(_dv_c + 48) = R2;
    R1 = NOT R1;
    R0 = R0 AND R1;
    DM(_dv_c + 32) = R0;                  // the status
    // the guard: finite samples of at most 8.0 in magnitude (one integer test: the magnitude bits of 8.0 are 0x41000000, Inf and NaN lie above 0x7f800000)
    R8 = DM(_dv_c + 8);
    I0 = R8;
    R13 = 0;
    R9 = 0x7fffffff;
    LCNTR = 32, DO _dv_gchk UNTIL LCE;
        R0 = DM(I0, M6);
        R0 = R0 AND R9;
_dv_gchk: R13 = MAX(R13, R0);
    R0 = 0x41000000;
    COMP(R13, R0);
    IF GT JUMP _dv_gfault;
    CALL _gs_post;                        // a stereo machine: check, scale and stamp R (dsp-voice-dn2-stereo.s)
    R1 = DK_DONE;
    R0 = DM(_dv_c + 32);
    COMP(R0, R1);
    IF NE JUMP _dv_back;
    R5 = DM(_dv_c + 12);                  // DK_DONE: the voice has ended, silent until the next edge
    I3 = R5;
    R0 = 0;
    DM(0, I3) = R0;
    JUMP _dv_back;
_dv_nfault:
    R12 = 3;                              // note_on returned a negative number
    JUMP _dv_gfaultk;
_dv_gfault:
    R12 = 1;                              // 1 = Inf or NaN, 2 = over the level bound
    R0 = 0x7f800000;
    COMP(R13, R0);
    R3 = 2;
    IF LT R12 = PASS R3;
_dv_gfaultk:
    DM(_dv_c + 36) = R12;
    // the words the module saw, packed in pairs, and the zeroed block, the ended voice
    I1 = _dv_params;
    I2 = _dv_diag;
    MODIFY(I2, 68);
    LCNTR = 4, DO _dv_gpk UNTIL LCE;
        R3 = DM(I1, M6);
        R9 = DM(I1, M6);
        R9 = LSHIFT R9 BY 16;
        R3 = R3 OR R9;
_dv_gpk: DM(I2, M6) = R3;
    R5 = DM(_dv_c + 12);
    I3 = R5;
    R0 = 0;
    DM(0, I3) = R0;
    R8 = DM(_dv_c + 8);
    I0 = R8;
    R3 = 0;
    LCNTR = 32, DO _dv_gz UNTIL LCE;
_dv_gz: DM(I0, M6) = R3;
    R3 = DM(_dv_row);
    I4 = R3;
    R5 = DM(_dv_diag);                    // blocks so far
    R6 = DM(-13, I4);                     // the row's window start
    R7 = R5 - R6;
    R3 = 1500;
    COMP(R7, R3);
    IF LT JUMP _dv_gwin;
    DM(-13, I4) = R5;
    R7 = 0;
    DM(-12, I4) = R7;
_dv_gwin:
    R7 = DM(-12, I4);
    R7 = R7 + 1;
    DM(-12, I4) = R7;                     // faults in this window
    R9 = DM(-11, I4);
    R9 = R9 + 1;
    DM(-11, I4) = R9;                     // faults of the type since the start
    R3 = 8;
    COMP(R7, R3);
    IF LT JUMP _dv_gnd;
    R0 = DM(_dv_c + 0);
    R3 = DM(-14, I4);
    COMP(R3, R0);
    IF EQ JUMP _dv_gnd;                   // already disabled
    DM(-14, I4) = R0;
    DM(_dv_diag + 84) = R0;               // the last disabled type
    R3 = DM(_dv_c + 44);
    R3 = R3 + 1;
    DM(_dv_c + 44) = R3;
_dv_gnd:
    R3 = DM(_dv_c + 40);
    R3 = R3 + 1;
    DM(_dv_c + 40) = R3;
    R0 = 0xffff;
    R3 = R3 AND R0;
    R0 = DM(_dv_c + 36);
    R0 = LSHIFT R0 BY 16;
    R3 = R3 OR R0;
    R0 = DM(_dv_c + 44);
    R0 = LSHIFT R0 BY 24;
    R3 = R3 OR R0;
    DM(_dv_diag + 60) = R3;               // faults (16 bits) | kind << 16 | disabled types << 24
    R0 = DM(_dv_c + 0);
    R3 = DM(_dv_c + 4);
    R3 = LSHIFT R3 BY 16;
    R0 = R0 OR R3;
    DM(_dv_diag + 64) = R0;               // type | voice << 16
    JUMP _dv_back;
// An invalid source owner ends only this slot, without callbacks or a type fault.
_dv_cbadowner:
    R0 = 0;
    DM(0, I3) = R0;
    JUMP _dv_cz;
// The rising edge: the slot is armed, this block is silent; the next block runs note_on and render.
_dv_cedge:
    R1 = 1;
    DM(0, I3) = R1;
// A silent block: a disabled type, a slot that has not been triggered, the edge block.
_dv_cz:
    R8 = DM(_dv_c + 8);
    I0 = R8;
    R3 = 0;
    LCNTR = 32, DO _dv_gdz UNTIL LCE;
_dv_gdz: DM(I0, M6) = R3;
    JUMP _dv_back;

// ---- helpers for machine routines (ordinary subroutines: CALL _dv_..., return with RTS) ----

// F0 = 2 ** F0 for F0 > -64 (relative error 8e-8: a degree-5 minimax polynomial of 2^f on [0, 1) and an exact
// scale by 2^floor). Clobbers R0-R3.
.GLOBAL _dv_exp2;
_dv_exp2:
    R1 = 0x42800000;                      // 64.0
    F0 = F0 + F1;
    R1 = TRUNC F0;
    F2 = FLOAT R1;
    F0 = F0 - F2;                         // f in [0, 1)
    R2 = 0x3af61942;
    F2 = F2 * F0;
    R3 = 0x3c1347f4;
    F2 = F2 + F3;
    F2 = F2 * F0;
    R3 = 0x3d64aa27;
    F2 = F2 + F3;
    F2 = F2 * F0;
    R3 = 0x3e75ead4;
    F2 = F2 + F3;
    F2 = F2 * F0;
    R3 = 0x3f31727b;
    F2 = F2 + F3;
    F2 = F2 * F0;
    R3 = 0x3f7fffff;
    F2 = F2 + F3;
    R3 = -64;
    R1 = R3 + R1;
    F0 = SCALB F2 BY R1;
    RTS;

// R0 = the unsigned 16-bit word k of the DSP's frame copy (R0 = k, 0..4095; the frame copy is about 1,350 halfwords); _dv_frame_sw sign-extends.
// Clobbers R0-R2, I4.
.GLOBAL _dv_frame_hw;
.GLOBAL _dv_frame_sw;
_dv_frame_hw:
    R1 = 0xffe;
    R1 = R0 AND R1;
    R1 = LSHIFT R1 BY 1;
    R2 = _fw_frame;
    R1 = R2 + R1;
    I4 = R1;
    R1 = DM(0, I4);
    R2 = FEXT R0 BY 0:1;
    R2 = LSHIFT R2 BY 4;
    R0 = 0x400;                           // len6 = 16 at Ry[11:6], bit6 below it
    R2 = R2 OR R0;
    R0 = FEXT R1 BY R2;
    RTS;
_dv_frame_sw:
    R1 = 0xffe;
    R1 = R0 AND R1;
    R1 = LSHIFT R1 BY 1;
    R2 = _fw_frame;
    R1 = R2 + R1;
    I4 = R1;
    R1 = DM(0, I4);
    R2 = FEXT R0 BY 0:1;
    R2 = LSHIFT R2 BY 4;
    R0 = 0x400;                           // len6 = 16 at Ry[11:6], bit6 below it
    R2 = R2 OR R0;
    R0 = FEXT R1 BY R2;
    R0 = LSHIFT R0 BY 16;
    R1 = -16;
    R0 = ASHIFT R0 BY R1;
    RTS;

.SECTION/DM seg_dmda;
.PKDATA _dv_save, 128;                     // R1-R15, I0-I3, I5, I12, I13, M1-M4
.PKDATA _dv_s_r0, 4;
.PKDATA _dv_s_i4, 4;
.PKDATA _dv_s_l4, 4;
.PKDATA _dv_mask, 4;                       // registered types: bit t (bit 31: 31 or more), rebuilt by slot 0 (see _dv_entry)
.PKDATA _dv_q1, 4;
.PKDATA _dv_q2, 4;
.PKDATA _dv_q4, 4;
.PKDATA _dv_ql4, 4;
.PKDATA _dv_ctx, 48;                       // M5, M6, M7, M13, M14, M15, L0-L3, L5, MODE1 of the context
// The diagnostic block (finding 45): word 0 counts the blocks that reached the hook, words 1-11 are M5, M6, M7, M13, M14, M15,
// I6, I7, B7, L7, MODE1 as the firmware left them at slot 0. core/dsp-link copies the block into the record it sends to the
// ColdFire each block (dl_diag).
.PKDATA _dv_diag, 128;
// Word 12 (the breadcrumb) holds only the call and the return stage here (_dv_mark), word 13 (the dispatch count of the Digitakt II wrapper, finding 48) stays 0; _dv_rec is written by core/dsp-link: [0] and [1] the two reply records, [2] the address of the module step word.
.PKDATA _dv_rec, 12;
.PKDATA _dv_t, 4;
.PKDATA _dv_typ, 4;
.PKDATA _dv_buf, 4;
.PKDATA _dv_row, 4;
// The contract adapter's data: the `dk_voice_in` it hands to the module, the parameter words (8), its scalars (see `_dv_contract`), the slots'
// records (4 words each) and the knob positions of the module's parameters in order (ADM_PARAM_SLOT0 .. 7; 255: none).
.PKDATA _dv_in, 24;
.PKDATA _dv_params, 32;
.PKDATA _dv_c, 64;
.PKDATA _dv_cst, 256;
.VAR _dv_slots = ADM_PARAM_SLOT0;
.VAR _dv_slots1 = ADM_PARAM_SLOT1;
.VAR _dv_slots2 = ADM_PARAM_SLOT2;
.VAR _dv_slots3 = ADM_PARAM_SLOT3;
.VAR _dv_slots4 = ADM_PARAM_SLOT4;
.VAR _dv_slots5 = ADM_PARAM_SLOT5;
.VAR _dv_slots6 = ADM_PARAM_SLOT6;
.VAR _dv_slots7 = ADM_PARAM_SLOT7;
