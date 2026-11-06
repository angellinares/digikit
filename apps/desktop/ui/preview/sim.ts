// The fake world of the UI preview: a device (modules and a samples tree), a samples folder on the computer, and how fast and how well the
// device answers. The state can be set from the address (`?dev=empty&folder=files&speed=1.5`, see PARAMS) and while the page runs
// (`window.__sim.set({ dev: 'noreply' })`), so a screenshot script (tools/devtools/ui_shots.mjs) reaches each state without clicking.
import type { SampleEntry } from '@digi/dk-session/samples';
import { DkNeeds } from '@web/modules/dkneeds';
import { setFitReader, type Caps, type Needs } from '@web/modules/fit';
import wasmUrl from '../../../web/public/patchkit.wasm?url';
import { fitWarnings } from '@web/modules/dktransfer';
import { noteActivity } from '@web/transfer/activity';
import type { LocalEntry } from '../src/samples-core';
import type { TransferRow, TransferView } from '@web/transfer/transfer-core';

// the rows' fit verdicts come from the Rust rule in patchkit.wasm, as in the app
setFitReader(await DkNeeds.create(await (await fetch(wasmUrl)).arrayBuffer()));

/** dev: ok (answers), empty (nothing stored), noreply (never answers: the wait notice), error (every read fails), slow (2.5 s per read).
 *  folder: files (the samples folder of ~20 WAVs), empty, none (no folder chosen yet). speed: MiB/s of an upload. */
export interface SimState {
  dev: 'ok' | 'empty' | 'noreply' | 'error' | 'slow';
  folder: 'files' | 'empty' | 'none';
  speed: number;
  paused: boolean;
  mode: 'modules' | 'samples';
  /** The device storage can make folders (mkdir=0: it cannot). */
  mkdir: boolean;
  /** The second file of an upload cannot be sent (putfail=1). */
  putfail: boolean;
}

export const PARAMS = ['dev', 'folder', 'speed', 'paused', 'mode', 'mkdir', 'putfail'] as const;

const query = new URLSearchParams(location.search);
export const sim: SimState = {
  dev: (query.get('dev') as SimState['dev']) ?? 'ok',
  folder: (query.get('folder') as SimState['folder']) ?? 'files',
  speed: Number(query.get('speed') ?? 3),
  paused: query.get('paused') === '1',
  mode: (query.get('mode') as SimState['mode']) ?? 'modules',
  mkdir: query.get('mkdir') !== '0',
  putfail: query.get('putfail') === '1',
};

export const delay = (ms: number) => new Promise<void>((r) => setTimeout(r, ms));

/** What a read of the device costs and whether it works: the states of the wait notice and the error line. */
export async function deviceRead(): Promise<void> {
  switch (sim.dev) {
    case 'noreply': {
      // a real session over the mocked emulator capture, which never answers: the request waits, Cancel works, a timeout follows
      const { session } = await import('@web/transfer/target');
      await (await session()).info();
      return;
    }
    case 'error':
      throw new Error('The card is not mounted (preview: dev=error).');
    case 'slow':
      // slow but alive: the device keeps answering in small pieces, so nothing says "Waiting for ..."
      for (let i = 0; i < 12; i++) {
        await delay(200);
        noteActivity();
      }
      return;
    default:
      return delay(120);
  }
}

// ---- modules ----------------------------------------------------------------------------------------------------------------------

