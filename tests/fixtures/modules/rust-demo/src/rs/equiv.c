/* The C counterpart of pk_rs_hash (code size comparison only; not built into the module). */
typedef unsigned char u8;
typedef unsigned int u32;
typedef unsigned long long u64;
u32 pk_c_hash(const u8 *p, u32 n)
{
    u64 h = 0xcbf29ce484222325ULL;
    for (u32 i = 0; i < n; i++)
        h = (h ^ p[i]) * 0x00000100000001b3ULL;
    return (u32)(h >> 32) ^ (u32)h;
}
