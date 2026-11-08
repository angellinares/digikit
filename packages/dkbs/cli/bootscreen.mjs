#!/usr/bin/env node
// Boot screen authoring without a device or emulator. SRC is an .mjs file that exports `ticks` (integer 1..300) and `draw(ctx, t)`;
// ctx is a CanvasRenderingContext2D from @napi-rs/canvas (128x64) and t is the integer tick. The same pack function serves all commands.
//
//     node packages/dkbs/cli/bootscreen.mjs pack SRC [-o OUT.dkbs]   default out/bootscreens/<name>.dkbs
//     node packages/dkbs/cli/bootscreen.mjs sheet SRC [-o OUT.png]   contact sheet of up to 24 ticks, drawn from the decoded .dkbs bytes
//     node packages/dkbs/cli/bootscreen.mjs dev SRC                  pack, then a Vite preview that reloads when SRC changes
//     node packages/dkbs/cli/bootscreen.mjs [gallery] [--no-open]   gallery of modules/tools/bootscreen/examples/*.mjs in a browser (the default command)
//     node packages/dkbs/cli/bootscreen.mjs put SRC [--port SUB] [--bin PATH]   pack, then upload to data/tools/bootscreen/<name>.dkbs on the device
import { spawn, spawnSync } from 'node:child_process';
import { mkdirSync, statSync, writeFileSync } from 'node:fs';
import path from 'node:path';
import { fileURLToPath, pathToFileURL } from 'node:url';
import { createCanvas } from '@napi-rs/canvas';
import { discoverExamples } from './examples.ts';
import {
  bitmapPixel,
  decode,
  encode,
  MAX_FILE_BYTES,
  MAX_TICKS,
  rgbaToBitmap,
  TICKS_PER_SECOND,
} from '../src/index.ts';

const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '../../..');
const usage =
  'usage: node packages/dkbs/cli/bootscreen.mjs [gallery [--no-open]] | pack SRC [-o OUT] | sheet SRC [-o PNG] | dev SRC | put SRC [--port SUB] [--bin PATH]';

function fail(message) {
  console.error(`error: ${message}`);
  process.exit(1);
}

/** Run the script for every tick and encode the result. Throws on any script or format error. */
export async function packScript(src) {
  const file = path.resolve(src);
  let mtime;
  try {
    mtime = statSync(file).mtimeMs;
  } catch (e) {
    throw new Error(`cannot read ${src}: ${e.message}`);
  }
  const mod = await import(`${pathToFileURL(file).href}?v=${mtime}`);
  const { ticks, draw } = mod;
  if (!Number.isInteger(ticks) || ticks < 1 || ticks > MAX_TICKS)
    throw new Error(`ticks must be an integer in 1..${MAX_TICKS}`);
  if (typeof draw !== 'function') throw new Error('the script must export a function draw(ctx, t)');
  const canvas = createCanvas(128, 64);
  const ctx = canvas.getContext('2d');
  const frames = [];
  for (let t = 0; t < ticks; t += 1) {
    ctx.save();
    ctx.clearRect(0, 0, 128, 64);
    try {
      await draw(ctx, t);
    } catch (e) {
      throw new Error(`draw failed at tick ${t}: ${e.message}`);
    }
    ctx.restore();
    frames.push({ bitmap: rgbaToBitmap(ctx.getImageData(0, 0, 128, 64).data), ticks: 1 });
  }
  const bytes = encode(frames);
  return { bytes, ticks, seconds: ticks / TICKS_PER_SECOND, kept: decode(bytes).frames.length };
}

const name = (src) => path.basename(src, path.extname(src));
const outDefault = (src, ext) => path.join(root, 'out/bootscreens', name(src) + ext);

function write(file, data) {
  mkdirSync(path.dirname(file), { recursive: true });
  writeFileSync(file, data);
}

