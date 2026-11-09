// The Firmware page's flash, headless, against an emulated device that shows OS UPGRADE: the same `runFlash` the page runs (firmware-flow.ts), the Rust
// sender through patchkit.wasm, the image `prepare` made, the answers asked through `DkSession`. The emulator's clock is the guest's (132,000
// instructions a millisecond), so the sender's 2 ms are guest milliseconds; the port "goes away" while the guest flashes and resets, and "comes
// back" with the `firmware.loaded` event. Skips unless DIGI_FW_EMU_SOCKET names a control socket; `tools/emulator/os_upgrade_rig.py ... --exec`
// opens OS UPGRADE and runs it (DIGI_FW_EMU_FIRMWARE: the stock file, DIGI_FW_EMU_RESTORE=1: send the stock file itself).
// The default is the RPC route (`runRpcFlash`: the rig run with `--rpc --exec`, the device stays on its main screen). DIGI_FW_EMU_ROUTE=menu runs the
// OS UPGRADE menu route (`runFlash`);
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import { test } from 'vitest';
import { connect } from '../../../packages/control-client/src/client.ts';
import { DkSession, EmuUsbTransport, decode8in7, splitMessages } from '@digi/dk-session';
import { askSession } from '../src/firmware/firmware-ask.ts';
import { LIMITS, runFlash, runRpcFlash, type FlowOps, type RpcOps } from '../src/firmware/firmware-flow.ts';
import type { FlashState } from '../src/firmware/firmware-core.ts';
import { PatchKit, type PreparedFlash } from '../src/patch/patchkit-core.ts';

const socket = process.env.DIGI_FW_EMU_SOCKET;
const NAME = 'Digitakt II';
// the fake port list steps the guest while it answers, which takes real time
const SLOW = { ...LIMITS, portsMs: 3_600_000 };
const INSTR_PER_MS = 132_000;
const FLASH_BASE = 0x80000;

const hex = (b: Uint8Array) => Array.from(b, (x) => x.toString(16).padStart(2, '0')).join('');
const bytesOf = (h: string) => Uint8Array.from(h.match(/../g) ?? [], (x) => parseInt(x, 16));

/** The container the device writes to flash: the data messages' bytes without the length and checksum words. */
function containerOf(p: PreparedFlash): Uint8Array {
  const chunks: Uint8Array[] = [];
  for (let i = 1; i < p.ends.length - 1; i++) {
    const m = p.image.subarray(p.ends[i - 1], p.ends[i]);
    chunks.push(decode8in7(m.subarray(10, 126)));
  }
  const stream = Uint8Array.from(chunks.flatMap((c) => Array.from(c)));
  const total = new DataView(stream.buffer).getUint32(0);
  return stream.subarray(8, 8 + total);
}

