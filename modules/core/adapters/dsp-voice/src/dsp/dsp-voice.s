// core/dsp-voice: the per-voice render hook of the SHARC engine (docs/findings/06, "Per-voice render hook").
//
// The firmware renders its 32 voices (two per track: 2t and 2t+1) with a CJUMP-called function whose first
// instruction is `MODIFY(I7, -26) (NW)` (the sample renderer, which also zero-fills an idle voice). patchkit
// overwrites that one 48-bit instruction with `JUMP _dv_entry` (op "jump"); this file executes the displaced
// instruction itself and continues in the stock renderer for every type it does not know, so a patched image with
// no registered type renders exactly the stock audio.
//
// At _dv_entry the stock contract holds: R4 = voice record, R8 = the voice's output buffer (32 floats), R12 = 32,
// R14 = the voice loop counter (32 on the first voice, 1 on the last), I6 = the frame the caller's CJUMP built, the
// delay-slot stores are done. The track is (32 - R14) >> 1; its machine type is the short word cache[t] at
// _fw_type_cache (the DSP's copy of frame word 0x94 + 2t), read as one 32-bit word that holds two tracks.
//
// A registered type (a row {type, pc} of the registry `voices` = _dv_table, filled by `[[entry]]` rows of the machine modules) is rendered by
// calling its routine once per track, on the even voice; the odd voice receives a copy of the even voice's block
// (a synthesised machine is mono; the engine's later stages pan and mix both voices as for a mono sample).
//
// Routine ABI (entered with JUMP (M13, I12); I13 holds the return address, the routine ends with JUMP (M13, I13).
// The simulator's tracer does not follow an indirect CALL, and the firmware has none; the routine may use ordinary
// CALL/RTS pairs for its own subroutines):
//   in   R4 = voice record (0x1d8 bytes; byte +0x1b8 ACTIVE, +0x1b9 note-on mark, see below), R8 = output block
//        (32 floats, written in full), R12 = 32, R0 = type, R1 = voice index (even), R2 = track t.
//   may clobber  R0-R15, I0-I5, I13, M1-M4, B3 and the ALU/multiplier state; this wrapper saves and restores the
//        registers the stock renderer preserves (and I13). Must keep: I6, I7, I8-I12, I14, I15, M0, M5-M15, L registers,
//        MODE1 (SIMD off).
//   The context registers are SET by this wrapper, never assumed. The firmware restores M5, M6, M7, M13, M14, M15, the L
//   registers and MODE1 per DSP context (blk69 context restore, finding 45), so their values at the hook are the context's,
//   not a constant of the machine. Before a registered type runs, the wrapper saves them in the frame and pins
//        M5 = 0, M6 = 1, M7 = -1, M13 = 0, M14 = 1, M15 = -1, L0-L5 = 0 (linear addressing),
//        MODE1: SIMD (PEYEN), ALUSAT and TRUNCATE clear, RND32 set (the startup value; round to nearest, 32-bit floats),
//   and restores the saved values before the stock epilogue runs. So a routine may use M5/M6/M7 as the constants above and
//   JUMP (M13, I13) as written, and it must leave them pinned. The wrapper's own code before the pin (the type read and the
//   registry scan) uses immediate offsets and no modify register; I4's L register is zeroed for it and put back on the stock path.
//   The routine may read the frame copy at _fw_frame (512 short words: pitch word 0x02 + 2t = note << 8; velocity
//   0x34 + 2t = velocity << 8; machine parameters from 0xda + 0x60 t + 2 (mirror index - 25)).
//
// Note-on: the firmware's arm routine stores the short 1 at record +0x1b8, which clears byte +0x1b9. A routine marks
// a running voice by writing a non-zero byte to +0x1b9; `+0x1b8 != 0 and +0x1b9 == 0` is therefore a note-on in this
// block, whatever the machine's selector did to the record in the meantime (docs/findings/06).
//
// Registers: R0-R2, I4 are free on the stock fall-through path (the stock code assigns them before it reads them).
//
// Two kinds of registered row (docs/design/module-sdk.md 4.1 and 5): a row whose word 16 (note_on) is 0 is a routine of the form above (the raw row of a `raw` module, module-sdk.md 11.5 D12: no contract) and
// runs as described. A row with a note_on pc is a machine of the SDK contract (<dk/machine.h>): the adapter below (`_dv_contract`)
// does what each machine's prologue used to do. It finds the note-on (record +0x1b8 ACTIVE != 0 and +0x1b9 mark == 0, and writes the
// mark), zeroes the first `clear` bytes of the voice's state (row words 17 state base, 18 stride, 19 clear; the state of track t is
// base + t stride) and calls note_on, then render, with R4 = state, R8 = a `dk_voice_in` of its own (pitch with the tune knob folded
// in, velocity, blocks since the note-on, the output block), R12 = the parameter words (the knobs of `ADM_PARAM_SLOT*`, clamped to
// the row's ranges, in its own buffer: the frame is not written). It ends the voice (ACTIVE and mark cleared) on DK_DONE or a fault,
// and the output check below (the guard) is the same for both kinds. An idle voice is not called: its block is silent.

#include "adapter-dt2.inc"
#include "dk_machine.inc"

.SECTION/PM seg_pmco;
.GLOBAL _dv_entry;
.EXTERN _fw_type_cache;
.EXTERN _fw_render_next;
.EXTERN _fw_frame;
.EXTERN _fw_param_rec;
.EXTERN _dv_table;

_dv_entry:
    // The cheap path when no row is registered (`_dv_mask` is 0): voice 0 (R14 = 32) keeps the full path for the heartbeat and the mask below; every other voice
    // goes back to the stock renderer at once (R0 is free here). No module routine can run then, so the breadcrumb of the voices is not written either.
    // (voice 0 is tested first: it writes `_dv_mask` before any voice reads it)
    R0 = 32;
    COMP(R0, R14);
    IF EQ JUMP _dv_full;
    R0 = DM(_dv_mask);
    R0 = PASS R0;
    IF EQ JUMP _dv_idle;
_dv_full:
    R0 = L4;
    DM(_dv_sv_l4) = R0;                   // I4 is used below as a linear pointer: L4 = 0 until the stock path (put back there)
    L4 = 0;
    R0 = 32;
    R0 = R0 - R14;                        // v, the voice index
    IF NE JUMP _dv_nodiag;
    // once per block (voice 0): the heartbeat and the context registers as the firmware left them (finding 45, dl_diag)
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
    // and the mask of the registered types: bit t (bit 31: a type of 31 or more) is set when a row of that type is registered. A free row holds
    // 0xffffffff, the zero row ends the table. A row written by the runtime is seen from the next block. (R0 accumulates, R1 and R2 are free)
    I4 = _dv_table;
    R0 = 0;
_dv_mkl:
    R1 = DM(0, I4);
    MODIFY(I4, 24) (NW);
    R1 = PASS R1;
    IF EQ JUMP _dv_mke;
    IF LT JUMP _dv_mkl;
    R2 = 31;
    COMP(R1, R2);
    IF GT R1 = PASS R2;
    R2 = 1;
    R2 = LSHIFT R2 BY R1;
    R0 = R0 OR R2;
    JUMP _dv_mkl;
_dv_mke:
    DM(_dv_mask) = R0;
    R0 = 0;                               // v
_dv_nodiag:
    R1 = LSHIFT R0 BY 8;
    R1 = BSET R1 BY 0;                    // breadcrumb stage 1 (hook entered) | v << 8, kept in the DSP's own diagnostic word 12
    DM(_dv_diag + 48) = R1;
    R2 = FEXT R0 BY 1:4;                  // t, the track
    R1 = 0xe;
    R1 = R2 AND R1;                       // t & 14
    R1 = LSHIFT R1 BY 1;                  // byte offset of the 32-bit word that holds cache[t] and cache[t^1]
    R2 = _fw_type_cache;
    R1 = R2 + R1;
    I4 = R1;
    R1 = DM(0, I4);                       // the two types
    R2 = FEXT R0 BY 1:4;                  // t
    R2 = FEXT R2 BY 0:1;                  // t & 1
    R2 = LSHIFT R2 BY 4;                  // 0 or 16: the bit position of the half
    R0 = 0x400;
    R2 = R2 OR R0;
    R1 = FEXT R1 BY R2;                   // cache[t]
    R2 = 31;                              // a type that no row registers takes the stock path without the table walk (`_dv_mask`, above)
    R0 = R1;
    COMP(R0, R2);
    IF GT R0 = PASS R2;
    R2 = 0;
    R0 = R2 - R0;
    R2 = DM(_dv_mask);
    R2 = LSHIFT R2 BY R0;                 // a shift by a negative count moves bit t down to bit 0
    R0 = 1;
    R2 = R2 AND R0;
    IF EQ JUMP _dv_stock;
    I4 = _dv_table;
