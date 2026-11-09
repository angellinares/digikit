#!/usr/bin/env node
// Screenshots of the Firmware page in headless Chromium, over CDP, for both apps:
//   --app web      the site (apps/web/src/pages/firmware.astro) from a running `astro preview` or dev server; a fake Web MIDI device, a test shim
//                  injected into each page (nothing of it is in the site), whose port feeds a fake device in this process;
//   --app desktop  the workbench page in the UI preview (`mise run ui:preview`): the Tauri stand-in answers the page's commands with the real
//                  patchkit.wasm checks and a fake device (apps/desktop/ui/preview/firmware-mock.ts).
// Both patch the real stock files in firmware/ (they are not in the repository). Each scene writes a PNG; a scene whose step fails still gets its
// picture (the failure is printed).
//
//   node tools/devtools/firmware_shots.mjs --app web --url http://127.0.0.1:4399/ [--out DIR] [--chrome PATH] [--port 9633] [scene ...]
//   node tools/devtools/firmware_shots.mjs --app desktop --url http://127.0.0.1:5199/ [...]
//   node tools/devtools/firmware_shots.mjs --list
import fs from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import { closeTab, connect, evaluate, launchChrome, sleep } from './cdp.mjs';
import { FakeDevice, be32, concat, cs } from '../../apps/web/tests/fakedevice.mjs';
import { FakeUpgrade } from '../../apps/web/tests/fakeflash.mjs';
import { PatchKit } from '../../apps/web/src/patch/patchkit-core.ts';
import { CAP, CMD } from '../../packages/dk-session/src/gen_proto.ts';

const args = process.argv.slice(2);
const opt = (name, fallback) => {
  const i = args.indexOf(`--${name}`);
  return i < 0 ? fallback : args[i + 1];
};
const valueFlags = new Set(['--app', '--out', '--url', '--chrome', '--port']);
const names = args.filter((a, i) => !a.startsWith('--') && !valueFlags.has(args[i - 1] ?? ''));
const here = path.dirname(fileURLToPath(import.meta.url));
const repo = path.resolve(here, '../..');
const app = opt('app', 'web');
const base = opt('url', app === 'web' ? 'http://127.0.0.1:4399/' : 'http://127.0.0.1:5199/').replace(/\/?$/, '/');
const out = path.resolve(opt('out', path.resolve(repo, 'out/runs/firmware-shots', app)));
const port = Number(opt('port', 9633));

const WIDE = [1440, 900];
const TALL = [1440, 1180];
const NARROW = [700, 900];
const NARROW_TALL = [700, 1500];
const FILES = { dn2: 'Digitone_II_OS1.12.syx', dt2: 'Digitakt_II_OS1.17.syx' };
const fwPath = (dev) => path.join(repo, 'firmware', FILES[dev]);
const PORTS = { dn2: 'Elektron Digitone II', dt2: 'Elektron Digitakt II' };
const OS = { dn2: '1.12', dt2: '1.17' };

// ---- the scenes -------------------------------------------------------------------------------------------------------------------
// dev: the device on the port; file: which stock file the user picks; state: stock | runtime | none | noreply
const tick = (label) =>
  `(() => { const c = document.querySelector('input[aria-label=${JSON.stringify(label)}]') || [...document.querySelectorAll('label.tick')].find((l) => l.textContent.includes(${JSON.stringify(label)}))?.querySelector('input'); if (!c) throw new Error('no box ${label}'); c.click(); })()`;
const click = (text) =>
  `(() => { const b = [...document.querySelectorAll('button')].find((e) => e.textContent.trim() === ${JSON.stringify(text)}); if (!b) throw new Error('no button ' + ${JSON.stringify(text)}); b.click(); })()`;

const clickStarting = (text) =>
  `(() => { const b = [...document.querySelectorAll('button')].find((e) => e.textContent.trim().startsWith(${JSON.stringify(text)})); if (!b) throw new Error('no button ' + ${JSON.stringify(text)}); b.click(); })()`;
