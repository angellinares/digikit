#!/usr/bin/env node
// Detached job runner for long commands: one blocking `wait` replaces repeated polling.
//   node tools/devtools/job.mjs start NAME [--timeout SEC] -- CMD ARGS...
//   node tools/devtools/job.mjs wait NAME [--max SEC] [--lines N]
//   node tools/devtools/job.mjs stop NAME
//   node tools/devtools/job.mjs status [NAME]
// --timeout defaults to 10800 s; 0 means no limit.
// State: <git toplevel>/out/runs/jobs/<worktree name>/NAME/{log,meta.json,exit.json}.
import { execFileSync, spawn, spawnSync } from 'node:child_process';
import {
  existsSync,
  mkdirSync,
  readFileSync,
  readdirSync,
  rmSync,
  openSync,
  statSync,
  writeFileSync,
  renameSync,
} from 'node:fs';
import { basename, join } from 'node:path';
import { fileURLToPath } from 'node:url';

const EXIT_RUNNING = 75;
const EXIT_DIED = 70;
const EXIT_TIMEOUT = 124;

const top = () => execFileSync('git', ['rev-parse', '--show-toplevel'], { encoding: 'utf8' }).trim();
const jobsDir = () => join(top(), 'out', 'runs', 'jobs', basename(top()));
const readJson = (file) => (existsSync(file) ? JSON.parse(readFileSync(file, 'utf8')) : null);
const alive = (pid) => {
  try {
    process.kill(pid, 0);
    return true;
  } catch (error) {
    return error.code === 'EPERM';
  }
};
const fail = (message, code = 2) => {
  console.error(`job: ${message}`);
  process.exit(code);
};

function flag(args, name, fallback) {
  const i = args.indexOf(name);
  if (i < 0) return fallback;
  const value = Number(args[i + 1]);
  if (!(value >= 0)) fail(`${name} needs a number`);
  args.splice(i, 2);
  return value;
}

function jobPaths(name) {
  if (!/^[\w.-]+$/.test(name || '')) fail('NAME must use letters, digits, ., _ and -');
  const dir = join(jobsDir(), name);
  return { dir, log: join(dir, 'log'), meta: join(dir, 'meta.json'), exit: join(dir, 'exit.json') };
}

// The state of a job: running, done (exit file) or died (process gone, no exit file).
function state(paths) {
  const meta = readJson(paths.meta);
  if (!meta) return null;
  let exit = readJson(paths.exit);
  if (!exit && alive(meta.pid)) return { meta, kind: 'running', elapsed: (Date.now() - meta.start) / 1000 };
  exit ||= readJson(paths.exit); // the wrapper may have finished between the two checks
  if (!exit) return { meta, kind: 'died', elapsed: (Date.now() - meta.start) / 1000 };
  const elapsed = (exit.end - meta.start) / 1000;
  return { meta, exit, elapsed, kind: exit.timedOut ? 'timed out' : 'done' };
}

// Internal: runs as the detached wrapper; writes the exit file when the command ends.
function runWrapper(dir) {
  const meta = readJson(join(dir, 'meta.json'));
  const log = openSync(join(dir, 'log'), 'a');
  const child = spawn(meta.command[0], meta.command.slice(1), {
    cwd: meta.cwd,
    stdio: ['ignore', log, log],
    detached: true, // own process group, so a timeout does not kill this wrapper
  });
  let timedOut = false;
  const finish = (code, signal) => {
    const record = { code, signal, timedOut, end: Date.now() };
    writeFileSync(join(dir, 'exit.json.tmp'), JSON.stringify(record));
    renameSync(join(dir, 'exit.json.tmp'), join(dir, 'exit.json'));
    process.exit(0);
  };
  child.on('error', (error) => {
    writeFileSync(log, `job: cannot start: ${error.message}\n`, { flag: 'a' });
    finish(127, null);
  });
  child.on('exit', (code, signal) => finish(code, signal));
  writeFileSync(join(dir, 'meta.json'), JSON.stringify({ ...meta, childPid: child.pid }));
  if (meta.timeout) {
    setTimeout(() => {
      timedOut = true;
      try {
        process.kill(-child.pid, 'SIGTERM');
        setTimeout(() => {
          try {
            process.kill(-child.pid, 'SIGKILL');
          } catch {}
        }, 5000).unref();
      } catch {}
    }, meta.timeout * 1000);
  }
}