_dv_scan:
    R0 = DM(0, I4);                       // row type; 0 ends the table (the last row is always empty)
    R2 = DM(1, I4);                       // row pc
    MODIFY(I4, 24) (NW);                  // a row is 24 words (see the header, "The registry row")
    R0 = PASS R0;
    IF EQ JUMP _dv_stock;
    COMP(R0, R1);
    IF NE JUMP _dv_scan;
    R0 = I4;
    DM(_dv_g + 0) = R0;                   // the matching row's end (the guard reads its fields with negative offsets)
#ifdef DV_BISECT_C2
    JUMP _dv_stock;                       // bisect build: no registered path; the type takes the stock path (stage 3 for 7 or more)
#endif

    // A registered type: the even voice runs the routine, the odd voice copies the even voice's block (the buffers
    // are 0x80 bytes apart).
    I12 = R2;                             // the routine's pc
#ifdef DV_C3_FINE
    // Rung C3 (docs/PATCHING.md): fine breadcrumb 8, the type matched (R0, R2, I4 are free here; written inline, no CALL, no RTS)
    R0 = 32;
    R0 = R0 - R14;
    R0 = LSHIFT R0 BY 8;
    R2 = LSHIFT R1 BY 16;
    R0 = R0 OR R2;
    DM(_dv_bcw) = R0;
    R2 = BSET R0 BY 3;
    I4 = _dv_diag;
    DM(12, I4) = R2;
    I4 = _dv_rec;
    R0 = DM(0, I4);
    R0 = PASS R0;
    IF EQ JUMP _dv_f1a;
    I4 = R0;
    DM(192, I4) = R2;
    R0 = 0x20D1A6;
    DM(45, I4) = R0;
    R0 = DM(_dv_diag + 52);
    DM(193, I4) = R0;
    R0 = DM(_dv_diag + 56);
    DM(213, I4) = R0;
    I4 = _dv_rec;
_dv_f1a:
    R0 = DM(1, I4);
    R0 = PASS R0;
    IF EQ JUMP _dv_f1b;
    I4 = R0;
    DM(192, I4) = R2;
    R0 = 0x20D1A6;
    DM(45, I4) = R0;
    R0 = DM(_dv_diag + 52);
    DM(193, I4) = R0;
    R0 = DM(_dv_diag + 56);
    DM(213, I4) = R0;
_dv_f1b:
#endif
#ifdef DV_C3_NOIRQ
    // Rung C3: interrupts masked from here to the epilogue (MODE1.IRPTEN clear; the saved MODE1 keeps it set). R0 holds the original MODE1.
    R0 = MODE1;
    BIT CLR MODE1 0x1000;
    NOP;
#endif
    MODIFY(I7, -40) (NW);
#ifdef DV_C3_DEEP
    // deep breadcrumb 11: after the MODIFY of I7 (R0, R2, I4 only; the stage word is in R2)
    R0 = 11;
    R2 = DM(_dv_bcw);
    R2 = R2 OR R0;
    I4 = _dv_diag;
    DM(12, I4) = R2;
    I4 = _dv_rec;
    R0 = DM(0, I4);
    R0 = PASS R0;
    IF EQ JUMP _dv_d11a;
    I4 = R0;
    DM(192, I4) = R2;
    R0 = 0x20D1A6;
    DM(45, I4) = R0;
    R0 = DM(_dv_diag + 52);
    DM(193, I4) = R0;
    R0 = DM(_dv_diag + 56);
    DM(213, I4) = R0;
    I4 = _dv_rec;
_dv_d11a:
    R0 = DM(1, I4);
    R0 = PASS R0;
    IF EQ JUMP _dv_d11b;
    I4 = R0;
    DM(192, I4) = R2;
    R0 = 0x20D1A6;
    DM(45, I4) = R0;
    R0 = DM(_dv_diag + 52);
    DM(193, I4) = R0;
    R0 = DM(_dv_diag + 56);
    DM(213, I4) = R0;
_dv_d11b:
#endif
    DM(-6, I6) = R3;
    DM(-7, I6) = R5;
    DM(-8, I6) = R6;
    DM(-9, I6) = R7;
    DM(-10, I6) = R9;
    DM(-11, I6) = R10;
    DM(-12, I6) = R11;
    DM(-13, I6) = R13;
    DM(-14, I6) = R14;
    DM(-15, I6) = R15;
#ifdef DV_C3_DEEP
    // deep breadcrumb 12: stores -6 .. -15 (R registers) done, before the first DAG1 register store (R0, R2, I4 only; the stage word is in R2)
    R0 = 12;
    R2 = DM(_dv_bcw);
    R2 = R2 OR R0;
    I4 = _dv_diag;
    DM(12, I4) = R2;
    I4 = _dv_rec;
    R0 = DM(0, I4);
    R0 = PASS R0;
    IF EQ JUMP _dv_d12a;
    I4 = R0;
    DM(192, I4) = R2;
    R0 = 0x20D1A6;
    DM(45, I4) = R0;
    R0 = DM(_dv_diag + 52);
    DM(193, I4) = R0;
    R0 = DM(_dv_diag + 56);
    DM(213, I4) = R0;
    I4 = _dv_rec;
_dv_d12a:
    R0 = DM(1, I4);
    R0 = PASS R0;
    IF EQ JUMP _dv_d12b;
    I4 = R0;
    DM(192, I4) = R2;
    R0 = 0x20D1A6;
    DM(45, I4) = R0;
    R0 = DM(_dv_diag + 52);
    DM(193, I4) = R0;
    R0 = DM(_dv_diag + 56);
    DM(213, I4) = R0;
_dv_d12b:
#endif
    R2 = I0;                           // PRM p.15-7: a DM store cannot take a DAG1 register (UREGXDAG1), go through R2 as the stock code does
    DM(-16, I6) = R2;
#ifdef DV_C3_DEEP
    // deep breadcrumb 13: after DM(-16, I6) = I0 (R0, R2, I4 only; the stage word is in R2)
    R0 = 13;
    R2 = DM(_dv_bcw);
    R2 = R2 OR R0;
    I4 = _dv_diag;
    DM(12, I4) = R2;
    I4 = _dv_rec;
    R0 = DM(0, I4);
    R0 = PASS R0;
    IF EQ JUMP _dv_d13a;
    I4 = R0;
    DM(192, I4) = R2;
    R0 = 0x20D1A6;
    DM(45, I4) = R0;
    R0 = DM(_dv_diag + 52);
    DM(193, I4) = R0;
    R0 = DM(_dv_diag + 56);
    DM(213, I4) = R0;
    I4 = _dv_rec;
_dv_d13a:
    R0 = DM(1, I4);
    R0 = PASS R0;
    IF EQ JUMP _dv_d13b;
    I4 = R0;
    DM(192, I4) = R2;
    R0 = 0x20D1A6;
    DM(45, I4) = R0;
    R0 = DM(_dv_diag + 52);
    DM(193, I4) = R0;
    R0 = DM(_dv_diag + 56);
    DM(213, I4) = R0;
_dv_d13b:
#endif
    R2 = I1;                           // PRM p.15-7: a DM store cannot take a DAG1 register (UREGXDAG1), go through R2 as the stock code does
    DM(-17, I6) = R2;
    R2 = I2;                           // PRM p.15-7: a DM store cannot take a DAG1 register (UREGXDAG1), go through R2 as the stock code does
    DM(-18, I6) = R2;
    R2 = I3;                           // PRM p.15-7: a DM store cannot take a DAG1 register (UREGXDAG1), go through R2 as the stock code does
    DM(-19, I6) = R2;
    R2 = I5;                           // PRM p.15-7: a DM store cannot take a DAG1 register (UREGXDAG1), go through R2 as the stock code does
    DM(-20, I6) = R2;
#ifdef DV_C3_DEEP
    // deep breadcrumb 14: after I1, I2, I3, I5 (R0, R2, I4 only; the stage word is in R2)
    R0 = 14;
    R2 = DM(_dv_bcw);
    R2 = R2 OR R0;
    I4 = _dv_diag;
    DM(12, I4) = R2;
    I4 = _dv_rec;
    R0 = DM(0, I4);
    R0 = PASS R0;
    IF EQ JUMP _dv_d14a;
    I4 = R0;
    DM(192, I4) = R2;
    R0 = 0x20D1A6;
    DM(45, I4) = R0;
    R0 = DM(_dv_diag + 52);
    DM(193, I4) = R0;
    R0 = DM(_dv_diag + 56);
    DM(213, I4) = R0;
    I4 = _dv_rec;
