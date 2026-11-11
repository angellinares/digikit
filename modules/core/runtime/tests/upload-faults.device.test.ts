import fs from 'node:fs';
import path from 'node:path';
import { isDeepStrictEqual } from 'node:util';
import { expect } from 'vitest';
import { blob, listing, pokeKnob, statusOf, type Files } from '@digi/regression/runtime/card-kit.ts';
import type { CardRig } from '@digi/regression/runtime/card-rig.ts';
import {
  advance,
  analysisJson,
  frames,
  pcmMatches,
  peak,
  playNote,
  readFloatWav,
} from '@digi/regression/runtime/fx-live.ts';
import { selectSine, type FxProfile } from '@digi/regression/runtime/fx-session.ts';
import { liveCheck as check } from '@digi/regression/runtime/artifacts.ts';
import { moduleArtifact } from '@digi/regression/runtime/install.ts';
import { deviceListing, uploadTests } from '@digi/regression/runtime/upload-kit.ts';
import type { DkSession } from '@digi/dk-session';

/** Python tools/patchkit/pk_digikit_upload.py: the `cut`, `clearcut` and `playback` parts (faults during an upload, and an upload while a note plays). */

/** The card is a store a fault left (`want`): the next boot reads it whole, the plusdrive reader agrees, and one more upload works and commits. */
async function expectStoreAfterFault(
  rig: CardRig,
  label: string,
  card: string,
  want: Files,
  next: { path: string; data: Buffer },
): Promise<void> {
  const dk = await rig.pausedBoot(card);
  expect(await deviceListing(dk), `${label}: the next boot reads the store, whole`).toEqual(listing(want));
  const view = await rig.view(`${label}-v`);
  expect(listing(view.files), `${label}: plusdrive reads the same from the card ${view.head}`).toEqual(listing(want));
  const status = await statusOf(() => dk.put(next.path, next.data));
  const files = { ...want, [next.path]: next.data };
  const after = await rig.view(`${label}-w`);
  check(
    `${label}: the next upload works and commits`,
    status === 0 && isDeepStrictEqual(listing(after.files), listing(files)),
    `${status} ${Object.keys(after.files).sort().join()} ${after.head}`,
  );
}

/** Boot on `card`, set the test knobs, run `action` (its status is returned), and return the card as the guest's writes leave it. */
async function faultSession(
  rig: CardRig,
  tag: string,
  card: string,
  knobs: Record<string, number>,
  action: (dk: DkSession) => Promise<number>,
) {
  const dk = await rig.pausedBoot(card);
  for (const [name, value] of Object.entries(knobs)) await pokeKnob(rig.session, rig.plan, name, value);
  const status = await action(dk);
  return { status, card: await rig.after(tag) };
}

/** A card whose store holds `modules/old.dkm`. */
async function cardWithOld(rig: CardRig, name: string, old: Buffer): Promise<string> {
  const file = path.join(rig.artifacts, 'old.dkm');
  fs.writeFileSync(file, old);
  return rig.card({ name, modules: [{ name: 'modules/old.dkm', file }] });
}

// ---- power cuts ---------------------------------------------------------------------------------------------------------------------

/**
 * The upload of a three-chunk file does six card writes: three DATA chunks, the table, header B, header A. A power cut after the Nth write
 * (`DKRT_UPLOAD_CUT` refuses every later write): the next boot reads a valid store (old or new, never mixed) and takes another upload.
 */
const cutCases = [
  ['after the first chunk', 1, false],
  ['after all chunks', 3, false],
  ['after the table', 4, false],
  ['after header B, before A', 5, true],
  ['after header A', 6, true],
] as const;

