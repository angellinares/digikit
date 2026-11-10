#!/usr/bin/env node
// Screenshots of the desktop UI pages in the preview (`mise run ui:preview`), headless Chromium over CDP, no dependencies.
// Each scene sets a state of the fake device through the address, optionally clicks, waits, and writes a PNG.
//
//   node tools/devtools/ui_shots.mjs [--out DIR (default out/runs/ui-shots/STAMP)] [--prefix before] [--url http://127.0.0.1:5199/] [--scale 1] [scene ...]
//   node tools/devtools/ui_shots.mjs --list
// With no scene names every scene runs. A scene whose step fails still gets its picture (the failure is printed): a missing control is
// itself what a before/after pair shows.
import fs from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import { closeTab, connect, evaluate as cdpEvaluate, launchChrome, sleep } from './cdp.mjs';

const args = process.argv.slice(2);
const opt = (name, fallback) => {
  const i = args.indexOf(`--${name}`);
  return i < 0 ? fallback : args[i + 1];
};
const valueFlags = new Set(['--out', '--prefix', '--url', '--scale', '--chrome', '--port']);
const names = args.filter((a, i) => !a.startsWith('--') && !valueFlags.has(args[i - 1] ?? ''));
const base = opt('url', 'http://127.0.0.1:5199/');
const stamp = new Date()
  .toISOString()
  .replace(/\.\d+Z$/, '')
  .replace(/[-:]/g, '')
  .replace('T', '-');
const out = opt('out', path.resolve(fileURLToPath(import.meta.url), '../../../out/runs/ui-shots', stamp));
const prefix = opt('prefix', '');
const scale = Number(opt('scale', 1));
const port = Number(opt('port', 9611));

// ---- the scenes ---------------------------------------------------------------------------------------------------------------------
const WIDE = [1440, 900];
const MIN = [700, 600];
/** The Memory panel remembers open or closed in localStorage, which outlives a scene: open it. */
const openMemory = (p) =>
  p.eval(
    "(() => { const b = document.querySelector('.memory-panel .card-toggle'); if (b && b.getAttribute('aria-expanded') === 'false') b.click(); })()",
  );
/** The Card panel remembers it too; the panels of the right-hand column: `want` is 'open' or 'closed'. */
const setPanel = (p, which, want) =>
  p.eval(
    `(() => { const b = document.querySelector('${which === 'memory' ? '.memory-panel' : '.card-panel:not(.memory-panel)'} .card-toggle'); if (b && (b.getAttribute('aria-expanded') === 'true') !== (${want === 'open'})) b.click(); })()`,
  );
