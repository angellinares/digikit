/* test/card-info: read-only diagnostic of the eMMC card and its controller path (tools/device/dkcardinfo.py decodes the replies).
 * It writes nothing: no register, no memory. It takes the commands 0x28 and 0x29 of the `file_write_test_b` range of sdk/sdk.toml, so it must not be
 * loaded together with test/file-write-b.
 *
 * 0x28 EXT_CSD  body (optional): u16 offset, u16 length (default 0 and 512; at most 512 in all)
 *               reply: u8 format (1), u8 device id (api->device: 1 DT2, 2 DN2), u32 address read, u16 offset, u16 length, the bytes.
 *               The bytes are the firmware's own copy of the EXT_CSD: its init reads it once with CMD8 (argument 0x083a0010) through eDMA channel 59 into
 *               the uncached alias of RAM below (the eSDHC bounce buffer follows at +0x200). The address is a constant per device (the init code is
 *               DT2 1.17 0x4012d78e, DN2 1.12 0x4012be52; both write the destination address to the TCD of channel 59 and read SEC_COUNT back at +0xd4).
 * 0x29 REGS     no body. reply: u8 format (1), u8 device id, u8 n, u8 0, then n pairs u32 address, u32 value, in the order of `regs` below. Every
 *               register of the list is read-only status or configuration (no register that clears or pops on read: no DATPORT, no ULPI viewport,
 *               no DMA timer or UART data). */
#include <dk/core/api.h>

typedef uint8_t u8;
typedef uint16_t u16;
typedef uint32_t u32;
typedef int32_t i32;

#define CMD_EXTCSD 0x28u
#define CMD_REGS 0x29u
#define E_ARG (-50)
#define EXTCSD_LEN 512u

static const u32 regs[] = {
    /* eSDHC (0xFC0CC000): PRSSTAT PROCTL SYSCTL HOSTCAPBLT WML VENDOR HOSTVER */
    0xFC0CC024u,
    0xFC0CC028u,
    0xFC0CC02Cu,
    0xFC0CC040u,
    0xFC0CC044u,
    0xFC0CC0C0u,
    0xFC0CC0FCu,
    /* PLL: PLL_CR PLL_DR PLL_SR */
    0xFC0C0000u,
    0xFC0C0004u,
    0xFC0C0008u,
    /* USB OTG (0xFC0B0000): USBCMD USBSTS BURSTSIZE TXFILLTUNING PORTSC1 MODE */
    0xFC0B0140u,
    0xFC0B0144u,
    0xFC0B0160u,
    0xFC0B0164u,
    0xFC0B0184u,
    0xFC0B01A8u,
    /* crossbar: XBS_PRS0 XBS_CRS0 XBS_PRS2 XBS_CRS2 */
    0xFC004000u,
    0xFC004010u,
    0xFC004200u,
    0xFC004210u,
    /* eDMA TCD of channel 59 (0xFC045760, 32 bytes) as eight longwords */
    0xFC045760u,
    0xFC045764u,
    0xFC045768u,
    0xFC04576Cu,
    0xFC045770u,
    0xFC045774u,
    0xFC045778u,
    0xFC04577Cu,
};
#define NREGS ((u32)(sizeof regs / sizeof regs[0]))

static const dk_api *sdk;

static void put16(u8 *p, u32 v)
{
    p[0] = (u8)(v >> 8);
    p[1] = (u8)v;
}

static void put32(u8 *p, u32 v)
{
    p[0] = (u8)(v >> 24);
    p[1] = (u8)(v >> 16);
    p[2] = (u8)(v >> 8);
    p[3] = (u8)v;
}

static u32 extcsd_addr(void)
{
    if (sdk->device == 1)
        return 0x4fe69100u; /* DT2 1.17 */
    if (sdk->device == 2)
        return 0x4e6f9100u; /* DN2 1.12 */
    return 0;
}

static i32 command(u32 cmd, u32 seq, const u8 *body, u32 len, u8 *out)
{
    (void)seq;
    u32 base = extcsd_addr();
    if (cmd == CMD_EXTCSD) {
        u32 off = 0, n = EXTCSD_LEN;
        if (base == 0)
            return E_ARG;
        if (len >= 4) {
            off = ((u32)body[0] << 8) | body[1];
            n = ((u32)body[2] << 8) | body[3];
        }
        if (off > EXTCSD_LEN || n > EXTCSD_LEN - off)
            return E_ARG;
        out[0] = 1;
        out[1] = (u8)sdk->device;
        put32(out + 2, base);
        put16(out + 6, off);
        put16(out + 8, n);
        const volatile u8 *src = (const volatile u8 *)(base + off);
        for (u32 i = 0; i < n; i++)
            out[10 + i] = src[i];
        return (i32)(10 + n);
    }
    if (cmd == CMD_REGS) {
        out[0] = 1;
        out[1] = (u8)sdk->device;
        out[2] = (u8)NREGS;
        out[3] = 0;
        for (u32 i = 0; i < NREGS; i++) {
            put32(out + 4 + 8 * i, regs[i]);
            put32(out + 8 + 8 * i, *(const volatile u32 *)regs[i]);
        }
        return (i32)(4 + 8 * NREGS);
    }
    return E_ARG;
}

void dkm_fini(const dk_api *api)
{
    (void)api;
}

i32 dkm_init(const dk_api *api)
{
    if (api->size < sizeof(dk_api))
        return -48;
    sdk = api;
    return api->register_cmds(CMD_EXTCSD, 2, command);
}
