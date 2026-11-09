// The digikit mascot boot screen. A progress bar stalls at 87% and swings down, and a
// full-screen exception slams on. The mascot, who ran in as the bar filled, stares at it,
// inspects it, leaps at the viewer, turns around in the air and falls back into the
// exception. Everything except the word Digikit crashes down, the mascot drops out of
// sight, and the word is left on the screen. 4 s long, so boot takes as long as stock.
//
// The mascot is one sprite. In the leap it is enlarged in crisp blocks, so it stays
// the same character at every size.
import { SMALL, EYES, shadow } from './mascot-art.mjs';

export const ticks = 120;

const FILL_END = 14;
const SCAMPER_END = 16; // the mascot has run in from the right
const PUZZLED = 23; // the bar has settled and the mascot works out what happened
const SLAM = 38; // the exception lands
const CROUCH = 42;
const LEAP = 45;
const TURN = 63; // it is close to the viewer and starts to turn around
const FALL = 69; // it falls back towards the exception
const HIT = 79; // it hits the exception

const BAR = { x: 14, y: 24, w: 100, h: 8 };
const FLOOR_TOP = 45; // the y of the sprite when standing
const START_X = 130; // the sprite's x off the right of the screen
const STAND_X = 53; // the sprite's x in front of the exception
const HIT_Y = 40; // the centre of the Digikit band, where it lands
const SAG = 10;
const SWING = [2, 6, 12, 11, 8, 9, 11, 10];

const clamp = (v, lo, hi) => Math.min(hi, Math.max(lo, v));
const ease = (u) => 1 - (1 - u) * (1 - u);
const lerp = (a, b, u) => a + (b - a) * u;

const FONT = {
  ' ': [0, 0, 0, 0, 0, 0, 0],
  '%': [0x18, 0x19, 0x02, 0x04, 0x08, 0x13, 0x03],
  0: [0x0e, 0x11, 0x13, 0x15, 0x19, 0x11, 0x0e],
  1: [0x04, 0x0c, 0x04, 0x04, 0x04, 0x04, 0x0e],
  2: [0x0e, 0x11, 0x01, 0x02, 0x04, 0x08, 0x1f],
  3: [0x1f, 0x02, 0x04, 0x02, 0x01, 0x11, 0x0e],
  4: [0x02, 0x06, 0x0a, 0x12, 0x1f, 0x02, 0x02],
  5: [0x1f, 0x10, 0x1e, 0x01, 0x01, 0x11, 0x0e],
  6: [0x06, 0x08, 0x10, 0x1e, 0x11, 0x11, 0x0e],
  7: [0x1f, 0x01, 0x02, 0x04, 0x08, 0x08, 0x08],
  8: [0x0e, 0x11, 0x11, 0x0e, 0x11, 0x11, 0x0e],
  9: [0x0e, 0x11, 0x11, 0x0f, 0x01, 0x02, 0x0c],
  A: [0x0e, 0x11, 0x11, 0x1f, 0x11, 0x11, 0x11],
  B: [0x1e, 0x11, 0x11, 0x1e, 0x11, 0x11, 0x1e],
  C: [0x0e, 0x11, 0x10, 0x10, 0x10, 0x11, 0x0e],
  D: [0x1e, 0x11, 0x11, 0x11, 0x11, 0x11, 0x1e],
  E: [0x1f, 0x10, 0x10, 0x1e, 0x10, 0x10, 0x1f],
  F: [0x1f, 0x10, 0x10, 0x1e, 0x10, 0x10, 0x10],
  I: [0x0e, 0x04, 0x04, 0x04, 0x04, 0x04, 0x0e],
  K: [0x11, 0x12, 0x14, 0x18, 0x14, 0x12, 0x11],
  M: [0x11, 0x1b, 0x15, 0x15, 0x11, 0x11, 0x11],
  N: [0x11, 0x19, 0x15, 0x13, 0x11, 0x11, 0x11],
  O: [0x0e, 0x11, 0x11, 0x11, 0x11, 0x11, 0x0e],
  P: [0x1e, 0x11, 0x11, 0x1e, 0x10, 0x10, 0x10],
  T: [0x1f, 0x04, 0x04, 0x04, 0x04, 0x04, 0x04],
  V: [0x11, 0x11, 0x11, 0x11, 0x11, 0x0a, 0x04],
  X: [0x11, 0x11, 0x0a, 0x04, 0x0a, 0x11, 0x11],
  '?': [0x0e, 0x11, 0x01, 0x02, 0x04, 0x00, 0x04],
  g: [0, 0x0f, 0x11, 0x11, 0x0f, 0x01, 0x0e],
  i: [0x04, 0, 0x0c, 0x04, 0x04, 0x04, 0x0e],
  k: [0x10, 0x10, 0x12, 0x14, 0x18, 0x14, 0x12],
  t: [0x08, 0x08, 0x1c, 0x08, 0x08, 0x09, 0x06],
};

