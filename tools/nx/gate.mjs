// Scheduling policy only: native Nx still owns task dependencies and cache inputs.
import { execFileSync, spawnSync } from 'node:child_process';
import { realpathSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import { readRuns, report } from './gate-history.mjs';

// Device and conformance results come from the Nx cache: their keys cover what the tests consume, so the merge
// tier runs them for every owning project instead of selecting them with `affected`.
export const DEVICE_TARGETS = ['test-device', 'test-conformance'];

// The merge tier in one Nx invocation, so all tasks share the scheduler: `run-many` over the affected projects
// and the owners of device and conformance targets. A project that is affected only through the graph also runs
// its uncached targets (test-host, typecheck); with the current owners that adds about 7 s.
export function mergeCommand(targets, projects, extra = []) {
  return ['run-many', '-t', targets.join(','), `--projects=${projects.join(',')}`, ...extra];
}

// `--explain` exists for `affected` only: the plan lists the selected non-device tasks.
export function explainCommand(base, targets, extra = []) {
  return [
    'affected',
    '--base',
    base,
    '-t',
    targets.filter((target) => !DEVICE_TARGETS.includes(target)).join(','),
    ...extra,
  ];
}

const nxProjects = (...query) => {
  const out = execFileSync('pnpm', ['--silent', 'nx:pilot', 'show', 'projects', ...query, '--json'], {
    encoding: 'utf8',
    maxBuffer: 16 * 1024 * 1024,
  });
  return JSON.parse(out.slice(out.indexOf('[')));
};

export const deviceOwners = () => [
  ...new Set(DEVICE_TARGETS.flatMap((target) => nxProjects(`--withTarget=${target}`))),
];

const MERGE_TARGETS = [
  'lint',
  'fmt-check',
  'rust-fmt',
  'typecheck',
  'test-host',
  'test',
  'check',
  'build:ui',
  'check:ui',
  'rust-tests',
  'catalog-check',
  'objects-check',
  'pack',
  'test-device',
  'test-conformance',
  'stock',
  'fx',
  'fx-no-aot',
  'fx-dt2',
  'python-checks',
  'python-tests',
  'pins',
  'fast-roots',
  'tier-quick',
  'dt2-tier-quick',
  'card',
  'smoke',
  'dt2-tone',
];

export const TIERS = {
  changed: {
    targets: ['lint', 'fmt-check', 'rust-fmt', 'typecheck', 'test-host', 'python-checks'],
    access: 'rw',
    narrow: false,
  },
  merge: { targets: MERGE_TARGETS, access: 'rw', narrow: false, deviceFromCache: true, pgo: true },
  full: {
    targets: [...MERGE_TARGETS, 'test-device-full', 'tier-sweep', 'dt2-tier-sweep'],
    access: 'rw',
    narrow: false,
    runMany: true,
    pgo: true,
  },
};

// The gate's emulators (daemons and coupled-run) use the per-device PGO profile of DN2 and DT2.
export function pgoRead(device) {
  const result = spawnSync('uv', ['run', 'python', '-m', 'tools.emulator.native_pgo', 'check', '--device', device], {
    encoding: 'utf8',
    timeout: 60000,
  });
  const out = (result.stdout || '').trim().split('\n').filter(Boolean).at(-1) || '';
  // DT2 reports a sources-only change as "stale changed: sources"; the profile tools treat it as partial.
  if (/^partly stale|^stale changed: sources$/.test(out)) return { state: 'partial', detail: 'sources' };
  if (out.startsWith('ok')) return { state: 'ok', detail: '' };
  const stale = out.match(/^stale (?:changed: )?(.*)/);
  if (stale) return { state: 'stale', detail: stale[1] };
  if (out.startsWith('missing'))
    return { state: 'missing', detail: out.replace(/^missing\s*\(?/, '').replace(/\)$/, '') };
  return { state: 'unknown', detail: out || `check exited with ${result.status}` };
}

// Prints one line per device before the first task; returns the warning lines to repeat at the end.
export function pgoReport(read = pgoRead, env = process.env) {
  if (env.DIGI_EMU_PGO === '0') {
    console.log('gate: PGO off (DIGI_EMU_PGO=0); daemons and coupled-run build without PGO');
    return [];
  }
  const warnings = [];
  for (const device of ['dn2', 'dt2']) {
    let result;
    try {
      result = read(device);
    } catch (error) {
      result = { state: 'unknown', detail: String(error.message || error) };
    }
    const { state, detail } = result;
    const note = detail ? ` (${detail})` : '';
    if (state === 'ok') console.log(`gate: PGO ${device}: current; daemons and coupled-run use the profile`);
    else if (state === 'partial')
      console.log(`gate: PGO ${device}: partly stale (${detail}); daemons and coupled-run use the profile`);
    else if (state === 'stale') {
      console.log(`gate: PGO ${device}: stale${note}; daemons use the stale profile, coupled-run builds plain`);
      warnings.push(
        `WARNING: PGO ${device} profile is stale${note}.`,
        '  Affected: daemons use the stale profile; coupled-run builds without PGO (about 30% slower DSP).',
        `  Retrain: mise run perf:pgo:${device}`,
      );
    } else if (state === 'missing') {
      console.log(`gate: PGO ${device}: missing${note}; daemons and coupled-run build without PGO`);
      warnings.push(
        `WARNING: PGO ${device} profile is missing${note}.`,
        '  Affected: daemons and coupled-run build without PGO (about 30% slower DSP).',
        `  Retrain: mise run perf:pgo:${device}`,
      );
    } else console.log(`gate: PGO ${device}: status unknown${note}`);
  }
  for (const line of warnings) console.log(line);
  return warnings;
}

if (process.argv[1] && realpathSync(process.argv[1]) === fileURLToPath(import.meta.url)) {
  let args = process.argv.slice(2);
  const tierIndex = args.findIndex((arg) => arg === '--tier' || arg.startsWith('--tier='));
  let tier;
  if (tierIndex >= 0) {
    const inline = args[tierIndex].startsWith('--tier=');
    const name = inline ? args[tierIndex].slice('--tier='.length) : args[tierIndex + 1];
    tier = TIERS[name];
    if (!tier) {
      console.error(`gate: unknown tier ${name}`);
      process.exit(2);
    }
    args = args.filter((_, i) => i !== tierIndex && (inline || i !== tierIndex + 1));
    process.env.DIGI_GATE_BASE ||= 'main';
    const targets = tier.targets.join(',');
    args = tier.runMany ? ['-t', targets, ...args] : ['--base', process.env.DIGI_GATE_BASE, '-t', targets, ...args];
  }
  const explain = args.includes('--explain');
  if (tier?.pgo) {
    try {
      const warnings = pgoReport();
      if (warnings.length) {
        process.on('exit', () => {
          console.log('gate: PGO status again, from the start of the gate:');
          for (const line of warnings) console.log(line);
        });
      }
    } catch {
      /* The PGO status is advice and never fails the gate. */
    }
  }
  // Nx prints the quiet `summary` style when it detects an AI agent, so nothing appears until the end.
  // `static-failures-only` prints one line per finished task as it finishes (a tick, `nx run project:target`
  // and the cache status) and the full output of failed tasks. A person can still choose another style
  // with NX_DEFAULT_OUTPUT_STYLE or `--output-style`.
  const styled = (command) =>
    tier && !explain && !process.env.NX_DEFAULT_OUTPUT_STYLE && !args.some((arg) => arg.startsWith('--output-style'))
      ? [...command, '--output-style=static-failures-only']
      : command;
  const wrap = (command) =>
    tier && !explain
      ? ['node', ['tools/nx/remote-cache.mjs', 'exec', '--access', tier.access, '--', 'pnpm', 'nx:pilot', ...command]]
      : ['pnpm', ['nx:pilot', ...command]];
  let commands;
  if (tier?.deviceFromCache) {
    // args: ['--base', BASE, '-t', TARGETS, ...extra]
    const extra = args.slice(4);
    if (explain) {
      commands = [explainCommand(args[1], tier.targets, extra)];
      console.log('gate: test-device and test-conformance run for every owning project and replay from the cache');
    } else {
      const affected = nxProjects('--affected', '--base', args[1]);
      const projects = [...new Set([...affected, ...deviceOwners()])];
      console.log(
        `gate: ${affected.length} affected projects + device owners = ${projects.length} projects, one run-many`,
      );
      commands = [mergeCommand(tier.targets, projects, extra)];
    }
  } else {
    commands = tier?.runMany ? [['run-many', ...args]] : [['affected', ...args]];
    console.log(`gate: ${commands[0][0]} ${tier ? 'over the tier targets' : 'with the given arguments'}`);
  }
  for (const command of commands) {
    const [file, fileArgs] = wrap(styled(command));
    console.log(`gate: ${file} ${fileArgs.join(' ')}`);
    const startedAt = Date.now();
    const result = spawnSync(file, fileArgs, { stdio: 'inherit' });
    if (result.error) throw result.error;
    if (tier && !explain) {
      // Nx records the run in its task history when the command ends; the durations come from there.
      try {
        console.log(`gate: this run, from the Nx task history\n${report(readRuns(startedAt))}`);
      } catch (error) {
        console.log(`gate: no task history (${error.message})`);
      }
    }
    if (result.status !== 0) process.exit(result.status ?? 1);
  }
}
