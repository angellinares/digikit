/** WAV and PNG files of a rig output (no dependencies: `node:zlib` and a 3x5 pixel font). Used by the report mode of `rigSuite` (docs/TESTING.md "Classifying a changed pin"). */
import { mkdirSync, writeFileSync } from 'node:fs';
import path from 'node:path';
import { crc32, deflateSync } from 'node:zlib';
import { FS } from './analysis.ts';
import { BANDS, bandLevels, bandEdges } from './fingerprint.ts';

/** Mono float32 WAV (format 3). */
export function wavBytes(x: readonly number[], rate = FS): Buffer {
  const data = Buffer.from(Float32Array.from(x).buffer);
  const h = Buffer.alloc(44);
  h.write('RIFF', 0);
  h.writeUInt32LE(36 + data.length, 4);
  h.write('WAVEfmt ', 8);
  h.writeUInt32LE(16, 16);
  h.writeUInt16LE(3, 20);
  h.writeUInt16LE(1, 22);
  h.writeUInt32LE(rate, 24);
  h.writeUInt32LE(rate * 4, 28);
  h.writeUInt16LE(4, 32);
  h.writeUInt16LE(32, 34);
  h.write('data', 36);
  h.writeUInt32LE(data.length, 40);
  return Buffer.concat([h, data]);
}

export function writeWav(file: string, x: readonly number[]): void {
  mkdirSync(path.dirname(file), { recursive: true });
  writeFileSync(file, wavBytes(x));
}

// ---- a small raster -------------------------------------------------------------------------------------------------------------

type Rgb = [number, number, number];

export class Raster {
  readonly px: Uint8Array;
  readonly w: number;
  readonly h: number;
  constructor(w: number, h: number, bg: Rgb = [255, 255, 255]) {
    this.w = w;
    this.h = h;
    this.px = new Uint8Array(w * h * 3);
    for (let i = 0; i < w * h; i++) this.px.set(bg, 3 * i);
  }
  set(x: number, y: number, c: Rgb): void {
    x = Math.round(x);
    y = Math.round(y);
    if (x >= 0 && y >= 0 && x < this.w && y < this.h) this.px.set(c, 3 * (y * this.w + x));
  }
  rect(x0: number, y0: number, x1: number, y1: number, c: Rgb): void {
    for (let y = y0; y <= y1; y++) for (let x = x0; x <= x1; x++) this.set(x, y, c);
  }
  line(x0: number, y0: number, x1: number, y1: number, c: Rgb): void {
    const n = Math.max(Math.abs(x1 - x0), Math.abs(y1 - y0), 1);
    for (let i = 0; i <= n; i++) this.set(x0 + ((x1 - x0) * i) / n, y0 + ((y1 - y0) * i) / n, c);
  }
  text(x: number, y: number, s: string, c: Rgb = [30, 30, 30], scale = 3): void {
    let cx = x;
    for (const ch of s.toUpperCase()) {
      const g = FONT[ch] ?? FONT[' ']!;
      for (let r = 0; r < 5; r++)
        for (let q = 0; q < 3; q++)
          if (g[3 * r + q] === '1')
            this.rect(cx + q * scale, y + r * scale, cx + q * scale + scale - 1, y + r * scale + scale - 1, c);
      cx += 4 * scale;
    }
  }
  png(): Buffer {
    const raw = Buffer.alloc((this.w * 3 + 1) * this.h);
    for (let y = 0; y < this.h; y++) {
      raw[y * (this.w * 3 + 1)] = 0;
      raw.set(this.px.subarray(y * this.w * 3, (y + 1) * this.w * 3), y * (this.w * 3 + 1) + 1);
    }
    const chunk = (type: string, body: Buffer): Buffer => {
      const len = Buffer.alloc(4);
      len.writeUInt32BE(body.length);
      const crc = Buffer.alloc(4);
      crc.writeUInt32BE(crc32(Buffer.concat([Buffer.from(type), body])));
      return Buffer.concat([len, Buffer.from(type), body, crc]);
    };
    const ihdr = Buffer.alloc(13);
    ihdr.writeUInt32BE(this.w, 0);
    ihdr.writeUInt32BE(this.h, 4);
    ihdr.set([8, 2, 0, 0, 0], 8);
    return Buffer.concat([
      Buffer.from([0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a]),
      chunk('IHDR', ihdr),
      chunk('IDAT', deflateSync(raw)),
      chunk('IEND', Buffer.alloc(0)),
    ]);
  }
}

// 3x5 glyphs, rows top to bottom, three bits per row
const FONT: Record<string, string> = {
  ' ': '000000000000000',
  A: '010101111101101',
  B: '110101110101110',
  C: '011100100100011',
  D: '110101101101110',
  E: '111100110100111',
  F: '111100110100100',
  G: '011100101101011',
  H: '101101111101101',
  I: '111010010010111',
  J: '001001001101010',
  K: '101101110101101',
  L: '100100100100111',
  M: '101111111101101',
  N: '110101101101101',
  O: '010101101101010',
  P: '110101110100100',
  Q: '010101101111011',
  R: '110101110101101',
  S: '011100010001110',
  T: '111010010010010',
  U: '101101101101111',
  V: '101101101101010',
  W: '101101111111101',
  X: '101101010101101',
  Y: '101101010010010',
  Z: '111001010100111',
  '0': '111101101101111',
  '1': '010110010010111',
  '2': '110001010100111',
  '3': '110001010001110',
  '4': '101101111001001',
  '5': '111100110001110',
  '6': '011100111101111',
  '7': '111001010010010',
  '8': '111101111101111',
  '9': '111101111001110',
  '.': '000000000000010',
  '-': '000000111000000',
  ':': '000010000010000',
  '/': '001001010100100',
  '(': '010100100100010',
  ')': '010001001001010',
  '+': '000010111010000',
  ',': '000000000010100',
  '=': '000111000111000',
  '>': '100010001010100',
};