function text(ctx, string, x, y, s = 1, pitch = 7) {
  for (const c of string) {
    FONT[c].forEach((row, r) => {
      for (let col = 0; col < 5; col++) {
        if (row & (0x10 >> col)) ctx.fillRect(x + col * s, y + r * s, s, s);
      }
    });
    x += pitch * s;
  }
}

function percent(ctx, value) {
  const string = `${value}%`;
  text(ctx, string, Math.round(64 - (string.length * 12 - 2) / 2), 8, 2, 6);
}

function bar(ctx, value, sag) {
  const { x, y, w, h } = BAR;
  const right = x + w - 1;
  const fillEnd = x + 2 + Math.round(((w - 4) * value) / 100);
  for (let c = x; c <= right; c++) {
    const off = Math.round((sag * (c - x)) / (right - x));
    if (c === x || c === right) {
      ctx.fillRect(c, y + off, 1, h);
      continue;
    }
    ctx.fillRect(c, y + off, 1, 1);
    ctx.fillRect(c, y + off + h - 1, 1, 1);
    if (c >= x + 2 && c < fillEnd) ctx.fillRect(c, y + off + 2, 1, h - 4);
  }
}

// A few specks flying out from where the bar came loose.
function puff(ctx, age) {
  if (age < 0 || age > 7) return;
  const x = BAR.x + BAR.w - 3;
  const y = BAR.y + 2;
  for (const [dx, dy] of [
    [-3, -3],
    [2, -4],
    [4, 0],
    [-1, 3],
    [3, 3],
  ]) {
    const reach = 1 + age * 0.7;
    const size = age < 4 ? 2 : 1;
    ctx.fillRect(Math.round(x + dx * reach), Math.round(y + dy * reach), size, size);
  }
}

const HEX_ROWS = [
  '40089F38424059F8',
  '405662E042405820',
  '400450D6424059F8',
  '0000000400000004',
  '4056640140566370',
  '4056637C40566378',
];
const WORD_X = 19;
const WORD_Y = 33;

function header(ctx, dy) {
  ctx.fillRect(0, dy, 128, 17);
  ctx.fillStyle = '#000';
  text(ctx, 'EXCEPTION  DK0087', 4, dy + 1);
  text(ctx, 'V05 M0 P08700000', 8, dy + 9);
  ctx.fillStyle = '#fff';
}

function word(ctx, dy) {
  text(ctx, 'Digikit', WORD_X, WORD_Y + dy, 2, 6.5);
}

function exception(ctx, dy) {
  header(ctx, dy);
  HEX_ROWS.forEach((row, r) => text(ctx, row, 8, dy + 17 + r * 8));
  ctx.fillStyle = '#000';
  ctx.fillRect(0, dy + 32, 128, 17);
  ctx.fillStyle = '#fff';
  ctx.fillRect(0, dy + 32, 128, 1);
  ctx.fillRect(0, dy + 48, 128, 1);
  word(ctx, dy);
}

// A character centred on the origin, for a turned piece. Its pixels are a little
// oversized so that thin strokes survive being rotated at one bit.
function tumbled(ctx, c) {
  FONT[c].forEach((row, r) => {
    for (let col = 0; col < 5; col++) {
      if (row & (0x10 >> col)) ctx.fillRect(col - 2.6, r - 3.6, 1.2, 1.2);
    }
  });
}

