import { execFileSync } from 'node:child_process';
import fs from 'node:fs';

const CHUNK = 1 << 20;

/**
 * Copy a card image without allocating its holes. Node's `COPYFILE_FICLONE` does not clone on macOS: it makes a dense
 * copy (24.8 GB of disk for a full-size card). `cp -c` (APFS) and `cp --reflink=auto --sparse=always` (Linux) share
 * blocks; if they fail, copy only the non-zero 1 MiB chunks into a sparse file.
 */
export function cloneFile(from: string, to: string): void {
  fs.rmSync(to, { force: true });
  const args = process.platform === 'darwin' ? ['-c', from, to] : ['--reflink=auto', '--sparse=always', from, to];
  try {
    execFileSync('cp', args, { stdio: 'pipe' });
    return;
  } catch {
    fs.rmSync(to, { force: true });
  }
  const src = fs.openSync(from, 'r');
  const dst = fs.openSync(to, 'w');
  try {
    const size = fs.fstatSync(src).size;
    fs.ftruncateSync(dst, size);
    const buf = Buffer.alloc(CHUNK);
    for (let at = 0; at < size; at += CHUNK) {
      const n = fs.readSync(src, buf, 0, CHUNK, at);
      if (n > 0 && buf.subarray(0, n).some((b) => b !== 0)) fs.writeSync(dst, buf, 0, n, at);
    }
  } finally {
    fs.closeSync(src);
    fs.closeSync(dst);
  }
}

/** Disk blocks a file really holds (512-byte units of `st_blocks`), not its apparent size. */
export function allocatedBytes(file: string): number {
  return fs.statSync(file).blocks * 512;
}
