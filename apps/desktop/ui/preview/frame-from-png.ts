// `?frame=NAME` in the preview: a 128 x 64 monochrome PNG (at any integer scale) shown on the device screen instead of the test pattern.
// NAME is a file of apps/web/public/images without the extension.
const images = import.meta.glob('../../../web/public/images/*.png', {
  query: '?url',
  import: 'default',
  eager: true,
}) as Record<string, string>;

/** The 1024-byte screen layout of `testPattern()` (remote-mock.ts) from an image: the centre of each 128 x 64 cell, lit above mid grey. */
export async function framePixels(url: string): Promise<Uint8Array> {
  const img = new Image();
  img.src = url;
  await img.decode();
  const canvas = document.createElement('canvas');
  canvas.width = img.naturalWidth;
  canvas.height = img.naturalHeight;
  const ctx = canvas.getContext('2d', { willReadFrequently: true })!;
  ctx.drawImage(img, 0, 0);
  const data = ctx.getImageData(0, 0, canvas.width, canvas.height).data;
  const cw = canvas.width / 128;
  const ch = canvas.height / 64;
  const buf = new Uint8Array(1024);
  for (let y = 0; y < 64; y++)
    for (let x = 0; x < 128; x++) {
      const i = (Math.floor((y + 0.5) * ch) * canvas.width + Math.floor((x + 0.5) * cw)) * 4;
      if ((data[i] + data[i + 1] + data[i + 2]) / 3 >= 128) buf[7 - (y >> 3) + 8 * x] |= 1 << (y & 7);
    }
  return buf;
}

/** The frame named by `?frame=`, or undefined (no option, or no such image). */
export async function queryFrame(): Promise<Uint8Array | undefined> {
  const name = new URLSearchParams(location.search).get('frame');
  const url = name ? images[`../../../web/public/images/${name}.png`] : undefined;
  return url ? framePixels(url) : undefined;
}