function sheetPng(bytes) {
  const { ticks, frames } = decode(bytes);
  const shown = Math.min(24, ticks);
  const picks = Array.from({ length: shown }, (_, i) =>
    shown === 1 ? 0 : Math.round((i * (ticks - 1)) / (shown - 1)),
  );
  const cols = Math.min(4, shown);
  const rows = Math.ceil(shown / cols);
  const cellW = 128 * 2 + 8;
  const cellH = 64 * 2 + 24;
  const canvas = createCanvas(cols * cellW + 8, rows * cellH + 8);
  const ctx = canvas.getContext('2d');
  ctx.fillStyle = '#16171c';
  ctx.fillRect(0, 0, canvas.width, canvas.height);
  ctx.font = '14px sans-serif';
  const starts = [];
  let sum = 0;
  for (const f of frames) {
    starts.push(sum);
    sum += f.ticks;
  }
  picks.forEach((tick, i) => {
    let k = starts.length - 1;
    while (k > 0 && starts[k] > tick) k -= 1;
    const bitmap = frames[k].bitmap;
    const ox = 8 + (i % cols) * cellW;
    const oy = 8 + Math.floor(i / cols) * cellH;
    for (let y = 0; y < 64; y += 1)
      for (let x = 0; x < 128; x += 1) {
        ctx.fillStyle = bitmapPixel(bitmap, x, y) ? '#f2f4f9' : '#08090c';
        ctx.fillRect(ox + 2 * x, oy + 2 * y, 2, 2);
      }
    ctx.fillStyle = '#9aa0b0';
    ctx.fillText(String(tick), ox, oy + 128 + 16);
  });
  return canvas.toBuffer('image/png');
}

function summary(r) {
  return `${r.ticks} ticks, ${r.seconds.toFixed(2)} s, ${r.kept} frames kept, ${r.bytes.length} of ${MAX_FILE_BYTES} bytes`;
}

async function dev(src) {
  const { createServer } = await import('vite');
  const state = {
    bytes: new Uint8Array(0),
    status: { ok: false, error: 'not packed yet', ticks: 0, seconds: 0, bytes: 0 },
  };
  const repack = async () => {
    try {
      const r = await packScript(src);
      state.bytes = r.bytes;
      state.status = { ok: true, error: '', ticks: r.ticks, seconds: r.seconds, bytes: r.bytes.length };
      console.log(`packed: ${summary(r)}`);
    } catch (e) {
      // keep serving the last good asset
      state.status = { ...state.status, ok: false, error: e.message };
      console.error(`error: ${e.message}`);
    }
  };
  await repack();
  const file = path.resolve(src);
  let timer;
  const plugin = {
    name: 'bootscreen-dev',
    configureServer(server) {
      server.middlewares.use((req, res, next) => {
        const url = (req.url ?? '').split('?')[0];
        if (url === '/asset.dkbs') {
          res.setHeader('content-type', 'application/octet-stream');
          res.setHeader('cache-control', 'no-store');
          res.end(Buffer.from(state.bytes));
        } else if (url === '/status') {
          res.setHeader('content-type', 'application/json');
          res.setHeader('cache-control', 'no-store');
          res.end(JSON.stringify(state.status));
        } else next();
      });
      server.watcher.add(file);
      server.watcher.on('change', (changed) => {
        if (path.resolve(changed) !== file) return;
        clearTimeout(timer);
        timer = setTimeout(async () => {
          await repack();
          server.ws.send({ type: 'custom', event: 'bootscreen:update', data: state.status });
        }, 50);
      });
    },
  };
  const server = await createServer({
    root: path.join(root, 'packages/dkbs/preview'),
    configFile: false,
    plugins: [plugin],
    server: { fs: { allow: [root] } },
  });
  await server.listen();
  server.printUrls();
}

const examplesDir = path.join(root, 'modules/tools/bootscreen/examples');
const CONCURRENCY = 4;

