| core/params on the Digitakt II (1.16, 1.17): the one run-time routine.
|
| The framework's work is data: patchkit copies the firmware's parameter table (60-byte rows, finding 02) to
| pf_table with one row appended per `[[entry]] registry = "params"`, repoints every reference to the table and raises
| every clamp bound by params.count (the write ops of module.toml). What remains for code is the accessor of the
| second per-code table (value-class objects built at boot, 0x44 bytes each, indexed by parameter code): an added code
| answers with its template's object.
|
| Hook (displaced = "run") on `cmpi.l #stock count,D0` after the accessor's `move.l 4(sp),D0`: an added code (>=
| params.first, < params.first + params.count) in d0 becomes its entry's template code, and a stock code with a
| `param_edits` entry whose class is not 0 becomes that class; then the stock clamp runs (both are stock codes, below the
| count). Every other register is preserved.

    .text
    .equ P_TEMPLATE, 8          | row layout (module.toml [[registry.field]] order): name, short, template, slot, ...
    .equ E_CLASS, 20            | param_edits rows: code, max, formatter, name, short, class

    .globl pf_t2
pf_t2:
    lea     -8(%sp),%sp
    movem.l %d1/%a0,(%sp)
    move.l  %d0,%d1
    subi.l  #_pf_first,%d1
    cmpi.l  #_pf_count,%d1
    bcc.s   1f
    move.l  #_pf_rowsize,%d0
    muls.l  %d1,%d0
    addi.l  #_pf_rows,%d0
    movea.l %d0,%a0
    move.l  P_TEMPLATE(%a0),%d0
    bra.s   2f
1:  move.l  %d1,%d0
    addi.l  #_pf_first,%d0
    move.l  #_pe_count,%d1
    beq.s   2f
    lea     _pe_rows,%a0
3:  cmp.l   (%a0),%d0
    bne.s   4f
    tst.l   E_CLASS(%a0)
    beq.s   2f
    move.l  E_CLASS(%a0),%d0
    bra.s   2f
4:  adda.l  #_pe_rowsize,%a0
    subq.l  #1,%d1
    bne.s   3b
2:  movem.l (%sp),%d1/%a0
    lea     8(%sp),%sp
    jmp     _pf_t_t2