const menuLink = click('Use the OS UPGRADE menu instead');
const SCENES = [
  { name: 'step1-empty', size: WIDE, dev: 'dn2', file: null, wait: 'Connected' },
  { name: 'step2-checks', size: WIDE, dev: 'dn2', file: 'dn2', wait: 'Connected', ready: true },
  { name: 'step4-ready', size: WIDE, dev: 'dn2', file: 'dn2', wait: 'Connected', ready: true },
  { name: 'dt2-ready', size: WIDE, dev: 'dt2', file: 'dt2', wait: 'Connected', ready: true },
  { name: 'file-refused', size: WIDE, dev: 'dn2', file: 'junk', wait: 'Connected', after: 800 },
  {
    name: 'failed-check',
    size: WIDE,
    dev: 'dn2',
    file: 'dn2',
    wait: 'Connected',
    ready: true,
    desktopOnly: true,
    query: 'fwfail=1',
  },
  // the Default modules list (desktop only): installed and enabled after the flash; `defmods` makes the fake install slow or fail
  {
    name: 'defaults-open',
    size: WIDE,
    dev: 'dn2',
    file: 'dn2',
    wait: 'Connected',
    ready: true,
    desktopOnly: true,
    query: 'fwforce=1',
    steps: [clickStarting('Default modules')],
  },
  {
    name: 'defaults-unticked',
    size: WIDE,
    dev: 'dt2',
    file: 'dt2',
    wait: 'Connected',
    ready: true,
    desktopOnly: true,
    query: 'fwforce=1',
    steps: [clickStarting('Default modules'), 'eval:document.querySelector(".defmods-row input").click()'],
  },
  {
    name: 'defaults-installing',
    size: TALL,
    dev: 'dn2',
    file: 'dn2',
    wait: 'Connected',
    ready: true,
    desktopOnly: true,
    query: 'fwforce=1&defmods=slow',
    steps: [click('Flash')],
    until: 'Installing digikit/remote',
    within: 240_000,
    after: 300,
  },
  {
    name: 'defaults-done',
    size: TALL,
    dev: 'dn2',
    file: 'dn2',
    wait: 'Connected',
    ready: true,
    desktopOnly: true,
    query: 'fwforce=1',
    steps: [click('Flash')],
    until: 'Default modules installed.',
    within: 240_000,
  },
  {
    name: 'defaults-failed',
    size: TALL,
    dev: 'dn2',
    file: 'dn2',
    wait: 'Connected',
    ready: true,
    desktopOnly: true,
    query: 'fwforce=1&defmods=fail',
    steps: [click('Flash')],
    until: 'The firmware flash is done',
    within: 240_000,
  },
  { name: 'no-device', size: WIDE, dev: 'dn2', file: 'dn2', state: 'none', wait: 'Not connected', ready: true },
  { name: 'mismatch', size: WIDE, dev: 'dt2', file: 'dn2', wait: 'Connected', ready: true },
  // the RPC route (default): Flash sends the image, the device checks it, the page commits at once, the device flashes
  {
    name: 'send',
    size: TALL,
    dev: 'dn2',
    file: 'dn2',
    wait: 'Connected',
    ready: true,
    fake: { writeMs: 12 },
    steps: [click('Flash')],
  },
  {
    name: 'send-dt2',
    size: TALL,
    dev: 'dt2',
    file: 'dt2',
    wait: 'Connected',
    ready: true,
    fake: { writeMs: 12 },
    steps: [click('Flash')],
    after: 4500,
  },
  {
    name: 'checking',
    size: TALL,
    dev: 'dn2',
    file: 'dn2',
    wait: 'Connected',
    ready: true,
    fake: { writeMs: 3, checkMs: 60_000 },
    steps: [click('Flash')],
    until: 'is checking the image',
    after: 400,
  },
  {
    name: 'flashing',
    size: TALL,
    dev: 'dn2',
    file: 'dn2',
    wait: 'Connected',
    ready: true,
    fake: { writeMs: 3, endMs: 60_000 },
    steps: [click('Flash')],
    until: 'do not unplug',
    after: 400,
  },
  {
    name: 'done',
    size: TALL,
    dev: 'dn2',
    file: 'dn2',
    wait: 'Connected',
    ready: true,
    steps: [click('Flash')],
    until: 'Restarted',
    within: 240_000,
  },
  {
    name: 'error',
    size: TALL,
    dev: 'dn2',
    file: 'dn2',
    wait: 'Connected',
    ready: true,
    fake: { errorAt: 0.4 },
    steps: [click('Flash')],
    until: 'Checksum error',
    after: 400,
  },
  {
    name: 'cancelled',
    size: TALL,
    dev: 'dn2',
    file: 'dn2',
    wait: 'Connected',
    ready: true,
    fake: { writeMs: 12 },
    steps: [click('Flash'), 'sleep:3000', click('Cancel')],
    until: 'Nothing was written',
    after: 400,
  },
  {
    name: 'restore-ready',
    restore: true,
    size: WIDE,
    dev: 'dn2',
    file: 'dn2',
    state: 'runtime',
    wait: 'Connected',
    ready: true,
    steps: [tick('Restore stock'), 'wait:Stock image'],
  },
  {
    name: 'restore-done',
    restore: true,
    size: TALL,
    dev: 'dn2',
    file: 'dn2',
    state: 'runtime',
    wait: 'Connected',
    ready: true,
    steps: [tick('Restore stock'), 'wait:Stock image', click('Restore stock')],
    until: 'stock OS',
    within: 240_000,
  },
  // the OS UPGRADE menu route, kept as a fallback
  { name: 'menu-ready', size: WIDE, dev: 'dn2', file: 'dn2', wait: 'Connected', ready: true, steps: [menuLink] },
  {
    name: 'menu-ready-ticked',
    size: WIDE,
    dev: 'dn2',
    file: 'dn2',
    wait: 'Connected',
    ready: true,
    steps: [menuLink, tick('The device shows WAITING FOR SYSEX')],
  },
  {
    name: 'menu-flashing',
    size: TALL,
    dev: 'dn2',
    file: 'dn2',
    wait: 'Connected',
    ready: true,
    steps: [menuLink, tick('The device shows WAITING FOR SYSEX'), click('Flash')],
    after: 7000,
  },
  { name: 'narrow-empty', size: NARROW, dev: 'dn2', file: null, wait: 'Connected' },
  { name: 'narrow-ready', size: NARROW_TALL, dev: 'dn2', file: 'dn2', wait: 'Connected', ready: true },
  {
    name: 'narrow-send',
    size: NARROW_TALL,
    dev: 'dn2',
    file: 'dn2',
    wait: 'Connected',
    ready: true,
    fake: { writeMs: 12 },
    steps: [click('Flash')],
    after: 4000,
  },
  {
    name: 'narrow-flashing',
    size: NARROW_TALL,
    dev: 'dn2',
    file: 'dn2',
    wait: 'Connected',
    ready: true,
    fake: { writeMs: 3, endMs: 60_000 },
    steps: [click('Flash')],
    until: 'do not unplug',
    after: 400,
  },
];

