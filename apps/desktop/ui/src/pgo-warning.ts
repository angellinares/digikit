// The status bar warning for a binary built without a valid PGO profile (the launcher passes the state in `emu_build_info`).
export interface BuildInfo {
  pgo?: string | null;
  pgo_detail?: string | null;
}

const WARN = new Set(['stale', 'missing', 'failed', 'skipped', 'mismatch']);

export function pgoWarning(info: BuildInfo | undefined): { label: string; title: string } | undefined {
  const state = info?.pgo;
  if (!state || !WARN.has(state)) return undefined;
  const first =
    state === 'mismatch'
      ? 'This PGO profile does not match the build: DSP runs about 30% slower.'
      : 'Built without a valid PGO profile: DSP runs about 30% slower.';
  const reason = info?.pgo_detail?.trim();
  const lines = [first];
  if (reason) lines.push(/[.!?]$/.test(reason) ? reason : `${reason}.`);
  lines.push('Fix: mise run perf:pgo:both, then relaunch.');
  return { label: 'no PGO', title: lines.join('\n') };
}
