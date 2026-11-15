/**
 * core/sample-pool, Digitone II DSP object on the SHARC-only rig: our assembled receiver against stand-ins of the stock LP0 driver.
 * Port of test_sample_pool_dsp_dn2.py (T1g phase 5).
 *
 * The stand-ins (tests/fixtures/sample-pool-dn2/driver.s) only record what we pass to the driver; they assert nothing about the driver
 * itself. What is tested is our code: the set-up order and refusals, the page rules against the C reference (spw_dsp_rule in
 * pool_wire.h), where a page lands in DDR, the rearm after every callback, and the acknowledgement words, which are decoded by the C
 * decoder of the ColdFire side. The real driver, the DMA, the cache and the timing are for the emulator test.
 */
import { execFileSync } from 'node:child_process';
import { mkdtempSync, readFileSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import path from 'node:path';
import { afterAll, beforeAll, describe, expect, it } from 'vitest';
import { RawRig } from '@digi/sharc-rig-kit';

const REPO = path.resolve(import.meta.dirname, '../../../../..');
const MOD = path.join(REPO, 'modules/core/adapters/sample-pool');
const FIX = path.join(REPO, 'tests/fixtures/sample-pool-dn2');
const BACK = 0xbb0000;
const PORT = 0x20070000;
const STACK = 0x20078000;
const GEN = readFileSync(path.join(MOD, 'build/gen/sample-pool-dsp-dn2.inc'), 'utf8');
const DDR_BASE_NW32 = Number(/#define SP_DDR_BASE_NW32 (\d+)/.exec(GEN)![1]);
const DDR_PAGES = Number(/#define SP_DDR_PAGES (\d+)/.exec(GEN)![1]);
const CONSTS = { M5: 0, M6: 1, M7: 0xffffffff, M13: 0, M14: 1, M15: 0xffffffff };
const REGS: Record<string, number> = { ...CONSTS, I6: STACK, I7: STACK, MODE1: 0x10000, IMASKP: 0 };
const REPLY = 0x20050000;
const u32 = (n: number): number => n >>> 0;

const BOOT_DRIVER = `
.SECTION/PM seg_pmco;
.GLOBAL _sp_cache_range;
_sp_cache_range:
    DK_ENTER(1); DK_SAVE(2, R3);
    DM(_test_cache_start) = R4; DM(_test_cache_end) = R8;
    DM(_test_cache_selector) = R12;
    R0 = DM(1, I6); DM(_test_cache_mode) = R0;
    R0 = DM(_test_cache_calls); R0 = R0 + 1; DM(_test_cache_calls) = R0;
    DM(0x3e018) = R4; R8 = R8 + 1; DM(0x3e019) = R8;
    R0 = 0x300; DM(0x3e002) = R0;
    R0 = DM(_test_cache_result);
    DK_RETADDR; NOP; NOP; NOP; DK_LEAVE(R3, 2);
.SECTION/DM seg_dmda;
.VAR _test_cache_start = 0; .VAR _test_cache_end = 0;
.VAR _test_cache_selector = 0; .VAR _test_cache_mode = 0;
.VAR _test_cache_calls = 0; .VAR _test_cache_result = 0;
`;

const SOURCES = ['src/dsp/dn2/boot-window.s', 'src/dsp/dn2/sample-pool.s', null];
const INCLUDES = [path.join(REPO, 'sdk/dsp/contract'), path.join(REPO, 'sdk'), path.join(MOD, 'build/gen')];

/** A fresh core with the three objects placed (the receiver, its boot window and the driver stand-ins), data at 0x20080000. */
class Pool {
  private constructor(readonly rig: RawRig) {}

  static async fresh(): Promise<Pool> {
    const bases = [0xb90000, 0xba0000, 0xbc0000];
    const rig = await RawRig.open({
      objects: SOURCES.map((s, i) => ({
        text: s === null ? readFileSync(path.join(FIX, 'driver.s'), 'utf8') + BOOT_DRIVER : readFileSync(path.join(MOD, s), 'utf8'),
        includes: INCLUDES,
        base: bases[i]!,
      })),
      externs: { _sp_boot_back: BACK, _sp_back: BACK, _test_return: BACK, _sp_port_record: PORT },
      returnSw: BACK,
      dataBase: 0x20080000,
    });
    rig.poke(0x3e000, 0x414141);
    rig.poke((PORT + 20) / 4, 0);
    for (const address of [0x30ffe000, 0x30fff008]) rig.poke(address, 0);
    return new Pool(rig);
  }

  /** The words of data symbols, in one round trip. */
  async vals<K extends string>(...names: K[]): Promise<Record<K, number>> {
    const rows = await this.rig.peekMany(names.map((n) => [this.rig.data[n]! / 4, 1]));
    return Object.fromEntries(names.map((n, i) => [n, rows[i]![0]!])) as Record<K, number>;
  }

  async val(name: string): Promise<number> {
    return (await this.vals(name))[name]!;
  }

  async boot(): Promise<void> {
    const r = await this.rig.call('_sp_boot_hook', { ...REGS }, { maxSteps: 30000, readRegs: Object.keys(REGS) });
    expect(r.halt).toBe('breakpoint');
    expect(await this.val('_sp_boot_status')).toBe(1);
  }

  /** One block tail (the first one sets up the port). */
  async tail(extra: Record<string, number> = {}, bootFirst = true): Promise<void> {
    if (bootFirst && (await this.val('_sp_boot_status')) === 0) await this.boot();
    const regs = { ...REGS, ...extra };
    const r = await this.rig.call('_sp_tail', regs, { maxSteps: 30000, readRegs: Object.keys(regs) });
    expect(r.halt).toBe('breakpoint');
    // every register comes back as it went in
    const changed = Object.entries(regs).filter(([k, v]) => r.regs[k] == null || u32(r.regs[k]!) !== u32(v));
    expect(changed).toEqual([]);
  }

  async init(): Promise<void> {
    await this.tail({ I4: REPLY });
  }

  async replyWords(): Promise<number[]> {
    return (await this.rig.peek((REPLY + 0xabc) / 4, 4)) as number[];
  }

  async decodedStatus(): Promise<number> {
    return (await this.replyWords())[1]! & 0xffff;
  }

  /** Hand the callback one received frame: the tag, then 1,024 payload words. */
  async sendPage(
    tagWord: number,
    o: { fill?: number[]; event?: number; address?: number; context?: number } = {},
  ): Promise<number[]> {
    const rx = await this.val('_sp_rx');
    const payload =
      o.fill ?? Array.from({ length: 1024 }, (_, i) => u32(0x12340000 + i * 3 + (tagWord & 0xffff)));
    this.rig.poke(rx / 4, tagWord);
    payload.forEach((w, i) => this.rig.poke(rx / 4 + 1 + i, w));
    const regs = {
      ...REGS,
      R4: o.context ?? this.rig.data._sp_status!,
      R8: o.event ?? 4,
      R12: o.address ?? rx,
    };
    const r = await this.rig.call('_test_callback', regs, { maxSteps: 30000, readRegs: ['I6', 'I7'] });
    expect(r.halt).toBe('breakpoint');
    expect([r.regs.I6, r.regs.I7]).toEqual([STACK, STACK]);
    return payload;
  }

  async dest(page: number): Promise<(number | null)[]> {
    return this.rig.peek(DDR_BASE_NW32 + page * 1024, 1024);
  }
}

const tag = (seq: number, page: number): number => u32(0xd0000000 | (seq << 20) | page);

// ---- the C reference and the C decoder of the ColdFire side ----

const REF_C = `
#include <stdio.h>
#include <stdlib.h>
#include "cf/pool_wire.h"
int main(int argc, char **argv)
{
    if (argv[1][0] == 'r') {   /* rule: last tag page-count */
        printf("%u\\n", spw_dsp_rule((uint32_t)strtoul(argv[2], 0, 16), (uint32_t)strtoul(argv[3], 0, 16), (uint32_t)atoi(argv[4])));
    } else {                   /* decode four words */
        uint16_t h[8]; spw_ack a; int i;
        for (i = 0; i < 4; i++) { uint32_t w = (uint32_t)strtoul(argv[2 + i], 0, 16); h[2 * i] = w & 0xffff; h[2 * i + 1] = w >> 16; }
        if (!spw_ack_decode(h, &a)) { puts("invalid"); return 0; }
        printf("%u %u %u %u %x\\n", a.status, a.error, a.errcount, a.count, a.last);
    }
    return 0;
}
`;

let tmp = '';
let exe = '';
const ref = (...args: (string | number)[]): string =>
  execFileSync(exe, args.map(String), { encoding: 'utf8' }).trim();

beforeAll(() => {
  tmp = mkdtempSync(path.join(tmpdir(), 'sp-ref-'));
  const src = path.join(tmp, 'ref.c');
  exe = path.join(tmp, 'ref');
  execFileSync('sh', ['-c', `cat > ${src}`], { input: REF_C });
  execFileSync(
    'cc',
    ['-Wall', '-Werror', `-I${path.join(MOD, 'src')}`, `-I${path.join(REPO, 'modules/core/runtime/include')}`, src, '-o', exe],
    { timeout: 25_000 },
  );
});
afterAll(() => rmSync(tmp, { recursive: true, force: true }));

function decoded(words: number[]): 'invalid' | number[] {
  const out = ref('d', ...words.map((w) => u32(w).toString(16)));
  return out === 'invalid' ? out : out.split(' ').map((x, i) => (i === 4 ? parseInt(x, 16) : Number(x)));
}

const withPool = (fn: (p: Pool) => Promise<void>) => async () => {
  const p = await Pool.fresh();
  try {
    await fn(p);
  } finally {
    await p.rig.close();
  }
};

describe('core/sample-pool DN2 DSP receiver on the rig', () => {
  it(
    'setup once in the stock order and every register returns',
    withPool(async (p) => {
      expect(DDR_BASE_NW32).toBe(0x1014c400); // stock DDR 0x80531000, as test_sample_pool_dsp_dn2.py pins
      expect(await p.val('_sp_status')).toBe(0);
      await p.init();
      expect(await p.val('_sp_status')).toBe(1);
      expect(await p.val('_sp_started')).toBe(1);
      const v = await p.vals('_test_open_arg0', '_test_open_arg1', '_test_open_size', '_test_setcb_fn', '_test_setcb_context');
      expect([v._test_open_arg0, v._test_open_arg1]).toEqual([0, 1]);
      expect(v._test_open_size).toBe(128);
      expect(v._test_setcb_fn).toBe(p.rig.symbols._sp_callback);
      expect(v._test_setcb_context).toBe(p.rig.data._sp_status);
      const s = await p.vals('_test_submit_config', '_test_submit_arg2');
      expect([s._test_submit_config, s._test_submit_arg2]).toEqual([0, 1]);
      const rx = await p.val('_sp_rx');
      expect(rx % 64).toBe(0);
      // descriptor: 4,100 bytes (0x401 words of 4) into the receive buffer, rebuilt for every submit
      const names = Array.from({ length: 7 }, (_, i) => `_test_desc${i}`);
      const d = await p.vals(...names);
      expect(names.map((n) => d[n as keyof typeof d])).toEqual([0, rx, 0x100000, 0x401, 4, 0, 0]);
      await p.init(); // later blocks only report
      const c = await p.vals('_test_open_calls', '_test_submit_calls');
      expect([c._test_open_calls, c._test_submit_calls]).toEqual([1, 1]);
    }),
  );

  it('a hostile register file is saved and restored', async () => {
    // tail() checks every register
    const hostile: Record<string, number> = {};
    for (let i = 0; i < 16; i++) hostile[`R${i}`] = 0x7100 + i;
    for (let i = 0; i < 16; i++) if (![4, 6, 7].includes(i)) hostile[`I${i}`] = 0x20060000 + 16 * i;
    for (let i = 0; i < 16; i++) hostile[`M${i}`] = 9 + i;
    for (let i = 0; i < 8; i++) hostile[`L${i}`] = 17 + i;
    for (let i = 0; i < 8; i++) hostile[`B${i}`] = 0x20050000 + 16 * i;
    await withPool(async (p) => {
      await p.boot();
      await p.tail({ ...hostile, I4: REPLY });
      expect(await p.val('_sp_status')).toBe(1);
    })();
  });

  for (const bad of ['context', 'owner', 'lpctl', 'dmacfg', 'boot'] as const)
    it(
      `setup refusal makes the link dead before any driver call: ${bad}`,
      withPool(async (p) => {
        const regs: Record<string, number> = { I4: REPLY };
        if (bad !== 'boot') await p.boot(); // 'boot': no boot hook ran, the window is not there
        if (bad === 'context') regs.IMASKP = 1;
        else if (bad !== 'boot') {
          const addr = { owner: PORT + 20, lpctl: 0x30ffe000, dmacfg: 0x30fff008 }[bad];
          p.rig.poke(bad === 'owner' ? addr / 4 : addr, 1);
        }
        await p.tail(regs, false);
        expect(await p.val('_sp_status')).toBe(3);
        expect(await p.val('_sp_error')).toBe({ context: 1, owner: 2, lpctl: 2, dmacfg: 2, boot: 8 }[bad]);
        let c = await p.vals('_test_open_calls', '_test_submit_calls');
        expect([c._test_open_calls, c._test_submit_calls]).toEqual([0, 0]);
        await p.tail(regs, false); // it stays dead and tries nothing
        c = await p.vals('_test_open_calls', '_test_submit_calls');
        expect(c._test_open_calls).toBe(0);
        expect(await p.decodedStatus()).toBe(3);
      }),
    );

  it(
    'page rules match the C reference and pages land where the tag says',
    withPool(async (p) => {
      await p.init();
      let last = tag(0, 0);
      let accepted = 0;
      let errors = 0;
      let submits = 1;
      // new pages (any order), a gap, a repeat, bad index, bad prefix, a stale sequence number, and 270 more pages: the sequence wraps
      const plan = [
        tag(1, 0), tag(2, 5), tag(3, 5), tag(3, 5), tag(5, 1), tag(4, DDR_PAGES), tag(4, DDR_PAGES - 1), 0xc0500001,
        tag(5, 0xfffff), tag(5, 9), tag(5, 9), tag(4, 9), tag(6, 7),
      ]; // prettier-ignore
      let seq = 6;
      for (let i = 0; i < 270; i++) {
        seq = (seq + 1) & 0xff;
        plan.push(tag(seq, (i * 977) % DDR_PAGES));
      }
      for (const t of plan) {
        const page = t & 0xfffff;
        const before = t >>> 28 === 0xd && page < DDR_PAGES ? await p.dest(page) : null;
        const verdict = Number(ref('r', last.toString(16), t.toString(16), DDR_PAGES));
        const payload = await p.sendPage(t);
        submits += 1; // every callback arms again
        if (verdict === 0 || verdict === 1) expect(await p.dest(page), `0x${t.toString(16)}`).toEqual(payload);
        else if (before !== null) expect(await p.dest(page), `0x${t.toString(16)}`).toEqual(before); // a rejected page writes nothing
        if (verdict === 0) {
          accepted += 1;
          last = t;
        } else if (verdict >= 4) errors += 1;
        const v = await p.vals('_sp_pages', '_sp_last', '_sp_errcount', '_sp_error', '_sp_status', '_test_submit_calls');
        const at = `0x${t.toString(16)}`;
        expect(v._sp_pages, at).toBe(accepted);
        expect(v._sp_last, at).toBe(last);
        expect(v._sp_errcount, at).toBe(errors);
        expect(v._sp_error, at).toBe(verdict >= 4 ? verdict : 0);
        expect(v._sp_status, at).toBe(verdict >= 4 ? 2 : 1);
        expect(v._test_submit_calls).toBe(submits);
      }
      // 6 + 270 new pages (the sequence number wraps past 255); the gap, the stale number, two bad indexes and the bad prefix are the 5 errors
      expect([accepted, errors]).toEqual([276, 5]);
    }),
  );

  it(
    'acknowledgement words decode in the CF decoder and keep the stock bytes',
    withPool(async (p) => {
      for (let i = 0; i < (0xabc + 16) / 4; i++) p.rig.poke(REPLY / 4 + i, 0xcafe1234);
      await p.init();
      const kept = await p.rig.peek(REPLY / 4, 0xabc / 4);
      expect(kept).toEqual(Array.from({ length: 0xabc / 4 }, () => 0xcafe1234));
      expect(decoded(await p.replyWords())).toEqual([1, 0, 0, 0, 0]);
      await p.sendPage(tag(1, 70000 % DDR_PAGES));
      await p.sendPage(tag(2, 3));
      // the tail runs with other DAG state: the reply still lands, and only after the callback finished
      await p.tail({ I4: REPLY, M6: 9, L4: 17, L0: 17, MODE1: 0x219000 });
      expect(decoded(await p.replyWords())).toEqual([1, 0, 0, 2, (2 << 8) | 3]);
      await p.sendPage(tag(7, 1)); // a gap
      await p.tail({ I4: REPLY });
      expect(decoded(await p.replyWords())).toEqual([2, 5, 1, 2, (2 << 8) | 3]);
      await p.sendPage(tag(3, 0x1ff));
      await p.tail({ I4: REPLY });
      // the error is gone, its count stays; the page index shows its low byte only
      expect(decoded(await p.replyWords())).toEqual([1, 0, 1, 3, (3 << 8) | 0xff]);
      // count above 16 bits and a high error count
      p.rig.poke(p.rig.data._sp_pages! / 4, 0x12345);
      p.rig.poke(p.rig.data._sp_errcount! / 4, 0x1ff);
      await p.tail({ I4: REPLY });
      expect(decoded(await p.replyWords())).toEqual([1, 0, 0xff, 0x12345, (3 << 8) | 0xff]);
      // a dead link shows status 3 and its error code
      p.rig.poke(p.rig.data._sp_status! / 4, 3);
      p.rig.poke(p.rig.data._sp_error! / 4, 9);
      await p.tail({ I4: REPLY });
      expect((decoded(await p.replyWords()) as number[]).slice(0, 2)).toEqual([3, 9]);
    }),
  );

  for (const bad of ['event', 'address', 'context'] as const)
    it(
      `a callback with wrong arguments kills the link without copy or rearm: ${bad}`,
      withPool(async (p) => {
        await p.init();
        const o: { event?: number; address?: number; context?: number } = {};
        if (bad === 'event') o.event = 3;
        else if (bad === 'address') o.address = (await p.val('_sp_rx')) + 4;
        else o.context = 0;
        const before = await p.dest(0);
        await p.sendPage(tag(1, 0), o);
        expect(await p.dest(0)).toEqual(before);
        let v = await p.vals('_sp_status', '_sp_error', '_test_submit_calls');
        expect([v._sp_status, v._sp_error]).toEqual([3, 4]);
        expect(v._test_submit_calls).toBe(1);
        await p.sendPage(tag(1, 0)); // dead: further callbacks do nothing
        v = await p.vals('_sp_status', '_sp_error', '_test_submit_calls');
        expect(await p.dest(0)).toEqual(before);
        expect(v._test_submit_calls).toBe(1);
      }),
    );

  it(
    'a callback before the link is armed is refused',
    withPool(async (p) => {
      await p.boot();
      // status 0: the port was not set up (a callback should not exist yet)
      const rx = await p.val('_sp_rx');
      const regs = { ...REGS, R4: p.rig.data._sp_status!, R8: 4, R12: rx };
      p.rig.poke(rx / 4, tag(1, 0));
      const r = await p.rig.call('_test_callback', regs, { maxSteps: 30000, readRegs: ['I6'] });
      expect(r.halt).toBe('breakpoint');
      const v = await p.vals('_sp_status', '_sp_error');
      expect([v._sp_status, v._sp_error]).toEqual([3, 4]);
    }),
  );

  it(
    'a rearm failure is dead with error 9',
    withPool(async (p) => {
      await p.init();
      p.rig.poke(p.rig.data._test_submit_result! / 4, 5); // the driver refuses the next submit
      await p.sendPage(tag(1, 2));
      expect(await p.val('_sp_pages')).toBe(1); // the page itself was taken
      const v = await p.vals('_sp_status', '_sp_error');
      expect([v._sp_status, v._sp_error]).toEqual([3, 9]);
      await p.tail({ I4: REPLY });
      expect(await p.decodedStatus()).toBe(3);
    }),
  );
});
