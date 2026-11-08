import { MAX_FILE_BYTES } from '@digi/dkbs';
import { DkbsPlayer } from '@digi/dkbs/player';
import { readoutText } from './controls.ts';

const $ = <T extends HTMLElement>(id: string) => document.getElementById(id) as T;
const player = new DkbsPlayer($<HTMLCanvasElement>('screen'));
const slider = $<HTMLInputElement>('slider');
const play = $<HTMLButtonElement>('play');
let size = 0;

const readout = () => {
  const tick = player.currentTick;
  $('readout').textContent = readoutText(player, size);
  $('fill').style.width = `${Math.min(100, (100 * size) / MAX_FILE_BYTES)}%`;
  slider.value = String(tick);
  play.textContent = player.isPlaying ? 'Pause' : 'Play';
};

player.onTick = readout;

async function reload() {
  const status = await (await fetch('/status', { cache: 'no-store' })).json();
  const banner = $('error');
  banner.style.display = status.ok ? 'none' : 'block';
  banner.textContent = status.error;
  const bytes = new Uint8Array(await (await fetch('/asset.dkbs', { cache: 'no-store' })).arrayBuffer());
  if (!bytes.length) return;
  size = bytes.length;
  try {
    player.load(bytes);
  } catch (e) {
    banner.style.display = 'block';
    banner.textContent = String(e);
    return;
  }
  slider.max = String(Math.max(0, player.ticks - 1));
  player.play();
  play.textContent = 'Pause';
}

play.onclick = () => {
  if (player.isPlaying) {
    player.pause();
    play.textContent = 'Play';
  } else {
    player.play();
    play.textContent = 'Pause';
  }
};
$<HTMLInputElement>('loop').onchange = (e) => {
  player.loop = (e.target as HTMLInputElement).checked;
};
slider.oninput = () => player.seek(Number(slider.value));

if (import.meta.hot) import.meta.hot.on('bootscreen:update', () => void reload());
void reload();
