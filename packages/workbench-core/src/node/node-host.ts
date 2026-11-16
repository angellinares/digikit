// The Host (host/host.ts) for a command line: the groups that make sense without a window, over Node. The desktop app's Host is Tauri's
// commands; this one reads files with node:fs, drives the `dkmidi` and `dkbulk` bridge processes for the ports, reads the wasm files from the
// checkout and sends the emulator RPC over the control socket (packages/control-client). A command with no headless meaning (a dialog, the build of
// the library, the emulator session of the app) fails with `NodeHostUnsupported`. `wb` (tools/devtools/wb.mjs) uses it, so a command line and the
// workbench run the same code over the same interfaces.
import { execFile, spawn, type ChildProcess } from 'node:child_process';
import { existsSync, readdirSync, readFileSync, realpathSync, statSync } from 'node:fs';
import { mkdir, readFile, writeFile } from 'node:fs/promises';
import path from 'node:path';
import { promisify } from 'node:util';
import { connect, type Client } from '@digi/control-client';
import { ProcessBulkLink } from '@digi/dk-session';
import { ProcessTransport } from '@digi/dk-session/process-transport';
import { parse } from 'smol-toml';
import type {
  BulkHost,
  BulkListed,
  EmuHost,
  EmuRpc,
  FirmwareHost,
  FsHost,
  Host,
  LibraryHost,
  LocalListing,
  MidiHost,
  ShellHost,
} from '../host/host.ts';

const run = promisify(execFile);

/** A Host command that has no meaning without the desktop app (a dialog, the library build, the app's emulator session). */
export class NodeHostUnsupported extends Error {
  constructor(command: string) {
    super(`${command} is not available in the Node host`);
    this.name = 'NodeHostUnsupported';
  }
}

export interface NodeHostOptions {
  /** The checkout: the library (`out/modules`), the device profiles (`devices/`) and the wasm files (`apps/web/public`). */
  root: string;
  /** The `dkmidi` bridge binary (`mise run dkmidi:build`). */
  dkmidiBin?: string;
  /** The `dkbulk` bridge binary (`mise run dkbulk:build`). */
  dkbulkBin?: string;
  /** The control socket of a running emulator, for `rpc`. */
  socket?: string;
  /** Where `saveLog` and `firmware.save` write; without it they are unsupported (a command line has no save dialog). */
  outDir?: string;
}

/** The ArrayBuffer of exactly these bytes. */
const toBuffer = (b: Uint8Array): ArrayBuffer =>
  b.buffer.slice(b.byteOffset, b.byteOffset + b.byteLength) as ArrayBuffer;

const unsupported = (command: string) => (): Promise<never> => Promise.reject(new NodeHostUnsupported(command));

/** Names under the output folder only: no path part, no dot file. */
function safeName(name: string): string {
  if (!name || name !== path.basename(name) || name.startsWith('.')) throw new Error(`not a file name: ${name}`);
  return name;
}

/** `rel` (`/` separated, empty for the folder itself) inside `root`: a real path under it, or an error (the rules of local.rs). */
function inside(root: string, rel: string): string {
  const dir = realpathSync(root);
  if (!statSync(dir).isDirectory()) throw new Error(`${root}: not a folder`);
  if (rel !== '' && rel.split('/').some((p) => p === '' || p === '.' || p === '..'))
    throw new Error(`not a path inside the folder: ${JSON.stringify(rel)}`);
  const real = realpathSync(path.join(dir, rel));
  if (real !== dir && !real.startsWith(dir + path.sep))
    throw new Error(`not a path inside the folder: ${JSON.stringify(rel)}`);
  return real;
}

