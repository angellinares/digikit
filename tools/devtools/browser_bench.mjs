#!/usr/bin/env node
// Headless-browser throughput bench of the coupled browser core (page mode
// `/emulator/?bench=1&seconds=N`, apps/web/src/runtime/bench.ts). Drives Chrome or
// Chromium over CDP with Node's built-in fetch/WebSocket (no dependencies),
// starts the astro dev server itself unless --url is given, and prints one
// JSON document.
//
//   mise run web:bench -- --seconds 10 --runs 3
//   mise run web:bench -- --js-flags=--no-liftoff
//   mise run web:bench -- --profile            # top wasm functions by self time
//
// Options: --seconds N (10) --runs N (1) --js-flags "..." --profile --split 0|1
//   --url http://host:port (use a running dev server) --port N (4399)
//   --chrome PATH (else $CHROME, Google Chrome, Chromium, Edge)
//   --syx PATH (else $DIGI_EMU_SYX, repo-root firmware/Digitone_II_OS1.11.syx, main tree)
//   --timeout S (900) --headed
//   --browser chrome|firefox|safari (chrome). Firefox is driven over WebDriver BiDi (no geckodriver): --firefox PATH
//   (/Applications/Firefox.app/Contents/MacOS/firefox). Safari uses /usr/bin/safaridriver and has no headless mode;
//   set it up once with `safaridriver --enable` and Safari > Develop > Allow Remote Automation.
//   --profile / --profile-out are Chrome-only (exit 2 with other browsers). Results carry browser and browser_version.
//   --profile-out DIR (implies --profile; writes each worker's raw CDP profile to DIR/<run>-<dsp|cf>.cpuprofile for DevTools)
//   env BENCH_CDP_BASE=N (9400): first Chrome CDP port, run i uses N+i; the bench exits 2 if a port it needs is taken
//   --fast off|desktop|auto|desktop+auto|T,M,F (page default is now desktop for firmware listed in
//   config/emulator.json; --fast off measures AOT only)
// The dev server serves the local DSP inputs and the coupled wasm only to
// loopback; nothing firmware-derived is written by this tool. Timed runs must
// hold the bench lock: `mise run web:bench` does (exclusive).
import { execFileSync, spawn } from 'node:child_process';
import fs from 'node:fs';
import net from 'node:net';
import os from 'node:os';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '../..');
const args = process.argv.slice(2);
const opt = (name, fallback) => {
  const i = args.findIndex((a) => a === `--${name}` || a.startsWith(`--${name}=`));
  if (i < 0) return fallback;
  return args[i].includes('=') ? args[i].slice(args[i].indexOf('=') + 1) : args[i + 1];
};
const flag = (name) => args.includes(`--${name}`);
const seconds = Number(opt('seconds', 10));
const runs = Number(opt('runs', 1));
const jsFlags = opt('js-flags', '');
const profileOut = opt('profile-out'); // DIR: also write each raw CDP profile as DIR/<run>-<worker>.cpuprofile (implies --profile)
const profile = flag('profile') || Boolean(profileOut);
const splitOpt = opt('split'); // 0: one worker; 1 (default when isolated): ColdFire and DSP workers
const fastOpt = opt('fast'); // DSP fast tier of the page: off, desktop, auto, desktop+auto or T,M,F (page default: desktop when config/emulator.json lists the firmware)
const timeoutS = Number(opt('timeout', 900));
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
const log = (...m) => console.error('[browser_bench]', ...m);

function findChrome() {
  const candidates = [
    opt('chrome'),
    process.env.CHROME,
    '/Applications/Google Chrome.app/Contents/MacOS/Google Chrome',
    '/Applications/Chromium.app/Contents/MacOS/Chromium',
    '/Applications/Microsoft Edge.app/Contents/MacOS/Microsoft Edge',
  ];
  const found = candidates.find((c) => c && fs.existsSync(c));
  if (!found) throw new Error('no Chrome/Chromium found; pass --chrome PATH');
  return found;
}

function findSyx() {
  const name = 'Digitone_II_OS1.11.syx';
  const main = root.includes(`${path.sep}.claude${path.sep}worktrees${path.sep}`)
    ? root.split(`${path.sep}.claude${path.sep}`)[0]
    : root;
  const found = [
    opt('syx'),
    process.env.DIGI_EMU_SYX,
    path.join(root, 'firmware', name),
    path.join(main, 'firmware', name),
  ].find((c) => c && fs.existsSync(c));
  if (!found) throw new Error(`firmware ${name} not found; pass --syx PATH`);
  return found;
}