uploadTests(
  'upload-cut: a power cut after the Nth card write of an upload',
  async (rig) => {
    const old = blob(3000, 21);
    const added = blob(70_000, 22);
    const base = await cardWithOld(rig, 'cut-base', old);
    const oldFiles: Files = { 'modules/old.dkm': old };
    for (const [k, [label, writes, committed]] of cutCases.entries()) {
      const { status, card } = await faultSession(rig, `cut${k}`, base, { DKRT_UPLOAD_CUT: writes }, (dk) =>
        statusOf(() => dk.put('modules/new.dkm', added)),
      );
      check(
        `cut ${label}: the upload ${writes >= 6 ? 'ends ok' : 'is refused its next write'}`,
        (status === 0) === writes >= 6,
        String(status),
      );
      await expectStoreAfterFault(
        rig,
        `cut ${label}`,
        card,
        committed ? { ...oldFiles, 'modules/new.dkm': added } : oldFiles,
        {
          path: 'modules/more.dkm',
          data: blob(700, 23),
        },
      );
    }
  },
  {
    full: true,
    symbols: ['DKRT_UPLOAD_CUT'],
  },
);

// ---- a card that holds other bytes than the host sent -------------------------------------------------------------------------------

/**
 * END reads the file back from the card before the commit (INFO capability READBACK). The knob `DKRT_UPLOAD_FLIP` flips the first byte of the
 * Nth write on the card, after the write reported success: END must refuse with the hash error and commit no entry (the old store stays).
 */
uploadTests(
  'upload-readback: END refuses a file that the card holds with a flipped byte',
  async (rig) => {
    const old = blob(3000, 31);
    const added = blob(70_000, 32); // three DATA chunks: writes 1 to 3
    const base = await cardWithOld(rig, 'flip-base', old);
    const oldFiles: Files = { 'modules/old.dkm': old };
    for (const [k, write] of [1, 2, 3].entries()) {
      const { status, card } = await faultSession(rig, `flip${k}`, base, { DKRT_UPLOAD_FLIP: write }, (dk) =>
        statusOf(() => dk.put('modules/new.dkm', added)),
      );
      check(`flip in write ${write}: END is refused with the hash error`, status === -56, String(status));
      await expectStoreAfterFault(rig, `flip ${write}`, card, oldFiles, { path: 'modules/more.dkm', data: blob(700, 33) });
    }
  },
  { symbols: ['DKRT_UPLOAD_FLIP'] },
);

// ---- CLEAR: two faults in a row -----------------------------------------------------------------------------------------------------

/**
 * Finding 43, "CLEAR double fault". Fault 1 cuts an operation between header B and header A; fault 2 tears the next upload's header B. The
 * store must still be a valid store (the committed one) afterwards.
 *   clear   CLEAR is cut after header B: B is the empty store, A still the old one, pointing at a table in sector 1. The next upload stages its
 *           file over sector 1 (the store starts over) and its header B write tears.
 *   upload  An upload is cut after header B (the new store in B, the old one in A) and the next upload's header B tears: the first upload's
 *           files must still be there (A was made equal to B by the second BEGIN).
 */
uploadTests(
  'upload-clear-cut: a cut between header B and header A, then a torn header B',
  async (rig) => {
    const old = blob(3000, 61);
    const one = blob(700, 62);
    const two = blob(700, 63);
    const base = await cardWithOld(rig, 'clear-cut-base', old);

    // clear: fault 1 = CLEAR writes header B (write 1) and is cut before header A
    const cleared = await faultSession(rig, 'cc-1', base, { DKRT_UPLOAD_CUT: 1 }, (dk) => statusOf(() => dk.clear()));
    check(
      'clear cut after header B: CLEAR is refused its second write',
      cleared.status === -57,
      String(cleared.status),
    );
    check(
      'after the cut the next boot reads B, the empty store',
      (await (await rig.pausedBoot(cleared.card)).list()).length === 0,
    );
    // fault 2 = the next upload: write 1 = the repair of header A, 2 = the data, 3 = the table, 4 = header B, torn
    const torn = await faultSession(rig, 'cc-2', cleared.card, { DKRT_UPLOAD_TEAR: 4 }, (dk) =>
      statusOf(() => dk.put('modules/one.dkm', one)),
    );
    check('the upload whose header B tears reports a write error', torn.status === -57, String(torn.status));
    await expectStoreAfterFault(rig, 'clear-cut-then-tear', torn.card, {}, { path: 'modules/after.dkm', data: two });

    // upload: fault 1 = an upload of a small file does data (1), table (2), header B (3), header A (4): cut after B
    const cut = await faultSession(rig, 'cu-1', base, { DKRT_UPLOAD_CUT: 3 }, (dk) =>
      statusOf(() => dk.put('modules/one.dkm', one)),
    );
    check('upload cut after header B: refused its last write', cut.status === -57, String(cut.status));
    // fault 2 = the next upload: 1 = repair of A, 2 = data, 3 = table, 4 = header B, torn
    const tear = await faultSession(rig, 'cu-2', cut.card, { DKRT_UPLOAD_TEAR: 4 }, (dk) =>
      statusOf(() => dk.put('modules/two.dkm', two)),
    );
    check("the second upload's header B tears", tear.status === -57, String(tear.status));
    await expectStoreAfterFault(
      rig,
      'upload-cut-then-tear',
      tear.card,
      { 'modules/old.dkm': old, 'modules/one.dkm': one },
      { path: 'modules/after.dkm', data: two },
    );
  },
  {
    full: true,
    symbols: ['DKRT_UPLOAD_CUT', 'DKRT_UPLOAD_TEAR'],
  },
);

