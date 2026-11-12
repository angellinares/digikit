/* <dk/platform/input.h>: panel keys and encoders from software (needs core/input). Hand-written. Include <dk/core/module.h> first.

   `dk_input_open(ctx, &in)` in `init`; without core/input it returns DK_E_MISSING and the module decides what to do. `dk_input_key(in, code, down)` presses
   or releases a panel key and `dk_input_turn(in, encoder, delta)` turns an encoder, through the firmware's own panel decoders. The key codes and
   the encoder numbers are the device file's ([input], [panel]); the results are the adapter's (0, or a negative code). UI task. */
#ifndef DK_INPUT_H
#define DK_INPUT_H
#include <dk/core/module.h>
#endif
