// Preload for `node --import`: logs file reads and process spawns as JSON lines.
// Active only when DIGI_FS_TRACE names a file. Used by tools/devtools/nx_input_audit.py.
import fs from 'node:fs';
import fsp from 'node:fs/promises';
import cp from 'node:child_process';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import { syncBuiltinESMExports } from 'node:module';

const traceFile = process.env.DIGI_FS_TRACE;

if (traceFile) {
  const append = fs.appendFileSync;
  let busy = false;

  const emit = (record) => {
    if (busy) return;
    busy = true;
    try {
      append(traceFile, JSON.stringify(record) + '\n');
    } catch {
      // tracing must never break the traced program
    } finally {
      busy = false;
    }
  };

  const toPath = (p) => {
    try {
      if (typeof p === 'string') return path.resolve(p);
      if (p instanceof URL) return p.protocol === 'file:' ? fileURLToPath(p) : null;
      if (Buffer.isBuffer(p)) return path.resolve(p.toString());
    } catch {
      return null;
    }
    return null;
  };

  const logRead = (p) => {
    const abs = toPath(p);
    if (abs) emit({ read: abs });
  };

  const isReadFlag = (flags) => {
    if (flags === undefined || flags === null) return true;
    if (typeof flags === 'number') return (flags & 3) === 0;
    return typeof flags === 'string' && flags.startsWith('r');
  };

  const wrap = (obj, name, hook) => {
    const orig = obj[name];
    if (typeof orig !== 'function') return;
    obj[name] = function (...args) {
      hook(args);
      return orig.apply(this, args);
    };
  };

  const firstPath = (args) => logRead(args[0]);
  const simple = [
    'readFile', 'readFileSync', 'createReadStream', 'readdir', 'readdirSync',
    'stat', 'statSync', 'lstat', 'lstatSync', 'existsSync', 'access', 'accessSync',
  ];
  for (const name of simple) wrap(fs, name, firstPath);
  for (const name of ['open', 'openSync']) {
    wrap(fs, name, (args) => {
      if (isReadFlag(args[1])) logRead(args[0]);
    });
  }
  for (const name of ['readFile', 'readdir', 'stat', 'lstat', 'access']) {
    wrap(fsp, name, firstPath);
  }
  wrap(fsp, 'open', (args) => {
    if (isReadFlag(args[1])) logRead(args[0]);
  });
  // fs.promises is the same object as the 'fs/promises' default export.
  if (fs.promises !== fsp) {
    for (const name of ['readFile', 'readdir', 'stat', 'lstat', 'access']) {
      wrap(fs.promises, name, firstPath);
    }
    wrap(fs.promises, 'open', (args) => {
      if (isReadFlag(args[1])) logRead(args[0]);
    });
  }

  const logSpawn = (command, rest) => {
    const args = Array.isArray(rest) ? rest.map(String) : [];
    emit({ spawn: [String(command), ...args], cwd: process.cwd() });
  };
  for (const name of ['spawn', 'spawnSync', 'execFile', 'execFileSync', 'fork']) {
    wrap(cp, name, (args) => logSpawn(args[0], args[1]));
  }
  for (const name of ['exec', 'execSync']) {
    wrap(cp, name, (args) => logSpawn(args[0], []));
  }

  syncBuiltinESMExports();
}