/** The files of the computer (`local_list`, `local_read`): one level, folders first, no dot files; a link that leaves the folder is left out. */
function nodeFs(outDir: string | undefined): FsHost {
  return {
    pickFolder: unsupported('fs.pickFolder'),
    async list(root, rel): Promise<LocalListing[]> {
      const dir = inside(root, rel);
      const base = realpathSync(root);
      const out: LocalListing[] = [];
      for (const name of readdirSync(dir)) {
        if (name.startsWith('.')) continue;
        let real: string;
        try {
          real = realpathSync(path.join(dir, name));
        } catch {
          continue;
        }
        if (real !== base && !real.startsWith(base + path.sep)) continue;
        const meta = statSync(real);
        if (meta.isDirectory()) out.push({ name, kind: 'folder', size: 0 });
        else if (meta.isFile()) out.push({ name, kind: 'file', size: meta.size });
      }
      return out.sort(
        (a, b) =>
          Number(a.kind !== 'folder') - Number(b.kind !== 'folder') || (a.name < b.name ? -1 : a.name > b.name ? 1 : 0),
      );
    },
    async read(root, rel, head) {
      if (rel === '') throw new Error('no file named');
      const file = inside(root, rel);
      if (!statSync(file).isFile()) throw new Error(`${rel}: not a file`);
      const bytes = await readFile(file);
      return toBuffer(head === null ? bytes : bytes.subarray(0, head));
    },
    saveLog: (bytes, name) => saveTo(outDir, 'fs.saveLog', bytes, name),
  };
}

async function saveTo(outDir: string | undefined, command: string, bytes: Uint8Array, name: string): Promise<string> {
  if (!outDir) throw new NodeHostUnsupported(command);
  await mkdir(outDir, { recursive: true });
  const file = path.join(outDir, safeName(name));
  await writeFile(file, bytes);
  return file;
}

/** The module library `out/modules/<device>/` (`lib_scan`, `lib_read`) and the device profiles (`device_storage`). `build` needs the Rust packer: unsupported. */
function nodeLibrary(root: string): LibraryHost {
  const deviceDir = (device: string) => {
    if (!/^[a-z0-9]+$/.test(device)) throw new Error(`not a device id: ${JSON.stringify(device)}`);
    return path.join(root, 'out/modules', device);
  };
  const walk = (dir: string, base: string, out: { path: string; size: number }[], ext = '.dkm') => {
    for (const e of readdirSync(dir, { withFileTypes: true })) {
      const full = path.join(dir, e.name);
      if (e.isDirectory()) walk(full, base, out, ext);
      else if (e.name.endsWith(ext))
        out.push({ path: path.relative(base, full).split(path.sep).join('/'), size: statSync(full).size });
    }
  };
  return {
    build: unsupported('library.build'),
    readMaps: unsupported('library.readMaps'),
    async scan(device) {
      const dir = deviceDir(device);
      const out: { path: string; size: number }[] = [];
      if (existsSync(dir)) walk(dir, dir, out);
      // the unbound services are packed into dist/modules/<device>/ (the desktop copies them into its library)
      const services = path.join(root, 'dist/modules', device);
      if (existsSync(services)) walk(services, services, out, '.dks');
      return out.sort((a, b) => (a.path < b.path ? -1 : a.path > b.path ? 1 : 0));
    },
    async read(device, rel) {
      const dir = deviceDir(device);
      if (
        rel.split('/').some((p) => p === '' || p === '.' || p === '..') ||
        !(rel.endsWith('.dkm') || rel.endsWith('.dks'))
      )
        throw new Error(`not a library file: ${JSON.stringify(rel)}`);
      const base = rel.endsWith('.dks') ? path.join(root, 'dist/modules', device) : dir;
      return toBuffer(await readFile(path.join(base, rel)));
    },
    async deviceStorage(device) {
      const dir = path.join(root, 'devices');
      for (const file of readdirSync(dir).filter((f) => f.endsWith('.toml'))) {
        const profile = parse(readFileSync(path.join(dir, file), 'utf8')) as {
          device?: { name?: string; short?: string };
          storage?: { samples?: string };
        };
        if (profile.device?.short !== device) continue;
        const samples = profile.storage?.samples;
        if (!samples) throw new Error(`the ${profile.device.name} profile has no [storage]`);
        return { name: profile.device.name ?? device, samples };
      }
      throw new Error(`no device profile for ${JSON.stringify(device)}`);
    },
  };
}

