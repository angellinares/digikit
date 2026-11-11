#!/usr/bin/env node
// A live check of the Memory panel against a RUNNING emulator: the desktop pages in headless Chromium (the live preview,
// apps/desktop/ui/preview/live.vite.config.ts), their `emu_call` going to the daemon's control socket. It measures what the mock hid:
// whether the panel's read completes, how long a Refresh takes, how many control calls the page makes, and the emulator's real-time
// factor (audio seconds produced per wall second, `status.audio.source_audio_seconds`) while the panel is open against idle.
//
//   uv run python -m tools.devtools.emu_live --device dn2 --run-dir DIR        # prints the socket
//   DIGI_LIVE_SOCKET=SOCKET pnpm --filter @digi/desktop exec vite --config ui/preview/live.vite.config.ts --port 5298
//   node tools/devtools/memory_live.mjs SOCKET [--url http://127.0.0.1:5298/] [--seconds 10] [--out DIR] [--check] [--entry live.html]
//
// One JSON line with the figures; `--check` exits 1 unless the panel read within a second, a Refresh and the read after a visit to the Transfer
// page took under a second, collapsed panels made no call and the real-time factor with the panel open is within 10% of idle. `--out DIR` writes screenshots of the Emulator page.
import fs from 'node:fs';
import path from 'node:path';
import { connect as emuConnect } from './emu_midi.mjs';
import { closeTab, connect, evaluate, launchChrome, sleep } from './cdp.mjs';

const args = process.argv.slice(2);
const flag = (name, fallback) => {
  const i = args.indexOf(`--${name}`);
  return i < 0 ? fallback : args[i + 1];
};
const socket = args[0];
if (!socket || socket.startsWith('--')) {
  console.error('usage: node tools/devtools/memory_live.mjs SOCKET [--url URL] [--seconds N] [--out DIR] [--check]');
  process.exit(2);
}
const base = flag('url', 'http://127.0.0.1:5298/');
const seconds = Number(flag('seconds', 10));
const out = flag('out', '');
const port = Number(flag('port', 9622));
const entry = flag('entry', 'live.html');

const emu = await emuConnect(socket);
/** Audio seconds per wall second over `ms` (the daemon's `status.audio.source_audio_seconds`). */
const rtf = async (ms) => {
  const read = async () => (await emu.call('status')).audio?.source_audio_seconds;
  const t0 = performance.now();
  const a = await read();
  await sleep(ms);
  const b = await read();
  return (b - a) / ((performance.now() - t0) / 1000);
};
const stats = async (reset = false) => (await fetch(`${base}__stats${reset ? '?reset=1' : ''}`)).json();
const round = (x, n = 3) => Math.round(x * 10 ** n) / 10 ** n;

const chrome = launchChrome(port, flag('chrome'));
const tab = await connect(port);
const { cdp } = tab;
const result = {};
try {
  await cdp.send('Page.enable');
  await cdp.send('Emulation.setDeviceMetricsOverride', {
    width: 1440,
    height: 900,
    deviceScaleFactor: 1,
    mobile: false,
  });
  // idle: the same page with no panels (its status ticks, like the app's frame pump, stay)
  await cdp.send('Page.navigate', { url: `${base}${entry}?page=emulator&idle=1` });
  await sleep(2000);
  const idle = await rtf(6000);
  result.idle_rtf = round(idle);
  await stats(true);
  // the column's open choice and tab outlive a run: start from the Memory tab, open
  await cdp.send('Page.navigate', { url: `${base}${entry}?page=emulator` });
  const t0 = performance.now();
  const bars = () => evaluate(cdp, "document.querySelectorAll('.memory-panel .mem-bar').length");
  const label = () => evaluate(cdp, "document.querySelector('.memory-panel .acts button')?.textContent ?? ''");
  await sleep(500);
  await evaluate(
    cdp,
    "(() => { const f = document.querySelector('.side-fold'); if (f?.getAttribute('aria-expanded') === 'false') f.click(); })()",
  );
  await sleep(100);
  await evaluate(
    cdp,
    "[...document.querySelectorAll('.side-tab')].find((t) => t.textContent.trim() === 'Memory')?.click()",
  );
  // the first read: how long until the bars are there (up to 20 s)
  let firstMs = null;
  while (performance.now() - t0 < 20000) {
    if ((await bars()) > 0) {
      firstMs = Math.round(performance.now() - t0);
      break;
    }
    await sleep(50);
  }
  result.first_read_ms = firstMs;
  // the real-time factor with the page open, and the control calls it made meanwhile
  await stats(true);
  const open = await rtf(seconds * 1000);
  const calls = await stats();
  const frames = await evaluate(cdp, 'window.__live.frames()');
  Object.assign(result, {
    open_rtf: round(open),
    rtf_ratio: round(open / idle, 2),
    calls_per_s: Object.fromEntries(Object.entries(calls).map(([k, v]) => [k, round(v / seconds, 1)])),
    status_frames: frames,
    button: await label(),
    bars: await bars(),
  });
  // one Refresh: how long it takes (the button says "Reading…" meanwhile)
  const r0 = performance.now();
  await evaluate(cdp, "document.querySelector('.memory-panel .acts button')?.click()");
  await sleep(30);
  while (performance.now() - r0 < 20000 && (await label()) !== 'Refresh') await sleep(20);
  result.refresh_ms = Math.round(performance.now() - r0);
  result.refresh_done = (await label()) === 'Refresh';
  // the Transfer page reads over the same session while the Memory panel is on the page behind it: back on the Emulator page the panel
  // reads again and completes (two sessions would take each other's replies)
  await evaluate(cdp, "window.__live.page('transfer')");
  await sleep(3000);
  await evaluate(cdp, "window.__live.page('emulator')");
  const b0 = performance.now();
  while (performance.now() - b0 < 20000 && !((await label()) === 'Refresh' && (await bars()) > 0)) await sleep(20);
  result.back_from_transfer_ms = Math.round(performance.now() - b0);
  result.back_from_transfer_bars = await bars();
  // a folded column reads nothing: folded, 4 s, and the page's control calls (the harness's own `status` poll aside)
  await evaluate(
    cdp,
    "(() => { const f = document.querySelector('.side-fold'); if (f?.getAttribute('aria-expanded') === 'true') f.click(); })()",
  );
  await sleep(300);
  await stats(true);
  await sleep(4000);
  const quiet = await stats();
  delete quiet.status;
  result.collapsed_calls_in_4s = quiet;
  await evaluate(
    cdp,
    "(() => { const f = document.querySelector('.side-fold'); if (f?.getAttribute('aria-expanded') === 'false') f.click(); })()",
  );
  await sleep(500);
  if (out) {
    fs.mkdirSync(out, { recursive: true });
    const shot = await cdp.send('Page.captureScreenshot', { format: 'png' });
    fs.writeFileSync(path.join(out, 'live-emulator-page.png'), Buffer.from(shot.data, 'base64'));
  }
} finally {
  await closeTab(port, tab);
  await chrome.stop();
  emu.close();
}
console.log(JSON.stringify(result));
if (args.includes('--check')) {
  const ok =
    result.first_read_ms !== null &&
    result.first_read_ms < 1000 + 600 && // page load and the first poll of the frame loop included
    result.refresh_done &&
    result.refresh_ms < 1000 &&
    result.back_from_transfer_ms < 1000 &&
    Object.keys(result.collapsed_calls_in_4s).length === 0 &&
    result.rtf_ratio >= 0.9;
  process.exit(ok ? 0 : 1);
}
process.exit(0);