_dv_d14a:
    R0 = DM(1, I4);
    R0 = PASS R0;
    IF EQ JUMP _dv_d14b;
    I4 = R0;
    DM(192, I4) = R2;
    R0 = 0x20D1A6;
    DM(45, I4) = R0;
    R0 = DM(_dv_diag + 52);
    DM(193, I4) = R0;
    R0 = DM(_dv_diag + 56);
    DM(213, I4) = R0;
_dv_d14b:
#endif
    R2 = M1;                           // PRM p.15-7: a DM store cannot take a DAG1 register (UREGXDAG1), go through R2 as the stock code does
    DM(-21, I6) = R2;
    R2 = M2;                           // PRM p.15-7: a DM store cannot take a DAG1 register (UREGXDAG1), go through R2 as the stock code does
    DM(-22, I6) = R2;
    R2 = M3;                           // PRM p.15-7: a DM store cannot take a DAG1 register (UREGXDAG1), go through R2 as the stock code does
    DM(-23, I6) = R2;
#ifdef DV_C3_DEEP
    // deep breadcrumb 15: after M1, M2, M3 (R0, R2, I4 only; the stage word is in R2)
    R0 = 15;
    R2 = DM(_dv_bcw);
    R2 = R2 OR R0;
    I4 = _dv_diag;
    DM(12, I4) = R2;
    I4 = _dv_rec;
    R0 = DM(0, I4);
    R0 = PASS R0;
    IF EQ JUMP _dv_d15a;
    I4 = R0;
    DM(192, I4) = R2;
    R0 = 0x20D1A6;
    DM(45, I4) = R0;
    R0 = DM(_dv_diag + 52);
    DM(193, I4) = R0;
    R0 = DM(_dv_diag + 56);
    DM(213, I4) = R0;
    I4 = _dv_rec;
_dv_d15a:
    R0 = DM(1, I4);
    R0 = PASS R0;
    IF EQ JUMP _dv_d15b;
    I4 = R0;
    DM(192, I4) = R2;
    R0 = 0x20D1A6;
    DM(45, I4) = R0;
    R0 = DM(_dv_diag + 52);
    DM(193, I4) = R0;
    R0 = DM(_dv_diag + 56);
    DM(213, I4) = R0;
_dv_d15b:
#endif
    R2 = B3;                           // PRM p.15-7: a DM store cannot take a DAG1 register (UREGXDAG1), go through R2 as the stock code does
    DM(-24, I6) = R2;
    DM(-25, I6) = I13;
    // save the context registers, then pin them to the ABI's values (see the header)
#ifdef DV_C3_DEEP
    // deep breadcrumb 16: after B3, I13 (R0, R2, I4 only; the stage word is in R2)
    R0 = 16;
    R2 = DM(_dv_bcw);
    R2 = R2 OR R0;
    I4 = _dv_diag;
    DM(12, I4) = R2;
    I4 = _dv_rec;
    R0 = DM(0, I4);
    R0 = PASS R0;
    IF EQ JUMP _dv_d16a;
    I4 = R0;
    DM(192, I4) = R2;
    R0 = 0x20D1A6;
    DM(45, I4) = R0;
    R0 = DM(_dv_diag + 52);
    DM(193, I4) = R0;
    R0 = DM(_dv_diag + 56);
    DM(213, I4) = R0;
    I4 = _dv_rec;
_dv_d16a:
    R0 = DM(1, I4);
    R0 = PASS R0;
    IF EQ JUMP _dv_d16b;
    I4 = R0;
    DM(192, I4) = R2;
    R0 = 0x20D1A6;
    DM(45, I4) = R0;
    R0 = DM(_dv_diag + 52);
    DM(193, I4) = R0;
    R0 = DM(_dv_diag + 56);
    DM(213, I4) = R0;
_dv_d16b:
#endif
    R2 = M5;                           // PRM p.15-7: a DM store cannot take a DAG1 register (UREGXDAG1), go through R2 as the stock code does
    DM(-26, I6) = R2;
    R2 = M6;                           // PRM p.15-7: a DM store cannot take a DAG1 register (UREGXDAG1), go through R2 as the stock code does
    DM(-27, I6) = R2;
    R2 = M7;                           // PRM p.15-7: a DM store cannot take a DAG1 register (UREGXDAG1), go through R2 as the stock code does
    DM(-28, I6) = R2;
#ifdef DV_C3_DEEP
    // deep breadcrumb 17: after M5, M6, M7 (R0, R2, I4 only; the stage word is in R2)
    R0 = 17;
    R2 = DM(_dv_bcw);
    R2 = R2 OR R0;
    I4 = _dv_diag;
    DM(12, I4) = R2;
    I4 = _dv_rec;
    R0 = DM(0, I4);
    R0 = PASS R0;
    IF EQ JUMP _dv_d17a;
    I4 = R0;
    DM(192, I4) = R2;
    R0 = 0x20D1A6;
    DM(45, I4) = R0;
    R0 = DM(_dv_diag + 52);
    DM(193, I4) = R0;
    R0 = DM(_dv_diag + 56);
    DM(213, I4) = R0;
    I4 = _dv_rec;
_dv_d17a:
    R0 = DM(1, I4);
    R0 = PASS R0;
    IF EQ JUMP _dv_d17b;
    I4 = R0;
    DM(192, I4) = R2;
    R0 = 0x20D1A6;
    DM(45, I4) = R0;
    R0 = DM(_dv_diag + 52);
    DM(193, I4) = R0;
    R0 = DM(_dv_diag + 56);
    DM(213, I4) = R0;
_dv_d17b:
#endif
    DM(-29, I6) = M13;
    DM(-30, I6) = M14;
    DM(-31, I6) = M15;
#ifdef DV_C3_DEEP
    // deep breadcrumb 18: after M13, M14, M15 (R0, R2, I4 only; the stage word is in R2)
    R0 = 18;
    R2 = DM(_dv_bcw);
    R2 = R2 OR R0;
    I4 = _dv_diag;
    DM(12, I4) = R2;
    I4 = _dv_rec;
    R0 = DM(0, I4);
    R0 = PASS R0;
    IF EQ JUMP _dv_d18a;
    I4 = R0;
    DM(192, I4) = R2;
    R0 = 0x20D1A6;
    DM(45, I4) = R0;
    R0 = DM(_dv_diag + 52);
    DM(193, I4) = R0;
    R0 = DM(_dv_diag + 56);
    DM(213, I4) = R0;
    I4 = _dv_rec;
_dv_d18a:
    R0 = DM(1, I4);
    R0 = PASS R0;
    IF EQ JUMP _dv_d18b;
    I4 = R0;
    DM(192, I4) = R2;
    R0 = 0x20D1A6;
    DM(45, I4) = R0;
    R0 = DM(_dv_diag + 52);
    DM(193, I4) = R0;
    R0 = DM(_dv_diag + 56);
    DM(213, I4) = R0;
_dv_d18b:
#endif
    R2 = L0;                           // PRM p.15-7: a DM store cannot take a DAG1 register (UREGXDAG1), go through R2 as the stock code does
    DM(-32, I6) = R2;
    R2 = L1;                           // PRM p.15-7: a DM store cannot take a DAG1 register (UREGXDAG1), go through R2 as the stock code does
    DM(-33, I6) = R2;
    R2 = L2;                           // PRM p.15-7: a DM store cannot take a DAG1 register (UREGXDAG1), go through R2 as the stock code does
    DM(-34, I6) = R2;
    R2 = L3;                           // PRM p.15-7: a DM store cannot take a DAG1 register (UREGXDAG1), go through R2 as the stock code does
    DM(-35, I6) = R2;
#ifdef DV_C3_DEEP
    // deep breadcrumb 19: after L0 .. L3 (R0, R2, I4 only; the stage word is in R2)
    R0 = 19;
    R2 = DM(_dv_bcw);
    R2 = R2 OR R0;
    I4 = _dv_diag;
    DM(12, I4) = R2;
    I4 = _dv_rec;
    R0 = DM(0, I4);
    R0 = PASS R0;
    IF EQ JUMP _dv_d19a;
    I4 = R0;
    DM(192, I4) = R2;
    R0 = 0x20D1A6;
    DM(45, I4) = R0;
    R0 = DM(_dv_diag + 52);
    DM(193, I4) = R0;
    R0 = DM(_dv_diag + 56);
    DM(213, I4) = R0;
    I4 = _dv_rec;