if (args.includes('--list')) {
  console.log(SCENES.map((s) => s.name).join('\n'));
  process.exit(0);
}

// ---- the site's fake MIDI device ----------------------------------------------------------------------------------------------------

/** Web MIDI with one port (or none): the port's output goes to this process, replies come back through `__midiRecv`. */
const shim = (cfg) => `(() => {
  const cfg = ${JSON.stringify(cfg)};
  const b64 = (u8) => { let s = ''; for (let i = 0; i < u8.length; i += 8192) s += String.fromCharCode(...u8.subarray(i, i + 8192)); return btoa(s); };
  const input = { id: 'in-1', name: cfg.name, manufacturer: 'Elektron', state: 'connected', onmidimessage: null, open: async () => {} };
  const output = { id: 'out-1', name: cfg.name, manufacturer: 'Elektron', state: 'connected', open: async () => {}, send: (d) => window.__midiSend(b64(Uint8Array.from(d))) };
  const access = { inputs: new Map(cfg.ports ? [['in-1', input]] : []), outputs: new Map(cfg.ports ? [['out-1', output]] : []), onstatechange: null };
  window.__midiRecv = (s) => { const u = Uint8Array.from(atob(s), (c) => c.charCodeAt(0)); input.onmidimessage && input.onmidimessage({ data: u }); };
  window.__unplug = () => { input.state = output.state = 'disconnected'; access.onstatechange && access.onstatechange({ port: output }); };
  window.__replug = () => { input.state = output.state = 'connected'; access.onstatechange && access.onstatechange({ port: output }); };
  navigator.requestMIDIAccess = async () => access;
})();`;

