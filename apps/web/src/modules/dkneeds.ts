// What a .dkm takes of the device (arena, DSP area, rows), from `pk_needs` of patchkit.wasm: the Rust code (packages/dkneeds) that infers it from
// the file's own contents and that the DIGIKIT menu runs on the card's files, and the fit rule of those needs (`pk_fit`, fit.ts). There is no
// TypeScript copy of the arithmetic.
// Written without TS-only runtime syntax so node can import it for the tests.
import type { FitAnswer, FitRequest, Needs } from './fit.ts';

interface Exports {
  memory: WebAssembly.Memory;
  pk_alloc(n: number): number;
  pk_free(p: number, n: number): void;
  pk_needs(p: number, n: number): number;
  pk_fit(p: number, n: number): number;
  pk_plan_ptr(): number;
  pk_plan_len(): number;
  pk_err_ptr(): number;
  pk_err_len(): number;
}

/** The needs reader: one instance of patchkit.wasm (no bundle is needed: the function reads the bytes it is given and nothing else). */
export class DkNeeds {
  private x: Exports;
  private constructor(x: Exports) {
    this.x = x;
  }
  static async create(wasm: BufferSource): Promise<DkNeeds> {
    const { instance } = await WebAssembly.instantiate(wasm, {});
    return new DkNeeds(instance.exports as unknown as Exports);
  }

  private text(ptr: number, len: number): string {
    return new TextDecoder().decode(new Uint8Array(this.x.memory.buffer.slice(ptr, ptr + len)));
  }

  /** The needs of the file `bytes` (`file_size` and the fields of `Needs`), or the reason it has none: the loader would refuse it. */
  read(bytes: Uint8Array): { needs: Needs } | { error: string } {
    const p = this.x.pk_alloc(bytes.length);
    try {
      new Uint8Array(this.x.memory.buffer, p, bytes.length).set(bytes);
      if (this.x.pk_needs(p, bytes.length) !== 0) return { error: this.text(this.x.pk_err_ptr(), this.x.pk_err_len()) };
      return { needs: JSON.parse(this.text(this.x.pk_plan_ptr(), this.x.pk_plan_len())) as Needs };
    } finally {
      this.x.pk_free(p, bytes.length);
    }
  }

  /** The fit rule (`pk_fit`: modules/core/runtime/fit) on a request; throws with the reason when the request is bad. */
  fit(req: FitRequest): FitAnswer {
    const bytes = new TextEncoder().encode(JSON.stringify(req));
    const p = this.x.pk_alloc(bytes.length);
    try {
      new Uint8Array(this.x.memory.buffer, p, bytes.length).set(bytes);
      if (this.x.pk_fit(p, bytes.length) !== 0) throw new Error(this.text(this.x.pk_err_ptr(), this.x.pk_err_len()));
      return JSON.parse(this.text(this.x.pk_plan_ptr(), this.x.pk_plan_len())) as FitAnswer;
    } finally {
      this.x.pk_free(p, bytes.length);
    }
  }

  /** The needs of the file, or null when it has none. */
  of(bytes: Uint8Array): Needs | null {
    const r = this.read(bytes);
    return 'needs' in r ? r.needs : null;
  }
}
