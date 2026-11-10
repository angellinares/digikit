import { createHash } from 'node:crypto';
import { mkdirSync, mkdtempSync, readFileSync, writeFileSync } from 'node:fs';
import path from 'node:path';
import { DkSession, EmuUsbTransport } from '@digi/dk-session';
import { Sample, prepareUpload } from '@digi/dk-session/dksample';
import { openSamples } from '@digi/dk-session/samples';
import { button, repoRoot, type Session } from '@digi/test-support';
import { expect, test } from 'vitest';
import { withSession } from '../support.ts';
import { record } from './artifacts.ts';
import { memory, panel } from './panel.ts';
import { pollEmulated } from './waits.ts';

// Current DT2 1.17 observations in finding81. All diagnostic accesses are reads.
const CF = { slotBlock: 0x405b5368, slotRef: 0x405b1368, slotLength: 0x405ba768, blocks: 0x405a0f64 };
const DSP = { slots: 0x257810, voice: 0x2412cc, voiceStride: 0x1d8 };

function toneWav(hz: number): Uint8Array {
  const frames = 480000;
  const wav = Buffer.alloc(44 + frames * 2);
  wav.write('RIFF');
  wav.writeUInt32LE(wav.length - 8, 4);
  wav.write('WAVEfmt ', 8);
  wav.writeUInt32LE(16, 16);
  wav.writeUInt16LE(1, 20);
  wav.writeUInt16LE(1, 22);
  wav.writeUInt32LE(48000, 24);
  wav.writeUInt32LE(96000, 28);
  wav.writeUInt16LE(2, 32);
  wav.writeUInt16LE(16, 34);
  wav.write('data', 36);
  wav.writeUInt32LE(frames * 2, 40);
  for (let n = 0; n < frames; n++)
    wav.writeInt16LE(Math.round(12000 * Math.sin((2 * Math.PI * hz * n) / 48000)), 44 + n * 2);
  return wav;
}

/** Column-major OLED: the top eight pixels are byte 7 of each column. */
function oledTitle(data: string | undefined): string {
  if (!data) throw new Error('SAMPLES navigation requires decoded OLED bytes');
  const raw = Buffer.from(data, 'base64');
  expect(raw.length).toBe(1024);
  return Buffer.from(raw.filter((_, index) => index % 8 === 7)).toString('hex');
}

/** Top pixel row of the inverted (white) bar in the action menu column, or null. A bar has two full white border rows. */
function actionBarTop(data: string | undefined): number | null {
  if (!data) throw new Error('action menu navigation requires decoded OLED bytes');
  const raw = Buffer.from(data, 'base64');
  for (let y = 0; y < 64; y++) {
    let white = 0;
    for (let x = 62; x < 122; x++) white += (raw[x * 8 + (y >> 3)]! >> (y & 7)) & 1;
    if (white === 60) return y;
  }
  return null;
}
// Top pixel rows of the SAMPLING action menu items (finding 82 frames): AUTO-PREVIEW 16, UNLOAD 24, REPLACE 32.
const ACTION = { unload: 24, replace: 32 };