interface LibModule {
  id: string;
  label: string;
  group: TransferRow['group'];
  version: string;
  size: number;
  test?: boolean;
  /** What a load of the module takes (a machine with DSP code: arena, DSP area, rows). */
  needs: Needs;
}
const KiB = 1024;
const MiB = 1024 * KiB;
/** Needs of a machine (`dsp` bytes of DSP area; 0: a module with no DSP code) or a tool. */
const need = (size: number, dsp = 0, rows = dsp ? 1 : 0): Needs => ({
  file_size: size,
  ARENA: 4096,
  ARENA_XFER: dsp ? 2048 : 0,
  DSP_AREA: dsp,
  DSP_ALIGN: dsp ? 8 : 0,
  MACHINE_ROWS: rows,
  VOICE_ROWS: rows,
  PARAM_ROWS: rows * 4,
});
export const LIBRARY: LibModule[] = [
  {
    id: 'sine',
    label: 'Sine',
    group: 'Machines',
    version: '0.3.1',
    size: 18 * KiB,
    needs: need(18 * KiB, 200 * KiB),
  },
  {
    id: 'pluck',
    label: 'Pluck',
    group: 'Machines',
    version: '0.2.0',
    size: 42 * KiB,
    needs: need(42 * KiB, 259 * KiB),
  },
  {
    id: 'wavetable',
    label: 'Wavetable Voyager with a rather long display name',
    group: 'Machines',
    version: '1.0.0',
    size: 60 * KiB,
    needs: need(60 * KiB, 420 * KiB),
  },
  {
    id: 'fm4',
    label: 'FM 4-op',
    group: 'Machines',
    version: '0.9.2',
    size: 61 * KiB,
    needs: need(61 * KiB, 600 * KiB),
  },
  { id: 'chorus', label: 'Chorus', group: 'Effects', version: '0.4.0', size: 26 * KiB, needs: need(26 * KiB) },
  { id: 'tuner', label: 'Tuner', group: 'Tools', version: '0.1.0', size: 9 * KiB, needs: need(9 * KiB) },
  { id: 'remote', label: 'Remote control', group: 'Tools', version: '0.1.0', size: 1032, needs: need(1032) },
  {
    id: 'test-noise',
    label: 'Noise test',
    group: 'Machines',
    version: '0.0.1',
    size: 7 * KiB,
    test: true,
    needs: need(7 * KiB, 100 * KiB),
  },
];
const pathOf = (id: string) => `modules/${id}.dkm`;

interface DevFile {
  path: string;
  size: number;
  /** The same bytes as the library's build. */
  same: boolean;
  loaded: boolean;
}
const device = new Map<string, DevFile>();
function resetDevice(): void {
  device.clear();
  if (sim.dev === 'empty') return;
  device.set(pathOf('sine'), { path: pathOf('sine'), size: 18 * KiB, same: true, loaded: true });
  device.set(pathOf('pluck'), { path: pathOf('pluck'), size: 40 * KiB, same: false, loaded: false });
  device.set(pathOf('chorus'), { path: pathOf('chorus'), size: 26 * KiB, same: true, loaded: false });
  device.set('modules/old-experiment.dkm', {
    path: 'modules/old-experiment.dkm',
    size: 5 * KiB,
    same: false,
    loaded: false,
  });
}
resetDevice();

/** CAPACITY of a Digitakt II with the loaded files' modules in it. */
function capacity(): Caps {
  const used = { ARENA: 0, DSP_AREA: 0, MACHINE_ROWS: 0, VOICE_ROWS: 0, PARAM_ROWS: 0, n: 0 };
  for (const f of device.values()) {
    const m = f.loaded ? LIBRARY.find((l) => pathOf(l.id) === f.path) : undefined;
    if (!m) continue;
    used.n++;
    used.ARENA += (m.needs.ARENA ?? 0) + (m.needs.ARENA_XFER ?? 0);
    used.DSP_AREA += m.needs.DSP_AREA ?? 0;
    used.MACHINE_ROWS += m.needs.MACHINE_ROWS ?? 0;
    used.VOICE_ROWS += m.needs.VOICE_ROWS ?? 0;
    used.PARAM_ROWS += m.needs.PARAM_ROWS ?? 0;
  }
  const c = (limit: number, u: number, largest = limit - u) => ({ limit, used: u, largest });
  return {
    MODULES: c(8, used.n, 0),
    SELECTION: c(8, used.n, 0),
    ARENA: c(524288, used.ARENA, 524272 - used.ARENA),
    STAGE: c(65536, 0, 65536),
    DSP_AREA: c(524288, used.DSP_AREA, 524284 - used.DSP_AREA),
    MACHINE_ROWS: c(8, used.MACHINE_ROWS, 0),
    VOICE_ROWS: c(8, used.VOICE_ROWS, 0),
    PARAM_ROWS: c(16, used.PARAM_ROWS, 0),
    PAGE_ROWS: c(0, 0, 0),
    FX_SLOTS: c(0, 0, 0),
  };
}

