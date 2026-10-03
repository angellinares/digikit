import type { RuntimeSnapshot } from './runtime';

// Counts only this session's emitted interleaved stereo values, never restored
// DSP instruction totals. Wall time excludes load but includes pause/yield time.
export function pcmMetrics(values: number, wallMs: number) {
  const seconds = values / (2 * 48_000);
  const elapsed = Math.max(0, wallMs) / 1000;
  return { pcm_produced_seconds: seconds, production_elapsed_wall_seconds: elapsed, pcm_seconds_per_wall_second: elapsed > 0 ? seconds / elapsed : 0 };
}

// Host wall time stays outside the guest clock. Fixed-size buckets per response,
// never per guest instruction. The final bucket contains values above 100 ms.
export function hostMetrics(kind: 'wasm_abi' | 'native_ipc') {
  const bounds = [1, 2, 5, 10, 20, 50, 100];
  const buckets = new Array<number>(bounds.length + 1).fill(0);
  const gapBuckets = new Array<number>(bounds.length + 1).fill(0);
  let started: number | undefined, loadMs: number | undefined;
  let chunks = 0, stepMs = 0, maxStepMs = 0;
  let previousStepEnd: number | undefined, measuredPumpGaps = 0, pumpGapMs = 0, maxPumpGapMs = 0;
  let firstFrameMs: number | undefined, firstMainMs: number | undefined, readyMs: number | undefined;
  return {
    reset() { started = performance.now(); loadMs = undefined; chunks = 0; stepMs = 0; maxStepMs = 0; previousStepEnd = undefined; measuredPumpGaps = 0; pumpGapMs = 0; maxPumpGapMs = 0; buckets.fill(0); gapBuckets.fill(0); firstFrameMs = undefined; firstMainMs = undefined; readyMs = undefined; },
    loaded() { if (started !== undefined) loadMs = performance.now() - started; },
    step(begin: number, end: number, snapshot: RuntimeSnapshot) {
      if (kind === 'native_ipc' && previousStepEnd !== undefined) {
        const gap = Math.max(0, begin - previousStepEnd);
        measuredPumpGaps += 1; pumpGapMs += gap; maxPumpGapMs = Math.max(maxPumpGapMs, gap);
        const gapBucket = bounds.findIndex((upper) => gap <= upper);
        gapBuckets[gapBucket < 0 ? bounds.length : gapBucket] += 1;
      }
      if (kind === 'native_ipc') previousStepEnd = end;
      chunks += 1; const elapsed = end - begin; stepMs += elapsed; maxStepMs = Math.max(maxStepMs, elapsed);
      const bucket = bounds.findIndex((upper) => elapsed <= upper);
      buckets[bucket < 0 ? bounds.length : bucket] += 1;
      if (started === undefined) return;
      if (snapshot.frame && firstFrameMs === undefined) firstFrameMs = end - started;
      if (snapshot.frame && snapshot.status.frame_source === 'main' && firstMainMs === undefined) firstMainMs = end - started;
      if (snapshot.status.ready && readyMs === undefined) readyMs = end - started;
    },
    // Do not connect measurements over pauses, load/restart epochs, or queued input work.
    invalidatePumpGap() { previousStepEnd = undefined; },
    report() { return { kind, load_response_wall_ms: loadMs, elapsed_wall_ms: started === undefined ? undefined : performance.now() - started, chunks, step_response_wall_ms: stepMs, max_step_response_wall_ms: maxStepMs, step_response_wall_ms_buckets: { upper_bounds_ms: [...bounds, null], counts: [...buckets] }, ...(kind === 'native_ipc' ? { measured_pump_scheduling: { note: 'Adjacent accepted step-response gaps only; excludes lifecycle, pause/resume, and queued input intervals.', gaps: measuredPumpGaps, gap_wall_ms: pumpGapMs, max_gap_wall_ms: maxPumpGapMs, gap_wall_ms_buckets: { upper_bounds_ms: [...bounds, null], counts: [...gapBuckets] } } } : {}), first_frame_response_wall_ms: firstFrameMs, first_main_response_wall_ms: firstMainMs, ready_response_wall_ms: readyMs }; },
  };
}