/** Returns the title of the SAMPLES root menu as observed in this run. */
async function discovery(session: Session, dk: DkSession): Promise<{ out: string; rootTitle: string }> {
  const root = path.join(repoRoot(), 'out/runs/stock-sampler-lifetime');
  mkdirSync(root, { recursive: true });
  // Discovery evidence is retained even when this test passes.
  const out = mkdtempSync(path.join(root, 'dt2-'));
  const shared: { rootTitle?: string } = {};
  console.info('retained stock sampler discovery:', out);
  const ui = panel(session);
  const { cf, words, dsp } = memory(session);
  async function observe(name: string) {
    const begin = await cf(CF.blocks);
    const end = await cf(CF.blocks + 4);
    const slotBlocks = await words(CF.slotBlock, 8);
    const blocks = [];
    // Guard diagnostic dereferences using the observed vector bounds, not slot values alone.
    if (begin >= 0x40000000 && end >= begin && end <= 0x47ffffff && (end - begin) % 0x34 === 0) {
      for (const index of new Set(slotBlocks.map((v) => v | 0).filter((v) => v >= 0))) {
        const addr = begin + index * 0x34;
        if (addr + 0x34 <= end)
          blocks.push({ index, addr, words: await words(addr, 13), refcountCandidate: await cf(addr + 0x30) });
      }
    }
    const voices = await dsp(DSP.voice, DSP.voiceStride * 4);
    const slots = await dsp(DSP.slots, 8 * 0x14);
    const value = {
      note: 'Coarse sequential snapshots are not atomic and do not prove absence of reads or a quiescence barrier.',
      status: await session.status(),
      faults: await session.faults(),
      frame: await session.client.framebufferGet({ hash: true, data: true }),
      cf: {
        vectorBegin: begin,
        vectorEnd: end,
        slotBlocks,
        slotRefs: await words(CF.slotRef, 32),
        slotLengths: await words(CF.slotLength, 8),
        blocks,
      },
      dsp: {
        slots,
        voices,
        voiceLayout: { ...DSP, note: 'voice fields are candidates until correlated with observed selection' },
      },
    };
    record(out, name, value);
    return value;
  }
  const executed = {
    baseline: false,
    selectedSlotChange: false,
    transferredSamplesLoadedIntoProject: false,
    replaceReferencedSlot: false,
    unload: false,
    projectReload: false,
    startEndReverse: false,
    readerQuiescenceProven: false,
  };
  async function critical(name: string) {
    const voices = await dsp(DSP.voice, DSP.voiceStride * 2);
    expect(voices.unknown, 'critical voice bytes are known').toBe(0);
    const raw = Buffer.from(voices.hex, 'hex');
    const ptr = raw.readUInt32LE(0);
    const slot = await dsp(DSP.slots + 0x14, 20);
    const index = (await cf(CF.slotBlock + 4)) | 0;
    const begin = await cf(CF.blocks);
    const end = await cf(CF.blocks + 4);
    const addr = begin + index * 0x34;
    expect(index).toBeGreaterThanOrEqual(0);
    expect(begin).toBeGreaterThanOrEqual(0x40000000);
    expect(end).toBeLessThanOrEqual(0x47ffffff);
    expect(addr + 0x34).toBeLessThanOrEqual(end);
    const block = await words(addr, 13);
    const sourceHead = await dsp(ptr, 64);
    const value = {
      voices,
      ptr,
      activeCandidate: raw[0x1b8],
      phaseFraction: raw.readUInt32LE(0x1b0),
      phaseWhole: raw.readUInt32LE(0x1b4),
      slot,
      block: { index, addr, words: block, refcountCandidate: block[12] },
      sourceHead,
      status: await session.status(),
      note: 'Sequential bounded observations, not proof that all readers are retired.',
    };
    record(out, name, value);
    return value;
  }
  async function note(name: string) {
    const file = path.join(out, `${name}.wav`);
    await session.client.call('pcm.tap', { action: 'start' });
    await session.client.call('pcm.tap', { action: 'capture', path: file });
    try {
      await session.client.inputButton(button('TRIG 1'), true);
      await expect
        .poll(
          async () => ((await session.client.call('pcm.tap', { action: 'report' })) as { samples: number }).samples,
          { timeout: 60_000, interval: 250 },
        )
        .toBeGreaterThanOrEqual(96000);
    } finally {
      await session.client.inputButton(button('TRIG 1'), false);
      await session.client.call('pcm.tap', { action: 'capture_stop' });
    }
    const wav = readFileSync(file);
    expect(wav.toString('ascii', 0, 4)).toBe('RIFF');
    expect(wav.toString('ascii', 8, 16)).toBe('WAVEfmt ');
    expect(wav.readUInt16LE(20)).toBe(3);
    expect(wav.readUInt16LE(22)).toBe(2);
    expect(wav.readUInt32LE(24)).toBe(48000);
    expect(wav.readUInt16LE(34)).toBe(32);
    expect(wav.toString('ascii', 36, 40)).toBe('data');
    expect(wav.readUInt32LE(40)).toBe(wav.length - 44);
    let peak = 0;
    for (let at = 44; at < wav.length; at += 4) {
      const value = wav.readFloatLE(at);
      expect(Number.isFinite(value)).toBe(true);
      peak = Math.max(peak, Math.abs(value));
    }
    const result = {
      peak,
      frames: (wav.length - 44) / 8,
      sha256: createHash('sha256').update(wav.subarray(44)).digest('hex'),
    };
    record(out, `${name}-pcm`, result);
    await session.expectNoFault(name);
    expect(peak, 'stock note is audible').toBeGreaterThan(0.001);
    return result;
  }
  try {
    // Boot wait of the stock image (no install, no restart): the main UI of the firmware, as a state and not as a fixed time.
    await pollEmulated(session, async () => (await session.status()).cf.main_ui_reached, Boolean, {
      emulatedMs: 120_000,
      intervalMs: 500,
      message: 'the main UI of the stock image',
    });
    const identity = await session.status();
    record(out, 'identity', { load: session.loadResult, status: identity });
    expect(identity.firmware?.injected, 'unpatched stock image').toBe(false);
    expect(identity.firmware?.runtime).toBeNull();
    expect(session.loadResult.runtime).toBeFalsy();
    expect(identity.firmware?.syx_sha256).toBe('26c22f6652625ac2cfd47f7ee970d388ed8b6427dae3563c0a6a2f2d334350d5');
    const sample = await Sample.create(
      Uint8Array.from(readFileSync(path.join(repoRoot(), 'apps/web/public/dksample.wasm'))),
    );
    const backend = openSamples('stock-rpc', { session: dk, hashOf: (data) => sample.hash(data) });
    await backend.mkdir('stockproof');
    for (const [name, hz] of [
      ['a', 330],
      ['b', 990],
    ] as const) {
      const wav = toneWav(hz);
      writeFileSync(path.join(out, `${name}.wav`), wav);
      const prepared = prepareUpload(sample, `${name}.wav`, wav);
      await backend.put('stockproof', prepared);
      const verified = await backend.verify('stockproof', prepared);
      record(out, `transfer-${name}`, { hz, info: prepared.info, verified });
      expect(verified.ok).toBe(true);
    }
    await observe('boot-before-selection');
    await ui.tap('NO');
    await ui.chord('TRK', 'TRIG 1');
    await ui.chord('FUNC', 'SRC');
    for (let i = 0; i < 16; i++) await ui.tap('UP');
    await observe('stock-machine-list-first-row');
    await ui.tap('YES');
    await ui.settle(300);
    await ui.tap('YES');
    await ui.settle(300);
    await ui.tap('SRC');
    const before = await observe('baseline-before-note');
    expect(before.dsp.slots.unknown).toBe(0);
    expect(before.dsp.voices.unknown).toBe(0);
    const first = await note('baseline');
    const baseline = await observe('baseline-after-note');
    // Selection only. This does not unload, replace or reclaim the previous slot.
    await ui.turn('ENC D', 1);
    // The built card has duplicate sample references. Choose project slot 1
    // explicitly; a knob detent alone can land on another reference to the same file.
    for (let i = 0; i < 16; i++) await ui.tap('UP');
    await ui.tap('DOWN');
    await observe('selection-popup');
    await ui.tap('YES');
    await ui.settle(300);
    await observe('selected-slot-before-note');
    const second = await note('selected-slot');
    const selected = await observe('selected-slot-after-note');
    expect(selected.dsp.voices.unknown).toBe(0);
    const pointer = (snapshot: typeof selected) => Buffer.from(snapshot.dsp.voices.hex, 'hex').readUInt32LE(0);
    const slotOffset = Buffer.from(selected.dsp.slots.hex, 'hex').readUInt32LE(0x14);
    expect(pointer(selected), 'stored voice binding references selected project slot 1').toBe(
      (0x8422b9c8 + slotOffset) >>> 0,
    );
    expect(pointer(selected), 'selected slot has a different sample pointer').not.toBe(pointer(baseline));
    record(out, 'selection-evidence', {
      baselinePointer: pointer(baseline),
      selectedPointer: pointer(selected),
      selectedSlot: 1,
      slotOffset,
      first,
      second,
    });
    executed.baseline = true;
    executed.selectedSlotChange = true;
    await ui.chord('FUNC', 50); // DT2 SAMPLING: devices/digitakt-ii-leds.toml, not KEYBOARD (18).
    for (let i = 0; i < 4; i++) await ui.tap('UP');
    const samplesRoot = await observe('samples-menu');
    await ui.tap('DOWN'); // PROJECT RAM
    await ui.tap('YES');
    for (let i = 0; i < 16; i++) await ui.tap('UP');
    await ui.tap('DOWN'); // A001, not A000/OFF
    await ui.tap('RIGHT');
    for (let i = 0; i < 8; i++) await ui.tap('UP');
    for (let i = 0; i < 3; i++) await ui.tap('DOWN'); // REPLACE, not UNLOAD
    await observe('first-replace-action');
    await ui.tap('YES');
    for (let i = 0; i < 16; i++) await ui.tap('UP'); // +Drive root: FACTORY, stockproof, fixture files
    await ui.tap('DOWN');
    await ui.tap('YES'); // stockproof folder
    await observe('first-replace-folder');
    await ui.tap('YES'); // a -> A001
    const slotSource = async () => {
      const header = await dsp(DSP.slots + 0x14, 20);
      if (header.unknown) return null;
      const raw = Buffer.from(header.hex, 'hex');
      if (raw.readUInt32LE(8) !== 960000) return null;
      const ptr = (0x8422b9c8 + raw.readUInt32LE(0)) >>> 0;
      const head = await dsp(ptr, 64);
      return head.unknown ? null : { header, ptr, head };
    };
    const expectedA = Buffer.from(toneWav(330)).subarray(44, 108);
    const expectedB = Buffer.from(toneWav(990)).subarray(44, 108);
    await expect
      .poll(async () => (await slotSource())?.head.hex, { timeout: 30_000, interval: 50 })
      .toBe(expectedA.toString('hex'));
    executed.transferredSamplesLoadedIntoProject = true;
    await observe('a-ready');
    await ui.chord('FUNC', 50); // leave assignment browser
    // Re-establish the track and source binding explicitly, as in the kept
    // discovery. Closing the ASSIGN overlay alone need not leave its browser.
    await ui.chord('TRK', 'TRIG 1');
    await ui.tap('SRC');
    await ui.turn('ENC D', 1);
    for (let i = 0; i < 16; i++) await ui.tap('UP');
    await ui.tap('DOWN');
    await ui.tap('YES');
    await ui.settle(300);
    await observe('a-src-before-trigger');
    const capture = path.join(out, 'active-a-to-b.wav');
    await session.client.call('pcm.tap', { action: 'start' });
    await session.client.call('pcm.tap', { action: 'capture', path: capture });
    try {
      await ui.tap('TRIG 1', 0);
      const early = await critical('a-active-early');
      await ui.settle(100);
      const advancing = await critical('a-active-after100ms');
      expect(early.activeCandidate).toBe(1);
      expect(advancing.activeCandidate).toBe(1);
      expect(advancing.phaseWhole).toBeGreaterThan(early.phaseWhole);
      expect(early.sourceHead.unknown).toBe(0);
      expect(Buffer.from(early.sourceHead.hex, 'hex')).toEqual(expectedA);
      await ui.chord('FUNC', 50);
      // The stock UI may reopen at the root or remember a directory. Return
      // only as far as this run's observed SAMPLES root; blind NOs can close it.
      const rootTitle = oledTitle(samplesRoot.frame.data_b64);
      shared.rootTitle = rootTitle;
      let atRoot = false;
      for (let depth = 0; depth < 6; depth++) {
        const frame = await session.client.framebufferGet({ hash: true, data: true });
        record(out, `samples-root-route-${depth}`, frame);
        if (oledTitle(frame.data_b64) === rootTitle) {
          atRoot = true;
          break;
        }
        await ui.tap('NO');
      }
      expect(atRoot, 'returned to the observed SAMPLES root before PROJECT RAM').toBe(true);
      for (let i = 0; i < 4; i++) await ui.tap('UP');
      await ui.tap('DOWN');
      await ui.tap('YES'); // PROJECT RAM
      for (let i = 0; i < 16; i++) await ui.tap('UP');
      await ui.tap('DOWN');
      await ui.tap('RIGHT');
      for (let i = 0; i < 8; i++) await ui.tap('UP');
      for (let i = 0; i < 3; i++) await ui.tap('DOWN');
      await ui.tap('YES'); // REPLACE browser remembers stockproof/a
      await ui.tap('DOWN'); // b
      record(out, 'b-highlight', await session.client.framebufferGet({ hash: true, data: true }));
      const beforeCommit = await critical('before-b-commit');
      expect(beforeCommit.activeCandidate, 'old note is still active immediately before replacement').toBe(1);
      expect(beforeCommit.block.refcountCandidate, 'a is a unique reference in this controlled project').toBe(1);
      expect(Buffer.from(beforeCommit.sourceHead.hex, 'hex')).toEqual(expectedA);
      await ui.tap('YES', 0);
      const afterCommit = await critical('after-b-commit');
      await ui.settle(100);
      const after100ms = await critical('after-b-additional100ms');
      // Equal-length A and B cannot be distinguished by a ready length alone.
      // Resolve the new slot view independently of any retained old voice pointer.
      await expect
        .poll(async () => (await slotSource())?.head.hex, { timeout: 30_000, interval: 50 })
        .toBe(expectedB.toString('hex'));
      const readySlot = await slotSource();
      expect(readySlot).not.toBeNull();
      record(out, 'b-ready-slot', readySlot);
      const ready = await critical('b-ready');
      record(out, 'replacement-observation', {
        beforeCommit,
        afterCommit,
        after100ms,
        ready,
        retainedActiveAddressWithChangedBytes: [afterCommit, after100ms, ready].some(
          (v) =>
            v.ptr === beforeCommit.ptr &&
            v.activeCandidate === 1 &&
            v.phaseWhole > beforeCommit.phaseWhole &&
            v.sourceHead.unknown === 0 &&
            v.sourceHead.hex === expectedB.toString('hex'),
        ),
        note: 'Observed snapshots do not establish a universal ordering guarantee or hardware behavior.',
      });
      executed.replaceReferencedSlot = true;
      await ui.settle(500);
    } finally {
      await session.client.call('pcm.tap', { action: 'capture_stop' });
    }
    await ui.chord('FUNC', 50);
    await note('b-new-trigger');
    const triggered = await critical('b-new-trigger-binding');
    const currentSlot = await slotSource();
    expect(currentSlot).not.toBeNull();
    expect(triggered.ptr, 'new trigger binds the published B slot').toBe(currentSlot!.ptr);
    expect(triggered.sourceHead.unknown).toBe(0);
    expect(triggered.sourceHead.hex).toBe(expectedB.toString('hex'));
    await session.expectNoFault('stock selection and replacement discovery');
  } finally {
    record(out, 'executed', executed);
    await observe('final-state');
  }
  return { out, rootTitle: shared.rootTitle! };
}