async function waitHttp(url, ms = 120000) {
  const end = Date.now() + ms;
  for (;;) {
    try {
      if ((await fetch(url)).ok) return;
    } catch {
      /* not up yet */
    }
    if (Date.now() > end) throw new Error(`timeout waiting for ${url}`);
    await sleep(300);
  }
}

// Refuse to measure a server or browser this run did not start: exit 2 when something already listens on the port.
async function requireFreePort(port) {
  await new Promise((resolve) => {
    const probe = net.createServer();
    probe.once('error', (e) => {
      if (e.code !== 'EADDRINUSE') throw e;
      console.error(
        `[browser_bench] port ${port} is in use: another process is listening there (find it with: lsof -iTCP:${port} -sTCP:LISTEN)`,
      );
      process.exit(2);
    });
    probe.listen(port, '127.0.0.1', () => probe.close(() => resolve()));
  });
}

async function portIsFree(port) {
  return new Promise((resolve) => {
    const probe = net.createServer();
    probe.once('error', () => resolve(false));
    probe.listen(port, '127.0.0.1', () => probe.close(() => resolve(true)));
  });
}

async function waitPortFree(port, ms) {
  const deadline = Date.now() + ms;
  while (Date.now() < deadline) {
    if (await portIsFree(port)) return true;
    await new Promise((r) => setTimeout(r, 100));
  }
  return portIsFree(port);
}

// Listeners on the port whose cwd is apps/web (astro re-spawns itself in a new process group, out of reach of the group kill).
function webListeners(port, webDir) {
  let pids = [];
  try {
    pids = execFileSync('lsof', ['-t', '-iTCP:' + port, '-sTCP:LISTEN'], { encoding: 'utf8' })
      .split('\n')
      .filter(Boolean);
  } catch {
    /* lsof exits nonzero when nothing listens */
  }
  const ours = [];
  for (const pid of pids) {
    let cwd = '';
    try {
      cwd =
        execFileSync('lsof', ['-a', '-p', pid, '-d', 'cwd', '-Fn'], { encoding: 'utf8' })
          .split('\n')
          .find((l) => l.startsWith('n'))
          ?.slice(1) ?? '';
    } catch {
      /* process gone */
    }
    let same = false;
    try {
      same = cwd !== '' && fs.realpathSync(cwd) === webDir;
    } catch {
      /* cwd unreadable */
    }
    if (same) ours.push(Number(pid));
    else log(`port ${port} still held by pid ${pid} (cwd ${cwd}), not ours`);
  }
  return ours;
}

// SIGTERM the server's process group, wait up to 3 s for the port to free; then terminate the apps/web listener
// that escaped the group, wait 3 s more, and SIGKILL it if it is still there.
async function stopDevServer(server, port) {
  try {
    process.kill(-server.pid, 'SIGTERM');
  } catch {
    /* already gone */
  }
  if (await waitPortFree(port, 3000)) return;
  const webDir = fs.realpathSync(path.join(root, 'apps', 'web'));
  for (const pid of webListeners(port, webDir)) {
    log(`killing astro dev pid ${pid} (SIGTERM)`);
    try {
      process.kill(pid, 'SIGTERM');
    } catch {
      /* already gone */
    }
  }
  if (await waitPortFree(port, 3000)) return;
  for (const pid of webListeners(port, webDir)) {
    log(`killing astro dev pid ${pid} (SIGKILL)`);
    try {
      process.kill(pid, 'SIGKILL');
    } catch {
      /* already gone */
    }
  }
}

function startDevServer(port, syx) {
  const cwd = path.join(root, 'apps', 'web');
  const bin = path.join(cwd, 'node_modules', '.bin', 'astro');
  const args = ['dev', '--host', '127.0.0.1', '--port', String(port)];
  const child = spawn(bin, args, {
    cwd,
    env: {
      ...process.env,
      DIGI_EMU_SYX: syx,
      ...(fs.existsSync(path.join(root, 'node_modules'))
        ? { DIGI_VITE_FS_ALLOW: path.dirname(fs.realpathSync(path.join(root, 'node_modules'))) }
        : {}),
    },
    stdio: ['ignore', 'pipe', 'pipe'],
    detached: true,
  });
  child.devPort = port;
  child.stdout.on('data', () => {});
  child.stderr.on('data', () => {});
  return child;
}

