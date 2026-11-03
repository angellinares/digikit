// Scheduling policy only: native Nx still owns task dependencies and cache inputs.
import { execFileSync, spawnSync } from 'node:child_process';
import { resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

export const MENU_SOURCE = 'modules/core/apps/digikit-menu/src/menu.c';

export function menuOnly(changes) {
  return changes.length > 0 && changes.every(({ file, status }) => file === MENU_SOURCE && status === 'M');
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
    !menuOnly(changes) ||
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

if (process.argv[1] && resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  const args = process.argv.slice(2);
  let changes = [];
  try {
    changes = changedFiles(args);
  } catch {
    /* Uncertain change metadata keeps the broad gate. */
  }
  const commands = gateCommands(args, changes);
  const files = [...new Set(changes.map(({ file }) => file))];
  const summary = files.slice(0, 3).join(', ') + (files.length > 3 ? ` (+${files.length - 3} files)` : '');
  const narrowed =
    option(commands[0], ['-t', '--targets', '--target']) !== option(args, ['-t', '--targets', '--target']);
  console.log(
    narrowed
      ? `gate: ${MENU_SOURCE} -> menu fixture policy -> Nx affected without test-conformance; includes dkui-host:rust-tests`
      : `gate: ${summary || 'unknown changes'} -> normal Nx affected selection`,
  );
  for (const command of commands) {
    console.log(`gate: pnpm nx:pilot ${command.join(' ')}`);
    const result = spawnSync('pnpm', ['nx:pilot', ...command], { stdio: 'inherit' });
    if (result.error) throw result.error;
    if (result.status !== 0) process.exit(result.status ?? 1);
  }
}
