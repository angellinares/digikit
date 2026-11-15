// The hash of a committed core object that follows its bytes: the content without `source_sha256` and `toolchain.cflags`, the
// fields that follow the source text and the flag list (a comment edit changes the first and no other field; a flag that changes
// code changes the sections). Shared by device-bytes.mjs and runtime-owners.mjs.
import { createHash } from 'node:crypto';
import { readFileSync } from 'node:fs';

export const objectHash = (path) => {
  const object = JSON.parse(readFileSync(path, 'utf8'));
  delete object.source_sha256;
  delete object.toolchain?.cflags;
  return createHash('sha256').update(JSON.stringify(object)).digest('hex');
};