/** The merged list the Transfer page shows (`TransferView`), from the library and the device's files. */
export function view(): TransferView {
  const cap = capacity();
  if (sim.dev === 'empty' && device.size === 0) {
    // nothing stored yet, but the library is there
  }
  const modules: TransferRow[] = LIBRARY.map((m) => {
    const f = device.get(pathOf(m.id));
    const row = {
      id: m.id,
      path: pathOf(m.id),
      name: m.id,
      label: m.label,
      group: m.group,
      test: !!m.test,
      version: m.version,
      file_size: m.size,
      arena: m.needs.ARENA,
      dsp: m.needs.DSP_AREA,
      needs: m.needs,
      on_card: f ? (f.same ? 'yes' : 'other version') : 'no',
      loaded: !!f?.loaded,
    } as TransferRow;
    row.warnings = fitWarnings(row as { loaded: boolean; needs?: Needs }, cap);
    row.fits = row.warnings.length === 0;
    return row;
  });
  const known = new Set(LIBRARY.map((m) => pathOf(m.id)));
  const card_only: TransferRow[] = [...device.values()]
    .filter((f) => !known.has(f.path))
    .map((f) => ({
      id: null,
      path: f.path,
      name: f.path.split('/').pop() as string,
      group: 'Tools',
      test: false,
      version: null,
      file_size: f.size,
      on_card: 'yes',
      loaded: f.loaded,
    }));
  return {
    device: 'dt2',
    runtime: '0.9.4',
    free_bytes: 3.2 * 2 ** 30 - [...device.values()].reduce((a, f) => a + f.size, 0),
    loaded: [...device.values()].filter((f) => f.loaded).map((f) => f.path),
    capacity: cap,
    modules,
    card_only,
  };
}

export const deviceFiles = device;
export const libraryById = (id: string) => LIBRARY.find((m) => m.id === id);
export { pathOf };

// ---- samples ----------------------------------------------------------------------------------------------------------------------

export const SAMPLES_ROOT = '/Users/em/Music/Samples';
const WAVS = [
  ['kick_808_long_tail', 2.4],
  ['kick_punchy', 1.1],
  ['snare_tight_01', 1.6],
  ['snare_rimshot', 1.3],
  ['hat_closed_a', 0.6],
  ['hat_open_wide', 1.8],
  ['clap_stack', 1.2],
  ['tom_low', 2.2],
  ['perc_conga_hi', 1.5],
  ['perc_shaker_loop_120bpm', 3.9],
  ['bass_sub_C1', 4.2],
  ['bass_reese_growl_with_a_very_long_name_to_force_the_ellipsis_in_the_row_120bpm_Fm', 5.7],
  ['lead_pluck_C3', 2.8],
  ['pad_warm_swell', 6.1],
  ['fx_riser_8bars', 5.2],
  ['fx_impact_big', 3.3],
  ['vox_chop_01', 1.9],
  ['vox_chop_02', 2.1],
  ['loop_amen_break_170', 4.6],
  ['loop_dub_chords_90', 5.9],
] as const;

type Kind = 'converted' | 'native' | 'unchanged';
interface LocalFile {
  name: string;
  size: number;
  kind: Kind;
}
const wavs = (names: string[], kb: number): LocalFile[] =>
  names.map((n) => ({ name: `${n}.wav`, size: Math.round(kb * KiB), kind: 'converted' as Kind }));
