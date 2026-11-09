import { mkdtempSync, rmSync, symlinkSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { afterAll, describe, expect, it } from 'vitest';
import { overrideInputs } from '../device-inputs.mjs';
import { releaseProfileEnv } from '../cargo-profile-env.mjs';

const scratch = mkdtempSync(join(tmpdir(), 'digikit-nx-runtime-inputs-'));
afterAll(() => rmSync(scratch, { recursive: true, force: true }));

describe('device override cache inputs', () => {
  it('keeps the default stable and ignores unrelated environment and files', async () => {
    const before = await overrideInputs({});
    writeFileSync(join(scratch, 'unrelated'), 'changed');
    expect(await overrideInputs({ UNRELATED: 'changed' })).toEqual(before);
  });

  it.each([
    'DIGI_EMUD',
    'DIGI_EMUD_DT2',
    'DIGI_EMUD_DN2',
    'DIGI_PATCHKIT',
    'DIGI_PATCHKIT_BIN',
    'DIGI_PLUSDRIVE',
    'DIGI_STORAGE_FAULT_EMUD',
    'DIGI_LP0_EMUD',
    'DIGI_DSP_PEEK_EMUD',
    'DIGI_DDR_MONITOR_EMUD',
    'DIGI_PATCHED_STARTUP_EMUD',
    'DIGI_STEREO_PROOF_EMUD',
    'DIGI_CATALOG_EMUD',
    'DIGI_FILES_EMUD_DT2',
    'DIGI_FILES_EMUD_DN2',
  ])('invalidates %s for in-place binary changes and follows symlinks', async (key) => {
    const binary = join(scratch, key);
    const link = `${binary}-link`;
    writeFileSync(binary, 'first');
    symlinkSync(binary, link);
    const before = await overrideInputs({ [key]: link });
    expect(await overrideInputs({ [key]: link })).toEqual(before);
    writeFileSync(binary, 'second');
    expect(await overrideInputs({ [key]: link })).not.toEqual(before);
    expect(await overrideInputs({ [key]: binary })).not.toEqual(await overrideInputs({ [key]: link }));
    rmSync(binary);
    const missing = await overrideInputs({ [key]: link });
    expect(missing).not.toEqual(before);
    expect(await overrideInputs({ [key]: link })).toEqual(missing);
  });

  it.each(['PATCHKIT_MODULES', 'DIGI_PK_ROOT', 'DIGI_RUNTIME_MODULES'])(
    'prevents cache reuse for nonempty %s, even at an unchanged or missing path',
    async (key) => {
      const path = join(scratch, `${key}-missing`);
      const before = await overrideInputs({ [key]: path });
      expect(await overrideInputs({ [key]: path })).not.toEqual(before);
      writeFileSync(path, 'changed in place');
      expect(await overrideInputs({ [key]: path })).not.toEqual(before);
      const empty = await overrideInputs({ [key]: '' });
      if (key === 'DIGI_RUNTIME_MODULES') expect(await overrideInputs({ [key]: '' })).not.toEqual(empty);
      else expect(await overrideInputs({ [key]: '' })).toEqual(empty);
    },
  );

  it.each([
    'DIGI_CATALOG_CARD',
    'DIGI_FILES_CARD_DT2',
    'DIGI_FILES_PATCH_DT2',
    'DIGI_FILES_PATCH_DN2',
    'DIGI_FILES_PLAN_DT2',
    'DIGI_FILES_PLAN_DN2',
    'DIGI_LP0_BOOT_FIXTURE',
  ])('prevents reuse with specialized external fixture %s, including an empty path', async (key) => {
    for (const value of ['', join(scratch, 'external-fixture')]) {
      expect(await overrideInputs({ [key]: value })).not.toEqual(await overrideInputs({ [key]: value }));
    }
  });

  it.each(['DIGI_LP0_VOICES', 'DIGI_LP0_CLOSE', 'DIGI_LP0_AUDIO', 'DIGI_LP0_FULL_BANK'])(
    'invalidates when the %s proof selection changes',
    async (key) => {
      const before = await overrideInputs({ [key]: '1' });
      expect(await overrideInputs({ [key]: '1' })).toEqual(before);
      expect(await overrideInputs({ [key]: '0' })).not.toEqual(before);
    },
  );
});

describe('Cargo release profile cache input', () => {
  it('normalizes key order and includes arbitrary release/build-override options', () => {
    const options = { CARGO_PROFILE_RELEASE_OPT_LEVEL: '1', CARGO_PROFILE_RELEASE_BUILD_OVERRIDE_DEBUG: 'true' };
    expect(releaseProfileEnv(options)).toEqual(
      releaseProfileEnv(Object.fromEntries(Object.entries(options).reverse())),
    );
    expect(releaseProfileEnv(options)).not.toEqual(
      releaseProfileEnv({ ...options, CARGO_PROFILE_RELEASE_OPT_LEVEL: '2' }),
    );
    expect(releaseProfileEnv(options)).not.toEqual(releaseProfileEnv({ CARGO_PROFILE_RELEASE_OPT_LEVEL: '1' }));
    expect(releaseProfileEnv({ ...options, UNRELATED: 'value', CARGO_PROFILE_DEV_DEBUG: 'true' })).toEqual(
      releaseProfileEnv(options),
    );
    expect(releaseProfileEnv({ CARGO_PROFILE_RELEASE_OPT_LEVEL: '' })).not.toEqual(releaseProfileEnv({}));
  });
});
