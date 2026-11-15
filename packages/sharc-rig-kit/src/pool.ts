import { availableParallelism } from 'node:os';

/** The number of workers: `DIGI_JOBS`, else a quarter of the cores (the conformance run shares the machine with the emulators). */
export const jobsFromEnv = (): number =>
  Math.max(1, Number(process.env.DIGI_JOBS) || Math.floor(availableParallelism() / 4));

/** Run `fn` over `items` with at most `jobs` at a time; the results are in the order of the items. */
export async function runPool<T, R>(items: readonly T[], jobs: number, fn: (item: T) => Promise<R>): Promise<R[]> {
  const out: R[] = Array.from({ length: items.length });
  let next = 0;
  const worker = async (): Promise<void> => {
    while (next < items.length) {
      const i = next++;
      out[i] = await fn(items[i]!);
    }
  };
  await Promise.all(Array.from({ length: Math.max(1, Math.min(jobs, items.length)) }, worker));
  return out;
}
