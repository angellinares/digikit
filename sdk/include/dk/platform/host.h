/* <dk/platform/host.h>: SysEx commands of the module (dk_host_register). Hand-written. Include <dk/core/module.h> first.

   dk_host_register(first, count, fn) registers the SysEx commands `first .. first + count` of the digikit protocol with the handler `fn` (a `dk_host_cmd_fn`). Call it in
   `init`. The range must lie inside one of the ranges that sdk/sdk.toml [protocol.ranges] reserves and overlap no registered one: the result is
   DK_E_CMD_RANGE otherwise. The range is released with the module.

   `fn(cmd, seq, body, len, out)` runs in the firmware's MIDI task: keep it short and never wait. `body` is the request body (`len` bytes, at most
   160); the reply body goes to `out` (at most 879 bytes). The handler returns the number of reply bytes (0 or more: status ok), or a negative
   status that is sent as the reply's status with no body. The settings and file calls are not for a handler: it can queue the work for `tick`. */
#ifndef DK_HOST_H
#define DK_HOST_H
#include <dk/core/module.h>
#endif
