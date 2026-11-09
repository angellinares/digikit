// The site's side of the Firmware page (env.ts): patchkit.wasm in a worker patches and checks the image (the same worker as the old Patch page),
// a second instance on the page's own thread drives the Rust sender, and a download link gives the .syx. The MIDI side is the Transfer
// pane's (../transfer/web-env.ts). Every URL goes through BASE_URL.
import { putHandoff } from '../patch/handoff';
import { PatchKit, type PreparedFlash } from '../patch/patchkit-core';
import '../transfer/web-env';
import { setFirmwareEnv, type FirmwareEnv } from './env';
import './firmware-site.css';

const BASE = import.meta.env.BASE_URL.replace(/\/?$/, '/');

interface Reply {
  type: string;
  id?: number;
  error?: string;
  prepared?: Omit<PreparedFlash, 'image'> & { image: ArrayBuffer };
}

let worker: Worker | undefined;
let nextId = 1;
const pending = new Map<number, { resolve: (r: Reply) => void; reject: (e: Error) => void }>();
let ready: Promise<void> | undefined;

function patchWorker(): Promise<Worker> {
  if (!worker) {
    const w = new Worker(new URL('../patch/patch-worker.ts', import.meta.url), { type: 'module' });
    worker = w;
    ready = new Promise<void>((resolve, reject) => {
      w.onmessage = ({ data }: MessageEvent<Reply>) => {
        if (data.type === 'ready') return resolve();
        const waiter = data.id === undefined ? undefined : pending.get(data.id);
        if (!waiter) return reject(new Error(data.error ?? 'The patch tool failed to start.'));
        pending.delete(data.id!);
        if (data.type === 'error') waiter.reject(new Error(data.error));
        else waiter.resolve(data);
      };
      w.onerror = (e) => reject(new Error(`The patch tool failed to start: ${e.message}`));
      w.postMessage({ type: 'init', wasmUrl: `${BASE}patchkit.wasm`, bundleUrl: `${BASE}modules/bundle.json` });
    });
    ready.catch(() => {
      worker = undefined;
      ready = undefined;
    });
  }
  return ready!.then(() => worker!);
}

let senderKit: Promise<PatchKit> | undefined;
/** The sender needs no firmware or modules: an empty bundle. */
function kit(): Promise<PatchKit> {
  senderKit ??= fetch(`${BASE}patchkit.wasm`)
    .then((r) => {
      if (!r.ok) throw new Error('patch tool files are missing from this site');
      return r.arrayBuffer();
    })
    .then((b) => PatchKit.create(b, '{"ports":[],"modules":[]}'))
    .catch((e) => {
      senderKit = undefined;
      throw e;
    });
  return senderKit;
}

export const webFirmwareEnv: FirmwareEnv = {
  async prepare(stock, restore, set = '') {
    const w = await patchWorker();
    const id = nextId++;
    const reply = await new Promise<Reply>((resolve, reject) => {
      pending.set(id, { resolve, reject });
      const copy = stock.slice().buffer;
      w.postMessage({ type: 'prepare', id, firmware: copy, restore, set }, [copy]);
    });
    const p = reply.prepared!;
    return { ...p, image: new Uint8Array(p.image) };
  },
  async sender(total) {
    return (await kit()).sender(total);
  },
  async rpc(image, chunk) {
    return (await kit()).rpcFlash(image, chunk);
  },
  async saveImage(name, bytes) {
    const url = URL.createObjectURL(new Blob([bytes.slice().buffer], { type: 'application/octet-stream' }));
    const a = document.createElement('a');
    a.href = url;
    a.download = name;
    document.body.append(a);
    a.click();
    a.remove();
    setTimeout(() => URL.revokeObjectURL(url), 10_000);
    return name;
  },
  async runInEmulator(name, device, bytes) {
    await putHandoff({ name, device, bytes: bytes.slice().buffer });
    location.assign(`${BASE}emulator/?handoff=1`);
  },
  log(run) {
    (run.level === 'error' ? console.warn : console.info)(`[firmware] ${run.text}`, run.detail ?? '');
  },
};

setFirmwareEnv(webFirmwareEnv);
