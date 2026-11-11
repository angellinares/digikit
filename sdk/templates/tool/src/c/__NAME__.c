/* __ID__: a service module (docs/design/module-sdk.md).
 *
 * dkm_init runs when the runtime loads the module; dkm_fini when it unloads it.
 *
 * To add SysEx commands, allocate a range in sdk/sdk.toml [protocol.ranges] and register it in dkm_init:
 *
 *     api->register_cmds(FIRST_CMD, COUNT, handler);
 *
 * Trace marks (optional, no code without `pack --trace`): `#include <dk/core/trace.h>`, call `DK_TRACE_INIT(api);` in dkm_init, then
 * `DK_TRACE_LOG("message", value);` anywhere in this file. See sdk/README.md, "Tracing".
 */

#include <dk/core/api.h>

int32_t dkm_init(const dk_api *api)
{
    api->log_lv(2, (const uint8_t *)"__NAME__: hello", 0);
    return 0;
}

void dkm_fini(const dk_api *api)
{
    (void)api;
}
