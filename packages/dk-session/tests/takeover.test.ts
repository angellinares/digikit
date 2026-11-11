// The take-over helper (bulk-select.ts) against fakes: TAKEOVER, the re-enumeration (the SysEx session may change), the nonce handshake with retries, the
// batches, the 10 s linger and RELEASE, release at once on close.
import { afterEach, beforeEach, describe, expect, test, vi } from 'vitest';
import { BulkTakeOver, type TakeOverDevice, type TakeOverSysex } from '../src/index.ts';
import { FakeBulkDevice } from '../src/bulk-fake.ts';

/** A device that is in USB AUDIO/MIDI until TAKEOVER: it has no bulk interface for `bulkAfter` handshake tries after the re-enumeration. */
function rig(
  opts: { refuse?: boolean; bulkAfter?: number; neverBulk?: boolean; newSessionAfterSwitch?: boolean } = {},
) {
  const log: string[] = [];
  const state = { bulk: new FakeBulkDevice() };
  let tries = 0;
  let switched = false;
  const make = (name: string): TakeOverSysex => ({
    usbFastNonce: async (n) => {
      log.push(`${name}:nonce`);
      state.bulk.setNonce(n);
    },
    usbFastTakeOver: async () => {
      log.push(`${name}:takeover`);
      if (opts.refuse) throw new Error('E_STATE');
      switched = true;
    },
    usbFastRelease: async () => {
      log.push(`${name}:release`);
    },
  });
  const first = make('s1');
  const second = make('s2');
  const device: TakeOverDevice = {
    sysex: () => (switched && opts.newSessionAfterSwitch ? second : first),
    reenumerated: async () => {
      log.push('reenumerated');
    },
  };
  const opener = {
    candidates: async () => {
      if (!switched || opts.neverBulk) return [];
      return ++tries > (opts.bulkAfter ?? 0) ? [{ id: '1:1', name: 'dt' }] : [];
    },
    open: async () => state.bulk,
  };
  return {
    log,
    get bulk() {
      return state.bulk;
    },
    /** The device is a new one after the next take-over (the held link's device is gone): returns the old one. */
    swap: () => {
      const old = state.bulk;
      state.bulk = new FakeBulkDevice();
      return old;
    },
    device,
    opener,
  };
}

beforeEach(() => {
  vi.useFakeTimers();
});
afterEach(() => {
  vi.useRealTimers();
});