const BLUE: Rgb = [40, 90, 200];
const ORANGE: Rgb = [235, 110, 20];
const RED: Rgb = [200, 30, 30];
const GRID: Rgb = [215, 215, 215];

/** A column of the waveform: the min and max of the samples that fall on it. */
function columns(x: readonly number[], width: number): [number, number][] {
  return Array.from({ length: width }, (_, c) => {
    const lo = Math.floor((c * x.length) / width);
    const hi = Math.max(lo + 1, Math.floor(((c + 1) * x.length) / width));
    let mn = Infinity;
    let mx = -Infinity;
    for (let i = lo; i < hi && i < x.length; i++) {
      mn = Math.min(mn, x[i]!);
      mx = Math.max(mx, x[i]!);
    }
    return mn === Infinity ? [0, 0] : [mn, mx];
  });
}

/**
 * The plot of one case: the waveform of the output (orange) over the reference (blue), the difference (red, own scale), and the band spectra of both.
 * `ref` is null when there is no cached output: only the output is drawn.
 */
export function plotPng(title: string, verdict: string, now: readonly number[], ref: readonly number[] | null): Buffer {
  const W = 1400;
  const H = 1020;
  const L = 70;
  const R = W - 20;
  const img = new Raster(W, H);
  img.text(L, 12, title, [0, 0, 0], 3);
  img.text(L, 36, verdict.slice(0, 105), [60, 60, 60], 2);
  const panel = (y0: number, y1: number, name: string) => {
    img.rect(L, y0, R, y0, GRID);
    img.rect(L, y1, R, y1, GRID);
    img.rect(L, y0, L, y1, GRID);
    img.rect(R, y0, R, y1, GRID);
    img.text(L + 6, y0 + 6, name, [90, 90, 90], 2);
  };
  const wave = (x: readonly number[], y0: number, y1: number, scale: number, c: Rgb) => {
    const mid = (y0 + y1) / 2;
    columns(x, R - L).forEach(([mn, mx], i) =>
      img.line(L + i, mid - (mx / scale) * ((y1 - y0) / 2 - 4), L + i, mid - (mn / scale) * ((y1 - y0) / 2 - 4), c),
    );
  };
  // 1 waveform
  panel(60, 380, 'WAVEFORM  ORANGE = OUTPUT   BLUE = REFERENCE');
  const top = Math.max(1e-9, ...now.map(Math.abs), ...(ref ?? []).map(Math.abs));
  img.line(L, 220, R, 220, GRID);
  if (ref) wave(ref, 60, 380, top, BLUE);
  wave(now, 60, 380, top, ORANGE);
  img.text(L + 6, 354, `FULL SCALE = ${top.toPrecision(3)}  SAMPLES = ${now.length}`, [90, 90, 90], 2);
  // 2 difference
  panel(400, 620, 'DIFFERENCE (OUTPUT - REFERENCE), OWN SCALE');
  if (ref) {
    const n = Math.min(now.length, ref.length);
    const d = Array.from({ length: n }, (_, i) => now[i]! - ref[i]!);
    const dm = Math.max(...d.map(Math.abs));
    img.line(L, 510, R, 510, GRID);
    if (dm > 0) wave(d, 400, 620, dm, RED);
    img.text(L + 6, 594, `PEAK DIFFERENCE = ${dm.toPrecision(3)}`, [90, 90, 90], 2);
  } else img.text(L + 6, 500, 'NO CACHED REFERENCE OUTPUT', [150, 150, 150], 3);
  // 3 band spectra
  panel(640, 1000, 'BAND LEVELS, 64 LOG BANDS 40 HZ TO 22 KHZ (DB)');
  const a = bandLevels(now);
  const b = ref ? bandLevels(ref) : null;
  const hi = Math.max(...a, ...(b ?? []));
  const lo = hi - 120;
  const yOf = (v: number) => 1000 - 8 - ((Math.max(v, lo) - lo) / (hi - lo)) * (360 - 30);
  for (let k = 0; k <= 4; k++) {
    const yy = yOf(hi - 30 * k);
    img.line(L, yy, R, yy, GRID);
    img.text(4, yy - 4, `${Math.round(hi - 30 * k)}`, [90, 90, 90], 2);
  }
  const xOf = (i: number) => L + ((i + 0.5) / BANDS) * (R - L);
  const edges = bandEdges();
  for (const f of [100, 1000, 10000]) {
    const i = Math.log(f / edges[0]!) / Math.log(edges[1]! / edges[0]!);
    img.line(L + (i / BANDS) * (R - L), 660, L + (i / BANDS) * (R - L), 995, GRID);
    img.text(L + (i / BANDS) * (R - L) + 4, 975, `${f >= 1000 ? f / 1000 + 'K' : f}HZ`, [90, 90, 90], 2);
  }
  const draw = (v: number[], c: Rgb) =>
    v.forEach((p, i) => i && img.line(xOf(i - 1), yOf(v[i - 1]!), xOf(i), yOf(p), c));
  if (b) draw(b, BLUE);
  draw(a, ORANGE);
  return img.png();
}

export function writePng(file: string, png: Buffer): void {
  mkdirSync(path.dirname(file), { recursive: true });
  writeFileSync(file, png);
}