class Cdp {
  constructor(ws) {
    this.ws = ws;
    this.id = 0;
    this.pending = new Map();
    this.handlers = [];
    ws.onmessage = (e) => this.onMessage(JSON.parse(e.data));
  }
  static async connect(url) {
    const ws = new WebSocket(url);
    await new Promise((res, rej) => {
      ws.onopen = res;
      ws.onerror = rej;
    });
    return new Cdp(ws);
  }
  onMessage(m) {
    if (m.id && this.pending.has(m.id)) {
      const { resolve, reject } = this.pending.get(m.id);
      this.pending.delete(m.id);
      if (m.error) reject(new Error(`${m.error.message}`));
      else resolve(m.result);
      return;
    }
    if (m.method) for (const h of this.handlers) h(m);
  }
  send(method, params = {}, sessionId) {
    const id = ++this.id;
    return new Promise((resolve, reject) => {
      this.pending.set(id, { resolve, reject });
      this.ws.send(JSON.stringify({ id, method, params, ...(sessionId ? { sessionId } : {}) }));
    });
  }
  close() {
    this.ws.close();
  }
}

// Self time per wasm/JS function from a CPU profile; DSP kernels are the
// generated `sharc` functions, the rest is ColdFire, host glue and JS.
function selfTimes(profileData) {
  const byId = new Map(profileData.nodes.map((n) => [n.id, n]));
  const self = new Map();
  const deltas = profileData.timeDeltas;
  profileData.samples.forEach((id, i) => {
    const node = byId.get(id);
    const cf = node.callFrame;
    const key = `${cf.functionName || '(anon)'} ${cf.url ? cf.url.split('/').pop() : ''}`.trim();
    self.set(key, (self.get(key) ?? 0) + (deltas[i] ?? 0));
  });
  const total = [...self.values()].reduce((a, b) => a + b, 0);
  // DSP: generated SHARC+ kernels and the engine around them. ColdFire: interpreter, bus and board. Idle is the worker waiting.
  const categories = { dsp: 0, coldfire: 0, idle: 0, other: 0 };
  for (const [name, us] of self) {
    const kind = name.startsWith('(idle)')
      ? 'idle'
      : /sharc_gen|sharc_native|sharc_rt|sharc_fast/.test(name)
        ? 'dsp'
        : /coldfire|elektron_native|machine|periph|plusdrive|card|loader/.test(name)
          ? 'coldfire'
          : 'other';
    categories[kind] += us;
  }
  const busy = categories.dsp + categories.coldfire + categories.other;
  const split = {
    ...Object.fromEntries(Object.entries(categories).map(([k, us]) => [`${k}_ms`, +(us / 1000).toFixed(1)])),
    dsp_share_of_busy: +(categories.dsp / busy).toFixed(3),
    coldfire_share_of_busy: +(categories.coldfire / busy).toFixed(3),
  };
  const top = [...self.entries()]
    .sort((a, b) => b[1] - a[1])
    .slice(0, 40)
    .map(([name, us]) => ({ name, self_ms: +(us / 1000).toFixed(1), pct: +((100 * us) / total).toFixed(2) }));
  return { total_ms: +(total / 1000).toFixed(1), split, top, all: self };
}

async function waitPort(port, ms = 20000) {
  const end = Date.now() + ms;
  for (;;) {
    const ok = await new Promise((resolve) => {
      const s = net.connect(port, '127.0.0.1', () => {
        s.destroy();
        resolve(true);
      });
      s.once('error', () => resolve(false));
    });
    if (ok) return;
    if (Date.now() > end) throw new Error(`timeout waiting for port ${port}`);
    await sleep(300);
  }
}