/** A bridge process with its end: `end()` closes its stdin and waits for it to exit (a bridge whose port vanished may not exit by itself: killed after 3 s). */
function bridge(bin: string, args: string[]) {
  const child: ChildProcess = spawn(bin, args, { stdio: ['pipe', 'pipe', 'inherit'] });
  const exited = new Promise<void>((resolve) => child.on('exit', () => resolve()));
  child.stdin!.on('error', () => {});
  const failed = new Promise<never>((_, reject) =>
    child.on('error', (e) => reject(new Error(`cannot start ${bin}: ${e.message}`))),
  );
  failed.catch(() => {});
  return {
    child,
    write: (b: Uint8Array) => void child.stdin!.write(b),
    subscribe: (cb: (chunk: Uint8Array) => void) => void child.stdout!.on('data', (d: Buffer) => cb(new Uint8Array(d))),
    async end() {
      child.stdin!.end();
      const timer = setTimeout(() => child.kill(), 3000);
      await exited;
      clearTimeout(timer);
    },
  };
}

const need = (bin: string | undefined, what: string): string => {
  if (!bin) throw new Error(`no ${what} binary given`);
  if (!existsSync(bin)) throw new Error(`no ${what} binary at ${bin}`);
  return bin;
};

/** `MidiHost` over `dkmidi open NAME`: whole SysEx messages through the same framing the command lines use (ProcessTransport). */
function nodeMidi(bin: string | undefined): MidiHost {
  let open: { bridge: ReturnType<typeof bridge>; transport: ProcessTransport } | undefined;
  let waiting: AbortController | undefined;
  const current = () => {
    if (!open) throw new Error('no MIDI port is open');
    return open;
  };
  return {
    async ports() {
      const { stdout } = await run(need(bin, 'dkmidi'), ['ports']);
      return stdout.split('\n').filter(Boolean);
    },
    async open(name) {
      await this.close();
      const b = bridge(need(bin, 'dkmidi'), ['open', name]);
      open = { bridge: b, transport: new ProcessTransport(b.write, b.subscribe) };
    },
    async close() {
      const o = open;
      open = undefined;
      await o?.bridge.end();
    },
    async send(message) {
      current().transport.send(message);
    },
    async recv(timeoutMs) {
      const transport = current().transport;
      waiting = new AbortController();
      try {
        const got = await transport.recv(timeoutMs, waiting.signal);
        return got ? toBuffer(got) : new ArrayBuffer(0);
      } finally {
        waiting = undefined;
      }
    },
    async cancel() {
      waiting?.abort();
    },
  };
}

/** `BulkHost` over `dkbulk list` and `dkbulk open [--out-only] ID`: whole frames through ProcessBulkLink. One link of each kind at a time. */
function nodeBulk(bin: string | undefined): BulkHost {
  let link: { bridge: ReturnType<typeof bridge>; link: ProcessBulkLink } | undefined;
  let outOnly: ReturnType<typeof bridge> | undefined;
  let waiting: AbortController | undefined;
  const closeLink = async () => {
    const l = link;
    link = undefined;
    await l?.bridge.end();
  };
  const closeOut = async () => {
    const b = outOnly;
    outOnly = undefined;
    await b?.end();
  };
  return {
    async list(): Promise<BulkListed[]> {
      const { stdout } = await run(need(bin, 'dkbulk'), ['list']);
      return stdout
        .split('\n')
        .filter(Boolean)
        .map((line) => {
          const [id, name, , flag] = line.split('\t');
          return { id, name, out_only: flag === 'out-only' };
        });
    },
    async open(id) {
      await closeLink();
      const b = bridge(need(bin, 'dkbulk'), ['open', id]);
      link = { bridge: b, link: new ProcessBulkLink(b.write, b.subscribe, () => b.child.stdin!.end()) };
    },
    close: closeLink,
    async send(frame) {
      if (!link) throw new Error('no bulk link is open');
      await link.link.send(frame);
    },
    async recv(timeoutMs) {
      if (!link) throw new Error('no bulk link is open');
      waiting = new AbortController();
      try {
        const got = await link.link.recv(timeoutMs, waiting.signal);
        return got ? toBuffer(got) : new ArrayBuffer(0);
      } finally {
        waiting = undefined;
      }
    },
    async cancel() {
      waiting?.abort();
    },
    async openOutOnly(id) {
      await closeOut();
      outOnly = bridge(need(bin, 'dkbulk'), ['open', '--out-only', id]);
    },
    async sendOutOnly(frame) {
      if (!outOnly) throw new Error('no OUT-only link is open');
      outOnly.write(frame);
    },
    closeOutOnly: closeOut,
  };
}

