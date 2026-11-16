// A Host for tests and previews that records every call (command, arguments, order) and answers with canned replies. It talks to nothing.
// A reply is a value or a function of the arguments; a command with no reply fails loudly, except the ones that answer nothing (open,
// close, send, cancel ...). Replies can be added or changed while a test runs (`replies`).
import type { Host } from './host';

/** One recorded call: `command` is `group.method` (`ports.midi.send`, `rpc.call`), `args` the arguments as given. */
export interface HostCall {
  command: string;
  args: unknown[];
}

export type Reply = unknown | ((...args: any[]) => unknown);

/** Commands that answer nothing: no reply needed. */
const ANSWERS_NOTHING = new Set([
  'ports.midi.open',
  'ports.midi.close',
  'ports.midi.send',
  'ports.midi.cancel',
  'ports.bulk.open',
  'ports.bulk.close',
  'ports.bulk.send',
  'ports.bulk.cancel',
  'ports.bulk.openOutOnly',
  'ports.bulk.sendOutOnly',
  'ports.bulk.closeOutOnly',
  'shell.openCardFolder',
  'emu.stop',
  'emu.card.reset',
  'emu.card.stage',
]);

export interface FakeHost extends Host {
  /** Every call so far, in order. */
  readonly calls: HostCall[];
  /** Canned replies by command; assign to add or change one. */
  readonly replies: Record<string, Reply>;
  /** The commands called so far, in order (`calls.map(c => c.command)`). */
  commands(): string[];
  /** The arguments of each call of one command. */
  argsOf(command: string): unknown[][];
}

export function createFakeHost(replies: Record<string, Reply> = {}): FakeHost {
  const calls: HostCall[] = [];
  const table: Record<string, Reply> = { ...replies };
  const answer = async (command: string, args: unknown[]): Promise<any> => {
    calls.push({ command, args });
    if (!(command in table)) {
      if (ANSWERS_NOTHING.has(command)) return undefined;
      throw new Error(`FakeHost: no reply for ${command}`);
    }
    const reply = table[command];
    return typeof reply === 'function' ? (reply as (...a: unknown[]) => unknown)(...args) : reply;
  };
  const leaf =
    (command: string) =>
    (...args: unknown[]) =>
      answer(command, args);
  return {
    calls,
    replies: table,
    commands: () => calls.map((c) => c.command),
    argsOf: (command) => calls.filter((c) => c.command === command).map((c) => c.args),
    rpc: { call: leaf('rpc.call') },
    fs: {
      pickFolder: leaf('fs.pickFolder'),
      list: leaf('fs.list'),
      read: leaf('fs.read'),
      saveLog: leaf('fs.saveLog'),
    },
    library: {
      build: leaf('library.build'),
      scan: leaf('library.scan'),
      read: leaf('library.read'),
      readMaps: leaf('library.readMaps'),
      deviceStorage: leaf('library.deviceStorage'),
    },
    ports: {
      midi: {
        ports: leaf('ports.midi.ports'),
        open: leaf('ports.midi.open'),
        close: leaf('ports.midi.close'),
        send: leaf('ports.midi.send'),
        recv: leaf('ports.midi.recv'),
        cancel: leaf('ports.midi.cancel'),
      },
      bulk: {
        list: leaf('ports.bulk.list'),
        open: leaf('ports.bulk.open'),
        close: leaf('ports.bulk.close'),
        send: leaf('ports.bulk.send'),
        recv: leaf('ports.bulk.recv'),
        cancel: leaf('ports.bulk.cancel'),
        openOutOnly: leaf('ports.bulk.openOutOnly'),
        sendOutOnly: leaf('ports.bulk.sendOutOnly'),
        closeOutOnly: leaf('ports.bulk.closeOutOnly'),
      },
    },
    firmware: {
      prepare: leaf('firmware.prepare'),
      save: leaf('firmware.save'),
      patchkitWasm: leaf('firmware.patchkitWasm'),
      bindBundle: leaf('firmware.bindBundle'),
      dkprofWasm: leaf('firmware.dkprofWasm'),
      sampleWasm: leaf('firmware.sampleWasm'),
    },
    emu: {
      setSource: leaf('emu.setSource'),
      pickSyx: leaf('emu.pickSyx'),
      sourceInfo: leaf('emu.sourceInfo'),
      buildInfo: leaf('emu.buildInfo'),
      launch: leaf('emu.launch'),
      // synchronous, and recorded like the others: the replies say whether the app is native (default: yes)
      inApp: () => {
        calls.push({ command: 'emu.inApp', args: [] });
        return 'emu.inApp' in table
          ? Boolean(
              typeof table['emu.inApp'] === 'function' ? (table['emu.inApp'] as () => unknown)() : table['emu.inApp'],
            )
          : true;
      },
      startup: leaf('emu.startup'),
      step: leaf('emu.step'),
      tap: leaf('emu.tap'),
      stop: leaf('emu.stop'),
      audioStatus: leaf('emu.audioStatus'),
      saveDiagnostics: leaf('emu.saveDiagnostics'),
      stageSyx: leaf('emu.stageSyx'),
      card: { reset: leaf('emu.card.reset'), stage: leaf('emu.card.stage'), load: leaf('emu.card.load') },
    },
    shell: { openCardFolder: leaf('shell.openCardFolder') },
  };
}