_dv_d19a:
    R0 = DM(1, I4);
    R0 = PASS R0;
    IF EQ JUMP _dv_d19b;
    I4 = R0;
    DM(192, I4) = R2;
    R0 = 0x20D1A6;
    DM(45, I4) = R0;
    R0 = DM(_dv_diag + 52);
    DM(193, I4) = R0;
    R0 = DM(_dv_diag + 56);
    DM(213, I4) = R0;
_dv_d19b:
#endif
    R3 = DM(_dv_sv_l4);                   // L4 at entry (zeroed since)
#ifdef DV_C3_DEEP
    // deep breadcrumb 20: after the load of _dv_sv_l4 (R0, R2, I4 only; the stage word is in R2)
    R0 = 20;
    R2 = DM(_dv_bcw);
    R2 = R2 OR R0;
    I4 = _dv_diag;
    DM(12, I4) = R2;
    I4 = _dv_rec;
    R0 = DM(0, I4);
    R0 = PASS R0;
    IF EQ JUMP _dv_d20a;
    I4 = R0;
    DM(192, I4) = R2;
    R0 = 0x20D1A6;
    DM(45, I4) = R0;
    R0 = DM(_dv_diag + 52);
    DM(193, I4) = R0;
    R0 = DM(_dv_diag + 56);
    DM(213, I4) = R0;
    I4 = _dv_rec;
_dv_d20a:
    R0 = DM(1, I4);
    R0 = PASS R0;
    IF EQ JUMP _dv_d20b;
    I4 = R0;
    DM(192, I4) = R2;
    R0 = 0x20D1A6;
    DM(45, I4) = R0;
    R0 = DM(_dv_diag + 52);
    DM(193, I4) = R0;
    R0 = DM(_dv_diag + 56);
    DM(213, I4) = R0;
_dv_d20b:
#endif
    DM(-36, I6) = R3;
    R2 = L5;                           // PRM p.15-7: a DM store cannot take a DAG1 register (UREGXDAG1), go through R2 as the stock code does
    DM(-37, I6) = R2;
#ifdef DV_C3_DEEP
    // deep breadcrumb 21: after DM(-36) and DM(-37) (R0, R2, I4 only; the stage word is in R2)
    R0 = 21;
    R2 = DM(_dv_bcw);
    R2 = R2 OR R0;
    I4 = _dv_diag;
    DM(12, I4) = R2;
    I4 = _dv_rec;
    R0 = DM(0, I4);
    R0 = PASS R0;
    IF EQ JUMP _dv_d21a;
    I4 = R0;
    DM(192, I4) = R2;
    R0 = 0x20D1A6;
    DM(45, I4) = R0;
    R0 = DM(_dv_diag + 52);
    DM(193, I4) = R0;
    R0 = DM(_dv_diag + 56);
    DM(213, I4) = R0;
    I4 = _dv_rec;
_dv_d21a:
    R0 = DM(1, I4);
    R0 = PASS R0;
    IF EQ JUMP _dv_d21b;
    I4 = R0;
    DM(192, I4) = R2;
    R0 = 0x20D1A6;
    DM(45, I4) = R0;
    R0 = DM(_dv_diag + 52);
    DM(193, I4) = R0;
    R0 = DM(_dv_diag + 56);
    DM(213, I4) = R0;
_dv_d21b:
#endif
#ifdef DV_C3_NOIRQ
    DM(-38, I6) = R0;                     // the MODE1 of the hook entry (IRPTEN set), read before the mask
#else
    R3 = MODE1;
#ifdef DV_C3_DEEP
    // deep breadcrumb 22: after the read of MODE1 (R0, R2, I4 only; the stage word is in R2)
    R0 = 22;
    R2 = DM(_dv_bcw);
    R2 = R2 OR R0;
    I4 = _dv_diag;
    DM(12, I4) = R2;
    I4 = _dv_rec;
    R0 = DM(0, I4);
    R0 = PASS R0;
    IF EQ JUMP _dv_d22a;
    I4 = R0;
    DM(192, I4) = R2;
    R0 = 0x20D1A6;
    DM(45, I4) = R0;
    R0 = DM(_dv_diag + 52);
    DM(193, I4) = R0;
    R0 = DM(_dv_diag + 56);
    DM(213, I4) = R0;
    I4 = _dv_rec;
_dv_d22a:
    R0 = DM(1, I4);
    R0 = PASS R0;
    IF EQ JUMP _dv_d22b;
    I4 = R0;
    DM(192, I4) = R2;
    R0 = 0x20D1A6;
    DM(45, I4) = R0;
    R0 = DM(_dv_diag + 52);
    DM(193, I4) = R0;
    R0 = DM(_dv_diag + 56);
    DM(213, I4) = R0;
_dv_d22b:
#endif
    DM(-38, I6) = R3;
#endif
#ifdef DV_C3_FINE
    // fine breadcrumb 9, the frame is saved
    R3 = 9;
    R5 = DM(_dv_bcw);
    R3 = R3 OR R5;
    I4 = _dv_diag;
    DM(12, I4) = R3;
    I4 = _dv_rec;
    R5 = DM(0, I4);
    R5 = PASS R5;
    IF EQ JUMP _dv_f2a;
    I4 = R5;
    DM(192, I4) = R3;
    R5 = 0x20D1A6;
    DM(45, I4) = R5;
    R5 = DM(_dv_diag + 52);
    DM(193, I4) = R5;
    R5 = DM(_dv_diag + 56);
    DM(213, I4) = R5;
    I4 = _dv_rec;
_dv_f2a:
    R5 = DM(1, I4);
    R5 = PASS R5;
    IF EQ JUMP _dv_f2b;
    I4 = R5;
    DM(192, I4) = R3;
    R5 = 0x20D1A6;
    DM(45, I4) = R5;
    R5 = DM(_dv_diag + 52);
    DM(193, I4) = R5;
    R5 = DM(_dv_diag + 56);
    DM(213, I4) = R5;
_dv_f2b:
#endif
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
#ifndef DV_C3_NOSYS
    BIT CLR MODE1 0x20A000;               // PEYEN, TRUNCATE, ALUSAT
    BIT SET MODE1 0x10000;                // RND32
    NOP;
#endif
#ifdef DV_C3_FINE
    // fine breadcrumb 10, the context is pinned
    R3 = 10;
    R5 = DM(_dv_bcw);
    R3 = R3 OR R5;
    I4 = _dv_diag;
    DM(12, I4) = R3;
    I4 = _dv_rec;
    R5 = DM(0, I4);
    R5 = PASS R5;
    IF EQ JUMP _dv_f3a;
    I4 = R5;
    DM(192, I4) = R3;
    R5 = 0x20D1A6;
    DM(45, I4) = R5;
    R5 = DM(_dv_diag + 52);
    DM(193, I4) = R5;
    R5 = DM(_dv_diag + 56);
    DM(213, I4) = R5;
    I4 = _dv_rec;
_dv_f3a:
    R5 = DM(1, I4);
    R5 = PASS R5;
    IF EQ JUMP _dv_f3b;
    I4 = R5;
    DM(192, I4) = R3;
    R5 = 0x20D1A6;
    DM(45, I4) = R5;
    R5 = DM(_dv_diag + 52);
    DM(193, I4) = R5;
    R5 = DM(_dv_diag + 56);
    DM(213, I4) = R5;
_dv_f3b:
#endif
    R3 = 32;
    R3 = R3 - R14;                        // v
    R3 = LSHIFT R3 BY 8;
    R5 = LSHIFT R1 BY 16;
    R3 = R3 OR R5;                        // v << 8 | type << 16: the breadcrumb word without its stage
    DM(_dv_bcw) = R3;
    R3 = 2;
#ifdef DV_C3_DB
    CALL _dv_crumb (DB);
    NOP;
    NOP;
#else
    CALL _dv_crumb;                       // stage 2: the type is registered
#endif
    R0 = R1;                              // type
    R1 = 32;
    R1 = R1 - R14;                        // v
    R2 = FEXT R1 BY 1:4;                  // t
    R3 = FEXT R1 BY 0:1;
    R3 = PASS R3;
    IF NE JUMP _dv_odd;
    R3 = DM(_dv_diag + 52);
    R3 = R3 + 1;
    DM(_dv_diag + 52) = R3;               // dispatches to registered types
    R3 = 4;
#ifdef DV_C3_DB
    CALL _dv_crumb (DB);
    NOP;
    NOP;
#else
    CALL _dv_crumb;                       // stage 4: the routine is about to be called (R3, R5, I4 are free here)
