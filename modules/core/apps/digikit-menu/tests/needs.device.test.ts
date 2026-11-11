import { expect, test } from 'vitest';
import { LOG_KIND } from '@digi/dk-session/gen_proto';
import { loadFailureText, loadFailures } from '@digi/dk-session';
import { cardRigs, RT } from '@digi/regression/runtime/card-rig.ts';
import { advance } from '@digi/regression/runtime/fx-live.ts';
import { tap } from '@digi/regression/runtime/fx-session.ts';
import { openDigikitMenu, ofKind, readAboutRows, readSmallRing } from '@digi/regression/runtime/runtime-menu.ts';
import { planSymbol } from '@digi/regression/runtime/runtime-plan.ts';

/**
 * A module whose imports belong to a module that is not in the image says which one (docs/plans/sdk-runtime-roadmap.md, "show why a module did not load").
 * The image has the runtime and the menu but not core/usb-bulk, and usb-fast is selected: its load ends with E_IMPORT and the runtime keeps `usb-bulk` in
 * DkrtBootInfo.needs. The name reaches the LOADS command (the Transfer pane), the MOD_NEEDS event, the BOOT page (`- NEEDS USB-BULK`) and the side panel
 * of the module's row in MODULES (the detail line of a row that shows a dash).
 */

const SYMBOLS = ['DK_ABOUT', 'DK_ABOUT_N', 'DK_PANEL', 'DK_PANEL_N'];
const BOOT_NEEDS_OFFSET = 80; // DkrtBootInfo.needs (modules/core/runtime/abi.toml)
const NEEDS_LEN = 12;

cardRigs('a module that needs a missing module', { dsp: 'sharc', roots: [...RT, 'core/digikit-menu'], symbols: SYMBOLS }, (rig) => {
  test('usb-fast without core/usb-bulk: the missing module is stored, logged, in LOADS, on the BOOT page and in the module row', async () => {
    await rig.first();
    const mod = rig.mod('digikit/usb-fast');
    const rt = await rig.load('needs', { modules: [mod], selection: [mod.name] });
    const { session } = rt;
    const peek = async (address: number) => (await session.client.memPeek(address))! >>> 0;
    const text = async (symbol: string, i: number, stride = 24) => {
      const bytes = Buffer.alloc(stride);
      for (let k = 0; k < stride; k += 4) bytes.writeUInt32BE(await peek(planSymbol(rig.plan, symbol) + stride * i + k), k);
      return bytes.toString('latin1').split('\0')[0]!;
    };

    // the runtime's record of the boot: slot 0 holds the name
    const needs = Buffer.alloc(NEEDS_LEN);
    for (let k = 0; k < NEEDS_LEN; k += 4)
      needs.writeUInt32BE(await peek(planSymbol(rig.plan, 'DKRT_BOOT') + BOOT_NEEDS_OFFSET + k), k);
    expect(needs.toString('latin1').split('\0')[0], 'DkrtBootInfo.needs[0]').toBe('usb-bulk');

    // the event after MOD_LOAD (error level), and the host's reading of it
    const ring = await readSmallRing(session, rig.plan);
    const failures = loadFailures(ring);
    expect(failures).toEqual([{ name: 'usb-fast.dkm', result: -7, needs: 'usb-bulk' }]);
    expect(ofKind(ring, LOG_KIND.MOD_NEEDS)).toHaveLength(1);
    expect(loadFailureText(failures[0]!)).toBe('usb-fast not loaded: this firmware has no core/usb-bulk. Flash firmware from the Firmware page.');

    // the LOADS command (no debug module needed)
    expect(await rig.usb(rt, (dk) => dk.loadFailures())).toEqual(failures);

    // SETTINGS > DIGIKIT > ABOUT (the first row): the BOOT lines
    await openDigikitMenu(async (name, settle) => tap(session, name, Math.min(settle ?? 300, 800)));
    await tap(session, 'YES', 1500);
    await expect
      .poll(async () => (await advance(session, 100), (await readAboutRows(session, rig.plan)).some((row) => row === '- NEEDS USB-BULK')), {
        timeout: 60_000,
        interval: 10,
      })
      .toBe(true);
    expect((await readAboutRows(session, rig.plan)).join('\n')).not.toContain('NO IMPORT');

    // BACK, then MODULES (the fourth row): RIGHT on the dashed row shows the detail line last in the side panel
    await tap(session, 'NO', 800);
    for (let i = 0; i < 3; i++) await tap(session, 'DOWN', 200);
    await tap(session, 'YES', 1500);
    await tap(session, 'RIGHT', 1000);
    await expect
      .poll(async () => (await advance(session, 100), await text('DK_PANEL', 2)), { timeout: 60_000, interval: 10 })
      .toBe('NEEDS USB-BULK');
    await session.expectNoFault('usb-fast without core/usb-bulk');
  });
});