test.skipIf(!socket)(
  'the Firmware page flashes an emulated device and the device reports the image it was sent',
  async () => {
    const root = (p: string) => fileURLToPath(new URL(`../../../${p}`, import.meta.url));
    const stockFile = process.env.DIGI_FW_EMU_FIRMWARE ?? root('firmware/Digitone_II_OS1.12.syx');
    const restore = process.env.DIGI_FW_EMU_RESTORE === '1';
    const kit = await PatchKit.create(
      new Uint8Array(readFileSync(root('apps/web/public/patchkit.wasm'))),
      readFileSync(root('apps/web/public/modules/bundle.json'), 'utf8'),
    );
    const p = kit.prepare(new Uint8Array(readFileSync(stockFile)), restore);
    assert.equal(p.ok, true, JSON.stringify(p.checks.filter((c) => !c.ok)));
    const sendTotal = p.ends.length;

    const client = await connect(socket!);
    await client.eventsSubscribe();
    let used = 0;
    const step = async (n: number) => {
      await client.step(n);
      used += n;
    };
    await client.call('midi.usb.out', {}); // starts the USB capture
    const call = <T>(method: string, params: Record<string, unknown> = {}) => client.call<T>(method, params);
    const usb = new EmuUsbTransport(call, { stepInstructions: 20_000_000 });

    type Phase = 'sending' | 'flashing' | 'gone' | 'back';
    let phase: Phase = 'sending';
    let wall = 0; // real milliseconds spent waiting for the boot job (the guest clock does not move then)
    const ops: FlowOps = {
      sender: async (total) => kit.sender(total),
      async send(m) {
        await call('midi.usb.in', { bytes: hex(m), chunk: Math.min(Math.max(m.length, 1), 384) });
      },
      now: () => used / INSTR_PER_MS + wall,
      async sleep(ms) {
        try {
          await step(ms * INSTR_PER_MS);
        } catch {
          // the cold boot is a job of its own that refuses steps until it is done
          await new Promise((r) => setTimeout(r, 300));
          wall += 300;
        }
      },
      async release() {
        phase = 'flashing';
      },
      port: NAME,
      async ports() {
        if (phase === 'sending') return [NAME];
        if (phase === 'flashing') {
          // the device validates, erases and programs; the port is gone once the programming has stopped
          let last = [0, 0];
          let stable = 0;
          for (let i = 0; i < 300 && stable < 2; i++) {
            await step(50_000_000);
            const st = await call<{ erases: number; programs: number }>('flash.status');
            const now = [st.erases, st.programs];
            stable = st.programs > 0 && now[0] === last[0] && now[1] === last[1] ? stable + 1 : 0;
            last = now;
          }
          assert.ok(last[1]! > 0, 'the device wrote nothing to its flash');
          const want = containerOf(p);
          const got = new Uint8Array(want.length);
          for (let at = 0; at < want.length; at += 1 << 20) {
            const n = Math.min(1 << 20, want.length - at);
            const r = await call<{ hex: string }>('flash.read', { offset: FLASH_BASE + at, length: n });
            got.set(bytesOf(r.hex), at);
          }
          assert.deepEqual(got, want, 'the flash holds the container of the image');
          console.info(
            `flash: ${last[0]} erases, ${last[1]} programs; the container of ${want.length} bytes equals the image's`,
          );
          phase = 'gone';
          return [];
        }
        if (phase === 'gone') {
          for (const ev of client.drainEvents()) {
            if (ev.kind === 'firmware.loaded' && (ev as { ok?: boolean }).ok === true) phase = 'back';
          }
          if (phase === 'gone') {
            await ops.sleep(400);
            return [];
          }
        }
        return [NAME];
      },
      async ask() {
        const a = await askSession(new DkSession(usb, { timeoutMs: 1500, retries: 0 }));
        return { os: a.os, runtime: a.runtime, imageId: a.imageId };
      },
    };

    const menu = process.env.DIGI_FW_EMU_ROUTE === 'menu';
    // the RPC route: one request out through the USB model, the next message the guest sends in, in guest time
    let rx: Uint8Array = new Uint8Array();
    const ready: Uint8Array[] = [];
    const rpcOps: RpcOps = {
      machine: async (image, chunk) => kit.rpcFlash(image, chunk),
      async send(m) {
        await call('midi.usb.in', { bytes: hex(m), chunk: 384, period: 16_500 });
      },
      async recv(ms) {
        const end = used + ms * INSTR_PER_MS;
        for (;;) {
          const m = ready.shift();
          if (m) return m;
          const { hex: got } = await call<{ hex?: string }>('midi.usb.out');
          if (got) {
            const joined = new Uint8Array(rx.length + got.length / 2);
            joined.set(rx);
            joined.set(bytesOf(got), rx.length);
            const split = splitMessages(joined);
            rx = split.rest;
            ready.push(...split.messages);
            continue;
          }
          if (used >= end) return null;
          await step(400_000);
        }
      },
      now: ops.now,
      sleep: ops.sleep,
      release: ops.release,
      port: ops.port,
      ports: ops.ports,
      ask: ops.ask,
    };
    const states: FlashState[] = [];
    const record = (s: FlashState) => {
      if (states.at(-1)?.phase !== s.phase)
        console.info(`firmware page: ${s.phase}${'stage' in s ? ` (${s.stage})` : ''}`);
      states.push(s);
    };
    const final = await (menu
      ? runFlash(ops, p, new AbortController().signal, record, SLOW)
      : runRpcFlash(rpcOps, p, new AbortController().signal, record, SLOW));
    console.info(`final: ${JSON.stringify(final)}`);
    console.info(
      `image id the patch made: ${p.imageId}; sent ${menu ? `${sendTotal} messages` : `${p.image.length} bytes`}, guest ${Math.round(used / INSTR_PER_MS)} ms`,
    );
    client.close();
    assert.equal(final.phase, 'done', JSON.stringify(final));
  },
  40 * 60_000,
);