/** The folders of the chosen folder: each path holds its files; its subfolders are the paths one level below it ('' is the top). */
const dirs = new Map<string, LocalFile[]>([
  [
    '',
    [
      ...WAVS.map(([n, mb]) => ({ name: `${n}.wav`, size: Math.round(mb * MiB), kind: 'converted' as Kind })),
      { name: 'amen_slices.dksmp', size: Math.round(2.0 * MiB), kind: 'native' as Kind },
      { name: 'notes.txt', size: 812, kind: 'unchanged' as Kind },
    ],
  ],
  ['Drums', wavs(['808_kick_a', '808_kick_b', '808_snare', '808_hat'], 900)],
  ['Vocals', wavs(['ooh', 'aah', 'hey'], 1400)],
  ['loops', wavs(['amen_170', 'dub_chords_90', 'funk_guitar_105'], 3200)],
  ['loops/breaks', wavs(['think_break', 'apache_break', 'funky_drummer'], 2600)],
  ['loops/breaks/chopped', wavs(['think_01', 'think_02'], 700)],
  [
    'MIDI',
    [
      { name: 'bassline_a.mid', size: 2 * KiB, kind: 'unchanged' as Kind },
      { name: 'bassline_b.mid', size: 3 * KiB, kind: 'unchanged' as Kind },
      { name: 'drums_swing.mid', size: 5 * KiB, kind: 'unchanged' as Kind },
    ],
  ],
  ['one-shots', wavs(['clap_a', 'clap_b', 'rim', 'cowbell', 'zap'], 180)],
  ['one-shots/vox', wavs(['yeah', 'uh'], 240)],
  [
    'Presets',
    [
      { name: 'kit_a.json', size: 6 * KiB, kind: 'unchanged' as Kind },
      { name: 'kit_b.json', size: 7 * KiB, kind: 'unchanged' as Kind },
    ],
  ],
  ['Presets/empty', []],
]);
let added: LocalFile[] = [];

const levelFiles = (rel: string): LocalFile[] =>
  rel === '' ? [...(dirs.get('') ?? []), ...added] : (dirs.get(rel) ?? []);
const parentDir = (rel: string) => rel.split('/').slice(0, -1).join('/');

/** The `local_list` of the preview's folder at `rel`. */
export function listLocal(rel: string): LocalEntry[] {
  if (sim.folder === 'empty') return [];
  const folders: LocalEntry[] = [...dirs.keys()]
    .filter((d) => d !== '' && parentDir(d) === rel)
    .map((d) => ({ name: d.split('/').pop() as string, kind: 'folder' as const, size: 0 }));
  return [...folders, ...levelFiles(rel).map((f) => ({ name: f.name, kind: 'file' as const, size: f.size }))];
}
export const localKindOf = (rel: string): Kind | undefined => {
  const i = rel.lastIndexOf('/');
  return levelFiles(i < 0 ? '' : rel.slice(0, i)).find((f) => f.name === rel.slice(i + 1))?.kind;
};
export const localSizeOf = (rel: string): number => {
  const i = rel.lastIndexOf('/');
  return levelFiles(i < 0 ? '' : rel.slice(0, i)).find((f) => f.name === rel.slice(i + 1))?.size ?? 0;
};

// the device's sample folders
const tree = new Map<string, SampleEntry[]>();
function resetTree(): void {
  tree.clear();
  if (sim.dev === 'empty') return;
  tree.set('', [
    { name: 'Drums', kind: 'folder', size: 0 },
    { name: 'Loops', kind: 'folder', size: 0 },
    { name: 'init_tone', kind: 'file', size: 482 * KiB },
  ]);
  tree.set('Drums', [
    { name: '808_kick_a', kind: 'file', size: 900 * KiB },
    { name: 'snare_tight_01', kind: 'file', size: 1.6 * MiB },
    { name: 'hat_closed_a', kind: 'file', size: 0.6 * MiB },
  ]);
  tree.set('Loops', [{ name: 'loop_amen_break_170', kind: 'file', size: 4.6 * MiB }]);
}
resetTree();
export const sampleTree = tree;

// ---- control from outside -----------------------------------------------------------------------------------------------------------

/** What `window.__sim` offers a script. */
export const control = {
  sim,
  /** Change the state; `dev` and `folder` reset what they stand for. */
  set(next: Partial<SimState>): void {
    Object.assign(sim, next);
    if (next.dev) {
      resetDevice();
      resetTree();
    }
  },
  /** A file appears in the samples folder (the folder poll or the focus read must show it). */
  addLocal(name: string, size = 1.5 * MiB): void {
    added = [...added, { name, size, kind: name.endsWith('.wav') ? 'converted' : 'unchanged' }];
  },
  /** A file of the top level disappears. */
  removeLocal(name: string): void {
    const top = dirs.get('') ?? [];
    const i = top.findIndex((f) => f.name === name);
    if (i >= 0) top.splice(i, 1);
    added = added.filter((f) => f.name !== name);
  },
  counts: { localList: 0, localPoll: 0, deviceRead: 0 },
};