/** Pack SRC in a short-lived child process. A fresh process re-imports every helper the script uses, so no change in the examples folder is served stale. */
function packInChild(src) {
  return new Promise((resolve, reject) => {
    const child = spawn(process.execPath, [...process.execArgv, fileURLToPath(import.meta.url), 'pack-json', src], {
      stdio: ['ignore', 'pipe', 'pipe'],
    });
    let out = '';
    let err = '';
    child.stdout.on('data', (d) => (out += d));
    child.stderr.on('data', (d) => (err += d));
    child.on('error', reject);
    child.on('close', (code) => {
      if (code !== 0) return reject(new Error(err.replace(/^error: /, '').trim() || `pack exited with ${code}`));
      const r = JSON.parse(out);
      resolve({ ...r, bytes: Buffer.from(r.bytes, 'base64') });
    });
  });
}

function openBrowser(url) {
  const [cmd, ...a] =
    process.platform === 'darwin'
      ? ['open', url]
      : process.platform === 'win32'
        ? ['cmd', '/c', 'start', '', url]
        : ['xdg-open', url];
  spawn(cmd, a, { stdio: 'ignore', detached: true })
    .on('error', () => {})
    .unref();
}

async function gallery(noOpen) {
  const { createServer } = await import('vite');
  /** name -> { file, bytes, ok, error, ticks, seconds, size, v, gen } */
  const examples = new Map();
  const list = () =>
    [...examples.values()]
      .filter((e) => e.v > 0 || e.error)
      .map((e) => ({
        name: e.name,
        ticks: e.ticks,
        seconds: e.seconds,
        bytes: e.size,
        ok: e.ok,
        error: e.error,
        v: e.v,
      }));
  let server;
  const notify = () => server?.ws.send({ type: 'custom', event: 'bootscreen:gallery', data: list() });
  let running = 0;
  const queue = [];
  const pump = () => {
    while (running < CONCURRENCY && queue.length) {
      running += 1;
      queue
        .shift()()
        .finally(() => {
          running -= 1;
          pump();
        });
    }
  };
  const repack = (e, error) => {
    const gen = (e.gen = (e.gen ?? 0) + 1);
    queue.push(async () => {
      let result;
      try {
        if (error) throw new Error(error);
        result = await packInChild(e.file);
      } catch (err) {
        result = { error: err.message };
      }
      if (e.gen !== gen || examples.get(e.name) !== e) return;
      if (result.error) {
        e.ok = false;
        e.error = result.error;
        console.error(`${e.name}: ${result.error}`);
      } else {
        Object.assign(e, {
          ok: true,
          error: '',
          bytes: result.bytes,
          ticks: result.ticks,
          seconds: result.seconds,
          size: result.bytes.length,
        });
        e.v += 1;
        console.log(`${e.name}: ${summary(result)}`);
      }
      notify();
    });
    pump();
  };
  /** Rediscover the folder and repack the changed names. A changed file that is not an example is a helper: repack every example. */
  const refresh = async (names) => {
    const found = await discoverExamples(examplesDir);
    const only = names && [...names].every((n) => found.some((f) => f.name === n)) ? names : undefined;
    for (const name of examples.keys()) if (!found.some((f) => f.name === name)) examples.delete(name);
    for (const f of found) {
      let e = examples.get(f.name);
      if (!e)
        examples.set(
          f.name,
          (e = {
            name: f.name,
            file: f.file,
            bytes: new Uint8Array(0),
            ok: false,
            error: '',
            ticks: 0,
            seconds: 0,
            size: 0,
            v: 0,
          }),
        );
      if (only === undefined || only.has(f.name) || e.v === 0) repack(e, f.error);
    }
    notify();
  };
  let timer;
  let changed = new Set();
  const plugin = {
    name: 'bootscreen-gallery',
    configureServer(s) {
      server = s;
      s.middlewares.use((req, res, next) => {
        const url = (req.url ?? '').split('?')[0];
        res.setHeader('cache-control', 'no-store');
        if (url === '/examples') {
          res.setHeader('content-type', 'application/json');
          res.end(JSON.stringify(list()));
        } else if (url.startsWith('/example/') && url.endsWith('.dkbs')) {
          const e = examples.get(decodeURIComponent(url.slice(9, -5)));
          if (!e || !e.bytes.length) {
            res.statusCode = 404;
            res.end();
          } else {
            res.setHeader('content-type', 'application/octet-stream');
            res.end(Buffer.from(e.bytes));
          }
        } else next();
      });
      s.watcher.add(examplesDir);
      const onFile = (file) => {
        if (!file.endsWith('.mjs') || path.dirname(path.resolve(file)) !== examplesDir) return;
        changed.add(path.basename(file, '.mjs'));
        clearTimeout(timer);
        timer = setTimeout(async () => {
          const names = changed;
          changed = new Set();
          await refresh(names);
        }, 50);
      };
      for (const ev of ['change', 'add', 'unlink']) s.watcher.on(ev, onFile);
    },
  };
  server = await createServer({
    root: path.join(root, 'packages/dkbs/preview'),
    configFile: false,
    plugins: [plugin],
    server: { fs: { allow: [root] } },
  });
  await server.listen();
  server.printUrls();
  const base = server.resolvedUrls?.local[0] ?? 'http://localhost:5173/';
  const url = `${base}gallery.html`;
  console.log(`gallery: ${url}`);
  if (!noOpen) openBrowser(url);
  await refresh(undefined);
}

