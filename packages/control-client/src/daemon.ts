import { spawn, type ChildProcess } from 'node:child_process';
import fs from 'node:fs';
import path from 'node:path';
import { connect, type Client, type ConnectOptions } from './client.ts';

export interface SpawnOptions {
  /** Path to digi-emud; default: env DIGI_EMUD. */
  binary?: string;
  /** Extra daemon arguments (after `--run-dir`). */
  args?: string[];
  /** Run directory (socket, current.json, daemon.stderr); created, stale sockets removed. */
  runDir: string;
  env?: Record<string, string>;
  cwd?: string;
  /** Wait for the socket, ms. Default 120000; 0 waits for as long as the child runs; a daemon that loads a firmware first opens its socket late, so callers pass a generous hang guard. */
  timeoutMs?: number;
  connect?: ConnectOptions;
  /** Abort startup, closing the connection and awaiting this child's exit. */
  signal?: AbortSignal;
}

export interface Daemon {
  client: Client;
  process: ChildProcess;
  runDir: string;
  /** Send quit, wait for exit (10 s), then kill only this child. */
  close(): Promise<void>;
}

export const BUILD_HINT = '`mise run emu:serve -- --build-only` builds it';

export function resolveBinary(binary?: string): string {
  const exe = binary ?? process.env.DIGI_EMUD ?? '';
  if (!exe) throw new Error(`set DIGI_EMUD to the digi-emud binary (${BUILD_HINT})`);
  if (!fs.existsSync(exe)) throw new Error(`DIGI_EMUD ${exe} does not exist (${BUILD_HINT})`);
  return exe;
}

const sleep = (ms: number) => new Promise((r) => setTimeout(r, ms));

/** The control socket that RUNDIR's `current.json` names, or null while there is none. */
export function readSocket(runDir: string): string | null {
  try {
    const info = JSON.parse(fs.readFileSync(path.join(runDir, 'current.json'), 'utf8'));
    const sock = String(info.socket);
    return fs.existsSync(sock) ? sock : null;
  } catch {
    return null;
  }
}

/** Start digi-emud in RUNDIR, wait for its socket, connect. */
export async function spawnDaemon(opts: SpawnOptions): Promise<Daemon> {
  const exe = resolveBinary(opts.binary);
  const runDir = path.resolve(opts.runDir);
  fs.mkdirSync(runDir, { recursive: true });
  for (const f of fs.readdirSync(runDir)) {
    if (f.endsWith('.sock') || f === 'current.json') fs.rmSync(path.join(runDir, f), { force: true });
  }
  const errPath = path.join(runDir, 'daemon.stderr');
  const errFd = fs.openSync(errPath, 'w');
  const child = spawn(exe, ['--run-dir', runDir, ...(opts.args ?? [])], {
    stdio: ['ignore', 'ignore', errFd],
    cwd: opts.cwd,
    env: { ...process.env, ...opts.env },
  });
  fs.closeSync(errFd);
  let exited: number | null | undefined;
  child.once('exit', (code) => {
    exited = code ?? -1;
  });
  child.once('error', (err) => {
    exited = -1;
    fs.appendFileSync(errPath, String(err));
  });

  const done = new Promise<void>((resolve) => {
    if (exited !== undefined) resolve();
    else {
      child.once('exit', () => resolve());
      child.once('error', () => resolve());
    }
  });
  let killRequested = false;
  const cancel = () => {
    if (exited === undefined && !killRequested) {
      killRequested = true;
      child.kill('SIGKILL');
    }
  };
  opts.signal?.addEventListener('abort', cancel, { once: true });
  let client: Client;
  try {
    opts.signal?.throwIfAborted();
    const limit = opts.timeoutMs ?? 120_000;
    const deadline = limit > 0 ? Date.now() + limit : 0;
    let sock: string | null = null;
    while (!sock) {
      opts.signal?.throwIfAborted();
      if (exited !== undefined) {
        const err = fs.readFileSync(errPath, 'utf8').trim().slice(-400);
        throw new Error(`digi-emud exited ${exited}: ${err}`);
      }
      if (deadline > 0 && Date.now() > deadline)
        throw new Error(`digi-emud did not open its socket in ${runDir} within ${limit} ms`);
      sock = readSocket(runDir);
      if (!sock) await sleep(100);
    }
    client = await connect(sock, { ...opts.connect, signal: opts.signal ?? opts.connect?.signal });
    if (opts.signal?.aborted) {
      client.close();
      opts.signal.throwIfAborted();
    }
  } catch (err) {
    cancel();
    await done;
    throw err;
  } finally {
    opts.signal?.removeEventListener('abort', cancel);
  }
  let closing: Promise<void> | undefined;
  return {
    client,
    process: child,
    runDir,
    close() {
      return (closing ??= (async () => {
        if (exited === undefined) {
          const timer = setTimeout(() => {
            client.close();
            child.kill('SIGKILL');
          }, 10_000);
          // A peer that never answers quit must not postpone the child deadline.
          void client.quit().catch(() => {});
          await done;
          clearTimeout(timer);
        }
        client.close();
      })());
    },
  };
}
