// Launcher of the shared Nx remote cache: one server for every worktree of the clone.
// The gate and plain launcher runs read and write (rw); ro is for read-only callers. The Nx variables are set only when the server
// answers, because with the server down Nx fails a green run.
// Usage: node tools/nx/remote-cache.mjs exec --access ro|rw [--no-start] -- CMD [ARGS...]
//        --no-start: use the server only if it already answers; start nothing, say nothing when it is down.
//        node tools/nx/remote-cache.mjs start | status | stop
import { spawn, spawnSync } from 'node:child_process';
import { randomBytes } from 'node:crypto';
import {
  closeSync,
  existsSync,
  linkSync,
  mkdirSync,
  openSync,
  readFileSync,
  readdirSync,
  statSync,
  unlinkSync,
  writeFileSync,
} from 'node:fs';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

const root = resolve(dirname(fileURLToPath(import.meta.url)), '../..');
const serverDir = join(root, 'tools/nx/cache-server');
const serverJs = join(serverDir, 'server.mjs');
const NX_VARS = ['NX_SELF_HOSTED_REMOTE_CACHE_SERVER', 'NX_SELF_HOSTED_REMOTE_CACHE_ACCESS_TOKEN'];

const cleanEnv = () => {
  const env = { ...process.env };
  for (const k of NX_VARS) delete env[k];
  return env;
};

const git = spawnSync('git', ['rev-parse', '--path-format=absolute', '--git-common-dir'], {
  cwd: root,
  encoding: 'utf8',
});
if (git.status !== 0) {
  console.error('remote-cache: not a git checkout');
  process.exit(1);
}
const state = join(git.stdout.trim(), 'nx-remote-cache');
mkdirSync(state, { recursive: true });
const configPath = join(state, 'config.json');
const pidPath = join(state, 'server.pid');
const logPath = join(state, 'server.log');

const readConfig = () => {
  if (existsSync(configPath)) return JSON.parse(readFileSync(configPath, 'utf8'));
  const config = {
    port: 47311,
    store: join(state, 'store'),
    ttlDays: 14,
    token: randomBytes(32).toString('hex'),
    readOnlyToken: randomBytes(32).toString('hex'),
  };
  const tmp = `${configPath}.${process.pid}.tmp`;
  writeFileSync(tmp, JSON.stringify(config, null, 2), { mode: 0o600 });
  try {
    linkSync(tmp, configPath);
  } catch (e) {
    if (e.code !== 'EEXIST') throw e;
  }
  unlinkSync(tmp);
  return JSON.parse(readFileSync(configPath, 'utf8'));
};

const healthy = async (port) => {
  try {
    const res = await fetch(`http://127.0.0.1:${port}/health`, { signal: AbortSignal.timeout(500) });
    return res.status === 200;
  } catch {
    return false;
  }
};

const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

const ensure = async () => {
  const config = readConfig();
  if (await healthy(config.port)) return config;
  if (!existsSync(join(serverDir, 'node_modules'))) {
    const npm = spawnSync(
      'npm',
      ['ci', '--ignore-scripts', '--no-audit', '--no-fund', '--prefix', 'tools/nx/cache-server'],
      { cwd: root, stdio: 'ignore' },
    );
    if (npm.status !== 0) return null;
  }
  const log = openSync(logPath, 'a');
  const child = spawn(process.execPath, [serverJs, '--config', configPath], {
    cwd: root,
    detached: true,
    stdio: ['ignore', log, log],
    env: cleanEnv(),
  });
  child.unref();
  closeSync(log);
  writeFileSync(pidPath, String(child.pid));
  for (let i = 0; i < 75; i++) {
    if (await healthy(config.port)) return config;
    await sleep(200);
  }
  return null;
};

const dirStats = (dir) => {
  let count = 0;
  let bytes = 0;
  const walk = (d) => {
    for (const e of readdirSync(d, { withFileTypes: true })) {
      const p = join(d, e.name);
      if (e.isDirectory()) walk(p);
      else if (e.isFile()) {
        count++;
        bytes += statSync(p).size;
      }
    }
  };
  if (existsSync(dir)) walk(dir);
  return { count, bytes };
};

const [cmd, ...rest] = process.argv.slice(2);

if (cmd === 'exec') {
  const sep = rest.indexOf('--');
  const opts = sep >= 0 ? rest.slice(0, sep) : rest;
  const argv = sep >= 0 ? rest.slice(sep + 1) : [];
  const access = opts[opts.indexOf('--access') + 1];
  if (!['ro', 'rw'].includes(access) || !argv.length) {
    console.error('usage: remote-cache.mjs exec --access ro|rw -- CMD [ARGS...]');
    process.exit(2);
  }
  const env = cleanEnv();
  const noStart = opts.includes('--no-start');
  let config = null;
  if (noStart) {
    const known = readConfig();
    config = (await healthy(known.port)) ? known : null;
  } else config = await ensure();
  if (config) {
    env.NX_SELF_HOSTED_REMOTE_CACHE_SERVER = `http://127.0.0.1:${config.port}`;
    env.NX_SELF_HOSTED_REMOTE_CACHE_ACCESS_TOKEN = access === 'rw' ? config.token : config.readOnlyToken;
  } else if (!noStart) {
    console.error(`nx remote cache unavailable (log: ${logPath}); running without it`);
  }
  const child = spawn(argv[0], argv.slice(1), { stdio: 'inherit', env, shell: false });
  child.on('error', (e) => {
    console.error(`remote-cache: ${e.message}`);
    process.exit(127);
  });
  child.on('exit', (code, signal) => process.exit(signal ? 1 : (code ?? 1)));
} else if (cmd === 'start') {
  const config = await ensure();
  if (!config) {
    console.error(`nx remote cache did not start (log: ${logPath})`);
    process.exit(1);
  }
  console.log(`http://127.0.0.1:${config.port}`);
} else if (cmd === 'status') {
  const config = readConfig();
  const up = await healthy(config.port);
  const { count, bytes } = dirStats(config.store);
  console.log(`healthy: ${up ? 'yes' : 'no'}`);
  console.log(`url: http://127.0.0.1:${config.port}`);
  console.log(`store: ${config.store}`);
  console.log(`entries: ${count}, bytes: ${bytes}`);
} else if (cmd === 'stop') {
  let stopped = false;
  if (existsSync(pidPath)) {
    const pid = readFileSync(pidPath, 'utf8').trim();
    const ps = spawnSync('ps', ['-p', pid, '-o', 'command='], { encoding: 'utf8' });
    if (/^\d+$/.test(pid) && ps.stdout.includes('cache-server/server.mjs')) {
      process.kill(Number(pid), 'SIGTERM');
      unlinkSync(pidPath);
      stopped = true;
      console.log(`stopped server ${pid}`);
    }
  }
  if (!stopped) console.log('no server of ours runs');
} else {
  console.error('usage: remote-cache.mjs exec|start|status|stop');
  process.exit(2);
}
