import fs from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import { defineConfig } from 'astro/config';
import solid from '@astrojs/solid-js';
import { SHOW_EMULATOR } from './site.config.mjs';

// Dev-only (`apply: 'serve'`): these files are firmware-derived and are never part of a build.
const repoRoot = fileURLToPath(new URL('../..', import.meta.url));
const readyDir =
  process.env.DIGI_EMU_READY_DIR ||
  JSON.parse(fs.readFileSync(path.join(repoRoot, 'tools', 'emulator', 'ready_inputs.json'), 'utf8')).ready_dir;
const localDir = path.resolve(repoRoot, readyDir);
const localFiles = {
  'dsp-image': process.env.DIGI_EMU_DSP_IMAGE || path.join(localDir, 'digi-audio-dn2-image.bin'),
  'dsp-state': process.env.DIGI_EMU_DSP_STATE || path.join(localDir, 'digi-audio-m5.snap.dsp'),
  'cf-snapshot': process.env.DIGI_EMU_CF_SNAPSHOT || path.join(localDir, 'digi-audio-m5.snap'),
  // Used only by the headless bench (`?bench=1`, tools/devtools/browser_bench.mjs): the page cannot pick a file there.
  firmware: process.env.DIGI_EMU_SYX || path.join(repoRoot, 'firmware', 'Digitone_II_OS1.11.syx'),
};
const coupledCore =
  process.env.DIGI_EMU_SHARC_CORE || fileURLToPath(new URL('./.local/emulator-core-sharc.wasm', import.meta.url));
const loopback = new Set(['127.0.0.1', '::1', '::ffff:127.0.0.1']);

// Answers GET (body) and HEAD (headers only) for one file; false when the file is missing.
function sendFile(req, res, file, contentType) {
  let stat;
  try {
    stat = fs.statSync(file);
  } catch {
    return false;
  }
  if (!stat.isFile()) return false;
  res.setHeader('Content-Type', contentType);
  res.setHeader('Cache-Control', 'no-store');
  res.setHeader('Content-Length', String(stat.size));
  if (req.method === 'HEAD') {
    res.end();
    return true;
  }
  fs.createReadStream(file)
    .on('error', () => res.destroy())
    .pipe(res);
  return true;
}

const localInputs = {
  name: 'digi-local-inputs',
  apply: 'serve',
  configureServer(server) {
    // Cross-origin isolation for the dev page, so SharedArrayBuffer works and the
    // coupled core can run its ColdFire and DSP in two workers (runtime.ts `splitAvailable`).
    server.middlewares.use((_req, res, next) => {
      res.setHeader('Cross-Origin-Opener-Policy', 'same-origin');
      res.setHeader('Cross-Origin-Embedder-Policy', 'require-corp');
      next();
    });
    server.middlewares.use('/__digi-local', (req, res) => {
      if (!loopback.has(req.socket.remoteAddress ?? '')) {
        res.statusCode = 403;
        res.end();
        return;
      }
      if (req.method !== 'GET' && req.method !== 'HEAD') {
        res.statusCode = 405;
        res.end();
        return;
      }
      const key = (req.url ?? '').split('?')[0].replace(/^\/+/, '');
      res.setHeader('Cache-Control', 'no-store');
      if (key === 'manifest') {
        const body = JSON.stringify({
          dspImage: fs.existsSync(localFiles['dsp-image']),
          dspState: fs.existsSync(localFiles['dsp-state']),
          cfSnapshot: fs.existsSync(localFiles['cf-snapshot']),
          firmware: fs.existsSync(localFiles.firmware),
        });
        res.setHeader('Content-Type', 'application/json');
        res.setHeader('Content-Length', String(Buffer.byteLength(body)));
        res.end(req.method === 'HEAD' ? undefined : body);
        return;
      }
      const file = Object.hasOwn(localFiles, key) ? localFiles[key] : undefined;
      if (!file || !sendFile(req, res, file, 'application/octet-stream')) {
        res.statusCode = 404;
        res.end();
      }
    });
    // The coupled core is firmware-derived: served from apps/web/.local, never from public/.
    server.middlewares.use((req, res, next) => {
      if (!(req.url ?? '').split('?')[0].endsWith('/emulator-core-sharc.wasm')) {
        next();
        return;
      }
      if (!loopback.has(req.socket.remoteAddress ?? '')) {
        res.statusCode = 403;
        res.end();
        return;
      }
      if (req.method !== 'GET' && req.method !== 'HEAD') {
        res.statusCode = 405;
        res.end();
        return;
      }
      if (!sendFile(req, res, coupledCore, 'application/wasm')) {
        res.statusCode = 404;
        res.end();
      }
    });
  },
};

// Fails the build if a firmware-derived file reached the output.
const buildGuard = {
  name: 'digi-build-guard',
  hooks: {
    'astro:build:done': ({ dir }) => {
      if (!SHOW_EMULATOR) {
        // They stay in public/ for the dev server's emulator page.
        for (const name of ['emulator-core.wasm', 'coi-serviceworker.js', 'pcm-worklet.js'])
          fs.rmSync(path.join(fileURLToPath(dir), name), { force: true });
      }
      const walk = (current) => {
        for (const entry of fs.readdirSync(current, { withFileTypes: true })) {
          const full = path.join(current, entry.name);
          if (entry.isDirectory()) {
            walk(full);
            continue;
          }
          if (!entry.isFile()) continue;
          // Ports and modules (hashes and our own code) are allowed; a firmware image or snapshot never is.
          const bad =
            /^emulator-core-sharc.*\.wasm$/.test(entry.name) ||
            /\.(syx|snap|mstate)$/i.test(entry.name) ||
            (entry.name !== 'emulator-core.wasm' && fs.statSync(full).size > 5 * 1024 * 1024);
          if (bad) throw new Error(`firmware-derived file in build output: ${full}`);
        }
      };
      walk(fileURLToPath(dir));
    },
  },
};

// A linked worktree shares node_modules with the main tree; the bench tool lists that tree here so Vite may serve it.
const extraFsAllow = (process.env.DIGI_VITE_FS_ALLOW || '').split(path.delimiter).filter(Boolean);
const server = extraFsAllow.length ? { fs: { allow: [repoRoot, ...extraFsAllow] } } : undefined;

// The emulator page is served by the dev server and, when SHOW_EMULATOR is set, by the build.
const emulatorPage = {
  name: 'digi-emulator-page',
  hooks: {
    'astro:config:setup': ({ command, injectRoute }) => {
      if (command === 'dev' || SHOW_EMULATOR)
        injectRoute({ pattern: '/emulator', entrypoint: './src/emulator-page/emulator.astro' });
    },
  },
};

export default defineConfig({
  // The Pages workflow sets these two; the dev server and local builds stay at the root.
  site: process.env.DIGI_SITE_ORIGIN || undefined,
  base: process.env.DIGI_SITE_BASE || undefined,
  devToolbar: { enabled: false },
  integrations: [solid(), emulatorPage, buildGuard],
  vite: { plugins: [localInputs], ...(server ? { server } : {}) },
});