/** Page helpers: `p.eval(js)`, `p.click(text)`, `p.tick(labelRegexSource)`, `p.hover(selector)`, `p.wait(ms)`. */
const SCENES = [
  { name: 'modules', q: 'mode=modules', size: WIDE },
  {
    name: 'modules-select-partial',
    q: 'mode=modules',
    size: WIDE,
    steps: async (p) => {
      await p.tick('^Install (Sine|Pluck)$');
      await p.tick('^Delete (Sine)$');
    },
  },
  {
    // fit: Sine is loaded on the fake device (200 KiB of the DSP area), so Wavetable does not fit next to it (the tag says so, the tooltip why) and FM 4-op is more than
    // the device has; the line above the list is the ticked modules together against the empty device: Pluck and Wavetable (679 KiB) are too much for the 512 KiB area
    name: 'modules-fit-ticked',
    q: 'mode=modules',
    size: WIDE,
    steps: async (p) => {
      await p.tick('^Install (Pluck|Wavetable)');
    },
  },
  {
    name: 'modules-fit-one',
    q: 'mode=modules',
    size: WIDE,
    steps: async (p) => {
      await p.tick('^Install Pluck$');
    },
  },
  {
    name: 'modules-uploading',
    q: 'mode=modules&speed=0.4',
    size: WIDE,
    steps: async (p) => {
      await p.tick('^Install (Wavetable|FM|Pluck)');
      await p.click(/^Install/);
      await p.wait(2500);
    },
  },
  {
    // the won't-fit tag's tooltip says what is short in the user-facing names; the storage line is under the device line (storage only)
    name: 'modules-fit-tooltips',
    q: 'mode=modules',
    size: WIDE,
    steps: async (p) => {
      p.say(
        'fit tooltips: ' +
          (await p.eval(
            "JSON.stringify([...document.querySelectorAll('.transfer-page .tag.warn, .transfer-page [title]')].map((e) => e.title).filter((t) => /short|fits/.test(t)))",
          )),
      );
      p.say('storage line: ' + (await p.eval("document.querySelector('.devhead .storage')?.textContent")));
    },
  },
  { name: 'modules-narrow', q: 'mode=modules', size: MIN },
  {
    name: 'modules-narrow-uploading',
    q: 'mode=modules&speed=0.4',
    size: MIN,
    steps: async (p) => {
      await p.tick('^Install (Wavetable|FM|Pluck)');
      await p.click(/^Install/);
      await p.wait(2500);
    },
  },
  { name: 'samples', q: 'mode=samples', size: WIDE },
  {
    // a folder with files: the armed Delete says its contents go; the second click shows the progress line with Cancel
    name: 'samples-delete-folder-armed',
    q: 'mode=samples',
    size: WIDE,
    steps: async (p) => {
      await p.tick('^Delete (Drums)$');
      await p.click(/^Delete/);
    },
  },
  {
    name: 'samples-delete-folder-running',
    q: 'mode=samples',
    size: WIDE,
    steps: async (p) => {
      await p.tick('^Delete (Drums)$');
      await p.click(/^Delete/);
      await p.click(/^Delete this folder/);
      await p.wait(200);
    },
  },
  {
    name: 'samples-select-partial',
    q: 'mode=samples',
    size: WIDE,
    steps: async (p) => {
      await p.tick('^Upload (kick|snare)');
      await p.tick('^Delete (Drums|init_tone)$');
    },
  },
  {
    name: 'samples-uploading',
    q: 'mode=samples&speed=1.2',
    size: WIDE,
    steps: async (p) => {
      await p.tick('^Upload (kick|snare|hat)');
      await p.click(/^Upload/);
      await p.wait(3000);
    },
  },
  {
    // a resumed upload: two of the three files are on the device already, the third is sent
    name: 'samples-upload-skip',
    q: 'mode=samples&speed=12&skip=kick_punchy,snare_tight_01',
    size: WIDE,
    steps: async (p) => {
      await p.tick('^Upload (kick_punchy|snare_tight_01|hat_closed_a)\\.wav');
      await p.click(/^Upload/);
      await p.wait(4000);
    },
  },
  {
    name: 'samples-upload-failed',
    q: 'mode=samples&speed=12&putfail=1',
    size: WIDE,
    steps: async (p) => {
      for (const n of ['kick', 'snare', 'hat']) await p.tick(`^Upload ${n}`);
      await p.click(/^Upload/);
      await p.wait(4000);
    },
  },
  { name: 'samples-narrow', q: 'mode=samples', size: MIN },
  {
    name: 'samples-narrow-uploading',
    q: 'mode=samples&speed=1.2',
    size: MIN,
    steps: async (p) => {
      await p.tick('^Upload (kick|snare|hat)');
      await p.click(/^Upload/);
      await p.wait(3000);
    },
  },
  { name: 'samples-no-folder', q: 'mode=samples&folder=none', size: WIDE },

  // ---- the explorer: selection, navigation, upload of folders, drag and drop (Samples mode) ----
  {
    name: 'explorer-folders-selectable',
    q: 'mode=samples',
    size: WIDE,
    steps: async (p) => {
      await p.clickRow('loops', { pane: 0 });
      p.say(`click on a folder selects it: ${JSON.stringify(await p.selected(0))} (expect ["loops/"])`);
    },
  },
  {
    name: 'explorer-select-all-folders',
    q: 'mode=samples',
    size: WIDE,
    steps: async (p) => {
      await p.eval('document.querySelector(\'input[aria-label="Select all to upload"]\').click()');
      await p.wait(200);
      const n = (await p.selected(0)).length;
      const folders = (await p.selected(0)).filter((x) => x.endsWith('/')).length;
      p.say(`header box: ${n} rows selected, ${folders} of them folders (expect all rows, 6 folders)`);
    },
  },
  {
    name: 'explorer-shift-range',
    q: 'mode=samples',
    size: WIDE,
    steps: async (p) => {
      await p.clickRow('hat_closed_a.wav');
      await p.clickRow('tom_low.wav', { mods: 8 });
      p.say(`shift-click range: ${JSON.stringify(await p.selected(0))}`);
    },
  },
  {
    name: 'explorer-cmd-toggle',
    q: 'mode=samples',
    size: WIDE,
    steps: async (p) => {
      await p.clickRow('Drums');
      await p.clickRow('kick_punchy.wav', { mods: 4 });
      await p.clickRow('snare_rimshot.wav', { mods: 4 });
      await p.clickRow('kick_punchy.wav', { mods: 4 });
      p.say(`cmd-click toggles (kick_punchy added then removed): ${JSON.stringify(await p.selected(0))}`);
    },
  },
  {
    name: 'explorer-marquee',
    q: 'mode=samples',
    size: WIDE,
    steps: async (p) => {
      await p.clickRow('Drums', { pane: 0, clicks: 2 });
      await p.wait(500);
      const [x1, y1] = await p.blank(0, 0.7, 10);
      const [, y2] = await p.rowAt('808_kick_b.wav', 0);
      await p.dragPath(x1, y1, x1 - 260, y2 - 4, { release: false });
      p.say(
        `marquee held: ${JSON.stringify(await p.selected(0))} (expect 808_kick_b, 808_snare, 808_hat... the rows it touches)`,
      );
    },
  },
  {
    name: 'explorer-marquee-additive-device',
    q: 'mode=samples',
    size: WIDE,
    steps: async (p) => {
      await p.clickRow('init_tone', { pane: 1 });
      const [x1, y1] = await p.blank(1, 0.7, 10);
      const [, y2] = await p.rowAt('Loops', 1);
      await p.dragPath(x1, y1, x1 - 200, y2 + 2, { mods: 4 });
      p.say(
        `device pane, cmd + marquee keeps the first selection: ${JSON.stringify(await p.selected(1))} (expect Loops/ and init_tone)`,
      );
    },
  },
  {
    name: 'explorer-marquee-autoscroll',
    q: 'mode=samples',
    size: [1440, 520],
    steps: async (p) => {
      // press in the blank space below the long list, drag to the top edge and hold there: the pane scrolls up and the selection grows
      await p.eval("document.querySelectorAll('section.col')[0].scrollTop = 99999");
      await p.wait(300);
      const [x1, y1] = await p.blank(0, 0.7, 10);
      const rect = await p.eval(
        '(() => { const r = document.querySelectorAll("section.col")[0].getBoundingClientRect(); return [r.top, r.bottom]; })()',
      );
      await p.mouse('mouseMoved', x1, y1);
      await p.mouse('mousePressed', x1, y1);
      await p.mouse('mouseMoved', x1 - 120, y1 - 100);
      await p.mouse('mouseMoved', x1 - 120, rect[0] + 70);
      await p.wait(1200);
      const top = await p.eval('document.querySelectorAll("section.col")[0].scrollTop');
      p.say(
        `held near the top edge for 1.2 s: pane scrollTop ${top} (started at the bottom), ${(await p.selected(0)).length} rows selected`,
      );
    },
  },
  {
    name: 'explorer-keys',
    q: 'mode=samples',
    size: WIDE,
    steps: async (p) => {
      await p.clickRow('kick_punchy.wav');
      await p.key('a', { mods: 4, code: 'KeyA', vk: 65 });
      const all = (await p.selected(0)).length;
      await p.key('Escape', { code: 'Escape', vk: 27 });
      const none = (await p.selected(0)).length;
      await p.key('a', { mods: 2, code: 'KeyA', vk: 65 });
      const ctrl = (await p.selected(0)).length;
      p.say(`Cmd+A: ${all} rows; Esc: ${none}; Ctrl+A: ${ctrl} (expect all, 0, all)`);
      await p.eval("document.querySelectorAll('section.col')[1].focus()");
      await p.key('a', { mods: 4, code: 'KeyA', vk: 65 });
      p.say(
        `Cmd+A in the device pane selects there: ${(await p.selected(1)).length} rows; the computer pane keeps ${(await p.selected(0)).length}`,
      );
    },
  },
  {
    name: 'explorer-navigate',
    q: 'mode=samples',
    size: WIDE,
    steps: async (p) => {
      const crumb = () =>
        p.eval(
          "[...document.querySelectorAll('nav.crumbs')[0].querySelectorAll('button')].map((b) => b.textContent.trim()).join(' / ')",
        );
      await p.clickRow('loops', { clicks: 2 });
      await p.wait(400);
      await p.clickRow('breaks', { clicks: 2 });
      await p.wait(400);
      p.say(`double-click twice: ${await crumb()} (expect Samples / loops / breaks)`);
      await p.key('ArrowUp', { mods: 4, code: 'ArrowUp', vk: 38 });
      await p.wait(400);
      p.say(`Cmd+Up: ${await crumb()} (expect Samples / loops)`);
      await p.clickRow('breaks');
      await p.key('Enter', { code: 'Enter', vk: 13 });
      await p.wait(400);
      p.say(`select a folder + Enter: ${await crumb()} (expect Samples / loops / breaks)`);
      await p.key('Backspace', { code: 'Backspace', vk: 8 });
      await p.wait(400);
      p.say(`Backspace: ${await crumb()}`);
      await p.click(/^↑ Up$/);
      await p.wait(400);
      p.say(`Up row: ${await crumb()} (expect Samples)`);
      await p.clickRow('loops', { clicks: 2 });
      await p.wait(400);
      await p.click('Samples');
      await p.wait(400);
      p.say(`breadcrumb: ${await crumb()}`);
      await p.clickRow('loops', { clicks: 2 });
      await p.wait(400);
      await p.clickRow('breaks', { clicks: 2 });
      await p.wait(500);
    },
  },
  {
    name: 'explorer-navigate-device',
    q: 'mode=samples',
    size: WIDE,
    steps: async (p) => {
      await p.clickRow('Drums', { pane: 1, clicks: 2 });
      await p.wait(500);
      const here = await p.eval(
        "[...document.querySelectorAll('nav.crumbs')[1].querySelectorAll('button')].map((b) => b.textContent.trim()).join(' / ')",
      );
      p.say(`device pane after a double-click on Drums: ${here} (an Up row is shown)`);
    },
  },
  {
    name: 'explorer-upload-folder-running',
    q: 'mode=samples&speed=1.5',
    size: WIDE,
    steps: async (p) => {
      await p.clickRow('loops', { pane: 0 });
      await p.click(/^Upload/);
      await p.wait(4500);
    },
  },
  {
    name: 'explorer-upload-folder-done',
    q: 'mode=samples&speed=12',
    size: WIDE,
    steps: async (p) => {
      await p.clickRow('loops', { pane: 0 });
      await p.click(/^Upload/);
      await p.wait(9000);
      await p.clickRow('loops', { pane: 1, clicks: 2 });
      await p.wait(600);
      await p.clickRow('breaks', { pane: 1, clicks: 2 });
      await p.wait(600);
      const list = await p.eval(
        `[...document.querySelectorAll('section.col')[1].querySelectorAll('li .nm')].map((n) => n.textContent.trim()).join(', ')`,
      );
      p.say(`device folder loops/breaks after the upload: ${list}`);
    },
  },
  {
    name: 'explorer-upload-multi-folders',
    q: 'mode=samples&speed=20',
    size: WIDE,
    steps: async (p) => {
      // two folders and a file together (Presets holds an empty folder: it is made too)
      await p.clickRow('MIDI');
      await p.clickRow('Presets', { mods: 4 });
      await p.clickRow('notes.txt', { mods: 4 });
      await p.click(/^Upload/);
      await p.wait(5000);
      const names = await p.eval(
        `[...document.querySelectorAll('section.col')[1].querySelectorAll('li .nm')].map((n) => n.textContent.trim()).join(', ')`,
      );
      p.say(`device top level after: ${names}`);
      await p.clickRow('Presets', { pane: 1, clicks: 2 });
      await p.wait(600);
      const sub = await p.eval(
        `[...document.querySelectorAll('section.col')[1].querySelectorAll('li .nm')].map((n) => n.textContent.trim()).join(', ')`,
      );
      p.say(`device Presets/: ${sub} (expect empty/, kit_a.json, kit_b.json)`);
    },
  },
  {
    name: 'explorer-no-folders-backend',
    q: 'mode=samples&mkdir=0',
    size: WIDE,
    steps: async (p) => {
      await p.clickRow('loops');
      await p.click(/^Upload/);
      await p.wait(800);
    },
  },
  {
    name: 'explorer-drag-over-pane',
    q: 'mode=samples',
    size: WIDE,
    steps: async (p) => {
      await p.clickRow('hat_closed_a.wav');
      await p.clickRow('hat_open_wide.wav', { mods: 8 });
      p.say(
        `drag types: ${await p.dnd('hat_closed_a.wav', 'pane', { drop: false })} (rows of the selection travel along)`,
      );
    },
  },
  {
    name: 'explorer-drag-over-folder',
    q: 'mode=samples',
    size: WIDE,
    steps: async (p) => {
      await p.clickRow('Drums', { pane: 0 });
      await p.clickRow('Vocals', { pane: 0, mods: 4 });
      await p.dnd('Drums', 'Loops', { drop: false });
    },
  },
  {
    name: 'explorer-drop-on-folder',
    q: 'mode=samples&speed=6',
    size: WIDE,
    steps: async (p) => {
      await p.clickRow('hat_closed_a.wav');
      await p.clickRow('hat_open_wide.wav', { mods: 8 });
      await p.dnd('hat_closed_a.wav', 'Loops', { drop: true });
      await p.wait(700);
      await p.wait(3000);
      await p.clickRow('Loops', { pane: 1, clicks: 2 });
      await p.wait(600);
      const list = await p.eval(
        `[...document.querySelectorAll('section.col')[1].querySelectorAll('li .nm')].map((n) => n.textContent.trim()).join(', ')`,
      );
      p.say(`device Loops/ after the drop on it: ${list}`);
    },
  },
  {
    name: 'explorer-drop-folder-on-pane',
    q: 'mode=samples&speed=20',
    size: WIDE,
    steps: async (p) => {
      await p.clickRow('Drums');
      p.say(`drag types: ${await p.dnd('Drums', 'pane', { drop: true })}`);
      await p.wait(3500);
      p.say(`error line: ${JSON.stringify(await p.eval("document.querySelector('.err')?.textContent ?? ''"))}`);
    },
  },
  {
    name: 'explorer-drop-finder-files',
    q: 'mode=samples&speed=20',
    size: WIDE,
    steps: async (p) => {
      await p.dnd(null, 'pane', { drop: false, files: ['from_finder_a.wav', 'from_finder_b.wav'] });
      await p.wait(300);
      await p.eval(
        "document.querySelectorAll('section.col')[1].dispatchEvent(new Event('dragleave', { bubbles: true }))",
      );
      await p.dnd(null, 'pane', { drop: true, files: ['from_finder_a.wav', 'from_finder_b.wav'] });
      await p.wait(3500);
    },
  },
  {
    name: 'explorer-no-text-selection',
    q: 'mode=samples',
    size: WIDE,
    steps: async (p) => {
      // a drag over the names must not select text, and no row text is selectable
      const [x1, y1] = await p.rowAt('kick_punchy.wav', 0, 0.1);
      const [x2, y2] = await p.rowAt('tom_low.wav', 0, 0.6);
      await p.dragPath(x1, y1, x2, y2, { release: true });
      const sel = await p.eval('getSelection().toString()');
      const us = await p.eval("getComputedStyle(document.querySelector('.rows li .nm')).userSelect");
      p.say(
        `selected text after dragging over names: ${JSON.stringify(sel)} (expect ""), user-select of a name: ${us} (expect none)`,
      );
    },
  },
  {
    name: 'explorer-modules',
    q: 'mode=modules',
    size: WIDE,
    steps: async (p) => {
      await p.clickRow('Sine');
      await p.clickRow('FM 4-op', { mods: 8 });
      await p.clickRow('Tuner', { mods: 4 });
      p.say(`modules, shift then cmd: ${JSON.stringify(await p.selected(0))}`);
      await p.clickRow('Sine', { pane: 1 });
      await p.clickRow('Pluck', { pane: 1, mods: 4 });
      p.say(`device pane: ${JSON.stringify(await p.selected(1))}`);
    },
  },
  {
    name: 'explorer-narrow',
    q: 'mode=samples',
    size: MIN,
    steps: async (p) => {
      await p.clickRow('Drums');
      await p.clickRow('Vocals', { mods: 4 });
      await p.clickRow('kick_punchy.wav', { mods: 4 });
    },
  },
  {
    name: 'noreply',
    q: 'mode=modules&dev=noreply',
    size: WIDE,
    steps: async (p) => {
      await p.wait(2600);
    },
  },
  {
    name: 'noreply-samples',
    q: 'mode=samples&dev=noreply',
    size: WIDE,
    steps: async (p) => {
      await p.wait(2600);
    },
  },
  {
    name: 'upload-not-waiting',
    q: 'mode=samples&speed=0.3',
    size: WIDE,
    steps: async (p) => {
      await p.tick('^Upload (perc_shaker|pad_warm)');
      await p.click(/^Upload/);
      await p.wait(4500);
    },
  },
  {
    name: 'samples-scrolled-narrow',
    q: 'mode=samples',
    size: MIN,
    steps: async (p) => {
      // the left pane scrolls alone: the right pane and the divider stay, the header sticks to the top of its pane
      await p.eval("document.querySelector('.split > section').scrollTop = 300");
      await p.wait(300);
      const tops = await p.eval('[...document.querySelectorAll(".split > section")].map((e) => e.scrollTop)');
      const handle = await p.eval(
        '(() => { const h = document.querySelector(".split-handle").getBoundingClientRect(); const s = document.querySelector(".split").getBoundingClientRect(); return [h.top - s.top, s.bottom - h.bottom]; })()',
      );
      p.say(
        `pane scrollTop ${JSON.stringify(tops)}; divider insets from the split box top/bottom ${JSON.stringify(handle)}`,
      );
    },
  },
  {
    name: 'auto-refresh',
    q: 'mode=samples&speed=0.3',
    size: WIDE,
    steps: async (p) => {
      const counts = () => p.eval('JSON.stringify(window.__sim.counts)');
      const has = (name) => p.eval(`document.body.innerText.includes(${JSON.stringify(name)})`);
      await p.wait(1500);
      const c0 = JSON.parse(await counts());
      await p.wait(7000);
      const c1 = JSON.parse(await counts());
      p.say(
        `idle 7 s: local polls +${c1.localPoll - c0.localPoll} (expect 2), device reads +${c1.deviceRead - c0.deviceRead} (expect 0)`,
      );
      await p.eval("window.__sim.addLocal('new_file_xyz.wav')");
      await p.wait(4500);
      p.say(`file added on disk: shown after the poll = ${await has('new_file_xyz.wav')} (expect true)`);
      const c2 = JSON.parse(await counts());
      await p.eval("window.dispatchEvent(new Event('focus'))");
      await p.wait(1500);
      const c3 = JSON.parse(await counts());
      p.say(
        `focus: device reads +${c3.deviceRead - c2.deviceRead} (expect 1), local reads +${c3.localList - c2.localList} (expect 1)`,
      );
      await p.eval("window.dispatchEvent(new Event('focus'))");
      await p.eval("document.dispatchEvent(new Event('visibilitychange'))");
      await p.wait(1200);
      const c4 = JSON.parse(await counts());
      p.say(
        `focus + visibilitychange together: device reads +${c4.deviceRead - c3.deviceRead} (expect 1 or 0: one read answers both)`,
      );
      // a job: nothing reads while it runs, and the file that appears meanwhile shows after the job
      await p.tick('^Upload (perc_shaker|pad_warm)');
      await p.click(/^Upload/);
      await p.wait(800);
      const c5 = JSON.parse(await counts());
      await p.eval("window.__sim.addLocal('during_upload.wav')");
      await p.eval("window.dispatchEvent(new Event('focus'))");
      await p.wait(6000);
      const c6 = JSON.parse(await counts());
      p.say(
        `during the upload (6 s): device reads +${c6.deviceRead - c5.deviceRead}, local polls +${c6.localPoll - c5.localPoll}, local reads +${c6.localList - c5.localList} (expect 0, 0, 0); new file shown yet = ${await has('during_upload.wav')} (expect false)`,
      );
    },
  },
  {
    name: 'samples-done',
    q: 'mode=samples&speed=8',
    size: WIDE,
    steps: async (p) => {
      await p.tick('^Upload (kick_punchy|hat_closed_a)');
      await p.click(/^Upload/);
      await p.wait(4000);
    },
  },
  {
    name: 'delete-confirm',
    q: 'mode=modules',
    size: WIDE,
    steps: async (p) => {
      await p.tick('^Delete (Pluck|Chorus)$');
      await p.click(/^Delete/);
      await p.wait(300);
    },
  },
  { name: 'slow-not-waiting', q: 'mode=modules&dev=slow', size: WIDE, steps: async (p) => p.wait(0) },
  { name: 'empty', q: 'mode=modules&dev=empty', size: WIDE },
  { name: 'empty-samples', q: 'mode=samples&dev=empty', size: WIDE },
  { name: 'error', q: 'mode=modules&dev=error', size: WIDE },
  // ---- the Remote page against the Emulator page's faceplate: the same test pattern on the 128 x 64 screen ----
  { name: 'remote-wide', q: 'page=remote', size: WIDE, steps: async (p) => p.wait(800) },
  { name: 'remote-narrow', q: 'page=remote', size: MIN, steps: async (p) => p.wait(800) },
  // ---- the Card panel of the Emulator page: the folder of the loaded device (preview/card-mock.ts) ----
  { name: 'card-dn2', q: 'page=emulator&card=dn2', size: WIDE, steps: async (p) => p.wait(700) },
  { name: 'card-dt2', q: 'page=emulator&card=dt2', size: WIDE, steps: async (p) => p.wait(700) },
  {
    // the Digitone II takes the chosen folder at once: no reload notice
    name: 'card-dn2-chosen',
    q: 'page=emulator&card=dn2',
    size: WIDE,
    steps: async (p) => {
      await p.click('Choose folder…');
      await p.wait(700);
    },
  },
  {
    // the Digitakt II builds its card from the folder: the notice and "Reload now"
    name: 'card-dt2-reload-notice',
    q: 'page=emulator&card=dt2',
    size: WIDE,
    steps: async (p) => {
      await p.click('Choose folder…');
      await p.wait(700);
    },
  },
  {
    name: 'card-dt2-reloaded',
    q: 'page=emulator&card=dt2',
    size: WIDE,
    steps: async (p) => {
      await p.click('Choose folder…');
      await p.wait(500);
      await p.click('Reload now');
      await p.wait(700);
    },
  },
  { name: 'card-dt2-saved-choice', q: 'page=emulator&card=dt2&chosen=1', size: WIDE, steps: async (p) => p.wait(700) },
  {
    name: 'card-dt2-missing-choice',
    q: 'page=emulator&card=dt2&chosen=1&missing=1',
    size: WIDE,
    steps: async (p) => p.wait(700),
  },
  {
    name: 'card-dt2-use-default',
    q: 'page=emulator&card=dt2&chosen=1',
    size: WIDE,
    steps: async (p) => {
      await p.click('Use default');
      await p.wait(700);
    },
  },
  {
    name: 'card-dt2-reload-notice-narrow',
    q: 'page=emulator&card=dt2',
    size: MIN,
    steps: async (p) => {
      await p.click('Choose folder…');
      await p.wait(700);
    },
  },
  // ---- the Memory panel (preview/memory-backend.mock.ts): bars per resource, segments per loaded module; mem=pluck|full|empty|dsp ----
  {
    // a Digitone II with Pluck and Remote control loaded and one module that is not in the library ("other")
    name: 'memory-dn2',
    q: 'page=emulator&card=dn2&mem=pluck',
    size: WIDE,
    steps: async (p) => {
      await p.wait(700);
      p.say(
        'segment titles: ' +
          (await p.eval(
            "JSON.stringify([...document.querySelectorAll('.memory-panel .seg:not(.free)')].slice(0, 6).map((e) => e.title))",
          )),
      );
    },
  },
  { name: 'memory-dn2-full', q: 'page=emulator&card=dn2&mem=full', size: WIDE, steps: async (p) => p.wait(700) },
  { name: 'memory-dn2-dsp-full', q: 'page=emulator&card=dn2&mem=dsp', size: WIDE, steps: async (p) => p.wait(700) },
  // the Digitakt II has no page rows: no Pages bar
  { name: 'memory-dt2', q: 'page=emulator&card=dt2&mem=pluck', size: WIDE, steps: async (p) => p.wait(700) },
  { name: 'memory-empty', q: 'page=emulator&card=dn2&mem=empty', size: WIDE, steps: async (p) => p.wait(700) },
  { name: 'memory-narrow', q: 'page=emulator&card=dn2&mem=pluck', size: MIN, steps: async (p) => p.wait(700) }, // closed by default at this width
  {
    name: 'memory-narrow-open',
    q: 'page=emulator&card=dn2&mem=pluck',
    size: MIN,
    steps: async (p) => {
      await p.wait(400);
      await openMemory(p);
      await p.wait(400);
    },
  },
  {
    name: 'memory-remote',
    q: 'page=remote&card=dn2&mem=full',
    size: WIDE,
    steps: async (p) => {
      await p.wait(500);
      await openMemory(p);
      await p.wait(500);
    },
  },
  {
    name: 'memory-remote-narrow',
    q: 'page=remote&card=dn2&mem=full',
    size: MIN,
    steps: async (p) => {
      await p.wait(500);
      await openMemory(p);
      await p.wait(500);
    },
  },
  {
    name: 'memory-collapsed',
    q: 'page=emulator&card=dn2&mem=pluck',
    size: WIDE,
    steps: async (p) => {
      await p.wait(500);
      await openMemory(p);
      await p.eval("document.querySelector('.memory-panel .card-toggle').click()");
      await p.wait(300);
    },
  },
  // ---- the right-hand column of the Emulator page: the Card panel on top, the Memory panel under it, each collapsible ----
  ...['dn2', 'dt2'].map((dev) => ({
    name: `side-${dev}`,
    q: `page=emulator&card=${dev}&mem=pluck`,
    size: WIDE,
    steps: async (p) => {
      await p.wait(400);
      await setPanel(p, 'card', 'open');
      await setPanel(p, 'memory', 'open');
      await p.wait(500);
    },
  })),
  {
    name: 'side-card-closed',
    q: 'page=emulator&card=dn2&mem=pluck',
    size: WIDE,
    steps: async (p) => {
      await p.wait(400);
      await setPanel(p, 'memory', 'open');
      await setPanel(p, 'card', 'closed');
      await p.wait(400);
    },
  },
  {
    name: 'side-memory-closed',
    q: 'page=emulator&card=dn2&mem=pluck',
    size: WIDE,
    steps: async (p) => {
      await p.wait(400);
      await setPanel(p, 'card', 'open');
      await setPanel(p, 'memory', 'closed');
      await p.wait(400);
    },
  },
  {
    // nothing in the column is open: a rail, and the faceplate has the width
    name: 'side-rail',
    q: 'page=emulator&card=dn2&mem=pluck',
    size: WIDE,
    steps: async (p) => {
      await p.wait(400);
      await setPanel(p, 'card', 'closed');
      await setPanel(p, 'memory', 'closed');
      await p.wait(400);
    },
  },
  {
    name: 'side-narrow',
    q: 'page=emulator&card=dn2&mem=pluck',
    size: MIN,
    steps: async (p) => {
      await p.wait(400);
      await setPanel(p, 'card', 'open');
      await setPanel(p, 'memory', 'open');
      await p.wait(400);
    },
  },
  // ---- the Log drawer over fake runtime logs (apps/desktop/ui/preview/runtime-log-mock.ts); logspeed makes the script run faster than real time ----
  ...[
    ['log-both-wide', 'page=log&log=both&logspeed=3&logh=640', WIDE, 5500],
    ['log-both-narrow', 'page=log&log=both&logspeed=3&logh=640', MIN, 5500],
    ['log-empty-wide', 'page=log&log=empty', WIDE, 1500],
    ['log-empty-narrow', 'page=log&log=empty', MIN, 1500],
    ['log-frozen', 'page=log&log=frozen', WIDE, 9500],
  ].map(([name, q, size, ms]) => ({ name, q, size, steps: async (p) => p.wait(ms) })),
  ...[
    ['log-fault-wide', WIDE],
    ['log-fault-narrow', MIN],
  ].map(([name, size]) => ({
    name,
    q: 'page=log&log=fault&logh=700',
    size,
    steps: async (p) => {
      await p.wait(2500);
      await p.eval(
        "(() => { const li = [...document.querySelectorAll('.log-list li')].find((l) => l.querySelector('.text')?.textContent.startsWith('fault:')); li?.querySelector('.more')?.click(); li?.scrollIntoView({ block: 'center' }); })()",
      );
      await p.wait(300);
    },
  })),
  {
    // the menu of a <select> is drawn by the system and is not in a screenshot: the same options as an open list
    name: 'log-menu',
    q: 'page=log&log=both&logspeed=3&logh=640',
    size: WIDE,
    steps: async (p) => {
      await p.wait(5500);
      await p.eval(
        "(() => { const s = document.querySelector('.log-drawer select'); s.size = 22; s.style.position = 'absolute'; s.style.zIndex = '9'; s.style.height = 'auto'; s.style.right = '40px'; s.style.top = '40px'; })()",
      );
      await p.wait(300);
    },
  },
  {
    name: 'log-menu-empty',
    q: 'page=log&log=empty',
    size: WIDE,
    steps: async (p) => {
      await p.wait(1200);
      await p.eval(
        "(() => { const s = document.querySelector('.log-drawer select'); s.size = 16; s.style.position = 'absolute'; s.style.zIndex = '9'; s.style.height = 'auto'; s.style.right = '40px'; s.style.top = '40px'; })()",
      );
      await p.wait(300);
    },
  },
  {
    // over a running emulator: `--url http://127.0.0.1:5298/live.html` (live.vite.config.ts, tools.devtools.emu_live); the drawer follows its real runtime log
    name: 'log-live',
    q: 'page=emulator&log=1',
    size: WIDE,
    steps: async (p) => {
      await p.wait(6000);
      p.say('rows: ' + (await p.eval("document.querySelectorAll('.log-list li').length")));
    },
  },
  {
    name: 'log-save',
    q: 'page=log&log=both&logspeed=3&logh=640',
    size: WIDE,
    steps: async (p) => {
      await p.wait(5500);
      await p.click('Save');
      await p.wait(300);
      p.say(
        'saved: ' + (await p.eval('JSON.stringify(window.__saved?.map((s) => [s.name, s.text.split("\\n").length]))')),
      );
      p.say('first lines: ' + (await p.eval('(window.__saved?.[0]?.text ?? "").split("\\n").slice(0, 4).join(" | ")')));
    },
  },
  // ---- the Level menu and the debug module indicator of the Log drawer (workbench-device-log.md "Log levels") ----
  ...[
    // [name, query, size, Source value (empty: leave All), steps after choosing it]
    [
      'log-level-menu',
      'page=log&log=both&logspeed=3&logh=640',
      WIDE,
      'device:all',
      async (p) => {
        await p.click(/^Level/);
        await p.wait(400);
      },
    ],
    [
      'log-level-menu-changed',
      'page=log&log=both&logspeed=3&logh=640',
      WIDE,
      'device:all',
      async (p) => {
        await p.click(/^Level/);
        await p.wait(400);
        // the module row of freqshift: error; the default row: trace
        await p.eval(
          "(() => { const rows = [...document.querySelectorAll('.level-row')]; const pick = (row, name) => [...row.querySelectorAll('button')].find((b) => b.textContent === name)?.click(); pick(rows[0], 'trace'); pick(rows[2], 'error'); })()",
        );
        await p.wait(500);
      },
    ],
    [
      'log-level-menu-emulator',
      'page=log&log=both&logspeed=3&logh=640',
      MIN,
      'emulator:all',
      async (p) => {
        await p.click(/^Level/);
        await p.wait(400);
      },
    ],
    // the debug module is loaded on the device, and on the emulator in the other scene it is not
    ['log-debug-module-loaded', 'page=log&log=both&logspeed=3&logh=640', WIDE, 'device:all', async () => {}],
    ['log-debug-module-not-loaded', 'page=log&log=nodebug&logh=640', WIDE, 'device:all', async () => {}],
    ['log-debug-module-not-loaded-narrow', 'page=log&log=nodebug&logh=640', MIN, 'device:all', async () => {}],
    // Source is All and both origins are followed: the menu does not know which one is meant
    ['log-debug-disabled-all', 'page=log&log=both&logspeed=3&logh=640', WIDE, '', async () => {}],
  ].map(([name, q, size, source, after]) => ({
    name,
    q,
    size,
    steps: async (p) => {
      await p.wait(5500);
      if (source)
        await p.eval(
          `(() => { const s = document.querySelector('.log-drawer select'); s.value = ${JSON.stringify(source)}; s.dispatchEvent(new Event('change', { bubbles: true })); })()`,
        );
      await p.wait(1500);
      await after(p);
      p.say(
        'header: ' +
          (await p.eval(
            "[...document.querySelectorAll('.log-drawer header .tb')].map((b) => b.textContent.trim() + (b.disabled ? ' [off]' : '')).join(' | ')",
          )),
      );
    },
  })),
  // the Default modules list next to the emulator's firmware file (the toolbar of Console.tsx, replicated in the preview)
  {
    name: 'emulator-defaults-open',
    q: 'page=emulator',
    size: WIDE,
    steps: async (p) => {
      await p.wait(600);
      await p.click(/^Default modules/);
      await p.wait(300);
    },
  },
  // a firmware is starting, and a load that failed: no old device badge, no faceplate, the state in the status bar
  { name: 'emulator-booting', q: 'page=emulator&boot=loading', size: WIDE },
  { name: 'emulator-boot-failed', q: 'page=emulator&boot=failed', size: WIDE },
  { name: 'emulator-faceplate-wide', q: 'page=emulator', size: WIDE },
  { name: 'emulator-faceplate-narrow', q: 'page=emulator', size: MIN },
  // README shots: the site's modules and its device screens, no preview banner
  {
    name: 'readme-emulator',
    q: 'page=emulator&card=dn2&frame=device-modules&banner=0',
    size: WIDE,
    steps: async (p) => p.wait(700),
  },
  {
    name: 'readme-transfer',
    q: 'mode=modules&lib=site&banner=0',
    size: WIDE,
    steps: async (p) => {
      await p.wait(500);
      // tick the first two library rows that can be installed
      await p.eval(
        "(() => { [...document.querySelectorAll('input[type=checkbox]')].filter((x) => /^Install /.test(x.getAttribute('aria-label') ?? '') && !x.checked && !x.disabled).slice(0, 2).forEach((x) => x.click()); })()",
      );
    },
  },
  {
    name: 'readme-remote',
    q: 'page=remote&frame=device-digikit&banner=0',
    size: WIDE,
    steps: async (p) => p.wait(800),
  },
];