function start(args) {
  const split = args.indexOf('--');
  if (split < 0 || split === args.length - 1) fail('usage: start NAME [--timeout SEC] -- CMD ARGS...');
  const command = args.slice(split + 1);
  const own = args.slice(0, split);
  const timeout = flag(own, '--timeout', 10800);
  const paths = jobPaths(own[0]);
  const old = state(paths);
  if (old?.kind === 'running') fail(`job ${own[0]} is still running (pid ${old.meta.pid})`);
  rmSync(paths.dir, { recursive: true, force: true });
  mkdirSync(paths.dir, { recursive: true });
  writeFileSync(paths.log, '');
  const meta = { name: own[0], command, cwd: process.cwd(), start: Date.now(), timeout, pid: 0 };
  writeFileSync(paths.meta, JSON.stringify(meta));
  const wrapper = spawn(process.execPath, [fileURLToPath(import.meta.url), '_run', paths.dir], {
    detached: true,
    stdio: 'ignore',
  });
  wrapper.unref();
  writeFileSync(paths.meta, JSON.stringify({ ...meta, pid: wrapper.pid }));
  console.log(`job ${own[0]} started (pid ${wrapper.pid}), log ${paths.log}`);
}

const clip = (line) => (line.length > 300 ? `${line.slice(0, 300)}...` : line);
const logLines = (file) => {
  const text = existsSync(file) ? readFileSync(file, 'utf8') : '';
  const lines = text.split('\n');
  if (lines.at(-1) === '') lines.pop();
  return lines;
};

async function wait(args) {
  const max = flag(args, '--max', 540);
  const count = flag(args, '--lines', 15);
  const name = args[0];
  const paths = jobPaths(name);
  if (!state(paths)) fail(`no job ${name} in this worktree`);
  const began = Date.now();
  const sizeBefore = statSync(paths.log).size;
  let current;
  while ((current = state(paths)).kind === 'running') {
    if ((Date.now() - began) / 1000 >= max) {
      const lines = logLines(paths.log);
      const seconds = Math.round(current.elapsed);
      const quiet = Math.round((Date.now() - statSync(paths.log).mtimeMs) / 1000);
      console.log(
        `job ${name}: still running after ${seconds}s, last log write ${quiet}s ago, log ${lines.length} lines; run wait again`,
      );
      if (lines.length) console.log(clip(lines.at(-1)));
      if (statSync(paths.log).size === sizeBefore) {
        const lock = spawnSync('uv', ['run', 'python', '-m', 'tools.devtools.benchlock', 'status'], {
          cwd: current.meta.cwd,
          encoding: 'utf8',
          timeout: 20000,
        });
        console.log(
          `no output during this wait; lock: ${lock.status === 0 ? lock.stdout.trim().replaceAll('\n', '; ') : 'unknown'}`,
        );
      }
      process.exit(EXIT_RUNNING);
    }
    await new Promise((resolve) => setTimeout(resolve, 1000));
  }
  const seconds = Math.round(current.elapsed);
  let code;
  if (current.kind === 'died') {
    console.log(`job ${name}: died after ${seconds}s (process gone, no exit status)`);
    code = EXIT_DIED;
  } else if (current.kind === 'timed out') {
    console.log(`job ${name}: timed out after ${seconds}s (limit ${current.meta.timeout}s)`);
    code = EXIT_TIMEOUT;
  } else {
    const { code: status, signal } = current.exit;
    console.log(`job ${name}: exit ${status ?? `signal ${signal}`} after ${seconds}s`);
    code = status ?? 1;
  }
  for (const line of logLines(paths.log).slice(-count)) console.log(clip(line));
  console.log(`log: ${paths.log}`);
  process.exit(code);
}

async function stop(args) {
  const paths = jobPaths(args[0]);
  const current = state(paths);
  if (!current) fail(`no job ${args[0]} in this worktree`);
  if (current.kind !== 'running') return console.log(`job ${args[0]}: ${current.kind}`);
  const target = current.meta.childPid || current.meta.pid;
  const send = (signal) => {
    try {
      process.kill(-target, signal);
    } catch {} // ESRCH: the group is gone
  };
  send('SIGTERM');
  await new Promise((resolve) => setTimeout(resolve, 5000));
  send('SIGKILL');
  console.log(`job ${args[0]}: stopped`);
}

function status(args) {
  const dir = jobsDir();
  const names = args[0] ? [args[0]] : existsSync(dir) ? readdirSync(dir) : [];
  for (const name of names) {
    const current = state(jobPaths(name));
    if (!current) continue;
    const exit = current.exit ? (current.exit.code ?? `signal ${current.exit.signal}`) : '-';
    console.log(`${name}\t${current.kind}\t${Math.round(current.elapsed)}s\t${exit}`);
  }
}

const [command, ...rest] = process.argv.slice(2);
if (command === '_run') runWrapper(rest[0]);
else if (command === 'start') start(rest);
else if (command === 'wait') await wait(rest);
else if (command === 'stop') await stop(rest);
else if (command === 'status') status(rest);
else fail('usage: job.mjs start|wait|stop|status ...');