#endif
    R3 = LSHIFT R2 BY 2;                  // the track's stereo flag starts the block clear (the odd voice normally consumes it; stale if it did not run)
    R5 = _dv_st;
    R5 = R5 + R3;
    I1 = R5;
    R3 = 0;
    DM(0, I1) = R3;
    CALL _dv_clrstep;                     // the module step word is cleared
    R3 = DM(_dv_g + 0);
    I4 = R3;
    R3 = DM(-8, I4);                      // row word 16: the note_on pc; 0 is a routine of the old form
    R3 = PASS R3;
    IF NE JUMP _dv_contract;
    // ---- the guard, before the routine (docs/findings/51): the machine parameter words are clamped to the row's declared ranges
    // in place in the frame copy and put back when the routine has returned; a type disabled by its faults is not called. ----
    DM(_dv_g + 4) = R0;                   // type
    DM(_dv_g + 8) = R1;                   // voice
    DM(_dv_g + 12) = R2;                  // track
    DM(_dv_g + 16) = R4;                  // voice record
    DM(_dv_g + 20) = R8;                  // output block
    R5 = 0;                               // mask of the words the clamp changed
    DM(_dv_g + 24) = R5;
    DM(_dv_c + 20) = R5;                  // not a contract voice (the fault report reads the frame words)
    R3 = DM(_dv_g + 0);
    I4 = R3;
    R3 = DM(-14, I4);                     // the row's disabled type
    COMP(R3, R0);
    IF EQ JUMP _dv_gdis;
    R3 = LSHIFT R2 BY 5;
    R6 = LSHIFT R2 BY 4;
    R3 = R3 + R6;
    R6 = 0x6d;
    R3 = R3 + R6;                         // halfword index of knob 0: 0x6d + 48 t
    DM(_dv_g + 28) = R3;
    R3 = DM(_dv_g + 0);
    R6 = 88;
    R3 = R3 - R6;                        // the row's word 2: the range of knob 0
    I3 = R3;
    R6 = 0;                               // knob j
_dv_gl:
    CALL _dv_gloc;                        // I2 = the word, R10 = bit 0 or 16 of the knob's half
    R9 = DM(I2, M5);
    R11 = 0x400;
    R11 = R11 OR R10;
    R12 = FEXT R9 BY R11;                 // the knob's word
    R13 = DM(I3, M6);                     // min | max << 16; 0: undeclared
    R14 = 0x7f000000;                     // 0 .. 0x7f00
    R13 = PASS R13;
    IF EQ R13 = PASS R14;
    R14 = FEXT R13 BY 16:16;
    R13 = FEXT R13 BY 0:16;
    R15 = MIN(R12, R14);
    R15 = MAX(R15, R13);
    COMP(R15, R12);
    IF EQ JUMP _dv_gn;
    R3 = LSHIFT R6 BY 2;
    R14 = _dv_g;                          // (a symbol + constant in an immediate drops the constant: add it)
    R14 = R14 + R3;
    R3 = 44;                              // the originals
    R14 = R14 + R3;
    I1 = R14;
    DM(I1, M5) = R12;
    R3 = 1;
    R3 = LSHIFT R3 BY R6;
    R5 = R5 OR R3;
    R3 = 0xffff;
    R3 = LSHIFT R3 BY R10;
    R3 = NOT R3;
    R9 = R9 AND R3;
    R3 = LSHIFT R15 BY R10;
    R9 = R9 OR R3;
    DM(I2, M5) = R9;
_dv_gn:
    R6 = R6 + 1;
    R3 = 8;
    COMP(R6, R3);
    IF LT JUMP _dv_gl;
    DM(_dv_g + 24) = R5;
    R0 = DM(_dv_g + 4);
    R1 = DM(_dv_g + 8);
    R2 = DM(_dv_g + 12);
    R4 = DM(_dv_g + 16);
    R8 = DM(_dv_g + 20);
    R12 = 32;
    I13 = _dv_back;
    JUMP (M13, I12);                      // the routine returns with JUMP (M13, I13)
_dv_back:
    R3 = DM(_dv_diag + 56);
    R3 = R3 + 1;
    DM(_dv_diag + 56) = R3;               // routines that returned
    R3 = 5;
#ifdef DV_C3_DB
    CALL _dv_crumb (DB);
    NOP;
    NOP;
#else
    CALL _dv_crumb;                       // stage 5: the routine returned
#endif
    JUMP _dv_gcheck;
_dv_odd:
    R3 = LSHIFT R2 BY 2;                  // the even voice of the track wrote R (stereo machine): keep it, clear the flag
    R5 = _dv_st;
    R5 = R5 + R3;
    I1 = R5;
    R3 = DM(0, I1);
    R5 = 0;
    DM(0, I1) = R5;
    R3 = PASS R3;
    IF NE JUMP _dv_odd6;
    I1 = R8;
    MODIFY(I1, -128);
    I0 = R8;
    LCNTR = 32, DO _dv_copy UNTIL LCE;
        R0 = DM(I1, M6);
_dv_copy: DM(I0, M6) = R0;
_dv_odd6:
    R3 = 6;
#ifdef DV_C3_DB
    CALL _dv_crumb (DB);
    NOP;
    NOP;
#else
    CALL _dv_crumb;                       // stage 6: the odd voice's copy is done
#endif
_dv_done:
#ifndef DV_C3_NOSYS
    R0 = DM(-38, I6);
    MODE1 = R0;
    NOP;
#endif
    M5 = DM(-26, I6);
    M6 = DM(-27, I6);
    M7 = DM(-28, I6);
    M13 = DM(-29, I6);
    M14 = DM(-30, I6);
    M15 = DM(-31, I6);
    L0 = DM(-32, I6);
    L1 = DM(-33, I6);
    L2 = DM(-34, I6);
    L3 = DM(-35, I6);
    R3 = 7;
#ifdef DV_C3_DB
    CALL _dv_crumb (DB);
    NOP;
    NOP;
#else
    CALL _dv_crumb;                       // stage 7: the epilogue is done but for the saved R registers and L4 (I4 needs L4 = 0 until here)
#endif
    L4 = DM(-36, I6);
    L5 = DM(-37, I6);
    B3 = DM(-24, I6);                     // a B register load also loads its I register (PRM p.6-24): B3 before I3, as the stock epilogue
    R5 = DM(-7, I6);
    R6 = DM(-8, I6);
    R7 = DM(-9, I6);
    R9 = DM(-10, I6);
    R10 = DM(-11, I6);
    R11 = DM(-12, I6);
    R13 = DM(-13, I6);
    R14 = DM(-14, I6);
    R15 = DM(-15, I6);
    I0 = DM(-16, I6);
    I1 = DM(-17, I6);
    I2 = DM(-18, I6);
    I3 = DM(-19, I6);
    I5 = DM(-20, I6);
    M1 = DM(-21, I6);
    M2 = DM(-22, I6);
    M3 = DM(-23, I6);
    I13 = DM(-25, I6);
    I12 = DM(M7, I6);                     // the return address, as the stock renderer's epilogue
    JUMP (M14, I12) (DB);
    R3 = DM(-6, I6);
    RFRAME;

// ---- the guard, after the routine (docs/findings/51) ----
// The block must hold finite samples of at most 8.0 in magnitude (+18 dB over the full scale of a voice; the stock voices stay below 2). One
// integer test covers both: the magnitude bits (sign cleared) of 8.0 are 0x41000000, and Inf and NaN lie above 0x7f800000. A block that fails
// is zeroed, the voice ends (ACTIVE and the mark clear: the routine takes its note-on path at the next trig) and the fault is counted in the
// type's row; the 8th fault of a type within 1500 blocks (one second) disables the type until the next start (its row names it: the voices are
// silent and the routine is not called). The parameter words the routine saw, the type, the voice and the counts go into the diagnostic words
// 15 to 21 (dl_diag words 17 to 23). Registers: everything is free here (the epilogue reloads them), I13 and the context are restored below.
_dv_gcheck:
    R8 = DM(_dv_g + 20);
    I0 = R8;
    R2 = DM(_dv_g + 12);                  // a stereo block (the track's flag) is 64 words: L, then R in the odd voice's buffer
    R2 = LSHIFT R2 BY 2;
    R3 = _dv_st;
    R3 = R3 + R2;
    I1 = R3;
    R5 = DM(0, I1);
    R4 = 32;
    R6 = 64;
    R5 = PASS R5;
    IF NE R4 = PASS R6;
    R13 = 0;
    R9 = 0x7fffffff;
    LCNTR = R4, DO _dv_gchk UNTIL LCE;
        R0 = DM(I0, M6);
        R0 = R0 AND R9;
