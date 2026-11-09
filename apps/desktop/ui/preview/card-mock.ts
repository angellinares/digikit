// A fake controller for the Card panel in the preview: `card.folder`, `card.folder_set`, `card.sync` and the folder dialog, so each state
// of the panel is reached from the address (`?page=emulator&card=dn2|dt2&chosen=1&missing=1`) and by clicking.
//   card=dn2|dt2   the loaded device
//   chosen=1       the user already chose a folder for it
//   missing=1      the saved choice is gone (the default is used)
//   overlay=0      the card has no saved changes (the reset button is off)

const query = new URLSearchParams(location.search);
const device = query.get('card') === 'dt2' ? 'dt2' : 'dn2';
const HOME = '/Users/em/src/digi/digitakt2';
const CHOSEN = `/Users/em/Music/Elektron/${device === 'dt2' ? 'Digitakt II' : 'Digitone II'} card`;
const DEFAULT = `${HOME}/plus_drive/${device}`;

/** The folder the module store is mirrored to now, and the saved choice (the next load's folder). */
const state = {
  folder: query.get('chosen') === '1' && query.get('missing') !== '1' ? CHOSEN : DEFAULT,
  choice: query.get('chosen') === '1' ? CHOSEN : (null as string | null),
  missing: query.get('missing') === '1',
  lastSync: Date.now() - 12_000,
  overlay: query.get('overlay') !== '0',
};

const files = [
  { path: 'modules/pluck.dkm', bytes: 2260, hash: 'aa' },
  { path: 'modules/sine.dkm', bytes: 932, hash: 'bb' },
  ...(device === 'dt2' ? [{ path: 'samples/kick.wav', bytes: 48_212, hash: 'cc' }] : []),
];

/** Only a device whose card is built from the folder (a path filesystem: the Digitakt II) needs a reload to take a new folder. */
const fromFolder = device === 'dt2';
const next = () => (state.missing ? DEFAULT : (state.choice ?? DEFAULT));

function folderState() {
  return {
    folder: state.folder,
    continuous: true,
    files,
    last_sync: state.lastSync,
    behind: false,
    error: null,
    device,
    default_folder: DEFAULT,
    custom: state.choice !== null && !state.missing,
    chosen_missing: state.missing ? (state.choice ?? CHOSEN) : null,
    next_folder: next(),
    reload_needed: state.folder !== next(),
    overlay: state.overlay,
  };
}

export async function cardCall(method: string, params: Record<string, unknown>): Promise<unknown> {
  if (method === 'card.folder_set') {
    state.choice = typeof params.dir === 'string' && params.dir ? params.dir : null;
    state.missing = false;
    if (!fromFolder) {
      state.folder = next();
      state.lastSync = Date.now();
    }
  }
  if (method === 'card.sync') state.lastSync = Date.now();
  if (method === 'card.reset_overlay') {
    const had = state.overlay;
    state.overlay = false;
    return {
      overlay: `${HOME}/out/native/run/cards/card/overlay.bin`,
      set_aside: had ? `${HOME}/out/native/run/cards/card/overlay.bin.bad-1` : null,
    };
  }
  return folderState();
}

export const pickFolder = async () => CHOSEN;

/** The Restart path: the card is built again from the folder in use. */
export async function reloadCard(): Promise<void> {
  state.folder = next();
}

export const cardDevice = device;
