import fs from 'node:fs';
import path from 'node:path';
import { describe, expect, test } from 'vitest';
import { bootInfo } from '@digi/regression/runtime/card-kit.ts';
import { cardRig, RT } from '@digi/regression/runtime/card-rig.ts';
import { portFile } from '@digi/regression/runtime/device-data.ts';
import { DEVICES, FIRMWARE } from '@digi/regression/runtime/devices.ts';
import { advance, rpc } from '@digi/regression/runtime/fx-live.ts';
import { planSymbol } from '@digi/regression/runtime/runtime-plan.ts';
import type { PatchedSession } from '@digi/regression/support.ts';
import type { DkSession } from '@digi/dk-session';

/**
 * core/usb-bulk (docs/plans/usb-fast.md step 3). The fixture test/usb-echo enables the bulk interface from an I/O job and echoes every buffer back on
 * EP2 IN (tests/fixtures/modules/usb-echo). The emulator does not model the USB controller: `usb.bulk.in` fills the dTDs the adapter has queued on EP2 OUT and
 * calls their completion callbacks at interrupt level, `usb.bulk.out` returns what the adapter queued on EP2 IN, and `connect` stands for the bus reset after
 * the enumeration. What this cannot show (enumeration on a real host, multi-page dTDs, ENDPTFLUSH timing, DMA and cache behaviour) is for the hardware echo.
 */

const ECHO_CMD = 0x2c; // STATUS of test/usb-echo; +1 HOLD
const STOCK = { connect: 0x400044b8, disconnect: 0x4000436e, length: 0x65 };
const PORT_ENTRY = 2;
const ENTRY_WORDS = 10; // 0x28 bytes
const TABLE_ENTRIES = 7; // entry 7 is not a port
const ALIAS = 0x08000000; // DDR_UNCACHED_ALIAS - DDR_CACHED_BASE
/** The stock configuration descriptor of USB MIDI (101 bytes, wTotalLength 0x65 as the firmware writes it at run time; finding 42 section 4). */
const STOCK_CONFIG =
  '09026500020100c0030904000000010100000924010001090001010904010002010300000724010001410006240201010006240202020009240301030102010009240302040101010009050102000200000005250101010905810200020000000525010103';
/** What the adapter serves instead: wTotalLength 0x7c, three interfaces, interface 2 (vendor class, EP2 OUT and IN bulk 512) appended. */
const EXPECTED_CONFIG = (() => {
  const b = Buffer.from(STOCK_CONFIG, 'hex');
  b[2] = 0x7c;
  b[4] = 3;
  return Buffer.concat([
    b,
    Buffer.from('09040200 02ff000000 07050202000200 07058202000200'.replaceAll(' ', ''), 'hex'),
  ]);
})();
const SELECTION = ['modules/usb-echo.dkm'];

const pattern = (n: number, seed: number) => Buffer.from(Uint8Array.from({ length: n }, (_, i) => (i * 7 + (i >> 8) + seed) & 255));

