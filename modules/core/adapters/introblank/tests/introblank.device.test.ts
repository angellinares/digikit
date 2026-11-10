import path from 'node:path';
import type { Device } from '@digi/test-support';
import { expect, test } from 'vitest';
import { bootInfo, bootInfoLoaded } from '@digi/regression/runtime/card-kit.ts';
import { CARD_SYMBOLS, cardRigs, RT, type CardRig } from '@digi/regression/runtime/card-rig.ts';
import { capture, peak, readFloatWav } from '@digi/regression/runtime/fx-live.ts';
import { introNow } from '@digi/regression/runtime/intro-frame.ts';
import { ringEvents } from '@digi/regression/runtime/runtime-menu.ts';
import { bootScreenCard, IB, lastFrame } from './introblank-support.ts';

/**
 * core/introblank: the stock intro draws black (the adapter's scene table replaces the stock one at the intro setup) until a boot screen module or the
 * safe-mode screen takes over; when nothing takes over, the runtime ends the intro after the module load. The tests run the same cards on an image without
 * the adapter (the stock intro) and with it. The framebuffer is read at the ready point: the intro's last frame.
 * This file: the stock reference and the adapter (the adapter tests compare with the reference, so they stay together). REC held is in introblank-rec.
 */

/** The stock intro's length in ticks (the five stock scene tables and the adapter's table). */
const STOCK_TICKS = 175;
/** The intro's drawn-frame counter (`_DAT_43153a0c` on the DT2, `_DAT_42c4cad8` on the DN2: the setup function clears it). */
const TICK_COUNTER: Record<Device, number> = { dt2: 0x43153a0c, dn2: 0x42c4cad8 };
/** ColdFire instructions per DSP block (132 M a second, 1500 blocks a second). */
const INSNS_PER_BLOCK = 88_000;

const BLACK = Buffer.alloc(1024);

/** What the boot reached, from the runtime's UI_FIRST event (DSP blocks at the first UI pass) and the intro's own counter. */
async function uiReached(h: CardRig) {
  const events = await ringEvents(h.session, h.plan, 'UI_FIRST');
  const blocks = events.at(-1)!.args[0]! >>> 0;
  const ticks = (await h.session.client.memPeek(TICK_COUNTER[h.device]))! >>> 0;
  return { blocks, insns: blocks * INSNS_PER_BLOCK, ticks };
}

/** A note of the sine machine reaches the PCM tap: the DSP runs and the machine list is complete. */
async function playsANote(h: CardRig, black: boolean) {
  const sine = h.mod('test/sine', 'sine');
  await h.load('note', { modules: [sine], selection: [sine.name] });
  const info = await bootInfoLoaded(h.session, h.plan);
  expect(info).toMatchObject({ safe: 0, loaded: 1, uiReached: 1 });
  if (black) expect((await introNow(h.session)).equals(BLACK), 'black: the adapter ended the intro').toBe(true);
  const status = await h.session.status();
  expect(status.dsp).toBe('sharc');
  expect(status.audio?.halted ?? false).toBeFalsy();
  // the first SRC list after a boot may not take (the project round trip tries the list position up to four times): same position twice, then its neighbours
  let best = 0;
  let tries = 0;
  for (const hint of [undefined, undefined, 6, 8]) {
    const wav = path.join(h.artifacts, `note-${tries++}.wav`);
    await capture(h.runtime, wav, hint);
    best = peak(readFloatWav(wav));
    if (best > 0.01) break;
  }
  console.log(`INTROBLANK ${h.device} ${black ? 'adapter' : 'stock'} note peak=${best} tries=${tries}`);
  expect(best, 'the machine makes sound').toBeGreaterThan(0.01);
}

/** The stock image's numbers per device, for the image with the adapter to beat. */
const stock = new Map<Device, { blocks: number; ticks: number }>();

cardRigs('introblank: the stock image (reference)', { dsp: 'sharc', relaxFs: true, roots: RT, symbols: CARD_SYMBOLS }, (h) => {
  test(
    'no boot screen: the stock intro plays to the end before the UI',
    async () => {
      await h.load('stock', { modules: [], selection: [] });
      const ui = await uiReached(h);
      console.log(`INTROBLANK ${h.device} stock blocks=${ui.blocks} insns=${ui.insns} ticks=${ui.ticks}`);
      expect(ui.ticks, 'the stock intro ran its whole length').toBeGreaterThanOrEqual(STOCK_TICKS);
      stock.set(h.device, ui);
    },
  );

  test('control: a note plays on the stock image', () => playsANote(h, false));
});

cardRigs('introblank', { dsp: 'sharc', relaxFs: true, roots: IB, symbols: CARD_SYMBOLS }, (h) => {
  test(
    'no boot screen: the intro is black and the UI comes sooner than on the stock image',
    async () => {
      await h.load('black', { modules: [], selection: [] });
      const info = await bootInfo(h.session, h.plan);
      expect(info).toMatchObject({ safe: 0, uiReached: 1 });
      const ui = await uiReached(h);
      const ref = stock.get(h.device);
      console.log(`INTROBLANK ${h.device} adapter blocks=${ui.blocks} insns=${ui.insns} ticks=${ui.ticks}`);
      expect((await introNow(h.session)).equals(BLACK), 'the intro framebuffer is all black').toBe(true);
      expect(ui.ticks, 'the intro ended early').toBeLessThan(STOCK_TICKS);
      expect(ref, 'the stock reference ran first').toBeDefined();
      expect(ref!.blocks - ui.blocks, 'at least 2 s of emulated time sooner').toBeGreaterThan(2 * 1500);
    },
  );

  test(
    'a boot screen is selected: the module takes over and its last frame stays',
    async () => {
      await h.load('screen', bootScreenCard(h));
      const info = await bootInfoLoaded(h.session, h.plan);
      expect(info).toMatchObject({ safe: 0, loaded: 1, uiReached: 1 });
      expect((await introNow(h.session)).equals(lastFrame), 'the module frame').toBe(true);
    },
  );

  test(
    'after the early end the boot is complete: a note plays and the DSP runs',
    () => playsANote(h, true),
  );
});
