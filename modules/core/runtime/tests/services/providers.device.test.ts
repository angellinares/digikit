import fs from 'node:fs';
import path from 'node:path';
import { LOG_KIND } from '@digi/dk-session/gen_proto';
import { button, emulatorClock, MS, repoRoot, resolveInputs, waitClock } from '@digi/test-support';
import { expect, test } from 'vitest';
import type { PatchedSession } from '@digi/regression/support.ts';
import { CARD_SYMBOLS, cardRigs, RT, type CardRig } from '@digi/regression/runtime/card-rig.ts';
import { FIRMWARE } from '@digi/regression/runtime/devices.ts';
import { moduleArtifact } from '@digi/regression/runtime/install.ts';
import { loadCode } from '@digi/regression/runtime/load-states.ts';
import { patchkit } from '@digi/regression/runtime/patchkit.ts';
import { ofKind } from '@digi/regression/runtime/runtime-menu.ts';
import { entry, nameHash, patched, rehash } from '../refusal/dkm-patch.ts';

/**
 * Providers (docs/plans/runtime-and-services.md section 13.2, R12-3): the selection [debug, svc-test-user] needs the service `svc-test`, which is not the
 * runtime's. The boot finds `services/svc-test.dks` in the store, loads it before the user, and installs its hook at `cf.menu.hook.settings_list.tail` (the last
 * step of the SETTINGS list rebuild) after its `init`. The user calls `dk_svctest_ping` (42) and logs it, and logs how often the hook ran. The provider file is
 * packed unbound; the test binds it to the STOCK firmware of the device with `patchkit bind` (the bound file holds firmware bytes: it goes under dist/, never
 * into the repository). The image is `rt`: no core/ui and no core/digikit-menu, which hook the same site.
 *
 * The log is the evidence: a provider has no module record, so MODULES and LOADS do not list it. Its result is the `MOD_LOAD` event with the provider index
 * `0xfffffffe`: [index, result, first four characters of the file name, service hash, base]. The tests need `digikit/debug` first in the selection.
 */
const PROVIDER_IDX = 0xfffffffe;
const E_PLAN_OWNER = loadCode('E_PLAN_OWNER');
const E_HOOK_STOCK = loadCode('E_HOOK_STOCK');
const E_MISSING = loadCode('E_MISSING');
/** The hook the fixture takes, and what the stock site holds (the displaced `movea.l (a2),a0` and `pea (2).w`). */
const HOOK = 'cf.menu.hook.settings_list.tail';
const STOCK_SITE = [0x2052, 0x4878, 0x0002];
const SITES = { dt2: 0x400a5eac, dn2: 0x4009ff88 } as const;
/** The tags of the header table. */
const TAG_NEEDS = 1;
const TAG_PROVIDES = 9;
const TAG_BIND = 10;

const text = (words: number[]) =>
  String.fromCharCode(...words.flatMap((w) => [w >>> 24, (w >>> 16) & 255, (w >>> 8) & 255, w & 255]).filter(Boolean));

interface Bound {
  file: string;
  site: number;
  trampOff: number;
}

/** The packed `test/svc-test` bound to the stock firmware of the device, under dist/. */
async function bind(rig: CardRig, tag: string): Promise<Bound> {
  const stock = resolveInputs(rig.device, FIRMWARE[rig.device]).syx;
  const file = path.join(repoRoot(), 'dist/svc-providers', rig.device, `${tag}.dks`);
  const { stdout } = await patchkit(['bind', moduleArtifact(rig.device, 'test/svc-test'), '--firmware', stock, '-o', file]);
  const hooks = JSON.parse(stdout) as { hook: string; site_addr: string; tramp_off: number }[];
  expect(hooks.map((x) => x.hook), 'the service binds its one hook').toEqual([HOOK]);
  const site = Number(hooks[0]!.site_addr);
  expect(site, 'the site of the device').toBe(SITES[rig.device]);
  return { file, site, trampOff: hooks[0]!.tramp_off };
}

/** The first six bytes at `site` as three words. */
async function siteBytes({ session }: PatchedSession, site: number): Promise<number[]> {
  const [a, b] = await Promise.all([session.client.memPeek(site), session.client.memPeek(site + 4)]);
  if (a === null || b === null) throw new Error(`hook site ${site.toString(16)} is not mapped`);
  return [a >>> 16, a & 0xffff, b >>> 16];
}

