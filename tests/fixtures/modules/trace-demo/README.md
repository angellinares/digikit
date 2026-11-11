# Trace demo (test)

The effect of `test/dyn-fx-gain` with trace marks: `DK_TRACE` and `DK_TRACE_INIT` (`<dk/core/trace.h>`) in the ColdFire part, `DK_STEP` (`dk_trace.inc`) in the DSP part.
Built without `--trace` the marks are gone and the `.dkm` equals the one of the sources without them; built with `--trace` it carries the trace
bit and imports `_dk_step`. The test is `packages/patchkit/tests/trace.rs`.
