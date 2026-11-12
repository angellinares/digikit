/* <dk/core/module.h>: the entry point of a module and the service imports it calls. Hand-written (docs/plans/sdk.md section 5).

   A module defines one constant, `dk_module`: its single entry point. `patchkit pack` finds it by its symbol and writes `init`, `fini`, `tick` and
   `menu` into the .dkm header; the runtime calls them with a `dk_module_ctx`. Every field is optional (NULL). There is no Api table and no `dkm_*` symbol.

     #include <dk/core/module.h>              first: the service headers below take their new names only after it
     #include <dk/core/log.h>
     #include <dk/platform/settings.h>

     static dk_settings *settings;

     static int32_t init(dk_module_ctx *ctx)
     {
         int32_t r = dk_settings_open(ctx, &settings);
         if (r != DK_OK)
             return r;
         dk_log(DK_LOG_INFO, "ready", 0);
         return DK_OK;
     }

     const dk_module_def dk_module = {.init = init};

   The type is `dk_module_def` and the constant is `dk_module` (the packer looks the symbol `dk_module` up). The parameter of every function that takes the module
   context is named `ctx`; a user-data pointer of a callback is named `arg`.

   - init(ctx): once, after the module is copied and relocated. Opens the services (`dk_*_open`), registers host commands. A result other than DK_OK
     unloads the module (the load fails with DK_E_INIT).
   - fini(ctx): when the module is unloaded (a failed load, or the runtime's teardown).
   - tick(ctx): once per pass of the UI task, for every loaded module, after init has succeeded. It must be short and must not block.
   - menu(ctx, menu): when the user opens the module's SETTINGS... in DIGIKIT > MODULES; it adds the rows of `menu` (core/ui must be in the image).

   `dk_module_ctx` is opaque: the runtime passes it to every lifecycle function and takes it back in the `_open` calls. The imports are in <dk/gen/imports.h>
   (generated from sdk/sdk.toml, with the context each may run in and the api minor that added it); the headers of the services (core/log.h,
   core/mem.h, core/time.h and the headers of platform/) add the constants and the rules of each. A service another module provides (core/ui, core/input,
   core/usb-bulk, core/dsp-link) may be absent: its `_open` returns DK_E_MISSING and the module decides whether it can run without it.

   Until the modules move to this entry point (docs/plans/sdk.md S2b-2) the old one, `dkm_init(const dk_api *)` with the Api table and the
   `dkm_slot_<n>` functions, still works: a module uses one of the two. The headers keep their old declarations for it; they are visible only when
   this header was not included before them. */
#ifndef DK_MODULE_H
#define DK_MODULE_H

#if defined(DK_SETTINGS_LEGACY) || defined(DK_INTRO_LEGACY)
#error "include <dk/core/module.h> before platform/settings.h and platform/intro.h"
#endif

#include <stdint.h>
#include <dk/core/status.h>
#include <dk/gen/imports.h>

#ifndef DK_OK
#define DK_OK 0 /* success; a failure is a negative DK_E_* code (<dk/core/status.h>) */
#endif

typedef struct dk_module_def {
    int32_t (*init)(
        dk_module_ctx *ctx); /* declare content, open services; DK_OK or a negative DK_E_* */
    void (*fini)(dk_module_ctx *ctx);                   /* the module goes away */
    void (*tick)(dk_module_ctx *ctx);                   /* every UI pass, for every loaded module */
    void (*menu)(dk_module_ctx *ctx, dk_ui_menu *menu); /* the module's SETTINGS... rows */
} dk_module_def;

/* The constant every module defines (the packer reads its four function pointers). */
extern const dk_module_def dk_module;

#endif
