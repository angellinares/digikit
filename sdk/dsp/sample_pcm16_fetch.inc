// Caller-owned raw packed-PCM16 containing-word fetch, SHARC SISD only.
// BASE and nonnegative INDEX_A/INDEX_B are normal-word source base/frame indices
// under sample_pcm16.inc's layout. Caller validates bounds and clamps neighbors.
// Both BASE+floor(INDEX/2) addresses must be valid, nonwrapping and in one
// normal-word address space, with both FULL 32-bit words addressable (including
// the unused high half of an odd terminal word). No validation occurs here.
//
// All Dreg operands are distinct, except INDEX_A and INDEX_B may be the same
// register. BASE/indices are preserved; WORD_A/WORD_B/ADDR, DAG and integer
// condition flags are clobbered. DAG is caller-owned with its L register zero;
// PEYEN must be clear. SIMD may otherwise add companion accesses.
// Caller must own/protect BOTH containing words across BOTH reads. This macro
// supplies no atomicity, admission, lifetime, mask, cache or allocation policy.
// No DM writes, storage, labels, unpacking, conversion, interpolation or ABI.
// Eight instructions, not cycles: adjacent DAG write/use is hardware-interlocked
// (public SHARC+ PRM Rev1.5 printed4-63/64); stalls are not model instruction counts.
#ifndef DK_SAMPLE_PCM16_FETCH_INC
#define DK_SAMPLE_PCM16_FETCH_INC

#define DK_SAMPLE_PCM16_FETCH_WORDS(BASE, INDEX_A, INDEX_B, WORD_A, WORD_B, ADDR, DAG) \
    ADDR = LSHIFT INDEX_A BY -1; \
    ADDR = BASE + ADDR; \
    DAG = ADDR; \
    WORD_A = DM(0, DAG); \
    ADDR = LSHIFT INDEX_B BY -1; \
    ADDR = BASE + ADDR; \
    DAG = ADDR; \
    WORD_B = DM(0, DAG)

#endif