_dv_gchk: R13 = MAX(R13, R0);
    R0 = 0x41000000;                      // 8.0
    COMP(R13, R0);
    IF GT JUMP _dv_gfault;
    R5 = DM(_dv_g + 24);
    R5 = PASS R5;
    IF EQ JUMP _dv_done;
    CALL _dv_grestore;
    JUMP _dv_done;

_dv_gfault:
    R12 = 1;                              // 1 = Inf or NaN, 2 = over the level bound (3: note_on returned a negative number)
    R0 = 0x7f800000;
    COMP(R13, R0);
    R3 = 2;
    IF LT R12 = PASS R3;
_dv_gfaultk:
    DM(_dv_g + 32) = R12;
    R3 = DM(_dv_c + 20);
    R3 = PASS R3;
    IF EQ JUMP _dv_gseen;
    I1 = _dv_params;                      // a contract voice: the words the module saw are its parameter buffer
    R14 = _dv_g;
    R3 = 76;
    R14 = R14 + R3;
    I2 = R14;
    LCNTR = 8, DO _dv_gcp UNTIL LCE;
        R3 = DM(I1, M6);
_dv_gcp: DM(I2, M6) = R3;
    JUMP _dv_gpack;
_dv_gseen:
    R6 = 0;                               // the words the routine saw (clamped), packed in pairs
_dv_gcl:
    CALL _dv_gloc;
    R9 = DM(I2, M5);
    R11 = 0x400;
    R11 = R11 OR R10;
    R12 = FEXT R9 BY R11;
    R3 = LSHIFT R6 BY 2;
    R14 = _dv_g;
    R14 = R14 + R3;
    R3 = 76;                              // the words the routine saw
    R14 = R14 + R3;
    I1 = R14;
    DM(I1, M5) = R12;
    R6 = R6 + 1;
    R3 = 8;
    COMP(R6, R3);
    IF LT JUMP _dv_gcl;
_dv_gpack:
    I1 = _dv_g;
    MODIFY(I1, 76);
    R3 = DM(I1, M6);
    R9 = DM(I1, M6);
    R9 = LSHIFT R9 BY 16;
    R3 = R3 OR R9;
    DM(_dv_diag + 68) = R3;
    R3 = DM(I1, M6);
    R9 = DM(I1, M6);
    R9 = LSHIFT R9 BY 16;
    R3 = R3 OR R9;
    DM(_dv_diag + 72) = R3;
    R3 = DM(I1, M6);
    R9 = DM(I1, M6);
    R9 = LSHIFT R9 BY 16;
    R3 = R3 OR R9;
    DM(_dv_diag + 76) = R3;
    R3 = DM(I1, M6);
    R9 = DM(I1, M6);
    R9 = LSHIFT R9 BY 16;
    R3 = R3 OR R9;
    DM(_dv_diag + 80) = R3;
    R5 = DM(_dv_g + 24);
    R5 = PASS R5;
    IF EQ JUMP _dv_gnr;
    CALL _dv_grestore;
_dv_gnr:
    R2 = DM(_dv_g + 12);                  // zero L, and R of a stereo block; the flag is cleared so that the odd voice copies the silent L
    R2 = LSHIFT R2 BY 2;
    R3 = _dv_st;
    R3 = R3 + R2;
    I1 = R3;
    R5 = DM(0, I1);
    R3 = 0;
    DM(0, I1) = R3;
    R4 = 32;
    R6 = 64;
    R5 = PASS R5;
    IF NE R4 = PASS R6;
    R8 = DM(_dv_g + 20);
    I0 = R8;
    LCNTR = R4, DO _dv_gz UNTIL LCE;
_dv_gz: DM(I0, M6) = R3;
    R4 = DM(_dv_g + 16);
    I1 = R4;
    MODIFY(I1, ADM_RECORD_ACTIVE);        // record byte +0x1b8: ACTIVE, +0x1b9 the mark
    R9 = DM(I1, M5);
    R3 = 0xffff0000;
    R9 = R9 AND R3;
    DM(I1, M5) = R9;
    R3 = DM(_dv_g + 0);
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
    R0 = DM(_dv_g + 4);
    R3 = DM(-14, I4);
    COMP(R3, R0);
    IF EQ JUMP _dv_gnd;                   // already disabled
    DM(-14, I4) = R0;
    DM(_dv_diag + 84) = R0;               // the last disabled type
    R3 = DM(_dv_g + 40);
    R3 = R3 + 1;
    DM(_dv_g + 40) = R3;
_dv_gnd:
    R3 = DM(_dv_g + 36);
    R3 = R3 + 1;
    DM(_dv_g + 36) = R3;
    R0 = 0xffff;
    R3 = R3 AND R0;
    R0 = DM(_dv_g + 32);
    R0 = LSHIFT R0 BY 16;
    R3 = R3 OR R0;
    R0 = DM(_dv_g + 40);
    R0 = LSHIFT R0 BY 24;
    R3 = R3 OR R0;
    DM(_dv_diag + 60) = R3;               // faults (16 bits) | kind << 16 | disabled types << 24
    R0 = DM(_dv_g + 4);
    R3 = DM(_dv_g + 8);
    R3 = LSHIFT R3 BY 16;
    R0 = R0 OR R3;
    DM(_dv_diag + 64) = R0;               // type | voice << 16
    JUMP _dv_done;

// A disabled type: a silent block.
_dv_gdis:
    I0 = R8;
    R3 = 0;
    LCNTR = 32, DO _dv_gdz UNTIL LCE;
_dv_gdz: DM(I0, M6) = R3;
    R0 = DM(_dv_g + 4);
    R1 = DM(_dv_g + 8);
    R2 = DM(_dv_g + 12);
    R4 = DM(_dv_g + 16);
    R12 = 32;
    JUMP _dv_back;

// In: R6 = knob j (0 .. 7). Out: I2 = the byte address of the frame copy's 32-bit word that holds the knob's halfword, R10 = the bit position (0 or 16)
// of the half. Knob j of track t is halfword 0x6d + 48 t + j + 2 (j >> 2). Clobbers R9, R10, R11.
_dv_gloc:
    R9 = FEXT R6 BY 2:1;
    R9 = R9 + R9;
    R9 = R9 + R6;
    R10 = DM(_dv_g + 28);
    R9 = R9 + R10;
    R10 = FEXT R9 BY 0:1;
    R10 = LSHIFT R10 BY 4;
    R11 = 0x1fe;
    R9 = R9 AND R11;
    R9 = LSHIFT R9 BY 1;
    R11 = _fw_frame;
    R9 = R9 + R11;
    I2 = R9;
    RTS;

// In: R5 = the mask of the words the clamp changed. Puts the originals back. Clobbers R3, R6, R9 to R14, I1, I2.
_dv_grestore:
    R6 = 0;
_dv_grl:
    R3 = 1;
    R3 = LSHIFT R3 BY R6;
    R3 = R3 AND R5;
    IF EQ JUMP _dv_grn;
    CALL _dv_gloc;
    R9 = DM(I2, M5);
    R3 = LSHIFT R6 BY 2;
    R14 = _dv_g;
    R14 = R14 + R3;
    R3 = 44;
    R14 = R14 + R3;
    I1 = R14;
    R12 = DM(I1, M5);
    R3 = 0xffff;
    R3 = LSHIFT R3 BY R10;
    R3 = NOT R3;
    R9 = R9 AND R3;
    R3 = LSHIFT R12 BY R10;
    R9 = R9 OR R3;
    DM(I2, M5) = R9;
_dv_grn:
    R6 = R6 + 1;
    R3 = 8;
    COMP(R6, R3);
    IF LT JUMP _dv_grl;
    RTS;