// Every character of the exception as its own piece. The header's are white tiles
// with a dark letter, and `w` and `h` are the tile's size; dump digits have no tile.
function makePieces() {
  const list = [];
  const tile = (c, x, y, w, h) => list.push({ c, x, y, w, h });
  [
    ['EXCEPTION  DK0087', 4, 0, 8],
    ['V05 M0 P08700000', 8, 8, 9],
  ].forEach(([string, left, y, h]) => {
    [...string].forEach((c, i) => tile(c, left + i * 7, y, 7, h));
    tile(' ', 0, y, left, h);
    const end = left + string.length * 7;
    tile(' ', end, y, 128 - end, h);
  });
  for (const r of [0, 1, 4, 5]) {
    [...HEX_ROWS[r]].forEach((c, i) => list.push({ c, x: 8 + i * 7, y: 17 + r * 8 }));
  }
  // A fixed scatter for each piece, so every run is the same.
  list.forEach((p, i) => {
    p.delay = ((i * 97 + 13) % 31) / 31;
    p.drift = (((i * 61 + 7) % 23) / 23 - 0.5) * 1.1;
    p.wobble = 1 + ((i * 41 + 5) % 17) / 17 * 2.5; // how far it sways, in pixels
    p.speed = 0.45 + ((i * 29 + 3) % 13) / 13 * 0.5; // how quickly it sways
    p.phase = ((i * 53 + 11) % 19) / 19 * Math.PI * 2;
    p.spin = (((i * 71 + 17) % 29) / 29 - 0.5) * 0.6; // turn per tick, either way
    p.flip = 0.12 + ((i * 37 + 19) % 11) / 11 * 0.3; // how fast it tumbles edge-on
  });
  return list;
}
const PIECES = makePieces();

// Everything but the word and its two lines crumbles away, spreading out from the hit.
function crash(ctx, t) {
  for (const p of PIECES) {
    const distance = Math.hypot(p.x + 3 - 64, (p.y - HIT_Y) * 1.4);
    const start = HIT + 2 + Math.round(distance / 9 + p.delay * 3);
    const age = Math.max(0, t - start);
    const y = p.y + Math.round(age + age * age * 0.7);
    // A steady drift plus a sway that changes direction as it falls.
    const sway = Math.sin(age * p.speed + p.phase) - Math.sin(p.phase);
    const x = p.x + Math.round(p.drift * age + sway * p.wobble);
    if (y > 64) continue;
    if (age === 0) {
      // Still in place: drawn square, so the screen stays sharp until it breaks.
      if (p.w) {
        ctx.fillRect(x, y, p.w, p.h);
        ctx.fillStyle = '#000';
        text(ctx, p.c, x, y + 1);
        ctx.fillStyle = '#fff';
      } else {
        text(ctx, p.c, x, y);
      }
      continue;
    }
    // Falling: turned on the screen, and narrowed as it tumbles edge-on.
    const w = p.w ?? 5;
    const h = p.h ?? 7;
    ctx.save();
    ctx.translate(x + w / 2, y + h / 2);
    ctx.rotate(p.spin * age);
    ctx.scale(Math.max(0.3, Math.abs(Math.cos(age * p.flip))), 1);
    if (p.w) {
      ctx.fillRect(-w / 2, -h / 2, w, h);
      ctx.fillStyle = '#000';
    }
    tumbled(ctx, p.c);
    ctx.restore();
    ctx.fillStyle = '#fff';
  }
  // Falling pieces pass behind the band, which does not move.
  ctx.fillStyle = '#000';
  ctx.fillRect(0, 32, 128, 17);
  ctx.fillStyle = '#fff';
  ctx.fillRect(0, 32, 128, 1);
  ctx.fillRect(0, 48, 128, 1);
  word(ctx, 0);
}

const LEGS = {
  stand: ['.......##.....##......', '.......##.....##......', '.....####.....####....'],
  a: ['.......##.....##......', '.....###.....###......', '.....##......##.......'],
  b: ['.......##.....##......', '........###.....###...', '.........##......##...'],
  crouch: ['......................', '.......##.....##......', '.....####.....####....'],
  spread: ['.....##.........##....', '....##...........##...', '...###...........###..'],
};