/** Pool state of one project slot: map index, block record, DSP slot record and the first 64 PCM bytes. */
async function slotState(session: Session, slot: number) {
  const { cf, words, dsp } = memory(session);
  const index = (await cf(CF.slotBlock + slot * 4)) | 0;
  const begin = await cf(CF.blocks);
  const block = index >= 0 ? await words(begin + index * 0x34, 13) : null;
  const header = await dsp(DSP.slots + slot * 0x14, 20);
  expect(header.unknown).toBe(0);
  const raw = Buffer.from(header.hex, 'hex');
  const ptr = (0x8422b9c8 + raw.readUInt32LE(0)) >>> 0;
  const head = raw.readUInt32LE(8) === 0 ? null : await dsp(ptr, 64);
  if (head) expect(head.unknown).toBe(0);
  return {
    index,
    offset: block?.[4] ?? null, // record +0x10
    size: block?.[5] ?? null, // record +0x14
    sharers: block?.[12] ?? null, // record +0x30
    dspRecord: header.hex,
    ptr,
    pcmHead: head?.hex ?? null,
  };
}

/**
 * Continues from the end of discovery: slot 1 holds b (earlier block record). Load a into slot 2 (later record),
 * play slot 2, then UNLOAD slot 1 through the stock SAMPLING menu. The block vector shifts only its 0x34-byte
 * records; PCM stays in place.
 */
