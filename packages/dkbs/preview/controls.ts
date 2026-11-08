import { MAX_FILE_BYTES, TICKS_PER_SECOND } from '@digi/dkbs';
import type { DkbsPlayer } from '@digi/dkbs/player';

/** The tick, time and size line shared by the preview and the gallery. */
export function readoutText(player: DkbsPlayer, size: number): string {
  const tick = player.currentTick;
  return (
    `tick ${tick} / ${player.ticks} · ${(tick / TICKS_PER_SECOND).toFixed(2)} s of ${(player.ticks / TICKS_PER_SECOND).toFixed(2)} s · ` +
    `${size} of ${MAX_FILE_BYTES} bytes`
  );
}
