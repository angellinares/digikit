import { bitmapPixel, decode, HEIGHT, TICKS_PER_SECOND, WIDTH, type DecodedAsset } from './index.ts';

export interface PlayerColours {
  on: [number, number, number];
  off: [number, number, number];
}

const DEFAULT_COLOURS: PlayerColours = { on: [242, 244, 249], off: [8, 9, 12] };

/** Plays a DKBS asset on a canvas at 30 ticks/s from a monotonic clock. Plays once and holds the last frame unless `loop` is set. */
export class DkbsPlayer {
  loop = false;
  onTick: ((tick: number) => void) | undefined;

  private asset: DecodedAsset | undefined;
  private starts: number[] = [];
  private tick = 0;
  private playing = false;
  private startedAt = 0;
  private startTick = 0;
  private raf = 0;
  private readonly context: CanvasRenderingContext2D | null;
  private readonly colours: PlayerColours;

  constructor(canvas: HTMLCanvasElement, colours: PlayerColours = DEFAULT_COLOURS) {
    this.colours = colours;
    canvas.width = WIDTH;
    canvas.height = HEIGHT;
    this.context = canvas.getContext('2d');
  }

  get ticks(): number {
    return this.asset?.ticks ?? 0;
  }

  get currentTick(): number {
    return this.tick;
  }

  get isPlaying(): boolean {
    return this.playing;
  }

  load(bytes: Uint8Array): void {
    const asset = decode(bytes);
    this.pause();
    this.asset = asset;
    this.starts = [];
    let sum = 0;
    for (const frame of asset.frames) {
      this.starts.push(sum);
      sum += frame.ticks;
    }
    this.tick = 0;
    this.draw();
    this.onTick?.(0);
  }

  play(): void {
    if (!this.asset || this.playing) return;
    if (!this.loop && this.tick >= this.asset.ticks - 1) this.tick = 0;
    this.playing = true;
    this.startTick = this.tick;
    this.startedAt = performance.now();
    this.raf = requestAnimationFrame(this.step);
  }

  pause(): void {
    this.playing = false;
    cancelAnimationFrame(this.raf);
  }

  seek(tick: number): void {
    if (!this.asset) return;
    this.tick = Math.min(Math.max(Math.floor(tick), 0), this.asset.ticks - 1);
    this.startTick = this.tick;
    this.startedAt = performance.now();
    this.draw();
    this.onTick?.(this.tick);
  }

  private readonly step = (now: number): void => {
    if (!this.playing || !this.asset) return;
    const elapsed = Math.floor(((now - this.startedAt) * TICKS_PER_SECOND) / 1000);
    let tick = this.startTick + elapsed;
    const last = this.asset.ticks - 1;
    if (tick > last) {
      if (this.loop) tick %= this.asset.ticks;
      else tick = last;
    }
    const finished = !this.loop && tick === last;
    if (finished) this.playing = false;
    if (tick !== this.tick) {
      this.tick = tick;
      this.draw();
      this.onTick?.(tick);
    }
    if (finished) {
      this.onTick?.(tick);
      return;
    }
    this.raf = requestAnimationFrame(this.step);
  };

  private draw(): void {
    if (!this.context || !this.asset) return;
    let index = this.starts.length - 1;
    while (index > 0 && this.starts[index]! > this.tick) index -= 1;
    const bitmap = this.asset.frames[index]!.bitmap;
    const image = this.context.createImageData(WIDTH, HEIGHT);
    for (let y = 0; y < HEIGHT; y += 1)
      for (let x = 0; x < WIDTH; x += 1) {
        const [r, g, b] = bitmapPixel(bitmap, x, y) ? this.colours.on : this.colours.off;
        const at = 4 * (y * WIDTH + x);
        image.data[at] = r;
        image.data[at + 1] = g;
        image.data[at + 2] = b;
        image.data[at + 3] = 255;
      }
    this.context.putImageData(image, 0, 0);
  }
}
