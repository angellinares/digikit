// Machine resources for the Nx launcher (tools/nx/nx.mjs) and the emulator slots (tools/devtools/emu-slot.mjs).
// This file is the one home of the policy: how many emulators may run at most, and how many cores stay free.
import { spawnSync } from 'node:child_process';
import { availableParallelism, cpus, totalmem } from 'node:os';

// One coupled emulator needs about 2 cores (measured about 1.5).
export const EMULATOR_CORES = 2;
// A new emulator is admitted when this many cores are idle, plus the reserve.
export const ADMIT_IDLE_CORES = 2;
// The idle check compares two CPU samples this far apart.
export const SAMPLE_MS = 1000;
// After an admission the next decision waits this long, so it sees the load of the new emulator.
export const RAMP_MS = 3000;

export const clamp = (n, lo, hi) => Math.min(hi, Math.max(lo, n));

const sysctl = (name) => {
  const { stdout } = spawnSync('sysctl', ['-n', name], { encoding: 'utf8' });
  const n = Number(stdout);
  return n > 0 ? n : 0;
};

// Logical cores in total, and the split on Apple Silicon. Elsewhere all cores count as performance cores.
export function coreCounts() {
  const total = availableParallelism();
  if (process.platform === 'darwin') {
    const performance = sysctl('hw.perflevel0.logicalcpu');
    if (performance > 0) return { total, performance, efficiency: sysctl('hw.perflevel1.logicalcpu') };
  }
  return { total, performance: total, efficiency: 0 };
}

// Cores for our work: the performance cores on Apple Silicon, so the efficiency cores stay free for the
// rest of the machine. Otherwise all cores but two.
export function workCores() {
  const { total, performance, efficiency } = coreCounts();
  return efficiency > 0 ? performance : total - 2;
}

// Most emulators at once: two cores each, and two GB of RAM each.
export function ceiling(work = workCores(), ramBytes = totalmem()) {
  return Math.max(1, Math.min(Math.floor(work / EMULATOR_CORES), Math.floor(ramBytes / 2 ** 30 / 2)));
}

// Cores that stay free for the rest of the machine: the efficiency cores on Apple Silicon, else a quarter of the cores.
export function reserve(env = process.env) {
  const set = env.DIGI_EMU_RESERVE;
  if (set !== undefined && set !== '') {
    const n = Number(set);
    if (!Number.isInteger(n) || n < 0) throw new Error(`DIGI_EMU_RESERVE must be a non-negative integer, got ${set}`);
    return n;
  }
  const { total, efficiency } = coreCounts();
  return efficiency > 0 ? efficiency : Math.max(2, Math.floor(total / 4));
}

const cpuTimes = () =>
  cpus().map(({ times }) => ({ idle: times.idle, total: Object.values(times).reduce((a, b) => a + b, 0) }));

// Idle core-equivalents over all cores, from two samples of the CPU times `ms` apart.
export async function idleCores(ms = SAMPLE_MS) {
  const before = cpuTimes();
  await new Promise((resolve) => setTimeout(resolve, ms));
  const after = cpuTimes();
  let idle = 0;
  after.forEach((now, i) => {
    const total = now.total - before[i].total;
    if (total > 0) idle += (now.idle - before[i].idle) / total;
  });
  return idle;
}

export function budget(work, override) {
  let b;
  if (override !== undefined && override !== '') {
    b = Number(override);
    if (!Number.isInteger(b) || b < 1) throw new Error(`DIGI_BUDGET must be a positive integer, got ${override}`);
  } else {
    b = Math.max(2, work);
  }
  // Nx tasks follow the emulator ceiling.
  const tasks = ceiling(b);
  const workers = 1;
  const threads = Math.ceil(b / tasks);
  const jobs = clamp(Math.floor(b / 2), 1, 6);
  return { budget: b, tasks, workers, threads, jobs };
}