// Browser drivers: start(), navigate(url), evaluate(expr) -> string|null, stop(); `browser` and `version` after start().
function chromeDriver(chrome, port) {
  const profileDir = fs.mkdtempSync(path.join(os.tmpdir(), 'digi-bench-'));
  const chromeArgs = [
    '--headless=new',
    `--remote-debugging-port=${port}`,
    `--user-data-dir=${profileDir}`,
    '--no-first-run',
    '--no-default-browser-check',
    '--autoplay-policy=no-user-gesture-required',
    '--disable-background-timer-throttling',
    '--disable-renderer-backgrounding',
    '--disable-backgrounding-occluded-windows',
    ...(jsFlags ? [`--js-flags=${jsFlags}`] : []),
    'about:blank',
  ];
  if (flag('headed')) chromeArgs.splice(0, 1);
  let proc;
  const d = {
    browser: 'chrome',
    version: undefined,
    ctx: undefined,
    async start() {
      proc = spawn(chrome, chromeArgs, { stdio: ['ignore', 'ignore', 'ignore'] });
      await waitHttp(`http://127.0.0.1:${port}/json/version`, 30000);
      const version = await (await fetch(`http://127.0.0.1:${port}/json/version`)).json();
      d.version = version.Browser;
      await d.attach(version);
    },
    async attach(version) {
      const cdp = await Cdp.connect(version.webSocketDebuggerUrl);
      const { targetId } = await cdp.send('Target.createTarget', { url: 'about:blank' });
      const { sessionId } = await cdp.send('Target.attachToTarget', { targetId, flatten: true });
      const workerSessions = [];
      const profiles = [];
      d.ctx = { cdp, sessionId, workerSessions, profiles };
      cdp.handlers.push((m) => {
        if (m.sessionId && m.method === 'Runtime.exceptionThrown')
          log('page exception:', m.params.exceptionDetails.exception?.description ?? m.params.exceptionDetails.text);
        if (m.sessionId && m.method === 'Runtime.consoleAPICalled' && flag('verbose'))
          log('console:', m.params.args.map((a) => a.value ?? a.description).join(' '));
        if (m.method === 'Target.detachedFromTarget') {
          const k = workerSessions.indexOf(m.params.sessionId);
          if (k >= 0) workerSessions.splice(k, 1);
        }
        if (m.method === 'Target.attachedToTarget' && m.params.targetInfo.type === 'worker') {
          const ws = m.params.sessionId;
          workerSessions.push(ws);
          (async () => {
            await cdp.send('Runtime.enable', {}, ws).catch(() => {});
            if (profile) {
              await cdp.send('Profiler.enable', {}, ws);
              await cdp.send('Profiler.setSamplingInterval', { interval: 500 }, ws);
              await cdp.send('Profiler.start', {}, ws);
            }
            await cdp.send('Runtime.runIfWaitingForDebugger', {}, ws);
          })().catch((e) => log('worker attach failed:', e.message));
        }
      });
      await cdp.send(
        'Target.setAutoAttach',
        { autoAttach: true, waitForDebuggerOnStart: true, flatten: true },
        sessionId,
      );
      await cdp.send('Runtime.enable', {}, sessionId);
      await cdp.send('Page.enable', {}, sessionId);
    },
    async navigate(u) {
      await d.ctx.cdp.send('Page.navigate', { url: u }, d.ctx.sessionId);
    },
    async evaluate(expression) {
      const r = await d.ctx.cdp
        .send('Runtime.evaluate', { expression, returnByValue: true }, d.ctx.sessionId)
        .catch(() => undefined);
      return r?.result?.value ?? null;
    },
    async stop() {
      try {
        d.ctx?.cdp.close();
      } catch {
        /* already closed */
      }
      proc?.kill('SIGKILL');
      await sleep(300);
      fs.rmSync(profileDir, { recursive: true, force: true });
    },
  };
  return d;
}

