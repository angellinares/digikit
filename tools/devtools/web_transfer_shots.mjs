#!/usr/bin/env node
// Screenshots of the site's Transfer page (apps/web/src/pages/transfer.astro) in headless Chromium against a running `astro preview` or dev
// server, with a fake Web MIDI device: a test shim injected into each page (nothing of it is in the site). The shim's port feeds a FakeDevice
// (packages/test-support/src/fakedevice.ts, the digikit protocol byte for byte) in this process, so the page runs its real session code.
//
//   node tools/devtools/web_transfer_shots.mjs --url http://127.0.0.1:4399/ [--out DIR] [--chrome PATH] [--port 9622] [scene ...]
//   node tools/devtools/web_transfer_shots.mjs --list
import fs from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import { closeTab, connect, evaluate, launchChrome, sleep } from './cdp.mjs';
import { FakeDevice } from '../../packages/test-support/src/fakedevice.ts';

const args = process.argv.slice(2);
const opt = (name, fallback) => {
  const i = args.indexOf(`--${name}`);
  return i < 0 ? fallback : args[i + 1];
};
const valueFlags = new Set(['--out', '--url', '--chrome', '--port']);
const names = args.filter((a, i) => !a.startsWith('--') && !valueFlags.has(args[i - 1] ?? ''));
const here = path.dirname(fileURLToPath(import.meta.url));
const site = path.resolve(here, '../../apps/web/public/modules');
const base = opt('url', 'http://127.0.0.1:4399/').replace(/\/?$/, '/');
const out = path.resolve(opt('out', path.resolve(here, '../../out/runs/web-transfer-shots')));
const port = Number(opt('port', 9622));

const WIDE = [1440, 900];
const NARROW = [700, 800];
const PORT = 'Elektron Digitakt II';

/** The fake card: the pluck module is on it. */
function device(delay) {
  const dev = new FakeDevice();
  dev.os = '1.17';
  dev.pageSize = 8;
  dev.arenaLimit = 256 * 1024; // room for every module of the site
  dev.dspLimit = 512 * 1024;
  dev.arenaLargest = dev.arenaLimit;
  dev.dspLargest = dev.dspLimit;
  dev.files.set('modules/pluck.dkm', new Uint8Array(fs.readFileSync(path.join(site, 'dt2/pluck.dkm'))));
  dev.delay = delay;
  return dev;
}

/** The shim: Web MIDI with one port (or none, or no API, or a denied permission). */
const shim = (cfg) => `(() => {
  const cfg = ${JSON.stringify(cfg)};
  if (cfg.api === 'none') { Object.defineProperty(Navigator.prototype, 'requestMIDIAccess', { value: undefined, configurable: true }); return; }
  if (cfg.api === 'denied') { navigator.requestMIDIAccess = () => Promise.reject(Object.assign(new Error('denied'), { name: 'SecurityError' })); return; }
  const b64 = (u8) => { let s = ''; for (let i = 0; i < u8.length; i += 8192) s += String.fromCharCode(...u8.subarray(i, i + 8192)); return btoa(s); };
  const input = { id: 'in-1', name: cfg.name, manufacturer: 'Elektron', state: 'connected', onmidimessage: null, open: async () => {} };
  const output = { id: 'out-1', name: cfg.name, manufacturer: 'Elektron', state: 'connected', open: async () => {}, send: (d) => window.__midiSend(b64(Uint8Array.from(d))) };
  const access = { inputs: new Map(cfg.ports ? [['in-1', input]] : []), outputs: new Map(cfg.ports ? [['out-1', output]] : []), onstatechange: null };
  window.__midiRecv = (s) => { const u = Uint8Array.from(atob(s), (c) => c.charCodeAt(0)); input.onmidimessage && input.onmidimessage({ data: u }); };
  window.__unplug = () => { input.state = output.state = 'disconnected'; access.onstatechange && access.onstatechange({ port: output }); };
  navigator.requestMIDIAccess = async () => access;
})();`;

const click = (text, exact = true) =>
  `(() => { const b = [...document.querySelectorAll('button, a')].find((e) => ${exact ? 'e.textContent.trim() === ' + JSON.stringify(text) : 'e.textContent.includes(' + JSON.stringify(text) + ')'}); if (!b) throw new Error('no button ' + ${JSON.stringify(text)}); b.click(); })()`;
