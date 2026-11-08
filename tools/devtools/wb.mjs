#!/usr/bin/env node
// Workbench actions from the command line, through the workbench's own code: the dk-session TypeScript (prepareUpload, openSamples, put,
// verify) over the dkmidi Rust MIDI port (packages/dkmidi, the code the desktop app uses), here as a child process on stdin and stdout.
//
//     node tools/devtools/wb.mjs [--port SUB] [--bin PATH] ports
//     node tools/devtools/wb.mjs [--port SUB] [--bin PATH] info
//     node tools/devtools/wb.mjs [--port SUB] [--bin PATH] samples ls [FOLDER]
//     node tools/devtools/wb.mjs [--port SUB] [--bin PATH] samples put FILE... [--folder F]
//     node tools/devtools/wb.mjs [--port SUB] [--bin PATH] store ls [PREFIX]
//     node tools/devtools/wb.mjs [--port SUB] [--bin PATH] store put LOCAL REMOTE
//     node tools/devtools/wb.mjs [--port SUB] [--bin PATH] store rm REMOTE
//
// `store` works on the card's store by path (for example data/tools/bootscreen/x.dkbs); `store put` verifies the file after the upload.
// --port: a part of the MIDI port name (default "Digitakt"). --bin: the dkmidi binary (default $DKMIDI_BIN, else the nogen target dir;
// `mise run dkmidi:build` builds it). `samples put` needs apps/web/public/dksample.wasm (`mise run wasm:dksample`).
import { spawn, spawnSync } from 'node:child_process';
import { existsSync, readFileSync, statSync } from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import { DkSession } from '../../packages/dk-session/src/index.ts';
import { ProcessTransport } from '../../packages/dk-session/src/process-transport.ts';
import { Sample, prepareUpload } from '../../packages/dk-session/src/dksample.ts';
import { openSamples } from '../../packages/dk-session/src/samples.ts';

const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '../..');
// the patchkit rule (tools/emulator/native_target.py): a worktree builds into its own target dir; no generated core is the "nogen" key
const targetRoot = existsSync(path.join(root, '.git', 'HEAD'))
  ? path.join(root, 'out/native/target')
  : path.join(root, 'out/native/target-wt', path.basename(root));
const usage =
  'usage: node tools/devtools/wb.mjs [--port SUB] [--bin PATH] ports | info | samples ls [FOLDER] | samples put FILE... [--folder F] | store ls [PREFIX] | store put LOCAL REMOTE | store rm REMOTE';

// options anywhere on the line; the rest are the command words
const args = process.argv.slice(2);
const opts = {
  port: 'Digitakt',
  bin: process.env.DKMIDI_BIN || path.join(targetRoot, 'nogen/release/dkmidi'),
  folder: '',
};
const words = [];
for (let i = 0; i < args.length; i++) {
  const a = args[i];
  if (a === '--port' || a === '--bin' || a === '--folder') {
    if (i + 1 >= args.length) fail(`${a} needs a value`);
    opts[a.slice(2)] = args[++i];
  } else words.push(a);
}

function fail(msg, code = 2) {
  console.error(`wb: ${msg}`);
  process.exit(code);
}

if (!existsSync(opts.bin)) fail(`no dkmidi binary at ${opts.bin} (run: mise run dkmidi:build)`);

/** Start the bridge on the port and open a session on it. */
function connect() {
  const child = spawn(opts.bin, ['open', opts.port], { stdio: ['pipe', 'pipe', 'inherit'] });
  const exited = new Promise((resolve) => child.on('exit', resolve));
  child.on('error', (e) => fail(`cannot start ${opts.bin}: ${e.message}`, 1));
  child.stdin.on('error', () => {});
  const transport = new ProcessTransport(
    (b) => child.stdin.write(b),
    (cb) => child.stdout.on('data', (d) => cb(new Uint8Array(d))),
  );
  const session = new DkSession(transport, { timeoutMs: 60000 });
  return {
    session,
    async close() {
      child.stdin.end();
      await exited;
    },
  };
}

async function withSession(run) {
  const link = connect();
  try {
    return await run(link.session);
  } finally {
    await link.close();
  }
}

async function loadSample() {
  const wasm = path.join(root, 'apps/web/public/dksample.wasm');
  if (!existsSync(wasm)) fail('apps/web/public/dksample.wasm is missing (run: mise run wasm:dksample)', 1);
  return Sample.create(Uint8Array.from(readFileSync(wasm)));
}

const [cmd, sub, ...rest] = words;
let status = 0;
if (cmd === 'ports') {
  const r = spawnSync(opts.bin, ['ports'], { stdio: 'inherit' });
  status = r.status ?? 1;
} else if (cmd === 'info') {
  await withSession(async (session) => {
    console.log(JSON.stringify(await session.info(), null, 2));
  });
} else if (cmd === 'samples' && sub === 'ls') {
  const sample = await loadSample();
  await withSession(async (session) => {
    const backend = openSamples('stock-rpc', { session, hashOf: (d) => sample.hash(d) });
    for (const e of await backend.list(rest[0] ?? ''))
      console.log(`${e.kind === 'folder' ? `${e.name}/` : e.name}\t${e.size}`);
  });
} else if (cmd === 'samples' && sub === 'put') {
  if (rest.length === 0) fail(usage);
  const sample = await loadSample();
  await withSession(async (session) => {
    const backend = openSamples('stock-rpc', { session, hashOf: (d) => sample.hash(d) });
    if (opts.folder) await backend.mkdir(opts.folder);
    for (const file of rest) {
      try {
        if (!statSync(file).isFile()) throw new Error('not a file');
        const prepared = prepareUpload(sample, path.basename(file), Uint8Array.from(readFileSync(file)));
        await backend.put(opts.folder, prepared);
        const v = await backend.verify(opts.folder, prepared);
        console.log(
          `${v.ok ? 'ok  ' : 'FAIL'} +Drive/${opts.folder ? `${opts.folder}/` : ''}${prepared.name}  ${v.message}`,
        );
        if (!v.ok) status = 1;
      } catch (e) {
        console.error(`FAIL ${file}: ${e instanceof Error ? e.message : e}`);
        status = 1;
      }
    }
  });
} else if (cmd === 'store' && sub === 'ls') {
  await withSession(async (session) => {
    for (const e of await session.list(rest[0] || undefined)) console.log(`${e.path}\t${e.size}`);
  });
} else if (cmd === 'store' && sub === 'put') {
  if (rest.length !== 2) fail(usage);
  const [local, remote] = rest;
  await withSession(async (session) => {
    try {
      if (!statSync(local).isFile()) throw new Error('not a file');
      const bytes = Uint8Array.from(readFileSync(local));
      await session.put(remote, bytes);
      const ok = await session.verifyFile(remote, bytes);
      console.log(`${ok ? 'ok  ' : 'FAIL'} ${remote}  ${bytes.length} bytes`);
      if (!ok) status = 1;
    } catch (e) {
      console.error(`FAIL ${local}: ${e instanceof Error ? e.message : e}`);
      status = 1;
    }
  });
} else if (cmd === 'store' && sub === 'rm') {
  if (rest.length !== 1) fail(usage);
  await withSession(async (session) => {
    await session.delete(rest[0]);
    console.log(`removed ${rest[0]}`);
  });
} else fail(usage);
process.exit(status);
