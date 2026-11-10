import assert from 'node:assert/strict';
import fs from 'node:fs';
import net from 'node:net';
import os from 'node:os';
import path from 'node:path';
import { test } from 'vitest';
import { connect, RpcError, spawnDaemon, type EmuEvent } from '../src/index.ts';

type Handler = (method: string, params: any, reply: (msg: object) => void) => void;

async function fakeServer(handler: Handler) {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'digi-ts-'));
  const sock = path.join(dir, 't.sock');
  const server = net.createServer((conn) => {
    let buf = '';
    conn.setEncoding('utf8');
    conn.on('data', (chunk: string) => {
      buf += chunk;
      let nl: number;
      while ((nl = buf.indexOf('\n')) >= 0) {
        const req = JSON.parse(buf.slice(0, nl));
        buf = buf.slice(nl + 1);
        const reply = (msg: object) => conn.write(`${JSON.stringify(msg)}\n`);
        if (req.method === 'hello') reply({ jsonrpc: '2.0', id: req.id, result: { schema_version: 1 } });
        else handler(req.method, req.params, (m) => reply({ jsonrpc: '2.0', id: req.id, ...m }));
      }
    });
  });
  await new Promise<void>((r) => server.listen(sock, r));
  return {
    sock,
    close: () => {
      server.close();
      fs.rmSync(dir, { recursive: true, force: true });
    },
  };
}

test('call returns the result and maps errors', async () => {
  const srv = await fakeServer((method, params, reply) => {
    if (method === 'step') reply({ result: { icount: params.instructions } });
    else reply({ error: { code: 6, message: 'guest fault', data: { pc: 1 } } });
  });
  const c = await connect(srv.sock);
  assert.deepEqual(await c.step(5), { icount: 5 });
  await assert.rejects(c.status(), (e: unknown) => {
    assert.ok(e instanceof RpcError);
    assert.equal(e.code, 6);
    assert.deepEqual(e.data, { pc: 1 });
    return true;
  });
  c.close();
  srv.close();
});

test('frameHash asks for hash without data', async () => {
  let seen: unknown;
  const srv = await fakeServer((_m, params, reply) => {
    seen = params;
    reply({ result: { revision: 1, sha256: 'ab' } });
  });
  const c = await connect(srv.sock);
  assert.equal(await c.frameHash(), 'ab');
  assert.deepEqual(seen, { hash: true, data: false });
  c.close();
  srv.close();
});

test('event notifications are buffered, bounded, and iterable', async () => {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'digi-ts-'));
  const sock = path.join(dir, 'e.sock');
  const server = net.createServer((conn) => {
    conn.setEncoding('utf8');
    conn.on('data', (chunk: string) => {
      for (const line of chunk.split('\n').filter(Boolean)) {
        const req = JSON.parse(line);
        const result = req.method === 'hello' ? { schema_version: 1 } : {};
        if (req.method === 'events.subscribe') {
          for (let i = 0; i < 5; i++) {
            conn.write(`${JSON.stringify({ jsonrpc: '2.0', method: 'event', params: { kind: 'mode', i } })}\n`);
          }
          conn.write(`${JSON.stringify({ jsonrpc: '2.0', method: 'event', params: { kind: 'lagged', n: 2 } })}\n`);
        }
        conn.write(`${JSON.stringify({ jsonrpc: '2.0', id: req.id, result })}\n`);
      }
    });
  });
  await new Promise<void>((r) => server.listen(sock, r));
  const c = await connect(sock, { maxBufferedEvents: 3 });
  await c.eventsSubscribe();
  const got: EmuEvent[] = [];
  const abort = new AbortController();
  for await (const ev of c.events(abort.signal)) {
    got.push(ev);
    if (got.length === 3) abort.abort();
  }
  assert.equal(c.dropped, 3);
  assert.deepEqual(
    got.map((e) => e.kind),
    ['mode', 'mode', 'lagged'],
  );
  c.close();
  server.close();
  fs.rmSync(dir, { recursive: true, force: true });
});

test('pending calls reject when the server closes', async () => {
  const srv = await fakeServer(() => {});
  const c = await connect(srv.sock);
  const p = c.status();
  srv.close();
  c.close();
  await assert.rejects(p);
});

test('status carries the typed CPU load', async () => {
  const load = {
    window_s: 1.2,
    cf: { model_pct: 62.1, real_pct: 40.2, real_high_pct: 57.0 },
    dsp: { busy_per_block: 334529, model_pct: 50.2, real_pct: 57.1, real_high_pct: 66.5 },
  };
  const srv = await fakeServer((_m, _p, reply) => reply({ result: { cf: {}, heartbeat: {}, load } }));
  const c = await connect(srv.sock);
  const status = await c.status();
  assert.equal(status.load?.cf.real_pct, 40.2);
  assert.equal(status.load?.dsp?.real_high_pct, 66.5);
  c.close();
  srv.close();
});

test('abort closes a connection whose hello handshake never replies', { timeout: 1000 }, async () => {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'digi-abort-'));
  const sock = path.join(dir, 'hello.sock');
  let accepted!: () => void;
  const connected = new Promise<void>((resolve) => {
    accepted = resolve;
  });
  const server = net.createServer((conn) => {
    conn.on('data', () => accepted());
  });
  await new Promise<void>((resolve) => server.listen(sock, resolve));
  const controller = new AbortController();
  try {
    const pending = connect(sock, { signal: controller.signal });
    const rejected = assert.rejects(pending, { name: 'AbortError' });
    await connected;
    controller.abort();
    await rejected;
    await new Promise<void>((resolve) => server.close(() => resolve()));
  } finally {
    server.close();
    fs.rmSync(dir, { recursive: true, force: true });
  }
});

test('spawnDaemon gives up on a daemon that never opens its socket and stops the child', async () => {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'digi-ts-'));
  const binary = path.join(dir, 'silent-daemon');
  fs.writeFileSync(binary, '#!/bin/sh\nexec sleep 30\n', { mode: 0o755 });
  try {
    await assert.rejects(
      spawnDaemon({ binary, runDir: path.join(dir, 'run'), timeoutMs: 300 }),
      /did not open its socket .* within 300 ms/,
    );
  } finally {
    fs.rmSync(dir, { recursive: true, force: true });
  }
});
