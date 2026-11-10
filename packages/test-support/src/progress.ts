/** The status fields that show a restart or load making progress (the daemon's `status` reply has more). */
export interface ProgressStatus {
  job?: unknown;
  epoch?: unknown;
  cf?: object | null;
  boot?: object | null;
}

/** A load (`job`) or a boot inside `firmware.load` (`boot`) is running: the emulated clock may stand still while it does. */
export function isLoading(status: ProgressStatus | undefined): boolean {
  return Boolean(status?.job || status?.boot);
}

/**
 * Movement other than the emulated clock, as a string that changes with it: the epoch and, only while a load or boot runs,
 * the ColdFire and boot instruction counts (a cold boot moves only these). Outside a load or boot only the epoch counts,
 * so a halted DSP with a running ColdFire is still stuck.
 */
export function restartProgress(status: ProgressStatus | undefined): string {
  const loading = isLoading(status);
  return [status?.epoch, loading ? icount(status?.cf) : undefined, loading ? icount(status?.boot) : undefined].join(
    '|',
  );
}

function icount(counts: object | null | undefined): unknown {
  return (counts as { icount?: unknown } | null | undefined)?.icount;
}

/** The counts for a stuck message, empty when no load or boot runs. */
export function describeRestart(status: ProgressStatus | undefined): string {
  if (!isLoading(status)) return '';
  return ` (load or boot active: icount ${icount(status?.cf)}, boot ${JSON.stringify(status?.boot ?? null)})`;
}