// Firefox over WebDriver BiDi (no geckodriver).
function firefoxDriver(firefox, port) {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'digi-bench-ff-'));
  fs.writeFileSync(
    path.join(dir, 'user.js'),
    'user_pref("browser.shell.checkDefaultBrowser", false); user_pref("datareporting.policy.dataSubmissionEnabled", false); user_pref("browser.aboutwelcome.enabled", false); user_pref("remote.prefs.recommended", true);\n',
  );
  const ffArgs = ['--headless', '--no-remote', '--profile', dir, '--remote-debugging-port', String(port)];
  if (flag('headed')) ffArgs.splice(0, 1);
  let proc;
  let ws;
  let id = 0;
  let context;
  const pending = new Map();
  const send = (method, params = {}) =>
    new Promise((resolve, reject) => {
      const n = ++id;
      pending.set(n, { resolve, reject });
      ws.send(JSON.stringify({ id: n, method, params }));
    });
  const d = {
    browser: 'firefox',
    version: undefined,
    async start() {
      proc = spawn(firefox, ffArgs, { stdio: ['ignore', 'ignore', 'ignore'] });
      await waitPort(port, 20000);
      ws = new WebSocket(`ws://127.0.0.1:${port}/session`);
      await new Promise((res, rej) => {
        ws.onopen = res;
        ws.onerror = rej;
      });
      ws.onmessage = (e) => {
        const m = JSON.parse(e.data);
        if (!m.id || !pending.has(m.id)) return;
        const { resolve, reject } = pending.get(m.id);
        pending.delete(m.id);
        if (m.type === 'error') reject(new Error(`${m.error}: ${m.message}`));
        else resolve(m.result);
      };
      const s = await send('session.new', { capabilities: {} });
      d.version = s.capabilities?.browserVersion;
      const tree = await send('browsingContext.getTree', {});
      context = tree.contexts[0].context;
    },
    async navigate(url) {
      await send('browsingContext.navigate', { context, url, wait: 'complete' });
    },
    async evaluate(expression) {
      const r = await send('script.evaluate', { expression, target: { context }, awaitPromise: false }).catch(
        () => undefined,
      );
      return r?.result?.type === 'string' ? r.result.value : null;
    },
    async stop() {
      try {
        await send('session.end', {});
      } catch {
        /* ignore */
      }
      try {
        ws?.close();
      } catch {
        /* ignore */
      }
      proc?.kill('SIGKILL');
      await sleep(300);
      fs.rmSync(dir, { recursive: true, force: true });
    },
  };
  return d;
}

// Safari through safaridriver (classic WebDriver). Safari has no headless mode.
function safariDriver(port) {
  const base = `http://127.0.0.1:${port}`;
  let proc;
  let sid;
  const call = async (method, p, body) => {
    const res = await fetch(base + p, {
      method,
      headers: { 'content-type': 'application/json' },
      ...(body === undefined ? {} : { body: JSON.stringify(body) }),
    });
    const j = await res.json().catch(() => ({}));
    if (!res.ok) throw Object.assign(new Error(`${method} ${p}: ${JSON.stringify(j.value ?? j)}`), { value: j.value });
    return j.value;
  };
  const d = {
    browser: 'safari',
    version: undefined,
    async start() {
      proc = spawn('/usr/bin/safaridriver', ['-p', String(port)], { stdio: ['ignore', 'ignore', 'ignore'] });
      await waitHttp(`${base}/status`, 20000);
      try {
        const v = await call('POST', '/session', { capabilities: { alwaysMatch: { browserName: 'safari' } } });
        sid = v.sessionId;
        d.version = v.capabilities?.browserVersion;
      } catch (e) {
        log('safaridriver session failed:', e.value ? JSON.stringify(e.value) : e.message);
        log('hint: run `safaridriver --enable` once and enable Develop > Allow Remote Automation in Safari');
        proc?.kill('SIGKILL');
        process.exit(2);
      }
    },
    async navigate(url) {
      await call('POST', `/session/${sid}/url`, { url });
    },
    async evaluate(expr) {
      const v = await call('POST', `/session/${sid}/execute/sync`, { script: `return ${expr};`, args: [] }).catch(
        () => undefined,
      );
      return v ?? null;
    },
    async stop() {
      if (sid) await call('DELETE', `/session/${sid}`).catch(() => {});
      proc?.kill('SIGKILL');
      await sleep(300);
    },
  };
  return d;
}