const args = process.argv.slice(2);
const command = args[0] && !args[0].startsWith('-') ? args[0] : 'gallery';
const src = args[1];
let out;
const oi = args.findIndex((a) => a === '-o');
if (oi >= 0) {
  out = args[oi + 1];
  if (!out) fail('-o needs a value');
}
if (!['pack', 'pack-json', 'sheet', 'dev', 'put', 'gallery'].includes(command) || (command !== 'gallery' && !src))
  fail(usage);

/** The value of an option such as --port, or undefined. */
function option(flag) {
  const i = args.indexOf(flag);
  if (i < 0) return undefined;
  if (!args[i + 1]) fail(`${flag} needs a value`);
  return args[i + 1];
}

/** Pack SRC, then send it to data/tools/bootscreen/<name>.dkbs with `wb.mjs store put` (spawned: wb.mjs runs its command on import). */
function put(r) {
  const file = path.resolve(outDefault(src, '.dkbs'));
  write(file, r.bytes);
  const fileName = `${name(src)}.dkbs`;
  // the module's chooser lists names shorter than 48 characters (modules/tools/bootscreen/src/module.c)
  if (fileName.length >= 48) {
    console.error(
      `${fileName} is ${fileName.length} characters; the file name must be shorter than 48. Rename the source.`,
    );
    process.exit(1);
  }
  const remote = `data/tools/bootscreen/${fileName}`;
  const wb = ['--port', option('--port'), '--bin', option('--bin')].filter((v) => v !== undefined);
  const run = spawnSync(
    process.execPath,
    [path.join(root, 'tools/devtools/wb.mjs'), ...wb, 'store', 'put', file, remote],
    { stdio: 'inherit' },
  );
  if (run.status !== 0) process.exit(run.status ?? 1);
  console.log(`${summary(r)}\nChoose it in the device's module settings; it applies on next restart.`);
}

try {
  if (command === 'gallery') await gallery(args.includes('--no-open'));
  else if (command === 'pack-json') {
    // internal: used by the gallery to pack in a fresh process
    const r = await packScript(src);
    process.stdout.write(JSON.stringify({ ...r, bytes: Buffer.from(r.bytes).toString('base64') }));
  } else if (command === 'dev') await dev(src);
  else if (command === 'put') put(await packScript(src));
  else {
    const r = await packScript(src);
    if (command === 'pack') {
      const file = path.resolve(out ?? outDefault(src, '.dkbs'));
      write(file, r.bytes);
      console.log(`${file}\n${summary(r)}`);
    } else {
      const bytes = r.bytes;
      const dkbs = outDefault(src, '.dkbs');
      write(dkbs, bytes);
      const file = path.resolve(out ?? outDefault(src, '.png'));
      write(file, sheetPng(bytes));
      console.log(`${file}\n${summary(r)}`);
    }
  }
} catch (e) {
  fail(e.message);
}
