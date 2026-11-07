// The fit rule: does a module, from the needs inferred from its contents, fit the capacity of a device (docs/plans/what-fits.md)?
// The rule exists once, in Rust (modules/core/runtime/fit: the code the DIGIKIT menu runs). This file only calls it through `pk_fit` of
// patchkit.wasm (dkneeds.ts) and gives the types. The reader is set once, where patchkit.wasm is loaded (`setFitReader`), so the callers stay
// synchronous. Written without TS-only runtime syntax so node can import it.

/** CAPACITY as the session returns it: kind name -> limit, used, largest free piece. */
export type Caps = Record<string, { limit: number; used: number; largest: number }>;

/** The needs of a .dkm as `dkneeds` infers them: `file_size` and these fields (`Needs::fields` in packages/dkneeds). Missing = 0. */
export type Needs = { file_size?: number } & Partial<
  Record<
    | 'ARENA'
    | 'ARENA_XFER'
    | 'DSP_AREA'
    | 'DSP_ALIGN'
    | 'MACHINE_ROWS'
    | 'VOICE_ROWS'
    | 'PARAM_ROWS'
    | 'PAGE_ROWS'
    | 'FX_SLOTS',
    number
  >
>;

/**
 * The first short resource: its CAPACITY kind name, what the module needs and what is free (bytes, or a count), and its texts. `text` and `brief`
 * are the ones the device shows (`DSP 400K > 254K FREE`, `DSP 400K>254K`); `describe` is `DSP memory 400 KiB needed, 254 KiB free` and
 * `describe_by` is `DSP memory 146 KiB short`.
 */
export type Short = {
  resource: string;
  need: number;
  free: number;
  text: string;
  brief: string;
  describe: string;
  describe_by: string;
};

/** A request to the rule. With `empty` the capacity first becomes the one of a runtime that has loaded nothing (the next boot). */
export type FitRequest = { caps: Caps; needs: Needs[]; empty?: boolean };

/**
 * The answer: `each` is per need (null: it fits the capacity alone), `set` is null when the needs fit in turn (the sum rule: it does not model
 * fragmentation, so it is conservative), else the index of the first that does not and what it is short of. `line` is the load line
 * (`DSP 52% CPU 1%`) of the capacity once the needs that fit are taken.
 */
export type FitAnswer = { each: (Short | null)[]; set: (Short & { index: number }) | null; line: string };

export type FitReader = { fit(req: FitRequest): FitAnswer };

let reader: FitReader | undefined;

/** Set the reader: the `DkNeeds` that holds patchkit.wasm (dkneeds.ts). */
export function setFitReader(r: FitReader): void {
  reader = r;
}

/** Ask the rule. */
export function fit(req: FitRequest): FitAnswer {
  if (!reader) throw new Error('the fit rule is not loaded (patchkit.wasm)');
  return reader.fit(req);
}

/** null when the module fits `caps`, else the first short resource. */
export function fitsNow(n: Needs, caps: Caps, empty = false): Short | null {
  return fit({ caps, needs: [n], empty }).each[0] ?? null;
}

/** null when all the modules fit in turn into the empty runtime of the next boot (`empty`) or into `caps`, else the first that does not. */
export function fitsSet(needs: Needs[], caps: Caps, empty = false): (Short & { index: number }) | null {
  return fit({ caps, needs, empty }).set;
}