const tick = (label) =>
  `(async () => { for (let i = 0; i < 80; i++) { const c = document.querySelector('input[aria-label=${JSON.stringify(label)}]'); if (c) return c.click(); await new Promise((r) => setTimeout(r, 250)); } throw new Error('no tick ${label}'); })()`;

const SCENES = [
  { name: 'not-connected', size: WIDE, cfg: { api: 'ok', ports: false }, wait: 'Not connected' },
  { name: 'connected', size: WIDE, cfg: { api: 'ok', ports: true }, wait: 'Connected' },
  {
    name: 'installing',
    size: WIDE,
    cfg: { api: 'ok', ports: true },
    delay: 250,
    wait: 'Connected',
    steps: [tick('Install freqshift'), click('Install (1)', false)],
    after: 1200,
  },
  {
    name: 'source',
    size: WIDE,
    cfg: { api: 'ok', ports: true },
    wait: 'Connected',
    steps: ["document.querySelector('.rowlink').click()"],
    after: 400,
  },
  { name: 'narrow', size: NARROW, cfg: { api: 'ok', ports: true }, wait: 'Connected' },
  { name: 'narrow-not-connected', size: NARROW, cfg: { api: 'ok', ports: false }, wait: 'Not connected' },
  {
    name: 'narrow-source',
    size: NARROW,
    cfg: { api: 'ok', ports: true },
    wait: 'Connected',
    steps: ["document.querySelector('.rowlink').click()"],
    after: 400,
  },
  {
    name: 'unplugged',
    size: WIDE,
    cfg: { api: 'ok', ports: true },
    wait: 'Connected',
    steps: ['window.__unplug()'],
    after: 600,
  },
  { name: 'no-webmidi', size: WIDE, cfg: { api: 'none' }, wait: 'no Web MIDI' },
  { name: 'denied', size: WIDE, cfg: { api: 'denied' }, wait: 'not allowed' },
];

if (args.includes('--list')) {
  console.log(SCENES.map((s) => s.name).join('\n'));
  process.exit(0);
}

fs.mkdirSync(out, { recursive: true });
const chrome = launchChrome(port, opt('chrome'));
let failed = 0;
try {
  for (const scene of SCENES.filter((s) => !names.length || names.includes(s.name))) {
    const tab = await connect(port);
    const { cdp } = tab;
    const dev = device(scene.delay ?? 0);
    let draining = false;
    cdp.ws.addEventListener('message', (m) => {
      const msg = JSON.parse(m.data);
      if (msg.method !== 'Runtime.bindingCalled' || msg.params.name !== '__midiSend') return;
      dev.send(Uint8Array.from(Buffer.from(msg.params.payload, 'base64')));
      if (draining) return;
      draining = true;
      const drain = async () => {
        while (dev.queue.length) {
          if (scene.delay) await sleep(scene.delay);
          const r = dev.queue.shift();
          await cdp.send('Runtime.evaluate', {
            expression: `window.__midiRecv(${JSON.stringify(Buffer.from(r).toString('base64'))})`,
          });
        }
      };
      void drain().finally(() => (draining = false));
    });
    await cdp.send('Page.enable');
    await cdp.send('Runtime.enable');
    await cdp.send('Runtime.addBinding', { name: '__midiSend' });
    await cdp.send('Page.addScriptToEvaluateOnNewDocument', { source: shim({ ...scene.cfg, name: PORT }) });
    await cdp.send('Emulation.setDeviceMetricsOverride', {
      width: scene.size[0],
      height: scene.size[1],
      deviceScaleFactor: 1,
      mobile: false,
    });
    await cdp.send('Page.navigate', { url: `${base}transfer/` });
    let note = '';
    try {
      for (let i = 0; i < 80; i++) {
        const ok = await evaluate(cdp, `document.body.innerText.includes(${JSON.stringify(scene.wait)})`).catch(
          () => false,
        );
        if (ok) break;
        await sleep(250);
        if (i === 79) throw new Error(`never showed "${scene.wait}"`);
      }
      await sleep(600);
      for (const step of scene.steps ?? []) {
        await evaluate(cdp, step);
        await sleep(150);
      }
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
