// Shared Nx remote cache server (Nx self-hosted OpenAPI cache: GET/PUT /v1/cache/{hash}).
// Started on demand by tools/nx/remote-cache.mjs; listens on 127.0.0.1 only.
// Dependencies are in this directory (package.json), installed with npm ci.
// Usage: node tools/nx/cache-server/server.mjs --config FILE
// Config JSON: { port, store, ttlDays, token, readOnlyToken }
import { readFileSync } from 'node:fs';
import { Cacheiro } from '@renatorodrigues/cacheiro';
import { FileSystemStore } from '@renatorodrigues/cacheiro-store-fs';

const flag = process.argv.indexOf('--config');
const configPath = flag >= 0 ? process.argv[flag + 1] : undefined;
if (!configPath) {
  console.error('server.mjs: missing --config FILE');
  process.exit(2);
}
const cfg = JSON.parse(readFileSync(configPath, 'utf8'));

const store = new FileSystemStore({
  cacheDirectory: cfg.store,
  ttlDays: cfg.ttlDays,
  sweepIntervalHours: 24,
});
await store.mount();
const server = new Cacheiro(store, {
  server: {
    port: cfg.port,
    host: '127.0.0.1',
    banner: false,
    infobox: false,
    logFormat: 'json',
  },
  auth: { token: cfg.token, readOnlyToken: cfg.readOnlyToken },
});
await server.start();
await server.listen();
