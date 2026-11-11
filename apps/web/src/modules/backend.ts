// Helpers for errors and sizes shared by the Transfer pane and its backend (errors from the native commands and from the TypeScript
// protocol client). Written without TS-only runtime syntax so node can import it.

/** A readable line for an error from either backend: the status name first, then its meaning. */
export function errorLine(e: unknown): string {
  const raw = e instanceof Error ? e.message : String(e);
  try {
    const j = JSON.parse(raw);
    if (j && typeof j.error === 'string') return j.error;
  } catch {
    /* plain text */
  }
  return raw;
}

/** Whether an error is the user's cancel. */
export function isCancelled(e: unknown): boolean {
  const raw = e instanceof Error ? e.message : String(e);
  return (e as { kind?: string })?.kind === 'cancelled' || /"kind":"cancelled"/.test(raw);
}

/** Whether an error is "no reply" (the device never answered). */
export function isTimeout(e: unknown): boolean {
  const raw = e instanceof Error ? e.message : String(e);
  return (e as { kind?: string })?.kind === 'timeout' || /"kind":\s*"timeout"/.test(raw);
}

/** The target answers, or is known to run, without the digikit runtime: no digikit request can succeed until it has one. */
export class NoRuntimeError extends Error {
  readonly kind = 'noruntime';
}
export function isNoRuntime(e: unknown): boolean {
  return (e as { kind?: string } | null)?.kind === 'noruntime';
}

export function formatBytes(n: number): string {
  if (n < 1024) return `${n} B`;
  if (n < 1024 * 1024) return `${(n / 1024).toFixed(1)} KiB`;
  return `${(n / 1024 / 1024).toFixed(2)} MiB`;
}
