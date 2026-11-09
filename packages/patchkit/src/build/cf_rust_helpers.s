/* ColdFire ISA_C compiler helpers, cdecl. compiler_builtins' weak bswap
 * implementation lowers to a call to itself with this GCC backend. A strong
 * target implementation is linked before the Rust archive and made private by
 * the normal helper-localization pass. MCF5441xRM: BYTEREV, pp. 97/113. */
    .section .text.__bswapsi2,"ax",@progbits
    .balign 2
    .globl __bswapsi2
    .type __bswapsi2,@function
__bswapsi2:
    move.l 4(%sp),%d0
    byterev.l %d0
    rts
    .size __bswapsi2,.-__bswapsi2
