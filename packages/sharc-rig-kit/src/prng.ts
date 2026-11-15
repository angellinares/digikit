/**
 * The random generator of Python's `random.Random` (Mersenne Twister, the same seeding and the same `random`, `uniform`, `choice`
 * and `randrange`). The conformance cases and the hostile contexts were drawn from it, so the kit keeps their values: the cost a
 * module records (`dsp_cost_voice`) is the worst block over these exact cases.
 */
const N = 624;
const M = 397;

export class PyRandom {
  private readonly mt = new Uint32Array(N);
  private index = N;

  /** `seed` is a non-negative integer below 2^53. */
  constructor(seed: number) {
    this.seedByArray(PyRandom.key(seed));
  }

  private static key(seed: number): number[] {
    if (!Number.isSafeInteger(seed) || seed < 0)
      throw new Error(`PyRandom: seed ${seed} is not a non-negative integer`);
    const key: number[] = [];
    let n = BigInt(seed);
    do {
      key.push(Number(n & 0xffffffffn));
      n >>= 32n;
    } while (n > 0n);
    return key;
  }

  private initGenrand(s: number): void {
    this.mt[0] = s >>> 0;
    for (let i = 1; i < N; i++) {
      const prev = this.mt[i - 1] ^ (this.mt[i - 1] >>> 30);
      this.mt[i] = (Math.imul(1812433253, prev) + i) >>> 0;
    }
    this.index = N;
  }

  private seedByArray(key: number[]): void {
    this.initGenrand(19650218);
    let i = 1;
    let j = 0;
    for (let k = Math.max(N, key.length); k > 0; k--) {
      const prev = this.mt[i - 1] ^ (this.mt[i - 1] >>> 30);
      this.mt[i] = ((this.mt[i] ^ Math.imul(prev, 1664525)) + key[j] + j) >>> 0;
      i++;
      j++;
      if (i >= N) {
        this.mt[0] = this.mt[N - 1];
        i = 1;
      }
      if (j >= key.length) j = 0;
    }
    for (let k = N - 1; k > 0; k--) {
      const prev = this.mt[i - 1] ^ (this.mt[i - 1] >>> 30);
      this.mt[i] = ((this.mt[i] ^ Math.imul(prev, 1566083941)) - i) >>> 0;
      i++;
      if (i >= N) {
        this.mt[0] = this.mt[N - 1];
        i = 1;
      }
    }
    this.mt[0] = 0x80000000;
    this.index = N;
  }

  private genrand(): number {
    if (this.index >= N) {
      const mt = this.mt;
      for (let k = 0; k < N; k++) {
        const y = (mt[k] & 0x80000000) | (mt[(k + 1) % N] & 0x7fffffff);
        mt[k] = mt[(k + M) % N] ^ (y >>> 1) ^ (y & 1 ? 0x9908b0df : 0);
      }
      this.index = 0;
    }
    let y = this.mt[this.index++];
    y ^= y >>> 11;
    y ^= (y << 7) & 0x9d2c5680;
    y ^= (y << 15) & 0xefc60000;
    y ^= y >>> 18;
    return y >>> 0;
  }

  /** A float in [0, 1) with 53 random bits. */
  random(): number {
    const a = this.genrand() >>> 5;
    const b = this.genrand() >>> 6;
    return (a * 67108864 + b) / 9007199254740992;
  }

  uniform(a: number, b: number): number {
    return a + (b - a) * this.random();
  }

  /** An integer in [0, n) by rejection on `bit_length(n)` bits (n at most 2^32). */
  randBelow(n: number): number {
    if (n <= 0) return 0;
    const k = 32 - Math.clz32(n);
    let r = this.genrand() >>> (32 - k);
    while (r >= n) r = this.genrand() >>> (32 - k);
    return r;
  }

  /** An integer in [start, stop). */
  randRange(start: number, stop: number): number {
    return start + this.randBelow(stop - start);
  }

  choice<T>(items: readonly T[]): T {
    return items[this.randBelow(items.length)]!;
  }
}