describe('BulkTakeOver', () => {
  test('begin takes over, waits for the bulk interface and finds the device by the nonce on the session it has after the switch', async () => {
    const r = rig({ bulkAfter: 2, newSessionAfterSwitch: true });
    const t = new BulkTakeOver(r.device, r.opener, { pollMs: 100, bulk: { timeoutMs: 20 } });
    const p = t.begin();
    await vi.advanceTimersByTimeAsync(1000);
    const c = await p;
    expect(c?.id).toBe('1:1');
    expect(r.log[0]).toBe('s1:takeover');
    expect(r.log[1]).toBe('reenumerated');
    expect(r.log.slice(2)).toEqual(['s2:nonce']); // empty candidate lists send no nonce; the handshake runs on the new session
    expect(t.active).toBe(true);
    await t.close();
  });

  test('the linger: RELEASE 10 s after the last batch; a batch within the linger reuses the link without a second take-over', async () => {
    const r = rig();
    const t = new BulkTakeOver(r.device, r.opener, { bulk: { timeoutMs: 20 } });
    const [a, b] = await Promise.all([t.begin(), t.begin()]);
    expect(a).toBeDefined();
    expect(b).toBe(a);
    expect(r.log.filter((l) => l.endsWith('takeover'))).toHaveLength(1);
    t.end();
    await vi.advanceTimersByTimeAsync(20_000);
    expect(
      r.log.filter((l) => l.endsWith('release')),
      'one batch still runs',
    ).toHaveLength(0);
    t.end();
    await vi.advanceTimersByTimeAsync(9_000);
    expect(await t.begin(), 'a second batch inside the linger').toBe(a);
    t.end();
    await vi.advanceTimersByTimeAsync(9_900);
    expect(
      r.log.filter((l) => l.endsWith('release')),
      'the linger restarted at the last end',
    ).toHaveLength(0);
    await vi.advanceTimersByTimeAsync(200);
    expect(r.log.filter((l) => l.endsWith('release'))).toHaveLength(1);
    expect(r.bulk.closed).toBe(true);
    expect(t.active).toBe(false);
    // after the release the next batch takes over again
    r.bulk.closed = false;
    expect(await t.begin()).toBeDefined();
    expect(r.log.filter((l) => l.endsWith('takeover'))).toHaveLength(2);
    await t.close();
  });

  test('a held link that died (the link closed, or the device went back to USB AUDIO/MIDI by itself and is silent) is dropped: the next batch takes over again', async () => {
    const r = rig();
    const notes: string[] = [];
    const t = new BulkTakeOver(r.device, r.opener, {
      note: (n) => notes.push(n),
      bulk: { timeoutMs: 20 },
      aliveMs: 200,
    });
    expect(await t.begin()).toBeDefined();
    t.end();
    // the link reports closed
    r.swap().closed = true;
    const b = await t.begin();
    expect(b, 'a new take-over').toBeDefined();
    expect(r.log.filter((l) => l.endsWith('takeover'))).toHaveLength(2);
    expect(notes.at(-1)).toMatch(/closed/);
    t.end();
    // the link is open but the device is silent
    const old = r.swap();
    old.mute.add(0x01);
    const p = t.begin();
    await vi.advanceTimersByTimeAsync(5000);
    expect(await p).toBeDefined();
    expect(
      r.log.filter((l) => l.endsWith('takeover')),
      'a silent link: a third take-over',
    ).toHaveLength(3);
    expect(old.closed, 'the dead link is closed').toBe(true);
    expect(
      notes.filter((n) => /lost/.test(n)),
      'one note for each dead link',
    ).toHaveLength(2);
    await t.close();
  });

  test('a healthy idle held link is checked and reused: no second take-over', async () => {
    const r = rig();
    const t = new BulkTakeOver(r.device, r.opener, { bulk: { timeoutMs: 20 } });
    const a = await t.begin();
    t.end();
    expect(await t.begin()).toBe(a);
    expect(r.log.filter((l) => l.endsWith('takeover'))).toHaveLength(1);
    await t.close();
  });

  test('close releases at once, also with the linger armed', async () => {
    const r = rig();
    const t = new BulkTakeOver(r.device, r.opener, { bulk: { timeoutMs: 20 } });
    await t.begin();
    t.end();
    await t.close();
    expect(r.log.filter((l) => l.endsWith('release'))).toHaveLength(1);
    expect(r.bulk.closed).toBe(true);
    await vi.advanceTimersByTimeAsync(30_000);
    expect(
      r.log.filter((l) => l.endsWith('release')),
      'the timer is gone',
    ).toHaveLength(1);
  });

  test('a refused take-over (Overbridge, usb-fast not loaded) is a note and no release', async () => {
    const r = rig({ refuse: true });
    const notes: string[] = [];
    const t = new BulkTakeOver(r.device, r.opener, { note: (n) => notes.push(n) });
    expect(await t.begin()).toBeUndefined();
    expect(notes[0]).toMatch(/take-over was refused/);
    expect(r.log).toEqual(['s1:takeover']);
    expect(t.active).toBe(false);
  });

  test('no bulk interface within the start timeout: undefined, the reason, and the device is released', async () => {
    const r = rig({ neverBulk: true });
    const notes: string[] = [];
    const t = new BulkTakeOver(r.device, r.opener, { startTimeoutMs: 1000, pollMs: 100, note: (n) => notes.push(n) });
    const p = t.begin();
    await vi.advanceTimersByTimeAsync(2000);
    expect(await p).toBeUndefined();
    expect(notes.at(-1)).toMatch(/no bulk interface within 1000 ms/);
    expect(r.log.at(-1)).toBe('s1:release');
    expect(t.active).toBe(false);
  });

  test('the device does not come back: undefined, release attempted', async () => {
    const r = rig();
    r.device.reenumerated = async () => {
      throw new Error('timeout');
    };
    const notes: string[] = [];
    const t = new BulkTakeOver(r.device, r.opener, { note: (n) => notes.push(n) });
    expect(await t.begin()).toBeUndefined();
    expect(notes[0]).toMatch(/did not come back/);
    expect(r.log.at(-1)).toBe('s1:release');
  });
});
