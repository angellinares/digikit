/* test/svc-test: a service provider of the test-only service svc-test (docs/plans/runtime-and-services.md section 13.2).
 *
 * It exports dk_svctest_ping, which returns 42. The test-only import has no public header, so the prototype is declared here.
 */

#include <dk/core/module.h>

int dk_svctest_ping(void);

int dk_svctest_ping(void)
{
    return 42;
}

static int32_t init(dk_module_ctx *ctx)
{
    (void)ctx;
    return DK_OK;
}

const dk_module_def dk_module = {.init = init};
