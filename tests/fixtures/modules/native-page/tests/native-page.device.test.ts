import { expect } from 'vitest';
import fs from 'node:fs';
import path from 'node:path';
import { record } from '@digi/regression/runtime/artifacts.ts';
import { shot } from '@digi/regression/runtime/fx-session.ts';
import { portFile } from '@digi/regression/runtime/device-data.ts';
import { installModule, moduleArtifact } from '@digi/regression/runtime/install.ts';
import { memory, panel } from '@digi/regression/runtime/panel.ts';
import { prepareRuntimePlan } from '@digi/regression/runtime/runtime-plan.ts';
import { expectRuntimeReady, runtimeTests } from '@digi/regression/runtime/ready.ts';

runtimeTests(
  'native callback page cycles and routes PARAM and LOCAL turns',
  async ({ session, dk }) => {
    await expectRuntimeReady(session);
    await installModule(dk, 'test/native-page', moduleArtifact(session.options.device, 'test/native-page'));
    const ui = panel(session);
    const artifacts = path.join(session.runDir, 'native-page-proof');
    fs.mkdirSync(artifacts, { recursive: true });
    const plan = await prepareRuntimePlan(session, artifacts, ['mf_native_register']);
    function machineSymbol(name: string) {
      const matches = Object.entries(plan.symbols).filter(
        ([key]) => key.startsWith('core/machines@') && key.endsWith(`:${name}`),
      );
      expect(matches).toHaveLength(1);
      return matches[0]![1];
    }
    const mem = memory(session);
    const source = session.options.device === 'dt2' ? 'SRC' : 'SYN';
    async function choose(position: number) {
      await ui.tap('NO');
      await ui.chord('FUNC', source);
      for (let i = 0; i < 16; i++) await ui.tap('UP');
      for (let i = 0; i < position; i++) await ui.tap('DOWN');
      await ui.tap('YES');
      await ui.tap('YES');
    }
    async function observed() {
      const reply = await dk.request(0x3e);
      expect(reply.status).toBe(0);
      // 7 words: draws, events, track, start, end, zoom, level seen.
      expect(reply.body).toHaveLength(28);
      const values = Buffer.from(reply.body);
      return {
        draws: values.readUInt32BE(0),
        events: values.readUInt32BE(4),
        track: values.readUInt32BE(8),
        start: values.readInt32BE(12),
        end: values.readInt32BE(16),
        zoom: values.readInt32BE(20),
      };
    }
    await ui.chord('TRK', 'TRIG 1');
    const position = session.options.device === 'dt2' ? 7 : 5;
    await choose(position);
    await shot(session, artifacts, 'page-zero');
    await ui.tap(source);
    // Depending on the menu's restored page, one further source press reaches page one.
    if ((await observed()).draws === 0) await ui.tap(source);
    const before = await observed();
    record(artifacts, 'page-one-before', before);
    const owner = (await dk.modules())[0]!.types[0]!;
    const operand = portFile(session.options.device, session.options.firmware).anchor!.find(
      (anchor) => anchor.name === 'cf.page.data.native_font_operand',
    )!.addr!;
    const font = await mem.cf(operand);
    record(artifacts, 'native-binding-evidence', {
      owner,
      page: await mem.words(machineSymbol('pages') + owner * 184, 46),
      font,
      fontWords: await mem.words(font, 5),
      pixels: await mem.words(machineSymbol('pixels0'), 32),
    });
    await shot(session, artifacts, 'page-one');
    expect(before.draws, 'custom page draw callback ran').toBeGreaterThan(0);
    expect(before.track).toBe(0);
    await ui.burst('ENC A', 30, 4, 2);
    await ui.settle(300); // the panel tick drains its accumulated delta in bounded chunks
    const left = await observed();
    record(artifacts, 'after-A', left);
    expect(left.start, 'A edits START through the native path').toBeGreaterThan(before.start);
    expect(left.end).toBe(before.end);
    await ui.burst('ENC D', 30, -4, 2);
    await ui.settle(300);
    const right = await observed();
    record(artifacts, 'after-D', right);
    expect(right.end, 'D edits the existing END word').toBeLessThan(left.end);
    expect(right.start).toBe(left.start);
    await ui.turn('ENC C', 4);
    const local = await observed();
    record(artifacts, 'page-one-after-turns', { before, left, right, local });
    await shot(session, artifacts, 'page-one-after-turns');
    expect(local.events - right.events, 'one LOCAL callback per turn').toBe(1);
    expect(local.zoom - right.zoom).toBe(4);
    expect([local.start, local.end]).toEqual([right.start, right.end]);
    await ui.turn('ENC B', 4);
    const unused = await observed();
    expect([unused.start, unused.end, unused.zoom, unused.events]).toEqual([
      local.start,
      local.end,
      local.zoom,
      local.events,
    ]);
    // Page zero retains its ordinary mapping (DT2 reserves A for TUNE); cycling restores saved words.
    await ui.tap(source);
    await ui.burst(session.options.device === 'dt2' ? 'ENC C' : 'ENC B', 30, -4, 2);
    await ui.settle(300);
    await ui.tap(source);
    const normal = await observed();
    expect(normal.end).toBeLessThan(local.end);
    expect(normal.zoom).toBe(local.zoom);
    for (let i = 0; i < 2; i++) {
      await ui.chord('TRK', 'TRIG 2');
      await choose(position);
      await ui.tap(source);
      if ((await observed()).track !== 1) await ui.tap(source);
      expect((await observed()).track, 'new track reaches its custom page').toBe(1);
      await ui.chord('TRK', 'TRIG 1');
      await ui.tap(source);
      if ((await observed()).track !== 0) await ui.tap(source);
      expect((await observed()).track).toBe(0);
    }
    await choose(0);
    const stock = await observed();
    await ui.tap(source);
    await ui.turn('ENC A', 1);
    expect((await observed()).events).toBe(stock.events);
    await choose(position);
    await ui.tap(source);
    if ((await observed()).draws === stock.draws) await ui.tap(source);
    expect((await observed()).draws).toBeGreaterThan(stock.draws);
    await session.expectNoFault('native callback page');
  },
  { timeout: 240_000 },
);