// Draws rows of cells, with every cell on whole pixels. `sx` is the cell width.
function cells(ctx, rows, x, y, s, halo, sx = s) {
  rows.forEach((row, r) => {
    [...row].forEach((c, col) => {
      if (c === '.') return;
      const left = Math.round(x + col * sx);
      const top = Math.round(y + r * s);
      const right = Math.round(x + (col + 1) * sx);
      const bottom = Math.round(y + (r + 1) * s);
      if (halo) {
        ctx.fillStyle = '#000';
        ctx.fillRect(left - 1, top - 1, right - left + 2, bottom - top + 2);
      } else {
        ctx.fillStyle = c === 'x' ? '#000' : '#fff';
        ctx.fillRect(left, top, Math.max(1, right - left), bottom - top);
      }
    });
  });
  ctx.fillStyle = '#fff';
}

// The mascot at any size. A dark halo keeps it readable over the exception's text.
// `narrow` from 0 to 1 squeezes its width, for the turn. `back` shows it from behind,
// with no eyes and the sprite mirrored.
function mascot(ctx, x, y, { face = 'idle', legs = 'stand', lean = 0, s = 1, narrow = 1, back = false } = {}) {
  const sx = s * narrow;
  const body = [...SMALL.slice(0, 15), ...LEGS[legs]];
  const rows = back ? body.map((row) => [...row].reverse().join('')) : body;
  const place = (r) => x + (r < 8 ? lean : 0);
  rows.forEach((row, r) => cells(ctx, [row], place(r), y + r * s, s, true, sx));
  rows.forEach((row, r) => cells(ctx, [row], place(r), y + r * s, s, false, sx));
  if (back) return;
  cells(ctx, EYES[face][0], x + (6 + lean) * sx, y + 6 * s, s, false, sx);
  cells(ctx, EYES[face][1], x + (11 + lean) * sx, y + 6 * s, s, false, sx);
}

function clearLine(ctx, x0, y0, x1, y1) {
  const n = Math.ceil(Math.max(Math.abs(x1 - x0), Math.abs(y1 - y0))) || 1;
  for (const [size, colour, inset] of [
    [3, '#000', 1],
    [1, '#fff', 0],
  ]) {
    ctx.fillStyle = colour;
    for (let i = 0; i <= n; i++) {
      ctx.fillRect(Math.round(x0 + ((x1 - x0) * i) / n) - inset, Math.round(y0 + ((y1 - y0) * i) / n) - inset, size, size);
    }
  }
}

function groundShadow(ctx, x, width) {
  ctx.fillStyle = '#000';
  ctx.fillRect(x - 1, 62, width + 2, 2);
  ctx.fillStyle = '#fff';
  shadow(ctx, x, 63, width);
}

const leapSize = (age) => Math.pow(1.125, age);

// Where the sprite's top-left is during the leap: an arc up and in to the screen's middle.
function leapPlace(age) {
  const u = clamp(age / (TURN - LEAP), 0, 1);
  const s = leapSize(age);
  return {
    s,
    x: lerp(STAND_X, 64 - 11 * s, u * u),
    y: lerp(FLOOR_TOP, 32 - 8.5 * s, u * u) - Math.sin(u * Math.PI) * 12,
  };
}

// Lines rushing out from the middle of the screen as it comes towards the viewer.
function speedLines(ctx, age) {
  for (let i = 0; i < 12; i++) {
    const angle = (i / 12) * Math.PI * 2 + 0.2 * (i % 3);
    const r = ((age * 7 + i * 11) % 60) + 26;
    const length = 8 + r * 0.3;
    clearLine(
      ctx,
      64 + Math.cos(angle) * r,
      32 + Math.sin(angle) * r * 0.65,
      64 + Math.cos(angle) * (r + length),
      32 + Math.sin(angle) * (r + length) * 0.65,
    );
  }
}

// Outlines left behind at its earlier, smaller sizes.
function trail(ctx, age) {
  for (const back of [6, 4, 2]) {
    if (age - back < 0) continue;
    const { x, y, s } = leapPlace(age - back);
    const left = Math.round(x + 3 * s);
    const top = Math.round(y + 2 * s);
    const w = Math.round(16 * s);
    const h = Math.round(12 * s);
    ctx.fillStyle = '#000';
    ctx.fillRect(left - 1, top - 1, w + 2, h + 2);
    ctx.fillStyle = '#fff';
    ctx.fillRect(left, top, w, 1);
    ctx.fillRect(left, top + h - 1, w, 1);
    ctx.fillRect(left, top, 1, h);
    ctx.fillRect(left + w - 1, top, 1, h);
    ctx.fillStyle = '#000';
    ctx.fillRect(left + 1, top + 1, w - 2, h - 2);
    ctx.fillStyle = '#fff';
  }
}