if (args.includes('--list')) {
  console.log(SCENES.map((s) => s.name).join('\n'));
  process.exit(0);
}

function pageApi(cdp) {
  const evaluate = (js) => cdpEvaluate(cdp, js);
  const api = {
    eval: evaluate,
    say: (m) => console.log(`  ${m}`),
    wait: sleep,
    async click(text) {
      const src = text instanceof RegExp ? text.source : `^${text.replace(/[.*+?^${}()|[\]\\]/g, '\\$&')}$`;
      // a button is disabled while the page reads (a window focus starts a read): try for up to 3 s
      for (let i = 0; i < 30; i++) {
        const ok = await evaluate(
          `(() => { const re = new RegExp(${JSON.stringify(src)}); const b = [...document.querySelectorAll('button')].find((x) => re.test(x.textContent.trim()) && !x.disabled); if (b) b.click(); return !!b; })()`,
        );
        if (ok) return;
        await sleep(100);
      }
      throw new Error(`no enabled button ${src}`);
    },
    async tick(labelSrc) {
      const n = await evaluate(
        `(() => { const re = new RegExp(${JSON.stringify(labelSrc)}); const xs = [...document.querySelectorAll('input[type=checkbox]')].filter((x) => re.test(x.getAttribute('aria-label') ?? '') && !x.checked && !x.disabled); xs.forEach((x) => x.click()); return xs.length; })()`,
      );
      if (!n) throw new Error(`no checkbox ${labelSrc}`);
    },
    /** The centre of the row whose name is `name` ("loops/" for a folder) in pane 0 (computer) or 1 (device): [x, y] in the viewport. */
    async rowAt(name, pane = 0, fx = 0.4) {
      const box = await evaluate(
        `(() => { const sec = document.querySelectorAll('section.col')[${pane}]; const li = [...sec.querySelectorAll('li')].find((l) => { const t = l.querySelector('.nm')?.textContent.trim(); return t === ${JSON.stringify(name)} || t === ${JSON.stringify(name + '/')}; }); if (!li) return null; li.scrollIntoView({ block: 'nearest' }); const r = li.getBoundingClientRect(); return [r.x + r.width * ${fx}, r.y + r.height / 2]; })()`,
      );
      if (!box) throw new Error(`no row ${name} in pane ${pane}`);
      return box;
    },
    /** A point in the empty space below the list of pane 0 or 1 (dy pixels above the bottom of the list area). */
    async blank(pane = 0, fx = 0.5, dy = 14) {
      const box = await evaluate(
        `(() => { const sec = document.querySelectorAll('section.col')[${pane}]; const r = sec.querySelector('.listarea').getBoundingClientRect(); const s = sec.getBoundingClientRect(); return [r.x + r.width * ${fx}, Math.min(r.bottom, s.bottom) - ${dy}]; })()`,
      );
      return box;
    },
    async mouse(type, x, y, { mods = 0, clicks = 1, button = 'left' } = {}) {
      await cdp.send('Input.dispatchMouseEvent', {
        type,
        x,
        y,
        button: type === 'mouseMoved' ? 'none' : button,
        buttons: type === 'mouseReleased' ? 0 : 1,
        clickCount: type === 'mouseMoved' ? 0 : clicks,
        modifiers: mods,
      });
    },
    /** A click on a row by name: mods are the CDP bits (alt 1, ctrl 2, meta 4, shift 8); clicks 2 is a double click. */
    async clickRow(name, { pane = 0, mods = 0, clicks = 1 } = {}) {
      const [x, y] = await api.rowAt(name, pane);
      for (let n = 1; n <= clicks; n++) {
        await api.mouse('mouseMoved', x, y);
        await api.mouse('mousePressed', x, y, { mods, clicks: n });
        await api.mouse('mouseReleased', x, y, { mods, clicks: n });
      }
      await sleep(250);
    },
    /** Press and drag from (x1, y1) to (x2, y2) in steps; `release` false leaves the button down (for a picture of the marquee). */
    async dragPath(x1, y1, x2, y2, { mods = 0, release = true, steps = 12 } = {}) {
      await api.mouse('mouseMoved', x1, y1);
      await api.mouse('mousePressed', x1, y1, { mods });
      for (let i = 1; i <= steps; i++)
        await api.mouse('mouseMoved', x1 + ((x2 - x1) * i) / steps, y1 + ((y2 - y1) * i) / steps, { mods });
      if (release) await api.mouse('mouseReleased', x2, y2, { mods });
      await sleep(250);
    },
    async key(key, { mods = 0, code = '', vk = 0 } = {}) {
      const base = { key, code, windowsVirtualKeyCode: vk, nativeVirtualKeyCode: vk, modifiers: mods };
      await cdp.send('Input.dispatchKeyEvent', { type: 'rawKeyDown', ...base });
      await cdp.send('Input.dispatchKeyEvent', { type: 'keyUp', ...base });
      await sleep(250);
    },
    /** How many rows are selected (class sel) in pane 0 or 1, and their names. */
    async selected(pane = 0) {
      return evaluate(
        `(() => { const sec = document.querySelectorAll('section.col')[${pane}]; return [...sec.querySelectorAll('li.sel .nm')].map((n) => n.textContent.trim()); })()`,
      );
    },
    /** A drag of rows from pane 0 as the browser sends it, with synthetic events: start on the row `from`, then over `target` (a row name in
     * pane 1, or "pane" for the empty space of pane 1); `drop` false stops over it (for a picture), true drops. */
    async dnd(from, target, { drop = true, files = null } = {}) {
      return evaluate(`(async () => {
        const secs = document.querySelectorAll('section.col');
        const rowOf = (sec, name) => [...sec.querySelectorAll('li')].find((l) => { const t = l.querySelector('.nm')?.textContent.trim(); return t === name || t === name + '/'; });
        const dt = new DataTransfer();
        const ev = (type, el, x, y) => el.dispatchEvent(new DragEvent(type, { dataTransfer: dt, bubbles: true, cancelable: true, clientX: x, clientY: y, relatedTarget: null }));
        if (${JSON.stringify(files)}) for (const n of ${JSON.stringify(files)}) dt.items.add(new File([new Uint8Array(2000)], n, { type: 'audio/wav' }));
        else { const li = rowOf(secs[0], ${JSON.stringify(from)}); const r = li.getBoundingClientRect(); ev('dragstart', li, r.x + 20, r.y + 5); }
        const tgt = ${JSON.stringify(target)};
        let el, r;
        if (tgt === 'pane') { el = secs[1].querySelector('.listarea'); r = el.getBoundingClientRect(); }
        else { el = rowOf(secs[1], tgt); r = el.getBoundingClientRect(); }
        const x = r.x + 40, y = tgt === 'pane' ? r.bottom - 12 : r.y + 5;
        ev('dragenter', el, x, y); ev('dragover', el, x, y);
        await new Promise((res) => setTimeout(res, 80));
        ev('dragover', el, x, y);
        if (${drop}) ev('drop', el, x, y);
        return dt.types.join(',');
      })()`);
    },
    async hover(selector) {
      const box = await evaluate(
        `(() => { const e = document.querySelector(${JSON.stringify(selector)}); if (!e) return null; const r = e.getBoundingClientRect(); return [r.x + r.width / 2, r.y + r.height / 2]; })()`,
      );
      if (!box) throw new Error(`nothing matches ${selector}`);
      await cdp.send('Input.dispatchMouseEvent', { type: 'mouseMoved', x: box[0], y: box[1] });
    },
  };
  return api;
}

