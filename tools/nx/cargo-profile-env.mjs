// Cargo permits release profile and build-override settings through CARGO_PROFILE_RELEASE_*.
// Nx env inputs cannot express a prefix; sort the present keys for a stable runtime input.
import { resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

export const releaseProfileEnv = (env = process.env) =>
  JSON.stringify(
    Object.keys(env)
      .filter((key) => key.startsWith('CARGO_PROFILE_RELEASE_') && env[key] !== undefined)
      .sort()
      .map((key) => [key, env[key]]),
  );

if (process.argv[1] && resolve(process.argv[1]) === fileURLToPath(import.meta.url)) console.log(releaseProfileEnv());
