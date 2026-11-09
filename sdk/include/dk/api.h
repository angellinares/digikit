/* <dk/api.h>: the dk_api table and the run-time imports for content modules.
   Hand-written. The layouts come from the generated <dk/gen/api.h> (from sdk/sdk.toml). */
#ifndef DK_API_H
#define DK_API_H
#include <dk/gen/api.h>

/* True when the table is long enough to hold FIELD and FIELD is set. */
#define DK_API_HAS(api, field) \
    ((api) && (api)->size >= offsetof(dk_api, field) + sizeof((api)->field) && (api)->field)
#endif
