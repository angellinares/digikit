import { execFile } from 'node:child_process';
import fs from 'node:fs';
import path from 'node:path';
import { promisify } from 'node:util';
import { repoRoot, type Session } from '@digi/test-support';
import { timePhase, timingContext } from '../timing.ts';

const exec = promisify(execFile);

export interface TransferEvent {
  event: string;
  loaded?: string[];
}

/** Exercise the transfer CLI against this daemon, preserving stdout/stderr even on failure. */
export async function transferCommand(
  session: Session,
  args: string[],
  artifactFile: string,
  timeout = 90_000,
): Promise<TransferEvent[]> {
  return timePhase(
    `cli.${args[0] ?? 'command'}`,
    async () => {
      fs.mkdirSync(path.dirname(artifactFile), { recursive: true });
      const command = [
        'run',
        'python',
        '-m',
        'tools.device.dktransfer',
        '--emu',
        '--run-dir',
        session.runDir,
        '--json',
        ...args,
      ];
      let stdout: string;
      let stderr: string;
      try {
        const result = await exec('uv', command, { cwd: repoRoot(), timeout, maxBuffer: 4 * 1024 * 1024 });
        stdout = result.stdout;
        stderr = result.stderr;
      } catch (error) {
        const output = error as Error & { stdout?: string; stderr?: string };
        fs.writeFileSync(artifactFile, `${output.stdout ?? ''}\n${output.stderr ?? ''}\n${output.message}`);
        throw new Error(`dktransfer ${args.join(' ')} failed: ${output.message}; see ${artifactFile}`, {
          cause: error,
        });
      }
      fs.writeFileSync(artifactFile, `${stdout}\n${stderr}`);
      return stdout
        .split('\n')
        .filter((line) => line.startsWith('{'))
        .map((line) => JSON.parse(line) as TransferEvent);
    },
    timingContext(session),
  );
}

/** Python owns module preparation and install; use the already-running daemon. `alongside` modules are installed and selected with it (the debug module, for a test that reads the log). */
export async function cliInstall(
  session: Session,
  moduleId: string,
  artifactDirectory: string,
  timeout = 90_000,
  alongside: string[] = [],
): Promise<void> {
  await timePhase(
    'cli.install-restart',
    async () => {
      const events = await transferCommand(
        session,
        ['install', moduleId, ...alongside, '--restart'],
        path.join(artifactDirectory, 'install.log'),
        timeout,
      );
      const result = events.find((event) => event.event === 'result');
      if (![moduleId, ...alongside].every((id) => result?.loaded?.includes(id))) {
        throw new Error(`install --restart did not load ${moduleId}: ${JSON.stringify(events)}`);
      }
    },
    { ...timingContext(session), detail: moduleId },
  );
}