for (const device of DEVICES) {
  describe(`${device.toUpperCase()} ${FIRMWARE[device]} core/usb-bulk`, () => {
    const kit = cardRig(device, 'usb-bulk', {
      dsp: 'zero',
      roots: [...RT, 'core/usb-bulk'],
      symbols: ['DKRT_MAILBOX', 'usb_bulk_connect', 'usb_bulk_disconnect'],
      initial: () => ({ selection: [] }),
      project: true,
      uploads: true,
      relaxFs: true,
    });
    const tableBase = () => {
      const anchors = (portFile(device, FIRMWARE[device]).anchor ?? []).filter((a) => a.name === 'cf.usb.data.port_table');
      expect(anchors.length, 'the port table anchor').toBe(1);
      return anchors[0]!.addr as number;
    };
    const usb = <T>(rt: PatchedSession, run: (dk: DkSession) => Promise<T>) => kit.usb(rt, run);
    const word = async (rt: PatchedSession, addr: number) => {
      const w = await rt.session.client.memPeek(addr);
      expect(w, `guest word at ${addr.toString(16)}`).not.toBeNull();
      return w!;
    };
    /** The port table as the guest holds it: 7 entries of 10 words. */
    const table = async (rt: PatchedSession) => {
      const base = tableBase();
      const words: number[] = [];
      for (let i = 0; i < TABLE_ENTRIES * ENTRY_WORDS; i++) words.push(await word(rt, base + 4 * i));
      return words;
    };
    const entry = (words: number[]) => words.slice(PORT_ENTRY * ENTRY_WORDS, (PORT_ENTRY + 1) * ENTRY_WORDS);
    const bytesAt = async (rt: PatchedSession, addr: number, len: number) => {
      const out = Buffer.alloc((len + 3) & ~3);
      for (let i = 0; i < out.length; i += 4) out.writeUInt32BE(await word(rt, addr + i), i);
      return out.subarray(0, len);
    };
    const echoStatus = async (rt: PatchedSession) => {
      const r = await usb(rt, (dk) => dk.request(ECHO_CMD));
      expect(r.status).toBe(0);
      const b = Buffer.from(r.body);
      return {
        enable: b.readInt32BE(0),
        state: b.readUInt32BE(4),
        steps: b.readUInt32BE(8),
        taken: b.readUInt32BE(12),
        bytesIn: b.readUInt32BE(16),
        bytesOut: b.readUInt32BE(20),
        busy: b.readUInt32BE(24),
        job: b.readInt32BE(28),
        epoch: b.readUInt32BE(32),
      };
    };
    const boot = async (tag: string, selected: boolean) => {
      const rt = await kit.load(tag, { modules: ['test/usb-echo'], selection: selected ? SELECTION : [] });
      if (selected) expect((await bootInfo(rt.session, kit.plan)).results, 'the module loads').toEqual([0]);
      return rt;
    };
    /** Run until the module's job has enabled the interface (usb_bulk_enable returned) and slept. */
    const enabled = async (rt: PatchedSession) => {
      for (let i = 0; i < 100; i++) {
        await advance(rt.session, 100);
        const s = await echoStatus(rt);
        if (s.steps >= 1) return s;
      }
      throw new Error('the module never ran its first step');
    };
    const connect = () => planSymbol(kit.plan, 'usb_bulk_connect');
    const hold = async (rt: PatchedSession, mode: number) =>
      expect((await usb(rt, (dk) => dk.request(ECHO_CMD + 1, Buffer.from([mode])))).status, `hold ${mode}`).toBe(0);
    const full = (state: number) => (state >>> 8) & 0xff;
    const txBusy = (state: number) => (state >>> 16) & 0xff;
    /** What the adapter queued on EP2 IN, until `want` bytes or `limitMs` of emulated time. */
    const collect = async (rt: PatchedSession, want: number, limitMs = 60_000) => {
      const got: Buffer[] = [];
      let have = 0;
      for (let t = 0; t < limitMs && have < want; t += 20) {
        await advance(rt.session, 20);
        const r = await out(rt);
        noSysError(r);
        if (r.bytes > 0) {
          got.push(Buffer.from(r.hex, 'hex'));
          have += r.bytes;
        }
      }
      return Buffer.concat(got);
    };
    /** The bus reset after the enumeration: the first connect callback. */
    const enumerate = async (rt: PatchedSession) => {
      await rpc(rt.session, 'usb.bulk.in', { connect: connect() });
      await advance(rt.session, 50);
    };
    const hostFile = (rt: PatchedSession, name: string, data: Buffer) => {
      const file = path.join(rt.session.runDir, name);
      fs.writeFileSync(file, data);
      return file;
    };
    const out = (rt: PatchedSession) =>
      rpc<{ bytes: number; hex: string; status: { bulk_naks: number; bulk_out_zlps: number; usb_sys_error: string | null } }>(
        rt.session,
        'usb.bulk.out',
        {},
      );
    /** A wedged controller fails the test with its cause instead of a timeout. */
    const noSysError = (r: { status: { usb_sys_error: string | null } }) => {
      if (r.status.usb_sys_error !== null) throw new Error(`emulated USB controller system error: ${r.status.usb_sys_error}`);
    };
    /** Send a host message and collect what comes back until `want` bytes, in emulated time. */
    const echo = async (rt: PatchedSession, data: Buffer, first: boolean, period?: number) => {
      const file = path.join(rt.session.runDir, `bulk-${data.length}.bin`);
      fs.writeFileSync(file, data);
      await rpc(rt.session, 'usb.bulk.in', { file, ...(first ? { connect: connect() } : {}), ...(period ? { period } : {}) });
      const got: Buffer[] = [];
      let have = 0;
      for (let i = 0; i < 4000 && have < data.length; i++) {
        await advance(rt.session, 20);
        const r = await out(rt);
        noSysError(r);
        if (r.bytes > 0) {
          got.push(Buffer.from(r.hex, 'hex'));
          have += r.bytes;
        }
      }
      return Buffer.concat(got);
    };

    test('the module repoints port entry 2 and serves the 124-byte descriptor; unselected, the table is stock', async () => {
      const stockRt = await boot('stock', false);
      await advance(stockRt.session, 500);
      const stock = await table(stockRt);
      expect(entry(stock).slice(0, 2), 'stock callbacks').toEqual([STOCK.connect, STOCK.disconnect]);
      expect(entry(stock)[4], 'stock descriptor length').toBe(STOCK.length);
      expect(entry(stock)[9], 'high speed').toBe(1);

      const rt = await boot('echo', true);
      const before = (await rt.session.client.status()).heartbeat.stall_count;
      const state = await enabled(rt);
      expect(state.enable, 'usb_bulk_enable').toBe(0);
      const t = await table(rt);
      const changed = t.map((w, i) => (w === stock[i] ? -1 : i)).filter((i) => i >= 0);
      const e = PORT_ENTRY * ENTRY_WORDS;
      expect(changed, 'only +0, +4, +0xC and +0x10 of entry 2 changed').toEqual([e, e + 1, e + 3, e + 4]);
      expect(t[e], 'connect callback').toBe(planSymbol(kit.plan, 'usb_bulk_connect'));
      expect(t[e + 1], 'disconnect callback').toBe(planSymbol(kit.plan, 'usb_bulk_disconnect'));
      expect(t[e + 4], 'descriptor length').toBe(EXPECTED_CONFIG.length);
      expect(EXPECTED_CONFIG.length).toBe(124);
      const d = t[e + 3]!;
      expect(d, 'the descriptor lies in the uncached alias of DDR').toBeGreaterThanOrEqual(0x40000000 + ALIAS);
      expect(d % 16, 'aligned').toBe(0);
      expect((await bytesAt(rt, d, EXPECTED_CONFIG.length)).toString('hex'), 'the descriptor bytes').toBe(EXPECTED_CONFIG.toString('hex'));
      expect((await rt.session.client.status()).heartbeat.stall_count, 'no UI stall').toBe(before);
      await rt.session.expectNoFault('enable');
    }, 900_000);

    test('data echoes back identical for every size, through NAK flow control; USB MIDI SysEx still works beside it', async () => {
      const rt = await boot('echo-sizes', true);
      const before = (await rt.session.client.status()).heartbeat.stall_count;
      await enabled(rt);
      let first = true;
      // 1, one packet short of 512, exactly one packet (needs the zero-length packet), several dTDs of 16 KiB, a multiple of 16 KiB (the zero-length packet goes to a dTD of
      // its own), and far more than the four buffers hold (the host is NAKed until the module has released one)
      for (const [i, size] of [1, 511, 512, 513, 1024, 4096, 16384, 65536, 100_000, 262_144, 1_048_576].entries()) {
        const data = pattern(size, i);
        // the host sends a dTD's worth every 8,000 instructions, faster than the module copies one back: the four buffers fill and the host is NAKed
        const back = await echo(rt, data, first, 8_000);
        first = false;
        expect(back.length, `echo of ${size} bytes: length`).toBe(size);
        expect(back.equals(data), `echo of ${size} bytes: content`).toBe(true);
      }
      const naks = (await out(rt)).status.bulk_naks;
      expect(naks, 'the host was NAKed while all four buffers were full or taken (flow control)').toBeGreaterThan(0);
      // a slow EP2 IN (a completion every 600,000 instructions): both send slots are in flight, usb_bulk_send says E_BUSY and the job sleeps until the completion wakes it
      const slow = pattern(65_536, 77);
      expect((await echo(rt, slow, false, 600_000)).equals(slow), 'echo with slow send completions').toBe(true);
      const s = await echoStatus(rt);
      expect(s.state & 3, 'enabled and connected').toBe(3);
      expect(s.bytesIn, 'every byte was taken').toBe(s.bytesOut);
      expect(s.busy, 'the send slots ran full at least once').toBeGreaterThan(0);

      // USB MIDI while a large bulk message is in flight
      const big = pattern(262_144, 99);
      await rpc(rt.session, 'usb.bulk.in', { bytes: big.toString('hex') });
      const info = await usb(rt, (dk) => dk.info());
      expect(info, 'INFO over USB MIDI during bulk traffic').toBeTruthy();
      let have = 0;
      const got: Buffer[] = [];
      for (let i = 0; i < 4000 && have < big.length; i++) {
        await advance(rt.session, 20);
        const r = await out(rt);
        if (r.bytes > 0) {
          got.push(Buffer.from(r.hex, 'hex'));
          have += r.bytes;
        }
      }
      expect(Buffer.concat(got).equals(big), 'the bulk message arrived whole').toBe(true);
      expect((await out(rt)).status.usb_sys_error, 'no controller system error').toBeNull();
      expect((await rt.session.client.status()).heartbeat.stall_count, 'no UI stall during transfers').toBe(before);
      await rt.session.expectNoFault('echo');
    }, 1_800_000);

    test('a bus reset cancels the queued transfers, changes the reset count and the interface works again, three times over', async () => {
      const rt = await boot('echo-reset', true);
      await enabled(rt);
      expect((await echo(rt, pattern(2000, 1), true)).length).toBe(2000);
      let epoch = (await echoStatus(rt)).state >>> 24;
      for (let round = 0; round < 3; round++) {
        // the connect callback again, as the firmware gets it after a bus reset or a resume: it cancels what was queued (the stock cancel path of
        // the engine runs our cancel callback for each buffer) and queues the buffers anew
        await rpc(rt.session, 'usb.bulk.in', { connect: connect() });
        await advance(rt.session, 100);
        const s = await echoStatus(rt);
        expect(s.state >>> 24, `reset ${round + 1} changed the reset count`).toBe((epoch + 1) & 0xff);
        epoch = s.state >>> 24;
        expect(s.state & 2, 'connected again').toBe(2);
        const data = pattern(70_000 + round, 10 + round);
        expect((await echo(rt, data, false)).equals(data), `echo after reset ${round + 1}`).toBe(true);
      }
      await rt.session.expectNoFault('reset');
    }, 900_000);

    test('a frame of a multiple of 512 bytes completes only with the zero-length packet (the host\'s duty); the IN side ends such a transfer with one too', async () => {
      const rt = await boot('echo-zlp', true);
      await enabled(rt);
      await enumerate(rt);
      const data = pattern(4096, 5);
      await rpc(rt.session, 'usb.bulk.in', { file: hostFile(rt, 'noz.bin', data), zlp: false });
      await advance(rt.session, 300);
      expect((await out(rt)).bytes, 'nothing is echoed: the dTD holds eight full packets and is still open').toBe(0);
      const s = await echoStatus(rt);
      expect([s.taken, full(s.state)], 'no buffer was completed').toEqual([0, 0]);
      await rpc(rt.session, 'usb.bulk.in', { zlp: true });
      const back = await collect(rt, data.length);
      expect(back.equals(data), 'the zero-length packet completed the transfer').toBe(true);
      expect((await out(rt)).status.bulk_out_zlps, 'EP2 IN: each 4096-byte piece is followed by a zero-length packet (ZLT on)').toBeGreaterThan(0);
      await rt.session.expectNoFault('zlp');
    }, 900_000);

    test('a bus reset with a taken buffer and a partly filled dTD, and one with a send in flight, lose no buffer and no slot', async () => {
      const rt = await boot('echo-reset-hold', true);
      await enabled(rt);
      await enumerate(rt);
      /** All four buffers are usable: one is taken (held by the job), and three fill; the fourth dTD of 16 KiB is NAKed. */
      const capacity = async (tag: string) => {
        await out(rt);
        await hold(rt, 1);
        const data = pattern(4 * 16_384, 20);
        await rpc(rt.session, 'usb.bulk.in', { file: hostFile(rt, `cap-${tag}.bin`, data), period: 20_000 });
        await advance(rt.session, 300);
        const s = await echoStatus(rt);
        expect(full(s.state), `${tag}: one buffer is held, three are full: none was lost`).toBe(3);
        await hold(rt, 0);
        const back = await collect(rt, data.length);
        expect(back.equals(data), `${tag}: and the stream is whole once they are released`).toBe(true);
      };
      // 1. a taken buffer and a partly filled dTD
      await hold(rt, 1);
      await rpc(rt.session, 'usb.bulk.in', { bytes: pattern(3000, 1).toString('hex') });
      await advance(rt.session, 100);
      expect((await echoStatus(rt)).taken, 'the job holds a buffer').toBe(1);
      await rpc(rt.session, 'usb.bulk.in', { bytes: pattern(1024, 2).toString('hex'), zlp: false });
      await advance(rt.session, 100); // the second dTD holds two packets and stays open
      const r0 = (await echoStatus(rt)).state >>> 24;
      await rpc(rt.session, 'usb.bulk.in', { connect: connect() });
      await advance(rt.session, 100);
      let s = await echoStatus(rt);
      expect([s.state >>> 24, s.state & 2], 'reset counted, connected again').toEqual([(r0 + 1) & 0xff, 2]);
      expect(txBusy(s.state)).toBe(0);
      await capacity('after reset with a taken buffer');
      // 2. a send in flight (the job sent one piece of a buffer and holds the rest; the completion is far away)
      await advance(rt.session, 500); // the completions of the last pieces
      await out(rt);
      await hold(rt, 2);
      await rpc(rt.session, 'usb.bulk.in', { bytes: pattern(6000, 3).toString('hex'), period: 5_000_000 });
      await advance(rt.session, 10);
      // no USB MIDI request here: it would queue behind the completion of the send and the emulator would run until that has happened
      const first = await out(rt);
      expect(first.bytes, 'one piece of the buffer is on EP2 IN and its completion is 5,000,000 instructions away').toBe(4096);
      const r1 = s.state >>> 24; // the reset count after the first reset
      await rpc(rt.session, 'usb.bulk.in', { connect: connect() });
      await advance(rt.session, 100);
      s = await echoStatus(rt);
      expect([s.state >>> 24, txBusy(s.state), s.state & 2], 'reset: the slot is free, connected again').toEqual([(r1 + 1) & 0xff, 0, 2]);
      await capacity('after reset with a send in flight');
      await rt.session.expectNoFault('reset with traffic');
    }, 1_800_000);

    test('outside USB MIDI mode enable is refused and the state shows the wrong mode; back in USB MIDI it enables again', async () => {
      const rt = await boot('echo-mode', true);
      await enabled(rt);
      const anchors = (portFile(device, FIRMWARE[device]).anchor ?? []).filter((a) => a.name === 'cf.usb.value.active_port');
      expect(anchors.length, 'the active port anchor').toBe(1);
      const portAddr = anchors[0]!.addr as number;
      const setPort = (value: number) => rpc(rt.session, 'mem.cf.poke', { addr: portAddr, value });
      /** REENABLE, then run until the job has stored a new enable result. */
      const reenable = async (want: number) => {
        expect((await usb(rt, (dk) => dk.request(ECHO_CMD + 2))).status, 'reenable').toBe(0);
        for (let i = 0; i < 100; i++) {
          await advance(rt.session, 100);
          const s = await echoStatus(rt);
          if (s.enable === want) return s;
        }
        throw new Error(`usb_bulk_enable never returned ${want}`);
      };
      try {
        expect(await word(rt, portAddr), 'the firmware is in USB MIDI mode').toBe(2);
        await setPort(5);
        const refused = await reenable(-53);
        expect(refused.state & 0x4, 'wrong USB mode').toBe(0x4);
        await setPort(2);
        const again = await reenable(0);
        expect(again.state & 0x1, 'enabled').toBe(0x1);
        expect(again.state & 0x4, 'USB MIDI mode').toBe(0);
        await enumerate(rt);
        const data = pattern(4096, 31);
        expect((await echo(rt, data, false)).equals(data), 'echo after the mode returned to USB MIDI').toBe(true);
        await rt.session.expectNoFault('usb mode');
      } finally {
        await setPort(2);
      }
    }, 1_800_000);
  });
}