/** The digikit device and the stock firmware's OS UPGRADE screen in one: SoftwareVersion always, INFO only with the runtime. */
class FwDevice extends FakeDevice {
  constructor(model, os, stock) {
    super();
    this.model = model;
    this.os = os;
    this.runtime = !stock;
    this.imageId = null;
    this.count = 0;
  }
  send(msg) {
    if (msg[1] === 0x7d && !this.runtime) return; // the stock firmware does not know INFO
    return super.send(msg);
  }
  handle(cmd, seq, body) {
    if (cmd !== CMD.INFO) return super.handle(cmd, seq, body);
    return this.reply(
      cmd,
      seq,
      0,
      concat(
        Uint8Array.of(0, 1, 0x02, 0x04),
        cs(this.model),
        be32(1 << 20),
        be32(32768),
        [0],
        be32(1),
        be32(1),
        be32(0x000502),
        be32(Object.values(CAP).reduce((a, bit) => a | (1 << bit), 0)),
        this.imageId ? Uint8Array.from(Buffer.from(this.imageId, 'hex')) : [],
      ),
    );
  }
}

// ---- the kit, to know the image id the device will report -----------------------------------------------------------------------------

let kit;
async function patchKit() {
  kit ??= PatchKit.create(
    new Uint8Array(fs.readFileSync(path.join(repo, 'apps/web/public/patchkit.wasm'))),
    fs.readFileSync(path.join(repo, 'apps/web/public/modules/bundle.json'), 'utf8'),
  );
  return kit;
}

const waitFor = async (cdp, js, what, ms = 40_000) => {
  const end = Date.now() + ms;
  while (Date.now() < end) {
    if (await evaluate(cdp, js).catch(() => false)) return;
    await sleep(250);
  }
  throw new Error(`never: ${what}`);
};
const hasText = (t) => `document.body.innerText.includes(${JSON.stringify(t)})`;

/** Put a file into the page's file input. */
async function chooseFile(cdp, file) {
  const { root } = await cdp.send('DOM.getDocument');
  const { nodeId } = await cdp.send('DOM.querySelector', {
    nodeId: root.nodeId,
    selector: 'input[aria-label="Stock firmware file"]',
  });
  await cdp.send('DOM.setFileInputFiles', { nodeId, files: [file] });
}