type Log = Parameters<typeof ofKind>[0];

/** The provider events of the boot log, in order: result, service hash and base. */
const providers = (log: Log) =>
  ofKind(log, LOG_KIND.MOD_LOAD)
    .filter((e) => e.args[0] === PROVIDER_IDX)
    .map((e) => ({ seq: e.seq, result: e.args[1]! | 0, name: text([e.args[2]!]), hash: e.args[3]!, base: e.args[4]! }));

/** The values a module logged under `message` (`dk_log`: the text in four words, then the value). */
const logged = (log: Log, message: string) =>
  ofKind(log, LOG_KIND.MOD_LOG)
    .filter((e) => text(e.args.slice(0, 4)).startsWith(message))
    .map((e) => e.args[4]!);

cardRigs('service providers', { dsp: 'zero', roots: RT, symbols: CARD_SYMBOLS }, (h) => {
  const modules = () => ({ debug: h.mod('digikit/debug', 'debug'), user: h.mod('test/svc-test-user', 'user') });

  test('a provider installs its hook and the consumer calls it; a safe boot leaves the site stock', async () => {
    const bound = await bind(h, 'good');
    const { debug, user } = modules();
    const provider = { name: 'services/svc-test.dks', file: bound.file };
    const selection = [debug.name, user.name];
    const rt = await h.load('providers', { modules: [debug, user, provider], selection });
    const log = await h.usb(rt, (dk) => dk.readLog());

    // order: the provider loads first (OK, with a base in the arena), and then the user
    const prov = providers(log);
    expect(prov, 'one provider').toHaveLength(1);
    expect([prov[0]!.result, prov[0]!.hash, prov[0]!.name], 'the provider of svc-test loaded').toEqual([0, nameHash('svc-test'), 'svc-']);
    expect(prov[0]!.base, 'in the arena').not.toBe(0);
    const userLoad = ofKind(log, LOG_KIND.MOD_LOAD).find((e) => e.args[0] === 1);
    expect(userLoad?.args[1], 'the user loaded').toBe(0);
    expect(prov[0]!.seq, 'the provider before the user').toBeLessThan(userLoad!.seq);
    const states = await h.usb(rt, (dk) => dk.loadStates());
    expect(states.map((m) => m.state), 'debug and the user are ACTIVE (a provider is no module of the list)').toEqual(['ACTIVE', 'ACTIVE']);

    // the site holds `jmp.l` to the trampoline of the provider (base + the trampoline offset of the bind)
    const tramp = prov[0]!.base + bound.trampOff;
    expect(await siteBytes(rt, bound.site), 'the site jumps to the trampoline').toEqual([0x4ef9, tramp >>> 16, tramp & 0xffff]);

    // the user's call of dk_svctest_ping reached the provider: 42 is in the log
    expect(logged(log, 'svctest-ping'), 'dk_svctest_ping() returned 42').toEqual([42]);

    // the firmware builds the SETTINGS list at start, so the hook ran already; the tap runs it again
    const hits = async () => logged(await h.usb(rt, (dk) => dk.readLog()), 'svctest-hits');
    await waitClock(emulatorClock(rt.session), 1500); // the hook's log line: no event marks it
    const before = await hits();
    expect(before.length, 'the user logged the count').toBeGreaterThan(0);
    expect(before.at(-1)!, 'the hook ran').toBeGreaterThanOrEqual(1);
    await rt.session.client.inputTap(button('SETTINGS'), 100 * MS);
    await waitClock(emulatorClock(rt.session), 3000); // the SETTINGS rebuild after the tap: no event marks it
    const after = await hits();
    expect(after.at(-1)!, 'the count does not fall').toBeGreaterThanOrEqual(before.at(-1)!);
    expect(after.at(-1)!, 'the count is a count').toBeLessThan(1000);
    await rt.session.expectNoFault('provider hook');

    // a safe boot (the safe-once flag of the selection record) loads no provider and no module, and leaves the site stock
    const safe = await h.boot(await h.card({ modules: [debug, user, provider], selection, flags: 1 }));
    const info = await h.ready(true);
    expect(info, 'safe mode by the flag').toMatchObject({ safe: 1, loaded: 0 });
    await safe.session.client.call('run.mode', { mode: 'paused' });
    expect(await siteBytes(safe, bound.site), 'safe mode leaves the site stock').toEqual(STOCK_SITE);
  });

  test('a bound file whose stock bytes differ is E_HOOK_STOCK and its dependent is REFUSED with the service hash', async () => {
    const bound = await bind(h, 'tampered');
    const f = Buffer.from(fs.readFileSync(bound.file));
    // one byte of the stock copy of the BIND item (site hash, address, length, trampoline offset, then the stock bytes): the live site no longer equals it
    f[entry(f, TAG_BIND).at + 16]! ^= 1;
    const tampered = path.join(path.dirname(bound.file), 'tampered-stock.dks');
    fs.writeFileSync(tampered, rehash(f));
    const { debug, user } = modules();
    const rt = await h.load('hook-stock', {
      modules: [debug, user, { name: 'services/svc-test.dks', file: tampered }],
      selection: [debug.name, user.name],
    });
    const log = await h.usb(rt, (dk) => dk.readLog());
    const prov = providers(log);
    expect(prov.map((p) => [p.result, p.hash, p.base]), 'the provider is refused before its init').toEqual([[E_HOOK_STOCK, nameHash('svc-test'), 0]]);
    expect(await siteBytes(rt, bound.site), 'the site was never written').toEqual(STOCK_SITE);
    const states = await h.usb(rt, (dk) => dk.loadStates());
    expect([states[1]!.state, states[1]!.result, states[1]!.detail], 'the user is REFUSED, with the hash of its service').toEqual([
      'REFUSED',
      E_MISSING,
      [nameHash('svc-test'), 0],
    ]);
    expect(logged(log, 'svctest-ping'), 'the user never ran').toEqual([]);
  });

  test('two providers of one hook site are E_PLAN_OWNER: the second is refused and its dependent is REFUSED', async () => {
    const bound = await bind(h, 'owner');
    const { debug, user } = modules();
    // a second provider, `svc-test-b`: the bound file with another service name in PROVIDES, so it takes the hook site that the first owns
    const b = Buffer.from(fs.readFileSync(bound.file));
    b.writeUInt32BE(nameHash('svc-test-b'), entry(b, TAG_PROVIDES).at);
    const second = path.join(path.dirname(bound.file), 'svc-test-b.dks');
    fs.writeFileSync(second, rehash(b));
    // and a second user that needs it: the user file with its need of svc-test changed to svc-test-b (it never loads)
    const userB = patched(fs.readFileSync(moduleArtifact(h.device, 'test/svc-test-user')), 'svc-test-user-b', (f) => {
      const { at, count } = entry(f, TAG_NEEDS);
      for (let k = 0; k < count; k++) if (f.readUInt32BE(at + 12 * k) === nameHash('svc-test')) f.writeUInt32BE(nameHash('svc-test-b'), at + 12 * k);
    });
    const userBFile = path.join(path.dirname(bound.file), 'svc-test-user-b.dkm');
    fs.writeFileSync(userBFile, userB);
    const rt = await h.load('plan-owner', {
      modules: [
        debug,
        user,
        { name: 'modules/userb.dkm', file: userBFile },
        { name: 'services/svc-test.dks', file: bound.file },
        { name: 'services/svc-test-b.dks', file: second },
      ],
      selection: [debug.name, user.name, 'modules/userb.dkm'],
    });
    const log = await h.usb(rt, (dk) => dk.readLog());
    const prov = providers(log).map((p) => [p.hash, p.result]);
    expect(prov, 'the first provider loads, the second takes a site that the first owns').toHaveLength(2);
    expect(prov).toContainEqual([nameHash('svc-test'), 0]);
    expect(prov).toContainEqual([nameHash('svc-test-b'), E_PLAN_OWNER]);
    const states = await h.usb(rt, (dk) => dk.loadStates());
    expect(states[1]!.state, 'the user of the first provider loads').toBe('ACTIVE');
    expect([states[2]!.state, states[2]!.result, states[2]!.detail], 'the user of the second is REFUSED with its hash').toEqual([
      'REFUSED',
      E_MISSING,
      [nameHash('svc-test-b'), 0],
    ]);
    expect(logged(log, 'svctest-ping'), 'dk_svctest_ping() ran once, for the first user').toEqual([42]);
  });
});