async function oneRun(url, driver, port) {
  try {
    await driver.start();
    const { cdp, workerSessions, profiles } = driver.ctx ?? {};
    // Warm the dev server (Vite may optimise dependencies and reload on the first visit).
    await driver.navigate(`${url}/emulator/`);
    await sleep(4000);
    await driver.navigate(
      `${url}/emulator/?bench=1&seconds=${seconds}${profile ? '&hold=1' : ''}${splitOpt === undefined ? '' : `&split=${splitOpt}`}${fastOpt === undefined ? '' : `&fast=${encodeURIComponent(fastOpt)}`}`,
    );
    const end = Date.now() + timeoutS * 1000;
    let result;
    for (;;) {
      const s = await driver.evaluate('JSON.stringify(window.__bench ?? null)');
      const v = s ? JSON.parse(s) : null;
      if (v && v.state !== 'running') {
        result = v;
        break;
      }
      if (Date.now() > end) throw new Error('bench timed out');
      await sleep(500);
    }
    if (profile) {
      for (const ws of workerSessions) {
        const { profile: p } = await cdp.send('Profiler.stop', {}, ws).catch((e) => {
          log('Profiler.stop failed:', e.message);
          return {};
        });
        if (p) profiles.push({ ...selfTimes(p), raw: p });
      }
      if (profiles.length) {
        // One entry per worker (the ColdFire worker's top is the wait on the DSP; the DSP worker's is the generated core).
        const shape = (w) => ({
          total_ms: w.total_ms,
          split: w.split,
          top: w.top.map((t) => ({
            ...t,
            name: t.name
              .replace(/^_R[A-Za-z]*?\d+/, '')
              .replace(/ emulator-core-sharc.wasm$/, '')
              .slice(0, 110),
          })),
        });
        profiles.sort((a, b) => b.split.dsp_ms - a.split.dsp_ms);
        result.profile = shape(profiles[0]);
        result.profiles = profiles.map(shape);
        if (profileOut) {
          // Worker labels: the profile with the most DSP time is dsp, the others cf (cf2, ... when several), numbered by run.
          fs.mkdirSync(profileOut, { recursive: true });
          profiles.forEach((w, k) => {
            const label = (k === 0 ? 'dsp' : k === 1 ? 'cf' : `cf${k}`).replace(/[^A-Za-z0-9_-]/g, '_');
            const file = path.join(
              profileOut,
              `${port - Number(process.env.BENCH_CDP_BASE || 9400) + 1}-${label}.cpuprofile`,
            );
            fs.writeFileSync(file, JSON.stringify(w.raw));
            log(`wrote ${file}`);
          });
        }
        // --profile-all FILE: every function's self time (ms) of every worker, for grouping.
        if (opt('profile-all'))
          fs.writeFileSync(
            opt('profile-all'),
            JSON.stringify(profiles.map((w) => [...w.all].map(([name, us]) => [name, +(us / 1000).toFixed(2)]))),
          );
      }
    }
    result.browser = driver.browser;
    if (driver.version) result.browser_version = driver.version;
    return result;
  } finally {
    await driver.stop();
  }
}

const browser = opt('browser', 'chrome');
if (!['chrome', 'firefox', 'safari'].includes(browser)) {
  console.error(`[browser_bench] unknown --browser ${browser} (chrome|firefox|safari)`);
  process.exit(2);
}
if (browser !== 'chrome' && profile) {
  console.error('[browser_bench] --profile and --profile-out need --browser chrome');
  process.exit(2);
}
const chrome = browser === 'chrome' ? findChrome() : undefined;
const firefox = opt('firefox', '/Applications/Firefox.app/Contents/MacOS/firefox');
let server;
let url = opt('url');
try {
  const cdpBase = Number(process.env.BENCH_CDP_BASE || 9400);
  for (let i = 0; i < runs; i += 1) await requireFreePort(cdpBase + i);
  if (!url) {
    const port = Number(opt('port', 4399));
    await requireFreePort(port);
    const syx = findSyx();
    log(`starting astro dev on ${port} (firmware ${syx})`);
    server = startDevServer(port, syx);
    url = `http://127.0.0.1:${port}`;
    await waitHttp(url);
  }
  const out = [];
  for (let i = 0; i < runs; i += 1) {
    log(`run ${i + 1}/${runs} flags=${JSON.stringify(jsFlags)} seconds=${seconds}`);
    const dport = Number(process.env.BENCH_CDP_BASE || 9400) + i;
    const driver =
      browser === 'chrome'
        ? chromeDriver(chrome, dport)
        : browser === 'firefox'
          ? firefoxDriver(firefox, dport)
          : safariDriver(dport);
    const r = await oneRun(url, driver, dport);
    out.push(r);
    log(
      r.state === 'done'
        ? `audio/wall ${r.audio_per_wall} (first half ${r.rate_first_half}, last half ${r.rate_last_half})`
        : `error: ${r.error}`,
    );
  }
  const summary = { browser, ...(chrome ? { chrome } : {}), js_flags: jsFlags, seconds, profile, runs: out };
  console.log(JSON.stringify(summary, null, 2));
  if (out.some((r) => r.state !== 'done')) process.exitCode = 1;
} finally {
  if (server) await stopDevServer(server, server.devPort);
}
