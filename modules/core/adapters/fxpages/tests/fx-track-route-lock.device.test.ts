import path from 'node:path';
import { button, encoder } from '@digi/test-support';
import { expect, test } from 'vitest';
import { advance, analysisJson, bounded, rpc } from '@digi/regression/runtime/fx-live.ts';
import { FX, SINE } from '@digi/regression/runtime/fixtures.ts';
import { shot, tap, selectSine } from '@digi/regression/runtime/fx-session.ts';
import { amountShown, setAmount } from '@digi/regression/runtime/fx-track-ui.ts';
import { record } from '@digi/regression/runtime/artifacts.ts';
import { trackRouteSuite } from './fx-track-route-support.ts';

// Track FX send page, one function of the former composite fx-track-route test (setup: fx-track-route-support.ts).

trackRouteSuite('lock', { initial: 'send', presets: ['send'], base: false }, (t) => {
  const { device, context, locks, pageTwo } = t;
  const boot = t.booter('send');
  test('project save controls stay ordered: base, none, amount, send', async () => {
        for (const turn of ['base', 'none', 'amount', 'send'] as const) {
          // Project save controls stay ordered: base, none, amount, send.
          console.info('track FX leg:', `project save control: ${turn}`);
          {
            const { runtime } = await boot(`lock-${turn}`, [SINE, FX], 'send');
            const { session } = runtime;
            const { artifacts, profile } = context;
            await selectSine(runtime, profile);
            await tap(session, 'NO', 300);
            if (turn === 'send' || turn === 'amount') await pageTwo(runtime);
            await shot(session, artifacts, `lock-${turn}-page`);
            const held = turn === 'none' || turn === 'send';
            let cc: { hex: string };
            try {
              if (held) {
                await tap(session, 'RECORD', 200);
                await tap(session, 'TRIG 1', 200);
                await bounded(() => session.client.inputButton(button('TRIG 1'), true));
                await advance(session, 100);
              }
              await rpc(session, 'midi.out');
              if (turn === 'amount') await setAmount(session, profile, 'ENC A', 0, 40);
              else if (turn === 'send') {
                await bounded(() => session.client.inputTurn(encoder('ENC A'), 11));
                await advance(session, 10);
              }
              await advance(session, 300);
              await shot(session, artifacts, `lock-${turn}-turned`);
              cc = await rpc<{ hex: string }>(session, 'midi.out');
            } finally {
              if (held) await bounded(() => session.client.inputButton(button('TRIG 1'), false));
            }
            if (held) {
              await advance(session, 300);
              await tap(session, 'RECORD', 300);
            }
            const shown = await amountShown(session, profile, 0);
            await tap(session, 'SETTINGS', 400, 'FUNC');
            await tap(session, 'YES', 700);
            await tap(session, 'YES', 500);
            await advance(session, 3500);
            const overlay = path.join(artifacts, `lock-${turn}.overlay.bin`);
            await rpc(session, 'card.overlay', { path: overlay });
            locks.set(turn, { overlay, cc: cc!.hex, shown });
            record(artifacts, `lock-${turn}-result`, { overlay, cc: cc!.hex, shown });
            if (turn === 'send') {
              const result = await analysisJson<{
                trig_changed_bytes: number;
                send_changed_bytes: number;
                amount_diff: { differ_bytes: Record<string, number> };
              }>(
                'tools.patchkit.pk_digikit_fxroute',
                [
                  'lock-check',
                  '--device',
                  device,
                  '--card',
                  context.baseline('send').cardImage!,
                  ...['base', 'none', 'amount', 'send'].flatMap((tag) => [`--${tag}`, locks.get(tag)!.overlay]),
                ],
                artifacts,
                'lock-project-byte-oracles',
              );
              expect(result.trig_changed_bytes, 'panel script records a trig: method control').toBeGreaterThan(0);
              // The legacy script could not produce a stock-knob lock control. Preserve
              // its bounded claim: same pattern bytes, supported by the class flag above.
              expect(result.send_changed_bytes, 'held send turn saves no lock bytes').toBe(0);
              expect(locks.get('amount')!.shown).toBe(40);
              expect(
                (result.amount_diff.differ_bytes.kit ?? 0) + (result.amount_diff.differ_bytes.pool ?? 0),
                'amount saves no kit or Sound record bytes',
              ).toBe(0);
              expect(locks.get('send')!.cc, 'send knob emits no MIDI CC').toBe('');
            }
          }
        }
  }, 600_000);
});
