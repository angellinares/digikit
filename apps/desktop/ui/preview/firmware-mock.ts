// The Firmware page in the preview (`?page=firmware`): the Tauri commands it calls, answered here. Patching and checking are the real Rust code,
// patchkit.wasm with the site's module bundle (the page's `firmware_prepare` answer has the format of src-tauri/src/firmware.rs); the MIDI port is
// a fake device that speaks the two protocols the page uses (SoftwareVersion of the stock firmware, INFO of the runtime), takes the OS UPGRADE
// stream and the RPC flash route (Start, Write, End; apps/web/tests/fakeflash.mjs), and restarts after the last message: its port goes away and comes back with the runtime and the image id of the stream.
// State from the address: fwdev=dt2|dn2 (the device; default dn2), fwstate=stock|runtime|noreply|none, fwos=1.12; the RPC route: fwwrite=ms per Write
// (default 0), fwforce=1 (all checks pass), fwerror=0..1 (the Write at this fraction of the image gets a device error).
import { decode8in7, encode8in7 } from '@digi/dk-session';
import { CMD } from '@digi/dk-session/gen_proto';
import { PatchKit } from '../../../web/src/patch/patchkit-core';
import { FakeUpgrade } from '../../../web/tests/fakeflash.mjs';
import bundleUrl from '../../../web/public/modules/bundle.json?url';
import wasmUrl from '../../../web/public/patchkit.wasm?url';

const q = new URLSearchParams(location.search);
export const page = q.get('page') === 'firmware';
export const fw = {
  dev: (q.get('fwdev') ?? 'dn2') as 'dt2' | 'dn2',
  state: q.get('fwstate') ?? 'stock',
  os: q.get('fwos') ?? ((q.get('fwdev') ?? 'dn2') === 'dt2' ? '1.17' : '1.12'),
  /** The port is gone for this long after the last message (ms), and the flash takes this long before it. */
  flashMs: Number(q.get('fwflash') ?? 3000),
  goneMs: Number(q.get('fwgone') ?? 4000),
  sent: 0,
  lastImageId: null as string | null,
  saved: [] as { name: string; size: number }[],
};
const PORT = fw.dev === 'dt2' ? 'Elektron Digitakt II' : 'Elektron Digitone II';

const upgrade = new FakeUpgrade({
  writeMs: Number(q.get('fwwrite') ?? 0),
  checkMs: Number(q.get('fwcheck') ?? 1200),
  endMs: Number(q.get('fwend') ?? 40),
  errorAt: q.get('fwerror') === null ? null : Number(q.get('fwerror')),
});

let runtime: { version: string; imageId: string | null } | null =
  fw.state === 'runtime' ? { version: '0.5.2', imageId: null } : null;
let offlineUntil = 0;
let queue: Uint8Array[] = [];
let waiter: (() => void) | undefined;
let wake: number | undefined;

/** `0.5.3` as the runtime's packed version word. */
const packed = (v: string) => {
  const [a = 0, b = 0, c = 0] = v.split('.').map(Number);
  return (a << 16) | (b << 8) | c;
};
const concat = (...p: ArrayLike<number>[]) => Uint8Array.from(p.flatMap((x) => Array.from(x)));
const cs = (s: string) => concat(new TextEncoder().encode(s), [0]);
const be32 = (n: number) => Uint8Array.of((n >>> 24) & 255, (n >>> 16) & 255, (n >>> 8) & 255, n & 255);

const online = () => fw.state !== 'none' && Date.now() >= offlineUntil;
export const ports = (): string[] => (online() ? [PORT, 'IAC Driver Bus 1'] : ['IAC Driver Bus 1']);

function push(m: Uint8Array) {
  queue.push(m);
  waiter?.();
}

function reply(cmd: number, seq: number, body: Uint8Array) {
  push(concat([0xf0, 0x7d, 0x64, 0x6b, 1, cmd | 0x40, seq], encode8in7(concat([0], body)), [0xf7]));
}

