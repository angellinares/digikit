/* Read and write a machine parameter from a DK_PARAM_ACTION callback (API 1.6: param_get, param_set; docs/design/ui-rules.md).
   Both calls work only inside the callback, in the UI task. WORD counts the module's DK_PARAM rows in declaration order. Values are raw
   (unit * 256, like DK_PARAM min/max/default): a 0..15 parameter is 0..0x0f00. */
#ifndef DK_PARAMS_H
#define DK_PARAMS_H

#define DK_PARAM_E_STATE (-0x10001) /* called outside a click callback */
#define DK_PARAM_E_ARG   (-0x10002) /* a track above 15, or a word the module does not have */
#define DK_PARAM_E_HELD  (-0x10003) /* a mode key (TRIG, FUNC) is held: nothing was written */

/* param_get returns a halfword (-0x8000 to 0x7fff) or a status below -0x8000. */
#define DK_PARAM_IS_ERROR(v) ((v) < -0x8000)

/* int32_t param_get(track, word)        the value the page shows for `track`
   int32_t param_set(track, word, raw)   0 or a status; the stock setter clamps raw to the parameter's range */

#endif
