// `StoreOps` over a `DkSession`: the same code reads the store of the emulator and of a device.
import { DkError, READ_MAX, storeSpace, type DkSession } from '@digi/dk-session';
import type { TargetApp } from '../transfer/target';
import { factsOf, unwiredRunOps, type StoreOps, type TargetOps } from './ops';
import { parseDkmHeader, usageOf } from './store';

/** E_PROTO: the runtime does not know the command (an older runtime). Other errors, timeouts included, are not swallowed. */
async function orEmpty<T>(call: () => Promise<T[]>): Promise<T[]> {
  try {
    return await call();
  } catch (e) {
    if (e instanceof DkError && e.status === 'E_PROTO') return [];
    throw e;
  }
}

export function sessionStoreOps(session: () => Promise<DkSession>): StoreOps {
  return {
    async snapshot() {
      const s = await session();
      // one request at a time: SysEx replies come on one stream
      const info = await s.info();
      const entries = await s.list();
      const space = await storeSpace(s, info);
      const modules = await orEmpty(() => s.modules());
      const states = await orEmpty(() => s.loadStates());
      return { facts: factsOf(info), entries, usage: usageOf(entries, space), modules, states };
    },
    async header(path) {
      return parseDkmHeader(await (await session()).header(path));
    },
    async read(path, offset, length) {
      return (await session()).read(path, offset, length);
    },
    async readAll(path, onProgress) {
      const s = await session();
      const first = await s.read(path, 0, READ_MAX);
      const size = first.size;
      const out = new Uint8Array(size);
      let done = Math.min(first.bytes.length, size);
      out.set(first.bytes.subarray(0, done), 0);
      onProgress?.(done, size);
      while (done < size) {
        const r = await s.read(path, done, Math.min(READ_MAX, size - done));
        if (r.bytes.length === 0) throw new Error(`${path}: the device returned no data at ${done} of ${size} bytes`);
        const part = r.bytes.subarray(0, size - done);
        out.set(part, done);
        done += part.length;
        onProgress?.(done, size);
      }
      return out;
    },
    async verify(path) {
      return (await session()).verify(path);
    },
  };
}

export function targetOps(app: Pick<TargetApp, 'target' | 'session'>): TargetOps {
  return { target: app.target(), store: sessionStoreOps(() => app.session()), run: unwiredRunOps() };
}