function onMessage(msg: Uint8Array) {
  if (!online() || fw.state === 'noreply') return;
  if (msg[1] === 0x00 && msg[2] === 0x20) {
    // the OS UPGRADE stream: header 0x7f/1, data 0x7e, footer 0x7f/2
    if (msg[6] === 0x7f && msg[7] === 1) fw.sent = 0;
    else if (msg[6] === 0x7e) fw.sent++;
    else if (msg[6] === 0x7f && msg[7] === 2) restart();
    if (msg[6] === 0x7e || msg[6] === 0x7f) return;
    // a stock MidiRpc request: SoftwareVersion (type 2), or the RPC flash route (0x50 to 0x52)
    const body = decode8in7(msg.subarray(6, msg.length - 1));
    if (body[4] >= 0x50 && body[4] <= 0x52) {
      const r = upgrade.handle(msg);
      if (!r) return;
      setTimeout(() => {
        push(r.message);
        if (r.commit === 1) restart();
      }, r.delayMs);
      return;
    }
    if (body[4] !== 2) return;
    const seq = body.subarray(0, 2);
    push(
      concat(
        [0xf0, 0x00, 0x20, 0x3c, 0x10, 0x00],
        encode8in7(concat(seq, seq, [2 | 0x80], cs('00'), cs('79'), cs(fw.os))),
        [0xf7],
      ),
    );
    return;
  }
  if (msg[1] === 0x7d && msg[5] === CMD.INFO && runtime) {
    const id = runtime.imageId ? Uint8Array.from(runtime.imageId.match(/../g)!.map((h) => parseInt(h, 16))) : [];
    reply(
      CMD.INFO,
      msg[6]!,
      concat(
        [0, 1, 0x02, 0x04],
        cs(fw.dev),
        be32(1 << 20),
        be32(32768),
        [0],
        be32(1),
        be32(1),
        be32(packed(runtime.version)),
        be32(0xffff),
        id,
      ),
    );
  }
}

/** After the footer: the device writes its flash, goes away, and comes back with the runtime and the image id the stream carried. */
function restart() {
  setTimeout(() => {
    offlineUntil = Date.now() + fw.goneMs;
    runtime = isRestore ? null : { version: patchedRuntime, imageId: fw.lastImageId };
  }, fw.flashMs);
}

let isRestore = false;
/** The runtime version the patched image carries (from the checks), so the fake device reports what the page expects. */
let patchedRuntime = '0.5.2';
/** The page asked for this image: what the device will report after it. */
export function expect(imageId: string | null, restore: boolean) {
  fw.lastImageId = imageId;
  isRestore = restore;
}

export const midi = {
  send(msg: Uint8Array) {
    onMessage(msg);
  },
  async recv(timeoutMs: number): Promise<ArrayBuffer> {
    if (!queue.length) {
      await new Promise<void>((resolve) => {
        waiter = resolve;
        wake = window.setTimeout(resolve, timeoutMs);
      });
      window.clearTimeout(wake);
      waiter = undefined;
    }
    const m = queue.shift();
    return m ? m.slice().buffer : new ArrayBuffer(0);
  },
  cancel() {
    waiter?.();
  },
  close() {
    queue = [];
  },
};

let kit: Promise<PatchKit> | undefined;
async function patchKit(): Promise<PatchKit> {
  kit ??= (async () => {
    const [wasm, bundle] = await Promise.all([fetch(wasmUrl), fetch(bundleUrl)]);
    return PatchKit.create(await wasm.arrayBuffer(), await bundle.text());
  })();
  return kit;
}

export const patchkitWasm = async (): Promise<ArrayBuffer> => (await fetch(wasmUrl)).arrayBuffer();

/** `firmware_prepare`: the real checks, packed as the Tauri command packs them. */
export async function prepare(stock: Uint8Array, restore: boolean, set = ''): Promise<ArrayBuffer> {
  const k = await patchKit();
  const p = k.prepare(stock, restore, set);
  expect(p.imageId, restore);
  if (p.runtime) patchedRuntime = String(p.runtime);
  if (q.get('fwforce') === '1') {
    // every check green: for the screenshots of the steps after the checks when the module bundle and patchkit.wasm of this tree do not match
    for (const c of p.checks) c.ok = true;
    p.ok = true;
  }
  if (q.get('fwfail') === '1') {
    // a failed check cannot be made from a real stock file: the screenshot of one changes the answer
    const c = p.checks.find((x) => x.name === 'DSP scan')!;
    c.ok = false;
    c.detail =
      '2 store(s) the SHARC+ ISA does not allow in section 7 (finding 49 section 10): block 103 pc 0xb8d6a9: DM store of I0 (DAG1); block 103 pc 0xb8d6ab: DM store of I1 (DAG1)';
    p.ok = false;
  }
  const json = new TextEncoder().encode(
    JSON.stringify({
      ok: p.ok,
      restore: p.restore,
      device: p.device,
      os: p.os,
      sha_stock: p.shaStock,
      sha_image: p.shaImage,
      image_id: p.imageId,
      runtime: p.runtime,
      pace_ms: p.paceMs,
      ends: p.ends,
      checks: p.checks,
    }),
  );
  const out = new Uint8Array(4 + json.length + p.image.length);
  new DataView(out.buffer).setUint32(0, json.length);
  out.set(json, 4);
  out.set(p.image, 4 + json.length);
  return out.buffer;
}

export function save(name: string, bytes: Uint8Array): string {
  fw.saved.push({ name, size: bytes.length });
  return `~/Downloads/${name}`;
}

(window as unknown as { __fw: typeof fw }).__fw = fw;
