import path from 'node:path';
import { runtimeEvent } from '@digi/dk-session';
import { LOG_KIND } from '@digi/dk-session/gen_proto';
import { button, encoder } from '@digi/test-support';
import { expect, test } from 'vitest';
import { bootInfoLoaded } from '@digi/regression/runtime/card-kit.ts';
import { CARD_SYMBOLS, cardRigs, RT } from '@digi/regression/runtime/card-rig.ts';
import { CARD_TIMEOUT, pausedProtocol } from '@digi/regression/runtime/debug-kit.ts';
import { advance, frames, pickSine, readFloatWav, rpc, windowRms } from '@digi/regression/runtime/fx-live.ts';
import { tap } from '@digi/regression/runtime/fx-session.ts';
import { ofKind } from '@digi/regression/runtime/runtime-menu.ts';

/**
 * The runtime guard of core/dsp-voice (full tier; the Python scenario debug-guard is deleted). The NaN fixture
 * (test/dyn-guard-nan) outputs NaN while its voice is active. Ten presses in a second: the guard zeroes each block, counts it, logs MACHINE_FAULT and switches the
 * type off after the 8th (MACHINE_DISABLED); the master output holds no NaN or Inf.
 * DT2: the fixture is on track 2 with its delay send up, a run-time sine on track 1 stays audible. DN2: the fixture (an own_dsp machine) is on track 1.
 */

/** The list position (`downs`) of the run-time machines on the SRC list; the fixture follows the sine on the DT2. */
const HINT = { sine: 7, nan: 8, dn2: 5 };
const FAULT_LIMIT = 8;


cardRigs('debug guard', { full: true, uploads: true, roots: RT, symbols: CARD_SYMBOLS }, (h) => {
  test(
    'the guard zeroes a faulting machine, switches its type off after 8 faults and the output stays finite',
    async () => {
      const dt2 = h.device === 'dt2';
      const nan = h.mod('test/dyn-guard-nan', 'nan');
      // the debug module last: the machine rows stay where they were; the log needs it (MACHINE_CHANGE is its)
      const mods = [...(dt2 ? [h.mod('test/sine', 'sine')] : []), nan, h.mod('digikit/debug', 'debug')];
      const runtime = await h.boot(await h.card({ modules: mods, selection: mods.map((m) => m.name) }));
      const { session } = runtime;
      await rpc(session, 'run.mode', { mode: 'paused' });
      const info = await bootInfoLoaded(session, h.plan); // the DSP part of a module follows its ColdFire part
      expect(info.count, 'every module and the debug module loaded at boot').toBe(mods.length);
      expect(info.results).toEqual(mods.map(() => 0));

      const log = () => pausedProtocol(runtime, (dk) => dk.readLog());
      const uiFirst = ofKind(await log(), LOG_KIND.UI_FIRST)[0]!.args[0]!;
      for (let i = 0; i < 60 && (await frames(session)) < uiFirst + 3300; i++) await advance(session, 500);

      const types = new Map((await pausedProtocol(runtime, (dk) => dk.modules())).map((m) => [m.name, m.types[0]!]));
      const tNan = types.get('test/dyn-guard-nan')!;
      const pick = async (track: number, want: number, hint: number) => {
        if (track > 1) await tap(session, `TRIG ${track}`, 300, 'TRK');
        for (const downs of dt2 ? [hint, hint + 1, hint - 1] : [hint]) {
          await pickSine(session, downs);
          await advance(session, 800);
          const changed = ofKind(await log(), LOG_KIND.MACHINE_CHANGE).some(
            (e) => e.args[0] === track - 1 && e.args[2] === want,
          );
          if (changed) return;
        }
        throw new Error(`no list position gives type ${want} on track ${track}`);
      };
      if (dt2) {
        await pick(1, types.get('test/sine')!, HINT.sine);
        await pick(2, tNan, HINT.nan);
        // track 2's delay send up: FX page, encoder E
        await tap(session, 'FX', 300);
        for (let i = 0; i < 4; i++) {
          await session.client.inputTurn(encoder('ENC E'), 1);
          await advance(session, 30);
        }
      } else await pick(1, tNan, HINT.dn2);

      const wav = path.join(session.runDir, 'guard.wav');
      await rpc(session, 'pcm.tap', { action: 'start' });
      await rpc(session, 'pcm.tap', { action: 'capture', path: wav });
      if (dt2) await session.client.inputButton(button('TRIG 1'), true); // the sine, held
      await advance(session, 400);
      const key = dt2 ? 'TRIG 2' : 'TRIG 1';
      for (let i = 0; i < 10; i++) {
        await tap(session, key, 0); // ten presses of the NaN machine in about a second
        await advance(session, 100);
      }
      await advance(session, 1500); // a poll of the guard's words is a second apart
      await rpc(session, 'pcm.tap', { action: 'capture_stop' });
      if (dt2) await session.client.inputButton(button('TRIG 1'), false);
      await advance(session, 2500);

      const events = await log();
      const faults = ofKind(events, LOG_KIND.MACHINE_FAULT);
      const disabled = ofKind(events, LOG_KIND.MACHINE_DISABLED);
      expect(faults.length, 'MACHINE_FAULT is logged').toBeGreaterThanOrEqual(1);
      for (const e of faults) expect(e.flags & 1, 'an error event').toBe(1);
      const last = runtimeEvent(faults.at(-1)!).fields;
      expect(last.type, "the fixture's type").toBe(tNan);
      expect(last.kind).toBe('nonfinite');
      if (dt2) expect(last.track, 'voice 2 (track 2)').toBe(2);
      expect(
        Number(last.faults),
        'the faults are counted: the type is switched off at the 8th within a second',
      ).toBeGreaterThanOrEqual(FAULT_LIMIT);
      expect(disabled, 'MACHINE_DISABLED is logged once').toHaveLength(1);
      expect(disabled[0]!.args[0], "for the fixture's type").toBe(tNan);
      expect(disabled[0]!.flags & 1).toBe(1);

      await tap(session, key, 0); // no fault after the switch-off: a press now changes nothing
      await advance(session, 2500);
      expect(
        ofKind(await log(), LOG_KIND.MACHINE_FAULT),
        'a press after the switch-off causes no new fault',
      ).toHaveLength(faults.length);

      const samples = readFloatWav(wav);
      expect(samples.length).toBeGreaterThan(48_000);
      expect(samples.every(Number.isFinite), 'the master output stays finite (no NaN or Inf in the capture)').toBe(
        true,
      );
      if (dt2) {
        const rms = windowRms(samples);
        expect(rms.length).toBeGreaterThanOrEqual(20);
        expect(
          Math.min(...rms.slice(0, 11)),
          'the sine on track 1 stays audible while the fixture faults (every 100 ms window of the first 1.1 s above -70 dBFS)',
        ).toBeGreaterThan(10 ** (-70 / 20));
      } else {
        const fixture = (await pausedProtocol(runtime, (dk) => dk.modules())).find(
          (m) => m.name === 'test/dyn-guard-nan',
        )!;
        expect(fixture, 'MODULES shows the fixture and that the guard switched it off').toMatchObject({
          types: [tNan],
          guard_off: true,
        });
        expect(fixture.faults).toBeGreaterThanOrEqual(FAULT_LIMIT);
      }
    },
    CARD_TIMEOUT,
  );
});
