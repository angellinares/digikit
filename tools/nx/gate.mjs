// Scheduling policy only: native Nx still owns task dependencies and cache inputs.
import { execFileSync, spawnSync } from 'node:child_process';
import { realpathSync } from 'node:fs';
import { fileURLToPath } from 'node:url';

export const MENU_SOURCE = 'modules/core/apps/digikit-menu/src/menu.c';
export const UI_SOURCE = 'modules/core/adapters/ui/src/ui.c';

export function menuOnly(changes) {
  return changes.length > 0 && changes.every(({ file, status }) => file === MENU_SOURCE && status === 'M');
}

export function uiOnly(changes) {
  return changes.length > 0 && changes.every(({ file, status }) => file === UI_SOURCE && status === 'M');
}

function option(args, names) {
  for (let i = args.length - 1; i >= 0; i--) {
    for (const name of names) {
      if (args[i].startsWith(`${name}=`)) return args[i].slice(name.length + 1);
      if (args[i] === name) return args[i + 1]?.startsWith('-') ? '' : (args[i + 1] ?? '');
    }
  }
}

// Only the explicit base comparison has a supported scheduling policy.
// Let Nx parse alternate selectors, including multi-value and repeated --files.
const alternateSelection = (args) =>
  args.some((arg) => ['--files', '--stdin', '--uncommitted', '--untracked', '--all'].includes(arg.split('=')[0]));

// mise supplies one explicit base. Other comparisons may use Nx environment or configuration defaults.
const supportedComparison = (args) =>
  !alternateSelection(args) &&
  !process.env.NX_HEAD &&
  args.filter((arg) => arg === '--base' || arg.startsWith('--base=')).length === 1 &&
  Boolean(option(args, ['--base'])) &&
  !args.some((arg) => arg === '--head' || arg.startsWith('--head='));

export function gateCommands(args, changes) {
  const targets = option(args, ['-t', '--targets', '--target']);
  const required = ['pack', 'test-device', 'test-conformance', 'rust-tests'];
  if (
    !supportedComparison(args) ||
    !(menuOnly(changes) || uiOnly(changes)) ||
    !required.every((target) => targets?.split(',').includes(target))
  )
    return [['affected', ...args]];
  const affected = [...args];
  for (let i = 0; i < affected.length; i++) {
    const name = ['-t', '--targets', '--target'].find(
      (name) => affected[i] === name || affected[i].startsWith(`${name}=`),
    );
    if (!name) continue;
    const narrowed = targets
      .split(',')
      .filter((target) => target !== 'test-conformance')
      .join(',');
    if (affected[i] === name) affected[++i] = narrowed;
    else affected[i] = `${name}=${narrowed}`;
  }
  return [['affected', ...affected]];
}

export function changedFiles(args, cwd = process.cwd()) {
  const git = (...argv) => execFileSync('git', argv, { cwd, encoding: 'utf8' }).trimEnd();
  // Unsupported selection modes fail closed to ordinary Nx selection.
  if (!supportedComparison(args)) return [];
  const head = option(args, ['--head']);
  const base = git('merge-base', option(args, ['--base']) || process.env.DIGI_GATE_BASE || 'main', head || 'HEAD');
  const statuses = [git('diff', '--name-status', '--no-renames', base, head || 'HEAD', '--')];
  if (!head) {
    statuses.push(git('diff', '--name-status', '--no-renames', 'HEAD', '--'));
    statuses.push(
      ...git('ls-files', '--others', '--exclude-standard')
        .split('\n')
        .filter(Boolean)
        .map((f) => `?\t${f}`),
    );
  }
  const changes = statuses
    .join('\n')
    .split('\n')
    .filter(Boolean)
    .map((line) => {
      const [status, file] = line.split('\t');
      return { status, file };
    });
  return changes;
}

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
    access: 'ro',
    narrow: false,
  },
  merge: { targets: MERGE_TARGETS, access: 'rw', narrow: true, pgo: true },
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
  const wrap = (command) =>
    tier && !explain
      ? ['node', ['tools/nx/remote-cache.mjs', 'exec', '--access', tier.access, '--', 'pnpm', 'nx:pilot', ...command]]
      : ['pnpm', ['nx:pilot', ...command]];
  let changes = [];
  try {
    changes = changedFiles(args);
  } catch {
    /* Uncertain change metadata keeps the broad gate. */
  }
  const commands = !tier
    ? gateCommands(args, changes)
    : tier.runMany
      ? [['run-many', ...args]]
      : tier.narrow
        ? gateCommands(args, changes)
        : [['affected', ...args]];
  const files = [...new Set(changes.map(({ file }) => file))];
  const summary = files.slice(0, 3).join(', ') + (files.length > 3 ? ` (+${files.length - 3} files)` : '');
  const narrowed =
    option(commands[0], ['-t', '--targets', '--target']) !== option(args, ['-t', '--targets', '--target']);
  console.log(
    narrowed
      ? uiOnly(changes)
        ? `gate: ${UI_SOURCE} -> UI fixture policy -> Nx affected without test-conformance`
        : `gate: ${MENU_SOURCE} -> menu fixture policy -> Nx affected without test-conformance; includes dkui-host:rust-tests`
      : `gate: ${summary || 'unknown changes'} -> normal Nx affected selection`,
  );
  for (const command of commands) {
    const [file, fileArgs] = wrap(command);
    console.log(`gate: ${file} ${fileArgs.join(' ')}`);
    const result = spawnSync(file, fileArgs, { stdio: 'inherit' });
    if (result.error) throw result.error;
    if (result.status !== 0) process.exit(result.status ?? 1);
  }
}