// ---- a machine of the SDK contract (the row has a note_on pc), the even voice of track t ----
// In: R0 = type, R1 = voice, R2 = track, R4 = voice record, R8 = output block, I4 = the row's end (word 16 is DM(-8, I4)). The guard's globals
// (_dv_g + 0 .. 28) are set here; the adapter's own scalars are _dv_c: word 0 note-on, 1 pitch word, 2 loop index, 3 state, 5 contract flag.
_dv_contract:
    DM(_dv_g + 4) = R0;
    DM(_dv_g + 8) = R1;
    DM(_dv_g + 12) = R2;
    DM(_dv_g + 16) = R4;
    DM(_dv_g + 20) = R8;
    R5 = 0;
    DM(_dv_g + 24) = R5;                  // no clamped word is put back: the module gets its own copy
    R5 = 1;
    DM(_dv_c + 20) = R5;                  // a contract voice (the fault report reads the parameter buffer)
    R3 = ADM_PARAM_STRIDE;
    R3 = R3 * R2 (SSI);
    R6 = ADM_PARAM_BASE;
    R3 = R3 + R6;
    DM(_dv_g + 28) = R3;                  // halfword of knob 0 of this track (the fault report reads the frame words with it)
    R3 = DM(-14, I4);                     // the row's disabled type
    COMP(R3, R0);
    IF EQ JUMP _dv_gdis;
    I1 = R4;
    MODIFY(I1, ADM_RECORD_ACTIVE);
    R9 = DM(I1, M5);
    R0 = 0xff;
    R0 = R9 AND R0;
    IF EQ JUMP _dv_cidle;
    R12 = 1;
    R11 = 0;
    R0 = 0xff00;
    R0 = R9 AND R0;
    IF EQ R11 = PASS R12;                 // the mark is clear: a note-on in this block
    R3 = LSHIFT R2 BY 2;                  // or the track's block counter is 0: the voice never ran since the start, whatever the mark says
    R6 = _dv_blk;
    R6 = R6 + R3;
    I3 = R6;
    R3 = DM(0, I3);
    R3 = PASS R3;
    IF EQ R11 = PASS R12;
    R11 = PASS R11;
    IF EQ JUMP _dv_cmarked;
    R0 = 0x5a00;                          // the adapter writes the mark
    R9 = R9 OR R0;
    DM(I1, M5) = R9;
_dv_cmarked:
    DM(_dv_c + 0) = R11;
    R2 = DM(_dv_g + 12);                  // actual source track, fixed for this DT2 voice
    DM(_dv_in + DK_IN_TRACK) = R2;
    // pitch = frame word ADM_PITCH_HW + t, the tune knob's word (clamped to the row's range) folded in as (word - 0x4000)
    R0 = ADM_PITCH_HW;
    R0 = R0 + R2;
    CALL _dv_frame_hw;
    DM(_dv_c + 4) = R0;
    R6 = ADM_TUNE_SLOT;
    CALL _dv_pw;
    R1 = DM(_dv_c + 4);
    R0 = R0 + R1;
    R1 = 0x4000;
    R0 = R0 - R1;
    DM(_dv_in + DK_IN_PITCH) = R0;
    R2 = DM(_dv_g + 12);
    R0 = ADM_VELOCITY_HW;
    R0 = R0 + R2;
    CALL _dv_frame_hw;
    DM(_dv_in + DK_IN_VELOCITY) = R0;
    // block counter of the track: 0 on the note-on block
    R2 = DM(_dv_g + 12);
    R2 = LSHIFT R2 BY 2;
    R3 = _dv_blk;
    R3 = R3 + R2;
    I1 = R3;
    R0 = DM(0, I1);
    R1 = 0;
    R11 = DM(_dv_c + 0);
    R11 = PASS R11;
    IF NE R0 = PASS R1;
    DM(_dv_in + DK_IN_BLOCK) = R0;
    R0 = R0 + 1;
    DM(0, I1) = R0;
    R8 = DM(_dv_g + 20);
    DM(_dv_in + DK_IN_OUT) = R8;
    R0 = 128;                             // machine contract 1.2: out_r is the odd voice's block of the pair (the buffers are 128 apart, as `_dv_odd` copies)
    R0 = R0 + R8;
    DM(_dv_in + DK_IN_OUT_R) = R0;
    // the parameter words: knob ADM_PARAM_SLOTi of the track, until a slot of 255 (the buffer's other words stay 0)
    R0 = 0;
    DM(_dv_c + 8) = R0;
_dv_cpl:
    R0 = DM(_dv_c + 8);
    R1 = LSHIFT R0 BY 2;
    R2 = _dv_slots;
    R2 = R2 + R1;
    I1 = R2;
    R6 = DM(0, I1);
    R2 = 255;
    COMP(R6, R2);
    IF EQ JUMP _dv_cpe;
    CALL _dv_pw;
    R1 = DM(_dv_c + 8);
    R2 = LSHIFT R1 BY 2;
    R3 = _dv_params;
    R3 = R3 + R2;
    I1 = R3;
    DM(0, I1) = R0;
    R1 = R1 + 1;
    DM(_dv_c + 8) = R1;
    R2 = DK_MAX_PARAMS;
    COMP(R1, R2);
    IF LT JUMP _dv_cpl;
_dv_cpe:
    // the voice's state
    R2 = DM(_dv_g + 12);
    R3 = DM(_dv_g + 0);
    I4 = R3;
    R1 = DM(-7, I4);                      // state base
    R3 = DM(-6, I4);                      // stride
    R3 = R3 * R2 (SSI);
    R4 = R1 + R3;
    DM(_dv_c + 12) = R4;
    R11 = DM(_dv_c + 0);
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
    R3 = DM(_dv_g + 0);
    I4 = R3;
    R0 = DM(-23, I4);
    I12 = R0;                             // the render pc
    R4 = DM(_dv_c + 12);
    R8 = _dv_in;
    R12 = _dv_params;
    I13 = _dv_rback;
    JUMP (M13, I12);
_dv_rback:
    R2 = DM(_dv_g + 12);                  // contract 1.2: a stereo machine or-s DK_STEREO into the status; the track's flag tells the odd voice to keep the R it wrote
    R2 = LSHIFT R2 BY 2;
    R3 = _dv_st;
    R3 = R3 + R2;
    I1 = R3;
    R1 = DK_STEREO;
    R2 = R0 AND R1;
    DM(0, I1) = R2;
    // DT2 1.17: the stock pan stage (FUN_1c642a, loop 0x1c7040) reads its per-pair gate (1: stereo pair, pan = balance; 0: matrix, both
    // voices summed into both planes) from the even voice's param record word +0xd8 (byte _fw_param_rec + 0xdc * 2t + 0xd8, the base 0x2506ec on 1.17),
    // one frame late (copy at 0x1c7158). Only the stock sample note-on (0x1c335b) writes it. A stereo machine sets it every block.
    R2 = PASS R2;
    IF EQ JUMP _dv_nogate;
    R3 = DM(_dv_g + 12);
    R4 = 0x1b8;
    R3 = R3 * R4 (SSI);
    R4 = _fw_param_rec;
    R3 = R3 + R4;
    R4 = 0xd8;
    R3 = R3 + R4;
    I1 = R3;
    R4 = 1;
    DM(0, I1) = R4;
_dv_nogate:
    R1 = NOT R1;
    R0 = R0 AND R1;
    R1 = DK_DONE;
    COMP(R0, R1);
    IF NE JUMP _dv_rdone;
    R4 = DM(_dv_g + 16);                  // DK_DONE: the voice has ended (ACTIVE and mark clear)
    I1 = R4;
    MODIFY(I1, ADM_RECORD_ACTIVE);
    R9 = DM(I1, M5);
    R3 = 0xffff0000;
    R9 = R9 AND R3;
    DM(I1, M5) = R9;
_dv_rdone:
    R0 = DM(_dv_g + 4);
    R1 = DM(_dv_g + 8);
    R2 = DM(_dv_g + 12);
    R4 = DM(_dv_g + 16);
    R12 = 32;
    JUMP _dv_back;
_dv_nfault:
    R12 = 3;
    JUMP _dv_gfaultk;
// An idle voice (ACTIVE clear): the mark is cleared (a stopped voice), the block is silent.
_dv_cidle:
    R0 = 0xff00;
    R0 = R9 AND R0;
    IF EQ JUMP _dv_gdis;
    R0 = 0xffff00ff;
    R9 = R9 AND R0;
    DM(I1, M5) = R9;
    JUMP _dv_gdis;

// In: R6 = knob j (0 .. 7). Out: R0 = the knob's word of the current track, clamped to the row's declared range of knob j (row word 2 + j: min | max << 16;
// 0: 0 .. 0x7f00), as the guard clamped it in the frame before. Clobbers R9 to R15, I2, I3.
_dv_pw:
    CALL _dv_gloc;
    R9 = DM(I2, M5);
    R11 = 0x400;
    R11 = R11 OR R10;
    R12 = FEXT R9 BY R11;                 // the knob's word
    R13 = DM(_dv_g + 0);
    R14 = LSHIFT R6 BY 2;
    R13 = R13 + R14;
    R14 = 88;
    R13 = R13 - R14;
    I3 = R13;
    R13 = DM(I3, M5);
    R14 = 0x7f000000;
    R13 = PASS R13;
    IF EQ R13 = PASS R14;
    R14 = FEXT R13 BY 16:16;
    R13 = FEXT R13 BY 0:16;
    R0 = MIN(R12, R14);
    R0 = MAX(R0, R13);
    RTS;

