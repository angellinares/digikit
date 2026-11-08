// The digikit mascot's sprite and eyes, drawn pixel by pixel. Used by mascot.mjs.

// '#' is lit, 'x' is dark and covers what is behind it, '.' is empty.
// The body is thicker on its right and bottom edges, so it always looks solid.
export const SMALL = [
  '......##..##..##......',
  '......##..##..##......',
  '....##############....',
  '...#xxxxxxxxxxx####...',
  '...#xxxxxxxxxxxx####..',
  '####xxxxxxxxxxxxx#####',
  '...#xxxxxxxxxxxxxx##..',
  '...#xxxxxxxxxxxxxx##..',
  '####xxxxxxxxxxxxxx####',
  '...#xxxxxxxxxxxxxx##..',
  '...#xxxxxxxxxxxxxx##..',
  '####xxxxxxxxxxxxxx####',
  '...#xxxxxxxxxxxxxx##..',
  '....##############.#..',
  '.....##############...',
  '.......##.....##......',
  '.......##.....##......',
  '.....####.....####....',
];

// Eyes, 5 by 5, left and right. The face is the only thing that changes.
export const EYES = {
  idle: [
    ['.##..', '.##..', '.##..', '.##..', '.....'],
    ['..##.', '..##.', '..##.', '..##.', '.....'],
  ],
  blink: [
    ['.....', '.....', '####.', '.....', '.....'],
    ['.....', '.....', '.####', '.....', '.....'],
  ],
  curious: [
    ['.##..', '.##..', '.##..', '.##..', '.....'],
    ['.....', '.....', '..##.', '..##.', '.....'],
  ],
  impact: [
    ['#....', '.#...', '..#..', '.#...', '#....'],
    ['....#', '...#.', '..#..', '...#.', '....#'],
  ],
  happy: [
    ['.....', '.#...', '#.#..', '.....', '.....'],
    ['.....', '...#.', '..#.#', '.....', '.....'],
  ],
  up: [
    ['.##..', '.##..', '.....', '.....', '.....'],
    ['..##.', '..##.', '.....', '.....', '.....'],
  ],
};

// A broken line on the ground, which says the character is standing on something.
export function shadow(ctx, x, y, width) {
  for (let i = 0; i < width; i += 2) ctx.fillRect(x + i, y, 1, 1);
}