// ---- an upload while a note plays ---------------------------------------------------------------------------------------------------

/** A note of a card-built module plays twice (two cold boots of the same card): alone, and with a module uploaded while it sounds. */
uploadTests(
  'upload-playback: the PCM of a note is the same during an upload',
  async (rig) => {
    const { session } = rig;
    const card = await rig.card({
      name: 'play',
      modules: [{ name: 'modules/m0.dkm', file: moduleArtifact(rig.device, 'test/sine') }],
      selection: ['modules/m0.dkm'],
    });
    const profile = await analysisJson<FxProfile>(
      'tools.patchkit.pk_digikit_fxroutes',
      ['profile', '--device', rig.device],
      rig.artifacts,
      'profile',
    );
    const payload = blob(100_000, 31);
    let trigger = 3500;
    let listed: string[] = [];
    const play = async (key: 'alone' | 'during') => {
      const dk = await rig.pausedBoot(card);
      for (let i = 0; i < 40 && !(await dk.modules()).some((m) => m.name === 'test/sine'); i++)
        await advance(session, 250);
      await selectSine({ session, dk, close: async () => {} }, profile);
      // the later of the fixed frame and 300 frames of setup margin; the second run keeps the first run's frame if it can
      trigger = Math.max(trigger, (await frames(session)) + 300);
      const file = path.join(rig.artifacts, `${key}.wav`);
      await playNote(
        session,
        file,
        trigger,
        key === 'during'
          ? async () => {
              await dk.put('modules/up.dkm', payload);
              listed = (await dk.list()).map((e) => e.path);
            }
          : undefined,
      );
      return { pcm: readFloatWav(file), written: await rig.writes(`play-${key}`) };
    };
    const alone = await play('alone');
    const during = await play('during');
    check(
      'the module was uploaded during the note',
      [...listed].sort().join() === ['modules/m0.dkm', 'modules/up.dkm'].join(),
      listed.join(),
    );
    check('the note is audible', peak(alone.pcm) > 0.01, `peak ${peak(alone.pcm).toFixed(3)}`);
    // The onset may differ by a few blocks: the MIDI task's work (header reads, the SHA-256 of the file) keeps the UI task from handling
    // the TRIG key while it runs. From the onset on, not one sample may differ.
    const pcm = pcmMatches(alone.pcm, during.pcm, 256);
    check(
      'PCM of the note during an upload equals the note without it (exact, or sample for sample from the onset on)',
      pcm.ok,
      pcm.detail,
    );
    const { first, limit, selA, selB } = rig.store;
    const extra = [...during.written].filter((s) => !alone.written.has(s));
    const stray = extra.filter((s) => !(first <= s && s < first + limit + 1)).sort((a, b) => a - b);
    check(
      'the card writes the upload added are store sectors only (header copies, data, table; no selection sector)',
      extra.length > 0 && stray.length === 0 && !extra.includes(selA) && !extra.includes(selB),
      `${extra.length} sectors added, outside the store: ${stray
        .slice(0, 6)
        .map((s) => s.toString(16))
        .join(' ')}`,
    );
  },
  { full: true, devices: ['dt2'], sharc: true },
);
