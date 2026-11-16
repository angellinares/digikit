/* test/svc-test-user: an ordinary module that calls dk_svctest_ping of the test-only service svc-test.
 *
 * The imports name the service, so the packer writes a hard NEED on it, and the loader resolves them to the exports of the provider that loaded before.
 * `init` logs the ping result (42); `tick` logs how often the provider's hook ran, each time the count differs from the last one logged. The device test
 * reads both from the runtime log. The test-only imports have no public header: the prototypes are declared here.
 */

#include <dk/core/log.h>
#include <dk/core/module.h>

int dk_svctest_ping(void);
uint32_t dk_svctest_hits(void);

int svc_test_result;
static uint32_t last = 0xffffffffu;

static int32_t init(dk_module_ctx *ctx)
{
    (void)ctx;
    svc_test_result = dk_svctest_ping();
    dk_log(DK_LOG_INFO, "svctest-ping", (uint32_t)svc_test_result);
    return DK_OK;
}

static void tick(dk_module_ctx *ctx)
{
    uint32_t n = dk_svctest_hits();
    (void)ctx;
    if (n != last) {
        last = n;
        dk_log(DK_LOG_INFO, "svctest-hits", n);
    }
}

const dk_module_def dk_module = {.init = init, .tick = tick};
