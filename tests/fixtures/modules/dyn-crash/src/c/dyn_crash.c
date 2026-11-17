/* test/dyn-crash: the one fault injector of the fixtures (docs/findings/46-digikit-debug.md, docs/findings/37-boot-load.md).
 *
 * The delay, the kind and the place come from the store file `cfg/crash-after.bin`; no file: never faults.
 *   bytes 0 to 3  u32 big endian: DSP blocks after the first tick before a fault in `tick` (the runtime's `dkrt_blocks` import, 1500 a second)
 *   byte 4        mode: 0 = illegal instruction, 1 = division by zero, 2 = reset the machine (SOFTRST, as the firmware's exception path ends),
 *                 3 = hang in the UI task with the other tasks running, 4 = reset the machine in `init` and never return
 *   byte 5        where (optional, default 0): 0 = in `tick`, 2 = in the frame callback of the boot animation (`init` takes it with
 *                 `dk_intro_take`; the callback runs on the intro task and makes no runtime call; modes 0 and 2; the delay is not used)
 * Just before an illegal instruction or a division by zero in `tick` it logs `crash-pc` with the address of that instruction, which is the
 * program counter the crash record must show. (A read of an unmapped address is no good as another kind: the emulator reports it as a bus fault
 * of the run, not as an exception the guest can handle.) */

#include <dk/core/log.h>
#include <dk/core/module.h>
#include <dk/core/time.h>
#include <dk/platform/files.h>
#include <dk/platform/intro.h>

typedef uint32_t u32;

#define RCM_RCR (*(volatile unsigned char *)0xEC090000)
#define SOFTRST 0x80

static u32 after = 0xffffffffu;
static volatile u32 mode;
static u32 in_intro; /* the fault is the frame callback's: `tick` stays quiet */
static u32 start;
static u32 started;
static uint8_t blank[DK_INTRO_BYTES] __attribute__((aligned(4)));

/* the boot animation's frame callback: it faults on its first call (intro task, no runtime call) */
static const uint8_t *frame(uint32_t tick, void *arg)
{
    (void)tick;
    (void)arg;
    if (mode == 0) {
        __asm__ volatile(".short 0x4afc"); /* illegal instruction */
    } else {
        RCM_RCR = SOFTRST;
        for (;;) {
        }
    }
    return blank;
}

static int32_t init(dk_module_ctx *ctx)
{
    unsigned char cfg[16];
    u32 where = 0;
    dk_files *files;
    int32_t r = dk_files_open(ctx, &files);
    if (r != DK_OK)
        return r;
    int n = dk_files_read(files, "cfg/crash-after.bin", cfg, sizeof cfg);
    if (n >= 5) {
        after = ((u32)cfg[0] << 24) | ((u32)cfg[1] << 16) | ((u32)cfg[2] << 8) | cfg[3];
        mode = cfg[4];
        if (n >= 6)
            where = cfg[5];
    }
    dk_log(DK_LOG_INFO, "crash-after", after);
    if (n >= 5 && mode == 4) {
        RCM_RCR = SOFTRST;
        for (;;) {
        }
    }
    if (n >= 5 && where == 2) {
        in_intro = 1;
        int32_t rc = dk_intro_take(frame, 0, 0, 30);
        dk_log(DK_LOG_INFO, "crash: intro take", (uint32_t)rc);
    }
    return DK_OK;
}

static void tick(dk_module_ctx *ctx)
{
    u32 now = dk_time_blocks();
    register u32 pc;
    (void)ctx;
    if (!started) {
        started = 1;
        start = now;
        return;
    }
    if (in_intro || now - start < after) {
        return;
    }
    if (mode == 0) {
        __asm__ volatile("lea 2f(%%pc),%0" : "=a"(pc));
        dk_log(DK_LOG_INFO, "crash-pc", pc);
        __asm__ volatile("2: .short 0x4afc");
    } else if (mode == 1) {
        /* a division by zero; the divisor is read through a volatile so that the compiler cannot see it */
        volatile u32 zero = 0;
        register u32 x = 1234;
        __asm__ volatile("lea 3f(%%pc),%0" : "=a"(pc));
        dk_log(DK_LOG_INFO, "crash-pc", pc);
        __asm__ volatile("3: divu.l %1,%0" : "+d"(x) : "d"(zero));
    } else if (mode == 2) {
        RCM_RCR = SOFTRST;
    }
    for (;;) {
    }
}

const dk_module_def dk_module = {.init = init, .tick = tick};