_dv_idle:
    MODIFY(I7, -26) (NW);                 // the displaced first instruction of the stock renderer
    JUMP _fw_render_next;

_dv_stock:
    R0 = 7;
    COMP(R1, R0);
    IF LT JUMP _dv_stock_go;              // a type below 7 is one of the engine's own: no breadcrumb
    DM(_dv_sv3) = R3;                     // a type of 7 or more that no module registered: stage 3 (stock path taken) says so
    DM(_dv_sv5) = R5;
    R0 = 32;
    R0 = R0 - R14;
    R0 = LSHIFT R0 BY 8;
    R2 = LSHIFT R1 BY 16;
    R0 = R0 OR R2;
    DM(_dv_bcw) = R0;
    R3 = 3;
#ifdef DV_BISECT_C2
    // Bisect build (docs/PATCHING.md "Rung C2"): the stage 3 breadcrumb written inline, the body of _dv_crumb with no CALL and no RTS.
    // R0 is the breadcrumb word without its stage, I4 is free (L4 is 0).
    R3 = R3 OR R0;
    R5 = DM(_dv_diag + 52);
    R5 = R5 + 1;
    DM(_dv_diag + 52) = R5;               // the dispatch counter counts the stage 3 passes here (the routine is never called, the returns stay 0)
    I4 = _dv_diag;
    DM(12, I4) = R3;
    I4 = _dv_rec;
    R5 = DM(0, I4);
    R5 = PASS R5;
    IF EQ JUMP _dv_i1;
    I4 = R5;
    DM(192, I4) = R3;
    R5 = 0x20D1A6;
    DM(45, I4) = R5;
    R5 = DM(_dv_diag + 52);
    DM(193, I4) = R5;
    R5 = DM(_dv_diag + 56);
    DM(213, I4) = R5;
    I4 = _dv_rec;
_dv_i1:
    R5 = DM(1, I4);
    R5 = PASS R5;
    IF EQ JUMP _dv_i2;
    I4 = R5;
    DM(192, I4) = R3;
    R5 = 0x20D1A6;
    DM(45, I4) = R5;
    R5 = DM(_dv_diag + 52);
    DM(193, I4) = R5;
    R5 = DM(_dv_diag + 56);
    DM(213, I4) = R5;
_dv_i2:
#else
#ifdef DV_C3_DB
    CALL _dv_crumb (DB);
    NOP;
    NOP;
#else
    CALL _dv_crumb;
#endif
#endif
    R3 = DM(_dv_sv3);
    R5 = DM(_dv_sv5);
_dv_stock_go:
    R0 = DM(_dv_sv_l4);
    L4 = R0;                              // the context's L4 for the stock code
    MODIFY(I7, -26) (NW);                 // the displaced first instruction of the stock renderer
    JUMP _fw_render_next;

// The breadcrumb (docs/findings/48-dsp-breadcrumbs.md). In: R3 = stage (1 hook entered, 2 type registered, 3 stock path taken for a type of 7
// or more, 4 routine about to be called, 5 routine returned, 6 odd copy done, 7 epilogue done); the word is stage | v << 8 | type << 16
// (`_dv_bcw` holds the upper parts). Out: nothing. Clobbers R3, R5, I4 (L4 must be 0). It goes into the DSP's diagnostic word 12 (copied
// into the reply record at the end of the block by core/dsp-link) and, at once, into both reply records (the two rings; `_dv_rec` holds
// their addresses, written by core/dsp-link's block tail, 0 before the first block or without that module): a core that stops inside the
// routine never reaches the block tail, so the record is where the last breadcrumb has to be already. The header and the count of
// dispatch and return counts go with it, so that a record the engine had cleared still decodes.
_dv_crumb:
    R5 = DM(_dv_bcw);
    R3 = R3 OR R5;
    I4 = _dv_diag;
    DM(12, I4) = R3;
    I4 = _dv_rec;
    R5 = DM(0, I4);
    R5 = PASS R5;
    IF EQ JUMP _dv_cb;
    I4 = R5;
    DM(192, I4) = R3;
    R5 = 0x20D1A6;
    DM(45, I4) = R5;
    R5 = DM(_dv_diag + 52);
    DM(193, I4) = R5;
    R5 = DM(_dv_diag + 56);
    DM(213, I4) = R5;
    I4 = _dv_rec;
_dv_cb:
    R5 = DM(1, I4);
    R5 = PASS R5;
    IF EQ JUMP _dv_cc;
    I4 = R5;
    DM(192, I4) = R3;
    R5 = 0x20D1A6;
    DM(45, I4) = R5;
    R5 = DM(_dv_diag + 52);
    DM(193, I4) = R5;
    R5 = DM(_dv_diag + 56);
    DM(213, I4) = R5;
_dv_cc:
#ifdef DV_C3_DB
    RTS (DB);
    NOP;
    NOP;
#else
    RTS;
#endif

// Clears the module step, the static word `_dk_step` (fault table word 25, dsp-link.inc; its address is _dv_rec[2], written by core/dsp-link), before the core
// calls a module. Clobbers R3, R5, I4 (L4 must be 0).
_dv_clrstep:
    I4 = _dv_rec;
    R5 = DM(2, I4);
    R5 = PASS R5;
    IF EQ RTS;
    I4 = R5;
    R3 = 0;
    DM(0, I4) = R3;
    RTS;

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
#ifdef DV_C3_DB
    RTS (DB);
    NOP;
    NOP;
#else
    RTS;
#endif

// R0 = the unsigned 16-bit word k of the DSP's frame copy (R0 = k, 0..511); _dv_frame_sw sign-extends.
// Clobbers R0-R2, I4.
.GLOBAL _dv_frame_hw;
.GLOBAL _dv_frame_sw;
_dv_frame_hw:
    R1 = 0x1fe;
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
#ifdef DV_C3_DB
    RTS (DB);
    NOP;
    NOP;
#else
    RTS;
#endif
_dv_frame_sw:
    R1 = 0x1fe;
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
#ifdef DV_C3_DB
    RTS (DB);
    NOP;
    NOP;
#else
    RTS;
#endif

.SECTION/DM seg_dmda;
.PKDATA _dv_sv_l4, 4;
.PKDATA _dv_mask, 4;                      // registered types (see _dv_nodiag)
.PKDATA _dv_sv3, 4;
.PKDATA _dv_sv5, 4;
.PKDATA _dv_bcw, 4;
// The guard's scalars and arrays (words): 0 row end, 1 type, 2 voice, 3 track, 4 voice record, 5 output block, 6 mask of clamped knobs, 7 halfword of knob 0,
// 8 fault kind, 9 faults, 10 disabled types, 11 to 18 the originals of the clamped words, 19 to 26 the words the routine saw.
.PKDATA _dv_g, 128;
// The diagnostic block (finding 45): word 0 counts the blocks that reached the hook, words 1-11 are M5, M6, M7, M13, M14, M15,
// I6, I7, B7, L7, MODE1 as the firmware left them at the first voice's entry; word 12 is the last breadcrumb, word 13 counts the
// dispatches to registered types, word 14 the routines that returned (finding 48). core/dsp-link copies the block into the record it sends to the ColdFire each block
// (dl_diag): its words 2 to 15.
.PKDATA _dv_diag, 128;
// The addresses of the two reply records (rings): [0] the one the last block built, [1] the one before it, and [2] the address of the module step word
// `_dk_step` (cleared before a module call). Written by core/dsp-link.
.PKDATA _dv_rec, 12;
// The contract adapter's data: the `dk_voice_in` it hands to the module, the parameter words (8), the blocks since the note-on per track (16), its scalars
// (see `_dv_contract`) and the knob positions of the module's parameters in order (ADM_PARAM_SLOT0 .. 7; 255: none).
.PKDATA _dv_in, 24;
.PKDATA _dv_params, 32;
.PKDATA _dv_blk, 64;
.PKDATA _dv_c, 64;
// Per track: nonzero while the even voice has written R into the odd voice's block this block (contract 1.2); the odd voice consumes it.
.PKDATA _dv_st, 64;
.VAR _dv_slots = ADM_PARAM_SLOT0;
.VAR _dv_slots1 = ADM_PARAM_SLOT1;
.VAR _dv_slots2 = ADM_PARAM_SLOT2;
.VAR _dv_slots3 = ADM_PARAM_SLOT3;
.VAR _dv_slots4 = ADM_PARAM_SLOT4;
.VAR _dv_slots5 = ADM_PARAM_SLOT5;
.VAR _dv_slots6 = ADM_PARAM_SLOT6;
.VAR _dv_slots7 = ADM_PARAM_SLOT7;