async function unloadEarlierWhilePlaying(session: Session, out: string, rootTitle: string): Promise<void> {
  const ui = panel(session);
  const { dsp } = memory(session);
  const expectedA = Buffer.from(toneWav(330)).subarray(44, 108).toString('hex');
  const expectedB = Buffer.from(toneWav(990)).subarray(44, 108).toString('hex');
  const shot = async (name: string) =>
    record(out, `unload-${name}`, await session.client.framebufferGet({ hash: true, data: true }));
  let shotCount = 0;
  const grab = async () => {
    const frame = await session.client.framebufferGet({ hash: true, data: true });
    record(out, `unload-frame-${shotCount++}`, frame);
    return frame.data_b64 ?? '';
  };
  /** NO goes up one level of the SAMPLING pages until the SAMPLES root menu shows. */
  async function noUntilRoot(): Promise<boolean> {
    for (let i = 0; i < 6; i++) {
      if (oledTitle(await grab()) === rootTitle) return true;
      await ui.tap('NO');
    }
    return false;
  }
  /** From the track page FUNC+SAMPLING opens the pages where they were left; the first call starts inside them. */
  async function openProjectRam(fromTrackPage: boolean): Promise<void> {
    if (fromTrackPage) {
      await ui.chord('FUNC', 50);
      await ui.settle(1000); // the SAMPLING pages draw slowly
    }
    expect(await noUntilRoot(), 'SAMPLES root reached').toBe(true);
    for (let i = 0; i < 4; i++) await ui.tap('UP');
    await ui.tap('DOWN');
    await ui.tap('YES'); // PROJECT RAM
  }
  /** Back to the track page: one more NO from the root menu. */
  async function leaveSamples(): Promise<void> {
    if (await noUntilRoot()) await ui.tap('NO');
  }
  async function openSlotActions(slot: number, action: number): Promise<void> {
    for (let i = 0; i < 16; i++) await ui.tap('UP');
    for (let i = 0; i < slot; i++) await ui.tap('DOWN'); // A00<slot>
    await ui.tap('RIGHT');
    // The action menu keeps its last cursor row: move the inverted bar to the wanted row.
    for (let i = 0; i < 12; i++) {
      const top = actionBarTop(await grab());
      if (top === action) return;
      await ui.tap(top !== null && top > action ? 'DOWN' : 'UP'); // lists run bottom-up: DOWN moves the bar up
    }
    throw new Error(`action row ${action} not reached`);
  }
  const before1 = await slotState(session, 1);
  expect(before1.pcmHead, 'slot 1 holds b').toBe(expectedB);

  // Load a into slot 2.
  await openProjectRam(false);
  await openSlotActions(2, ACTION.replace);
  await shot('slot2-replace-action');
  await ui.tap('YES');
  await shot('slot2-browser');
  await ui.tap('UP'); // the browser opens on the slot's current file at the +Drive root; lists run bottom-up: UP is stockproof
  await ui.tap('YES');
  await shot('slot2-browser-folder'); // a is the first row
  await ui.tap('YES');
  await expect
    .poll(async () => (await slotState(session, 2)).pcmHead, { timeout: 30_000, interval: 50 })
    .toBe(expectedA);
  await leaveSamples();

  // Play slot 2.
  await ui.chord('TRK', 'TRIG 1');
  await ui.tap('SRC');
  await shot('play-src-page');
  await ui.turn('ENC D', 1);
  await shot('play-after-turn');
  await ui.tap('UP');
  await shot('play-after-first-up');
  for (let i = 0; i < 15; i++) await ui.tap('UP');
  await shot('play-after-16-up');
  for (let i = 0; i < 2; i++) await ui.tap('DOWN'); // A002
  await shot('slot2-popup');
  await ui.tap('YES');
  await ui.settle(300);
  await ui.tap('TRIG 1', 0);
  const voice = async () => {
    const reply = await dsp(DSP.voice, DSP.voiceStride);
    expect(reply.unknown).toBe(0);
    return Buffer.from(reply.hex, 'hex');
  };
  const slot2Before = await slotState(session, 2);
  const slot1Before = await slotState(session, 1);
  // The voice binds the slot at note start; the earlier note's pointer can still be there for a moment.
  await expect
    .poll(async () => (await voice()).readUInt32LE(0), { timeout: 10_000, interval: 50 })
    .toBe(slot2Before.ptr);
  const v0 = await voice();
  expect(v0.readUInt32LE(0), 'voice reads slot 2').toBe(slot2Before.ptr);
  expect(v0[0x1b8], 'voice active').toBe(1);
  await ui.settle(100);
  const v1 = await voice();
  expect(v1.readUInt32LE(0x1b4), 'cursor advances').toBeGreaterThan(v0.readUInt32LE(0x1b4));

  // UNLOAD slot 1 while slot 2 plays.
  await openProjectRam(true);
  await openSlotActions(1, ACTION.unload);
  await shot('slot1-unload-action');
  await ui.tap('YES');
  await shot('slot1-unload-confirm'); // CONFIRM Y/N UNLOAD 1 SAMPLE?
  await ui.tap('YES');
  await ui.settle(500);
  await shot('after-unload');
  const slot1After = await slotState(session, 1);
  const slot2After = await slotState(session, 2);
  const v2 = await voice();
  record(out, 'unload-observation', {
    slot1Before,
    slot2Before,
    slot1After,
    slot2After,
    voice: [v0, v1, v2].map((v) => v.subarray(0, 8).toString('hex')),
    cursor: [v0, v1, v2].map((v) => v.readUInt32LE(0x1b4)),
  });
  expect(slot1After.index, 'slot 1 is empty').toBe(-1);
  // Release removes only the 0x34-byte record of slot 1, so a record after it moves down by one.
  expect(slot2After.index, 'record index').toBe(slot2Before.index - (slot1Before.index < slot2Before.index ? 1 : 0));
  expect(slot2After.offset, 'pool offset unchanged').toBe(slot2Before.offset);
  expect(slot2After.dspRecord, 'DSP slot record unchanged').toBe(slot2Before.dspRecord);
  expect(slot2After.pcmHead, 'PCM head unchanged').toBe(slot2Before.pcmHead);
  expect(v2.readUInt32LE(0), 'voice still reads slot 2').toBe(slot2Before.ptr);
  await session.expectNoFault('unload earlier slot while a later slot plays');
}

const binary = process.env.DIGI_EMUD_DT2;
if (!binary) throw new Error('DIGI_EMUD_DT2 must name the preflighted DT2 daemon');
const handle = withSession({
  device: 'dt2',
  firmware: '1.17',
  dsp: 'sharc',
  binary,
  card: path.join(repoRoot(), 'out/runs/patchkit/samples'),
  restart: false,
});
test('stock DT2 slot binding and active ten-second replacement discovery', async () => {
  const session = handle.session;
  await session.client.call('run.mode', { mode: 'free' });
  const dk = new DkSession(new EmuUsbTransport((method, params) => session.client.call(method, params)));
  try {
    const { out, rootTitle } = await discovery(session, dk);
    await unloadEarlierWhilePlaying(session, out, rootTitle);
  } finally {
    dk.cancelPending();
  }
}, 360_000);
