import { MAX_FILE_BYTES } from '@digi/dkbs';
import { DkbsPlayer } from '@digi/dkbs/player';
import { readoutText } from './controls.ts';

interface Entry {
  name: string;
  ticks: number;
  seconds: number;
  bytes: number;
  ok: boolean;
  error: string;
  v: number;
}
interface Tile {
  el: HTMLButtonElement;
  player: DkbsPlayer;
  meta: HTMLElement;
  error: HTMLElement;
  data: Uint8Array;
  v: number;
  size: number;
}

const $ = <T extends HTMLElement>(id: string) => document.getElementById(id) as T;
const grid = $('grid');
const stage = $('stage');
const slider = $<HTMLInputElement>('slider');
const play = $<HTMLButtonElement>('play');
const loop = $<HTMLInputElement>('loop');
const zoomCanvas = $<HTMLCanvasElement>('zoom');
const zoom = new DkbsPlayer(zoomCanvas);
zoom.loop = loop.checked;
const tiles = new Map<string, Tile>();
const reduced = matchMedia('(prefers-reduced-motion: reduce)');
let open: string | undefined;
let closing = 0;

const readout = () => {
  const tile = open ? tiles.get(open) : undefined;
  $('readout').textContent = readoutText(zoom, tile?.size ?? 0);
  slider.value = String(zoom.currentTick);
  play.textContent = zoom.isPlaying ? 'Pause' : 'Play';
};
zoom.onTick = readout;

function makeTile(name: string): Tile {
  const el = document.createElement('button');
  el.className = 'tile';
  el.dataset.name = name;
  const canvas = document.createElement('canvas');
  const title = document.createElement('span');
  title.className = 'name';
  title.textContent = name;
  const meta = document.createElement('span');
  meta.className = 'meta';
  const error = document.createElement('span');
  error.className = 'error';
  error.hidden = true;
  el.append(canvas, title, meta, error);
  el.onclick = () => openTile(name);
  const player = new DkbsPlayer(canvas);
  player.loop = true;
  return { el, player, meta, error, data: new Uint8Array(0), v: 0, size: 0 };
}

async function fetchBytes(name: string): Promise<Uint8Array> {
  const r = await fetch(`/example/${encodeURIComponent(name)}.dkbs`, { cache: 'no-store' });
  return new Uint8Array(await r.arrayBuffer());
}

async function apply(entries: Entry[]) {
  const names = new Set(entries.map((e) => e.name));
  for (const [name, tile] of tiles)
    if (!names.has(name)) {
      tile.player.pause();
      tile.el.remove();
      tiles.delete(name);
      if (open === name) closeTile();
    }
  for (const e of entries) {
    let tile = tiles.get(e.name);
    if (!tile) {
      tile = makeTile(e.name);
      tiles.set(e.name, tile);
    }
    const sorted = [...tiles.keys()].sort();
    grid.insertBefore(tile.el, grid.children[sorted.indexOf(e.name)] ?? null);
    tile.error.hidden = e.ok;
    tile.error.textContent = e.error;
    if (e.v > 0 && e.v !== tile.v) {
      const data = await fetchBytes(e.name);
      try {
        tile.player.load(data);
      } catch (err) {
        tile.error.hidden = false;
        tile.error.textContent = String(err);
        continue;
      }
      tile.data = data;
      tile.v = e.v;
      tile.size = e.bytes;
      tile.meta.textContent = `${e.seconds.toFixed(2)} s · ${(e.bytes / 1024).toFixed(1)} KB of ${MAX_FILE_BYTES / 1024}`;
      tile.player.play();
      if (open === e.name) startZoom(tile);
    }
  }
}

async function refresh() {
  await apply(await (await fetch('/examples', { cache: 'no-store' })).json());
}

function startZoom(tile: Tile) {
  zoom.load(tile.data);
  slider.max = String(Math.max(0, zoom.ticks - 1));
  zoom.play();
}

/** Stage box (outer size includes 8 px padding and a 1 px border) around a canvas of width `w`. */
const PAD = 18;
function setBox(left: number, top: number, canvasWidth: number) {
  stage.style.left = `${left}px`;
  stage.style.top = `${top}px`;
  stage.style.width = `${canvasWidth + PAD}px`;
}

function target(): { left: number; top: number; width: number } {
  const maxW = Math.min(innerWidth - 32 - PAD, 1024);
  const maxH = innerHeight - 32 - PAD - 120;
  const k = Math.floor(Math.min(maxW / 128, maxH / 64));
  const width = k >= 1 ? 128 * k : Math.max(64, maxW);
  return { left: 0, top: 0, width };
}

function layout(animate: boolean) {
  const t = target();
  stage.style.transition = animate && !reduced.matches ? '' : 'none';
  // the width is still animating, so compute the height: target canvas height plus the rest of the stage as it is now
  const height = t.width / 2 + stage.offsetHeight - zoomCanvas.offsetHeight;
  stage.style.width = `${t.width + PAD}px`;
  const left = (innerWidth - t.width - PAD) / 2;
  const top = Math.max(16, (innerHeight - height) / 2);
  setBox(left, top, t.width);
}

function openTile(name: string) {
  const tile = tiles.get(name);
  if (!tile || !tile.v) return;
  clearTimeout(closing);
  open = name;
  const from = tile.el.querySelector('canvas')!.getBoundingClientRect();
  document.body.classList.add('zoomed');
  stage.style.display = 'grid';
  stage.style.transition = 'none';
  stage.classList.add('moving');
  setBox(from.left - 9, from.top - 9, from.width);
  stage.getBoundingClientRect();
  startZoom(tile);
  layout(true);
  setTimeout(() => stage.classList.remove('moving'), reduced.matches ? 0 : 200);
}

function closeTile() {
  if (!open) return;
  const tile = tiles.get(open);
  open = undefined;
  zoom.pause();
  document.body.classList.remove('zoomed');
  const done = () => {
    if (!open) stage.style.display = 'none';
  };
  if (!tile || reduced.matches) return done();
  const to = tile.el.querySelector('canvas')!.getBoundingClientRect();
  stage.style.transition = '';
  stage.classList.add('moving');
  setBox(to.left - 9, to.top - 9, to.width);
  closing = window.setTimeout(done, 200);
}

play.onclick = () => (zoom.isPlaying ? zoom.pause() : zoom.play(), readout());
loop.onchange = () => {
  zoom.loop = loop.checked;
};
slider.oninput = () => zoom.seek(Number(slider.value));
$('close').onclick = closeTile;
$('backdrop').onclick = closeTile;
addEventListener('keydown', (e) => {
  if (e.key === 'Escape') closeTile();
});
addEventListener('resize', () => open && layout(false));

if (import.meta.hot) import.meta.hot.on('bootscreen:gallery', (entries: Entry[]) => void apply(entries));
void refresh();