// The mascot's size and place as it falls back into the exception.
const peak = leapSize(TURN - LEAP);
const fallSize = (age) => peak * Math.pow(0.85, age);
const LANDED = fallSize(HIT - FALL);

function placeCentred(ctx, cy, s, options) {
  mascot(ctx, 64 - 11 * s * (options.narrow ?? 1), cy - 8.5 * s, { ...options, s });
}

export function draw(ctx, t) {
  ctx.fillStyle = '#000';
  ctx.fillRect(0, 0, 128, 64);
  ctx.fillStyle = '#fff';

  if (t < SLAM) {
    const value = t < FILL_END ? Math.round(87 * ease(t / FILL_END)) : 87;
    bar(ctx, value, t < FILL_END ? 0 : t - FILL_END < SWING.length ? SWING[t - FILL_END] : SAG);
    percent(ctx, value);
    puff(ctx, t - FILL_END);
    if (t < SCAMPER_END) {
      const x = Math.round(START_X + (STAND_X - START_X) * (t / SCAMPER_END));
      mascot(ctx, x, FLOOR_TOP - (t % 2), { legs: t % 4 < 2 ? 'a' : 'b' });
      groundShadow(ctx, x + 4, 15);
    } else if (t < PUZZLED) {
      // It stops and looks up as the bar comes loose.
      mascot(ctx, STAND_X, FLOOR_TOP, { face: t === SCAMPER_END + 3 ? 'blink' : 'up' });
      groundShadow(ctx, STAND_X + 4, 15);
    } else {
      // A clear, puzzled beat on an empty screen: one eye narrows and its head tilts.
      const age = t - PUZZLED;
      const blink = age === 9 || age === 10;
      mascot(ctx, STAND_X, FLOOR_TOP, { face: blink ? 'blink' : 'curious', lean: age < 7 ? -1 : 1 });
      text(ctx, '?', STAND_X + 26, FLOOR_TOP - 3, 2, 6);
      groundShadow(ctx, STAND_X + 4, 15);
    }
    return;
  }

  if (t >= HIT) {
    const age = t - HIT;
    if (age < 2) {
      exception(ctx, age === 0 ? 3 : -2); // the jolt of the hit
      placeCentred(ctx, HIT_Y, LANDED, { back: true, legs: 'spread' });
      return;
    }
    crash(ctx, t);
    // Once the pieces start to go, the mascot drops quickly out of sight.
    const drop = age - 2;
    const cy = HIT_Y + drop + drop * drop * 0.9;
    if (cy < 90) placeCentred(ctx, cy, LANDED, { back: true, legs: 'spread' });
    return;
  }

  // The exception lands hard, then stays behind everything that follows.
  const shake = [5, -4, 3, -2, 1, -1];
  exception(ctx, shake[t - SLAM] ?? 0);

  if (t < CROUCH) {
    // A beat after the exception lands, staring up at it.
    mascot(ctx, STAND_X, FLOOR_TOP, { face: 'up' });
    groundShadow(ctx, STAND_X + 4, 15);
  } else if (t >= CROUCH && t < LEAP) {
    mascot(ctx, STAND_X, FLOOR_TOP + 1, { face: 'up', legs: 'crouch' });
    groundShadow(ctx, STAND_X + 2, 19);
  } else if (t >= LEAP && t < TURN) {
    const age = t - LEAP;
    speedLines(ctx, age);
    trail(ctx, age);
    const { x, y, s } = leapPlace(age);
    mascot(ctx, x, y, { face: 'impact', legs: 'spread', s });
  } else if (t >= TURN && t < FALL) {
    // Turns around in the air: it narrows, and comes back showing its back.
    const v = (t - TURN + 1) / (FALL - TURN + 1);
    const narrow = Math.max(0.12, Math.abs(Math.cos(v * Math.PI)));
    placeCentred(ctx, 32, peak, { face: 'impact', legs: 'spread', narrow, back: v > 0.5 });
  } else if (t >= FALL) {
    const age = t - FALL;
    const u = age / (HIT - FALL);
    placeCentred(ctx, lerp(32, HIT_Y, u * u), fallSize(age), { back: true, legs: 'spread' });
  }
}
