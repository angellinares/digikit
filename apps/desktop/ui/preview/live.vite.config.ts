// The live UI preview: the same desktop pages as the plain preview (vite.config.ts), over a RUNNING emulator instead of the fake device. The
// Tauri `invoke` is replaced by live-tauri.ts, whose `emu_call` goes through this server to the daemon's control socket, the way the app's
// Rust side does; the library folder and patchkit.wasm come from the repository. Nothing is mocked but the file dialogs.
//   DIGI_LIVE_SOCKET=SOCKET pnpm --filter @digi/desktop exec vite --config ui/preview/live.vite.config.ts --port 5298
// Open `/panel.html` for only the interactive faceplate, or `/live.html` for the workbench preview.
// (SOCKET: `uv run python -m tools.devtools.emu_live`). `/__stats` answers how many control calls each method got since the last `/__stats?reset=1`.
import fs from 'node:fs';
import net from 'node:net';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import { defineConfig, type Plugin } from 'vite';
import solid from 'vite-plugin-solid';

const here = (p: string) => fileURLToPath(new URL(p, import.meta.url));
const ROOT = here('../../../..');

function controller(socketPath: string): Plugin {
  const counts: Record<string, number> = {};
  let sock: net.Socket | undefined;
  let buf = '';
  let nextId = 1;
  let hello: Promise<unknown> | undefined;
  const waiting = new Map<number, { ok: (v: unknown) => void; fail: (e: Error) => void }>();
  const open = () => {
    if (sock && !sock.destroyed) return sock;
    const s = net.connect(socketPath);
    sock = s;
    buf = '';
    const disconnect = (error: Error) => {
      if (sock !== s) return;
      sock = undefined;
      hello = undefined;
      buf = '';
      for (const pending of waiting.values()) pending.fail(error);
      waiting.clear();
      s.destroy();
    };
    s.on('close', () => disconnect(new Error('emulator socket closed')));
    s.on('error', disconnect);
    s.on('data', (d) => {
      buf += d;
      for (let i = buf.indexOf('\n'); i >= 0; i = buf.indexOf('\n')) {
        const m = JSON.parse(buf.slice(0, i));
        buf = buf.slice(i + 1);
        const w = m.id !== undefined ? waiting.get(m.id) : undefined;
        if (!w) continue;
        waiting.delete(m.id);
        if (m.error) w.fail(new Error(m.error.message));
        else w.ok(m.result);
      }
    });
    return s;
  };
  const call = (method: string, params: unknown) =>
    new Promise<unknown>((ok, fail) => {
      const id = nextId++;
      const socket = open();
      const timer = setTimeout(() => socket.destroy(new Error('emulator socket timed out')), 10_000);
      waiting.set(id, {
        ok: (value) => {
          clearTimeout(timer);
          ok(value);
        },
        fail: (error) => {
          clearTimeout(timer);
          fail(error);
        },
      });
      socket.write(JSON.stringify({ jsonrpc: '2.0', id, method, params }) + '\n');
    });
  return {
    name: 'digi-live-controller',
    configureServer(server) {
      server.middlewares.use(async (req, res, next) => {
        const url = new URL(req.url ?? '/', 'http://x');
        const send = (code: number, body: string | Buffer, type = 'application/json') => {
          res.writeHead(code, { 'content-type': type });
          res.end(body);
        };
        try {
          if (url.pathname === '/__emu' && req.method === 'POST') {
            let raw = '';
            for await (const c of req) raw += c;
            const { method, params } = JSON.parse(raw);
            counts[method] = (counts[method] ?? 0) + 1;
            hello ??= call('hello', { min_schema: 1 });
            await hello;
            return send(200, JSON.stringify((await call(method, params ?? {})) ?? null));
          }
          if (url.pathname === '/__stats') {
            const out = JSON.stringify(counts);
            if (url.searchParams.get('reset')) for (const k of Object.keys(counts)) delete counts[k];
            return send(200, out);
          }
          const device = url.searchParams.get('device') ?? '';
          if (url.pathname === '/__lib/scan' && /^[a-z0-9]+$/.test(device)) {
            const base = path.join(ROOT, 'out/modules', device);
            const files: { path: string; size: number }[] = [];
            const walk = (d: string) => {
              for (const e of fs.existsSync(d) ? fs.readdirSync(d, { withFileTypes: true }) : []) {
                const p = path.join(d, e.name);
                if (e.isDirectory()) walk(p);
                else if (e.name.endsWith('.dkm'))
                  files.push({ path: path.relative(base, p), size: fs.statSync(p).size });
              }
            };
            walk(base);
            return send(200, JSON.stringify(files.sort((a, b) => a.path.localeCompare(b.path))));
          }
          if (url.pathname === '/__lib/read' && /^[a-z0-9]+$/.test(device)) {
            const rel = url.searchParams.get('path') ?? '';
            if (rel.split('/').some((p) => !p || p === '.' || p === '..') || !rel.endsWith('.dkm'))
              return send(400, 'bad path');
            return send(200, fs.readFileSync(path.join(ROOT, 'out/modules', device, rel)), 'application/octet-stream');
          }
          if (url.pathname === '/__wasm/patchkit')
            return send(200, fs.readFileSync(path.join(ROOT, 'apps/web/public/patchkit.wasm')), 'application/wasm');
        } catch (e) {
          return send(500, String(e));
        }
        next();
      });
    },
  };
}

export default defineConfig({
  root: here('.'),
  base: './',
  plugins: [solid(), controller(process.env.DIGI_LIVE_SOCKET ?? '')],
  resolve: {
    alias: [
      { find: '@web', replacement: here('../../../web/src') },
      { find: '@tauri-apps/api/core', replacement: here('./live-tauri.ts') },
      // the file dialogs and the samples folder have no live stand-in: the plain preview's fakes stay for the Samples mode
      { find: /^\.\/samples-backend$/, replacement: here('./samples-backend.mock.ts') },
    ],
    dedupe: ['solid-js'],
  },
  server: { host: '127.0.0.1', port: 5298, strictPort: true, fs: { allow: [here('../../../..')] } },
});
