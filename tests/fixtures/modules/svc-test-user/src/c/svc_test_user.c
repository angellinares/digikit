/* test/svc-test-user: an ordinary module that calls dk_svctest_ping of the test-only service svc-test.
 *
 * The import names the service, so the packer writes a hard NEED on it. The test-only import has no public header: the prototype is declared here.
 */

#include <dk/core/module.h>

int dk_svctest_ping(void);

int svc_test_result;

static int32_t init(dk_module_ctx *ctx)
{
    (void)ctx;
    svc_test_result = dk_svctest_ping();
    return DK_OK;
}

const dk_module_def dk_module = {.init = init};
