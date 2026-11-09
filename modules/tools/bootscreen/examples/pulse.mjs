// Example boot screen: an expanding ring and a short bar. Canvas 2D only; 128x64, 30 ticks per second.
// Pack it with: mise run bootscreen -- sheet modules/tools/bootscreen/examples/pulse.mjs
export const ticks = 90;

export function draw(ctx, t) {
  ctx.fillStyle = '#000';
  ctx.fillRect(0, 0, 128, 64);
  ctx.fillStyle = '#fff';
  ctx.strokeStyle = '#fff';

  // ring: grows from 2 to 26 px radius every 45 ticks
  const phase = (t % 45) / 45;
  ctx.lineWidth = 2;
  ctx.beginPath();
  ctx.arc(64, 28, 2 + 24 * phase, 0, Math.PI * 2);
  ctx.stroke();

  // bar: fills over the whole animation
  ctx.strokeRect(24.5, 56.5, 79, 5);
  ctx.fillRect(26, 58, Math.round((76 * (t + 1)) / ticks), 2);
}
