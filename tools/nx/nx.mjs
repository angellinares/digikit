// The one launcher for Nx (`pnpm nx:pilot`): one CPU budget, one shared lock, one priority.
// Nx has no per-kind limits, so the per-target counts (Vitest workers, emulators, cargo jobs)
// come from this budget and reach the targets through environment variables.
import { spawnSync } from 'node:child_process';
import { availableParallelism } from 'node:os';
import { realpathSync } from 'node:fs';
import { fileURLToPath } from 'node:url';

const clamp = (n, lo, hi) => Math.min(hi, Math.max(lo, n));

export function budget(work, override) {
  let b;
  if (override !== undefined && override !== '') {
    b = Number(override);
    if (!Number.isInteger(b) || b < 1) throw new Error(`DIGI_BUDGET must be a positive integer, got ${override}`);
  } else {
    b = Math.max(2, work);
  }
  // One emulator keeps 2 cores busy. One task per 3 cores keeps the machine usable.
  const tasks = clamp(Math.floor(b / 3), 1, 4);
  const workers = 1;
  const threads = Math.ceil(b / tasks);
  const jobs = clamp(Math.floor(b / 2), 1, 6);
  return { budget: b, tasks, workers, threads, jobs };
}

// Cores for our work: the performance cores on Apple Silicon, so the efficiency cores stay
// free for the rest of the machine. Otherwise all cores but two.
export function workCores() {
  if (process.platform === 'darwin') {
    const { stdout } = spawnSync('sysctl', ['-n', 'hw.perflevel0.logicalcpu'], { encoding: 'utf8' });
    const performance = Number(stdout);
    if (performance > 0) return performance;
  }
  return availableParallelism() - 2;
}

export function runsTasks(args) {
  const first = args[0];
  const runs =
    first === 'run' || first === 'run-many' || first === 'affected' || (first?.includes(':') && !first.startsWith('-'));
  if (!runs) return false;
  return !args.some((arg) => ['--graph', '--explain', '--help'].some((flag) => arg === flag || arg.startsWith(flag)));
}

export const LIGHT_TARGETS = new Set([
  'lint',
  'fmt-check',
  'rust-fmt',
  'typecheck',
  'test-host',
  'check',
  'check:ui',
  'build:ui',
  'python-checks',
]);

function targetsOf(args) {
  for (let i = 0; i < args.length; i++) {
    const arg = args[i];
    for (const flag of ['-t', '--targets']) {
      if (arg === flag) return args[i + 1]?.split(',').filter(Boolean) ?? [];
      if (arg.startsWith(`${flag}=`))
        return arg
          .slice(flag.length + 1)
          .split(',')
          .filter(Boolean);
    }
  }
  const spec = args[0] === 'run' ? args[1] : args[0];
  if (spec?.includes(':') && !spec.startsWith('-')) return [spec.slice(spec.lastIndexOf(':') + 1)];
  return [];
}

export function needsLock(args) {
  if (!runsTasks(args)) return false;
  const targets = targetsOf(args);
  return targets.length === 0 || !targets.every((target) => LIGHT_TARGETS.has(target));
}

// Plain launcher runs read and write the shared remote cache when its server already answers; they
// never start it. `remote-cache.mjs exec` (the gate) sets the Nx variables first and wins.
export function usesRemote(args, env) {
  return (
    runsTasks(args) &&
    env.DIGI_NX_REMOTE !== '0' &&
    !env.NX_SELF_HOSTED_REMOTE_CACHE_SERVER &&
    !args.some((arg) => arg === '--skip-nx-cache' || arg.startsWith('--skip-nx-cache='))
  );
}

function main() {
  const args = process.argv.slice(2);
  const cwd = process.cwd();
  let b;
  try {
    b = budget(workCores(), process.env.DIGI_BUDGET);
  } catch (error) {
    console.error(`nx: ${error.message}`);
    process.exit(2);
  }
  const env = {
    ...process.env,
    NX_CACHE_DIRECTORY: `${cwd}/.nx/cache`,
    NX_WORKSPACE_DATA_DIRECTORY: `${cwd}/.nx/workspace-data`,
    NX_DAEMON: 'false',
    NX_NO_CLOUD: 'true',
  };
  env.NX_PARALLEL ??= String(b.tasks);
  env.REGRESSION_JOBS ??= String(b.workers);
  env.DIGI_JOBS ??= String(b.jobs);
  env.CARGO_BUILD_JOBS ??= String(b.budget);
  env.NEXTEST_TEST_THREADS ??= String(b.threads);

  let command = ['pnpm', 'exec', 'nx', ...args];
  if (runsTasks(args)) {
    const lock = ['uv', 'run', 'python', '-m', 'tools.devtools.benchlock', 'run', '--shared', '--label', 'nx', '--'];
    command = [
      ...(process.platform === 'win32' ? [] : ['nice', '-n', '10']),
      ...(needsLock(args) ? lock : []),
      ...command,
    ];
    const remote = usesRemote(args, env);
    if (remote)
      command = [
        'node',
        fileURLToPath(new URL('remote-cache.mjs', import.meta.url)),
        'exec',
        '--access',
        'rw',
        '--no-start',
        '--',
        ...command,
      ];
    console.error(
      `nx: budget ${b.budget}: ${b.tasks} tasks, ${b.workers} vitest workers, ${b.jobs} emulators, ${b.budget} cargo jobs${remote ? ', remote' : ''}`,
    );
  }
  const result = spawnSync(command[0], command.slice(1), { stdio: 'inherit', env });
  if (result.error) throw result.error;
  process.exit(result.status ?? 1);
}

if (process.argv[1] && realpathSync(process.argv[1]) === fileURLToPath(import.meta.url)) main();