/** The wasm files of the checkout (`mise run wasm:patchkit`); the image patching (`prepare`) is the desktop's Rust command. */
function nodeFirmware(root: string, outDir: string | undefined): FirmwareHost {
  const wasm = async (name: string) => {
    const file = path.join(root, 'apps/web/public', name);
    if (!existsSync(file)) throw new Error(`${file} is missing (build it: mise run wasm:patchkit)`);
    return toBuffer(await readFile(file));
  };
  return {
    prepare: unsupported('firmware.prepare'),
    save: (bytes, name) => saveTo(outDir, 'firmware.save', bytes, name),
    patchkitWasm: () => wasm('patchkit.wasm'),
    bindBundle: () => wasm('modules/bind.json'),
    sampleWasm: () => wasm('dksample.wasm'),
    dkprofWasm: () => wasm('dkprof.wasm'),
  };
}

/** `EmuRpc` over the control socket of a running emulator; connected at the first call. The session token of the webview has no meaning here. */
function nodeRpc(socket: string | undefined): EmuRpc & { close(): void } {
  let client: Promise<Client> | undefined;
  return {
    async call<T>(method: string, params: Record<string, unknown> = {}) {
      if (!socket) throw new Error('no control socket given (start a headless emulator: mise run emu:serve)');
      client ??= connect(socket);
      return (await client).call<T>(method, params);
    },
    close() {
      void client?.then((c) => c.close());
    },
  };
}

/** The app's emulator session commands are the desktop's; headless code uses `rpc`. */
const nodeEmu: EmuHost = {
  setSource: unsupported('emu.setSource'),
  pickSyx: unsupported('emu.pickSyx'),
  sourceInfo: unsupported('emu.sourceInfo'),
  buildInfo: unsupported('emu.buildInfo'),
  launch: unsupported('emu.launch'),
  inApp: () => false,
  startup: unsupported('emu.startup'),
  step: unsupported('emu.step'),
  tap: unsupported('emu.tap'),
  stop: unsupported('emu.stop'),
  audioStatus: unsupported('emu.audioStatus'),
  saveDiagnostics: unsupported('emu.saveDiagnostics'),
  stageSyx: unsupported('emu.stageSyx'),
  card: {
    reset: unsupported('emu.card.reset'),
    stage: unsupported('emu.card.stage'),
    load: unsupported('emu.card.load'),
  },
};

const nodeShell: ShellHost = { openCardFolder: unsupported('shell.openCardFolder') };

/** A Host for a command line. `close()` ends the control socket, if it was used. */
export function createNodeHost(options: NodeHostOptions): Host & { close(): void } {
  const rpc = nodeRpc(options.socket);
  return {
    fs: nodeFs(options.outDir),
    library: nodeLibrary(options.root),
    ports: { midi: nodeMidi(options.dkmidiBin), bulk: nodeBulk(options.dkbulkBin) },
    firmware: nodeFirmware(options.root, options.outDir),
    emu: nodeEmu,
    rpc,
    shell: nodeShell,
    close: () => rpc.close(),
  };
}