// ---- run -----------------------------------------------------------------------------------------------------------------------------
fs.mkdirSync(out, { recursive: true });
const chrome = launchChrome(port, opt('chrome'));
let failed = 0;
try {
  const chosen = names.length ? SCENES.filter((s) => names.includes(s.name)) : SCENES;
  for (const scene of chosen) {
    const tab = await connect(port);
    const { cdp } = tab;
    const p = pageApi(cdp);
    await cdp.send('Page.enable');
    await cdp.send('Emulation.setDeviceMetricsOverride', {
      width: scene.size[0],
      height: scene.size[1],
      deviceScaleFactor: scale,
      mobile: false,
    });
    await cdp.send('Page.navigate', { url: `${base}?${scene.q}` });
    await sleep(1500);
    let note = '';
    try {
      await scene.steps?.(p);
      if (!scene.steps) await sleep(800);
    } catch (e) {
      note = ` (step failed: ${e.message})`;
      failed++;
    }
    // the bar's text, when the scene shows one (a speed is only proof if it reads right)
    const stats = await p.eval("document.querySelector('.bar .stats')?.textContent ?? ''").catch(() => '');
    if (stats) note += ` [.stats: ${stats}]`;
    const shot = await cdp.send('Page.captureScreenshot', { format: 'png' });
    const file = path.join(out, `${prefix ? `${prefix}-` : ''}${scene.name}.png`);
    fs.writeFileSync(file, Buffer.from(shot.data, 'base64'));
    console.log(`${file}${note}`);
    await closeTab(port, tab);
  }
} finally {
  await chrome.stop();
}
process.exit(0);