fs.mkdirSync(out, { recursive: true });
const junk = path.join(out, 'not-a-firmware.syx');
fs.writeFileSync(junk, Buffer.from(Array.from({ length: 3000 }, (_, i) => (i * 7) & 0x7f)));
const chrome = launchChrome(port, opt('chrome'));
let failed = 0;
try {
  for (const scene of SCENES.filter(
    (s) => (!names.length || names.includes(s.name)) && (app === 'desktop' || !s.desktopOnly),
  )) {
    const tab = await connect(port);
    const { cdp } = tab;
    const state = scene.state ?? 'stock';
    let note = '';
    let dev;
    await cdp.send('Page.enable');
    await cdp.send('Runtime.enable');
    await cdp.send('Emulation.setDeviceMetricsOverride', {
      width: scene.size[0],
      height: scene.size[1],
      deviceScaleFactor: 1,
      mobile: false,
    });
    let url;
    if (app === 'web') {
      dev = new FwDevice(scene.dev, OS[scene.dev], state !== 'runtime');
      if (state === 'runtime' && scene.file)
        dev.imageId = (
          await (await patchKit()).prepare(new Uint8Array(fs.readFileSync(fwPath(scene.file))), false)
        ).imageId;
      let draining = false;
      const flashAfter = 2500;
      const goneFor = 3500;
      const upgrade = new FakeUpgrade(scene.fake ?? {});
      const restartAfter = async () => {
        const k = await patchKit();
        const p = k.prepare(new Uint8Array(fs.readFileSync(fwPath(scene.file))), Boolean(scene.restore));
        setTimeout(async () => {
          await cdp.send('Runtime.evaluate', { expression: 'window.__unplug()' }).catch(() => {});
          setTimeout(async () => {
            dev.runtime = !p.restore;
            dev.imageId = p.imageId;
            await cdp.send('Runtime.evaluate', { expression: 'window.__replug()' }).catch(() => {});
          }, goneFor);
        }, flashAfter);
      };
      const deliver = (r) =>
        cdp.send('Runtime.evaluate', {
          expression: `window.__midiRecv(${JSON.stringify(Buffer.from(r).toString('base64'))})`,
        });
      cdp.ws.addEventListener('message', async (m) => {
        const msg = JSON.parse(m.data);
        if (msg.method !== 'Runtime.bindingCalled' || msg.params.name !== '__midiSend') return;
        const bytes = Uint8Array.from(Buffer.from(msg.params.payload, 'base64'));
        // the RPC flash route (0x50 to 0x52): the fake answers after its delay; End with commit 1 starts the restart
        if (FakeUpgrade.is(bytes)) {
          const r = upgrade.handle(bytes);
          if (!r) return;
          setTimeout(async () => {
            await deliver(r.message).catch(() => {});
            if (r.commit === 1) await restartAfter();
          }, r.delayMs);
          return;
        }
        // the OS UPGRADE stream (F0 00 20 3C <id> 00 7E|7F): counted; the footer starts the restart
        if (bytes[1] === 0x00 && bytes[2] === 0x20 && bytes[5] === 0x00 && (bytes[6] === 0x7e || bytes[6] === 0x7f)) {
          if (bytes[6] === 0x7f && bytes[7] === 2) await restartAfter();
          return;
        }
        dev.send(bytes);
        if (draining) return;
        draining = true;
        try {
          while (dev.queue.length) await deliver(dev.queue.shift());
        } finally {
          draining = false;
        }
      });
      await cdp.send('Runtime.addBinding', { name: '__midiSend' });
      await cdp.send('Page.addScriptToEvaluateOnNewDocument', {
        source: shim({ ports: state !== 'none', name: PORTS[scene.dev] }),
      });
      url = `${base}firmware/`;
    } else {
      const q = new URLSearchParams({
        page: 'firmware',
        fwdev: scene.dev,
        fwstate: state,
        fwos: OS[scene.dev],
        fwflash: '2500',
        fwgone: '3500',
      });
      if (scene.fake?.writeMs !== undefined) q.set('fwwrite', String(scene.fake.writeMs));
      if (scene.fake?.checkMs !== undefined) q.set('fwcheck', String(scene.fake.checkMs));
      if (scene.fake?.endMs !== undefined) q.set('fwend', String(scene.fake.endMs));
      if (scene.fake?.errorAt !== undefined) q.set('fwerror', String(scene.fake.errorAt));
      if (scene.query) for (const [k, v] of new URLSearchParams(scene.query)) q.set(k, v);
      // the desktop keeps the chosen target in localStorage: start on the MIDI port
      await cdp.send('Page.addScriptToEvaluateOnNewDocument', {
        source: `try { localStorage.setItem('digikit.target', JSON.stringify({ kind: 'midi', port: ${JSON.stringify(PORTS[scene.dev])} })); } catch {}`,
      });
      url = `${base}?${q}`;
    }
    await cdp.send('Page.navigate', { url });
    try {
      await waitFor(cdp, hasText(scene.wait), scene.wait);
      await sleep(500);
      if (scene.file === 'junk') await chooseFile(cdp, junk);
      else if (scene.file) await chooseFile(cdp, fwPath(scene.file));
      if (scene.ready) await waitFor(cdp, hasText('stock ✓'), 'the checks', 90_000);
      await sleep(400);
      for (const step of scene.steps ?? []) {
        if (step.startsWith('sleep:')) await sleep(Number(step.slice(6)));
        else if (step.startsWith('eval:')) await evaluate(cdp, step.slice(5));
        else if (step.startsWith('wait:')) await waitFor(cdp, hasText(step.slice(5)), step, 90_000);
        else await evaluate(cdp, step);
        await sleep(200);
      }
      if (scene.until) await waitFor(cdp, hasText(scene.until), scene.until, scene.within);
      await sleep(scene.after ?? 300);
    } catch (e) {
      note = ` (${e.message})`;
      failed++;
    }
    const shot = await cdp.send('Page.captureScreenshot', { format: 'png' });
    fs.writeFileSync(path.join(out, `${scene.name}.png`), Buffer.from(shot.data, 'base64'));
    console.log(`${scene.name}${note}`);
    await closeTab(port, tab);
  }
} finally {
  await chrome.stop();
}
process.exit(failed ? 1 : 0);
