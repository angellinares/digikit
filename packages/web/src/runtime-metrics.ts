import type { RuntimeSnapshot } from './runtime';

// Host wall time stays outside the guest clock. Sample once per response,
// never per guest instruction; export only when explicitly requested.
export function hostMetrics(kind: 'wasm_abi' | 'native_ipc') {
  let started: number | undefined, loadMs: number | undefined;
  let chunks = 0, stepMs = 0, maxStepMs = 0;
  let firstFrameMs: number | undefined, firstMainMs: number | undefined, readyMs: number | undefined;
  return {
    reset() { started = performance.now(); loadMs = undefined; chunks = 0; stepMs = 0; maxStepMs = 0; firstFrameMs = undefined; firstMainMs = undefined; readyMs = undefined; },
    loaded() { if (started !== undefined) loadMs = performance.now() - started; },
    step(begin: number, end: number, snapshot: RuntimeSnapshot) {
      chunks += 1; const elapsed = end - begin; stepMs += elapsed; maxStepMs = Math.max(maxStepMs, elapsed);
      if (started === undefined) return;
      if (snapshot.frame && firstFrameMs === undefined) firstFrameMs = end - started;
      if (snapshot.frame && snapshot.status.frame_source === 'main' && firstMainMs === undefined) firstMainMs = end - started;
      if (snapshot.status.ready && readyMs === undefined) readyMs = end - started;
    },
    report() { return { kind, load_response_wall_ms: loadMs, elapsed_wall_ms: started === undefined ? undefined : performance.now() - started, chunks, step_response_wall_ms: stepMs, max_step_response_wall_ms: maxStepMs, first_frame_response_wall_ms: firstFrameMs, first_main_response_wall_ms: firstMainMs, ready_response_wall_ms: readyMs }; },
  };
}
