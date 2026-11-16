// Binding a service file (`.dks`) to the user's stock firmware: `pk_bind` of patchkit.wasm, the code `patchkit bind` runs (packages/patchkit/src/bind.rs).
// The bound file holds copied firmware bytes: it stays in memory and on the user's card and is never shipped or committed.
// Written without TS-only runtime syntax so node can import it for the tests.
import type { ServiceBinder } from './services.ts';

interface Exports {
  memory: WebAssembly.Memory;
  pk_alloc(n: number): number;
  pk_free(p: number, n: number): void;
  pk_bind(a: number, an: number, b: number, bn: number, c: number, cn: number): number;
  pk_out_ptr(): number;
  pk_out_len(): number;
  pk_plan_ptr(): number;
  pk_plan_len(): number;
  pk_err_ptr(): number;
  pk_err_len(): number;
}

/** One bound hook of the report (`bind::BoundHook` as `bind_json` writes it). */
export interface BoundHookReport {
  hook: string;
  site_addr: number;
  [key: string]: unknown;
}

/** The binder: one instance of patchkit.wasm and the bind bundle (`ports` and `fw`, no modules and no firmware). */
export class DkBind {
  private x: Exports;
  private bundle: Uint8Array;
  private constructor(x: Exports, bundle: Uint8Array) {
    this.x = x;
    this.bundle = bundle;
  }
  static async create(wasm: BufferSource, bundleJson: string | Uint8Array): Promise<DkBind> {
    const { instance } = await WebAssembly.instantiate(wasm, {});
    const bundle = typeof bundleJson === 'string' ? new TextEncoder().encode(bundleJson) : bundleJson;
    return new DkBind(instance.exports as unknown as Exports, bundle);
  }

  private put(bytes: Uint8Array): [number, number] {
    const p = this.x.pk_alloc(bytes.length);
    new Uint8Array(this.x.memory.buffer, p, bytes.length).set(bytes);
    return [p, bytes.length];
  }
  private text(ptr: number, len: number): string {
    return new TextDecoder().decode(new Uint8Array(this.x.memory.buffer.slice(ptr, ptr + len)));
  }

  /** The bound file and the hooks it bound; throws patchkit's reason (no stock image it knows, no service, a hook the image lacks). */
  bind(dks: Uint8Array, stock: Uint8Array): { file: Uint8Array; hooks: BoundHookReport[] } {
    const m = [this.put(dks), this.put(stock), this.put(this.bundle)];
    try {
      if (this.x.pk_bind(m[0]![0], m[0]![1], m[1]![0], m[1]![1], m[2]![0], m[2]![1]) !== 0)
        throw new Error(this.text(this.x.pk_err_ptr(), this.x.pk_err_len()));
      const file = new Uint8Array(
        this.x.memory.buffer.slice(this.x.pk_out_ptr(), this.x.pk_out_ptr() + this.x.pk_out_len()),
      );
      return { file, hooks: JSON.parse(this.text(this.x.pk_plan_ptr(), this.x.pk_plan_len())) as BoundHookReport[] };
    } finally {
      for (const [p, n] of m) this.x.pk_free(p!, n!);
    }
  }
}

/** What the binder needs of the user's stock firmware: the bytes behind the hash `firmwareHash` answered (`StockStore.bytesOf`). */
export interface StockBytes {
  bytesOf(hash: string): Promise<Uint8Array | undefined>;
}

/**
 * The `ServiceBinder` over a binder instance made on first use (a failed load is tried again at the next use) and the stock firmware of the
 * user. `firmwareHash` is the key of the bound file's cache: the same stock image and the same unbound file always bind to the same bytes.
 */
export function createServiceBinder(make: () => Promise<DkBind>, stock: StockBytes): ServiceBinder {
  let binder: Promise<DkBind> | undefined;
  return {
    async bind(dks, firmwareHash) {
      const loading = (binder ??= make().catch((e) => {
        binder = undefined;
        throw e;
      }));
      // a service that hooks no firmware site binds without the image (patchkit returns it as built), so a missing image is only an error when the bind fails
      const fw = await stock.bytesOf(firmwareHash);
      const b = await loading;
      try {
        return b.bind(dks, fw ?? new Uint8Array()).file;
      } catch (e) {
        if (fw) throw e;
        throw new Error(
          `the stock firmware is not kept here: choose it on the Firmware page (${e instanceof Error ? e.message : String(e)})`,
        );
      }
    },
  };
}
